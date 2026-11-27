//! Redeem and the pairing transcript: challenge, proof, prepared, applied,
//! status and abort.
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- redeem -------------------------------------------------------------------

pub(super) fn handle_redeem(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    redeem: crate::protocol_v2::RedeemRequest,
    redeemer_endpoint: Option<EndpointBlob>,
) -> Flow {
    let now_instant = Instant::now();
    // Budget rejections run before the lookup and are immediate. The
    // per-connection window first (tightest, no shared state), then the
    // five shared layers (CR-P1-03 + CR-P2-10): per /64 source, the
    // IPv6 /56 and /48 aggregates, per identity, and the global breaker
    // with fair-share shedding — a flood from one source (even across
    // self-generated identities, reconnects, or /64 rotation inside its
    // delegation) exhausts only its own windows and can no longer flip
    // every other source's redeem to RATE_LIMITED.
    if !state.redeem_window.allow_at(now_instant) {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                state.redeem_window.retry_after_ms(now_instant),
            ),
        );
        return Flow::Continue;
    }
    let (shared_ok, retry) = shared.redeem_allows(source_key(state.peer_ip), &me, now_instant);
    if !shared_ok {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                RATE_LIMITED,
                retry,
            ),
        );
        return Flow::Continue;
    }
    let now = now_ms();
    // Deadline sanity (one attempt spans at most 120 s, contract §8).
    if redeem.deadline_ms <= now || redeem.deadline_ms > now + REDEEM_MAX_WINDOW.as_millis() as u64
    {
        deliver_msg(
            out,
            pair_error(
                redeem.request_id,
                ErrorScope::Request,
                redeem.request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return Flow::Continue;
    }
    // Signature over the frozen redeem domain (MAC stays endpoint-only).
    let signed = hello2::redeem_message(
        &shared.registry_id,
        &redeem.lookup_digest,
        &me,
        &redeem.request_id,
        &redeem.redeemer_nonce,
        redeem.deadline_ms,
        &redeem.request_mac,
    );
    if !hello2::verify(&me, &signed, &redeem.signature) {
        // A forged redeem request is a protocol violation: disconnect.
        log::info!(
            "redeem from {} rejected: bad signature",
            util::hex_prefix(&me, 4)
        );
        return Flow::Disconnect;
    }
    // P2 (redeem 幂等): a retry with the same (redeemer, request_id)
    // replays the ORIGINAL response — minting a fresh pairing id per
    // retry forked the transaction (a second pending row for a real
    // code, a second ghost otherwise, and a second offer pushed to the
    // owner). Same journal discipline as the invite claim path; a
    // different attempt reusing the id is a REQUEST_CONFLICT.
    {
        let inner = shared.lock();
        let replay = inner.redeem_journal.get(&(me, redeem.request_id)).copied();
        match replay {
            Some(flags) if flags.matches(&redeem) => {
                drop(inner);
                deliver_msg(
                    out,
                    CtrlMessage::InviteRedeemPending {
                        request_id: redeem.request_id,
                        pairing_id: flags.pairing_id,
                        deadline_ms: flags.deadline_ms,
                    },
                );
                return Flow::Continue;
            }
            Some(_) => {
                drop(inner);
                deliver_msg(
                    out,
                    pair_error(
                        redeem.request_id,
                        ErrorScope::Request,
                        redeem.request_id,
                        REQUEST_CONFLICT,
                        0,
                    ),
                );
                return Flow::Continue;
            }
            None => {}
        }
    }
    let code = match shared.store.find_active_code(&redeem.lookup_digest, now) {
        Ok(code) => code,
        Err(e) => {
            log::error!("redeem lookup failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    redeem.request_id,
                    ErrorScope::Request,
                    redeem.request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
            return Flow::Continue;
        }
    };
    let pairing_id = random_id16();
    match code {
        // A live code owned by someone else: the real redeem path.
        Some(code) if code.owner != me => {
            match shared.store.begin_pairing(
                &pairing_id,
                &me,
                &redeem,
                &code,
                redeemer_endpoint.as_ref().map(|blob| blob.0.as_slice()),
                now,
            ) {
                Ok(BeginRedeem::Begun) => {
                    let expires_at =
                        now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
                    record_redeem_replay(
                        &mut shared.lock(),
                        &me,
                        &redeem,
                        pairing_id,
                        expires_at + REDEEM_REPLAY_GRACE,
                    );
                    // Q-N3 recurrence (2026-11-27): which branch took a
                    // redeem, and how much of its window was left.
                    log::debug!(
                        "redeem from {} begun under code revision {}, {} ms before its deadline",
                        util::hex_prefix(&me, 4),
                        code.revision,
                        redeem.deadline_ms.saturating_sub(now)
                    );
                    deliver_msg(
                        out,
                        CtrlMessage::InviteRedeemPending {
                            request_id: redeem.request_id,
                            pairing_id,
                            deadline_ms: redeem.deadline_ms,
                        },
                    );
                    // Owner online: offer immediately; offline: the
                    // pending row waits and is pushed on the owner's next
                    // Hello (catch-up in process_hello). Wave-28: the
                    // redeemer's endpoint rides the offer frame, so the
                    // owner has the peer settings in this one push.
                    let mut offer = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                        code_id: code.code_id,
                        code_revision: code.revision,
                        pairing_id,
                        redeemer_device_id: me,
                        redeem,
                    });
                    if let Some(blob) = redeemer_endpoint {
                        offer = offer.with_endpoint(blob);
                    }
                    shared.deliver(&code.owner, offer);
                }
                Ok(BeginRedeem::OwnerPendingLimit | BeginRedeem::GlobalPendingLimit) => {
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            RESOURCE_LIMIT,
                            0,
                        ),
                    );
                }
                // Wave-28 single-redemption cap: immediate, explicit —
                // no deferred deadline semantics (the code is known to
                // exist; the redeemer already holds it).
                Ok(BeginRedeem::RedeemExhausted) => {
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Code,
                            code.code_id,
                            REDEEM_EXHAUSTED,
                            0,
                        ),
                    );
                }
                Err(e) => {
                    log::error!("begin pairing failure: {e}");
                    deliver_msg(
                        out,
                        pair_error(
                            redeem.request_id,
                            ErrorScope::Request,
                            redeem.request_id,
                            STORAGE_UNAVAILABLE,
                            0,
                        ),
                    );
                }
            }
        }
        // Unknown, expired, revoked, or self-redeem: same pending shape,
        // unified failure at the deadline (no existence oracle).
        _ => {
            let expires_at =
                now_instant + Duration::from_millis(redeem.deadline_ms.saturating_sub(now));
            // Q-N3 recurrence (2026-11-27): the lookup found no active code
            // (or the owner is the redeemer); the unified answer follows at
            // the deadline (see `sweep`).
            log::debug!(
                "redeem from {} recorded as a ghost, {} ms before its deadline (self-redeem: {})",
                util::hex_prefix(&me, 4),
                redeem.deadline_ms.saturating_sub(now),
                code.as_ref().map(|code| code.owner == me).unwrap_or(false)
            );
            {
                let mut inner = shared.lock();
                inner.ghosts.insert(
                    pairing_id,
                    Ghost {
                        requester: me,
                        request_id: redeem.request_id,
                        deadline_ms: redeem.deadline_ms,
                        expires_at,
                    },
                );
                record_redeem_replay(
                    &mut inner,
                    &me,
                    &redeem,
                    pairing_id,
                    expires_at + REDEEM_REPLAY_GRACE,
                );
            }
            deliver_msg(
                out,
                CtrlMessage::InviteRedeemPending {
                    request_id: redeem.request_id,
                    pairing_id,
                    deadline_ms: redeem.deadline_ms,
                },
            );
        }
    }
    Flow::Continue
}

/// Bounded insert into the redeem idempotency journal: expired entries
/// leave first, then arbitrary victims until there is room (the cap is
/// [`JOURNAL_MAX_ROWS`], same bound as the store's journal).
fn record_redeem_replay(
    inner: &mut Inner,
    redeemer: &DeviceId,
    redeem: &RedeemRequest,
    pairing_id: PairingId,
    expires_at: Instant,
) {
    let entry = RedeemReplay {
        lookup_digest: redeem.lookup_digest,
        redeemer_nonce: redeem.redeemer_nonce,
        request_mac: redeem.request_mac,
        deadline_ms: redeem.deadline_ms,
        pairing_id,
        expires_at,
    };
    inner
        .redeem_journal
        .retain(|_, existing| existing.expires_at > Instant::now());
    while inner.redeem_journal.len() >= JOURNAL_MAX_ROWS {
        let victim = *inner
            .redeem_journal
            .keys()
            .next()
            .expect("len checked above");
        inner.redeem_journal.remove(&victim);
    }
    inner
        .redeem_journal
        .insert((*redeemer, redeem.request_id), entry);
}

// ---- pairing handshakes --------------------------------------------------------

/// Pure transcript-binding check (`pairing-flow.md` §7: key/code
/// substitution detection). The transcript must name exactly the
/// registry, code, ids, keys, nonce, request and deadline the durable
/// row recorded.
pub(super) fn transcript_bindings_ok(
    typed: &PairTranscript,
    snapshot: &PairingSnapshot,
    registry_id: &RegistryId,
) -> bool {
    typed.registry_id == *registry_id
        && typed.code_id == snapshot.code_id
        && typed.code_revision == snapshot.code_revision
        && typed.pairing_id == snapshot.pairing_id
        && typed.request_id == snapshot.request_id
        && typed.lookup_digest == snapshot.lookup_digest
        && typed.owner_device_id == snapshot.owner
        && typed.redeemer_device_id == snapshot.redeemer
        && typed.redeemer_nonce == snapshot.redeemer_nonce
        && typed.deadline_ms == snapshot.deadline_ms
}

pub(super) fn handle_pair_challenge(
    shared: &Shared,
    me: DeviceId,
    transcript: [u8; 256],
    owner_proof: PairProof,
) {
    let typed = PairTranscript::from_bytes(&transcript);
    let now = now_ms();
    let snapshot = shared.store.pairing_snapshot(&typed.pairing_id);
    let snapshot = match snapshot {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair challenge for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    // The owner is the only legitimate author of a PairChallenge; wrong
    // sender, wrong state, deadline passed, or any binding mismatch is a
    // protocol failure. No reply distinguishes these (no oracle); the
    // pending transaction simply fails at its deadline.
    let valid_sender = snapshot.owner == me
        && snapshot.state == PairingState::Pending
        && now <= snapshot.deadline_ms;
    if !valid_sender || !transcript_bindings_ok(&typed, &snapshot, &shared.registry_id) {
        log::info!(
            "pair challenge from {} rejected: bindings",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    // Server-verifiable part of the proof: the Ed25519 signature over the
    // frozen owner domain, canonical transcript and MAC. The MAC itself
    // is endpoint-only material and is deliberately not checked here.
    let signed = hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &transcript, &owner_proof.mac);
    if !hello2::verify(&typed.owner_device_id, &signed, &owner_proof.signature) {
        log::info!("pair challenge rejected: owner signature");
        return;
    }
    let digest = hello2::transcript_digest(&transcript);
    match shared.store.store_owner_challenge(
        &typed.pairing_id,
        &me,
        &transcript,
        &digest,
        &owner_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.redeemer,
                CtrlMessage::PairChallenge {
                    transcript,
                    owner_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair challenge lost its race; dropped"),
        Err(e) => log::error!("store owner challenge failure: {e}"),
    }
}

pub(super) fn handle_pair_proof(
    shared: &Shared,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    redeemer_proof: PairProof,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            log::debug!("pair proof for unknown pairing");
            return;
        }
        Err(e) => {
            log::error!("pairing snapshot failure: {e}");
            return;
        }
    };
    if snapshot.redeemer != me
        || snapshot.state != PairingState::Pending
        || now > snapshot.deadline_ms
        || snapshot.transcript_digest != Some(transcript_digest)
    {
        log::info!(
            "pair proof from {} rejected: bindings/digest",
            util::hex_prefix(&me, 4)
        );
        return;
    }
    let transcript = match snapshot.transcript {
        Some(transcript) => transcript,
        None => return,
    };
    let signed =
        hello2::proof_signed_message(REDEEMER_PROOF_DOMAIN, &transcript, &redeemer_proof.mac);
    if !hello2::verify(&snapshot.redeemer, &signed, &redeemer_proof.signature) {
        log::info!("pair proof rejected: redeemer signature");
        return;
    }
    match shared.store.store_redeemer_proof(
        &pairing_id,
        &me,
        &transcript_digest,
        &redeemer_proof,
        now,
    ) {
        Ok(true) => {
            shared.deliver(
                &snapshot.owner,
                CtrlMessage::PairProof {
                    pairing_id,
                    transcript_digest,
                    redeemer_proof,
                },
            );
        }
        Ok(false) => log::debug!("pair proof lost its race; dropped"),
        Err(e) => log::error!("store redeemer proof failure: {e}"),
    }
}

/// The authoritative commit frame. Wave-28: the owner endpoint snapshot
/// rides the extension section, so the redeemer receives the owner's
/// connection settings at the same commit point (identical frame to both
/// participants; the owner merely sees its own blob again).
fn commit_message(snapshot: &PairingSnapshot) -> Option<CtrlFrame> {
    let mut frame = CtrlFrame::new(CtrlMessage::PairCommit {
        transcript: snapshot.transcript?,
        owner_proof: snapshot.owner_proof.clone()?,
        redeemer_proof: snapshot.redeemer_proof.clone()?,
        owner_prepared_signature: snapshot.owner_prepared_signature?,
        redeemer_prepared_signature: snapshot.redeemer_prepared_signature?,
        commit_revision: snapshot.commit_revision?,
    });
    if let Some(blob) = &snapshot.owner_endpoint {
        frame = frame.with_endpoint(EndpointBlob(blob.clone()));
    }
    Some(frame)
}

fn ready_message(snapshot: &PairingSnapshot) -> Option<CtrlMessage> {
    Some(CtrlMessage::PairReady {
        pairing_id: snapshot.pairing_id,
        transcript_digest: snapshot.transcript_digest?,
        commit_revision: snapshot.commit_revision?,
        owner_applied_signature: snapshot.owner_applied_signature?,
        redeemer_applied_signature: snapshot.redeemer_applied_signature?,
    })
}

/// The name a DEBUG trace gives a Prepared decision.
fn prepared_outcome<Error>(decision: &Result<PreparedDecision, Error>) -> &'static str {
    match decision {
        Ok(PreparedDecision::WaitingOther) => "stored, waiting for the other side",
        Ok(PreparedDecision::Committed { .. }) => "committed",
        Ok(PreparedDecision::AlreadyFinal(_)) => "already final",
        Ok(PreparedDecision::Unavailable) => "unavailable (the pairing was aborted)",
        Ok(PreparedDecision::RedeemExhausted) => "redeem exhausted",
        Ok(PreparedDecision::ResourceLimit) => "resource limit",
        Ok(PreparedDecision::NotFound) => "not found",
        Err(_) => "storage failure",
    }
}

pub(super) fn handle_prepared(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            // Q-N3 recurrence (2026-11-27): a silent `unavailable`; say why.
            log::debug!(
                "prepared from {} answered unavailable: no transaction to prepare",
                util::hex_prefix(&me, 4)
            );
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || now > snapshot.deadline_ms
    {
        // Q-N3 recurrence (2026-11-27): a silent `unavailable`; say which
        // condition ruled the Prepared out (a late one has no INFO line).
        log::debug!(
            "prepared from {} answered unavailable: not a participant: {}, transcript digest differs: {}, {} ms past the deadline",
            util::hex_prefix(&me, 4),
            snapshot.owner != me && snapshot.redeemer != me,
            snapshot.transcript_digest != Some(transcript_digest),
            now.saturating_sub(snapshot.deadline_ms)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::prepared_message(&pairing_id, &transcript_digest);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "prepared from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_prepared_and_try_commit(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        now,
    );
    // Q-N3 recurrence (2026-11-27): how this Prepared landed.
    log::debug!(
        "prepared from {} at {} ms past the deadline: {}",
        util::hex_prefix(&me, 4),
        now.saturating_sub(snapshot.deadline_ms),
        prepared_outcome(&decision)
    );
    match decision {
        Ok(PreparedDecision::WaitingOther) => {}
        Ok(PreparedDecision::Committed { .. })
        | Ok(PreparedDecision::AlreadyFinal(PairingState::Committed | PairingState::Applied)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = commit_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(PairingState::Ready)) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&me, message);
                }
            }
        }
        Ok(PreparedDecision::AlreadyFinal(_)) => {}
        Ok(PreparedDecision::Unavailable) => {
            // The aborting path (revoke, sweep, explicit abort) already
            // notified both participants; tell only the sender.
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Ok(PreparedDecision::RedeemExhausted) => {
            // P1-03: the cap bound at the commit linearization point — a
            // competing pending won this code's last redeem slot. The
            // store aborted this pairing without consuming the cap; both
            // participants get the explicit failure. Wave-30 (recheck
            // R29-P2-01): the error is PAIRING-scoped with THIS pairing's
            // id — the production RegistryClient dispatch routes
            // pairing-scope errors to the pairing inbox/plane and the
            // orchestrator's wait_frame terminates on
            // object_id == pairing_id, so the losing flow actually
            // receives the terminal 0x2109 instead of waiting out its
            // deadline (a Code-scoped frame with the all-zero request id
            // had no waiter and no fan-out — silently dropped).
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REDEEM_EXHAUSTED,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
        }
        Ok(PreparedDecision::ResourceLimit) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                let error = pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    RESOURCE_LIMIT,
                    0,
                );
                shared.deliver(&fresh.owner, error.clone());
                shared.deliver(&fresh.redeemer, error);
            }
        }
        Ok(PreparedDecision::NotFound) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("prepared storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

pub(super) fn handle_applied(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    pairing_id: PairingId,
    transcript_digest: Digest32,
    commit_revision: u64,
    signature: Signature64,
) {
    let now = now_ms();
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
            return;
        }
    };
    if (snapshot.owner != me && snapshot.redeemer != me)
        || snapshot.transcript_digest != Some(transcript_digest)
        || !matches!(
            snapshot.state,
            PairingState::Committed | PairingState::Applied | PairingState::Ready
        )
    {
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let signed = hello2::applied_message(&pairing_id, &transcript_digest, commit_revision);
    if !hello2::verify(&me, &signed, &signature) {
        log::info!(
            "applied from {} rejected: signature",
            util::hex_prefix(&me, 4)
        );
        deliver_msg(out, unavailable([0u8; 16], pairing_id));
        return;
    }
    let decision = shared.store.store_applied(
        &pairing_id,
        &me,
        &signature,
        &transcript_digest,
        commit_revision,
        now,
    );
    match decision {
        Ok(AppliedDecision::WaitingOther) => {}
        Ok(AppliedDecision::Ready) | Ok(AppliedDecision::AlreadyReady) => {
            if let Some(fresh) = shared.store.pairing_snapshot(&pairing_id).ok().flatten() {
                if let Some(message) = ready_message(&fresh) {
                    shared.deliver(&fresh.owner, message.clone());
                    shared.deliver(&fresh.redeemer, message);
                }
            }
        }
        Ok(AppliedDecision::RevisionConflict) => {
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AppliedDecision::NotFound) => {
            deliver_msg(out, unavailable([0u8; 16], pairing_id));
        }
        Err(e) => {
            log::error!("applied storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    [0u8; 16],
                    ErrorScope::Pairing,
                    pairing_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

pub(super) fn handle_status_query(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // P2 (反枚举): a live ghost of THIS requester answers with the exact
    // `PairStatusReply` shape a real pending transaction produces.
    // Before, the ghost fell straight through to the SQLite lookup, hit
    // `no row`, and answered `PAIR_UNAVAILABLE` immediately — one status
    // query right after `InviteRedeemPending` told a redeemer whether the
    // code existed, defeating the unified-deadline design. Ghost and real
    // now share one answer shape while the attempt is live; after the
    // deadline the ghost is gone and the same single SQLite query yields
    // the unified `PAIR_UNAVAILABLE` for both.
    {
        let inner = shared.lock();
        let live_deadline = inner
            .ghosts
            .get(&pairing_id)
            .filter(|ghost| ghost.requester == me && ghost.expires_at > Instant::now())
            .map(|ghost| ghost.deadline_ms);
        if let Some(deadline_ms) = live_deadline {
            drop(inner);
            deliver_msg(
                out,
                CtrlMessage::PairStatusReply {
                    request_id,
                    pairing_id,
                    state: PairingState::Pending,
                    commit_revision: 0,
                    deadline_ms,
                },
            );
            return;
        }
    }
    let snapshot = match shared.store.pairing_snapshot(&pairing_id) {
        Ok(Some(snapshot)) => snapshot,
        // Unknown or non-participant: one unified answer.
        Ok(None) | Err(_) => {
            deliver_msg(out, unavailable(request_id, pairing_id));
            return;
        }
    };
    if snapshot.owner != me && snapshot.redeemer != me {
        deliver_msg(out, unavailable(request_id, pairing_id));
        return;
    }
    deliver_msg(
        out,
        CtrlMessage::PairStatusReply {
            request_id,
            pairing_id,
            state: snapshot.state,
            commit_revision: snapshot.commit_revision.unwrap_or(0),
            deadline_ms: snapshot.deadline_ms,
        },
    );
    // Recovery resends after the reply (contract §6): committed
    // transactions re-deliver the fixed PairCommit, ready ones the fixed
    // PairReady.
    match snapshot.state {
        PairingState::Committed | PairingState::Applied => {
            if let Some(frame) = commit_message(&snapshot) {
                deliver(out, frame);
            }
        }
        PairingState::Ready => {
            if let Some(frame) = ready_message(&snapshot) {
                deliver(out, frame);
            }
        }
        _ => {}
    }
}

pub(super) fn handle_abort(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    pairing_id: PairingId,
) {
    // Participants only; unknown pairings and foreign ones look the same.
    let snapshot = shared.store.pairing_snapshot(&pairing_id);
    let participant = matches!(&snapshot, Ok(Some(s)) if s.owner == me || s.redeemer == me);
    if !participant {
        deliver_msg(out, unavailable(request_id, pairing_id));
        return;
    }
    match shared.store.abort_pairing(&pairing_id, now_ms()) {
        Ok(AbortDecision::Aborted(aborted)) => {
            // Q-N3 recurrence (2026-11-27): an owner or redeemer giving up
            // is otherwise visible only as the other side's `unavailable`.
            log::debug!(
                "pairing {} aborted by an explicit request from {}; the other participant is told it is unavailable",
                util::hex_prefix(&pairing_id, 4),
                util::hex_prefix(&me, 4)
            );
            // The other participant learns the pairing is gone; the
            // aborter gets silence (idempotent success).
            let other = if aborted.owner == me {
                aborted.redeemer
            } else {
                aborted.owner
            };
            shared.deliver(&other, unavailable([0u8; 16], pairing_id));
        }
        Ok(AbortDecision::NotAbortable) => {
            // Committed transactions must be recovered, not dropped.
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Pairing,
                    pairing_id,
                    REVISION_CONFLICT,
                    0,
                ),
            );
        }
        Ok(AbortDecision::NotFound) => {
            deliver_msg(out, unavailable(request_id, pairing_id));
        }
        Err(e) => log::error!("abort storage failure: {e}"),
    }
}
