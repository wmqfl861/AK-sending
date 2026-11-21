//! Hello exchange, scheduler admission handshakes and the bounded connect.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

pub(super) fn hello_frame(name: &str, identity: &TlsBundle, capabilities: u16) -> Frame {
    let mut public = [0u8; 32];
    public.copy_from_slice(identity.fingerprint.as_bytes());
    // The peer verifies the certificate itself; the key field is a hint for
    // display and is pinned via the TLS handshake, not trusted from here.
    Frame {
        version: ak_wire::VERSION,
        message: Message::Hello {
            proto_version: PROTO_VERSION,
            device_name: name.chars().take(64).collect(),
            public_key: public,
            capabilities,
        },
    }
}

/// Capabilities this receiver advertises: it can always resume, honor
/// Pause/Resume, accept per-file parallel streams, and take the sender's
/// liveness signal before Finalize.
pub(super) const RECV_CAPABILITIES: u16 =
    CAP_RESUME | CAP_PAUSE_RESUME | CAP_MULTISTREAM | CAP_SENDER_KEEPALIVE;

/// Capabilities every engine sender advertises; multistream and scheduled
/// admission are added per job.
pub(super) const SEND_CAPABILITIES: u16 = CAP_RESUME | CAP_PAUSE_RESUME | CAP_SENDER_KEEPALIVE;

pub(super) async fn expect_hello(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<u16, EngineError> {
    match recv_within(rx, limits, idle).await? {
        Frame {
            message:
                Message::Hello {
                    proto_version,
                    capabilities,
                    ..
                },
            ..
        } => {
            if proto_version != PROTO_VERSION {
                return Err(EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    message: format!("peer speaks protocol version {proto_version}"),
                });
            }
            Ok(capabilities)
        }
        Frame {
            message: Message::Error { code, message },
            ..
        } => Err(EngineError::PeerError { code, message }),
        other => Err(unexpected(other.message)),
    }
}

/// One frame receive under an idle deadline; silence past `idle` is fatal
/// with [`EngineError::IdleTimeout`].
pub(super) async fn recv_within(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<Frame, EngineError> {
    match tokio::time::timeout(idle, rx.recv_frame(limits)).await {
        Ok(result) => result.map_err(EngineError::Stream),
        Err(_) => Err(EngineError::IdleTimeout(idle)),
    }
}

/// Wrap a wire decode failure the way ak-net's stream layer does.
fn decode_error(err: ak_wire::DecodeError) -> EngineError {
    EngineError::Stream(ak_net::StreamError::Wire(err))
}

/// One admission-exchange frame receive (TaskOpen / Admission /
/// AdmissionAck / Error) under an idle deadline (wave 23). Same two-gate
/// header discipline as ak-net's `recv_frame` — the all-frame hard cap and
/// the per-type payload budget both fire on the header alone, before any
/// payload buffer is allocated — but sourced from
/// [`ak_wire::payload_budget`] directly: the wave-23 admission payloads
/// (113/38/32 bytes) grew past the wave-20 mirror table that still governs
/// ak-net's pre-filter (96/34/32, unchanged for every file-protocol
/// frame), so admission frames must not pass through it. [`Frame::decode`]
/// re-validates everything on the assembled bytes, as always.
pub(super) async fn recv_admission_frame(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<Frame, EngineError> {
    let mut header = [0u8; ak_wire::HEADER_LEN];
    tokio::time::timeout(idle, rx.recv_exact(&mut header))
        .await
        .map_err(|_| EngineError::IdleTimeout(idle))?
        .map_err(EngineError::Stream)?;
    let declared = u32::from_be_bytes([header[3], header[4], header[5], header[6]]);
    if declared as usize > ak_wire::MAX_FRAME_BYTES {
        return Err(decode_error(ak_wire::DecodeError::Oversized {
            declared,
            budget: ak_wire::MAX_FRAME_BYTES as u32,
        }));
    }
    if header[0] != ak_wire::VERSION {
        return Err(decode_error(ak_wire::DecodeError::BadVersion(header[0])));
    }
    let message_type = u16::from_be_bytes([header[1], header[2]]);
    let Some(budget) = ak_wire::payload_budget(message_type, limits) else {
        return Err(decode_error(ak_wire::DecodeError::UnknownType(
            message_type,
        )));
    };
    if u64::from(declared) > budget {
        return Err(decode_error(ak_wire::DecodeError::Oversized {
            declared,
            budget: budget as u32,
        }));
    }
    let mut frame_bytes = header.to_vec();
    frame_bytes.resize(ak_wire::HEADER_LEN + declared as usize, 0);
    tokio::time::timeout(idle, rx.recv_exact(&mut frame_bytes[ak_wire::HEADER_LEN..]))
        .await
        .map_err(|_| EngineError::IdleTimeout(idle))?
        .map_err(EngineError::Stream)?;
    Frame::decode(&frame_bytes, limits).map_err(decode_error)
}

pub(super) fn unexpected(message: Message) -> EngineError {
    EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: format!("unexpected frame: {message:?}"),
    }
}

/// A peer-caused protocol violation with a stable wire code (review round
/// 17): used for cross-frame validation failures — wrong job id, manifest
/// page order violations, resume entry budgets, and the like.
pub(super) fn protocol_error(message: impl Into<String>) -> EngineError {
    EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: message.into(),
    }
}

/// Bounded TaskOpen resubmit budget when the peer answers a permit with
/// `StalePermit` (wave 21): the wire contract says "resubmit a fresh
/// TaskOpen", but an endless stale loop against a broken peer would hang
/// the task, so the budget caps it and surfaces the denial.
const MAX_STALE_PERMIT_RESUBMITS: usize = 3;

/// How long the receiver holds the channel open after answering a denied
/// admission, so the sender observes the decision frame instead of a
/// connection loss (bounded; see `admission_handshake_receiver`).
const ADMISSION_DENY_HOLD: Duration = Duration::from_millis(300);

/// Lease the receiver answers a granted [`Admission`] with, in milliseconds
/// (wave 23, scheduler.md §6: tentative permits carry a bounded lease and
/// expire on their own). The receiver holds the tentative permit only until
/// the sender's `AdmissionAck` or the idle deadline, so the lease mirrors
/// the default idle budget. Denials carry `0`.
pub(super) const ADMISSION_LEASE_MS: u32 = 30_000;

/// Fresh permit id for one TaskOpen attempt: domain-separated blake3 over
/// the task id, the attempt ordinal and the wall clock. Uniqueness per
/// attempt is what matters (it is an id, not a secret — the permit's
/// authenticity comes from the TLS-pinned channel it travels on).
fn fresh_permit_id(task_id: &[u8; 16], attempt: usize) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"AKPERMIT1");
    hasher.update(task_id);
    hasher.update(&(attempt as u64).to_be_bytes());
    hasher.update(&now_ms().to_be_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Sender side of the scheduler admission handshake (wave 21,
/// scheduler.md §6): TaskOpen → Admission → AdmissionAck. Only runs after
/// [`ak_wire::scheduler_frames_allowed`] said both Hellos carry
/// `CAP_SCHEDULER`. `StalePermit` denials resubmit a fresh TaskOpen under
/// [`MAX_STALE_PERMIT_RESUBMITS`]; every other denial ends the task with
/// [`EngineError::AdmissionDenied`] before a single Offer byte moves.
pub(super) async fn admission_handshake_sender(
    tx: &mut DirectSendStream,
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
    ctx: &AdmissionCtx,
) -> Result<(), EngineError> {
    let mut attempt = 0usize;
    loop {
        // Attempt 0 runs on the runtime-minted permit id (AdmissionCtx::new
        // guaranteed it non-zero); every StalePermit resubmit derives a
        // fresh one under the bounded budget.
        let permit_id = if attempt == 0 {
            ctx.permit_id
        } else {
            fresh_permit_id(&ctx.task_id, attempt)
        };
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::TaskOpen {
                job_id: ctx.task_id,
                permit_id,
                group: ctx.group,
                group_registry: ctx.group_registry,
                pairing_id: ctx.pairing_id,
                // The opener of a TaskOpen is by definition the sending
                // side (the receiver answers with Admission).
                direction: DIRECTION_SEND,
            },
        })
        .await?;
        attempt += 1;
        match recv_admission_frame(rx, limits, idle).await? {
            Frame {
                message:
                    Message::Admission {
                        job_id,
                        permit_id: answer_permit,
                        granted,
                        reason,
                        lease_ms,
                    },
                ..
            } => {
                if job_id != ctx.task_id || answer_permit != permit_id {
                    return Err(protocol_error(
                        "admission answer names a different task or permit",
                    ));
                }
                if granted {
                    if reason != AdmissionReason::Ok {
                        return Err(protocol_error("granted admission must carry reason ok"));
                    }
                    if lease_ms == 0 {
                        return Err(protocol_error(
                            "granted admission must carry a positive lease",
                        ));
                    }
                    // Final handshake step: the sender confirms it acts on
                    // the granted permit, releasing the tentative state.
                    tx.send_frame(&Frame {
                        version: ak_wire::VERSION,
                        message: Message::AdmissionAck {
                            job_id: ctx.task_id,
                            permit_id,
                        },
                    })
                    .await?;
                    return Ok(());
                }
                match reason {
                    AdmissionReason::StalePermit if attempt <= MAX_STALE_PERMIT_RESUBMITS => {
                        // The permit the peer held is gone (expired or
                        // unknown): resubmit a fresh TaskOpen.
                        continue;
                    }
                    reason => return Err(EngineError::AdmissionDenied { reason }),
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
}

/// One wire-read `TaskOpen`, destructured into the fields the receive-side
/// admission paths consume (wave 27 split of `admission_handshake_receiver`
/// into "read + validate" and "answer"; see [`recv_task_open`]).
pub(super) struct TaskOpenWire {
    pub(super) task_id: [u8; 16],
    pub(super) permit_id: [u8; 16],
    pub(super) request: AdmissionRequest,
    pub(super) direction: u8,
}

/// Read one `TaskOpen` frame off the wire (header-budgeted through
/// [`recv_admission_frame`]) and destructure its fields. No validation
/// here — callers pick the fail-closed set via [`validate_task_open`]
/// (sentinels only, e.g. the handshake entry retains `direction`
/// verbatim) or the full set including the direction tag.
pub(super) async fn recv_task_open(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
) -> Result<TaskOpenWire, EngineError> {
    match recv_admission_frame(rx, limits, idle).await? {
        Frame {
            message:
                Message::TaskOpen {
                    job_id,
                    permit_id,
                    group,
                    group_registry,
                    pairing_id,
                    direction,
                },
            ..
        } => Ok(TaskOpenWire {
            task_id: job_id,
            permit_id,
            request: AdmissionRequest {
                group,
                group_registry,
                pairing_id,
            },
            direction,
        }),
        Frame {
            message: Message::Error { code, message },
            ..
        } => Err(EngineError::PeerError { code, message }),
        other => Err(unexpected(other.message)),
    }
}

/// The wire-level fail-closed rules in front of every receive-side
/// admission consumer (wave 23, unchanged semantics): the all-zero task
/// id is the reserved staging sentinel (it can never own a staging tree),
/// and neither the pairing nor the registry identity can be the all-zero
/// sentinel of a real Ready relation (mirroring what `AdmissionCtx::new`
/// enforces on the sending side). With `require_send_direction`, a
/// TaskOpen must be opened by the sending side by definition.
pub(super) fn validate_task_open(
    open: &TaskOpenWire,
    require_send_direction: bool,
) -> Result<(), EngineError> {
    if open.task_id == [0u8; 16] {
        return Err(protocol_error("task id must not be the all-zero sentinel"));
    }
    if open.request.pairing_id == [0u8; 16] {
        return Err(protocol_error(
            "pairing id must not be the all-zero sentinel",
        ));
    }
    if open.request.group_registry == [0u8; 32] {
        return Err(protocol_error(
            "group registry must not be the all-zero sentinel",
        ));
    }
    if require_send_direction && open.direction != DIRECTION_SEND {
        return Err(protocol_error(
            "TaskOpen must be opened by the sending side",
        ));
    }
    Ok(())
}

/// Receiver side of the scheduler admission handshake (wave 21, hardened
/// wave 23): read TaskOpen, refuse the wire-level sentinels and direction
/// violations, hand the *complete* admission context ([`AdmissionRequest`]:
/// code group + registry + pairing relation) to the caller's decider,
/// answer `Admission` (a grant carries the bounded ``ADMISSION_LEASE_MS``
/// lease), and — only on grant — wait for AdmissionAck before the frozen
/// receive body runs. Returns the task id that keys the receiver's per-task
/// staging tree. A denial is answered on the wire and fails the call
/// locally with the same reason; no Offer is ever read afterwards.
pub(super) async fn admission_handshake_receiver(
    tx: &mut DirectSendStream,
    rx: &mut DirectRecvStream,
    limits: &Limits,
    idle: Duration,
    decider: AdmissionDecider<'_>,
) -> Result<[u8; 16], EngineError> {
    let opened = recv_task_open(rx, limits, idle).await?;
    validate_task_open(&opened, true)?;
    let TaskOpenWire {
        task_id,
        permit_id,
        request,
        ..
    } = opened;
    let (granted, reason) = match decider(&request) {
        AdmissionDecision::Admit => (true, AdmissionReason::Ok),
        AdmissionDecision::Deny(reason) => (false, reason),
    };
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Admission {
            job_id: task_id,
            permit_id,
            granted,
            reason,
            lease_ms: if granted { ADMISSION_LEASE_MS } else { 0 },
        },
    })
    .await?;
    if !granted {
        // The wire answer IS the denial; the local call fails with the
        // same closed reason and no file protocol runs. Hold the
        // connection briefly first so the denial frame is observably
        // delivered before the error return drops the channel (the same
        // discipline as the FinalAck tail hold — an immediate close could
        // race the frame off the wire and surface as a bare "connection
        // lost" on the peer instead of the actual decision).
        tokio::time::sleep(ADMISSION_DENY_HOLD).await;
        return Err(EngineError::AdmissionDenied { reason });
    }
    match recv_admission_frame(rx, limits, idle).await? {
        Frame {
            message:
                Message::AdmissionAck {
                    job_id,
                    permit_id: acked_permit,
                },
            ..
        } => {
            if job_id != task_id || acked_permit != permit_id {
                return Err(protocol_error(
                    "admission ack names a different task or permit",
                ));
            }
            Ok(task_id)
        }
        Frame {
            message: Message::Error { code, message },
            ..
        } => Err(EngineError::PeerError { code, message }),
        other => Err(unexpected(other.message)),
    }
}

/// Connect with a fast family check and a hard deadline. A v4 socket dialing
/// a v6 peer would otherwise hang in QUIC's retry loop with no error (found
/// by the IPv6 scenario tests); the idle budget bounds every other
/// unreachable-peer case too.
///
/// CR-P1-05: the family check bridges BEFORE failing, exactly the way
/// `DirectEndpoint::connect` bridges the dial itself — an IPv6 local socket
/// (the adopted dual-stack sender socket, or a `[::]` bind) dials a plain-v4
/// peer in `::ffff:` mapped form, and an IPv4 local socket expands a mapped
/// peer back to plain v4 — so only a genuinely undialable combination
/// (v4 local, v6 peer) fast-fails. Callers that always matched families
/// before (the historical task-plane per-candidate binds) observe identical
/// outcomes.
pub(super) async fn connect_bounded(
    endpoint: &DirectEndpoint,
    peer: SocketAddr,
    pinned: &Fingerprint,
    idle: Duration,
) -> Result<DirectChannel, EngineError> {
    let local = endpoint.local_addr()?;
    let peer = match local {
        local if local.is_ipv6() && peer.is_ipv4() => ak_net::classify::to_mapped(peer),
        local if local.is_ipv4() => ak_net::classify::expand_mapped(peer),
        _ => peer,
    };
    if local.is_ipv4() != peer.is_ipv4() {
        return Err(EngineError::Connect(ak_net::ConnectError::Io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "address family mismatch: endpoint bound on {local} cannot reach {peer}; \
                     use a matching-family --to/--port or an [::] bind"
                ),
            ),
        )));
    }
    match tokio::time::timeout(idle, endpoint.connect(peer, pinned)).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(EngineError::IdleTimeout(idle)),
    }
}
