//! The op bodies: op dispatch, server info, code creation and the resident
//! receiver.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// The op bodies (async, on the embedded runtime; Arc-owned state only).
// ---------------------------------------------------------------------------

pub(super) async fn run_task_op(rt: &Arc<TaskRt>, spec: TaskOpSpec) -> Result<String, String> {
    match spec {
        TaskOpSpec::CreateCode {
            role,
            candidates,
            ttl_secs,
        } => run_create_code(rt, role, candidates, ttl_secs).await,
        TaskOpSpec::Join {
            code,
            candidates,
            role_hint,
        } => run_join(rt, &code, candidates, role_hint).await,
        TaskOpSpec::Send { conn_id, paths } => run_send(rt, conn_id, paths).await,
        TaskOpSpec::ServerInfo => run_server_info(rt).await,
    }
}

/// The `task_server_info` op body (G6 follow-through, batch G6b): ask the
/// runtime's shared registry client for the serving registry's build
/// revision (protocol-v2.md §6a). The whole G6 degrade contract rides
/// unchanged: an OLD server (no capability bit) is never asked, a failed
/// query never faults the control connection, and both surface as
/// `"revision":null` — the op itself only FAILS when the registry stack
/// could not be established at all (not started / stopped / dial failed),
/// exactly like every other op's `ensure_stack` gate.
async fn run_server_info(rt: &Arc<TaskRt>) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let revision = stack.pairing.orchestrator().client().server_info().await;
    Ok(format!(
        "{{\"op\":\"task_server_info\",\"revision\":{}}}",
        revision
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
    ))
}

/// Claim one task code at the registry. A RECEIVER-role creation binds
/// the resident receiver FIRST (real wildcard bind + sequential serve
/// loop with the offer-level consent gate) and its real port leads the
/// advertised candidates; the op fails without claiming anything if the
/// bind fails. Derived advertisements additionally carry the probe echo
/// port (module doc); explicit `candidates` in the spec are advertised
/// verbatim instead.
async fn run_create_code(
    rt: &Arc<TaskRt>,
    role: TaskRole,
    extras: Vec<SocketAddr>,
    ttl_secs: u32,
) -> Result<String, String> {
    let stack = ensure_stack(rt).await?;
    let placeholder = random_id16();
    let explicit = !extras.is_empty();
    let mut dial = extras;
    let mut echo: Vec<SocketAddr> = Vec::new();
    let mut code_slot: Option<ReceiverCodeSlot> = None;
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 2 条, sender half): a
    // sender-role code binds THE future-QUIC dial socket now — before
    // the claim, because the registered blob carries the reflexive
    // address — STUN-queries ON IT (same fail-open 2 s budget), and the
    // socket is stashed for the send op (probe round + every dial
    // attempt leave from this port). Without a STUN server the behavior
    // is byte-identical to the old port-0 advertisement plus a bound
    // socket for the dial.
    let mut sender_sock: Option<(std::net::UdpSocket, Option<SocketAddr>)> = None;
    if role == TaskRole::Sender {
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
                // Fail-open: the send path falls back to the engine's
                // own bind (the old behavior) — the bounded reason rides
                // the sender_bind event's shape.
                rt.push_event(format!(
                    "{{\"event\":\"sender_bind\",\"addr\":\"null\",\"dual_stack\":false,\"fallback_reason\":{},\"reflexive\":\"null\",\"stun_error\":\"null\"}}",
                    quoted(&message),
                ));
            }
        }
    }
    if !explicit {
        let derived = if role == TaskRole::Receiver {
            let ((bound, reflexive), slot) = match bind_resident_receiver(rt, placeholder).await {
                Ok(bound) => bound,
                Err(message) => {
                    abort_spawned(rt, placeholder);
                    return Err(message);
                }
            };
            code_slot = Some(slot);
            // CR-P1-02 改法第 2 条: the STUN server-reflexive address of
            // the receiver's own socket rides the derived list (inserted
            // right after the v4 interface block — see the helper).
            insert_reflexive_candidate(local_candidates(bound), reflexive)
        } else {
            // CR-P1-05: the sender-role blob carries the DIAL socket's
            // real port (the paired receiver's punch loop needs a
            // dialable target — port-0 "no listener" markers defeat it)
            // plus the socket's reflexive address when STUN answered.
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
        dial = derived;
        let responder = match spawn_echo_responder(rt, placeholder) {
            Ok(responder) => responder,
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        };
        echo = local_candidates(responder);
    } else if role == TaskRole::Receiver {
        // The receiver must exist (and its bind must succeed) even when
        // the caller advertises other addresses — its real port just
        // stays off the wire list.
        match bind_resident_receiver(rt, placeholder).await {
            Ok((_, slot)) => code_slot = Some(slot),
            Err(message) => {
                abort_spawned(rt, placeholder);
                return Err(message);
            }
        }
    }
    let blob = blob_candidates(
        dial,
        Vec::new(), // extras already folded into `dial` above
        echo,
        &rt.device_name,
    );
    if blob.is_empty() {
        abort_spawned(rt, placeholder);
        return Err("no dialable candidate to advertise".to_string());
    }
    let code = stack
        .pairing
        .create_task_code_with_ttl(role.direction(), &blob, ttl_secs)
        .await
        .map_err(|error| {
            abort_spawned(rt, placeholder);
            task_error_message(&error)
        })?;
    rekey_spawned(rt, placeholder, code.invite.code_id);
    // CR-P1-05: the sender's adopted dial socket lands under the REAL
    // code id (the send op looks it up by the connection's code id).
    if let Some((socket, _)) = sender_sock {
        stash_hole_socket(rt, code.invite.code_id, socket);
    }
    if let Some(slot) = code_slot {
        if let Ok(mut value) = slot.lock() {
            *value = code.invite.code_id;
        }
    }
    // Cache the code engine-locally (same-engine role mirroring, instant
    // peer notify, the owner's display lookup — module doc).
    {
        let mut table = rt
            .codes
            .lock()
            .map_err(|_| "task code cache lock poisoned".to_string())?;
        table.insert(
            normalize_code_key(&code.invite.display),
            TaskCodeEntry {
                code_id: code.invite.code_id,
                role,
                display: code.invite.display.clone(),
                owner: Arc::clone(rt),
                joiner: None,
                conn: None,
                cancelled: AtomicBool::new(false),
            },
        );
    }
    let endpoint = order_candidates(blob)
        .first()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    Ok(format!(
        "{{\"op\":\"task_create_code\",\"code\":{},\"role\":{},\"code_id\":\"{}\",\"expires_ms\":{},\"endpoint\":{}}}",
        quoted(&code.invite.display),
        quoted(role.as_str()),
        core::hex_encode(&code.invite.code_id),
        code.invite.expires_ms,
        endpoint,
    ))
}

/// Spawn the resident receiver for one code and wait (bounded) for its
/// first bound address. The task registers under `spawned` for teardown.
/// The registry code id of one resident receiver, filled in when the
/// claim/redeem returns (the receiver is bound BEFORE the registry
/// assigns the id, under a local placeholder key).
pub(super) type ReceiverCodeSlot = Arc<StdMutex<[u8; 16]>>;

/// Spawn the resident receiver for one code and wait (bounded) for its
/// first bound address (plus the STUN server-reflexive address observed
/// on the same socket, 改法第 2 条 — `None` when no server was configured
/// or the query failed open). The task registers under `key` in the
/// spawned table (teardown / disconnect abort it); the returned slot
/// carries the registry code id to the receiver once it is known.
pub(super) async fn bind_resident_receiver(
    rt: &Arc<TaskRt>,
    key: [u8; 16],
) -> Result<((SocketAddr, Option<SocketAddr>), ReceiverCodeSlot), String> {
    let (addr_tx, addr_rx) = oneshot::channel();
    let code_slot: ReceiverCodeSlot = Arc::new(StdMutex::new(key));
    let task = {
        let rt = Arc::clone(rt);
        let addr_tx = addr_tx;
        let code_slot = Arc::clone(&code_slot);
        tokio::spawn(async move {
            run_resident_receiver(rt, code_slot, addr_tx).await;
        })
    };
    rt.spawned
        .lock()
        .map_err(|_| "spawned task table lock poisoned".to_string())?
        .entry(key)
        .or_default()
        .push(task);
    match tokio::time::timeout(RECEIVER_BIND_TIMEOUT, addr_rx).await {
        Ok(Ok(bound_and_reflexive)) => Ok((bound_and_reflexive, code_slot)),
        Ok(Err(_)) => Err("the resident receiver stopped before binding".to_string()),
        Err(_) => Err("resident receiver bind timed out".to_string()),
    }
}

/// The resident receiver (wave-29 P1-04/P2-05/P2-06): bind ONE wildcard
/// endpoint (one advertised port) and serve a LOOP of connections until
/// the task reaches a terminal state.
///
/// * **Admission binds the pairing (P1-04)** — the accept `authorize`
///   admits ONLY the fingerprint ak-session's completed pairing verified
///   for this code's connection, and only while that connection is live.
///   Before the pairing completes there is no entry, so EVERY dial is
///   refused at the TLS gate and the loop keeps waiting: a stranger can
///   neither deliver an Offer nor consume the receiver. The consent hook
///   re-checks the attribution independently (fingerprint, not port).
/// * **Handshake failures are per-connection (wave-30 P1-04 residual)** —
///   a dial whose TLS handshake fails on OUR side of the accept
///   (`Transport`/`PeerIdentity`: a stranger's aborted handshake or a
///   rejected certificate) is recorded as a bounded `handshake_failed`
///   event and the loop keeps listening; only the endpoint's own death
///   or a task terminal state ends the listener. A stranger cannot end
///   the receiver by dialing badly.
/// * **Decline keeps the listener (P2-05)** — a DECLINED offer ends only
///   that connection; the sender may retry on the same code (a fresh
///   `incoming_request` fires).
/// * **Done is the single-transfer terminal state (P2-05)** — a completed
///   transfer ends the task: both sides converge their connection entries
///   to `disconnected` (`done`).
/// * **Per-task save folder (P2-06)** — the out dir is resolved from the
///   runtime's CURRENT setting each time a transfer starts, so
///   `ak_task_set_save_dir` applies to every subsequent task.
/// * **Idle close notification (P2-03)** — a TASK-CLOSE Error frame from
///   the paired peer ends the pairing ByPeer (the pump surfaces
///   `disconnected`/`peer`).
async fn run_resident_receiver(
    rt: Arc<TaskRt>,
    code_slot: ReceiverCodeSlot,
    first_addr: oneshot::Sender<(SocketAddr, Option<SocketAddr>)>,
) {
    // The CURRENT registry code id (the placeholder until the claim or
    // redeem that owns this receiver assigned the real one).
    let current_code = || -> [u8; 16] {
        code_slot
            .lock()
            .map(|slot| *slot)
            .unwrap_or_else(|poisoned| *poisoned.into_inner())
    };
    let identity = make_self_signed(&rt.key);
    // CR-P1-02 改法第 1 条 + 第 2 条: the socket is created explicitly —
    // dual-stack `[::]` first (C1; v4 wildcard fallback for hosts without
    // IPv6) — the optional STUN reflexive query runs ON IT (same socket,
    // so the NAT mapping survives), and only then is it handed to QUIC
    // via `bind_with_socket`. Every optional step fails open; the outcome
    // rides one bounded `receiver_bind` event (the reflexive address and
    // the bounded STUN failure reason extend C1's shape — the Dart pump
    // ignores unknown fields).
    let sock = match bind_receiver_socket(rt.stun_addr).await {
        Ok(sock) => sock,
        Err(_) => return, // the dropped oneshot fails the create/join op
    };
    // CR-P1-05 (review 2026-11-11 §4.5 改法第 3 条): keep a SEND-ONLY
    // `try_clone` of the very socket QUIC is about to adopt (duplicate
    // handle, same local port) for the receiver-side hole punch — the
    // punch task spawned at pairing completion sends the peer-opening
    // packets FROM the QUIC socket's port, so a port-restricted-cone NAT
    // on this side admits the sender's QUIC Initial. The clone NEVER
    // receives (it would compete with quinn for the socket queue).
    // Stashed under the CURRENT code id — the placeholder until the
    // claim/redeem fills the slot; [`rekey_spawned`] moves it with the
    // resident tasks.
    let punch_clone = sock.socket.try_clone().ok();
    let endpoint = match ak_net::DirectEndpoint::bind_with_socket(sock.socket, &identity).await {
        Ok(endpoint) => endpoint,
        Err(_) => return, // the dropped oneshot fails the create/join op
    };
    if let Some(clone) = punch_clone {
        stash_hole_socket(&rt, current_code(), clone);
    }
    let bound = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let _ = first_addr.send((bound, sock.reflexive));
    rt.push_event(format!(
        "{{\"event\":\"receiver_bind\",\"addr\":{},\"dual_stack\":{},\"fallback_reason\":{},\"reflexive\":{},\"stun_error\":{}}}",
        quoted(&bound.to_string()),
        // Tracked at bind time: adopted-socket endpoints always report
        // is_dual_stack() == false (ak-net cannot portably re-query
        // V6ONLY) — see ReceiverSocket::dual_stack.
        sock.dual_stack,
        sock.bind_fallback
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
        sock.reflexive
            .as_ref()
            .map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
        sock.stun_error
            .as_deref()
            .map_or_else(|| "null".to_string(), quoted),
    ));
    // Progress lines ride under the CONNECTION id (wave-31 R30-P2-02):
    // the pairing transaction's id is the ONE correlation key both sides
    // of a pairing hold — the sender's rows already use it, and a joiner
    // that typed someone else's code never learns the code id at all, so
    // the Dart side maps receive progress by conn id alone. The current
    // code id (the registry id once the claim/redeem that owns this
    // receiver filled the slot) resolves the connection entry; entries
    // are never removed, so a started transfer always finds its conn.
    let progress_queue = Arc::clone(&rt.engine_progress);
    let progress_slot = Arc::clone(&code_slot);
    let progress_rt = Arc::clone(&rt);
    let progress = move |event: Progress| {
        let code_id = progress_slot
            .lock()
            .map(|slot| *slot)
            .unwrap_or_else(|poisoned| *poisoned.into_inner());
        let job = progress_rt
            .conn_of_code(&code_id)
            .map(|conn| core::hex_encode(&conn.conn_id))
            // Unreachable for a started transfer (consent already
            // required a fingerprint-matched, live connection entry);
            // keep a diagnosable code-id row rather than dropping the
            // event silently.
            .unwrap_or_else(|| core::hex_encode(&code_id));
        // Client UI U1 (ffi-v2 §8.4 U1 revision): the task plane renders
        // the SAME payload fields as the engine-level lines (`files`/
        // `bytes`/`file`/`chunk`/`path`/`verified`) — the GUI's live
        // progress needs the byte counts, not just the event name.
        core::push_event(&progress_queue, &job, None, &event);
    };
    loop {
        if rt.is_stopped() {
            return;
        }
        let code_id = current_code();
        // The task already reached a terminal state (done, ByPeer,
        // receiver_failed, local disconnect): stop listening.
        if rt
            .conn_of_code(&code_id)
            .is_some_and(|conn| conn.state == ConnState::Disconnected)
        {
            return;
        }
        // P1-04 admission: only this pairing's verified peer, and only
        // while its connection is live. A refused dial closes the
        // stranger's connection and the loop keeps listening.
        let authorize = |fp: &Fingerprint| -> bool {
            let code_id = current_code();
            rt.conn_of_code(&code_id).is_some_and(|conn| {
                conn.state != ConnState::Disconnected && conn.peer_fp == Some(*fp)
            })
        };
        let channel = match endpoint.accept(&authorize).await {
            Ok(channel) => channel,
            Err(ak_net::ConnectError::Unauthorized) => continue,
            Err(ak_net::ConnectError::Io(err))
                if err.kind() == std::io::ErrorKind::NotConnected
                    && err.to_string().contains("endpoint closed") =>
            {
                // The ENDPOINT itself is gone (teardown/driver dropped):
                // the one honest end of the listener.
                return;
            }
            Err(failure) => {
                // Wave 30 (review 2026-11-07, P1-04 residual): ONE
                // connection's handshake failed (a stranger's aborted
                // dial — Transport — or a certificate the server side
                // rejected — PeerIdentity). That is a per-connection
                // fact, never a listener verdict: record the bounded
                // event and keep serving; the listener only ends with a
                // task terminal state above or the endpoint's own death.
                // Wave-31 (R30-P3-01): the field is `code_id` (a 32hex
                // task-code id STRING, no conn — an unpaired dial has no
                // attributable connection), matching the frozen
                // contract shape and the Dart typed parser.
                rt.push_event(format!(
                    "{{\"event\":\"handshake_failed\",\"code_id\":\"{}\",\"reason\":{}}}",
                    core::hex_encode(&current_code()),
                    quoted(handshake_failure_word(&failure)),
                ));
                continue;
            }
        };
        // CR-P1-05: the FIRST successful data-plane connection stops the
        // receiver punch loop early (the review's early-stop rule) —
        // from here on the QUIC connection itself keeps the mapping hot.
        if let Ok(holes) = rt.hole_sockets.lock() {
            if let Some(entry) = holes.get(&code_id) {
                entry.punch_stop.store(true, Ordering::Relaxed);
            }
        }
        // P2-06 (wave-31): the save folder is resolved ONCE per transfer
        // start — the moment this connection is accepted — and that ONE
        // binding is what the engine's ReceiveSpec writes into, what the
        // consent event shows and what `done` reports. No second read of
        // the global setting can skew them: a `set_save_dir` during the
        // pending window applies to the NEXT request, never the
        // in-flight one (the immutable-binding semantics the wave-28
        // report blessed for this fix).
        let out_dir = match rt.save_dir.lock() {
            Ok(dir) => dir.clone(),
            Err(_) => return,
        };
        // ak-engine's save target requires the directory to exist
        // (created at start/set time; re-created best-effort here in case
        // it was removed).
        let _ = std::fs::create_dir_all(&out_dir);
        let mut spec = ReceiveSpec::new(out_dir.clone(), identity.clone());
        spec.checkpoint_dir = rt.id_dir.join("checkpoints");
        spec.device_name = format!("ak-task-{}", rt.device_fp.short_id());
        spec.idle_timeout = TASK_SEND_IDLE;
        // The non-blocking consent hook (P2-07): registers + returns
        // pending. Built PER CONNECTION so the event's saveDir is THIS
        // transfer's binding (wave-31 P2-06) — the loop serves one
        // connection at a time, so the capture cannot race a later one.
        let consent: TransferConsent = {
            let rt = Arc::clone(&rt);
            let code_slot = Arc::clone(&code_slot);
            let bound_dir = out_dir.clone();
            Arc::new(move |info: &ConsentInfo| {
                let code_id = code_slot
                    .lock()
                    .map(|slot| *slot)
                    .unwrap_or_else(|poisoned| *poisoned.into_inner());
                register_consent(&rt, &code_id, info, &bound_dir, PENDING_CONSENT_WINDOW)
            })
        };
        let result = receive_accepted_consent(channel, spec, &progress, Some(consent)).await;
        match result {
            Ok(report) => {
                // The single-transfer terminal state: report and converge.
                let conn = rt.conn_of_code(&current_code());
                let mut line = String::with_capacity(160);
                line.push_str("{\"event\":\"done\"");
                if let Some(conn) = &conn {
                    line.push_str(&format!(
                        ",\"conn\":\"{}\"",
                        core::hex_encode(&conn.conn_id)
                    ));
                }
                // saveDir (wave-31 P2-06): the ACTUAL landing directory —
                // the same binding the consent event showed, so the UI
                // can display the real result of the finished transfer.
                line.push_str(&format!(
                    ",\"saveDir\":{}",
                    quoted(&out_dir.to_string_lossy()),
                ));
                line.push_str(&format!(
                    ",\"files\":{},\"bytes\":{}}}",
                    report.files, report.bytes,
                ));
                rt.push_event(line);
                if let Some(conn) = conn {
                    rt.push_disconnected_once(&conn.conn_id, "done");
                }
                return;
            }
            Err(EngineError::ConsentDeclined { .. }) => {
                // The user (or the 120 s window) declined: the pending
                // entry is resolved, the peer was told over the wire,
                // nothing landed on disk. Only THIS connection ended —
                // keep listening, the sender may retry on the same code.
                continue;
            }
            Err(EngineError::PeerError { message, .. }) if message == TASK_CLOSE_MESSAGE => {
                // The paired peer's authenticated idle-close (P2-03):
                // end the pairing ByPeer through the event pump.
                report_peer_departure(&rt, &current_code());
                return;
            }
            Err(_) => {
                // Channel-level failure (sender gave up mid-handshake,
                // peer disconnected mid-transfer, IO error). The task is
                // over; when the pairing had completed, this is the peer
                // going away — report ByPeer through the event pump (the
                // cross-device `disconnected`/`peer` notification).
                report_peer_departure(&rt, &current_code());
                return;
            }
        }
    }
}

/// Report one pairing as ended ByPeer when it had completed (the receiver
/// observed its peer's data plane depart).
fn report_peer_departure(rt: &Arc<TaskRt>, code_id: &[u8; 16]) {
    if rt.conn_of_code(code_id).is_some() {
        if let Some(stack) = rt.stack.lock().ok().and_then(|slot| slot.clone()) {
            stack.pairing.report_peer_disconnect(*code_id);
        }
    }
}
