//! Hello v2 processing, catch-up after hello and the hello replay cache.
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- hello ------------------------------------------------------------------

pub(super) fn process_hello(
    shared: &Shared,
    state: &mut ConnState,
    out: &mpsc::Sender<CtrlFrame>,
    message: CtrlMessage,
) -> Flow {
    let CtrlMessage::Hello {
        version,
        device_id,
        name,
        timestamp_ms,
        challenge,
        signature,
    } = message
    else {
        log::debug!("v2 connection must start with Hello");
        return Flow::Disconnect;
    };
    if version != crate::protocol_v2::VERSION.into() {
        log::debug!("hello v2 with unsupported version {version}");
        return Flow::Disconnect;
    }
    let Some(expected_nonce) = state.challenge else {
        log::debug!("hello without a live AuthChallenge");
        return Flow::Disconnect;
    };
    // The challenge is single-use and bound to this connection.
    state.challenge = None;
    if challenge != expected_nonce {
        log::debug!("hello challenge echo mismatch");
        return Flow::Disconnect;
    }
    let now = now_ms();
    if now > state.challenge_expires_ms {
        log::debug!("hello arrived after its AuthChallenge expired");
        return Flow::Disconnect;
    }
    if Duration::from_millis(now.abs_diff(timestamp_ms)) > shared.limits.hello_max_age {
        log::info!(
            "hello from {} rejected: stale",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Exporter-bound signature: valid only on this TLS connection.
    let signed = hello2::hello2_message(
        &shared.registry_id,
        &state.exporter,
        version,
        &device_id,
        &name,
        timestamp_ms,
        &challenge,
    );
    if !hello2::verify(&device_id, &signed, &signature) {
        log::info!(
            "hello from {} rejected: bad signature",
            util::hex_prefix(&device_id, 4)
        );
        return Flow::Disconnect;
    }
    // Replay window (defense in depth on top of the exporter binding and
    // the single-use challenge).
    let digest = hello_digest(&device_id, &signature);
    let registered = {
        let mut inner = shared.lock();
        if !inner
            .hello_replay
            .admit(digest, Instant::now(), shared.limits.hello_max_age)
        {
            false
        } else {
            inner.next_conn_id = inner.next_conn_id.wrapping_add(1);
            let conn_id = inner.next_conn_id;
            if let Some(previous) = inner.conns.get(&device_id) {
                // R36-P2-04 remainder (run 43321601964 diagnosis): the
                // replaced connection gets the REASON FRAME before the
                // close signal — a bare close reads as plain EOF on the
                // client, `replace_closed` never sets, and the daemon's
                // keeper reconnects IMMEDIATELY (backoff step 0),
                // killing the one-shot (invite revoke) that replaced it:
                // the exact connection war this loop used to cause by
                // silence. 0x1001/`ERR_DEVICE_REPLACED` is the
                // long-reserved code both client stacks already route.
                deliver_msg(
                    &previous.out,
                    server_error(0x1001, "replaced by a newer connection"),
                );
                let _ = previous.close.send(true);
            }
            inner.conns.insert(
                device_id,
                ConnEntry {
                    conn_id,
                    out: out.clone(),
                    close: state.close.clone(),
                    candidates: Vec::new(),
                    peer_ip: state.peer_ip,
                },
            );
            true
        }
    };
    if !registered {
        log::warn!(
            "hello from {} rejected: replay suspected",
            util::hex_prefix(&device_id, 4)
        );
        deliver_msg(out, server_error(ERR_BAD_REQUEST, "replay suspected"));
        return Flow::Disconnect;
    }
    state.device = Some(device_id);
    state.conn_id = {
        let inner = shared.lock();
        inner
            .conns
            .get(&device_id)
            .map(|entry| entry.conn_id)
            .unwrap_or(0)
    };
    log::info!("v2 device {} online", util::hex_prefix(&device_id, 4));
    deliver_msg(
        out,
        CtrlMessage::Welcome {
            session_ttl_secs: shared.limits.session_ttl.as_secs() as u32,
            capabilities: CAPABILITIES,
        },
    );
    // Catch-up pushes: pending redeem offers for this owner, stored
    // owner challenges / redeemer proofs the other side already sent.
    catchup_after_hello(shared, &device_id);
    Flow::Continue
}

fn catchup_after_hello(shared: &Shared, me: &DeviceId) {
    match shared.store.pending_offers_for_owner(me) {
        Ok(offers) => {
            for offer in offers {
                let mut frame = CtrlFrame::new(CtrlMessage::InviteRedeemOffered {
                    code_id: offer.code_id,
                    code_revision: offer.code_revision,
                    pairing_id: offer.pairing_id,
                    redeemer_device_id: offer.redeemer,
                    redeem: offer.redeem,
                });
                if let Some(blob) = offer.redeemer_endpoint {
                    frame = frame.with_endpoint(EndpointBlob(blob));
                }
                shared.deliver(me, frame);
            }
        }
        Err(e) => log::error!("pending offer catch-up failed: {e}"),
    }
    match shared.store.pending_challenges_for_redeemer(me) {
        Ok(challenges) => {
            for (_pairing_id, transcript, owner_proof) in challenges {
                shared.deliver(
                    me,
                    CtrlMessage::PairChallenge {
                        transcript,
                        owner_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("challenge catch-up failed: {e}"),
    }
    match shared.store.pending_proofs_for_owner(me) {
        Ok(proofs) => {
            for (pairing_id, transcript_digest, redeemer_proof) in proofs {
                shared.deliver(
                    me,
                    CtrlMessage::PairProof {
                        pairing_id,
                        transcript_digest,
                        redeemer_proof,
                    },
                );
            }
        }
        Err(e) => log::error!("proof catch-up failed: {e}"),
    }
}

/// 256-bit digest of (device_id || signature) for the Hello replay
/// window — the same splitmix64-lane construction as the v1 server
/// (no crypto dependency on this plane; the v1 original and its
/// rationale live in the `legacy-plaintext-signal`-gated `crate::server`).
fn hello_digest(device_id: &DeviceId, signature: &Signature64) -> [u8; 32] {
    let mut lanes = [
        0x243F_6A88_85A3_08D3u64,
        0x1319_8A2E_0370_7344u64,
        0xA409_3822_299F_31D0u64,
        0x082E_FA98_EC4E_6C89u64,
    ];
    let absorb = |lanes: &mut [u64; 4], chunk: u64, index: usize| {
        for (i, lane) in lanes.iter_mut().enumerate() {
            *lane = lane
                .wrapping_add(chunk.rotate_left(((index * 8 + i * 3) % 64) as u32))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15 ^ ((i as u64) << 32));
        }
    };
    let mut input = [0u8; 96];
    input[..32].copy_from_slice(device_id);
    input[32..].copy_from_slice(signature);
    let mut chunks = [0u64; 12];
    for (i, chunk) in chunks.iter_mut().enumerate() {
        *chunk = u64::from_le_bytes(input[i * 8..i * 8 + 8].try_into().unwrap());
    }
    for (index, chunk) in chunks.iter().enumerate() {
        absorb(&mut lanes, *chunk, index);
    }
    let mut out = [0u8; 32];
    for (i, lane) in lanes.iter().enumerate() {
        let mut z = *lane;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out[i * 8..i * 8 + 8].copy_from_slice(&z.to_le_bytes());
    }
    out
}

/// Sliding-window memory of accepted Hello signature digests (v1
/// semantics, server-wide across connections).
#[derive(Debug, Default)]
pub(super) struct HelloReplayCache {
    seen: VecDeque<([u8; 32], Instant)>,
}

impl HelloReplayCache {
    pub(super) fn admit(&mut self, digest: [u8; 32], now: Instant, window: Duration) -> bool {
        while let Some((_, at)) = self.seen.front() {
            if now.duration_since(*at) > window {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        if self.seen.iter().any(|(seen, _)| *seen == digest) {
            return false;
        }
        if self.seen.len() >= crate::util::HELLO_REPLAY_CAPACITY {
            self.seen.pop_front();
        }
        self.seen.push_back((digest, now));
        true
    }
}
