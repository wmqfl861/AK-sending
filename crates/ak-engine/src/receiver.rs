//! The receiving side: entry points, job setup, the frozen receive body and
//! `serve_concurrently`.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

struct ReceiverJob {
    manifest: Manifest,
}

/// Run one receive to completion (one job, then returns). `addr_tx` receives
/// the bound address before the first accept, so callers can display it.
pub async fn receive_with_addr(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
    addr_tx: tokio::sync::oneshot::Sender<SocketAddr>,
) -> Result<Report, EngineError> {
    receive_with_addr_consent(spec, progress, addr_tx, None).await
}

/// [`receive_with_addr`] with the optional receive-side user-consent hook
/// (wave 28): `Some(callback)` is asked once at the Offer handling point
/// with the offer summary ([`ConsentInfo`]); `false` answers the DECLINED
/// error frame to the peer, closes the connection and fails with
/// [`EngineError::ConsentDeclined`] before any payload byte is written.
/// `None` is the frozen automatic-accept behavior.
pub async fn receive_with_addr_consent(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
    addr_tx: tokio::sync::oneshot::Sender<SocketAddr>,
    consent: Option<TransferConsent>,
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;

    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let local = endpoint.local_addr()?;
    let _ = addr_tx.send(local);
    let channel = endpoint.accept(&spec.authorize).await?;
    // One connection through the shared single-job protocol body — the very
    // same body [`serve_concurrently`] runs per connection. Legacy path:
    // no admission decider, so no CAP_SCHEDULER bit and no staging.
    receive_job(
        channel,
        &target,
        &JobSpec::of(&spec),
        started,
        None,
        progress,
        consent.as_ref(),
    )
    .await
}

/// Wave-21 on-channel receive entry (scheduler.md §6), hardened wave 23:
/// run one receive on an already-accepted [`DirectChannel`] the runtime
/// handed in, with the scheduler admission handshake in front of the frozen
/// receive body.
///
/// * the Hello advertises `CAP_SCHEDULER`; when the peer's does too
///   ([`ak_wire::scheduler_frames_allowed`]), the body first reads the
///   sender's `TaskOpen` and hands its *complete* admission context
///   ([`AdmissionRequest`]: code group + registry + pairing relation) to
///   `admission_decider`: [`AdmissionDecision::Admit`] answers a granted
///   `Admission` (with the bounded lease), waits for the sender's
///   `AdmissionAck`, and only then accepts the Offer;
///   [`AdmissionDecision::Deny`] answers the refusal and fails locally
///   with [`EngineError::AdmissionDenied`] — no Offer is read;
/// * an admitted task stages its `.akpart` files under its own per-task
///   tree (`ak_storage::prepare_file_staged`, keyed by the wire task id)
///   and the tree is discarded when the task reaches a terminal state
///   (completion, or cancellation);
/// * a peer without `CAP_SCHEDULER` is refused (wave 23, review P1-4):
///   this entry exists precisely for scheduled tasks, so a legacy sender
///   cannot silently downgrade the receive to the unadmitted body — the
///   call fails with `PROTOCOL_UNSUPPORTED` and the channel closes,
///   symmetric with the send side's refusal. (The legacy
///   `receive_with_addr`/`serve_concurrently` entries still run the frozen
///   unstaged body for their own connections.)
///
/// `spec.bind`/`spec.authorize` are unused here (the channel exists); the
/// destination, conflict policy, checkpoints, idle timeout and policy gate
/// all still apply exactly as in [`receive_with_addr`].
pub async fn receive_on_channel(
    channel: DirectChannel,
    spec: ReceiveSpec,
    admission_decider: AdmissionDecider<'_>,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    receive_on_channel_consent(channel, spec, admission_decider, progress, None).await
}

/// [`receive_on_channel`] with the optional receive-side user-consent hook
/// (wave 28): `Some(callback)` is asked once at the Offer handling point
/// with the offer summary ([`ConsentInfo`]); `false` answers the DECLINED
/// error frame to the peer, closes the connection and fails with
/// [`EngineError::ConsentDeclined`] before any payload byte is written.
/// `None` is the frozen automatic-accept behavior.
pub async fn receive_on_channel_consent(
    channel: DirectChannel,
    spec: ReceiveSpec,
    admission_decider: AdmissionDecider<'_>,
    progress: &(dyn Fn(Progress) + Send + Sync),
    consent: Option<TransferConsent>,
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;
    receive_job(
        channel,
        &target,
        &JobSpec::of(&spec),
        started,
        Some(admission_decider),
        progress,
        consent.as_ref(),
    )
    .await
}

/// One receive on a channel the CALLER already accepted and authorized
/// (wave 29, review P1-04/P2-05): the frozen LEGACY single-job body — the
/// very same one [`receive_with_addr_consent`] runs after its internal
/// bind+accept — driven on an externally owned [`DirectChannel`].
///
/// This is the entry a caller-side accept LOOP needs: it can keep one
/// endpoint (one advertised port) bound across many connections, reject
/// unauthorized TLS peers at `accept` without consuming the listener, and
/// still run each authorized channel through the unmodified receive
/// protocol with the optional user-consent hook. `receive_on_channel*`
/// cannot serve that role — its admission decider makes it refuse every
/// legacy (non-CAP_SCHEDULER) sender by construction.
///
/// `spec.bind`/`spec.authorize` are unused here (the channel exists and was
/// authorized by the caller); the destination, conflict policy,
/// checkpoints, idle timeout and policy gate all apply exactly as in
/// [`receive_with_addr`]. The channel closes when this returns.
pub async fn receive_accepted_consent(
    channel: DirectChannel,
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
    consent: Option<TransferConsent>,
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;
    receive_job(
        channel,
        &target,
        &JobSpec::of(&spec),
        started,
        None,
        progress,
        consent.as_ref(),
    )
    .await
}

/// Daemon-side scheduler handshake (wave 27, scheduler.md §2/§6): on an
/// already-accepted [`DirectChannel`], run the receive head — peer
/// identity re-check, policy gate, the Hello exchange — and split by the
/// peer's capabilities, so the upper layer (ak-session) can read the
/// sender's `TaskOpen`, bind it to its two-dimensional code group and
/// ask its scheduler for a permit BEFORE the engine answers `Admission`.
///
/// Identity semantics, frozen to the receive path's: `spec.authorize` is
/// the same callback type `DirectEndpoint::accept` and
/// [`ReceiveSpec::authorize`] use, re-run here against the channel's
/// verified TLS fingerprint — a refusal closes the connection with the
/// accept path's code and surfaces
/// `EngineError::Connect(ConnectError::Unauthorized)`, exactly what
/// `receive_with_addr` returns for an unauthorized accept. Hello version
/// mismatches fail with the same `PROTOCOL_UNSUPPORTED` peer error
/// [`receive_on_channel`] produces (``expect_hello``).
///
/// Wire behavior: our Hello always advertises `CAP_SCHEDULER` (this entry
/// exists for the scheduler path), so the split is decided purely by the
/// peer's bits —
///
/// * peer WITH `CAP_SCHEDULER`: its `TaskOpen` is read (the fail-closed
///   sentinel rules of the combined path apply — the all-zero task id,
///   pairing id and registry are protocol violations, never handed
///   upward) and returned retained in
///   [`SchedulerHandshake::Scheduler`] together with the live protocol
///   stream. The `Admission` answer is deliberately NOT sent here; it is
///   [`receive_on_channel_preadmitted`]'s first protocol byte.
/// * peer WITHOUT `CAP_SCHEDULER`: [`SchedulerHandshake::Legacy`] with
///   the peer's capability bits. No `TaskOpen` is read (a legacy sender
///   never sends one). The accepted protocol stream — whose Hello
///   exchange already happened — closes when this function returns; the
///   channel itself stays open and caller-owned. Such peers are served
///   by the daemon's legacy receive path, not by this entry pair.
///
/// No `Progress` events are emitted (this is the pre-decision phase; the
/// transfer entry emits from `Connected` onward).
pub async fn channel_scheduler_handshake(
    channel: &mut DirectChannel,
    spec: &ReceiveSpec,
) -> Result<SchedulerHandshake, EngineError> {
    // Peer identity re-check with the accept path's authorize shape and
    // refusal semantics (see the method docs): the same condition
    // `DirectEndpoint::accept` enforces at TLS time, re-derived here
    // because this entry receives the channel after the fact.
    if !(spec.authorize)(channel.peer_fingerprint()) {
        channel.close(1, b"unauthorized");
        return Err(EngineError::Connect(ak_net::ConnectError::Unauthorized));
    }
    let job = JobSpec::of(spec);
    let (tx, mut rx, peer_caps) = receive_hello_exchange(channel, &job, true, &|_| {}).await?;
    if peer_caps & CAP_SCHEDULER == 0 {
        // Legacy peer: no TaskOpen will ever arrive. Hand the
        // Hello-exchanged stream back so the caller can resume the frozen
        // unstaged body via `receive_after_hello` on this SAME connection
        // (wave 27 integrator fix: dropping the halves here killed the
        // legacy peer's connection right after its Hello).
        return Ok(SchedulerHandshake::Legacy {
            peer_capabilities: peer_caps,
            stream: HelloStream { tx, rx },
        });
    }
    let opened = recv_task_open(&mut rx, &engine_limits(), job.idle_timeout).await?;
    // Sentinel fail-closed set (direction deliberately retained verbatim:
    // it is the pre-admitted entry's frozen validation).
    validate_task_open(&opened, false)?;
    Ok(SchedulerHandshake::Scheduler {
        open: TaskOpenAdmission {
            job_id: opened.task_id,
            permit_id: opened.permit_id,
            group: opened.request.group,
            group_registry: opened.request.group_registry,
            pairing_id: opened.request.pairing_id,
            direction: opened.direction,
            peer_caps,
            tx,
            rx,
        },
    })
}

/// Pre-admitted on-channel receive (wave 27): the daemon already read the
/// sender's `TaskOpen` through [`channel_scheduler_handshake`], validated
/// the pairing relation against its store and obtained its scheduler's
/// permit — this entry performs the wire half: answer the granted
/// `Admission` (with the bounded ``ADMISSION_LEASE_MS`` lease), wait for
/// the sender's `AdmissionAck`, then run the exact frozen receive body of
/// [`receive_on_channel`] (per-task staging tree keyed by the wire task
/// id included).
///
/// Checks re-run here, per the "engine entries keep their defenses"
/// rule (scheduler.md §4): `spec.authorize` against the channel's peer
/// fingerprint (refusal semantics of the accept path), the policy gate
/// before the first protocol byte this entry writes, and the wire-level
/// fail-closed rules on the retained `open` — in particular
/// `open.direction` must equal the sending direction (the peer is the
/// sender of this task); anything else is the same
/// `PROTOCOL_UNSUPPORTED` protocol violation the combined path produces,
/// and no `Admission` is ever answered.
///
/// The `AdmissionAck` must name the same task and permit as the retained
/// `open`; a mismatch is a protocol violation. From the granted ack on,
/// the caller's wire position is identical to a task admitted through
/// [`receive_on_channel`].
pub async fn receive_on_channel_preadmitted(
    channel: DirectChannel,
    spec: ReceiveSpec,
    open: TaskOpenAdmission,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    receive_on_channel_preadmitted_consent(channel, spec, open, progress, None).await
}

/// [`receive_on_channel_preadmitted`] with the optional receive-side
/// user-consent hook (wave 28): `Some(callback)` is asked once at the Offer
/// handling point with the offer summary ([`ConsentInfo`]); `false` answers
/// the DECLINED error frame to the peer, closes the connection and fails
/// with [`EngineError::ConsentDeclined`] before any payload byte is
/// written. `None` is the frozen automatic-accept behavior — the daemon's
/// default until the UI wires a prompt.
pub async fn receive_on_channel_preadmitted_consent(
    channel: DirectChannel,
    spec: ReceiveSpec,
    open: TaskOpenAdmission,
    progress: &(dyn Fn(Progress) + Send + Sync),
    consent: Option<TransferConsent>,
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;
    let job = JobSpec::of(&spec);
    // Defense in depth (the handshake already checked both on the same
    // channel): identity first, policy gate before the first byte this
    // entry writes.
    if !(spec.authorize)(channel.peer_fingerprint()) {
        channel.close(1, b"unauthorized");
        return Err(EngineError::Connect(ak_net::ConnectError::Unauthorized));
    }
    gate_channel(&job.policy, &channel)?;
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });

    let TaskOpenAdmission {
        job_id: task_id,
        permit_id,
        group,
        group_registry,
        pairing_id,
        direction,
        peer_caps,
        tx,
        rx,
    } = open;
    // Full fail-closed set on the retained fields, including the frozen
    // direction contract of this entry (sentinels are re-checked as
    // defense: nothing between the two calls can weaken the handshake's
    // validation, but the check is cheap and this entry keys staging by
    // the task id).
    let opened = TaskOpenWire {
        task_id,
        permit_id,
        request: AdmissionRequest {
            group,
            group_registry,
            pairing_id,
        },
        direction,
    };
    validate_task_open(&opened, true)?;

    let limits = engine_limits();
    let mut tx = tx;
    let mut rx = rx;
    // The granted answer, mirroring `admission_handshake_receiver`'s
    // grant arm byte for byte (bounded lease, reason ok).
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Admission {
            job_id: task_id,
            permit_id,
            granted: true,
            reason: AdmissionReason::Ok,
            lease_ms: ADMISSION_LEASE_MS,
        },
    })
    .await?;
    // Final handshake step: the sender confirms it acts on the granted
    // permit; the ids must be the ones this task was opened with.
    match recv_admission_frame(&mut rx, &limits, job.idle_timeout).await? {
        Frame {
            message:
                Message::AdmissionAck {
                    job_id: acked_job,
                    permit_id: acked_permit,
                },
            ..
        } => {
            if acked_job != task_id || acked_permit != permit_id {
                return Err(protocol_error(
                    "admission ack names a different task or permit",
                ));
            }
        }
        Frame {
            message: Message::Error { code, message },
            ..
        } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    }
    let store = FileCheckpointStore::open(&job.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    receive_frozen_body(
        channel,
        tx,
        rx,
        peer_caps,
        Some(task_id),
        &target,
        &job,
        started,
        &store,
        progress,
        consent.as_ref(),
    )
    .await
}

/// Legacy continuation (wave 27): run the frozen UNSTAGED receive body on
/// a connection whose Hello exchange already happened inside
/// [`channel_scheduler_handshake`] and whose peer turned out to have no
/// `CAP_SCHEDULER` (a legacy sender). The [`HelloStream`] handed back by
/// the handshake's `Legacy` arm is fed straight in — this entry resumes
/// exactly where the handshake stopped, so the daemon's wired mode can
/// serve legacy peers on the SAME connection instead of dropping it after
/// their Hello.
///
/// Defenses re-run here per the "engine entries keep their defenses" rule
/// (scheduler.md §4): `spec.authorize` against the channel's peer
/// fingerprint (refusal semantics of the accept path) and the policy gate
/// before the first protocol byte. The `Connected` progress event is
/// emitted here (the handshake deliberately emits nothing — it passed a
/// no-op sink — so callers keying `job_started` off `Connected` see
/// exactly one event per task, same as every other receive entry).
///
/// No admission stage exists on this path by construction (the peer never
/// sends a `TaskOpen`); staging is the frozen unstaged behavior of the
/// legacy `receive_with_addr` kernel.
pub async fn receive_after_hello(
    channel: DirectChannel,
    spec: ReceiveSpec,
    hello: HelloStream,
    peer_capabilities: u16,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    if !(spec.authorize)(channel.peer_fingerprint()) {
        channel.close(1, b"unauthorized");
        return Err(EngineError::Connect(ak_net::ConnectError::Unauthorized));
    }
    let job = JobSpec::of(&spec);
    gate_channel(&job.policy, &channel)?;
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let started = Instant::now();
    let target = LocalSaveTarget::open(&spec.out_dir)?;
    let store = FileCheckpointStore::open(&job.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let HelloStream { tx, rx } = hello;
    receive_frozen_body(
        channel,
        tx,
        rx,
        peer_capabilities,
        None,
        &target,
        &job,
        started,
        &store,
        progress,
        None,
    )
    .await
}

/// The per-connection subset of [`ReceiveSpec`] the single-job protocol body
/// ([`receive_job`]) needs. [`ReceiveSpec`] is not `Clone` (its `authorize`
/// callback belongs to the accept side only), so the concurrent receiver
/// clones this small value into every spawned task instead.
#[derive(Debug, Clone)]
struct JobSpec {
    out_dir: PathBuf,
    identity: TlsBundle,
    device_name: String,
    conflict: ConflictPolicy,
    checkpoint_dir: PathBuf,
    idle_timeout: Duration,
    policy: DirectPolicy,
    stats_sink: Option<ChannelStatsSink>,
}

impl JobSpec {
    fn of(spec: &ReceiveSpec) -> Self {
        Self {
            out_dir: spec.out_dir.clone(),
            identity: spec.identity.clone(),
            device_name: spec.device_name.clone(),
            conflict: spec.conflict,
            checkpoint_dir: spec.checkpoint_dir.clone(),
            idle_timeout: spec.idle_timeout,
            policy: spec.policy,
            stats_sink: spec.stats_sink.clone(),
        }
    }
}

/// Does the committed (final-path) file for `entry` still hold exactly the
/// content the manifest promises? Used only when a previous run of the same
/// record already verified every chunk of this file: the re-hash proves the
/// skip is safe, and any mismatch (file deleted, truncated, modified, or
/// replaced by a symlink) falls back to a normal retransfer. Blocking read
/// of the target, same as the part-file re-verification resume already
/// performs; bounded memory (64 KiB buffer).
///
/// Path safety: `entry.rel_path` is validated by the manifest constructors
/// (`validate_rel_path`: no `..`, no rooted paths, no separators besides
/// `/`), and the component walk here accepts `Normal` components only as a
/// second lock — mirroring the rule the storage layer applies when it joins
/// the same path.
fn committed_file_matches(root: &Path, entry: &ak_core::manifest::FileEntry) -> bool {
    use std::io::Read;
    let mut path = root.to_path_buf();
    for component in entry.rel_path.components() {
        match component {
            std::path::Component::Normal(part) => path.push(part),
            _ => return false,
        }
    }
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() || meta.len() != entry.size {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(&path) else {
        return false;
    };
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(_) => return false,
        }
    }
    *hasher.finalize().as_bytes() == entry.whole_digest
}

/// One accepted connection driven through the full receive protocol:
/// policy gate, Hello/(optional admission)/Offer/Accept, manifest pages,
/// resume answer, chunk phase (single-stream or multistream), verify,
/// commit, FinalAck. [`receive_with_addr`], [`serve_concurrently`] and
/// [`receive_on_channel`] all run this verbatim, so the concurrent path is
/// the single-job path — no protocol divergence.
///
/// `admission` is the wave-21 scheduler hook: `Some(decider)` advertises
/// `CAP_SCHEDULER` and, when the peer negotiated it too, runs the
/// TaskOpen→Admission→AdmissionAck handshake whose task id keys the
/// per-task staging tree. `None` (the legacy entries) keeps the frozen v1
/// behavior byte for byte.
///
/// Wave 27 split: the head (policy gate, Hello exchange, admission stage)
/// and the frozen transfer body live in [`receive_hello_exchange`],
/// [`receive_admission_stage`] and [`receive_frozen_body`], shared with
/// the daemon-side entries ([`channel_scheduler_handshake`] /
/// [`receive_on_channel_preadmitted`]) so no copy of the frozen body
/// exists. Behavior is byte for byte what the single function did.
///
/// Concurrency isolation when several of these run at once (see module
/// docs): the checkpoint store is opened per call, and distinct job ids
/// mean distinct `<job>.akjob` files written atomically by exactly one
/// task; `target` is shared immutably (`LocalSaveTarget` holds only a
/// canonicalized root path — no interior mutable state), and different
/// manifest entries map to different `.akpart` files (per-task staging
/// trees under scheduled admission), so a same-path race between two jobs
/// is resolved by the per-file conflict policy.
async fn receive_job(
    channel: DirectChannel,
    target: &LocalSaveTarget,
    spec: &JobSpec,
    started: Instant,
    admission: Option<AdmissionDecider<'_>>,
    progress: &(dyn Fn(Progress) + Send + Sync),
    consent: Option<&TransferConsent>,
) -> Result<Report, EngineError> {
    let limits = engine_limits();
    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let (mut tx, mut rx, peer_caps) =
        receive_hello_exchange(&channel, spec, admission.is_some(), progress).await?;
    let staged_task = receive_admission_stage(
        &mut tx,
        &mut rx,
        &limits,
        spec.idle_timeout,
        admission,
        peer_caps,
    )
    .await?;
    receive_frozen_body(
        channel,
        tx,
        rx,
        peer_caps,
        staged_task,
        target,
        spec,
        started,
        &store,
        progress,
        consent,
    )
    .await
}

/// Shared receive head (wave 27): the policy gate (protocol step 0 — a
/// refused path closes the connection before any stream is accepted or
/// frame moves), the Connected progress event, the accepted bidi stream,
/// our Hello and the peer's. `advertise_scheduler` adds `CAP_SCHEDULER`
/// to our capabilities (the daemon handshake always does; the legacy
/// entries never do). Returns the split stream halves plus the peer's
/// capability bits.
async fn receive_hello_exchange(
    channel: &DirectChannel,
    spec: &JobSpec,
    advertise_scheduler: bool,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(DirectSendStream, DirectRecvStream, u16), EngineError> {
    let limits = engine_limits();
    gate_channel(&spec.policy, channel)?;
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let stream = channel.accept_bi().await?;
    let (mut tx, mut rx) = stream.split();
    let capabilities = scheduler_recv_caps(advertise_scheduler);
    tx.send_frame(&hello_frame(
        &spec.device_name,
        &spec.identity,
        capabilities,
    ))
    .await?;
    let peer_caps = expect_hello(&mut rx, &limits, spec.idle_timeout).await?;
    Ok((tx, rx, peer_caps))
}

/// Our receiver-side capability bits, with or without the scheduler
/// admission bit (wave 27 helper shared by every receive head).
fn scheduler_recv_caps(advertise_scheduler: bool) -> u16 {
    if advertise_scheduler {
        RECV_CAPABILITIES | CAP_SCHEDULER
    } else {
        RECV_CAPABILITIES
    }
}

/// Shared admission stage (wave 27): after both Hellos, either run the
/// combined TaskOpen→Admission→AdmissionAck handshake (whose task id keys
/// the per-task staging tree) or keep the frozen unstaged path.
///
/// Scheduled admission (wave 21): only when BOTH Hellos carry the bit.
/// Wave 23 (review P1-4) closes the downgrade hole: when this call
/// carries a decider (i.e. it is `receive_on_channel`, which exists
/// precisely for scheduled tasks), a sender without CAP_SCHEDULER is
/// refused with PROTOCOL_UNSUPPORTED and the channel closes — the
/// frozen unstaged body is reserved for the legacy entries, whose
/// `admission` is None.
async fn receive_admission_stage(
    tx: &mut DirectSendStream,
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
    admission: Option<AdmissionDecider<'_>>,
    peer_caps: u16,
) -> Result<Option<[u8; 16]>, EngineError> {
    let capabilities = scheduler_recv_caps(admission.is_some());
    if scheduler_frames_allowed(capabilities, peer_caps) {
        let decider = admission.expect("CAP_SCHEDULER is advertised only with a decider");
        Ok(Some(
            admission_handshake_receiver(tx, rx, limits, idle, decider).await?,
        ))
    } else if admission.is_some() {
        Err(EngineError::PeerError {
            code: ErrorCode::ProtocolUnsupported,
            message: "peer does not advertise CAP_SCHEDULER; \
                      scheduled task admission cannot run"
                .into(),
        })
    } else {
        Ok(None)
    }
}

/// Discard every prepared sink that holds no verified chunk: after a
/// failed receive those part files are pure litter, while parts holding
/// verified chunks survive for resume. Sinks already handed to
/// multistream tasks are no longer in `sinks` and are left alone.
fn discard_empty_sinks(sinks: &mut [Option<ak_storage::LocalFileSink>]) {
    for slot in sinks.iter_mut() {
        let empty = slot
            .as_ref()
            .is_some_and(|sink| sink.verified_prefix_chunks() == 0);
        if empty {
            if let Some(sink) = slot.take() {
                sink.discard();
            }
        }
    }
}

/// The frozen receive body (wave 27 split out of `receive_job` verbatim;
/// wave 28 adds the optional `consent` hook):
/// everything from Offer/Accept through manifest pages, resume answer,
/// chunk phase (single-stream or multistream), verify, commit, FinalAck
/// and the terminal-state staging cleanup. `staged_task` is the per-task
/// staging key under scheduled admission (`None` = legacy `.akpart`
/// beside the final name). `consent` is the optional receive-side
/// user decision at the Offer handling point: `None` accepts every offer
/// (the frozen pre-wave-28 behavior, byte for byte), `Some` asks the
/// callback and — on `false` — answers the DECLINED error frame, closes
/// the connection and fails with [`EngineError::ConsentDeclined`]
/// before any disk write of the payload.
#[allow(clippy::too_many_arguments)]
async fn receive_frozen_body(
    channel: DirectChannel,
    mut tx: DirectSendStream,
    mut rx: DirectRecvStream,
    peer_caps: u16,
    staged_task: Option<[u8; 16]>,
    target: &LocalSaveTarget,
    spec: &JobSpec,
    started: Instant,
    store: &FileCheckpointStore,
    progress: &(dyn Fn(Progress) + Send + Sync),
    consent: Option<&TransferConsent>,
) -> Result<Report, EngineError> {
    let limits = engine_limits();
    let multistream = (peer_caps & CAP_MULTISTREAM) != 0;
    // Every receiver Hello carries the bit (RECV_CAPABILITIES), so the
    // sender's announcement decides.
    let sender_keepalive = sender_keepalive_allowed(RECV_CAPABILITIES, peer_caps);

    // Offer -> consent -> Accept. Channel-level consent already happened via
    // the authorize callback; the offer is accepted per current product copy.
    //
    // A connection LOSS while awaiting the FIRST frame is normalized to
    // [`EngineError::PeerClosed`] (wave 27): the peer hung up before
    // offering a job — the canonical case is a wave-27 sender aborting its
    // scheduler-admission attempt against this legacy endpoint (its Hello
    // carried no `CAP_SCHEDULER`) and retrying on a fresh connection. Such
    // a connection is NOT a transfer, and one-shot receivers
    // (`receive --once`, `invite redeem --once`) use exactly this signal
    // to keep waiting instead of consuming their single accept. Wire
    // rejections (oversized headers, budget violations) still surface
    // verbatim — only `Connection` losses are a hang-up.
    let (job_id, root_name, file_count, total_bytes, offered_chunk_size) =
        match recv_within(&mut rx, &limits, spec.idle_timeout).await {
            Ok(Frame {
                message:
                    Message::Offer {
                        job_id,
                        root_name,
                        file_count,
                        total_bytes,
                        chunk_size,
                        ..
                    },
                ..
            }) => (job_id, root_name, file_count, total_bytes, chunk_size),
            Ok(Frame {
                message: Message::Error { code, message },
                ..
            }) => return Err(EngineError::PeerError { code, message }),
            Ok(other) => return Err(unexpected(other.message)),
            Err(EngineError::Stream(ak_net::StreamError::Connection(_))) => {
                return Err(EngineError::PeerClosed)
            }
            Err(other) => return Err(other),
        };
    #[cfg(test)]
    seams::arm_receive(&root_name, job_id);
    // P1-1 defense in depth (review round 17): validate the offered chunk
    // size against the engine's own limits BEFORE consenting. The wire
    // decoder's budget covers frame bytes (and may itself refuse the
    // field), but a syntactically legal frame carrying e.g.
    // `chunk_size = u32::MAX` must never reach the storage layer, where it
    // would size per-chunk buffers. Zero is equally refused.
    let chunk_size = match NonZeroU32::new(offered_chunk_size)
        .filter(|size| size.get() <= limits.max_chunk_bytes)
    {
        Some(size) => size,
        None => {
            let message = format!(
                "offered chunk_size {offered_chunk_size} is outside the engine limit \
                     1..={} bytes",
                limits.max_chunk_bytes
            );
            // Refuse before Accept: tell the peer, then fail locally.
            let _ = tx
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Error {
                        code: ErrorCode::ProtocolUnsupported,
                        message: message.clone(),
                    },
                })
                .await;
            return Err(EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                message,
            });
        }
    };
    progress(Progress::Offered {
        files: file_count,
        bytes: total_bytes,
    });
    // Receive-side user consent (wave 28; non-blocking shape wave 29): the
    // one decision point between the validated Offer and the Accept. `None`
    // (every pre-wave-28 entry) falls straight through — the frames below
    // are byte-identical to the frozen behavior. A `false` answer tells the
    // peer with the DECLINED error frame (existing Error-frame semantics;
    // see [`DECLINED_WIRE_CODE`]), fails locally with
    // [`EngineError::ConsentDeclined`] and lets the channel close — no
    // Accept, no sink, no checkpoint record, nothing on disk. The callback
    // itself returns immediately ([`ConsentAnswer`]); a pending decision is
    // awaited here, and a decider that vanishes without answering (dropped
    // sender half) fails closed as a declination.
    if let Some(consent) = consent {
        let info = ConsentInfo {
            job_id,
            root_name: root_name.clone(),
            file_count,
            total_bytes,
            sender_fingerprint: *channel.peer_fingerprint(),
        };
        let decided = match consent(&info) {
            ConsentAnswer::Now(decided) => decided,
            // Every decider going away fails closed as a declination.
            ConsentAnswer::Pending(pending) => pending.await.unwrap_or_default(),
        };
        if !decided {
            let _ = tx
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Error {
                        code: DECLINED_WIRE_CODE,
                        message: DECLINED_MESSAGE.into(),
                    },
                })
                .await;
            // Half-close our send side, then hold the connection until the
            // peer reacts (see [`DECLINED_DRAIN_WAIT`]) so the frame is on
            // the wire before the teardown that follows this return.
            let _ = tx.finish();
            let _ = tokio::time::timeout(DECLINED_DRAIN_WAIT, rx.recv_frame(&limits)).await;
            return Err(EngineError::ConsentDeclined { job_id });
        }
    }
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Accept { job_id },
    })
    .await?;
    progress(Progress::Accepted {
        files: file_count,
        bytes: total_bytes,
    });

    // Manifest pages, assembled under full cross-frame validation: page
    // order strictly 0-based consecutive, every page naming this job, the
    // accumulated entry count bounded by the offered file_count, and the
    // totals reconciled against the offer on the last page.
    let mut pages = ManifestAssembler::new(job_id, file_count, total_bytes, &limits);
    loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::ManifestPage {
                        job_id: page_job_id,
                        page_index,
                        last_page,
                        entries: page,
                    },
                ..
            } => {
                if pages.push(page_job_id, page_index, last_page, page)? {
                    break;
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    let entries = pages.entries;
    let manifest = Manifest {
        chunk_size,
        entries: entries.clone(),
    };
    progress(Progress::ManifestReceived);
    let job = ReceiverJob { manifest };

    // The record key is the per-instance record id (content id + peer +
    // direction + root), not the wire's content id: retries of this same
    // instance find the previous record, while a different peer or the
    // opposite direction through the same checkpoint directory keeps a
    // separate file (see `record_id_for`).
    let record_id = record_id_for(
        &job_id,
        channel.peer_fingerprint(),
        Direction::Receive,
        target.root(),
    );
    // Previous run of this exact transfer instance, if any: the committed-
    // file skip below reads its per-file verified prefixes.
    let previous = store.load(&record_id).map_err(EngineError::Checkpoint)?;
    let mut record = TaskRecord {
        job_id: record_id,
        direction: Direction::Receive,
        root: spec.out_dir.clone(),
        peer_fingerprint: Some(*channel.peer_fingerprint()),
        chunk_size: chunk_size.get(),
        total_files: job.manifest.file_count(),
        total_bytes: job.manifest.total_bytes(),
        state: "transferring".into(),
        created_unix_ms: now_ms(),
        updated_unix_ms: now_ms(),
        files: job
            .manifest
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| FileCheckpoint {
                file_index: index as u32,
                size: e.size,
                verified_prefix_chunks: 0,
            })
            .collect(),
    };

    // CR-P3-02 (manifest leg): on Windows, refuse the whole job when any
    // manifest entry carries a path component Windows cannot save (the
    // `< > " | ? *` chars or a reserved stem — the Offer's root name was
    // already checked at the consent gate; this is the full per-entry
    // sweep). Still BEFORE any sink/part file is created: the peer is told
    // ProtocolUnsupported with the offending path, the checkpoint records
    // "failed_name_rejected", and the local verdict carries the readable
    // reason. Non-Windows receivers never take this branch (Linux->Linux
    // transfers with these characters stay legal).
    #[cfg(windows)]
    if let Some(problem) = windows_unsavable_entry(&job.manifest.entries) {
        let _ = tx
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Error {
                    code: ErrorCode::ProtocolUnsupported,
                    message: format!(
                        "receiver cannot save a manifest entry on this platform: {problem}"
                    ),
                },
            })
            .await;
        record.state = "failed_name_rejected".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        return Err(EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("manifest entry unsavable on Windows: {problem}"),
        )));
    }

    // Storage-full precheck: refuse the job before any part file is created.
    // Unknown capacity passes (see [`check_headroom`]); a shortage is told to
    // the peer over the wire, checkpointed as "failed_storage_full", and
    // returned as the local verdict. The sender already maps a peer Error at
    // its ResumeRequest wait into `EngineError::PeerError` — numbers are not
    // invented on either side.
    if let Err(error) = check_headroom(target.available_bytes(), job.manifest.total_bytes()) {
        // Best effort: the job dies of a local condition regardless of
        // whether the peer still hears us.
        let _ = tx
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Error {
                    code: ErrorCode::StorageFull,
                    message: "insufficient free space at destination".into(),
                },
            })
            .await;
        record.state = "failed_storage_full".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        return Err(error);
    }

    // Prepare sinks (transparent resume from .akpart files) and answer.
    // Committed-file retry idempotency (review round 17): a file the
    // previous run of this same record already received in full AND whose
    // committed bytes still match the manifest is skipped entirely — no
    // part file is prepared, the resume answer carries the full prefix (so
    // the sender skips it too), and the verify phase must not re-finish it
    // (a re-finish would hit the conflict policy against the file the
    // previous run renamed into place). "Received in full" comes from the
    // previous record's verified prefix; "still matches" is proven by
    // re-hashing the committed file, so a deleted or modified target falls
    // back to a normal retransfer.
    //
    // The answer is sent while the sinks are prepared (review 2026-11-18):
    // a `more = true` shard leaves whenever it fills a frame or the
    // liveness interval has passed, so the sender's per-frame idle budget
    // never has to cover the whole prepare pass (see LIVENESS_MAX_INTERVAL).
    let mut sinks: Vec<Option<ak_storage::LocalFileSink>> =
        Vec::with_capacity(job.manifest.entries.len());
    let liveness = liveness_interval(spec.idle_timeout);
    let mut shards = ResumeShards::new(
        usize::from(limits.max_resume_entries),
        liveness,
        Instant::now(),
    );
    // While one large file's prepare pass runs on the blocking pool (see
    // OFFLOAD_WHOLE_FILE_BYTES), the liveness signal is an empty shard.
    let prepare_keepalive = Frame {
        version: ak_wire::VERSION,
        message: Message::ResumeRequest {
            job_id,
            files: Vec::new(),
            more: true,
        },
    };
    let mut resumed_chunks: u64 = 0;
    let mut committed_skip = vec![false; job.manifest.entries.len()];
    for (index, entry) in job.manifest.entries.iter().enumerate() {
        // A large file's pass here can re-hash gigabytes (a committed
        // final, or a resumed part's verified prefix): off this task.
        let offload = !entry.is_dir && entry.size >= OFFLOAD_WHOLE_FILE_BYTES;
        let was_fully_verified = previous
            .as_ref()
            .and_then(|record| record.files.get(index))
            .is_some_and(|file| {
                !entry.is_dir && file.verified_prefix_chunks >= entry.chunk_digests.len() as u64
            });
        let committed = if !was_fully_verified {
            false
        } else if offload {
            let root = target.root().to_path_buf();
            let owned = entry.clone();
            let checked = offload_with_liveness(
                &mut tx,
                Some(&prepare_keepalive),
                liveness,
                &mut shards.last_frame,
                move || committed_file_matches(&root, &owned),
            )
            .await;
            match checked {
                Ok(matches) => matches,
                Err(error) => {
                    // The pass itself failed (panicked); same litter rule
                    // as the outcome wrapper below.
                    discard_empty_sinks(&mut sinks);
                    return Err(error);
                }
            }
        } else {
            committed_file_matches(target.root(), entry)
        };
        if committed {
            committed_skip[index] = true;
            let chunks = entry.chunk_digests.len() as u64;
            resumed_chunks += chunks;
            shards.push(ResumeEntry {
                file_index: index as u32,
                verified_prefix_chunks: chunks,
            });
            if let Some(slot) = record.files.get_mut(index) {
                slot.verified_prefix_chunks = chunks;
            }
            // No sink: no part file, no chunk phase, no finish for this file.
            sinks.push(None);
        } else {
            // Scheduled task: the part file lives in this task's own
            // staging tree (`<root>/.ak-staging/<task_hex>/...`), so a
            // second task aiming at the same destination can never
            // compete for one `.akpart` (wave 20 staging contract).
            let prepared = if offload {
                let owned_target = target.clone();
                let owned = entry.clone();
                let conflict = spec.conflict;
                let done = offload_with_liveness(
                    &mut tx,
                    Some(&prepare_keepalive),
                    liveness,
                    &mut shards.last_frame,
                    move || match staged_task {
                        Some(task) => {
                            owned_target.prepare_file_staged(task, &owned, conflict, chunk_size)
                        }
                        None => {
                            owned_target.prepare_file_with_chunk_size(&owned, conflict, chunk_size)
                        }
                    },
                )
                .await;
                match done {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        discard_empty_sinks(&mut sinks);
                        return Err(error);
                    }
                }
            } else {
                match staged_task {
                    Some(task) => {
                        target.prepare_file_staged(task, entry, spec.conflict, chunk_size)
                    }
                    None => target.prepare_file_with_chunk_size(entry, spec.conflict, chunk_size),
                }
            };
            let sink = prepared?;
            let prefix = sink.verified_prefix_chunks();
            if !entry.is_dir {
                resumed_chunks += prefix;
                shards.push(ResumeEntry {
                    file_index: index as u32,
                    verified_prefix_chunks: prefix,
                });
            }
            sinks.push(Some(sink));
        }
        if let Some(files) = shards.take_due(Instant::now()) {
            let shard = Frame {
                version: ak_wire::VERSION,
                message: Message::ResumeRequest {
                    job_id,
                    files,
                    more: true,
                },
            };
            if let Err(error) = tx.send_frame(&shard).await {
                // Same litter rule as the outcome wrapper below.
                discard_empty_sinks(&mut sinks);
                return Err(EngineError::from(error));
            }
            // Let the transport (and a peer sharing this runtime) run.
            tokio::task::yield_now().await;
        }
    }
    let resume_rest = shards.into_rest();
    if resumed_chunks > 0 {
        progress(Progress::Resuming {
            chunks: resumed_chunks,
        });
    }

    // Everything below can fail mid-transfer; the outcome wrapper then
    // discards empty part files so a failed receive leaves no litter.
    // Parts holding verified chunks always survive for resume.
    let outcome = async {
        // The rest of the resume answer: at most `max_resume_entries`
        // entries per frame, the last one (the only one when the prepare
        // pass was short) with more = false.
        for message in
            resume_request_frames(job_id, &resume_rest, usize::from(limits.max_resume_entries))
        {
            tx.send_frame(&Frame {
                version: ak_wire::VERSION,
                message,
            })
            .await?;
        }
        store.save(&record).map_err(EngineError::Checkpoint)?;

        // Chunk phase: the frozen single-stream loop, or per-file streams
        // when both sides announced CAP_MULTISTREAM.
        let tail = if multistream {
            match multistream_receive(
                &channel,
                rx,
                job_id,
                chunk_size,
                &job.manifest,
                &mut sinks,
                &mut record,
                store,
                spec.idle_timeout,
                sender_keepalive,
                progress,
            )
            .await
            {
                Ok(events) => TailHold::Events(events),
                Err(error) => {
                    // Close explicitly: the receiver's lingering stream
                    // tasks would otherwise keep the connection open and
                    // the sender waiting (see MULTISTREAM_ABORT_CLOSE_CODE).
                    channel.close(MULTISTREAM_ABORT_CLOSE_CODE, b"multistream receive failed");
                    return Err(error);
                }
            }
        } else {
            TailHold::Stream(
                receive_single_stream(
                    rx,
                    &mut tx,
                    job_id,
                    chunk_size,
                    &job.manifest,
                    &mut sinks,
                    &mut record,
                    store,
                    spec.idle_timeout,
                    sender_keepalive,
                    progress,
                )
                .await?,
            )
        };

        // Verify and commit every file; only then report completion. A
        // retry of the same record skips a file this pass committed (the
        // prepare loop checks the final name's whole digest), so a crash
        // after any commit does not collide with its own output.
        progress(Progress::Verifying);
        // One save up front captures the whole chunk phase (the chunk-phase
        // cadence may trail the last acks); during the pass the record is
        // saved on a timer, not per file (VERIFY_CHECKPOINT_INTERVAL).
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        let mut last_save = Instant::now();
        // Liveness while verifying (review 2026-11-18): the sender waits
        // for the FinalAck under its idle budget and tolerates Control
        // frames at that gate, so a peer that negotiated Pause/Resume gets
        // a no-op Control::Resume whenever the liveness interval passes.
        let verify_keepalive = ((peer_caps & CAP_PAUSE_RESUME) != 0).then_some(Frame {
            version: ak_wire::VERSION,
            message: Message::Control {
                job_id,
                control: Control::Resume,
            },
        });
        let report_committed = |index: usize| {
            progress(Progress::FileVerified {
                file_index: index as u32,
            });
            progress(Progress::FileCommitted {
                file_index: index as u32,
                path: job.manifest.entries[index]
                    .rel_path
                    .to_string_lossy()
                    .replace('\\', "/"),
            });
        };
        // Up to VERIFY_PARALLEL_FILES files' finish() passes run at a time
        // on the blocking pool (batch Q review); this task keeps the
        // liveness signal going (a large file's pass included, see
        // OFFLOAD_WHOLE_FILE_BYTES) and records each file as its pass
        // returns, so progress events come in completion order. After a
        // failure no further pass starts and the running ones are waited
        // for: none is still writing in the destination once this returns.
        let mut passes: tokio::task::JoinSet<(usize, std::io::Result<()>)> =
            tokio::task::JoinSet::new();
        let mut large_in_flight = false;
        let mut next = 0;
        let mut failure: Option<EngineError> = None;
        let mut last_signal = Instant::now();
        loop {
            // Due between any two steps, a slow progress callback included.
            if failure.is_none() && last_signal.elapsed() >= liveness {
                if let Some(keepalive) = &verify_keepalive {
                    match tx.send_frame(keepalive).await {
                        Ok(()) => {
                            // Let the transport (and a peer sharing this
                            // runtime) run.
                            tokio::task::yield_now().await;
                            last_signal = Instant::now();
                        }
                        Err(error) => failure = Some(EngineError::from(error)),
                    }
                }
            }
            while failure.is_none() && next < sinks.len() && passes.len() < VERIFY_PARALLEL_FILES {
                let entry = &job.manifest.entries[next];
                let large = !entry.is_dir && entry.size >= OFFLOAD_WHOLE_FILE_BYTES;
                if large && large_in_flight {
                    break;
                }
                let index = next;
                next += 1;
                if let Some(mut sink) = sinks[index].take() {
                    if entry.is_dir {
                        if let Err(error) = sink.finish() {
                            failure = Some(EngineError::Io(error));
                        }
                    } else {
                        large_in_flight |= large;
                        passes.spawn_blocking(move || (index, sink.finish()));
                    }
                } else if committed_skip[index] {
                    // Committed by the previous run of this record and
                    // skipped; still report it as verified/committed for
                    // honest progress.
                    report_committed(index);
                }
            }
            if passes.is_empty() {
                // Every file was started and has returned, or a failure
                // stopped the loop and the running passes have returned.
                break;
            }
            let due = tokio::time::Instant::from_std(last_signal + liveness);
            let signal = failure.is_none() && verify_keepalive.is_some();
            let joined = tokio::select! {
                biased;
                joined = passes.join_next() => joined,
                () = tokio::time::sleep_until(due), if signal => continue,
            };
            match joined {
                Some(Ok((index, Ok(())))) => {
                    let entry = &job.manifest.entries[index];
                    if entry.size >= OFFLOAD_WHOLE_FILE_BYTES {
                        large_in_flight = false;
                    }
                    record.files[index].verified_prefix_chunks = entry.chunk_digests.len() as u64;
                    if failure.is_none() && last_save.elapsed() >= VERIFY_CHECKPOINT_INTERVAL {
                        record.updated_unix_ms = now_ms();
                        match store.save(&record) {
                            Ok(()) => last_save = Instant::now(),
                            Err(error) => failure = Some(EngineError::Checkpoint(error)),
                        }
                    }
                    report_committed(index);
                }
                Some(Ok((index, Err(error)))) => {
                    if job.manifest.entries[index].size >= OFFLOAD_WHOLE_FILE_BYTES {
                        large_in_flight = false;
                    }
                    failure.get_or_insert(EngineError::Io(error));
                }
                Some(Err(join)) => {
                    failure.get_or_insert(EngineError::Io(std::io::Error::other(format!(
                        "verify pass did not complete: {join}"
                    ))));
                }
                None => {}
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        // Completion marker strictly after verification and commit (PROTOCOL).
        record.state = "completed".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::FinalAck {
                job_id,
                verified: true,
            },
        })
        .await?;
        tx.finish()?;
        progress(Progress::FinalAck { verified: true });
        // Hold the connection until the sender observes the FinalAck:
        // dropping this endpoint on return would close the QUIC connection
        // immediately and could discard the FinalAck before the peer reads
        // it.
        match tail {
            TailHold::Events(mut events) => {
                // The control-reader task drains until the sender
                // half-closes its side after reading the FinalAck; that
                // closes this channel.
                let _ = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
            }
            TailHold::Stream(mut rx_tail) => {
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if rx_tail.recv_frame(&limits).await.is_err() {
                            break;
                        }
                    }
                })
                .await;
            }
        }
        progress(Progress::Done);

        // End-of-transfer transport-stats snapshot (see `ChannelStatsSink`),
        // receiver side: after the tail hold, so the counters are final
        // (the sender has observed the FinalAck and closed by now in the
        // healthy case; quinn keeps the stats queryable after close).
        if let Some(sink) = &spec.stats_sink {
            sink.notify(&channel.stats());
        }

        Ok(Report {
            job_id,
            files: job.manifest.file_count(),
            bytes: job.manifest.total_bytes(),
            duration: started.elapsed(),
            resumed_chunks,
            peer_fingerprint: Some(channel.peer_fingerprint().to_hex()),
            roots: Vec::new(),
        })
    }
    .await;

    match outcome {
        Ok(report) => {
            // Scheduled task terminal state (success): every file was
            // committed out of the staging tree, so the tree itself is
            // pure litter — best-effort removal (the transfer already
            // succeeded; a cleanup failure must not turn it into an
            // error).
            if let Some(task) = staged_task {
                let _ = ak_storage::discard_staging(target.root(), task);
            }
            Ok(report)
        }
        Err(error) => {
            // Sinks consumed by multistream tasks are already gone; every
            // sink still held here with no verified chunk is pure litter.
            discard_empty_sinks(&mut sinks);
            // Cancellation is the other terminal state whose whole
            // staging tree is discarded (wave 20 contract: parts holding
            // verified chunks otherwise survive for resume). Any other
            // failure keeps the staged parts exactly like the legacy
            // `.akpart` rule.
            if let (EngineError::Cancelled, Some(task)) = (&error, staged_task) {
                let _ = ak_storage::discard_staging(target.root(), task);
            }
            Err(error)
        }
    }
}

/// Terminal state of the accept loop before every expected connection
/// arrived. Produces one error per unfilled result slot: `io::Error` and
/// [`ak_net::ConnectError`] are not `Clone`, so [`AcceptStop::Failed`]
/// hands the original error to the first unfilled slot and descriptive io
/// errors to the rest.
enum AcceptStop {
    /// No authorized peer within the idle budget — the expected way a
    /// concurrent receive ends early (a sender that never came).
    TimedOut(Duration),
    /// The listener itself failed mid-loop.
    Failed(Option<ak_net::ConnectError>),
}

impl AcceptStop {
    fn error(&mut self) -> EngineError {
        match self {
            AcceptStop::TimedOut(idle) => EngineError::IdleTimeout(*idle),
            AcceptStop::Failed(first) => {
                first.take().map(EngineError::Connect).unwrap_or_else(|| {
                    EngineError::Io(std::io::Error::other(
                        "listener failed before every expected connection arrived",
                    ))
                })
            }
        }
    }
}

/// Serve several senders concurrently on one bound endpoint: one
/// independent task per accepted connection, each running the exact
/// single-job receive protocol of [`receive_with_addr`] (the authorize
/// callback and the policy gate execute per connection).
///
/// Completion condition: `expected_jobs` connections accepted and every
/// task finished. There is deliberately no stop parameter — callers that
/// expect slow senders should raise [`ReceiveSpec::idle_timeout`], which
/// bounds each accept wait (an unfilled slot reports
/// [`EngineError::IdleTimeout`] instead of hanging forever).
///
/// Ordering: result slot `i` — and the first `progress` argument `i` — is
/// the `i`-th accepted connection, 0-based. The returned Vec always holds
/// exactly `expected_jobs` entries (`expected_jobs = 0` yields an empty
/// Vec): a task that fails carries its [`EngineError`] in its own slot
/// without affecting the others, and if the listener stops before every
/// expected connection arrived, the unfilled slots carry that accept
/// failure. A pre-listen failure (opening the destination or binding)
/// yields a one-element Vec with that error. Per-task progress events keep
/// their order; events from different tasks interleave arbitrarily. The
/// callback itself runs on this driver task and must be cheap.
///
/// Concurrency safety (see also the module docs and `receive_job`): the
/// receive directory is shared only as `&LocalSaveTarget`
/// (`prepare_file*` has no interior mutable state; different entries map
/// to different `.akpart` files), and every task opens its own checkpoint
/// store with its own `<job>.akjob` file (atomic writes) — so under
/// `ConflictPolicy::Fail` two jobs racing for the same target path fail
/// per file, exactly as two sequential receives would.
pub async fn serve_concurrently(
    spec: ReceiveSpec,
    expected_jobs: usize,
    progress: &(dyn Fn(u64, Progress) + Send + Sync),
) -> Vec<Result<Report, EngineError>> {
    let target = match LocalSaveTarget::open(&spec.out_dir) {
        Ok(target) => Arc::new(target),
        Err(error) => return vec![Err(error.into())],
    };
    let endpoint = match DirectEndpoint::bind(spec.bind, &spec.identity).await {
        Ok(endpoint) => endpoint,
        Err(error) => return vec![Err(error.into())],
    };
    let job_spec = JobSpec::of(&spec);
    let idle = spec.idle_timeout;
    // `Box<dyn Fn + Send + Sync>` is 'static: the authorize callback moves
    // into the accept task and still runs once per connection.
    let authorize = spec.authorize;

    // The accept loop runs in its own task because `DirectEndpoint::accept`
    // drives a QUIC handshake to completion, which is not cancel-safe:
    // polling it inside the driver's select! below would drop a
    // mid-handshake connection every time a progress event arrived first.
    // Here its future is never cancelled.
    let (channel_tx, mut channel_rx) =
        tokio::sync::mpsc::channel::<Result<DirectChannel, ak_net::ConnectError>>(1);
    let expected = expected_jobs;
    tokio::spawn(async move {
        let mut accepted = 0usize;
        while accepted < expected {
            match endpoint.accept_within(&authorize, idle).await {
                Ok(channel) => {
                    accepted += 1;
                    if channel_tx.send(Ok(channel)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = channel_tx.send(Err(error)).await;
                    break;
                }
            }
        }
        // Dropping the endpoint closes the listener for NEW connections
        // only; already-accepted channels keep working (the transport keeps
        // its driver alive while connections exist).
    });

    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, Progress)>();
    let mut results: Vec<Option<Result<Report, EngineError>>> =
        (0..expected_jobs).map(|_| None).collect();
    let mut set: tokio::task::JoinSet<(u64, Result<Report, EngineError>)> =
        tokio::task::JoinSet::new();
    let mut accepted = 0usize;
    let mut accept_stop: Option<AcceptStop> = None;
    let mut lost_task: Option<EngineError> = None;

    // Phase 1: fill every slot with an accepted connection (or a terminal
    // accept failure) while forwarding progress and recording finishes.
    while accepted < expected_jobs && accept_stop.is_none() {
        tokio::select! {
            channel = channel_rx.recv() => {
                match channel {
                    Some(Ok(channel)) => {
                        let job = job_spec.clone();
                        let events = events_tx.clone();
                        let shared = Arc::clone(&target);
                        let index = accepted as u64;
                        accepted += 1;
                        set.spawn(async move {
                            // Unbounded on purpose: the sync progress
                            // callback must never block or drop a transfer
                            // event; the driver loop below drains eagerly.
                            let forward = move |event: Progress| {
                                let _ = events.send((index, event));
                            };
                            (index, receive_job(channel, &shared, &job, Instant::now(), None, &forward, None).await)
                        });
                    }
                    Some(Err(ak_net::ConnectError::Timeout(idle))) => {
                        accept_stop = Some(AcceptStop::TimedOut(idle));
                    }
                    Some(Err(error)) => {
                        accept_stop = Some(AcceptStop::Failed(Some(error)));
                    }
                    None => {
                        // The accept task ended without delivering a
                        // terminal error; nothing more can ever arrive.
                        accept_stop = Some(AcceptStop::Failed(None));
                    }
                }
            }
            event = events_rx.recv() => {
                if let Some((index, event)) = event {
                    progress(index, event);
                }
            }
            joined = set.join_next(), if !set.is_empty() => {
                match joined {
                    Some(Ok((index, outcome))) => results[index as usize] = Some(outcome),
                    Some(Err(join)) => {
                        lost_task = Some(EngineError::Io(std::io::Error::other(format!(
                            "receive task failed: {join}"
                        ))));
                    }
                    None => {}
                }
            }
        }
    }

    // Phase 2: every slot is filled; drain the still-running tasks. The
    // driver keeps its own events_tx alive so events_rx.recv() below can
    // never observe a closed channel (and busy-loop on it).
    while !set.is_empty() {
        tokio::select! {
            joined = set.join_next() => {
                match joined {
                    Some(Ok((index, outcome))) => results[index as usize] = Some(outcome),
                    Some(Err(join)) => {
                        lost_task = Some(EngineError::Io(std::io::Error::other(format!(
                            "receive task failed: {join}"
                        ))));
                    }
                    None => break,
                }
            }
            event = events_rx.recv() => {
                if let Some((index, event)) = event {
                    progress(index, event);
                }
            }
        }
    }
    // All tasks joined, so every sender clone is dropped; release ours and
    // flush whatever was still buffered when the last join completed.
    drop(events_tx);
    while let Ok((index, event)) = events_rx.try_recv() {
        progress(index, event);
    }

    results
        .into_iter()
        .enumerate()
        .map(|(index, slot)| match slot {
            Some(outcome) => outcome,
            // Slots the listener never filled before stopping.
            None if index >= accepted => Err(accept_stop
                .as_mut()
                .map(|stop| stop.error())
                .unwrap_or_else(|| {
                    EngineError::Io(std::io::Error::other(
                        "listener stopped before this connection arrived",
                    ))
                })),
            // A spawned task that never delivered an outcome (join failure).
            None => Err(lost_task.take().unwrap_or_else(|| {
                EngineError::Io(std::io::Error::other("receive task produced no outcome"))
            })),
        })
        .collect()
}
