//! `ak_task_start`, the coordination stack, the owner-side probe and the
//! hole-punch helpers.
//!
//! Moved out of `task.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// ak_task_start.
// ---------------------------------------------------------------------------

/// Start one task runtime. Registry fields mirror `ak_pairing_start` (the
/// Dart layer resolves them from `AK_REGISTRY_ADDR`/`AK_REGISTRY_ID` env
/// or the compiled `--dart-define` defaults, then hands them to the spec);
/// the default save folder is `<id_dir>/downloads`.
pub fn task_start(engine: &AkEngine, spec: &str) -> Result<[u8; 16], FfiError> {
    let args = crate::core::json::parse(spec).map_err(FfiError::invalid)?;
    let registry_text = field_str(&args, "registry", true)?
        .ok_or_else(|| FfiError::invalid("missing \"registry\""))?;
    let registry_addr: SocketAddr = registry_text.parse().map_err(|_| {
        FfiError::invalid(format!(
            "field \"registry\" is not an ip:port address: {registry_text:?}"
        ))
    })?;
    let registry_hex = field_str(&args, "registry_id", true)?
        .ok_or_else(|| FfiError::invalid("missing \"registry_id\""))?;
    let registry_id = hex32(&registry_hex, "field \"registry_id\"")?;
    // CR-P1-02 改法第 2 条 ("STUN 地址与注册中心一起配置"): the optional
    // STUN endpoint the receiver's socket-reflexive query dials. The Rust
    // layer stays DEFAULT-FREE on purpose — the registry endpoint itself
    // is resolved by the Dart layer (env / --dart-define) and passed in
    // the spec, and the STUN address rides the exact same resolution
    // path (contract ffi-v2 §8.1); an absent/empty value means NO query
    // (fail-open: derived local candidates still advertise, and
    // same-subnet/LAN joins are unaffected).
    let stun_addr = match field_str(&args, "stun_addr", false)? {
        Some(text) if !text.trim().is_empty() => {
            Some(text.trim().parse::<SocketAddr>().map_err(|_| {
                FfiError::invalid(format!(
                    "field \"stun_addr\" is not an ip:port address: {text:?}"
                ))
            })?)
        }
        _ => None,
    };
    let id_dir =
        field_str(&args, "id_dir", true)?.ok_or_else(|| FfiError::invalid("missing \"id_dir\""))?;
    if id_dir.is_empty() {
        return Err(FfiError::invalid("field \"id_dir\" must be non-empty"));
    }
    let device_name = field_str(&args, "device_name", false)?.unwrap_or_else(|| "ak-task".into());
    if device_name.chars().count() > 64 {
        return Err(FfiError::invalid(
            "field \"device_name\" must be at most 64 characters",
        ));
    }
    let trimmed_dir = id_dir.trim_end_matches(['/', '\\']);
    let save_dir_text = field_str(&args, "save_dir", false)?
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format!("{trimmed_dir}/downloads"));
    // The save folder is created eagerly: ak-engine's LocalSaveTarget
    // requires an existing directory, so a missing default would fail the
    // receiver-role code creation later (better to fail at start).
    std::fs::create_dir_all(&save_dir_text).map_err(|error| {
        FfiError::invalid(format!("cannot create save dir {save_dir_text}: {error}"))
    })?;

    // Local, synchronous bootstrap (disk under id_dir, no network).
    let dir = PathBuf::from(&id_dir);
    std::fs::create_dir_all(&dir).map_err(|error| {
        FfiError::invalid(format!("cannot create id_dir {}: {error}", dir.display()))
    })?;
    let key_path = dir.join("device.key");
    let (key, _created) = DeviceKeyPair::load_or_create(&key_path).map_err(|error| {
        FfiError::invalid(format!(
            "cannot load device key at {}: {error}",
            key_path.display()
        ))
    })?;
    let device_fp = key.fingerprint();
    let runtime_id = random_id16();
    let (codes, engine_progress) = {
        let v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        (Arc::clone(&v2.task_codes), engine.events_queue())
    };
    let rt = Arc::new(TaskRt {
        runtime_id,
        id_dir: dir,
        key,
        device_fp,
        device_name,
        registry_addr,
        registry_id,
        stun_addr,
        save_dir: StdMutex::new(PathBuf::from(save_dir_text)),
        ops: StdMutex::new(HashMap::new()),
        events: Arc::new(StdMutex::new(VecDeque::new())),
        conns: StdMutex::new(HashMap::new()),
        pending: StdMutex::new(HashMap::new()),
        spawned: StdMutex::new(HashMap::new()),
        echo_observed: Arc::new(StdMutex::new(None)),
        stopped: StdMutex::new(false),
        request_ids: StdMutex::new(HashMap::new()),
        stack: StdMutex::new(None),
        stack_init: tokio::sync::Mutex::new(()),
        conn_ops: StdMutex::new(HashMap::new()),
        codes,
        engine_progress,
        notify: StdMutex::new(None),
        hole_sockets: StdMutex::new(HashMap::new()),
        sender_lifetimes: StdMutex::new(HashMap::new()),
        // W1: the production send idle budget; task tests shorten this
        // per runtime only (see the field docs on TaskRt).
        send_idle_timeout: StdMutex::new(TASK_SEND_IDLE),
        executor: engine.runtime().handle().clone(),
    });
    {
        let mut v2 = engine
            .v2
            .lock()
            .map_err(|_| FfiError::internal("v2 state lock poisoned"))?;
        v2.tasks.insert(runtime_id, Arc::clone(&rt));
    }
    rt.push_event(format!(
        "{{\"event\":\"runtime_ready\",\"runtime\":\"{}\"}}",
        core::hex_encode(&runtime_id),
    ));
    Ok(runtime_id)
}

// CR-P2-11: candidate enumeration/ordering, reflexive insertion and the
// observed-dial-order helpers (local_candidates, filter_and_order_,
// insert_reflexive_, order_candidates, observed_dial_head,
// observed_first_order) moved to ak-session `pairing::task_net`.

/// Parse the optional `candidates` field: an array of `ip:port` strings.
pub(super) fn parse_candidates(
    args: &crate::core::json::Json,
) -> Result<Vec<SocketAddr>, FfiError> {
    match args.get("candidates") {
        None | Some(crate::core::json::Json::Null) => Ok(Vec::new()),
        Some(value) => {
            let texts = value
                .as_str_array()
                .ok_or_else(|| type_error("candidates", "an array of ip:port strings"))?;
            let mut out = Vec::with_capacity(texts.len());
            for text in &texts {
                let addr: SocketAddr = text.parse().map_err(|_| {
                    FfiError::invalid(format!(
                        "field \"candidates\" holds a non ip:port entry: {text:?}"
                    ))
                })?;
                out.push(addr);
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// The registry task-pairing stack (lazily built by the first create/join).
// ---------------------------------------------------------------------------

/// Build (or fetch) this runtime's [`TaskStack`]: a REAL v2 registry
/// client (pinned TLS, the runtime's device key and display name), the
/// orchestrator (journal + trust book under `id_dir`), the `TaskPairing`
/// on top, and its resident tasks — the task-aware owner loop (behind the
/// pre-commit redeem-offer gate, P1-05) and the event pump. Runs inside
/// op bodies, i.e. on the engine's embedded runtime (the client's reader
/// tasks spawn there).
///
/// The build is serialized by [`TaskRt::stack_init`] (wave-29 P2-02):
/// concurrent first ops await the gate and then find the finished stack,
/// so exactly ONE registry login ever runs — no duplicate device login
/// (the server's "new connection closes the old" would strand the saved
/// stack), no orphaned replacement client. A failed build leaves the slot
/// empty, so a later op retries cleanly.
pub(super) async fn ensure_stack(rt: &Arc<TaskRt>) -> Result<Arc<TaskStack>, String> {
    if let Some(stack) = rt
        .stack
        .lock()
        .map_err(|_| "task stack lock poisoned".to_string())?
        .clone()
    {
        return Ok(stack);
    }
    if rt.is_stopped() {
        return Err("task runtime is stopped".to_string());
    }
    // One initializer at a time; the gate is released on both success and
    // failure (a failed login must be retryable by the next op).
    let init_gate = rt.stack_init.lock().await;
    if let Some(stack) = rt
        .stack
        .lock()
        .map_err(|_| "task stack lock poisoned".to_string())?
        .clone()
    {
        return Ok(stack);
    }
    if rt.is_stopped() {
        return Err("task runtime is stopped".to_string());
    }
    // Batch G6 (R36-P2-04 client half): claim the identity's
    // control-connection lock BEFORE the registry dial — the registry
    // replaces older same-device connections, so without this a CLI task
    // run or a daemon on the same id-dir (or a second task runtime in
    // this same engine) would silently kick this stack's connection and
    // be kicked back by its keeper forever. The lease (guard + heartbeat)
    // releases itself on every failure path below and otherwise moves
    // into the stack, holding until teardown.
    let connection_lock =
        ak_session::pairing::ConnectionLockLease::acquire(&rt.id_dir).map_err(|error| {
            format!(
                "cannot claim the identity's control-connection lock under {}: {error}",
                rt.id_dir.display()
            )
        })?;
    let connect = RegistryClient::connect_with_config(
        rt.registry_addr,
        rt.registry_id,
        &rt.key,
        &rt.device_name,
        ClientConfig::default(),
    );
    let client = match tokio::time::timeout(REGISTRY_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return Err(format!("registry connect failed: {error}")),
        Err(_) => return Err("registry connect timed out".to_string()),
    };
    let orch = PairingOrchestrator::open(
        client,
        rt.key.clone(),
        &rt.id_dir,
        OrchestratorConfig {
            // The v1 task window a code owner has to answer a redeem: a
            // live owner loop answers in milliseconds; an unknown code,
            // an offline owner or a revoked code surfaces the unified
            // "unavailable" answer at this deadline instead of hanging
            // the join op for the 90 s library default.
            redeem_window_ms: TASK_REDEEM_WINDOW_MS,
            ..OrchestratorConfig::default()
        },
    )
    .map_err(|error| {
        format!(
            "cannot open the task pairing journal under {}: {error}",
            rt.id_dir.display()
        )
    })?;
    let pairing = TaskPairing::open(
        orch,
        TaskPairingConfig {
            ttl_secs: TASK_CODE_TTL_SECS,
            probe: TASK_PROBE_OPTIONS,
        },
    );
    let stack = Arc::new(TaskStack {
        pairing,
        owner_loop: StdMutex::new(None),
        event_pump: StdMutex::new(None),
        keeper: StdMutex::new(None),
        connection_lock: StdMutex::new(Some(connection_lock)),
    });
    // Wave 36 (R36-P1-02): the shared connection keeper from ak-session
    // (RegistryClient::spawn_connection_keeper — the same component the
    // CLI task plane and the daemon use since this wave): 20s ping inside
    // the 30s FRAME_IO_DEADLINE, bounded-backoff reconnect on Disconnected
    // (a failed batch now terminal-publishes Disconnected, so recovery
    // continues across prolonged outages), 3-timeout black-hole recovery.
    // Shutdown rides the runtime-stopped watch: stop flips it, the keeper
    // exits; abort_tasks still aborts the handle for prompt teardown.
    {
        let client = stack.pairing.orchestrator().client().clone();
        // Shutdown: poll the runtime-stopped flag at the ping cadence —
        // abort_tasks() also aborts the handle on teardown, so the poll
        // is a cooperative fast path, not the only exit.
        let stop_rt = Arc::clone(rt);
        let (keeper_stop_tx, keeper_stop_rx) = tokio::sync::watch::channel(false);
        let _watcher = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                if stop_rt.is_stopped() {
                    let _ = keeper_stop_tx.send(true);
                    break;
                }
                interval.tick().await;
            }
        });
        let keeper = client.spawn_connection_keeper(keeper_stop_rx);
        if let Ok(mut slot) = stack.keeper.lock() {
            *slot = Some(keeper);
        }
    }
    // Wave 30 (review 2026-11-07, P1-05): the PRODUCTION owner loop runs
    // behind the same pre-commit redeem-offer gate the library's
    // `TaskPairing::spawn_owner_loop` installs. This stack deliberately
    // spawns `spawn_owner_loop_verified` directly (the synchronous
    // fingerprint registration below must run inside the completion
    // callback), so the gate is installed explicitly FIRST — without it a
    // wrong-direction or endpoint-less join completed the pairing and
    // burned the code's single redeem before the post-completion check
    // refused it.
    stack.pairing.install_redeem_offer_gate();
    // The task-aware owner loop: ak-session's own completion bookkeeping
    // (direction rule + active-task registry) PLUS the task plane's —
    // the connection entry, `connected` event and probe. `spawn_owner_
    // loop_verified` invokes the callback inside the runtime, so the
    // completion work can be spawned straight away.
    let owner_stack = Arc::clone(&stack);
    let owner_rt = Arc::clone(rt);
    let on_paired: Arc<dyn Fn(VerifiedPairing) + Send + Sync> = Arc::new(move |verified| {
        let code_id = verified.record().transcript.code_id;
        let my_direction = owner_stack
            .pairing
            .active_tasks()
            .into_iter()
            .find(|task| task.code == code_id)
            .map(|task| task.direction);
        let Some(direction) = my_direction else {
            return; // not one of this runtime's task codes
        };
        // Completion refused (direction conflict / malformed blob): the
        // pairing stands server-side but this side refuses to register
        // the task — no `connected` event is the honest surface
        // (ak-session logged the refusal on its own stream).
        if let Ok(paired) = owner_stack
            .pairing
            .complete_owner_pairing(&verified, direction)
        {
            // The connection entry and `connected` event are inserted
            // SYNCHRONOUSLY here, before any await: the resident
            // receiver's P1-04 admission is bound to this entry, and the
            // peer's join may already be returning on the other device —
            // its send can dial this endpoint within milliseconds.
            owner_side_bookkeeping(&owner_rt, code_id, &paired);
            let rt = Arc::clone(&owner_rt);
            let stack = Arc::clone(&owner_stack);
            tokio::spawn(async move {
                owner_side_probe(&rt, &stack, paired).await;
            });
        }
    });
    let owner_loop = stack
        .pairing
        .orchestrator()
        .spawn_owner_loop_verified(Some(on_paired));
    let pump = spawn_task_event_pump(Arc::clone(rt), &stack);
    let mut unbuilt: Option<[JoinHandle<()>; 2]> = Some([owner_loop, pump]);
    // Install under the stack lock, re-checking `stopped` INSIDE it: either
    // teardown took the slot first (we see `stopped` and abort what we
    // built) or we install first (teardown then finds and closes our
    // stack). No live registry client can slip past a teardown.
    {
        let mut slot = rt
            .stack
            .lock()
            .map_err(|_| "task stack lock poisoned".to_string())?;
        if !rt.is_stopped() {
            if let Some([owner_loop, pump]) = unbuilt.take() {
                if let Ok(mut handle) = stack.owner_loop.lock() {
                    *handle = Some(owner_loop);
                }
                if let Ok(mut handle) = stack.event_pump.lock() {
                    *handle = Some(pump);
                }
            }
            *slot = Some(Arc::clone(&stack));
        }
    }
    if let Some(tasks) = unbuilt {
        // Teardown ran mid-build: nothing may install a live registry
        // client past the engine's death — abort what we built.
        for task in tasks {
            task.abort();
        }
        let client = stack.pairing.orchestrator().client().clone();
        let _ = tokio::time::timeout(Duration::from_millis(200), client.close()).await;
        return Err("task runtime is stopped".to_string());
    }
    drop(init_gate);
    Ok(stack)
}

/// Pump the [`TaskEvent`] broadcast into the task event queue. `Local`
/// ends are skipped — `ak_task_disconnect` pushes that line directly, so
/// the pump repeating it would duplicate the event.
fn spawn_task_event_pump(rt: Arc<TaskRt>, stack: &Arc<TaskStack>) -> JoinHandle<()> {
    let mut events = stack.pairing.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(TaskEvent::TaskDisconnected { code, reason }) => {
                    if reason == TaskDisconnectReason::Local {
                        continue;
                    }
                    let Some(conn) = rt.conn_of_code(&code) else {
                        continue;
                    };
                    // The frozen ffi-v2 §8.3 vocabulary: `peer` for a peer
                    // departure, `receiver_failed` for a data-plane link
                    // loss. Wave 30 (review 2026-11-07, P2-08): a
                    // registry/control-connection drop no longer produces
                    // `Network` at all — ak-session's task layer only
                    // reports it for the task's OWN data plane now, so
                    // `receiver_failed` is always a genuine data-plane
                    // terminal verdict and the Dart terminal-reason table
                    // agrees with it.
                    let text = match reason {
                        TaskDisconnectReason::ByPeer => "peer",
                        _ => "receiver_failed",
                    };
                    rt.push_disconnected_once(&conn.conn_id, text);
                }
                // A slow consumer lagging the 64-slot ring just misses
                // events; the state flip already happened either way.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// The owner side of a completed registry pairing, synchronous half: the
/// connection entry (conn id = the pairing transaction's id, identical on
/// both sides) and the `connected` event. Runs inside the owner-loop
/// callback (see `ensure_stack`) so the entry exists before the peer's
/// returning join op can possibly dial this endpoint.
fn owner_side_bookkeeping(rt: &Arc<TaskRt>, code_id: [u8; 16], paired: &TaskPaired) {
    if rt.conn_of_code(&code_id).is_some() {
        return; // duplicate completion (Hello catch-up re-push): dedup
    }
    let role = TaskRole::of_direction(paired.direction);
    let conn_id = paired.pairing_id;
    let candidates = paired.peer.candidates.clone();
    let endpoint = order_candidates(candidates.clone())
        .first()
        .copied()
        .map(|addr| quoted(&addr.to_string()))
        .unwrap_or_else(|| "null".to_string());
    let display = display_of_code(rt, &code_id);
    {
        let Ok(mut lifetimes) = rt.sender_lifetimes.lock() else {
            return;
        };
        if role == TaskRole::Sender {
            lifetimes.entry(conn_id).or_insert_with(|| {
                SenderLifetime::new(tokio::time::Instant::now() + OPENER_KEEPALIVE_BUDGET)
            });
        }
        let Ok(mut conns) = rt.conns.lock() else {
            return;
        };
        conns.insert(
            conn_id,
            TaskConn {
                conn_id,
                role,
                state: ConnState::Probing,
                candidates: candidates.clone(),
                peer_endpoint: order_candidates(candidates.clone()).first().copied(),
                peer_fp: Some(paired.verified.redeemer_fingerprint()),
                // X5: the joiner's self-reported blob name rides the
                // record (display-only; the verified `peer_fp` above is
                // the identity anchor).
                peer_name: Some(paired.peer.name.clone()),
                code_id,
                code: display.clone(),
            },
        );
    }
    if let Some(conn) = rt.conn_of_code(&code_id) {
        ensure_sender_lifetime(rt, &conn);
    }
    rt.push_event(format!(
        "{{\"event\":\"connected\",\"conn\":\"{}\",\"code_id\":\"{}\",\"role\":{},\"peer_endpoint\":{endpoint},\"code\":{},\"peer_name\":{},\"peer_fp_short\":{}}}",
        core::hex_encode(&conn_id),
        // Wave-31 R30-P2-02: both sides of the pairing share the SAME
        // code id — the joiner (which never saw the create result) fills
        // its conn→code mapping from this field.
        core::hex_encode(&code_id),
        quoted(role.as_str()),
        quoted(&display),
        // X5 (batch X, 2026-11-22, ffi-v2 §8.3 批次 X 增补): the joiner's
        // self-reported device name plus the VERIFIED fingerprint's short
        // form — the name is display-only, the short code is the identity
        // anchor the UI annotates it with.
        quoted(&paired.peer.name),
        quoted(&paired.verified.redeemer_fingerprint().short_id()),
    ));
}

/// The owner side of a completed registry pairing, asynchronous half: the
/// real reachability probe over the peer's advertised candidates and the
/// connection's honest `probing → connected|unreachable` settle (wave-30
/// P2-09 residual: a total probe failure settles to `unreachable`, never
/// a fake `connected`).
///
/// CR-P1-07 (review 2026-11-13 §4.4 改法 1/2): the sender role now runs
/// its PAIRING-TIME opener + mapping keepalive here (the same shared
/// `task_net::sender_pairing_keeper` the CLI runs — identical timing), and
/// its connectivity test rides the ADOPTED dial socket
/// ([`adopted_socket_probe`]) instead of a throwaway socket. A receiver
/// owner keeps the CR-P1-05 punch plus the pairing-plane probe: its QUIC
/// socket is already owned by quinn, so a receive round on a clone would
/// compete for the socket queue.
async fn owner_side_probe(rt: &Arc<TaskRt>, stack: &Arc<TaskStack>, paired: TaskPaired) {
    let conn_id = paired.pairing_id;
    let role = TaskRole::of_direction(paired.direction);
    // CR-P1-05, receiver half (the create-code entry point): the pairing
    // just completed and the SENDER's advertised candidates are known —
    // punch them from the resident receiver's QUIC-socket clone,
    // concurrently with the sender's dial. Owners of sender-role codes
    // run their own opener instead (their peer is the receiver).
    match role {
        TaskRole::Receiver => {
            spawn_receiver_punch(rt, paired.code_id, &paired.peer.candidates);
        }
        TaskRole::Sender => {
            spawn_sender_opener(rt, paired.code_id, &paired.peer.candidates);
        }
    }
    let outcome = if role == TaskRole::Sender {
        adopted_socket_probe(rt, &paired.code_id, &paired.peer.candidates).await
    } else {
        let endpoint_probe = PeerEndpoint {
            candidates: paired.peer.candidates.clone(),
            name: paired.peer.name.clone(),
            direction: paired.peer.direction,
        };
        probe_with_retry(&stack.pairing, &endpoint_probe).await
    };
    settle_probe_state(rt, &conn_id, &outcome);
    let (line, _verdict) = probe_result_line(&conn_id, &outcome);
    rt.push_event(line);
}

/// Settle one connection's `probing` state by the probe's honest verdict
/// (wave-30 P2-09 residual): success → `connected`, total failure →
/// `unreachable`. A connection that already reached a terminal state
/// (`disconnected`) keeps it; `ak_task_list` and the join result agree
/// with the `probe_result` event.
pub(super) fn settle_probe_state(rt: &Arc<TaskRt>, conn_id: &[u8; 16], outcome: &ProbeOutcome) {
    let reachable = matches!(outcome, ProbeOutcome::Success { .. });
    if let Ok(mut conns) = rt.conns.lock() {
        if let Some(conn) = conns.get_mut(conn_id) {
            if conn.state == ConnState::Probing {
                conn.state = if reachable {
                    ConnState::Connected
                } else {
                    ConnState::Unreachable
                };
            }
        }
    }
}

/// The owner's display form of one of its cached codes (fallback: the
/// code id hex — the registry display never travelled back on this path).
fn display_of_code(rt: &TaskRt, code_id: &[u8; 16]) -> String {
    rt.codes
        .lock()
        .ok()
        .and_then(|table| {
            table
                .values()
                .find(|entry| &entry.code_id == code_id)
                .map(|entry| entry.display.clone())
        })
        .unwrap_or_else(|| core::hex_encode(code_id))
}

/// Render the `probe_result` event line + the join-result verdict of one
/// probe outcome (`reachable` carries the confirmed address, or false
/// with one bounded reason per candidate).
pub(super) fn probe_result_line(
    conn_id: &[u8; 16],
    outcome: &ProbeOutcome,
) -> (String, &'static str) {
    match outcome {
        ProbeOutcome::Success { address } => (
            format!(
                "{{\"event\":\"probe_result\",\"conn\":\"{}\",\"endpoint\":{},\"reachable\":true}}",
                core::hex_encode(conn_id),
                quoted(&address.to_string()),
            ),
            "verified",
        ),
        ProbeOutcome::AllFailed { failures } => {
            let list = failures
                .iter()
                .map(|failure| {
                    format!(
                        "{{\"candidate\":{},\"reason\":{}}}",
                        quoted(&failure.candidate.to_string()),
                        quoted(failure.reason),
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            (
                format!(
                    "{{\"event\":\"probe_result\",\"conn\":\"{}\",\"endpoint\":null,\"reachable\":false,\"failures\":[{list}]}}",
                    core::hex_encode(conn_id),
                ),
                "all_failed",
            )
        }
    }
}

/// Render the `probe_observed` event line of one send-time probe round
/// (batchF G2 / CR-P3-06 diagnostics; contract ffi-v2 §8.3, prose
/// registration beside `sender_bind`): exactly ONE bounded line per
/// round — the observed peer address, its candidate kind and the dial
/// phase — whether or not anything was observed (the fail-open shape
/// mirrors the round itself). `kind` follows
/// [`task_net::observed_dial_head`]'s two rules: a head inside
/// `report.reachable` is an ADVERTISED candidate the round
/// echo-confirmed (`candidate`); any other head is the raw source of a
/// PEER probe from outside the candidate list (`observed`); no head at
/// all is `none`.
pub(super) fn probe_observed_event(
    conn_id: &[u8; 16],
    head: Option<SocketAddr>,
    report: &ak_net::holepunch::PunchReport,
) -> String {
    let kind = match head {
        Some(head) if report.reachable.contains(&head) => "candidate",
        Some(_) => "observed",
        None => "none",
    };
    format!(
        "{{\"event\":\"probe_observed\",\"conn\":\"{}\",\"observed\":{},\"kind\":\"{kind}\",\"phase\":\"send\",\"reachable\":{},\"packets_in\":{},\"elapsed_ms\":{}}}",
        core::hex_encode(conn_id),
        head.map_or_else(|| "null".to_string(), |addr| quoted(&addr.to_string())),
        report.reachable.len(),
        report.packets_received,
        report.elapsed.as_millis(),
    )
}

/// Bind the probe echo responder (the shared loop lives in
/// `task_net::echo_responder_loop`): a small UDP socket that answers
/// ak-net hole-punch probe packets from any of this device's advertised
/// addresses, so the peer's `probe_peer` genuinely confirms two-way
/// reachability (the QUIC receiver itself never speaks the probe
/// exchange). Registered under `key` in the spawned table (teardown /
/// disconnect abort it).
///
/// Since CR-P1-02 改法第 3 条 the responder also OBSERVES: every genuine
/// probe's source lands in [`TaskRt::echo_observed`] (most-recent, one
/// slot, normalized to plain-family form) — the receiver side's raw
/// hole-punch observation, consumed by [`close_notify_peer`] for its
/// one outbound dial role. The QUIC accept itself is already bound to
/// the receiver's own socket and needs no dial, so the observation is
/// deliberately NOT plumbed into the accept path — see the struct's
/// honest-scoping note.
pub(super) fn spawn_echo_responder(rt: &Arc<TaskRt>, key: [u8; 16]) -> Result<SocketAddr, String> {
    let (socket, addr) = ak_session::pairing::task_net::bind_echo_responder_socket()?;
    let observed_slot = Arc::clone(&rt.echo_observed);
    let task = tokio::spawn(async move {
        // The FFI side reports nothing per observation (the event set is
        // contract-frozen) — the observation slot is the whole surface.
        ak_session::pairing::task_net::echo_responder_loop(socket, observed_slot, |_| {}).await;
    });
    if let Ok(mut spawned) = rt.spawned.lock() {
        spawned.entry(key).or_default().push(task);
    }
    Ok(addr)
}

/// Abort and forget the resident tasks registered under `from` and
/// re-register them under the registry-assigned `code_id` (the bind
/// happens before the claim, under a local placeholder id). CR-P1-05:
/// the code's hole socket (stashed under the placeholder by the resident
/// receiver) re-keys with them.
pub(super) fn rekey_spawned(rt: &TaskRt, from: [u8; 16], code_id: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&from) {
            spawned.entry(code_id).or_default().extend(tasks);
        }
    }
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        if let Some(socket) = holes.remove(&from) {
            holes.insert(code_id, socket);
        }
    }
}

/// Abort the resident tasks registered under a placeholder id (the op
/// failed after binding — nothing may leak past the failed op). CR-P1-05:
/// the placeholder's hole socket goes with them (its punch stop flips so
/// a spawned punch loop ends too).
pub(super) fn abort_spawned(rt: &TaskRt, key: [u8; 16]) {
    if let Ok(mut spawned) = rt.spawned.lock() {
        if let Some(tasks) = spawned.remove(&key) {
            for task in tasks {
                task.abort();
            }
        }
    }
    drop_hole_socket(rt, &key);
}

/// Release UDP resources without owning the pairing's policy deadline.
pub(super) fn drop_hole_socket(rt: &TaskRt, code_id: &[u8; 16]) {
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        if let Some(socket) = holes.remove(code_id) {
            socket.punch_stop.store(true, Ordering::Relaxed);
        }
    }
}

/// CR-P1-05: stash one code's hole socket (sender: the adopted dial
/// socket; receiver: the send-only QUIC-socket clone).
pub(super) fn stash_hole_socket(rt: &TaskRt, code_id: [u8; 16], socket: std::net::UdpSocket) {
    if let Ok(mut holes) = rt.hole_sockets.lock() {
        holes.insert(
            code_id,
            HoleSocket {
                socket,
                punch_stop: Arc::new(AtomicBool::new(false)),
            },
        );
    }
}

/// CR-P1-05, receiver half: spawn the punch loop toward the paired
/// SENDER's advertised candidates from the receiver's stashed QUIC-socket
/// clone. Called the moment the pairing completes (both entry points —
/// the owner-loop completion and the joiner's redeem return) so the
/// punches overlap the sender's dial. No clone stashed (e.g. an older
/// receiver bind path) is an honest no-op; the loop itself is bounded by
/// [`PUNCH_WINDOW`] and stopped early by the accept path's flag.
pub(super) fn spawn_receiver_punch(
    rt: &Arc<TaskRt>,
    code_id: [u8; 16],
    sender_candidates: &[SocketAddr],
) {
    let (socket, stop) = {
        let Ok(holes) = rt.hole_sockets.lock() else {
            return;
        };
        let Some(entry) = holes.get(&code_id) else {
            return;
        };
        match entry.socket.try_clone() {
            Ok(clone) => (clone, Arc::clone(&entry.punch_stop)),
            Err(_) => return,
        }
    };
    let candidates = sender_candidates.to_vec();
    tokio::spawn(async move {
        ak_session::pairing::task_net::punch_candidates(socket, &candidates, &stop, |_| {}).await;
    });
}
