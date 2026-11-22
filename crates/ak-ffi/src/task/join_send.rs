//! The op bodies: joining a code and sending to a joined peer.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;
use ak_core::error::ErrorCode;
use ak_engine::DECLINED_MESSAGE;

/// Join one live code AT THE REGISTRY: resolve the joiner's side (spec
/// hint → engine-cache mirror → the v1 receiver default), bind the
/// joiner's resident receiver + probe echo responder when the candidates
/// are derived, drive the frozen pairing transcript to `PairReady`
/// through `join_task_code`, then book the connection (conn id = the
/// pairing transaction id, identical on both sides), emit `connected`,
/// run the REAL reachability probe and report its honest verdict.
pub(super) async fn run_join(
    rt: &Arc<TaskRt>,
    code: &str,
    extras: Vec<SocketAddr>,
    role_hint: Option<TaskRole>,
) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let my_role = role_hint
        .or_else(|| {
            rt.codes
                .lock()
                .ok()
                .and_then(|table| table.get(code).map(|entry| entry.role.opposite()))
        })
        .unwrap_or(TaskRole::Receiver);
    let placeholder = random_id16();
    let explicit = !extras.is_empty();
    let mut dial = extras;
    let mut echo: Vec<SocketAddr> = Vec::new();
    let mut code_slot: Option<ReceiverCodeSlot> = None;
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): the
    // joining SENDER binds THE future-QUIC dial socket BEFORE the redeem
    // — the blob the redeem registers must carry the reflexive address
    // (the paired receiver's punch loop aims at it) and the socket's
    // real port. STUN runs ON IT under the same fail-open budget; the
    // socket is stashed under the code id once the redeem returns.
    let mut sender_sock: Option<(std::net::UdpSocket, Option<SocketAddr>)> = None;
    if my_role == TaskRole::Sender {
        match bind_sender_socket(rt.stun_addr).await {
            Ok(sock) => {
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":{},\"dual_stack\":{},\"fallback_reason\":{},\"reflexive\":{},\"stun_error\":{}}}",
                    quoted(&sock.socket.local_addr().map(|addr| addr.to_string()).unwrap_or_default()),
                    sock.dual_stack,
                    sock.bind_fallback.as_deref().map_or_else(|| "null".to_string(), quoted),
                    sock.reflexive.as_ref().map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
                    sock.stun_error.as_deref().map_or_else(|| "null".to_string(), quoted),
                ));
                sender_sock = Some((sock.socket, sock.reflexive));
            }
            Err(message) => {
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":\"null\",\"dual_stack\":false,\"fallback_reason\":{},\"reflexive\":\"null\",\"stun_error\":\"null\"}}",
                    quoted(&message),
                ));
            }
        }
    }
    if !explicit {
        dial = if my_role == TaskRole::Receiver {
            match bind_resident_receiver(rt, placeholder).await {
                Ok(((bound, reflexive), slot)) => {
                    code_slot = Some(slot);
                    // 改法第 2 条, the joiner-receiver mirror of the
                    // create path: the socket-reflexive address rides the
                    // derived candidates.
                    insert_reflexive_candidate(local_candidates(bound), reflexive)
                }
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            }
        } else {
            // CR-P1-05: the joining sender's blob carries the dial
            // socket's real port + reflexive address (same shape as the
            // create-side sender path above).
            let (socket, reflexive) = sender_sock
                .as_ref()
                .map(|(socket, reflexive)| {
                    (
                        socket
                            .local_addr()
                            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0))),
                        *reflexive,
                    )
                })
                .unwrap_or((SocketAddr::from(([0, 0, 0, 0], 0)), None));
            insert_reflexive_candidate(local_candidates(socket), reflexive)
        };
        let responder = match spawn_echo_responder(rt, placeholder) {
            Ok(responder) => responder,
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        };
        echo = local_candidates(responder);
    }
    let blob = blob_candidates(dial, Vec::new(), echo, &rt.device_name);
    if blob.is_empty() {
        abort_spawned(rt, placeholder);
        return Err("no dialable candidate to advertise".to_string());
    }
    let paired = match stack
        .pairing
        .join_task_code(code, &blob, my_role.direction())
        .await
    {
        Ok(paired) => paired,
        Err(error) => {
            abort_spawned(rt, placeholder);
            return Err(task_error_message(&error));
        }
    };
    let code_id = paired.code_id;
    let conn_id = paired.pairing_id;
    let dial_candidates = paired.peer.candidates.clone();
    let peer_fp = paired.verified.owner_fingerprint();
    rekey_spawned(rt, placeholder, code_id);
    // CR-P1-05: the joining sender's adopted dial socket lands under the
    // real code id (the send op adopts it); the joining RECEIVER punches
    // toward the paired sender's advertised candidates from its stashed
    // QUIC-socket clone — concurrent with the sender's dial, bounded by
    // PUNCH_WINDOW, stopped by the accept path's flag. CR-P1-07: the
    // joining SENDER runs its pairing-time opener + mapping keepalive now
    // (the same shared core as the CLI) instead of at send time — the
    // GUI's send may come arbitrarily late.
    if let Some((socket, _)) = sender_sock {
        stash_hole_socket(rt, code_id, socket);
    }
    if let Some(slot) = code_slot {
        if let Ok(mut value) = slot.lock() {
            *value = code_id;
        }
    }
    // Publish the connection and its policy deadline under the same gate.
    {
        let mut lifetimes = rt
            .sender_lifetimes
            .lock()
            .map_err(|_| "sender lifecycle lock poisoned".to_string())?;
        if my_role == TaskRole::Sender {
            lifetimes.entry(conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
        let mut conns = rt
            .conns
            .lock()
            .map_err(|_| "connection table lock poisoned".to_string())?;
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role: my_role,
                state: ConnState::Probing,
                candidates: dial_candidates.clone(),
                peer_endpoint: order_candidates(dial_candidates.clone()).first().copied(),
                peer_fp: Some(peer_fp),
                // X5: the peer's self-reported blob name rides the record
                // (display-only; the verified `peer_fp` is the identity).
                peer_name: Some(paired.peer.name.clone()),
                code_id,
                code: code.to_string(),
            },
        );
    }
    match my_role {
        TaskRole::Receiver => {
            spawn_receiver_punch(rt, code_id, &dial_candidates);
        }
        TaskRole::Sender => {
            spawn_sender_opener(rt, code_id, &dial_candidates);
        }
    }
    // Same-engine cache flip: the owner runtime (if any) learns the
    // joiner + conn id for its instant disconnect notify.
    {
        if let Ok(mut table) = rt.codes.lock() {
            if let Some(entry) = table.get_mut(code) {
                entry.conn = Some(conn_id);
                entry.joiner = Some(Arc::clone(rt));
            }
        }
    }
    // The reported endpoint is the LAN-first first candidate — the same
    // address a send would dial first.
    let endpoint = order_candidates(dial_candidates.clone())
        .first()
        .copied()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    rt.push_event(format!(
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"code\":{},\"peer_name\":{},\"peer_fp_short\":{}}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: the shared code id rides the joiner's
        // `connected` event too (same value as the owner's).
        core::hex_encode(&code_id),
        quoted(my_role.as_str()),
        endpoint,
        quoted(code),
        // X5 (batch X, 2026-11-22, ffi-v2 §8.3 批次 X 增补): the peer's
        // self-reported device name plus the VERIFIED fingerprint's short
        // form — the name is display-only, the short code is the identity
        // anchor the UI annotates it with.
        quoted(&paired.peer.name),
        quoted(&peer_fp.short_id()),
    ));
    // The REAL reachability probe; the settle is honest (P2-09):
    // all-failed leaves the entry `unreachable`, matching the join
    // result's `probe` verdict. CR-P1-07 改法 2: a SENDER's test rides the
    // ADOPTED dial socket (meaningful through a port-restricted-cone NAT,
    // and the GUI no longer treats its failure as terminal); a RECEIVER
    // keeps the pairing-plane probe (quinn owns its socket).
    let outcome = if my_role == TaskRole::Sender {
        adopted_socket_probe(rt, &code_id, &dial_candidates).await
    } else {
        probe_with_retry(&stack.pairing, &paired.peer).await
    };
    let (probe_line, verdict) = probe_result_line(&conn_id, &outcome);
    settle_probe_state(rt, &conn_id, &outcome);
    rt.push_event(probe_line);
    Ok(format!(
        "{{\"op\":\"task_join\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{},\"probe\":\"{verdict}\"}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: the join result carries the redeemed code's
        // registry id — a joiner that never saw the create result fills
        // its conn→code (and progress-correlation) mapping from here.
        core::hex_encode(&code_id),
        quoted(my_role.as_str()),
        endpoint,
    ))
}

/// One send op: run the shared candidate dial pass
/// (`task_net::staggered_dial_pass`, CR-P3-09: reflexive/observed
/// addresses strictly first, Happy-Eyeballs stagger on the self-bound
/// path, sequential on the adopted socket) under the split connect
/// budget; a post-connect failure fails the op (another address cannot
/// fix it); success emits `done` and completes the op. Purely
/// connect-level failure of the WHOLE list gets [`TASK_CONNECT_PASSES`]
/// passes (the just-paired peer's admission bookkeeping can trail the
/// joiner by a beat — see the constant). Since CR-P1-02 改法第 3 条 the
/// connect phase OPENS with one simultaneous-probe round
/// ([`probe_dial_round`]): when it observes an answering address, that
/// address leads the dial order under the bounded observed-candidate
/// budget; with no observation the round is invisible and the static
/// order dials exactly as before. Since CR-P1-07 the send-time
/// reflexive opener is GONE — the opener + mapping keepalive run at
/// PAIRING COMPLETION ([`spawn_sender_opener`]); the admission gate pauses
/// keeper packets while any send owns the connection.
pub(super) async fn run_send(
    rt: &Arc<TaskRt>,
    conn_id: [u8; 16],
    paths: Vec<String>,
) -> Result<String, String> {
    let conn = {
        let conns = rt
            .conns
            .lock()
            .map_err(|_| "connection table lock poisoned".to_string())?;
        conns
            .get(&conn_id)
            .cloned()
            .ok_or_else(|| format!("unknown connection {}", core::hex_encode(&conn_id)))?
    };
    if conn.role != TaskRole::Sender {
        return Err("only the sender side of a task connection may send".to_string());
    }
    let pinned = conn.peer_fp.ok_or_else(|| {
        "the peer key is unresolved (ak-session PeerEndpoint pending)".to_string()
    })?;
    let static_order = order_candidates(conn.candidates.clone());
    if static_order.is_empty() {
        return Err(format!(
            "connection {} carries no dial candidates",
            core::hex_encode(&conn_id)
        ));
    }
    // CR-P1-05: when this sender stashed an adopted dial socket at
    // join/create time, the probe round runs ON a `try_clone` of IT (a
    // duplicate handle of the same OS socket — same local port, same NAT
    // mapping) and every dial attempt below adopts the same socket; the
    // review's core rule (审核改法第 4 条: the probing AND the QUIC dial
    // ride one socket). No stash (bind failed, or an old runtime): the
    // legacy throwaway round, byte-identical to the pre-CR path.
    //
    // Admission, not this attempt, owns keeper activity. No sender-side
    // stop flag or epoch lives in the socket stash.
    let adopted = rt
        .hole_sockets
        .lock()
        .ok()
        .and_then(|holes| holes.get(&conn.code_id)?.socket.try_clone().ok());
    // batchF G2: the round's observation rides one bounded
    // `probe_observed` event per send (the emission pattern of
    // `sender_bind` — a plain `push_event`, unknown-name-tolerated on
    // the Dart side); the round itself early-exits on the first
    // confirmed head since CR-P3-06 (task_net's probe_dial_round_on).
    let dial = match adopted.as_ref().map(|socket| socket.try_clone().ok()) {
        Some(Some(round_socket)) => {
            probe_dial_round_on(
                round_socket,
                static_order.len(),
                static_order,
                |head, report, _| {
                    rt.push_event(probe_observed_event(&conn_id, head, report));
                },
            )
            .await
        }
        _ => {
            probe_dial_round(static_order, |head, report, _| {
                rt.push_event(probe_observed_event(&conn_id, head, report));
            })
            .await
        }
    };
    let mut failures: Vec<(SocketAddr, String)> = Vec::new();
    for pass in 0..TASK_CONNECT_PASSES {
        if pass > 0 {
            // Stop retrying a connection that ended while we waited.
            let severed = rt
                .conns
                .lock()
                .map(|conns| {
                    conns
                        .get(&conn_id)
                        .is_none_or(|conn| conn.state == ConnState::Disconnected)
                })
                .unwrap_or(true);
            if severed {
                break;
            }
            tokio::time::sleep(TASK_SETTLE_PAUSE).await;
            failures.clear();
        }
        // CR-P3-09: the per-candidate dial loop lives in the shared
        // `staggered_dial_pass` (reflexive/observed first, Happy-Eyeballs
        // stagger on the self-bound path, strictly sequential on the
        // adopted socket — concurrent QUIC endpoints on handles of one OS
        // socket would compete for its receive queue).
        let outcome = staggered_dial_pass(
            &dial.order,
            dial.head,
            adopted.is_some(),
            |addr, marks, budget| {
                attempt_task_send(
                    rt,
                    &conn,
                    addr,
                    &paths,
                    pinned,
                    budget,
                    adopted.as_ref(),
                    marks,
                )
            },
            |outcome| match outcome {
                AttemptOutcome::Done(_) => AttemptVerdict::Done,
                AttemptOutcome::FailedAfterConnect { .. } => AttemptVerdict::Fatal,
                AttemptOutcome::ConnectFailed(reason) => AttemptVerdict::Retryable(reason.clone()),
            },
        )
        .await;
        if let Some((addr, AttemptOutcome::Done(report))) = outcome.done {
            rt.push_event(format!(
                "{{\"event\":\"done\",\"conn\":\"{}\",\"files\":{},\"bytes\":{}}}",
                core::hex_encode(&conn_id),
                report.files,
                report.bytes,
            ));
            // The single-transfer task is complete: converge BOTH
            // sides' entries (the receiver ends its loop and marks
            // the same terminal state; wave-29 P2-05 — the UI never
            // keeps offering a dead connection).
            rt.push_disconnected_once(&conn_id, "done");
            return Ok(format!(
                "{{\"op\":\"task_send\",\"conn\":\"{}\",\"files\":{},\"bytes\":{},\"peer\":{}}}",
                core::hex_encode(&conn_id),
                report.files,
                report.bytes,
                quoted(&addr.to_string()),
            ));
        }
        if let Some((addr, failure)) = outcome.fatal {
            if let AttemptOutcome::FailedAfterConnect { reason, verdict } = failure {
                // W1 (batch W, 2026-11-22, V-N2): a post-connect failure
                // is terminal for the CONNECTION — EXCEPT the declined
                // offer, which the frozen ffi-v2 §8.4 keeps retryable on
                // the same code. Before this fix only `PeerClosed`
                // reported the pairing; every other post-connect error
                // left the op a plain failure and the connection a
                // zombie — the V2-b walkthrough shape (the sender's QUIC
                // connection died of its idle budget while Android froze
                // the process; after thaw the send op failed, no
                // `disconnected` event ever fired, and the row stayed
                // `connected` from t=121 s until the 10-minute pairing
                // budget's `idle_expired` at t=719.6 s). Each verdict
                // ends (or deliberately spares) the connection exactly
                // once: a verifiable peer departure keeps the ByPeer
                // reporting (the event pump's `disconnected`/`peer`), a
                // decline spares the pairing, and the rest close through
                // [`TaskRt::push_disconnected_once`] with the honest
                // `conn_lost` reason — a connection interrupted
                // mid-flight, NOT the pairing idle budget's
                // `idle_expired` (ffi-v2 §8.3, 批次 W 增补).
                match verdict {
                    PostConnectVerdict::PeerGone => {
                        // The peer's data plane is gone — the pairing is
                        // ending ByPeer through the event pump; a resumed
                        // keepalive would only hold a dead mapping.
                        report_peer_gone(rt, &conn);
                    }
                    PostConnectVerdict::Declined => {
                        // The receiver's protocol Reject (ffi-v2 §8.4:
                        // 拒绝不是任务终态): the listener keeps serving,
                        // a fresh send on the same code is legal — the
                        // connection survives the failed op.
                    }
                    PostConnectVerdict::Lost => {
                        rt.push_disconnected_once(&conn_id, "conn_lost");
                    }
                }
                return Err(format!(
                    "transfer failed after connecting via {addr}: {reason}"
                ));
            }
            unreachable!("the FFI verdict maps only FailedAfterConnect to Fatal");
        }
        failures = outcome.failures;
    }
    let detail = failures
        .iter()
        .map(|(addr, reason)| format!("{addr}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("no candidate accepted the connection ({detail})"))
}

/// Report one pairing as ended ByPeer (the peer's data plane went away).
/// The event pump turns it into the `disconnected`/`peer` line.
fn report_peer_gone(rt: &Arc<TaskRt>, conn: &TaskConn) {
    if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
        stack.pairing.report_peer_disconnect(conn.code_id);
    }
}

enum AttemptOutcome {
    Done(Report),
    FailedAfterConnect {
        reason: String,
        /// W1 (batch W): what the failure means for the CONNECTION —
        /// see [`PostConnectVerdict`].
        verdict: PostConnectVerdict,
    },
    ConnectFailed(String),
}

/// What one post-connect failure means for the connection's lifecycle
/// (W1, batch W 2026-11-22): every variant except the decline ends the
/// connection — the pairing is unusable or the peer is verifiably gone —
/// while a DECLINED offer keeps it for the frozen same-code retry.
/// `pub(super)` + derives for the unit tests of
/// [`post_connect_verdict`] (task_tests.rs); behavior is untouched.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PostConnectVerdict {
    /// The engine named the peer's departure (`PeerClosed`): the pairing
    /// is reported ByPeer after the op fails (the event pump's
    /// `disconnected`/`peer`).
    PeerGone,
    /// The receiver's protocol Reject — the offer was declined. NOT a
    /// task terminal state (ffi-v2 §8.4: 同码可重试): the failed op
    /// leaves the connection and its listener alone.
    Declined,
    /// Everything else (the idle-timeout death of a frozen process, IO
    /// and protocol failures): the connection is interrupted — terminal
    /// `disconnected`/`conn_lost`.
    Lost,
}

/// W1 (batch W, 2026-11-22, external ruling): the three-state verdict as
/// a pure function — a post-connect `PeerError` is only a DECLINE or a
/// TASK-CLOSE when BOTH the wire code and the message text match. The
/// closed `ErrorCode` set reuses `LocalPermissionDenied` for DECLINED
/// ([`DECLINED_WIRE_CODE`]) and `Cancelled` for TASK-CLOSE, so a
/// single-attribute match misreads the peer: same-code-different-text
/// (a REAL local permission refusal relayed by the peer) must not pose
/// as a user decline, and same-text-different-code must not pose as a
/// TASK-CLOSE — both mismatches fall back to [`PostConnectVerdict::Lost`]
/// (`conn_lost`). The protocol Reject frame ([`EngineError::Rejected`])
/// keeps its own independent Declined branch: it is a distinct frame
/// kind, not an Error-frame code reuse. Extracted from the async send
/// body (behavior unchanged) so the counter-examples pin the match at
/// the unit level (ffi-v2 §8.3 批次 W 增补的双匹配例外).
pub(super) fn post_connect_verdict(error: &EngineError) -> PostConnectVerdict {
    match error {
        EngineError::PeerClosed => PostConnectVerdict::PeerGone,
        EngineError::PeerError { code, message, .. }
            if *code == ErrorCode::Cancelled && message == TASK_CLOSE_MESSAGE =>
        {
            PostConnectVerdict::PeerGone
        }
        EngineError::PeerError { code, message, .. }
            if *code == DECLINED_WIRE_CODE && message == DECLINED_MESSAGE =>
        {
            PostConnectVerdict::Declined
        }
        EngineError::Rejected(_) => PostConnectVerdict::Declined,
        _ => PostConnectVerdict::Lost,
    }
}

/// One candidate attempt under the split connect budget (the wave-25/26
/// CLI driver, P1-01 shape): no deadline before the engine's
/// `Connecting` report, the caller's `connect_budget` from it (normally
/// [`TASK_CONNECT_BUDGET`]; the leading observed/reflexive candidate
/// dials under [`OBSERVED_DIAL_BUDGET`] — the shared pass's hot-mapping
/// rule), no deadline once
/// connected. Post-connect failures are distinguishable so the fallback
/// loop stops instead of retrying a protocol error on another address.
///
/// CR-P1-05: `adopted` (the sender's stashed dial socket, when any) is
/// adopted into the spec — the QUIC Initial leaves from the socket the
/// same-socket STUN query and probe round mapped. The bind fallback
/// stays family-matched for the no-adoption path.
///
/// CR-P3-09: `marks` is the CALLER's (the shared `staggered_dial_pass`'s)
/// [`ConnectMarks`] for this attempt — the pass reads its `connected()`
/// flag as the Happy-Eyeballs winner signal, so the marks cannot live
/// inside this function. (The extra argument pushes the count past
/// clippy's default — bundling the borrow-only FFI context into a struct
/// would be churn for one call site; the shape mirrors the CLI
/// adapter's `SendCtx` grouping where it pays for itself.)
#[allow(clippy::too_many_arguments)]
async fn attempt_task_send(
    rt: &Arc<TaskRt>,
    conn: &TaskConn,
    addr: SocketAddr,
    paths: &[String],
    pinned: Fingerprint,
    connect_budget: Duration,
    adopted: Option<&std::net::UdpSocket>,
    marks: Arc<ConnectMarks>,
) -> AttemptOutcome {
    let mut spec = SendSpec::new(
        PathBuf::from(&paths[0]),
        addr,
        pinned,
        make_self_signed(&rt.key),
    );
    for extra in &paths[1..] {
        spec.add_root(PathBuf::from(extra));
    }
    // Family-matched wildcard bind (the engine fast-fails mismatches);
    // ignored when the adopted socket carries the dial.
    spec.bind = if addr.is_ipv6() {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    if let Some(socket) = adopted {
        if let Ok(handle) = socket.try_clone() {
            spec.adopt_socket(handle);
        }
    }
    spec.checkpoint_dir = rt.id_dir.join("checkpoints");
    spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
    // W1: the runtime's idle budget (TASK_SEND_IDLE in production; a
    // per-runtime test override exists for the conn_lost scenario — see
    // the field docs on TaskRt). A poisoned lock falls back to the
    // production constant — the budget is never worth a panic.
    spec.idle_timeout = rt
        .send_idle_timeout
        .lock()
        .map(|slot| *slot)
        .unwrap_or(TASK_SEND_IDLE);

    let progress_queue = Arc::clone(&rt.engine_progress);
    let conn_hex = core::hex_encode(&conn.conn_id);
    let send_marks = Arc::clone(&marks);
    let progress = move |event: Progress| {
        send_marks.mark(&event);
        // Client UI U1: the engine payload fields ride along (ffi-v2 §8.4).
        core::push_event(&progress_queue, &conn_hex, None, &event);
    };
    let send = send(spec, &progress);
    tokio::pin!(send);
    // The caller's budget: [`TASK_CONNECT_BUDGET`] for static candidates,
    // [`OBSERVED_DIAL_BUDGET`] for the leading observed/reflexive
    // candidate (see the constant).
    let result = match drive_with_connect_budget(&marks, send.as_mut(), connect_budget).await {
        Ok(result) => result,
        Err(()) => {
            return AttemptOutcome::ConnectFailed(format!(
                "timeout after {}s (no QUIC handshake)",
                connect_budget.as_secs(),
            ))
        }
    };
    let after_connect = marks.connected();
    match result {
        Ok(report) => AttemptOutcome::Done(report),
        Err(error) => {
            let reason = one_line_reason(&error);
            if after_connect {
                // W1 (batch W): classify the failure's meaning for the
                // connection — the declined offer is the one post-connect
                // failure that must NOT end the pairing (ffi-v2 §8.4).
                // The decline rides an Error frame with the DECLINED wire
                // code (the receiver's consent refusal), and the peer's
                // authenticated TASK-CLOSE is a deliberate departure —
                // both recognized ONLY on the code+message double match
                // (see [`post_connect_verdict`]): same-code-foreign-text
                // (a real local permission refusal) and same-text-foreign-
                // code both fall back to Lost/`conn_lost`.
                let verdict = post_connect_verdict(&error);
                AttemptOutcome::FailedAfterConnect { verdict, reason }
            } else {
                AttemptOutcome::ConnectFailed(reason)
            }
        }
    }
}
