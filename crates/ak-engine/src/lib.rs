//! Transfer engine: one sender, one receiver, one authenticated direct
//! channel, zero servers in the data path.
//!
//! Frozen protocol flow (see docs/PROTOCOL.zh-CN.md):
//! 0. Policy gate (`authorize_path`): the freshly connected channel's path
//!    is checked against the configured `DirectPolicy`
//!    (`SendSpec::policy` / `ReceiveSpec::policy`). A path the policy does
//!    not allow closes the connection (code `POLICY_GATE_CLOSE_CODE`)
//!    before any protocol frame is sent or accepted — zero bytes of this
//!    protocol enter a refused route.
//! 1. Hello exchange (versions, capabilities, identities).
//! 2. Offer with a deterministic job id: `blake3(root_name || per-file
//!    rel_path/size/mtime/digests || chunk_size)[..16]`. Same source, same
//!    id, so a resumed run continues the same job (no new content identity).
//! 3. Accept (receiver consent; consent is required, never assumed).
//! 4. Manifest pages (byte-budgeted, bounded), then a sharded ResumeRequest
//!    exchange carrying per-file verified prefixes taken from the receiver's
//!    part files: consecutive frames of at most `max_resume_entries` entries
//!    each, `more = true` on every frame but the last (an empty job still
//!    sends one empty frame with `more = false`). This removes the old
//!    single-frame 65,535-entry cap on one job.
//! 5. Per file, chunked send with windowed acks; the receiver verifies every
//!    chunk digest on arrival and checkpoints the verified prefix.
//! 6. Per file: receiver finish (whole digest, fsync, atomic rename).
//! 7. Finalize -> FinalAck only after every file verified and committed; the
//!    checkpoint is marked completed strictly before FinalAck is sent.
//! 8. Cancel/Error at any point aborts; part files and checkpoints survive
//!    for resume. No relay fallback exists anywhere by construction.
//!
//! The engine consumes `DirectChannel` values only — there is no API that
//! accepts an unverified connection or a relay-ish transport.
//!
//! v2 additions over the frozen v1 flow (all capability-negotiated):
//! - idle timeouts: any receive wait longer than `idle_timeout` aborts with
//!   [`EngineError::IdleTimeout`]; checkpoints and part files survive;
//! - in-band Pause/Resume: the receiver may send `Control::Pause` while
//!   chunks flow; the sender stops issuing new chunks until `Control::Resume`
//!   (or the idle timeout fires);
//! - optional multistream: when both Hellos carry `CAP_MULTISTREAM` and the
//!   sender opts in, every file rides its own QUIC bidirectional stream (at
//!   most `min(in_flight_window, 4)` concurrent), with per-file windowed
//!   acks. Otherwise the single-stream v1 path runs unchanged.
//!
//! v3 additions (append-only, no wire-format change):
//! - storage-full precheck: before the receiver answers with a ResumeRequest
//!   it compares the destination's reported free space against the job's
//!   total bytes ([`check_headroom`]); a shortage aborts with
//!   [`EngineError::StorageFull`] after telling the peer over the wire;
//! - multi-root send jobs: [`SendSpec::extra_roots`] merges several
//!   files/directories into one manifest, each entry prefixed with its
//!   root's name; the receiver side is unchanged (the tree simply grows a
//!   top-level directory per root).
//!
//! Concurrent receive (round 6, no wire-format change):
//! [`serve_concurrently`] binds once and serves several senders — one
//! independent task per accepted connection, each running the exact
//! single-job body of [`receive_with_addr`] (authorize callback and policy
//! gate execute per connection). Checkpoints never overlap: every task
//! owns its own `<record>.akjob` file (one per record id, atomic writes;
//! see the record-id section below). The receive directory is shared
//! read-only through `LocalSaveTarget`, whose `prepare_file` keeps no
//! interior mutable state, so the conflict policy acts at file granularity
//! — two concurrent jobs racing for the same target path resolve it per
//! file (under `ConflictPolicy::Fail` one of them fails; `Rename`
//! sidesteps). Per-task progress order is preserved; events from different
//! tasks interleave arbitrarily.
//!
//! Durability contract of `ChunkAck` (round 17, documented for PROTOCOL/F7):
//! a ChunkAck means the chunk's digest was verified and its bytes were
//! written to the receiver's part file — into the OS page cache, **not**
//! durably on disk. The engine's durability boundaries are exactly two:
//! (a) checkpoint batches — every `CHECKPOINT_EVERY_ACKS` acknowledged
//! chunks the verified prefix is persisted to the checkpoint store; and
//! (b) file commit — `finish()` re-verifies the whole digest, calls
//! `sync_all` and atomically renames the part file, and only then is the
//! per-file commit marker persisted (strictly before the final
//! `completed` record and FinalAck). A crash between an ack and the next
//! boundary can lose the acked-but-unsynced bytes; resume re-hashes the
//! part file, so nothing already durable is ever trusted from the
//! checkpoint alone.
//!
//! Content identity vs record identity (round 17, wire unchanged): the
//! wire `job_id` (see [`job_id_for`]) stays a pure function of the source
//! content, so a resumed run continues the same job. Checkpoint records
//! are keyed separately by [`record_id_for`] — blake3 of the content id,
//! the peer fingerprint, the direction and the (source|destination) root.
//! The same source sending to two peers, or one machine both sending and
//! receiving through a shared checkpoint directory, therefore keeps
//! distinct `<record>.akjob` files instead of overwriting one another.
//!
//! Multistream idle budget (round 17): the control stream legitimately
//! carries no frame between the ResumeRequest and the Finalize (the sender
//! writes Finalize only after every file stream finished), so the
//! multistream control reader must not own an independent idle timeout.
//! It runs under one task-wide activity budget that data-stream events
//! (chunks acked, streams finished, new streams accepted) refresh; only
//! whole-task silence for `idle_timeout` aborts with
//! [`EngineError::IdleTimeout`]. Per-file data streams still enforce their
//! own per-stream idle budget.
//!
//! Known v1 limitations still true (deliberate, documented): receiver accepts
//! one job per `receive()` call — and one job per connection under
//! [`serve_concurrently`] (one connection = one job).
//!
//! Wave 21 on-channel split (scheduler.md §6 wiring, wire unchanged): the
//! protocol bodies now also run on an already-connected [`DirectChannel`]
//! handed in by a runtime. [`send_on_channel`] drives the scheduler
//! admission handshake (TaskOpen → Admission → AdmissionAck, negotiated
//! through [`ak_wire::CAP_SCHEDULER`]) before the frozen Offer→FinalAck
//! path; [`receive_on_channel`] is the symmetric receive side, deciding
//! admission through a caller-supplied decider and staging part files in a
//! per-task tree (see `ak_storage::prepare_file_staged`). The legacy
//! `send`/`receive_with_addr`/`serve_concurrently` entries keep their exact
//! frozen behavior: they self-connect and run the same bodies with
//! admission absent (no `CAP_SCHEDULER` bit, no TaskOpen, no staging).
//!
//! Wave 23 on-channel hardening (review P1-4): the admission context is
//! only constructible through the validating [`AdmissionCtx::new`] (it now
//! also carries the runtime-minted `permit_id` and the `pairing_id` that
//! travels in `TaskOpen`); the receiver-side decider sees the complete
//! [`AdmissionRequest`] context (code group + registry + pairing relation);
//! [`send_on_channel`] re-checks the injected channel's TLS peer against
//! `spec.pinned` as its very first step and closes on mismatch; and
//! [`receive_on_channel`] no longer silently downgrades a scheduled receive
//! when the sender lacks `CAP_SCHEDULER` — it fails with
//! `PROTOCOL_UNSUPPORTED`, symmetric with the send side. The wire frames
//! grew accordingly (ak-wire wave 23: `TaskOpen.pairing_id`/`direction`,
//! `Admission.lease_ms`; both ends must ship the same version).
#![forbid(unsafe_code)]

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ak_core::error::ErrorCode;
use ak_core::handle::{ConflictPolicy, FileSink, SaveTarget, SourceHandle};
use ak_core::manifest::{FileEntry, Manifest};
use ak_core::policy::{DirectPolicy, PathKind};
use ak_identity::tls::TlsBundle;
use ak_identity::Fingerprint;
use ak_net::{DirectChannel, DirectEndpoint, DirectRecvStream, DirectSendStream, DirectStream};
use ak_storage::{LocalSaveTarget, LocalSource};
use ak_store::{CheckpointStore, Direction, FileCheckpoint, FileCheckpointStore, TaskRecord};
use ak_wire::{
    scheduler_frames_allowed, AdmissionReason, Control, Frame, JobId, Limits, Message, ResumeEntry,
    WireEntry, CAP_MULTISTREAM, CAP_PAUSE_RESUME, CAP_RESUME, CAP_SCHEDULER, DIRECTION_SEND,
};

/// Manifest pages are assembled under this byte budget so every page fits a
/// frame comfortably below `ak_wire::MAX_FRAME_BYTES`.
const PAGE_BUDGET_BYTES: usize = 4 << 20;
/// Checkpoint write cadence, in acknowledged chunks.
const CHECKPOINT_EVERY_ACKS: u64 = 16;
/// Wire protocol version this engine speaks.
const PROTO_VERSION: u16 = 1;

/// The one decode-limits profile used by every engine receive site in both
/// roles. Encoding has no limits, so the decoder budget must cover whatever
/// the peer's encoder produces: the receiver shards its resume answer into
/// ResumeRequest frames of at most `max_resume_entries` entries each
/// ([`resume_request_frames`]), so matching
/// [`ak_wire::MAX_RESUME_ENTRIES`] keeps a full shard decodable while the
/// multi-frame exchange itself carries no per-job entry cap.
fn engine_limits() -> Limits {
    Limits {
        max_chunk_bytes: 8 << 20,
        max_entries_per_page: 256,
        max_string_bytes: 1024,
        max_error_message_bytes: 256,
        max_resume_entries: ak_wire::MAX_RESUME_ENTRIES,
        max_chunk_digests: 65536,
    }
}
/// Default idle timeout applied to every frame receive wait.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on concurrent per-file streams in multistream mode.
const MAX_PARALLEL_STREAMS: usize = 4;
/// QUIC application-error code for a policy-gate refusal: `0xA0` marks the
/// AK protocol family, `0x01` the policy gate. An application-space code
/// (no IANA transport meaning), carried in the CONNECTION_CLOSE frame when
/// [`authorize_path`] refuses a path so a peer's transport log can name the
/// reason. No protocol frame precedes it.
const POLICY_GATE_CLOSE_CODE: u32 = 0xA0_01;
/// QUIC application-error code for the on-channel pinned-identity mismatch
/// (wave 23, review P1-4): `0xA0` protocol family, `0x02` identity
/// re-check. Carried in the CONNECTION_CLOSE frame when
/// [`send_on_channel`] refuses a runtime-injected channel whose TLS peer
/// fingerprint differs from `spec.pinned` — the identity condition the
/// legacy [`send`] establishes itself at connect time. No protocol frame
/// precedes it (not even the admission TaskOpen).
const PEER_MISMATCH_CLOSE_CODE: u32 = 0xA0_02;

/// Sender-side configuration.
#[derive(Debug, Clone)]
pub struct SendSpec {
    /// File or directory to send.
    pub source_root: PathBuf,
    /// Additional files/directories sent in the same job. Empty (the
    /// default) keeps the frozen single-root behavior; otherwise every
    /// root's manifest entries are prefixed with that root's name
    /// (`<root_name>/<rel_path>`) and merged into one manifest. Root names
    /// must be unique; duplicates fail the send with an `InvalidInput` io
    /// error before any connection is made.
    pub extra_roots: Vec<PathBuf>,
    pub peer: SocketAddr,
    /// Expected peer fingerprint; the connection aborts on mismatch.
    pub pinned: Fingerprint,
    /// Our TLS identity (self-signed certificate for this device key).
    pub identity: TlsBundle,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Local bind (0.0.0.0:0 for auto).
    pub bind: SocketAddr,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
    /// Maximum unacknowledged chunks in flight.
    pub in_flight_window: u32,
    /// Abort when no frame arrives within this budget on any receive wait
    /// (including while paused or waiting for acks).
    pub idle_timeout: Duration,
    /// Opt into parallel per-file streams. Takes effect only when the peer
    /// also advertises `CAP_MULTISTREAM`; otherwise the single-stream path
    /// runs. Also caps concurrent streams at `min(in_flight_window, 4)`.
    pub multistream: bool,
    /// Route policy gate, applied to the connected channel's path before
    /// any protocol frame is sent (see [`authorize_path`]). Default:
    /// `DirectPolicy::default()` (`allow_internet = true`,
    /// `allow_cellular = false`). A `LocalDirect` path always passes.
    pub policy: DirectPolicy,
}

impl SendSpec {
    /// Defaults for every field: default chunk size, `Fail` conflict
    /// policy, auto bind, temp checkpoint dir, window 8, single-stream.
    pub fn new(
        source_root: PathBuf,
        peer: SocketAddr,
        pinned: Fingerprint,
        identity: TlsBundle,
    ) -> Self {
        Self {
            source_root,
            extra_roots: Vec::new(),
            peer,
            pinned,
            identity,
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
            in_flight_window: 8,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            multistream: false,
            policy: DirectPolicy::default(),
        }
    }

    /// Append another file or directory to this send job (multi-root send).
    /// Root names must be unique across the job; see [`SendSpec::extra_roots`].
    pub fn add_root(&mut self, path: PathBuf) {
        self.extra_roots.push(path);
    }
}

/// Receiver-side configuration.
pub struct ReceiveSpec {
    pub out_dir: PathBuf,
    pub bind: SocketAddr,
    /// Our TLS identity (self-signed certificate for this device key).
    pub identity: TlsBundle,
    pub chunk_size: NonZeroU32,
    pub conflict: ConflictPolicy,
    /// Consent callback: channel-level peer authorization. Called once per
    /// connection with the verified peer fingerprint.
    pub authorize: Box<dyn Fn(&Fingerprint) -> bool + Send + Sync>,
    pub device_name: String,
    pub checkpoint_dir: PathBuf,
    /// Abort when no frame arrives within this budget on any receive wait.
    pub idle_timeout: Duration,
    /// Route policy gate, applied to the accepted channel's path before
    /// any protocol frame is received or sent (see [`authorize_path`]).
    /// Default: `DirectPolicy::default()` (`allow_internet = true`,
    /// `allow_cellular = false`). A `LocalDirect` path always passes.
    pub policy: DirectPolicy,
}

impl ReceiveSpec {
    /// Defaults for every field: bind 0.0.0.0:7100, default chunk size,
    /// `Fail` conflict policy, authorize-everyone callback, temp
    /// checkpoint dir.
    pub fn new(out_dir: PathBuf, identity: TlsBundle) -> Self {
        Self {
            out_dir,
            bind: SocketAddr::from(([0, 0, 0, 0], 7100)),
            identity,
            chunk_size: ak_core::handle::default_chunk_size(),
            conflict: ConflictPolicy::Fail,
            authorize: Box::new(|_| true),
            device_name: String::new(),
            checkpoint_dir: std::env::temp_dir().join("ak-sending-checkpoints"),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            policy: DirectPolicy::default(),
        }
    }
}

/// Scheduler admission context for one on-channel send task (wave 21,
/// fields extended wave 23; scheduler.md §4/§6). Carries exactly what the
/// three admission frames need: the scheduler task identity, the
/// runtime-minted permit, the two-dimensional code group the task runs
/// under, and the pairing relation that selected the target device.
/// Handed to [`send_on_channel`] by the runtime that owns the connection;
/// the legacy [`send`] path has none.
///
/// Not constructible by field assignment from outside this crate (wave 23,
/// review P1-4): the fields are crate-private and the only public
/// constructor is the validating [`AdmissionCtx::new`], per the frozen
/// contract's "admission contexts come from a controlled constructor" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionCtx {
    /// Scheduler task identity. Travels as `TaskOpen.job_id`, mirrors
    /// `AdmissionAck.job_id`, and — on the receiving side — keys the
    /// per-task staging tree (`ak_storage::prepare_file_staged`). The
    /// all-zero id is the contract sentinel and is refused by [`Self::new`]
    /// and on the wire.
    pub(crate) task_id: [u8; 16],
    /// The permit id the owning runtime minted when it granted the local
    /// tentative permit (scheduler.md §6): attempt 0 of the handshake runs
    /// on it, and `StalePermit` resubmits derive fresh ids under the
    /// bounded resubmit budget. Never the all-zero sentinel.
    pub(crate) permit_id: [u8; 16],
    /// The code group this task belongs to (the pairing `code_id` in its
    /// 32-byte form). The receiver hands this to its admission decider.
    pub(crate) group: [u8; 32],
    /// Registry identity that owns the group (`TaskOpen.group_registry`).
    pub(crate) group_registry: [u8; 32],
    /// The pairing relation that selected the target device
    /// (`TaskOpen.pairing_id`, scheduler.md §2: one code may be redeemed by
    /// several devices, so the task must name which relation it runs on).
    pub(crate) pairing_id: [u8; 16],
}

impl AdmissionCtx {
    /// Controlled constructor, the only way to build an [`AdmissionCtx`]
    /// (wave 23, review P1-4; scheduler.md §4: the admission context is
    /// produced by a controlled constructor — a caller cannot bypass
    /// validation by filling in fields). Validates the full identity set:
    /// `task_id`/`permit_id` must be non-zero, and `group_registry`/
    /// `pairing_id` must be non-zero (the all-zero values are the reserved
    /// sentinels and can never name a real task, permit, registry or
    /// pairing relation). Returns [`EngineError::InvalidAdmissionCtx`]
    /// naming the first offending field.
    pub fn new(
        task_id: [u8; 16],
        permit_id: [u8; 16],
        group: [u8; 32],
        group_registry: [u8; 32],
        pairing_id: [u8; 16],
    ) -> Result<Self, EngineError> {
        for (field, zero) in [
            ("task_id", task_id == [0u8; 16]),
            ("permit_id", permit_id == [0u8; 16]),
            ("group_registry", group_registry == [0u8; 32]),
            ("pairing_id", pairing_id == [0u8; 16]),
        ] {
            if zero {
                return Err(EngineError::InvalidAdmissionCtx { field });
            }
        }
        Ok(Self {
            task_id,
            permit_id,
            group,
            group_registry,
            pairing_id,
        })
    }
}

/// Receiver-side view of one `TaskOpen` (wave 23, review P1-4): the
/// complete admission context the decider of [`receive_on_channel`] needs —
/// the code group, the registry owning it, and the pairing relation the
/// sender claims. By the time the decider runs, the receiving engine has
/// already verified the channel identity (TLS-pinned fingerprint) and
/// refused the all-zero sentinels and a wrong `direction` tag, so the
/// decider decides on a structurally valid request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionRequest {
    /// The code group (`TaskOpen.group`, the pairing `code_id` in its
    /// 32-byte form).
    pub group: [u8; 32],
    /// Registry identity that owns the group (`TaskOpen.group_registry`).
    pub group_registry: [u8; 32],
    /// Pairing relation the sender claims (`TaskOpen.pairing_id`).
    pub pairing_id: [u8; 16],
}

/// Receiver-side admission verdict returned by the decider callback of
/// [`receive_on_channel`]. `Deny` carries the closed wire
/// [`AdmissionReason`] the receiver answers with (and the local call fails
/// with the same reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Grant the permit; the receiver waits for `AdmissionAck` and then
    /// runs the frozen receive body with per-task staging.
    Admit,
    /// Refuse the task; the `Admission` frame carries this reason and the
    /// call returns [`EngineError::AdmissionDenied`] locally.
    Deny(AdmissionReason),
}

/// Receiver-side admission decider callback of [`receive_on_channel`]:
/// handed the complete [`AdmissionRequest`] context of the sender's
/// `TaskOpen` (code group + registry + pairing relation, wave 23), answers
/// [`AdmissionDecision`].
pub type AdmissionDecider<'a> = &'a (dyn Fn(&AdmissionRequest) -> AdmissionDecision + Send + Sync);

/// Outcome of [`channel_scheduler_handshake`] (wave 27): what a daemon
/// learned about an accepted channel's peer before any admission decision
/// is made.
///
/// * [`SchedulerHandshake::Scheduler`] — the peer advertises
///   `CAP_SCHEDULER` and its `TaskOpen` has been read off the wire (the
///   retained fields are in the [`TaskOpenAdmission`] value). The upper
///   layer validates the pairing relation, resolves the two-dimensional
///   code group and, once its scheduler grants the permit, completes the
///   task with [`receive_on_channel_preadmitted`].
/// * [`SchedulerHandshake::Legacy`] — the peer's Hello carried no
///   `CAP_SCHEDULER`; a legacy sender never sends a `TaskOpen`. The
///   Hello-exchanged protocol stream is handed back in
///   [`HelloStream`] so [`receive_after_hello`] can resume the frozen
///   receive body on the SAME connection (wave 27 integrator fix: the
///   stream halves used to drop here, forcing the daemon to kill the
///   legacy peer's connection right after its Hello).
#[derive(Debug)]
pub enum SchedulerHandshake {
    /// Peer without `CAP_SCHEDULER`: legacy protocol; the upper layer
    /// serves it with [`receive_after_hello`].
    Legacy {
        /// The peer's Hello capability bits, verbatim (e.g. to inspect
        /// `CAP_RESUME`/`CAP_MULTISTREAM` before choosing the legacy path).
        peer_capabilities: u16,
        /// The live protocol stream with the Hello exchange already done;
        /// feed it to [`receive_after_hello`] to run the frozen receive
        /// body (unstaged, no admission) on this same connection.
        stream: HelloStream,
    },
    /// Peer advertises `CAP_SCHEDULER` and has sent its `TaskOpen` (the
    /// reply is pending — see [`TaskOpenAdmission`]).
    Scheduler {
        /// The read-and-retained admission fields plus the live protocol
        /// stream; feed the same value back into
        /// [`receive_on_channel_preadmitted`] once the scheduler grants.
        open: TaskOpenAdmission,
    },
}

/// The Hello-exchanged protocol stream of a legacy peer, handed back by
/// [`SchedulerHandshake::Legacy`] (wave 27). Opaque to callers: the only
/// thing the upper layer can do with it is feed it back into
/// [`receive_after_hello`], which resumes the frozen unstaged receive body
/// on the same stream. Fields are private by design, mirroring
/// [`TaskOpenAdmission`]'s retention pattern.
pub struct HelloStream {
    pub(crate) tx: DirectSendStream,
    pub(crate) rx: DirectRecvStream,
}

impl std::fmt::Debug for HelloStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The stream halves carry no meaningful debug payload; name the
        // state instead of leaking quinn internals (same reasoning as
        // TaskOpenAdmission's hand-written Debug).
        f.debug_struct("HelloStream")
            .field("state", &"hello-exchanged")
            .finish()
    }
}

/// The admission fields of one wire-read `TaskOpen`, retained for the
/// pre-admitted receive entry (wave 27, scheduler.md §6). Produced ONLY by
/// [`channel_scheduler_handshake`]; the fields are private and readable
/// through the accessors, so the upper layer (ak-session) can bind the
/// task to its code group before the scheduler has granted a permit.
///
/// Besides the frozen admission field set (`job_id`/`permit_id`/`group`/
/// `group_registry`/`pairing_id`/`direction`) the value privately retains
/// the accepted protocol stream with its Hello exchange already complete
/// and the peer's capabilities: the daemon-side protocol is single-stream
/// (one bidi stream carries Hello, admission and the whole file body), so
/// the only way [`receive_on_channel_preadmitted`] can answer the pending
/// `Admission` in place is to find the exact stream state here. Dropping
/// the value drops that stream.
///
/// The wire-level fail-closed rules already applied by the handshake:
/// `job_id`, `pairing_id` and `group_registry` are non-zero (the same
/// sentinel refusal the combined [`receive_on_channel`] path performs
/// before its decider). `direction` is retained verbatim and is checked
/// by [`receive_on_channel_preadmitted`].
pub struct TaskOpenAdmission {
    /// `TaskOpen.job_id` — the scheduler task identity; on the receiving
    /// side this keys the per-task staging tree.
    job_id: [u8; 16],
    /// `TaskOpen.permit_id` — the permit the sender's runtime minted.
    permit_id: [u8; 16],
    /// `TaskOpen.group` — the code group (the pairing `code_id`, 32-byte
    /// form).
    group: [u8; 32],
    /// `TaskOpen.group_registry` — the registry owning the group.
    group_registry: [u8; 32],
    /// `TaskOpen.pairing_id` — the pairing relation that selected us.
    pairing_id: [u8; 16],
    /// `TaskOpen.direction` — verbatim wire tag (0 = the peer sends,
    /// 1 = the peer receives). The pre-admitted entry requires the
    /// sending direction.
    direction: u8,
    /// Retained wire state (not part of the frozen admission field set):
    /// the accepted stream halves after the Hello exchange, and the peer
    /// capabilities read from its Hello (the frozen body needs them for
    /// the multistream decision).
    peer_caps: u16,
    tx: DirectSendStream,
    rx: DirectRecvStream,
}

impl TaskOpenAdmission {
    /// `TaskOpen.job_id`: the scheduler task identity that keys the
    /// per-task staging tree on the receiving side.
    pub fn job_id(&self) -> [u8; 16] {
        self.job_id
    }

    /// `TaskOpen.permit_id`: the permit id the sender's runtime minted.
    pub fn permit_id(&self) -> [u8; 16] {
        self.permit_id
    }

    /// `TaskOpen.group`: the code group (the pairing `code_id`, 32-byte
    /// form).
    pub fn group(&self) -> [u8; 32] {
        self.group
    }

    /// `TaskOpen.group_registry`: the registry identity owning the group.
    pub fn group_registry(&self) -> [u8; 32] {
        self.group_registry
    }

    /// `TaskOpen.pairing_id`: the pairing relation that selected this
    /// device.
    pub fn pairing_id(&self) -> [u8; 16] {
        self.pairing_id
    }

    /// `TaskOpen.direction` verbatim (0 = the peer sends, 1 = the peer
    /// receives). [`receive_on_channel_preadmitted`] requires the sending
    /// direction.
    pub fn direction(&self) -> u8 {
        self.direction
    }

    /// The admission fields in the exact shape the combined
    /// [`receive_on_channel`] hands its decider ([`AdmissionRequest`]) —
    /// convenient for the upper layer's pairing/group validation.
    pub fn admission_request(&self) -> AdmissionRequest {
        AdmissionRequest {
            group: self.group,
            group_registry: self.group_registry,
            pairing_id: self.pairing_id,
        }
    }
}

impl std::fmt::Debug for TaskOpenAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The retained stream halves are not Debug; the admission fields
        // are the identity that matters.
        f.debug_struct("TaskOpenAdmission")
            .field("job_id", &self.job_id)
            .field("permit_id", &self.permit_id)
            .field("group", &self.group)
            .field("group_registry", &self.group_registry)
            .field("pairing_id", &self.pairing_id)
            .field("direction", &self.direction)
            .field("peer_caps", &self.peer_caps)
            .finish_non_exhaustive()
    }
}

/// Progress events, distinct per direction so the CLI can render honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Connecting,
    Connected {
        peer: String,
    },
    Offered {
        files: u64,
        bytes: u64,
    },
    Accepted {
        files: u64,
        bytes: u64,
    },
    ManifestPageSent {
        page: u32,
    },
    ManifestReceived,
    Resuming {
        chunks: u64,
    },
    FileStarted {
        file_index: u32,
        path: String,
        bytes: u64,
    },
    ChunkSent {
        file_index: u32,
        chunk_index: u64,
    },
    ChunkReceived {
        file_index: u32,
        chunk_index: u64,
    },
    /// Cumulative verified bytes of one file, piggybacked on the checkpoint
    /// cadence (every `CHECKPOINT_EVERY_ACKS` acknowledged chunks).
    BytesAcked {
        file_index: u32,
        cumulative_bytes: u64,
    },
    /// The peer asked us to stop sending; we hold until Resume or timeout.
    Paused,
    /// The peer let us continue after a Pause.
    Resumed,
    FileVerified {
        file_index: u32,
    },
    FileCommitted {
        file_index: u32,
        path: String,
    },
    Verifying,
    FinalAck {
        verified: bool,
    },
    Done,
}

/// Final report. Presence of this struct means the transfer completed and,
/// on the receive side, every file passed whole-digest verification and was
/// committed with an atomic rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub job_id: JobId,
    pub files: u64,
    pub bytes: u64,
    pub duration: Duration,
    /// Chunks not re-sent/re-received thanks to verified part files.
    pub resumed_chunks: u64,
    pub peer_fingerprint: Option<String>,
    /// Root names of the job on the sender (primary first, then extras in
    /// the order they were added). Empty on the receiver side.
    pub roots: Vec<String>,
}

/// Summary of one offered transfer, handed to a [`TransferConsent`]
/// callback at the Offer handling point (wave 28): everything the receiving
/// user needs to make the accept/decline decision before a single payload
/// byte or disk write happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentInfo {
    /// The offered job (matches the wire `Offer.job_id`).
    pub job_id: JobId,
    /// Offered root name (the sender's label for the payload).
    pub root_name: String,
    /// Number of files the offer announces.
    pub file_count: u64,
    /// Total payload bytes the offer announces.
    pub total_bytes: u64,
    /// The sender's VERIFIED TLS fingerprint (wave 29, review P1-04): the
    /// channel identity the transport established before the Offer was
    /// read. Consent decisions can be attributed to a specific paired
    /// device instead of to whatever listener the offer happened to reach.
    pub sender_fingerprint: Fingerprint,
}

/// What one [`TransferConsent`] hook answered (wave 29, review P2-07):
/// either the decision is already known, or the callback registered the
/// request out-of-band and returned immediately — the receive body then
/// awaits the pending half WITHOUT blocking the calling worker (a plain
/// bool callback had to park inside the async body, starving bounded
/// runtimes while a human decided).
pub enum ConsentAnswer {
    /// Decide now: `true` accepts the offer, `false` declines it.
    Now(bool),
    /// The decision is pending elsewhere (a UI, a 30 s window, a teardown).
    /// The receiver end resolves it; a dropped sender half (no decider
    /// left) fails closed as a declination.
    Pending(tokio::sync::oneshot::Receiver<bool>),
}

/// Optional receive-side consent hook (wave 28; shape made non-blocking
/// wave 29): invoked once per receive with the offer summary; the callback
/// returns IMMEDIATELY with either the decision or the pending half
/// ([`ConsentAnswer`]) — it must never block the calling thread. `false`
/// refuses the transfer — the peer is told over the wire with the DECLINED
/// semantics ([`DECLINED_WIRE_CODE`], see also
/// [`EngineError::ConsentDeclined`]) and the connection closes before
/// anything is written to disk. `None` (every pre-wave-28 entry) keeps the
/// frozen behavior: the offer is accepted automatically.
pub type TransferConsent = Arc<dyn Fn(&ConsentInfo) -> ConsentAnswer + Send + Sync>;

/// Wire code of a user declination (wave 28). The closed `ErrorCode` set
/// (frozen, ak-wire) has no dedicated DECLINED value, so the refusal rides
/// [`ErrorCode::LocalPermissionDenied`] — "this endpoint's user refused" —
/// with the fixed [`DECLINED_MESSAGE`] text so peers can tell a declination
/// from a filesystem permission failure.
pub const DECLINED_WIRE_CODE: ErrorCode = ErrorCode::LocalPermissionDenied;

/// Fixed message text of the DECLINED refusal frame.
pub const DECLINED_MESSAGE: &str = "DECLINED: receiving user declined the transfer";

/// Bounded drain wait after the DECLINED frame (wave 28): the refusal is
/// a user decision the peer must see, so after sending the frame and
/// half-closing the send stream the receiver holds the connection until
/// the peer reacts (a well-behaved sender drops the connection on an
/// Error frame) or this bound expires — never longer. Without it the
/// immediate channel teardown on return could discard an unsent frame
/// (the same best-effort trade the frozen refusal paths accept).
const DECLINED_DRAIN_WAIT: Duration = Duration::from_millis(500);

/// Transfer failure taxonomy: connect/stream errors, peer refusals and
/// errors, timeouts, checkpoint/io problems, verification failure, and the
/// policy-gate refusal ([`EngineError::RouteForbidden`]).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("connect failed: {0}")]
    Connect(#[from] ak_net::ConnectError),
    #[error("stream failed: {0}")]
    Stream(#[from] ak_net::StreamError),
    #[error("peer rejected the offer: {0}")]
    Rejected(ak_core::error::ErrorCode),
    #[error("peer sent error {code}: {message}")]
    PeerError { code: ErrorCode, message: String },
    #[error("peer vanished mid-transfer (checkpoint kept)")]
    PeerClosed,
    #[error("idle timeout: no frame within {0:?} (checkpoint kept)")]
    IdleTimeout(Duration),
    #[error("source changed during transfer")]
    SourceChanged,
    #[error("storage error: {0}")]
    Storage(#[from] ak_storage::OpenError),
    #[error("destination storage full: {available} bytes available, {needed} needed")]
    StorageFull {
        /// Free bytes the destination reported at precheck time.
        available: u64,
        /// Total payload bytes the job needs.
        needed: u64,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checkpoint store error: {0}")]
    Checkpoint(#[source] std::io::Error),
    #[error("final verification failed")]
    VerificationFailed,
    #[error("cancelled")]
    Cancelled,
    /// The connected route's path kind is not allowed by the configured
    /// [`DirectPolicy`] (internet refused by policy, or a Relay/Unknown
    /// path that must never carry file data — see [`authorize_path`]).
    /// The channel is closed with `POLICY_GATE_CLOSE_CODE` before any
    /// protocol byte moves.
    #[error("route refused by policy: {0:?}")]
    RouteForbidden(PathKind),
    /// The scheduler admission handshake refused this task (wave 21).
    /// On the send side the *peer* denied [`send_on_channel`]'s TaskOpen —
    /// `SerialWait`/`Capacity` are "retry later" denials, and `StalePermit`
    /// only surfaces here after the bounded resubmit budget ran out. On
    /// the receive side it is the local decider's own
    /// [`AdmissionDecision::Deny`] reason echoed back. No part of the file
    /// protocol ran: no Offer was sent or accepted.
    #[error("admission denied: {reason:?}")]
    AdmissionDenied {
        /// The closed wire denial cause the handshake ended with.
        reason: AdmissionReason,
    },
    /// The runtime-injected channel's TLS peer is not the fingerprint the
    /// spec pins (wave 23, review P1-4). [`send_on_channel`] re-checks, as
    /// its very first step, the identity condition the legacy [`send`]
    /// establishes itself at connect time; a mismatched channel is closed
    /// with ``PEER_MISMATCH_CLOSE_CODE`` before any protocol byte moves —
    /// not even the sources are scanned.
    #[error("channel peer {actual} is not the pinned fingerprint {expected}")]
    PeerMismatch {
        /// Fingerprint the injected channel actually verified.
        actual: String,
        /// Fingerprint the spec pins (hex).
        expected: String,
    },
    /// [`AdmissionCtx::new`] refused an identity field (wave 23): the
    /// all-zero value is the reserved sentinel and can never key a real
    /// scheduler task, permit, registry or pairing relation.
    #[error("invalid admission context: {field} must not be the all-zero sentinel")]
    InvalidAdmissionCtx {
        /// Name of the offending field.
        field: &'static str,
    },
    /// The receiving user declined the offered transfer through a
    /// [`TransferConsent`] hook (wave 28). The peer was told over the wire
    /// with the DECLINED semantics ([`DECLINED_WIRE_CODE`]) and the
    /// connection was closed at the Offer handling point: no part file, no
    /// checkpoint record, nothing was written.
    #[error("declined by the receiving user: job {job_id:?}")]
    ConsentDeclined {
        /// The refused job (the wire `Offer.job_id`).
        job_id: JobId,
    },
}

/// Deterministic job id from source identity (see module docs).
pub fn job_id_for(root_name: &str, manifest: &Manifest) -> JobId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"AKJOB1");
    hasher.update(root_name.as_bytes());
    hasher.update(&manifest.chunk_size.get().to_be_bytes());
    for entry in &manifest.entries {
        hasher.update(&[u8::from(entry.is_dir)]);
        hasher.update(
            entry
                .rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .as_bytes(),
        );
        hasher.update(&entry.size.to_be_bytes());
        hasher.update(&entry.mtime_unix_ms.to_be_bytes());
        hasher.update(&entry.whole_digest);
    }
    let digest = hasher.finalize();
    let mut id = JobId::default();
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Deterministic *record* id for one transfer instance: the checkpoint
/// (`<record>.akjob`) key and the `TaskRecord.job_id` field. Unlike the
/// content id ([`job_id_for`], still what the wire speaks), this binds the
/// peer fingerprint, the direction and the root (`source_root` for a send,
/// canonical destination root for a receive), hashed as:
///
/// ```text
/// blake3("AKREC1" || content_id[16] || peer_fingerprint[32]
///        || direction_byte(0=send,1=receive) || root_utf8_with_slashes)[..16]
/// ```
///
/// Consequences (all intentional): the same source sent to two peers keeps
/// two sender records; a machine that sends and receives through one shared
/// checkpoint directory keeps a send record and a receive record; retrying
/// the same transfer instance (same content, peer, direction and root)
/// derives the same record id, which is what makes the committed-file skip
/// of a retry find the previous run's record. Protocol note: no wire frame
/// ever carries this value — `Offer.job_id` and friends keep the content
/// id, so the frozen v1 wire format is untouched.
pub fn record_id_for(
    content_id: &JobId,
    peer: &Fingerprint,
    direction: Direction,
    root: &Path,
) -> JobId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"AKREC1");
    hasher.update(content_id);
    hasher.update(peer.as_bytes());
    hasher.update(&[match direction {
        Direction::Send => 0,
        Direction::Receive => 1,
    }]);
    hasher.update(root.to_string_lossy().replace('\\', "/").as_bytes());
    let digest = hasher.finalize();
    let mut id = JobId::default();
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Pure Windows-savability decision for the receive side (CR-P3-02,
/// manifest leg): the first manifest entry whose `rel_path` contains a
/// component Windows cannot save, rendered as `<path>: <issue>` for the
/// refusal message. `None` = every entry is fine. A non-UTF-8 component is
/// itself unsavable (the checker cannot even name it). Pure — the same
/// decision runs on every platform in tests; only the receive path arms it
/// on Windows.
pub fn windows_unsavable_entry(entries: &[FileEntry]) -> Option<String> {
    entries.iter().find_map(|entry| {
        entry.rel_path.components().find_map(|component| {
            let issue = match component.as_os_str().to_str() {
                Some(name) => {
                    ak_core::manifest::windows_incompatible(name).map(|issue| issue.to_string())
                }
                None => Some("path component is not valid Unicode".to_owned()),
            };
            issue.map(|issue| format!("{}: {issue}", entry.rel_path.display()))
        })
    })
}

/// Pure storage-headroom decision for the receive side. Unknown capacity
/// (`None`: the platform cannot report free space) passes — exhaustion is
/// still caught later by real write errors, never faked here. An exact fit
/// passes; any shortage fails with [`EngineError::StorageFull`] carrying the
/// real reported numbers.
pub fn check_headroom(available: Option<u64>, total_needed: u64) -> Result<(), EngineError> {
    match available {
        Some(available) if available < total_needed => Err(EngineError::StorageFull {
            available,
            needed: total_needed,
        }),
        _ => Ok(()),
    }
}

/// Pure pre-transport policy gate (ADR-001: a path that is unknown or not
/// allowed by policy never enters the file-data channel):
/// - `LocalDirect` always passes — local-first, no policy can lock out the
///   loopback/LAN path;
/// - `InternetDirect` passes only when `policy.allow_internet`;
/// - `Relay` and `Unknown` are always refused: neither can be produced by
///   the direct-only transport (`ak-net` classifies only LocalDirect /
///   InternetDirect), so meeting one is a defect — refuse defensively.
///
/// Relation to [`DirectPolicy::choose`]'s `Observation` model: the facts
/// `choose` re-checks per candidate (`peer_id` match, `authenticated`,
/// `encrypted`, `receive_authorized`) are deliberately NOT re-filled as
/// booleans here, because the transport already guarantees them **by
/// type** — the ADR-002 D6 constraint landing point. A
/// [`DirectChannel`] exists only after a TLS handshake that verified the
/// peer's self-signed certificate against the pinned fingerprint (that is
/// the identity/authentication/encryption triple), and the receiver's
/// `authorize` callback supplied channel-level consent at accept time.
/// Likewise `channel.path()` is the trusted `classify` product of
/// `ak-net`, not a caller-asserted flag. This gate is therefore a purely
/// permissive filter over the path kind — not a re-declaration of
/// Observation booleans, which would only create a place for untrusted
/// input to sneak back in. `Observation::cellular` has no
/// transport-level equivalent (a QUIC connection does not expose the
/// underlying network interface), so `allow_cellular` stays advisory in
/// the engine until an interface-aware adapter exists; and `choose`'s
/// local-probe sequencing (`WaitReason::LocalProbePending`) belongs to
/// candidate selection *before* connecting, not to this per-connection
/// gate.
pub fn authorize_path(policy: &DirectPolicy, path: PathKind) -> Result<(), EngineError> {
    match path {
        PathKind::LocalDirect => Ok(()),
        PathKind::InternetDirect if policy.allow_internet => Ok(()),
        PathKind::InternetDirect | PathKind::Relay | PathKind::Unknown => {
            Err(EngineError::RouteForbidden(path))
        }
    }
}

/// Gate a freshly obtained channel: refuse (and close with
/// [`POLICY_GATE_CLOSE_CODE`]) before a single protocol byte is sent or
/// accepted. Called at exactly two sites: `send` right after
/// `connect_bounded`, and `receive_with_addr` right after `accept` — in
/// both cases before the first stream is opened or the first frame moves.
fn gate_channel(policy: &DirectPolicy, channel: &DirectChannel) -> Result<(), EngineError> {
    if let Err(error) = authorize_path(policy, channel.path()) {
        channel.close(POLICY_GATE_CLOSE_CODE, b"route refused by policy");
        Err(error)
    } else {
        Ok(())
    }
}

fn now_ms() -> u64 {
    ak_store::now_unix_ms()
}

fn hello_frame(name: &str, identity: &TlsBundle, capabilities: u16) -> Frame {
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
/// Pause/Resume, and accept per-file parallel streams.
const RECV_CAPABILITIES: u16 = CAP_RESUME | CAP_PAUSE_RESUME | CAP_MULTISTREAM;

async fn expect_hello(
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
async fn recv_within(
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
async fn recv_admission_frame(
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

fn unexpected(message: Message) -> EngineError {
    EngineError::PeerError {
        code: ErrorCode::ProtocolUnsupported,
        message: format!("unexpected frame: {message:?}"),
    }
}

/// A peer-caused protocol violation with a stable wire code (review round
/// 17): used for cross-frame validation failures — wrong job id, manifest
/// page order violations, resume entry budgets, and the like.
fn protocol_error(message: impl Into<String>) -> EngineError {
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
const ADMISSION_LEASE_MS: u32 = 30_000;

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
async fn admission_handshake_sender(
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
struct TaskOpenWire {
    task_id: [u8; 16],
    permit_id: [u8; 16],
    request: AdmissionRequest,
    direction: u8,
}

/// Read one `TaskOpen` frame off the wire (header-budgeted through
/// [`recv_admission_frame`]) and destructure its fields. No validation
/// here — callers pick the fail-closed set via [`validate_task_open`]
/// (sentinels only, e.g. the handshake entry retains `direction`
/// verbatim) or the full set including the direction tag.
async fn recv_task_open(
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
fn validate_task_open(
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
async fn admission_handshake_receiver(
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
/// a v6 peer (or vice versa) would otherwise hang in QUIC's retry loop with
/// no error (found by the IPv6 scenario tests); the idle budget bounds every
/// other unreachable-peer case too.
async fn connect_bounded(
    endpoint: &DirectEndpoint,
    peer: SocketAddr,
    pinned: &Fingerprint,
    idle: Duration,
) -> Result<DirectChannel, EngineError> {
    let local = endpoint.local_addr()?;
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

/// Assemble manifest pages under the byte budget.
fn assemble_pages(manifest: &Manifest, limits: &Limits) -> Vec<Vec<WireEntry>> {
    let mut pages: Vec<Vec<WireEntry>> = Vec::new();
    let mut current: Vec<WireEntry> = Vec::new();
    let mut current_bytes = 0usize;
    for entry in &manifest.entries {
        let wire = WireEntry::from(entry);
        let size = 128 + wire.rel_path.len() + 32 * wire.chunk_digests.len();
        let cap = usize::from(limits.max_entries_per_page);
        if !current.is_empty() && (current_bytes + size > PAGE_BUDGET_BYTES || current.len() >= cap)
        {
            pages.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += size;
        current.push(wire);
    }
    if !current.is_empty() || pages.is_empty() {
        pages.push(current);
    }
    pages
}

/// Shard the receiver's resume answer into ResumeRequest frames of at most
/// `max_per_frame` entries. Every frame but the last carries `more = true`;
/// an empty answer still produces exactly one empty frame with
/// `more = false`, so the exchange always terminates with a final frame the
/// sender can wait for. A `max_per_frame` of 0 is clamped to 1 (a shard
/// size, not a stall).
fn resume_request_frames(
    job_id: JobId,
    files: &[ResumeEntry],
    max_per_frame: usize,
) -> Vec<Message> {
    let max = max_per_frame.max(1);
    let mut frames: Vec<Message> = files
        .chunks(max)
        .map(|shard| Message::ResumeRequest {
            job_id,
            files: shard.to_vec(),
            more: true,
        })
        .collect();
    if let Some(Message::ResumeRequest { more, .. }) = frames.last_mut() {
        *more = false;
    } else {
        frames.push(Message::ResumeRequest {
            job_id,
            files: Vec::new(),
            more: false,
        });
    }
    frames
}

/// Sender-side accumulator for the sharded ResumeRequest exchange: folds in
/// frames until one arrives with `more = false`. Cross-frame validation
/// (review round 17): every frame must name the expected job; the total
/// entry count is budgeted by the negotiated manifest (one entry per
/// transferable file, so an endless `more = true` stream cannot grow memory
/// unboundedly); file indexes must be in range, must not name directories,
/// must not repeat; and each verified prefix may not exceed the file's
/// actual chunk count. Violations abort with a protocol error.
struct ResumeCollector {
    job_id: JobId,
    /// Per manifest entry: `None` for directories (never resumable), `Some`
    /// chunk count for files.
    chunk_counts: Vec<Option<u64>>,
    /// Number of transferable (non-directory) files: the total entry budget.
    capacity: usize,
    entries: Vec<ResumeEntry>,
    seen: HashSet<u32>,
    max_per_frame: usize,
}

impl ResumeCollector {
    fn new(job_id: JobId, manifest: &Manifest, limits: &Limits) -> Self {
        let chunk_counts: Vec<Option<u64>> = manifest
            .entries
            .iter()
            .map(|entry| {
                if entry.is_dir {
                    None
                } else {
                    Some(entry.chunk_digests.len() as u64)
                }
            })
            .collect();
        let capacity = chunk_counts.iter().filter(|slot| slot.is_some()).count();
        ResumeCollector {
            job_id,
            chunk_counts,
            capacity,
            entries: Vec::new(),
            seen: HashSet::new(),
            max_per_frame: usize::from(limits.max_resume_entries),
        }
    }

    /// Fold one frame's entries in; returns `true` when the exchange is
    /// complete (the frame carried `more = false`).
    fn push(
        &mut self,
        frame_job_id: JobId,
        files: Vec<ResumeEntry>,
        more: bool,
    ) -> Result<bool, EngineError> {
        if frame_job_id != self.job_id {
            return Err(protocol_error("resume frame names a different job"));
        }
        if files.len() > self.max_per_frame {
            return Err(protocol_error(format!(
                "resume frame carries {} entries, above the per-frame budget {}",
                files.len(),
                self.max_per_frame
            )));
        }
        for entry in files {
            let Some(chunks) = self
                .chunk_counts
                .get(entry.file_index as usize)
                .copied()
                .flatten()
            else {
                return Err(protocol_error(format!(
                    "resume entry names file index {}, which is not a file of this manifest",
                    entry.file_index
                )));
            };
            if !self.seen.insert(entry.file_index) {
                return Err(protocol_error(format!(
                    "resume entry repeats file index {}",
                    entry.file_index
                )));
            }
            if entry.verified_prefix_chunks > chunks {
                return Err(protocol_error(format!(
                    "resume prefix {} exceeds the {} chunks of file index {}",
                    entry.verified_prefix_chunks, chunks, entry.file_index
                )));
            }
            self.entries.push(entry);
        }
        if self.entries.len() > self.capacity {
            return Err(protocol_error(
                "resume answer carries more entries than the manifest has files",
            ));
        }
        Ok(!more)
    }
}

/// Receiver-side accumulator for ManifestPage frames with full cross-frame
/// validation (review round 17): every page must name the offered job;
/// `page_index` must be strictly 0-based and consecutive (no gaps, no
/// repeats); the running entry count may never exceed the offered
/// `file_count` (too many is refused immediately); the `last_page` frame
/// must close exactly at `file_count` entries whose sizes sum to the
/// offered `total_bytes`. This bounds the accumulated entry memory by the
/// negotiated offer instead of trusting an endless page stream.
struct ManifestAssembler {
    job_id: JobId,
    expected_files: u64,
    expected_bytes: u64,
    expected_page: u32,
    max_per_page: usize,
    entries: Vec<ak_core::manifest::FileEntry>,
    bytes: u64,
}

impl ManifestAssembler {
    fn new(job_id: JobId, file_count: u64, total_bytes: u64, limits: &Limits) -> Self {
        ManifestAssembler {
            job_id,
            expected_files: file_count,
            expected_bytes: total_bytes,
            expected_page: 0,
            max_per_page: usize::from(limits.max_entries_per_page),
            entries: Vec::new(),
            bytes: 0,
        }
    }

    /// Fold one page in; returns `Ok(true)` when the `last_page` frame
    /// closed a complete, offer-consistent manifest.
    fn push(
        &mut self,
        job_id: JobId,
        page_index: u32,
        last_page: bool,
        page: Vec<WireEntry>,
    ) -> Result<bool, EngineError> {
        if job_id != self.job_id {
            return Err(protocol_error("manifest page names a different job"));
        }
        if page_index != self.expected_page {
            return Err(protocol_error(format!(
                "manifest page_index {} violates the strict 0-based page order (expected {})",
                page_index, self.expected_page
            )));
        }
        if page.len() > self.max_per_page {
            return Err(protocol_error(format!(
                "manifest page carries {} entries, above the per-page budget {}",
                page.len(),
                self.max_per_page
            )));
        }
        if self.entries.len() as u64 + page.len() as u64 > self.expected_files {
            return Err(protocol_error(
                "manifest carries more entries than the offer announced",
            ));
        }
        for wire in page {
            let entry: ak_core::manifest::FileEntry = wire
                .try_into()
                .map_err(|_| protocol_error("peer sent an invalid manifest entry"))?;
            self.bytes = self.bytes.saturating_add(entry.size);
            self.entries.push(entry);
        }
        self.expected_page += 1;
        if last_page {
            if self.entries.len() as u64 != self.expected_files {
                return Err(protocol_error(
                    "manifest ended with fewer entries than the offer announced",
                ));
            }
            if self.bytes != self.expected_bytes {
                return Err(protocol_error(
                    "manifest entry sizes disagree with the offered total bytes",
                ));
            }
            return Ok(true);
        }
        Ok(false)
    }
}

fn sender_record(spec: &SendSpec, record_id: JobId, manifest: &Manifest) -> TaskRecord {
    TaskRecord {
        // The record key is the per-instance record id (see record_id_for);
        // the wire keeps speaking the content id in Offer and friends.
        job_id: record_id,
        direction: Direction::Send,
        root: spec.source_root.clone(),
        peer_fingerprint: Some(spec.pinned),
        chunk_size: spec.chunk_size.get(),
        total_files: manifest.file_count(),
        total_bytes: manifest.total_bytes(),
        state: "connecting".into(),
        created_unix_ms: now_ms(),
        updated_unix_ms: now_ms(),
        files: manifest
            .entries
            .iter()
            .enumerate()
            .map(|(index, e)| FileCheckpoint {
                file_index: index as u32,
                size: e.size,
                verified_prefix_chunks: 0,
            })
            .collect(),
    }
}

/// Sender-side source set: the primary root plus any extra roots, exposed to
/// the transfer loops as one manifest. With no extra roots this is an
/// identity wrapper, so the single-root wire behavior (manifest, job id,
/// Offer root name) is byte-for-byte the frozen one. With extras, every
/// entry of root `r` is re-pathed as `<root_name>/<rel_path>`; root names
/// must be unique or the job is refused up front.
struct SourceSet {
    sources: Vec<Arc<LocalSource>>,
    /// Display name of each root, in the same order as `sources`.
    names: Vec<String>,
    /// Merged manifest the wire protocol speaks.
    manifest: Manifest,
    /// Merged entry index -> (root ordinal, per-root entry index). Synthesized
    /// entries for entirely empty roots carry `u32::MAX` as their local index;
    /// they are directories and are never read.
    index_map: Vec<(u32, u32)>,
    /// True when more than one root was merged.
    multi: bool,
}

/// Map a manifest-construction failure (root name unusable as a transfer
/// path component, e.g. a Windows-reserved stem) to a clear invalid-input
/// error, before any connection is made.
fn invalid_root_name(name: &str) -> EngineError {
    EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("root name not usable as a transfer directory name: {name:?}"),
    ))
}

impl SourceSet {
    fn open(spec: &SendSpec) -> Result<Self, EngineError> {
        let mut sources = vec![Arc::new(LocalSource::open(
            &spec.source_root,
            spec.chunk_size,
        )?)];
        for extra in &spec.extra_roots {
            sources.push(Arc::new(LocalSource::open(extra, spec.chunk_size)?));
        }
        let names: Vec<String> = sources.iter().map(|s| s.root_name()).collect();
        let multi = !spec.extra_roots.is_empty();
        if multi {
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            if sorted.len() != names.len() {
                return Err(EngineError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("duplicate transfer root names: [{}]", names.join(", ")),
                )));
            }
        }
        let (manifest, index_map) = if multi {
            let mut entries = Vec::new();
            let mut index_map = Vec::new();
            for (ordinal, source) in sources.iter().enumerate() {
                let local = source.manifest();
                if local.entries.is_empty() {
                    // An entirely empty directory root still appears on the
                    // receiver as its own top-level directory.
                    entries.push(
                        FileEntry::directory(&names[ordinal])
                            .map_err(|_| invalid_root_name(&names[ordinal]))?,
                    );
                    index_map.push((ordinal as u32, u32::MAX));
                    continue;
                }
                for (local_index, entry) in local.entries.iter().enumerate() {
                    let rel = format!(
                        "{}/{}",
                        names[ordinal],
                        entry.rel_path.to_string_lossy().replace('\\', "/")
                    );
                    let merged = if entry.is_dir {
                        FileEntry::directory(&rel)
                    } else {
                        FileEntry::new(
                            &rel,
                            entry.size,
                            entry.mtime_unix_ms,
                            entry.whole_digest,
                            entry.chunk_digests.clone(),
                        )
                    }
                    .map_err(|_| invalid_root_name(&names[ordinal]))?;
                    entries.push(merged);
                    index_map.push((ordinal as u32, local_index as u32));
                }
            }
            (
                Manifest {
                    chunk_size: spec.chunk_size,
                    entries,
                },
                index_map,
            )
        } else {
            let manifest = sources[0].manifest().clone();
            let index_map = (0..manifest.entries.len() as u32).map(|i| (0, i)).collect();
            (manifest, index_map)
        };
        Ok(Self {
            sources,
            names,
            manifest,
            index_map,
            multi,
        })
    }

    /// Read a merged-index chunk through the owning root's source.
    fn read_chunk(
        &self,
        file_index: u32,
        chunk_index: u64,
        buf: &mut [u8],
    ) -> std::io::Result<usize> {
        let (ordinal, local_index) = self
            .index_map
            .get(file_index as usize)
            .copied()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "file index out of range")
            })?;
        let source = self.sources.get(ordinal as usize).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "root index out of range")
        })?;
        source.read_chunk(local_index, chunk_index, buf)
    }

    /// Re-check every root against its manifest.
    fn verify_stable(&self) -> Result<(), EngineError> {
        for source in &self.sources {
            source
                .verify_stable()
                .map_err(|_| EngineError::SourceChanged)?;
        }
        Ok(())
    }

    /// Name used for the Offer and the job id: the single root's name, or
    /// `"multi"` when several roots were merged.
    fn display_name(&self) -> &str {
        if self.multi {
            "multi"
        } else {
            &self.names[0]
        }
    }

    /// Root names for the report (primary first, extras in add order).
    fn roots(&self) -> Vec<String> {
        self.names.clone()
    }
}

/// Read one chunk through the blocking pool, returning exactly the chunk bytes.
async fn read_chunk_via(
    source: Arc<SourceSet>,
    file_index: u32,
    chunk_index: u64,
    len: usize,
) -> std::io::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; len];
        let read = source.read_chunk(file_index, chunk_index, &mut buf)?;
        buf.truncate(read);
        Ok(buf)
    })
    .await
    .unwrap_or_else(|join| {
        Err(std::io::Error::other(format!(
            "chunk read task failed: {join}"
        )))
    })
}

/// Outcome of applying one ChunkAck against the sent-but-unacknowledged set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckOutcome {
    /// The ack named a chunk we sent and had not been acknowledged yet.
    Confirmed,
    /// A duplicate of an already-confirmed chunk or a chunk we never sent:
    /// ignored and counted, never an error and never retiring an
    /// outstanding slot (review round 17, ACK idempotency).
    DuplicateOrUnknown,
}

/// Sent-but-unacknowledged chunk ids of one transfer (single stream) or one
/// file stream (multistream). Bounded by the in-flight window: ids leave the
/// set exactly when a genuine ack confirms them, so a peer replaying or
/// inventing acks can neither inflate nor fast-forward the window.
#[derive(Debug, Default)]
struct AckLedger {
    inflight: HashSet<(u32, u64)>,
    /// Duplicate/unknown acks seen so far (observability; asserted by tests).
    duplicates: u64,
}

impl AckLedger {
    fn track(&mut self, file_index: u32, chunk_index: u64) {
        self.inflight.insert((file_index, chunk_index));
    }

    fn apply(&mut self, file_index: u32, chunk_index: u64) -> AckOutcome {
        if self.inflight.remove(&(file_index, chunk_index)) {
            AckOutcome::Confirmed
        } else {
            self.duplicates += 1;
            log::debug!(
                "ignoring duplicate or unknown ChunkAck (file {file_index}, chunk {chunk_index})"
            );
            AckOutcome::DuplicateOrUnknown
        }
    }
}

/// Sender-side ack window state, shared across drain calls of one transfer.
struct AckWindow {
    outstanding: u32,
    paused: bool,
    acks_since_progress: u64,
    /// Which chunks the outstanding count actually counts.
    ledger: AckLedger,
}

impl AckWindow {
    fn new() -> Self {
        AckWindow {
            outstanding: 0,
            paused: false,
            acks_since_progress: 0,
            ledger: AckLedger::default(),
        }
    }
}

/// Wait until the window has room again. While paused, no ack expectation is
/// enforced: the loop only returns after a Resume (or fails on Cancel/Error/
/// idle). Every genuine ack also feeds byte-level progress at checkpoint
/// cadence; duplicate or unknown acks are ignored and counted (see
/// [`AckLedger`]), never treated as errors and never retiring an outstanding
/// slot.
#[allow(clippy::too_many_arguments)]
async fn drain_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    job_id: JobId,
    wanted: u32,
    idle: Duration,
    window: &mut AckWindow,
    manifest: &Manifest,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), EngineError> {
    while window.outstanding > wanted || window.paused {
        match recv_within(rx, limits, idle).await? {
            Frame {
                message:
                    Message::ChunkAck {
                        job_id: ack_job,
                        file_index,
                        chunk_index,
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("chunk ack names a different job"));
                }
                if window.ledger.apply(file_index, chunk_index) == AckOutcome::Confirmed {
                    window.outstanding = window.outstanding.saturating_sub(1);
                    window.acks_since_progress += 1;
                    if window.acks_since_progress >= CHECKPOINT_EVERY_ACKS {
                        window.acks_since_progress = 0;
                        if let Some(entry) = manifest.entries.get(file_index as usize) {
                            let cumulative = (chunk_index + 1)
                                .saturating_mul(u64::from(manifest.chunk_size.get()))
                                .min(entry.size);
                            progress(Progress::BytesAcked {
                                file_index,
                                cumulative_bytes: cumulative,
                            });
                        }
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        job_id: control_job,
                        control,
                    },
                ..
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                match control {
                    Control::Pause => {
                        if !window.paused {
                            window.paused = true;
                            progress(Progress::Paused);
                        }
                    }
                    Control::Resume => {
                        if window.paused {
                            window.paused = false;
                            progress(Progress::Resumed);
                        }
                    }
                    Control::Cancel => return Err(EngineError::Cancelled),
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    Ok(())
}

/// Sender-side progress relay from per-file stream tasks to the driver.
enum SendEvent {
    ChunkSent {
        file_index: u32,
        chunk_index: u64,
    },
    BytesAcked {
        file_index: u32,
        cumulative_bytes: u64,
    },
    Paused,
    Resumed,
}

/// Send every file on its own QUIC bidirectional stream, at most
/// `min(in_flight_window, 4)` at a time. Files fully covered by resume are
/// skipped: no stream is opened for them.
#[allow(clippy::too_many_arguments)]
async fn multistream_send_files(
    channel: &Arc<DirectChannel>,
    source: &Arc<SourceSet>,
    manifest: &Manifest,
    skip: &[u64],
    in_flight_window: u32,
    idle: Duration,
    job_id: JobId,
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), EngineError> {
    let limits = engine_limits();
    let window = in_flight_window.max(1);
    let concurrency = (window as usize).min(MAX_PARALLEL_STREAMS);
    let mut work: VecDeque<u32> = manifest
        .entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            !entry.is_dir
                && skip
                    .get(*index)
                    .is_some_and(|skip| *skip < entry.chunk_digests.len() as u64)
        })
        .map(|(index, _)| index as u32)
        .collect();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<SendEvent>(64);
    let mut set: tokio::task::JoinSet<Result<u32, EngineError>> = tokio::task::JoinSet::new();
    let mut events_alive = true;
    loop {
        while set.len() < concurrency {
            let Some(file_index) = work.pop_front() else {
                break;
            };
            let entry = &manifest.entries[file_index as usize];
            progress(Progress::FileStarted {
                file_index,
                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                bytes: entry.size,
            });
            set.spawn(multistream_file_send(
                Arc::clone(channel),
                Arc::clone(source),
                job_id,
                file_index,
                entry.size,
                skip[file_index as usize],
                manifest.chunk_size,
                window,
                idle,
                limits,
                events_tx.clone(),
            ));
        }
        if set.is_empty() {
            return Ok(());
        }
        tokio::select! {
            event = events_rx.recv(), if events_alive => {
                match event {
                    Some(SendEvent::ChunkSent { file_index, chunk_index }) => {
                        progress(Progress::ChunkSent { file_index, chunk_index })
                    }
                    Some(SendEvent::BytesAcked {
                        file_index,
                        cumulative_bytes,
                    }) => progress(Progress::BytesAcked {
                        file_index,
                        cumulative_bytes,
                    }),
                    Some(SendEvent::Paused) => progress(Progress::Paused),
                    Some(SendEvent::Resumed) => progress(Progress::Resumed),
                    None => events_alive = false,
                }
            }
            joined = set.join_next() => {
                match joined {
                    Some(Ok(Ok(file_index))) => {
                        if let (Some(entry), Some(checkpoint)) = (
                            manifest.entries.get(file_index as usize),
                            record.files.get_mut(file_index as usize),
                        ) {
                            checkpoint.verified_prefix_chunks = entry.chunk_digests.len() as u64;
                        }
                        record.updated_unix_ms = now_ms();
                        store.save(record).map_err(EngineError::Checkpoint)?;
                    }
                    Some(Ok(Err(error))) => return Err(error),
                    Some(Err(join)) => {
                        return Err(EngineError::Io(std::io::Error::other(format!(
                            "file stream task failed: {join}"
                        ))))
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

/// One file over one dedicated stream: sequential chunks with a per-stream
/// windowed acks, Pause/Resume honored mid-stream, then a half-close after
/// the last ack. Returns the file index for checkpoint bookkeeping.
#[allow(clippy::too_many_arguments)]
async fn multistream_file_send(
    channel: Arc<DirectChannel>,
    source: Arc<SourceSet>,
    job_id: JobId,
    file_index: u32,
    file_size: u64,
    skip: u64,
    chunk_size: NonZeroU32,
    window: u32,
    idle: Duration,
    limits: Limits,
    events: tokio::sync::mpsc::Sender<SendEvent>,
) -> Result<u32, EngineError> {
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();
    let plan = ak_core::chunk::ChunkPlan::new(file_size, chunk_size);
    let mut outstanding: u32 = 0;
    let mut paused = false;
    let mut acks_since_progress: u64 = 0;
    let mut ledger = AckLedger::default();
    for chunk_index in skip..plan.chunk_count() {
        if paused {
            drain_file_acks(
                &mut rx,
                &limits,
                job_id,
                0,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &mut ledger,
                &events,
            )
            .await?;
        }
        let range = plan.range(chunk_index).ok_or(EngineError::SourceChanged)?;
        let len = (range.end - range.start) as usize;
        let data = read_chunk_via(Arc::clone(&source), file_index, chunk_index, len).await?;
        if data.len() != len {
            return Err(EngineError::SourceChanged);
        }
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::Chunk {
                job_id,
                file_index,
                chunk_index,
                data,
            },
        })
        .await?;
        outstanding += 1;
        ledger.track(file_index, chunk_index);
        if events
            .send(SendEvent::ChunkSent {
                file_index,
                chunk_index,
            })
            .await
            .is_err()
        {
            return Err(EngineError::Cancelled);
        }
        if outstanding >= window {
            drain_file_acks(
                &mut rx,
                &limits,
                job_id,
                window - 1,
                idle,
                &mut outstanding,
                &mut paused,
                file_index,
                file_size,
                chunk_size,
                &mut acks_since_progress,
                &mut ledger,
                &events,
            )
            .await?;
        }
    }
    drain_file_acks(
        &mut rx,
        &limits,
        job_id,
        0,
        idle,
        &mut outstanding,
        &mut paused,
        file_index,
        file_size,
        chunk_size,
        &mut acks_since_progress,
        &mut ledger,
        &events,
    )
    .await?;
    tx.finish()?;
    Ok(file_index)
}

/// Per-file variant of [`drain_acks`] for multistream tasks: progress goes
/// through an event channel because the driver owns the progress callback.
/// Same ack-idempotency rule: only chunks in this file's [`AckLedger`]
/// retire an outstanding slot; duplicates and unknowns are counted, not
/// errors.
#[allow(clippy::too_many_arguments)]
async fn drain_file_acks(
    rx: &mut DirectRecvStream,
    limits: &Limits,
    job_id: JobId,
    wanted: u32,
    idle: Duration,
    outstanding: &mut u32,
    paused: &mut bool,
    file_index: u32,
    file_size: u64,
    chunk_size: NonZeroU32,
    acks_since_progress: &mut u64,
    ledger: &mut AckLedger,
    events: &tokio::sync::mpsc::Sender<SendEvent>,
) -> Result<(), EngineError> {
    while *outstanding > wanted || *paused {
        match recv_within(rx, limits, idle).await? {
            Frame {
                message:
                    Message::ChunkAck {
                        job_id: ack_job,
                        chunk_index,
                        ..
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("chunk ack names a different job"));
                }
                if ledger.apply(file_index, chunk_index) == AckOutcome::Confirmed {
                    *outstanding = outstanding.saturating_sub(1);
                    *acks_since_progress += 1;
                    if *acks_since_progress >= CHECKPOINT_EVERY_ACKS {
                        *acks_since_progress = 0;
                        let cumulative = (chunk_index + 1)
                            .saturating_mul(u64::from(chunk_size.get()))
                            .min(file_size);
                        if events
                            .send(SendEvent::BytesAcked {
                                file_index,
                                cumulative_bytes: cumulative,
                            })
                            .await
                            .is_err()
                        {
                            return Err(EngineError::Cancelled);
                        }
                    }
                }
            }
            Frame {
                message:
                    Message::Control {
                        job_id: control_job,
                        control,
                    },
                ..
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                match control {
                    Control::Pause => {
                        if !*paused {
                            *paused = true;
                            if events.send(SendEvent::Paused).await.is_err() {
                                return Err(EngineError::Cancelled);
                            }
                        }
                    }
                    Control::Resume => {
                        if *paused {
                            *paused = false;
                            if events.send(SendEvent::Resumed).await.is_err() {
                                return Err(EngineError::Cancelled);
                            }
                        }
                    }
                    Control::Cancel => return Err(EngineError::Cancelled),
                }
            }
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    }
    Ok(())
}

/// Run one send to completion. Progress callbacks are synchronous and must
/// be cheap.
/// Source/manifest/checkpoint preparation shared by [`send`] and
/// [`send_on_channel`]: opens the sources, derives the content job id and
/// the per-instance record id, and persists the initial sender record
/// before anything connects.
struct PreparedSend {
    sources: Arc<SourceSet>,
    manifest: Manifest,
    root_name: String,
    job_id: JobId,
    record: TaskRecord,
    store: FileCheckpointStore,
}

fn prepare_send(spec: &SendSpec) -> Result<PreparedSend, EngineError> {
    let limits = engine_limits();
    // Engine-side Offer field validation (review round 17, defense in depth
    // with ak-wire's decode budget): a chunk size above the engine limit
    // would be refused by the receiver (and possibly already by the wire
    // decoder); refuse it here, before any connection is made, with a clear
    // local invalid-input error.
    if spec.chunk_size.get() > limits.max_chunk_bytes {
        return Err(EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "chunk_size {} bytes exceeds the engine limit of {} bytes",
                spec.chunk_size.get(),
                limits.max_chunk_bytes
            ),
        )));
    }
    let sources = Arc::new(SourceSet::open(spec)?);
    let manifest = sources.manifest.clone();
    let root_name = sources.display_name().to_owned();
    // `job_id` is the content identity the wire speaks; the checkpoint
    // record is keyed by the per-instance record id instead (see
    // `record_id_for`), so this same source sent to another peer (or this
    // machine also receiving through the same checkpoint directory) keeps a
    // separate record instead of overwriting this one.
    let job_id = job_id_for(&root_name, &manifest);
    let record_id = record_id_for(&job_id, &spec.pinned, Direction::Send, &spec.source_root);

    let store = FileCheckpointStore::open(&spec.checkpoint_dir).map_err(EngineError::Checkpoint)?;
    let record = sender_record(spec, record_id, &manifest);
    store.save(&record).map_err(EngineError::Checkpoint)?;
    Ok(PreparedSend {
        sources,
        manifest,
        root_name,
        job_id,
        record,
        store,
    })
}

pub async fn send(
    spec: SendSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let started = Instant::now();
    let prepared = prepare_send(&spec)?;
    progress(Progress::Connecting);
    let endpoint = DirectEndpoint::bind(spec.bind, &spec.identity).await?;
    let channel = connect_bounded(&endpoint, spec.peer, &spec.pinned, spec.idle_timeout).await?;
    // Policy gate (protocol step 0): a refused path closes the connection
    // before any stream is opened or frame sent.
    gate_channel(&spec.policy, &channel)?;
    // Legacy path (wave 21 split): self-connected channel, admission absent
    // — no CAP_SCHEDULER bit, no TaskOpen. Wire-identical to the frozen v1
    // behavior; see [`send_on_channel`] for the scheduler-admission form.
    send_channel_body(channel, &spec, prepared, started, None, progress).await
}

/// Wave-21 on-channel send entry (scheduler.md §6), hardened wave 23: run
/// the whole send protocol on an already-connected [`DirectChannel`] the
/// runtime handed in. Differences from [`send`], and only these:
///
/// * **pinned-identity re-check (wave 23, review P1-4), the first thing
///   this entry does**: the channel's verified TLS peer fingerprint must
///   equal `spec.pinned` — the identity condition the legacy [`send`]
///   establishes itself at connect time. A mismatch returns
///   [`EngineError::PeerMismatch`] and closes the connection with
///   ``PEER_MISMATCH_CLOSE_CODE`` before any protocol byte moves (not even
///   the sources are scanned);
/// * no endpoint bind/connect here (the runtime owns the connection);
/// * the Hello advertises `CAP_SCHEDULER` and, when the peer does too
///   ([`ak_wire::scheduler_frames_allowed`]), the scheduler admission
///   handshake runs before the frozen Offer→FinalAck path:
///   `TaskOpen(task_id, group, group_registry, pairing, permit)` → wait
///   `Admission` → on grant send `AdmissionAck` and continue; on deny
///   return [`EngineError::AdmissionDenied`] with the closed wire reason
///   (`StalePermit` resubmits a fresh TaskOpen under a bounded budget);
/// * a peer without `CAP_SCHEDULER` fails immediately with
///   `PROTOCOL_UNSUPPORTED` — there is no silent fallback to unadmitted
///   transfer, because the caller explicitly asked for a scheduled task.
///
/// The policy gate still runs on the handed-in channel before any protocol
/// byte moves, and staging is keyed by the peer's admission decision (the
/// receiver stages under this `task_id`, not the sender).
pub async fn send_on_channel(
    channel: DirectChannel,
    spec: SendSpec,
    admission_ctx: AdmissionCtx,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    // Pinned-identity re-check (wave 23, review P1-4) — before anything
    // else, including the source scan: the runtime owns the connection,
    // so the engine re-derives the identity guarantee itself.
    if channel.peer_fingerprint() != &spec.pinned {
        let mismatch = EngineError::PeerMismatch {
            actual: channel.peer_fingerprint().to_hex(),
            expected: spec.pinned.to_hex(),
        };
        channel.close(
            PEER_MISMATCH_CLOSE_CODE,
            b"channel peer is not the pinned fingerprint",
        );
        return Err(mismatch);
    }
    let started = Instant::now();
    let prepared = prepare_send(&spec)?;
    // Policy gate: same step-0 refusal for a runtime-provided channel.
    gate_channel(&spec.policy, &channel)?;
    send_channel_body(
        channel,
        &spec,
        prepared,
        started,
        Some(&admission_ctx),
        progress,
    )
    .await
}

/// The protocol body shared by [`send`] and [`send_on_channel`]: everything
/// from the Connected progress event through Hello, (optional) admission,
/// Offer→Accept, manifest, resume, chunks (single-stream or multistream),
/// Finalize/FinalAck, and the report. `admission = None` keeps the frozen
/// no-scheduler wire behavior byte for byte.
#[allow(clippy::too_many_arguments)]
async fn send_channel_body(
    channel: DirectChannel,
    spec: &SendSpec,
    prepared: PreparedSend,
    started: Instant,
    admission: Option<&AdmissionCtx>,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let limits = engine_limits();
    let PreparedSend {
        sources,
        manifest,
        root_name,
        job_id,
        mut record,
        store,
    } = prepared;
    let channel = Arc::new(channel);
    progress(Progress::Connected {
        peer: channel.peer_fingerprint().to_hex(),
    });
    let stream = channel.open_bi().await?;
    let (mut tx, mut rx) = stream.split();

    let mut capabilities = CAP_RESUME | CAP_PAUSE_RESUME;
    if spec.multistream {
        capabilities |= CAP_MULTISTREAM;
    }
    if admission.is_some() {
        capabilities |= CAP_SCHEDULER;
    }
    tx.send_frame(&hello_frame(
        &spec.device_name,
        &spec.identity,
        capabilities,
    ))
    .await?;
    let peer_caps = expect_hello(&mut rx, &limits, spec.idle_timeout).await?;
    if let Some(ctx) = admission {
        // Capability-negotiated gate (scheduler.md §6): admission frames
        // only flow when BOTH Hellos carry CAP_SCHEDULER. A peer without
        // the bit cannot silently downgrade a scheduled task to the
        // unadmitted path — the sender asked for scheduling, so it fails
        // loudly with the stable protocol-unsupported code.
        if !scheduler_frames_allowed(capabilities, peer_caps) {
            return Err(EngineError::PeerError {
                code: ErrorCode::ProtocolUnsupported,
                message: "peer does not advertise CAP_SCHEDULER; \
                          scheduled task admission cannot run"
                    .into(),
            });
        }
        admission_handshake_sender(&mut tx, &mut rx, &limits, spec.idle_timeout, ctx).await?;
    }
    let multistream = spec.multistream && (peer_caps & CAP_MULTISTREAM) != 0;

    // Offer -> Accept/Reject (receiver consent is mandatory).
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Offer {
            job_id,
            root_name: root_name.chars().take(1024).collect(),
            file_count: manifest.file_count(),
            total_bytes: manifest.total_bytes(),
            chunk_size: spec.chunk_size.get(),
        },
    })
    .await?;
    progress(Progress::Offered {
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
    });
    match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
        Frame {
            message: Message::Accept { job_id: ack_job },
            ..
        } => {
            if ack_job != job_id {
                return Err(protocol_error("accept names a different job"));
            }
        }
        Frame {
            message:
                Message::Reject {
                    job_id: reject_job,
                    reason,
                },
            ..
        } => {
            if reject_job != job_id {
                return Err(protocol_error("reject names a different job"));
            }
            return Err(EngineError::Rejected(reason));
        }
        Frame {
            message: Message::Error { code, message },
            ..
        } => return Err(EngineError::PeerError { code, message }),
        other => return Err(unexpected(other.message)),
    }
    progress(Progress::Accepted {
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
    });

    // Manifest pages.
    let pages = assemble_pages(&manifest, &limits);
    for (index, page) in pages.iter().enumerate() {
        tx.send_frame(&Frame {
            version: ak_wire::VERSION,
            message: Message::ManifestPage {
                job_id,
                page_index: index as u32,
                last_page: index + 1 == pages.len(),
                entries: page.clone(),
            },
        })
        .await?;
        progress(Progress::ManifestPageSent { page: index as u32 });
    }

    // ResumeRequest: skip the receiver's verified prefixes. The answer is
    // sharded — frames accumulate (each wait under the idle budget) until
    // one carries more = false, and every frame must name this job. The
    // collector also cross-validates every entry against this manifest
    // (index in range and not a directory, no repeats, prefix within the
    // file's real chunk count, total budget = manifest file count).
    let mut resume = ResumeCollector::new(job_id, &manifest, &limits);
    loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::ResumeRequest {
                        job_id: frame_job_id,
                        files,
                        more,
                    },
                ..
            } => {
                if resume.push(frame_job_id, files, more)? {
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
    let resume = resume.entries;
    let mut skip = vec![0u64; manifest.entries.len()];
    let mut resumed_chunks = 0u64;
    for entry in &resume {
        if let Some(slot) = skip.get_mut(entry.file_index as usize) {
            *slot = entry.verified_prefix_chunks;
            resumed_chunks += entry.verified_prefix_chunks;
        }
    }
    if resumed_chunks > 0 {
        progress(Progress::Resuming {
            chunks: resumed_chunks,
        });
    }

    // Chunk transfer. Multistream mode rides one QUIC stream per file; the
    // single-stream path keeps the frozen v1 wire behavior.
    if multistream {
        multistream_send_files(
            &channel,
            &sources,
            &manifest,
            &skip,
            spec.in_flight_window,
            spec.idle_timeout,
            job_id,
            &mut record,
            &store,
            progress,
        )
        .await?;
    } else {
        let window_size = spec.in_flight_window.max(1);
        let mut acks = AckWindow::new();
        for (file_index, entry) in manifest.entries.iter().enumerate() {
            if entry.is_dir {
                continue;
            }
            progress(Progress::FileStarted {
                file_index: file_index as u32,
                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                bytes: entry.size,
            });
            let plan = ak_core::chunk::ChunkPlan::new(entry.size, spec.chunk_size);
            for chunk_index in skip[file_index]..plan.chunk_count() {
                if acks.paused {
                    drain_acks(
                        &mut rx,
                        &limits,
                        job_id,
                        window_size - 1,
                        spec.idle_timeout,
                        &mut acks,
                        &manifest,
                        progress,
                    )
                    .await?;
                }
                let range = plan.range(chunk_index).ok_or(EngineError::SourceChanged)?;
                let len = (range.end - range.start) as usize;
                let data =
                    read_chunk_via(Arc::clone(&sources), file_index as u32, chunk_index, len)
                        .await?;
                if data.len() != len {
                    return Err(EngineError::SourceChanged);
                }
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Chunk {
                        job_id,
                        file_index: file_index as u32,
                        chunk_index,
                        data,
                    },
                })
                .await?;
                acks.outstanding += 1;
                acks.ledger.track(file_index as u32, chunk_index);
                progress(Progress::ChunkSent {
                    file_index: file_index as u32,
                    chunk_index,
                });
                if acks.outstanding >= window_size {
                    drain_acks(
                        &mut rx,
                        &limits,
                        job_id,
                        window_size - 1,
                        spec.idle_timeout,
                        &mut acks,
                        &manifest,
                        progress,
                    )
                    .await?;
                }
            }
            // Keep checkpointed progress roughly current per file.
            record.files[file_index].verified_prefix_chunks =
                manifest.entries[file_index].chunk_digests.len() as u64;
            record.updated_unix_ms = now_ms();
            store.save(&record).map_err(EngineError::Checkpoint)?;
        }
        drain_acks(
            &mut rx,
            &limits,
            job_id,
            0,
            spec.idle_timeout,
            &mut acks,
            &manifest,
            progress,
        )
        .await?;
    }

    // Final integrity gate on the sources, then Finalize.
    sources.verify_stable()?;
    tx.send_frame(&Frame {
        version: ak_wire::VERSION,
        message: Message::Finalize { job_id },
    })
    .await?;
    // Pause/Resume is tolerated at the gate; only the verdict matters here.
    let verified = loop {
        match recv_within(&mut rx, &limits, spec.idle_timeout).await? {
            Frame {
                message:
                    Message::FinalAck {
                        job_id: ack_job,
                        verified,
                    },
                ..
            } => {
                if ack_job != job_id {
                    return Err(protocol_error("final ack names a different job"));
                }
                break verified;
            }
            Frame {
                message: Message::Control { .. },
                ..
            } => continue,
            Frame {
                message: Message::Error { code, message },
                ..
            } => return Err(EngineError::PeerError { code, message }),
            other => return Err(unexpected(other.message)),
        }
    };
    progress(Progress::FinalAck { verified });
    if !verified {
        record.state = "failed".into();
        record.updated_unix_ms = now_ms();
        store.save(&record).map_err(EngineError::Checkpoint)?;
        return Err(EngineError::VerificationFailed);
    }
    tx.finish()?;
    record.state = "completed".into();
    record.updated_unix_ms = now_ms();
    store.save(&record).map_err(EngineError::Checkpoint)?;
    progress(Progress::Done);

    Ok(Report {
        job_id,
        files: manifest.file_count(),
        bytes: manifest.total_bytes(),
        duration: started.elapsed(),
        resumed_chunks,
        peer_fingerprint: Some(spec.pinned.to_hex()),
        roots: sources.roots(),
    })
}

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
    let mut sinks: Vec<Option<ak_storage::LocalFileSink>> =
        Vec::with_capacity(job.manifest.entries.len());
    let mut resume: Vec<ResumeEntry> = Vec::new();
    let mut committed_skip = vec![false; job.manifest.entries.len()];
    for (index, entry) in job.manifest.entries.iter().enumerate() {
        let was_fully_verified = previous
            .as_ref()
            .and_then(|record| record.files.get(index))
            .is_some_and(|file| {
                !entry.is_dir && file.verified_prefix_chunks >= entry.chunk_digests.len() as u64
            });
        if was_fully_verified && committed_file_matches(target.root(), entry) {
            committed_skip[index] = true;
            resume.push(ResumeEntry {
                file_index: index as u32,
                verified_prefix_chunks: entry.chunk_digests.len() as u64,
            });
            if let Some(slot) = record.files.get_mut(index) {
                slot.verified_prefix_chunks = entry.chunk_digests.len() as u64;
            }
            // No sink: no part file, no chunk phase, no finish for this file.
            sinks.push(None);
            continue;
        }
        let sink = match staged_task {
            // Scheduled task: the part file lives in this task's own
            // staging tree (`<root>/.ak-staging/<task_hex>/...`), so a
            // second task aiming at the same destination can never
            // compete for one `.akpart` (wave 20 staging contract).
            Some(task) => target.prepare_file_staged(task, entry, spec.conflict, chunk_size)?,
            None => target.prepare_file_with_chunk_size(entry, spec.conflict, chunk_size)?,
        };
        let prefix = sink.verified_prefix_chunks();
        if !entry.is_dir {
            resume.push(ResumeEntry {
                file_index: index as u32,
                verified_prefix_chunks: prefix,
            });
        }
        sinks.push(Some(sink));
    }
    let resumed_chunks: u64 = resume.iter().map(|r| r.verified_prefix_chunks).sum();
    if resumed_chunks > 0 {
        progress(Progress::Resuming {
            chunks: resumed_chunks,
        });
    }

    // Everything below can fail mid-transfer; the outcome wrapper then
    // discards empty part files so a failed receive leaves no litter.
    // Parts holding verified chunks always survive for resume.
    let outcome = async {
        // ResumeRequest, sharded: each frame carries at most
        // `max_resume_entries` entries; the last (the only one for an empty
        // answer) carries more = false.
        for message in
            resume_request_frames(job_id, &resume, usize::from(limits.max_resume_entries))
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
            TailHold::Events(
                multistream_receive(
                    &channel,
                    rx,
                    job_id,
                    chunk_size,
                    &job.manifest,
                    &mut sinks,
                    &mut record,
                    store,
                    spec.idle_timeout,
                    progress,
                )
                .await?,
            )
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
                    progress,
                )
                .await?,
            )
        };

        // Verify and commit every file; only then report completion. Each
        // successful finish persists the per-file commit marker (verified
        // prefix == total chunks) immediately, so a crash after this point
        // lets a retry of the same record skip the file (see the prepare
        // loop) instead of colliding with its own committed output.
        progress(Progress::Verifying);
        for (index, sink_slot) in sinks.iter_mut().enumerate() {
            if let Some(mut sink) = sink_slot.take() {
                if job.manifest.entries[index].is_dir {
                    sink.finish().map_err(EngineError::Io)?;
                    continue;
                }
                sink.finish().map_err(EngineError::Io)?;
                record.files[index].verified_prefix_chunks =
                    job.manifest.entries[index].chunk_digests.len() as u64;
                record.updated_unix_ms = now_ms();
                store.save(&record).map_err(EngineError::Checkpoint)?;
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
            } else if committed_skip[index] {
                // Committed by the previous run of this record and skipped;
                // still report it as verified/committed for honest progress.
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
            }
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

/// How to hold the connection open until the sender observed the FinalAck.
enum TailHold {
    /// Single-stream mode: drain the control stream receive half.
    Stream(DirectRecvStream),
    /// Multistream mode: the control-reader task drains; its exit closes
    /// the event channel, which is what we await.
    Events(tokio::sync::mpsc::Receiver<StreamEvent>),
}

/// Frozen v1 chunk loop: one stream, chunks one way, windowed acks the
/// other way. Returns the receive half so the caller can hold the
/// connection for the FinalAck.
#[allow(clippy::too_many_arguments)]
async fn receive_single_stream(
    mut rx: DirectRecvStream,
    tx: &mut DirectSendStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<DirectRecvStream, EngineError> {
    let limits = engine_limits();
    let mut acks_since_checkpoint: u64 = 0;
    let mut current_file: u32 = u32::MAX;
    'transfer: loop {
        let frame = match recv_within(&mut rx, &limits, idle).await {
            Ok(frame) => frame,
            Err(EngineError::Stream(ak_net::StreamError::Connection(_))) => {
                return Err(EngineError::PeerClosed)
            }
            Err(e) => return Err(e),
        };
        match frame.message {
            Message::Chunk {
                job_id: chunk_job,
                file_index,
                chunk_index,
                data,
            } => {
                if chunk_job != job_id {
                    return Err(protocol_error("chunk names a different job"));
                }
                let sink = sinks
                    .get_mut(file_index as usize)
                    .and_then(Option::as_mut)
                    .ok_or(EngineError::PeerClosed)?;
                sink.write_chunk(chunk_index, &data)
                    .map_err(EngineError::Io)?;
                if let Some(slot) = record.files.get_mut(file_index as usize) {
                    slot.verified_prefix_chunks = sink.verified_prefix_chunks();
                }
                // Durability contract (see module docs): this ack confirms a
                // verified digest written to the part file's page cache —
                // not a durable write. Durability lands at the checkpoint
                // cadence below and at finish()'s sync_all + rename.
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck {
                        job_id,
                        file_index,
                        chunk_index,
                    },
                })
                .await?;
                if file_index != current_file {
                    current_file = file_index;
                    if let Some(entry) = manifest.entries.get(file_index as usize) {
                        progress(Progress::FileStarted {
                            file_index,
                            path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                            bytes: entry.size,
                        });
                    }
                }
                progress(Progress::ChunkReceived {
                    file_index,
                    chunk_index,
                });
                acks_since_checkpoint += 1;
                if acks_since_checkpoint >= CHECKPOINT_EVERY_ACKS {
                    acks_since_checkpoint = 0;
                    record.updated_unix_ms = now_ms();
                    store.save(record).map_err(EngineError::Checkpoint)?;
                    if let Some(entry) = manifest.entries.get(file_index as usize) {
                        let cumulative = sink
                            .verified_prefix_chunks()
                            .saturating_mul(u64::from(chunk_size.get()))
                            .min(entry.size);
                        progress(Progress::BytesAcked {
                            file_index,
                            cumulative_bytes: cumulative,
                        });
                    }
                }
            }
            Message::Finalize {
                job_id: finalize_job,
            } => {
                if finalize_job != job_id {
                    return Err(protocol_error("finalize names a different job"));
                }
                break 'transfer;
            }
            Message::Control {
                job_id: control_job,
                control: Control::Cancel,
            } => {
                if control_job != job_id {
                    return Err(protocol_error("control frame names a different job"));
                }
                return Err(EngineError::Cancelled);
            }
            Message::Error { code, message } => {
                return Err(EngineError::PeerError { code, message })
            }
            other => return Err(unexpected(other)),
        }
    }
    Ok(rx)
}

/// Event from a receiver-side stream task back to the driver loop.
enum StreamEvent {
    Chunk {
        file_index: u32,
        chunk_index: u64,
    },
    /// The file's stream FINed after the last ack; the sink travels back for
    /// the shared verify/commit phase.
    FileDone {
        file_index: u32,
        sink: ak_storage::LocalFileSink,
    },
    /// A stream task or the control stream failed.
    Failed(EngineError),
    /// Finalize observed on the control stream.
    Finalize,
    Cancelled,
}

/// Receive a multistream job: accept per-file streams until Finalize, drive
/// each stream in its own task (write/verify/ack), keep the checkpoint
/// current at the same cadence as the single-stream path. Returns the event
/// channel tail for the FinalAck hold (the control task keeps draining).
#[allow(clippy::too_many_arguments)]
async fn multistream_receive(
    channel: &DirectChannel,
    control_rx: DirectRecvStream,
    job_id: JobId,
    chunk_size: NonZeroU32,
    manifest: &Manifest,
    sinks: &mut [Option<ak_storage::LocalFileSink>],
    record: &mut TaskRecord,
    store: &FileCheckpointStore,
    idle: Duration,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>, EngineError> {
    let limits = engine_limits();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);

    // P1-2 fix (review round 17): one task-wide activity budget for the
    // control stream. The control stream legitimately carries no frame
    // between the ResumeRequest and the Finalize — the sender only writes
    // Finalize after every file stream finished — so an independent
    // per-read timeout on it killed healthy long multistream transfers the
    // moment the data phase outlasted `idle`. Now data-stream events
    // (chunks acked, files finished, streams accepted) refresh the budget
    // through the activity channel below, and only whole-task silence for
    // `idle` aborts.
    //
    // Structure: an inner reader task owns the receive half and forwards
    // decoded frames — it is never cancelled, because quinn reads are not
    // cancel-safe and dropping a mid-read future would lose frame bytes.
    // The monitor task below runs cancel-safe selects only (channel recv,
    // sleep) so the deadline can move without touching the reader.
    let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel::<()>(1);
    let (frames_tx, mut frames_rx) =
        tokio::sync::mpsc::channel::<Result<Frame, ak_net::StreamError>>(8);
    tokio::spawn(async move {
        let mut rx = control_rx;
        loop {
            match rx.recv_frame(&limits).await {
                Ok(frame) => {
                    if frames_tx.send(Ok(frame)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = frames_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    // Control monitor: forwards Finalize/Cancel/Error into the driver's
    // event channel, then keeps draining silently so the connection is
    // held until the sender half-closes after reading the FinalAck. All
    // frames must name this job (cross-frame validation).
    let control_tx = events_tx.clone();
    tokio::spawn(async move {
        let mut finalized = false;
        let mut activity_open = true;
        let mut deadline = tokio::time::Instant::now() + idle;
        loop {
            tokio::select! {
                frame = frames_rx.recv() => {
                    let Some(frame) = frame else {
                        // Reader ended: the connection closed underneath it.
                        break;
                    };
                    deadline = tokio::time::Instant::now() + idle;
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(ak_net::StreamError::Connection(_)) => break,
                        Err(e) => {
                            let _ = control_tx.send(StreamEvent::Failed(e.into())).await;
                            break;
                        }
                    };
                    match frame.message {
                        Message::Finalize { job_id: finalize_job } if !finalized => {
                            if finalize_job != job_id {
                                let _ = control_tx
                                    .send(StreamEvent::Failed(protocol_error(
                                        "finalize names a different job",
                                    )))
                                    .await;
                                break;
                            }
                            finalized = true;
                            if control_tx.send(StreamEvent::Finalize).await.is_err() {
                                break;
                            }
                        }
                        Message::Control { job_id: control_job, control: Control::Cancel } => {
                            if control_job != job_id {
                                let _ = control_tx
                                    .send(StreamEvent::Failed(protocol_error(
                                        "control frame names a different job",
                                    )))
                                    .await;
                                break;
                            }
                            let _ = control_tx.send(StreamEvent::Cancelled).await;
                            break;
                        }
                        Message::Error { code, message } => {
                            let _ = control_tx
                                .send(StreamEvent::Failed(EngineError::PeerError {
                                    code,
                                    message,
                                }))
                                .await;
                            break;
                        }
                        other => {
                            let _ = control_tx
                                .send(StreamEvent::Failed(unexpected(other)))
                                .await;
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // No control frame AND no data activity for the whole
                    // budget: the task is truly idle, abort it. (Data
                    // activity alone keeps the budget fresh even while the
                    // control stream itself stays silent.)
                    let _ = control_tx
                        .send(StreamEvent::Failed(EngineError::IdleTimeout(idle)))
                        .await;
                    break;
                }
                signal = activity_rx.recv(), if activity_open => {
                    match signal {
                        // Coalesced activity pulse: refresh the budget. A
                        // bounded(1) channel keeps this cheap no matter how
                        // many chunks flow; try_send on the producer side
                        // drops extra pulses while one is pending.
                        Some(()) => deadline = tokio::time::Instant::now() + idle,
                        // Every producer (the driver loop) is gone — e.g.
                        // the transfer finished and the FinalAck hold is
                        // running. Stop arming this branch; the last
                        // deadline still bounds the drain.
                        None => activity_open = false,
                    }
                }
            }
        }
    });

    let mut finalized = false;
    let mut open_streams = 0usize;
    let mut acks_since_checkpoint: u64 = 0;
    let mut last_activity = Instant::now();
    loop {
        if finalized && open_streams == 0 {
            break;
        }
        // Accept waits share the idle budget: any chunk or ack event on an
        // already-open stream refreshes it.
        let budget = idle.saturating_sub(last_activity.elapsed());
        if budget.is_zero() {
            return Err(EngineError::IdleTimeout(idle));
        }
        tokio::select! {
            event = events_rx.recv() => {
                let Some(event) = event else {
                    // Every producer ended. Without Finalize nothing more can
                    // arrive; with Finalize all streams already finished.
                    if finalized {
                        break;
                    }
                    return Err(EngineError::PeerClosed);
                };
                last_activity = Instant::now();
                // Data activity refreshes the control monitor's budget too
                // (see the activity channel above): one coalesced pulse per
                // processed event, dropped while one is already pending.
                let _ = activity_tx.try_send(());
                match event {
                    StreamEvent::Chunk { file_index, chunk_index } => {
                        let Some(slot) = record.files.get_mut(file_index as usize) else {
                            return Err(EngineError::PeerClosed);
                        };
                        slot.verified_prefix_chunks =
                            slot.verified_prefix_chunks.max(chunk_index + 1);
                        progress(Progress::ChunkReceived { file_index, chunk_index });
                        acks_since_checkpoint += 1;
                        if acks_since_checkpoint >= CHECKPOINT_EVERY_ACKS {
                            acks_since_checkpoint = 0;
                            record.updated_unix_ms = now_ms();
                            let prefix = slot.verified_prefix_chunks;
                            store.save(record).map_err(EngineError::Checkpoint)?;
                            if let Some(entry) = manifest.entries.get(file_index as usize) {
                                let cumulative = prefix
                                    .saturating_mul(u64::from(chunk_size.get()))
                                    .min(entry.size);
                                progress(Progress::BytesAcked {
                                    file_index,
                                    cumulative_bytes: cumulative,
                                });
                            }
                        }
                    }
                    StreamEvent::FileDone { file_index, sink } => {
                        open_streams = open_streams.saturating_sub(1);
                        if let Some(slot) = sinks.get_mut(file_index as usize) {
                            *slot = Some(sink);
                        }
                        if let Some(entry) = manifest.entries.get(file_index as usize) {
                            if let Some(checkpoint) = record.files.get_mut(file_index as usize) {
                                checkpoint.verified_prefix_chunks =
                                    entry.chunk_digests.len() as u64;
                            }
                        }
                    }
                    StreamEvent::Failed(error) => return Err(error),
                    StreamEvent::Finalize => finalized = true,
                    StreamEvent::Cancelled => return Err(EngineError::Cancelled),
                }
            }
            accepted = tokio::time::timeout(budget, channel.accept_bi()), if !finalized => {
                match accepted {
                    Err(_elapsed) => return Err(EngineError::IdleTimeout(idle)),
                    Ok(Err(e)) => return Err(EngineError::Stream(e)),
                    Ok(Ok(mut stream)) => {
                        last_activity = Instant::now();
                        let _ = activity_tx.try_send(());
                        // The first frame on a new stream names its file and
                        // must belong to this job.
                        let first = match tokio::time::timeout(idle, stream.recv_frame(&limits)).await {
                            Ok(Ok(frame)) => frame,
                            Ok(Err(e)) => return Err(EngineError::Stream(e)),
                            Err(_) => return Err(EngineError::IdleTimeout(idle)),
                        };
                        let Message::Chunk {
                            job_id: chunk_job,
                            file_index,
                            ..
                        } = first.message
                        else {
                            return Err(unexpected(first.message));
                        };
                        if chunk_job != job_id {
                            return Err(protocol_error("chunk names a different job"));
                        }
                        let sink = sinks
                            .get_mut(file_index as usize)
                            .and_then(Option::take)
                            .ok_or(EngineError::PeerClosed)?;
                        if let Some(entry) = manifest.entries.get(file_index as usize) {
                            progress(Progress::FileStarted {
                                file_index,
                                path: entry.rel_path.to_string_lossy().replace('\\', "/"),
                                bytes: entry.size,
                            });
                        }
                        open_streams += 1;
                        receive_stream_task(
                            stream,
                            sink,
                            file_index,
                            first,
                            job_id,
                            idle,
                            events_tx.clone(),
                        );
                    }
                }
            }
        }
    }
    Ok(events_rx)
}

/// Drive one accepted per-file stream: write/verify/ack every chunk until
/// the sender half-closes, then hand the sink back to the driver.
fn receive_stream_task(
    mut stream: DirectStream,
    mut sink: ak_storage::LocalFileSink,
    file_index: u32,
    first: Frame,
    job_id: JobId,
    idle: Duration,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
) {
    tokio::spawn(async move {
        let limits = engine_limits();
        let mut current = Some(first);
        let result: Result<(), EngineError> = loop {
            let frame = match current.take() {
                Some(frame) => frame,
                None => match tokio::time::timeout(idle, stream.recv_frame(&limits)).await {
                    Ok(Ok(frame)) => frame,
                    // The sender half-closes only after every ack, so a clean
                    // early end means the file is complete.
                    Ok(Err(ak_net::StreamError::Connection(message)))
                        if message.starts_with("stream finished early") =>
                    {
                        break Ok(());
                    }
                    Ok(Err(ak_net::StreamError::Connection(_))) => {
                        break Err(EngineError::PeerClosed)
                    }
                    Ok(Err(e)) => break Err(EngineError::Stream(e)),
                    Err(_) => break Err(EngineError::IdleTimeout(idle)),
                },
            };
            match frame.message {
                Message::Chunk {
                    job_id: chunk_job,
                    file_index: frame_file_index,
                    chunk_index,
                    data,
                } => {
                    // Cross-frame consistency: every frame on this stream
                    // must belong to the job and to the file this stream
                    // announced in its first frame (otherwise a peer could
                    // smuggle chunks of one file into another file's ack
                    // stream).
                    if chunk_job != job_id {
                        break Err(protocol_error("chunk names a different job"));
                    }
                    if frame_file_index != file_index {
                        break Err(protocol_error("chunk arrives on a different file's stream"));
                    }
                    if let Err(e) = sink.write_chunk(chunk_index, &data) {
                        break Err(EngineError::Io(e));
                    }
                    // Durability contract (see module docs): the ack
                    // confirms a verified digest in the part file's page
                    // cache, not a durable write.
                    if let Err(e) = stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::ChunkAck {
                                job_id,
                                file_index,
                                chunk_index,
                            },
                        })
                        .await
                    {
                        break Err(EngineError::Stream(e));
                    }
                    if events
                        .send(StreamEvent::Chunk {
                            file_index,
                            chunk_index,
                        })
                        .await
                        .is_err()
                    {
                        break Err(EngineError::Cancelled);
                    }
                }
                Message::Control {
                    job_id: control_job,
                    control: Control::Cancel,
                } => {
                    if control_job != job_id {
                        break Err(protocol_error("control frame names a different job"));
                    }
                    break Err(EngineError::Cancelled);
                }
                Message::Error { code, message } => {
                    break Err(EngineError::PeerError { code, message })
                }
                other => break Err(unexpected(other)),
            }
        };
        // Best-effort half-close of the ack side; the peer already read all
        // acks before it half-closed ours.
        let _ = stream.finish_send();
        let _ = events
            .send(match result {
                Ok(()) => StreamEvent::FileDone { file_index, sink },
                Err(error) => StreamEvent::Failed(error),
            })
            .await;
    });
}

/// Run one receive to completion (see [`receive_with_addr`]).
pub async fn receive(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let (_tx, _rx) = tokio::sync::oneshot::channel::<SocketAddr>();
    receive_with_addr(spec, progress, _tx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::DeviceKeyPair;
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn count_entries(dir: &Path) -> (u64, u64) {
        let mut files = 0u64;
        let mut bytes = 0u64;
        fn walk(dir: &Path, files: &mut u64, bytes: &mut u64) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    walk(&entry.path(), files, bytes);
                } else if !entry
                    .path()
                    .to_string_lossy()
                    .ends_with(ak_storage::PART_SUFFIX)
                {
                    *files += 1;
                    *bytes += meta.len();
                }
            }
        }
        walk(dir, &mut files, &mut bytes);
        (files, bytes)
    }

    fn specs(src: &Path, dst: &Path, chunk_kb: u32) -> (SendSpec, ReceiveSpec, Fingerprint) {
        let sender_key = DeviceKeyPair::generate();
        let receiver_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let receiver_fp = receiver_key.fingerprint();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let mut send_spec = SendSpec::new(
            src.to_path_buf(),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            receiver_fp,
            sender_bundle,
        );
        send_spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
        send_spec.checkpoint_dir = checkpoint.clone();
        let mut recv = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.chunk_size = send_spec.chunk_size;
        recv.checkpoint_dir = checkpoint;
        (send_spec, recv, receiver_fp)
    }

    fn noop(_progress: Progress) {}

    /// Loopback transfer under the default policy: the step-0 gate passes
    /// LocalDirect on both sides and the frozen flow runs unchanged — this
    /// is the default-policy gate integration.
    #[tokio::test]
    async fn loopback_directory_transfer_is_byte_identical() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"hello ak");
        write(
            &src.path().join("nested/deep/b.bin"),
            &(0..50_000u32).map(|i| i as u8).collect::<Vec<_>>(),
        );
        fs::create_dir_all(src.path().join("empty/dir")).unwrap();

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
        send_spec.device_name = "sender-test".into();
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let sender_spec = send_spec.clone();
        let sender = tokio::spawn(async move { send(sender_spec, &noop).await });
        let recv_outcome = receiver.await.unwrap();
        let send_outcome = sender.await.unwrap();
        if let Err(error) = &recv_outcome {
            eprintln!("receiver error: {error:?}");
        }
        let report = send_outcome.expect("send should complete");
        let recv_report = recv_outcome.expect("receive should complete");

        assert_eq!(report.files, 6); // 2 files + 4 directory entries (nested, deep, empty, empty/dir)
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
        assert_eq!(
            fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
            (0..50_000u32).map(|i| i as u8).collect::<Vec<_>>()
        );
        assert!(dst.path().join("empty/dir").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (2, 8 + 50_000));
        assert!(report.duration.as_nanos() > 0);
    }

    #[tokio::test]
    async fn resume_skips_verified_prefix_without_resending() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        // 8 KiB file with 1 KiB chunks gives an 8-chunk prefix to resume from.
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("resume.bin"), &payload);
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
        let checkpoint = send_spec.checkpoint_dir.clone();

        // Simulate an interrupted earlier run: 3 verified chunks on disk.
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let source_probe = LocalSource::open(&send_spec.source_root, send_spec.chunk_size).unwrap();
        let entry = &source_probe.manifest().entries[0];
        let mut sink = target
            .prepare_file_with_chunk_size(entry, ConflictPolicy::Fail, send_spec.chunk_size)
            .unwrap();
        for chunk in 0..3u64 {
            let mut buf = vec![0u8; 1024];
            let len = source_probe.read_chunk(0, chunk, &mut buf).unwrap();
            sink.write_chunk(chunk, &buf[..len]).unwrap();
        }
        drop(sink);

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("send should complete");
        let recv_report = receiver.await.unwrap().expect("receive should complete");

        assert_eq!(
            report.resumed_chunks, 3,
            "sender should skip 3 verified chunks"
        );
        assert_eq!(recv_report.resumed_chunks, 3);
        assert_eq!(fs::read(dst.path().join("resume.bin")).unwrap(), payload);
        let _ = FileCheckpointStore::open(&checkpoint).map(|s| s.remove(&report.job_id));
    }

    /// T04/T10 analog: the receiver dies mid-transfer. The sender must fail
    /// (never fall back to anything), and the checkpoint plus part file must
    /// survive so a second run resumes instead of restarting from zero.
    #[tokio::test]
    async fn mid_transfer_disconnect_keeps_checkpoint_and_resumes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
        write(&src.path().join("mid.bin"), &payload);
        let (send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 16 chunks of 1 KiB
        let checkpoint_dir = send_spec.checkpoint_dir.clone();
        let part_path = dst
            .path()
            .join(format!("mid.bin{}", ak_storage::PART_SUFFIX));

        let (chunks_tx, mut chunks_rx) = tokio::sync::watch::channel(0u64);
        let progress = std::sync::Arc::new(move |event: Progress| {
            if let Progress::ChunkReceived { .. } = event {
                chunks_tx.send_modify(|count| *count += 1);
            }
        });
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = {
            let progress = std::sync::Arc::clone(&progress);
            tokio::spawn(async move { receive_with_addr(recv, &*progress, addr_tx).await })
        };
        let mut first_spec = send_spec.clone();
        first_spec.peer = addr_rx.await.unwrap();
        let sender = tokio::spawn(async move { send(first_spec, &noop).await });

        // Kill the receiver once a few chunks are durable.
        while *chunks_rx.borrow() < 4 {
            chunks_rx.changed().await.unwrap();
        }
        receiver.abort();
        let send_result = sender.await.unwrap();
        assert!(
            matches!(
                send_result,
                Err(EngineError::Stream(_) | EngineError::PeerClosed | EngineError::Connect(_))
            ),
            "sender must fail without any fallback, got {send_result:?}"
        );
        assert!(part_path.exists(), "part file must survive the disconnect");
        let store = FileCheckpointStore::open(&checkpoint_dir).unwrap();
        assert!(
            !store.list().unwrap().is_empty(),
            "checkpoint must survive the disconnect"
        );

        // Second run: same deterministic job id resumes and completes.
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("resumed send should complete");
        receiver
            .await
            .unwrap()
            .expect("resumed receive should complete");

        assert!(
            report.resumed_chunks >= 1,
            "second run should resume, got {}",
            report.resumed_chunks
        );
        assert_eq!(fs::read(dst.path().join("mid.bin")).unwrap(), payload);
        assert!(!part_path.exists(), "committed file leaves no part behind");
    }

    /// A v4 endpoint dialing a v6 peer must fail fast instead of hanging in
    /// QUIC's retry loop (defect found by the IPv6 scenario tests).
    #[tokio::test]
    async fn connect_family_mismatch_fails_fast_instead_of_hanging() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("f.txt"), b"x");
        let (mut send_spec, _, _) = specs(src.path(), dst.path(), 16);
        send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0)); // IPv4 endpoint
        send_spec.peer = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 9)); // IPv6 peer, nothing listening
        let started = Instant::now();
        let err = send(send_spec, &noop)
            .await
            .expect_err("family mismatch must fail");
        assert!(
            matches!(
                &err,
                EngineError::Connect(ak_net::ConnectError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
            ),
            "got: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must fail fast, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wrong_pin_fails_closed_before_any_transfer() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("secret.txt"), b"must not leak");
        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 16);
        // Pin a fingerprint that is NOT the receiver's.
        send_spec.pinned = DeviceKeyPair::generate().fingerprint();
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let err = send(send_spec, &noop)
            .await
            .expect_err("wrong pin must fail");
        assert!(matches!(err, EngineError::Connect(_)), "got: {err:?}");
        // Nothing may be written even though the offer was never reached.
        assert_eq!(count_entries(dst.path()), (0, 0));
        let _ = receiver.await;
    }

    #[tokio::test]
    async fn receiver_authorize_false_rejects_the_sender() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("x.txt"), b"x");
        let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 16);
        recv.authorize = Box::new(|_fp| false);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();
        send_spec.peer = addr;
        let err = send(send_spec, &noop)
            .await
            .expect_err("unauthorized sender must fail");
        // The TLS handshake itself succeeds (client cert is validly
        // self-signed); rejection surfaces on first use as a lost stream.
        assert!(
            matches!(err, EngineError::Connect(_) | EngineError::Stream(_)),
            "got: {err:?}"
        );
        let recv_err = receiver.await.unwrap().expect_err("receiver must reject");
        assert!(matches!(
            recv_err,
            EngineError::Connect(ak_net::ConnectError::Unauthorized)
        ));
        assert_eq!(count_entries(dst.path()), (0, 0));
    }

    #[tokio::test]
    async fn job_id_is_stable_for_source_and_sensitive_to_content() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"content-a");
        let manifest = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
            .unwrap()
            .manifest()
            .clone();
        let id_a = job_id_for("f.bin", &manifest);
        let id_b = job_id_for("f.bin", &manifest);
        assert_eq!(id_a, id_b);

        write(&src.path().join("f.bin"), b"content-b");
        let manifest2 = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap())
            .unwrap()
            .manifest()
            .clone();
        assert_ne!(id_a, job_id_for("f.bin", &manifest2));
    }

    // ------------------------------------------------------------------
    // Storage-full precheck (pure decision)
    // ------------------------------------------------------------------

    /// Minimal FileEntry for the name-gate decision tests (only rel_path
    /// matters to [`windows_unsavable_entry`]).
    fn name_entry(rel: &str) -> FileEntry {
        FileEntry {
            rel_path: std::path::PathBuf::from(rel),
            is_dir: false,
            size: 1,
            mtime_unix_ms: 0,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: [0u8; 32],
            chunk_digests: Vec::new(),
        }
    }

    #[test]
    fn windows_unsavable_entry_flags_each_rule_at_any_depth() {
        for rel in [
            "what?now.bin",
            "a<b.txt",
            "dir>n/x.txt",
            "CONIN$.log",
            "COM¹.bin",
        ] {
            assert!(
                windows_unsavable_entry(&[name_entry(rel)]).is_some(),
                "{rel} must be flagged"
            );
        }
        // The offending path and a readable reason ride the message.
        let flagged = windows_unsavable_entry(&[name_entry("docs/what?now.bin")]).unwrap();
        assert!(flagged.contains("docs/what?now.bin"), "{flagged}");
        assert!(flagged.contains('?'), "{flagged}");
    }

    #[test]
    fn windows_unsavable_entry_passes_clean_manifests_and_finds_offenders() {
        assert!(windows_unsavable_entry(&[]).is_none());
        // Clean names, including cases near the reserved-set boundaries.
        assert!(windows_unsavable_entry(&[
            name_entry("报告-最终.txt"),
            name_entry("myconin$/note.txt"), // substring, not a reserved stem
            name_entry("COM10/serial.log"),  // past the COM1-9 range
        ])
        .is_none());
        // Reserved stems are caught at any depth, with any extension.
        assert!(windows_unsavable_entry(&[name_entry("sub/CON.txt.ok")]).is_some());
    }

    #[test]
    fn check_headroom_passes_unknown_available() {
        assert!(check_headroom(None, 0).is_ok());
        assert!(check_headroom(None, u64::MAX).is_ok());
    }

    #[test]
    fn check_headroom_passes_exact_fit() {
        assert!(check_headroom(Some(0), 0).is_ok());
        assert!(check_headroom(Some(1000), 1000).is_ok());
    }

    #[test]
    fn check_headroom_rejects_shortage_with_the_real_numbers() {
        assert!(matches!(
            check_headroom(Some(999), 1000),
            Err(EngineError::StorageFull {
                available: 999,
                needed: 1000
            })
        ));
        assert!(matches!(
            check_headroom(Some(0), 1),
            Err(EngineError::StorageFull {
                available: 0,
                needed: 1
            })
        ));
    }

    // The receive-path integration (a real volume reporting less free space
    // than the job size) cannot be constructed on a real local disk in this
    // environment: Windows temp directories have no quota mechanism and the
    // free-space probe reads the live volume. Only the pure decision above
    // is covered by automated tests; the Error-frame and
    // "failed_storage_full" checkpoint wiring is untested here by design
    // (no faked numbers, no faked filesystems). See the round-3 report.

    // ------------------------------------------------------------------
    // Policy gate (pre-transport route authorization)
    // ------------------------------------------------------------------

    #[test]
    fn authorize_path_local_direct_passes_even_with_local_only_policy() {
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        assert!(authorize_path(&DirectPolicy::default(), PathKind::LocalDirect).is_ok());
        assert!(authorize_path(&local_only, PathKind::LocalDirect).is_ok());
    }

    #[test]
    fn authorize_path_internet_direct_follows_allow_internet() {
        assert!(
            authorize_path(&DirectPolicy::default(), PathKind::InternetDirect).is_ok(),
            "the default policy allows internet"
        );
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        assert!(matches!(
            authorize_path(&local_only, PathKind::InternetDirect),
            Err(EngineError::RouteForbidden(PathKind::InternetDirect))
        ));
    }

    #[test]
    fn authorize_path_rejects_relay_and_unknown_even_when_permissive() {
        for path in [PathKind::Relay, PathKind::Unknown] {
            assert!(
                matches!(
                    authorize_path(&DirectPolicy::default(), path),
                    Err(EngineError::RouteForbidden(refused)) if refused == path
                ),
                "Relay/Unknown must always be refused, got a pass for {path:?}"
            );
        }
    }

    /// The documented API contract for integrators (e.g. CLI flags):
    /// untouched specs carry `allow_internet = true, allow_cellular = false`.
    #[test]
    fn specs_default_to_the_standard_direct_policy() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let (send_spec, recv_spec, _) = specs(src.path(), dst.path(), 16);
        for policy in [send_spec.policy, recv_spec.policy] {
            assert!(policy.allow_internet, "default must allow internet");
            assert!(!policy.allow_cellular, "default must refuse cellular");
        }
    }

    /// `allow_internet = false` must not affect a loopback transfer: the
    /// loopback peer classifies `LocalDirect` (ak-net::classify treats
    /// loopback as always local), which no policy can lock out. Both sides
    /// carry the local-only policy here.
    ///
    /// The InternetDirect-refused branch of the wiring cannot be produced
    /// naturally on a single machine: every address a test can actually
    /// connect to here classifies LocalDirect, and `DirectChannel` exposes
    /// no public constructor to forge a path (by design). That branch is
    /// covered by the pure unit branches above instead.
    #[tokio::test]
    async fn local_only_policy_still_completes_loopback_transfer() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("local-only.bin"), &payload);
        let (mut send_spec, mut recv, _) = specs(src.path(), dst.path(), 4);
        let local_only = DirectPolicy {
            allow_internet: false,
            allow_cellular: false,
        };
        send_spec.policy = local_only;
        recv.policy = local_only;

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("local-only policy must not affect a LocalDirect loopback");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("local-only policy must not affect a LocalDirect loopback");
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(
            fs::read(dst.path().join("local-only.bin")).unwrap(),
            payload
        );
    }

    // ------------------------------------------------------------------
    // Multi-root send
    // ------------------------------------------------------------------

    /// Three-root fixture: two directory trees (nesting plus empty dirs) and
    /// one standalone file. Returns (tree_a, tree_b, loose, deep_payload).
    fn three_root_fixture(
        base: &Path,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<u8>,
    ) {
        let tree_a = base.join("photos");
        write(&tree_a.join("a.txt"), b"hello ak");
        let deep: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
        write(&tree_a.join("nested/deep/b.bin"), &deep);
        fs::create_dir_all(tree_a.join("empty/dir")).unwrap();
        let tree_b = base.join("docs");
        write(&tree_b.join("notes.md"), b"notes");
        fs::create_dir_all(tree_b.join("solo")).unwrap();
        let loose = base.join("readme.txt");
        write(&loose, b"loose file");
        (tree_a, tree_b, loose, deep)
    }

    #[tokio::test]
    async fn multi_root_transfer_merges_three_roots_byte_identical() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();

        let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 16);
        send_spec.add_root(tree_b.clone());
        send_spec.add_root(loose.clone());

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("multi-root send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multi-root receive completes");

        // Primary root first, extras in add order; the receiver reports none.
        assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
        assert_eq!(recv_report.roots, Vec::<String>::new());
        // 4 files + 5 directory entries (photos/{empty,empty/dir,nested,
        // nested/deep} + docs/solo); a single-file root adds no directory.
        assert_eq!(report.files, 9);
        assert_eq!(report.bytes, 8 + 30_000 + 5 + 10);
        assert_eq!(report.bytes, recv_report.bytes);

        assert_eq!(
            fs::read(dst.path().join("photos/a.txt")).unwrap(),
            b"hello ak"
        );
        assert_eq!(
            fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
            deep
        );
        assert!(dst.path().join("photos/empty/dir").is_dir());
        assert_eq!(
            fs::read(dst.path().join("docs/notes.md")).unwrap(),
            b"notes"
        );
        assert!(dst.path().join("docs/solo").is_dir());
        // A single-file root lands under a top-level directory of its own name.
        assert_eq!(
            fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
            b"loose file"
        );
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
    }

    #[tokio::test]
    async fn multi_root_multistream_transfer_is_byte_identical() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, deep) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();

        let (mut send_spec, recv, _) = specs(&tree_a, dst.path(), 1);
        send_spec.add_root(tree_b.clone());
        send_spec.add_root(loose.clone());
        send_spec.multistream = true;
        send_spec.in_flight_window = 4;

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop)
            .await
            .expect("multi-root multistream send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multi-root multistream receive completes");

        assert_eq!(report.roots, vec!["photos", "docs", "readme.txt"]);
        assert_eq!(report.files, 9);
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(
            fs::read(dst.path().join("photos/a.txt")).unwrap(),
            b"hello ak"
        );
        assert_eq!(
            fs::read(dst.path().join("photos/nested/deep/b.bin")).unwrap(),
            deep
        );
        assert!(dst.path().join("photos/empty/dir").is_dir());
        assert_eq!(
            fs::read(dst.path().join("docs/notes.md")).unwrap(),
            b"notes"
        );
        assert!(dst.path().join("docs/solo").is_dir());
        assert_eq!(
            fs::read(dst.path().join("readme.txt/readme.txt")).unwrap(),
            b"loose file"
        );
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (4, 8 + 30_000 + 5 + 10));
    }

    #[tokio::test]
    async fn multi_root_rejects_duplicate_root_names() {
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("data");
        let b = base.path().join("other").join("data");
        write(&a.join("one.txt"), b"one");
        write(&b.join("two.txt"), b"two");

        let dst = tempfile::tempdir().unwrap();
        let (mut send_spec, _recv, _) = specs(&a, dst.path(), 16);
        send_spec.add_root(b);
        // No receiver is spawned: the job must fail before any connect.
        let err = send(send_spec, &noop)
            .await
            .expect_err("duplicate root names must fail the job");
        assert!(
            matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
            "got: {err:?}"
        );
        assert_eq!(count_entries(dst.path()), (0, 0));
    }

    #[test]
    fn multi_root_manifest_is_deterministic_and_prefixed() {
        let base = tempfile::tempdir().unwrap();
        let (tree_a, tree_b, loose, _) = three_root_fixture(base.path());
        let dst = tempfile::tempdir().unwrap();
        let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
        spec.add_root(tree_b.clone());
        spec.add_root(loose.clone());

        let first = SourceSet::open(&spec).unwrap();
        let second = SourceSet::open(&spec).unwrap();
        assert!(first.multi);
        assert_eq!(first.display_name(), "multi");
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(
            job_id_for("multi", &first.manifest),
            job_id_for("multi", &second.manifest)
        );
        let rels: Vec<String> = first
            .manifest
            .entries
            .iter()
            .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(rels.len(), 9);
        assert!(rels.iter().all(|r| {
            r.starts_with("photos/") || r.starts_with("docs/") || r.starts_with("readme.txt/")
        }));
        // Roots stay contiguous so parents always precede children.
        let first_docs = rels.iter().position(|r| r.starts_with("docs/")).unwrap();
        assert!(rels[..first_docs].iter().all(|r| r.starts_with("photos/")));
    }

    #[test]
    fn single_root_source_set_is_the_frozen_manifest() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"x");
        let plain = LocalSource::open(src.path(), NonZeroU32::new(1024).unwrap()).unwrap();
        let (spec, _, _) = specs(src.path(), src.path(), 1);
        let set = SourceSet::open(&spec).unwrap();
        assert!(!set.multi);
        assert_eq!(set.manifest, plain.manifest().clone());
        assert_eq!(set.display_name(), plain.root_name());
    }

    #[test]
    fn empty_root_directory_becomes_a_top_level_entry() {
        let base = tempfile::tempdir().unwrap();
        let tree_a = base.path().join("photos");
        write(&tree_a.join("a.txt"), b"hello ak");
        let empty_root = base.path().join("nothing");
        fs::create_dir_all(&empty_root).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let (mut spec, _, _) = specs(&tree_a, dst.path(), 16);
        spec.add_root(empty_root.clone());
        let set = SourceSet::open(&spec).unwrap();
        let rels: Vec<String> = set
            .manifest
            .entries
            .iter()
            .map(|e| e.rel_path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(rels, vec!["photos/a.txt", "nothing"]);
        assert!(set.manifest.entries[1].is_dir);
    }

    // ------------------------------------------------------------------
    // Concurrent receive (serve_concurrently)
    // ------------------------------------------------------------------

    /// serve_concurrently keeps its signature minimal (no addr_tx), so
    /// tests probe a free UDP port exactly the way the receiver will bind
    /// one; same-process UDP rebinding is immediate on every platform we
    /// run the suite on.
    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        socket.local_addr().unwrap().port()
    }

    fn concurrent_receive_spec(dst: &Path, checkpoint: &Path) -> (ReceiveSpec, Fingerprint) {
        let receiver_key = DeviceKeyPair::generate();
        let mut recv = ReceiveSpec::new(
            dst.to_path_buf(),
            ak_identity::tls::make_self_signed(&receiver_key),
        );
        recv.bind = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
        recv.checkpoint_dir = checkpoint.to_path_buf();
        (recv, receiver_key.fingerprint())
    }

    fn concurrent_send_spec(
        src: PathBuf,
        peer: SocketAddr,
        pinned: Fingerprint,
        chunk_kb: u32,
        checkpoint: &Path,
    ) -> SendSpec {
        let mut spec = SendSpec::new(
            src,
            peer,
            pinned,
            ak_identity::tls::make_self_signed(&DeviceKeyPair::generate()),
        );
        spec.chunk_size = NonZeroU32::new(chunk_kb * 1024).unwrap();
        spec.checkpoint_dir = checkpoint.to_path_buf();
        spec
    }

    /// Two independent senders (own keys and certificates, own source
    /// trees, one pinned receiver) land concurrently in one
    /// serve_concurrently(2) receiver: both jobs complete, bytes are
    /// identical on both sides, and every progress event carries its task
    /// ordinal (0/1, accept order) with each ordinal observing its own
    /// Connected..Done sequence.
    #[tokio::test]
    async fn two_senders_concurrently_complete_into_one_receiver() {
        let src_a = tempfile::tempdir().unwrap();
        let src_b = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload_a: Vec<u8> = (0..30_000u32).map(|i| i as u8).collect();
        let payload_b: Vec<u8> = (0..24_000u32).map(|i| (i % 251) as u8).collect();
        write(&src_a.path().join("alpha/a.bin"), &payload_a);
        write(&src_b.path().join("beta/b.bin"), &payload_b);
        fs::create_dir_all(src_b.path().join("beta/empty")).unwrap();

        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
        let spec_a = concurrent_send_spec(
            src_a.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            1,
            &checkpoint,
        );
        let spec_b = concurrent_send_spec(
            src_b.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            4,
            &checkpoint,
        );

        let seen: Arc<std::sync::Mutex<Vec<(u64, Progress)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let progress = move |index: u64, event: Progress| log.lock().unwrap().push((index, event));
        let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &progress).await });
        // Let the listener bind before the senders dial (the port was
        // probed, not reported).
        tokio::time::sleep(Duration::from_millis(150)).await;

        let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
        let sender_b = tokio::spawn(async move { send(spec_b, &noop).await });
        let report_a = sender_a.await.unwrap().expect("sender A completes");
        let report_b = sender_b.await.unwrap().expect("sender B completes");
        let outcomes = receiver.await.unwrap();

        assert_eq!(outcomes.len(), 2);
        let mut reports = Vec::with_capacity(2);
        for (index, outcome) in outcomes.into_iter().enumerate() {
            reports.push(outcome.unwrap_or_else(|error| panic!("job {index} failed: {error:?}")));
        }
        // Slots are in accept order, which is racy; compare as a set.
        let mut by_bytes: Vec<(u64, u64)> = reports.iter().map(|r| (r.bytes, r.files)).collect();
        by_bytes.sort_unstable();
        let mut expected = vec![(24_000, 3), (30_000, 2)];
        expected.sort_unstable();
        assert_eq!(by_bytes, expected, "reports: {reports:?}");
        assert_eq!(report_a.bytes, 30_000);
        assert_eq!(report_b.bytes, 24_000);

        assert_eq!(fs::read(dst.path().join("alpha/a.bin")).unwrap(), payload_a);
        assert_eq!(fs::read(dst.path().join("beta/b.bin")).unwrap(), payload_b);
        assert!(dst.path().join("beta/empty").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (2, 30_000 + 24_000));

        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().all(|(index, _)| *index < 2),
            "task ordinals out of range: {seen:?}"
        );
        for index in 0..2u64 {
            assert!(
                seen.iter()
                    .any(|(i, e)| *i == index && matches!(e, Progress::Connected { .. })),
                "no Connected event for task {index}: {seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|(i, e)| *i == index && matches!(e, Progress::Done)),
                "no Done event for task {index}: {seen:?}"
            );
        }
    }

    /// One sender aborted mid-transfer must not sink the other job: the
    /// Vec holds exactly one Ok (the healthy job, bytes intact) and one Err
    /// (the aborted one), and the aborted job's part file survives with its
    /// verified prefix for resume.
    #[tokio::test]
    async fn one_failing_sender_does_not_sink_the_other_job() {
        let src_a = tempfile::tempdir().unwrap();
        let src_b = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload_a: Vec<u8> = (0..20_000u32).map(|i| (i % 249) as u8).collect();
        let payload_b: Vec<u8> = (0..16_384u32).map(|i| (i % 253) as u8).collect();
        write(&src_a.path().join("solid/a.bin"), &payload_a);
        write(&src_b.path().join("doomed/b.bin"), &payload_b);

        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, receiver_fp) = concurrent_receive_spec(dst.path(), &checkpoint);
        let spec_a = concurrent_send_spec(
            src_a.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            4,
            &checkpoint,
        );
        let mut spec_b = concurrent_send_spec(
            src_b.path().to_path_buf(),
            recv.bind,
            receiver_fp,
            1,
            &checkpoint,
        );
        // Park after every chunk: the abort below then lands deterministically
        // mid-transfer.
        spec_b.in_flight_window = 1;

        let receiver = tokio::spawn(async move { serve_concurrently(recv, 2, &|_, _| {}).await });
        tokio::time::sleep(Duration::from_millis(150)).await;

        let sender_a = tokio::spawn(async move { send(spec_a, &noop).await });
        let (sent_tx, mut sent_rx) = tokio::sync::watch::channel(0u64);
        let sender_b = {
            let progress = std::sync::Arc::new(move |event: Progress| {
                if let Progress::ChunkSent { .. } = event {
                    sent_tx.send_modify(|count| *count += 1);
                }
            });
            tokio::spawn(async move { send(spec_b, &*progress).await })
        };
        while *sent_rx.borrow() < 2 {
            sent_rx.changed().await.unwrap();
        }
        sender_b.abort();

        let report_a = sender_a.await.unwrap().expect("healthy sender completes");
        let outcomes = receiver.await.unwrap();
        assert_eq!(outcomes.len(), 2);
        let oks: Vec<&Report> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        let errs: Vec<&EngineError> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();
        assert_eq!(oks.len(), 1, "outcomes: {outcomes:?}");
        assert_eq!(errs.len(), 1, "outcomes: {outcomes:?}");
        assert_eq!(oks[0].bytes, report_a.bytes);

        assert_eq!(fs::read(dst.path().join("solid/a.bin")).unwrap(), payload_a);
        // The aborted job leaves its verified part file for resume, and no
        // committed final file.
        assert!(
            dst.path()
                .join(format!("doomed/b.bin{}", ak_storage::PART_SUFFIX))
                .exists(),
            "part file must survive the aborted sender"
        );
        assert!(!dst.path().join("doomed/b.bin").exists());
    }

    /// The no-hang contract: a slot whose sender never arrives fails with
    /// IdleTimeout once the (per-accept) idle budget expires, instead of
    /// blocking forever.
    #[tokio::test]
    async fn serve_concurrently_times_out_unfilled_slots_instead_of_hanging() {
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (mut recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
        recv.idle_timeout = Duration::from_millis(200);
        let started = Instant::now();
        let outcomes = serve_concurrently(recv, 1, &|_index: u64, _event: Progress| {}).await;
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(outcomes[0], Err(EngineError::IdleTimeout(_))),
            "got: {outcomes:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not hang, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn serve_concurrently_zero_expected_jobs_returns_empty() {
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = std::env::temp_dir().join(format!("ak-tests-{}", std::process::id()));
        let (recv, _) = concurrent_receive_spec(dst.path(), &checkpoint);
        let outcomes = serve_concurrently(recv, 0, &|_index: u64, _event: Progress| {}).await;
        assert!(outcomes.is_empty());
    }

    /// T12 analog: >4 GiB via a sparse file. Release-only (debug hashing is
    /// too slow); run explicitly with `cargo test --release -p ak-engine --
    /// --ignored`.
    #[tokio::test]
    #[ignore = "5 GiB sparse transfer; run in release only"]
    async fn five_gib_sparse_file_transfers_with_u64_offsets() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let big = src.path().join("big.bin");
        let file = fs::File::create(&big).unwrap();
        file.set_len(5_u64 * 1024 * 1024 * 1024 + 13).unwrap();
        drop(file);
        write(&src.path().join("tail.txt"), b"after the big one");

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1024);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(async move { receive_with_addr(recv, &noop, addr_tx).await });
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &noop).await.expect("5 GiB send");
        receiver.await.unwrap().expect("5 GiB receive");
        assert_eq!(report.bytes, 5 * 1024 * 1024 * 1024 + 13 + 17); // big.bin + tail.txt (17 bytes)
        assert_eq!(
            fs::metadata(dst.path().join("big.bin")).unwrap().len(),
            5 * 1024 * 1024 * 1024 + 13
        );
        assert_eq!(
            fs::read(dst.path().join("tail.txt")).unwrap(),
            b"after the big one"
        );
    }

    /// Drive a raw v1 receiver through Hello/Offer/Accept/Manifest and reply
    /// with an empty ResumeRequest; returns the job id. Used by the pause and
    /// fallback tests.
    async fn fake_receiver_preamble(
        stream: &mut ak_net::DirectStream,
        identity: &TlsBundle,
        capabilities: u16,
    ) -> JobId {
        let limits = engine_limits();
        let _ = stream.recv_frame(&limits).await.unwrap(); // sender Hello
        stream
            .send_frame(&hello_frame("fake", identity, capabilities))
            .await
            .unwrap();
        let job_id = match stream.recv_frame(&limits).await.unwrap().message {
            Message::Offer { job_id, .. } => job_id,
            other => panic!("expected Offer, got {other:?}"),
        };
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Accept { job_id },
            })
            .await
            .unwrap();
        loop {
            match stream.recv_frame(&limits).await.unwrap().message {
                Message::ManifestPage { last_page, .. } => {
                    if last_page {
                        break;
                    }
                }
                other => panic!("expected ManifestPage, got {other:?}"),
            }
        }
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ResumeRequest {
                    job_id,
                    files: Vec::new(),
                    more: false,
                },
            })
            .await
            .unwrap();
        job_id
    }

    /// Ack every remaining chunk on the stream, answer Finalize with a
    /// positive FinalAck, and return the number of chunks acked.
    async fn fake_ack_until_finalize(stream: &mut ak_net::DirectStream, job_id: JobId) -> u64 {
        let limits = engine_limits();
        let mut acked = 0u64;
        loop {
            match stream.recv_frame(&limits).await.unwrap().message {
                Message::Chunk {
                    file_index,
                    chunk_index,
                    ..
                } => {
                    stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::ChunkAck {
                                job_id,
                                file_index,
                                chunk_index,
                            },
                        })
                        .await
                        .unwrap();
                    acked += 1;
                }
                Message::Finalize { .. } => {
                    stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::FinalAck {
                                job_id,
                                verified: true,
                            },
                        })
                        .await
                        .unwrap();
                    // Hold the connection like the real receiver does: the
                    // sender reads the FinalAck and then half-closes. Dropping
                    // the endpoint now could discard the FinalAck in flight.
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if stream.recv_frame(&limits).await.is_err() {
                                break;
                            }
                        }
                    })
                    .await;
                    return acked;
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    }

    /// The peer completes the handshake then goes silent: the sender must
    /// fail with IdleTimeout (never hang), keeping its checkpoint.
    #[tokio::test]
    async fn idle_timeout_when_peer_goes_silent_after_handshake() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("idle.bin"), b"payload");
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 16);
        send_spec.idle_timeout = Duration::from_millis(300);
        let checkpoint = send_spec.checkpoint_dir.clone();

        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;
        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            let limits = engine_limits();
            let _ = stream.recv_frame(&limits).await; // sender Hello
            stream
                .send_frame(&hello_frame("stall", &bundle, RECV_CAPABILITIES))
                .await
                .unwrap();
            let _ = stream.recv_frame(&limits).await; // Offer
                                                      // Silent from here; hold the stream open so the sender observes
                                                      // silence rather than a close.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let err = send(send_spec, &noop)
            .await
            .expect_err("silent peer must time out");
        assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
        assert!(
            !FileCheckpointStore::open(&checkpoint)
                .unwrap()
                .list()
                .unwrap()
                .is_empty(),
            "checkpoint must survive the idle abort"
        );
        fake.abort();
    }

    /// The sender stalls right after Hello: the receiver must fail with
    /// IdleTimeout while waiting for the Offer.
    #[tokio::test]
    async fn idle_timeout_on_receive_when_sender_stalls() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_millis(300);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        stream
            .send_frame(&hello_frame("stall", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&engine_limits()).await; // receiver Hello

        let err = receiver
            .await
            .unwrap()
            .expect_err("stalled sender must time out");
        assert!(matches!(err, EngineError::IdleTimeout(_)), "got {err:?}");
    }

    /// The receiver sends Pause mid-transfer: the sender stops issuing
    /// chunks until Resume, then finishes the job.
    #[tokio::test]
    async fn pause_holds_sender_until_resume() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("pause.bin"), &payload);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1); // 8 chunks
        send_spec.in_flight_window = 1;
        send_spec.idle_timeout = Duration::from_secs(20);
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
            let limits = engine_limits();
            let (file_index, chunk_index) = match stream.recv_frame(&limits).await.unwrap().message
            {
                Message::Chunk {
                    file_index,
                    chunk_index,
                    ..
                } => (file_index, chunk_index),
                other => panic!("expected Chunk, got {other:?}"),
            };
            // Pause, ack the in-flight chunk, then hold.
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Control {
                        job_id,
                        control: Control::Pause,
                    },
                })
                .await
                .unwrap();
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ChunkAck {
                        job_id,
                        file_index,
                        chunk_index,
                    },
                })
                .await
                .unwrap();
            let leaked =
                tokio::time::timeout(Duration::from_millis(800), stream.recv_frame(&limits)).await;
            assert!(leaked.is_err(), "sender sent a chunk while paused");
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Control {
                        job_id,
                        control: Control::Resume,
                    },
                })
                .await
                .unwrap();
            1 + fake_ack_until_finalize(&mut stream, job_id).await
        });

        let events: Arc<std::sync::Mutex<Vec<Progress>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&events);
        let progress = move |event: Progress| log.lock().unwrap().push(event);
        let report = send(send_spec, &progress)
            .await
            .expect("send completes after Resume");
        let acked = fake.await.unwrap();
        assert_eq!(acked, 8, "all chunks must be acked after resume");
        assert_eq!(report.files, 1);

        let events = events.lock().unwrap();
        let paused = events
            .iter()
            .position(|e| matches!(e, Progress::Paused))
            .expect("Paused progress event");
        let resumed = events
            .iter()
            .position(|e| matches!(e, Progress::Resumed))
            .expect("Resumed progress event");
        let sent_while_paused = events[paused..=resumed]
            .iter()
            .filter(|e| matches!(e, Progress::ChunkSent { .. }))
            .count();
        assert_eq!(
            sent_while_paused, 0,
            "no chunk may be sent while paused (events: {events:?})"
        );
    }

    /// The sender opts into multistream but the peer does not advertise the
    /// capability: the transfer must run over the frozen single-stream path.
    #[tokio::test]
    async fn multistream_sender_falls_back_when_peer_lacks_capability() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
        write(&src.path().join("fallback.bin"), &payload);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
        send_spec.multistream = true;
        send_spec.in_flight_window = 2;
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            // A v1-only receiver: no CAP_MULTISTREAM bit in its Hello.
            let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
            fake_ack_until_finalize(&mut stream, job_id).await
        });
        let report = send(send_spec, &noop)
            .await
            .expect("fallback send completes");
        assert_eq!(fake.await.unwrap(), 8);
        assert_eq!(report.files, 1);
    }

    /// Both sides advertise CAP_MULTISTREAM and the sender opts in: every
    /// file rides its own stream, interleaved, and the result is identical.
    #[tokio::test]
    async fn multistream_loopback_transfer_is_byte_identical() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("a.txt"), b"hello ak");
        let deep: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
        write(&src.path().join("nested/deep/b.bin"), &deep);
        let third: Vec<u8> = (0..20_000u32).map(|i| (i % 199) as u8).collect();
        write(&src.path().join("nested/c.bin"), &third);
        fs::create_dir_all(src.path().join("empty/dir")).unwrap();

        let (mut send_spec, recv, _) = specs(src.path(), dst.path(), 1); // 1 KiB chunks
        send_spec.multistream = true;
        send_spec.in_flight_window = 4; // up to 4 concurrent file streams

        let sent: Arc<std::sync::Mutex<Vec<u32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bytes_seen: Arc<std::sync::Mutex<Vec<(u32, u64)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sender_log = Arc::clone(&sent);
        let sender_progress = move |event: Progress| {
            if let Progress::ChunkSent { file_index, .. } = event {
                sender_log.lock().unwrap().push(file_index);
            }
        };
        let recv_log = Arc::clone(&bytes_seen);
        let recv_progress = move |event: Progress| {
            if let Progress::BytesAcked {
                file_index,
                cumulative_bytes,
            } = event
            {
                recv_log
                    .lock()
                    .unwrap()
                    .push((file_index, cumulative_bytes));
            }
        };

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(async move { receive_with_addr(recv, &recv_progress, addr_tx).await });
        send_spec.peer = addr_rx.await.unwrap();
        let report = send(send_spec, &sender_progress)
            .await
            .expect("multistream send completes");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("multistream receive completes");

        assert_eq!(report.files, 7); // 3 files + 4 directory entries
        assert_eq!(report.bytes, recv_report.bytes);
        assert_eq!(fs::read(dst.path().join("a.txt")).unwrap(), b"hello ak");
        assert_eq!(
            fs::read(dst.path().join("nested/deep/b.bin")).unwrap(),
            deep
        );
        assert_eq!(fs::read(dst.path().join("nested/c.bin")).unwrap(), third);
        assert!(dst.path().join("empty/dir").is_dir());
        let (files, bytes) = count_entries(dst.path());
        assert_eq!((files, bytes), (3, 8 + 40_000 + 20_000));

        // Streams ran in parallel: file indexes interleave in ChunkSent.
        // Manifest order: a.txt=0, empty=1, empty/dir=2, nested=3,
        // nested/c.bin=4 (20 chunks), nested/deep=5, nested/deep/b.bin=6 (40).
        let sent = sent.lock().unwrap();
        let file_b = sent.iter().filter(|f| **f == 6).count();
        assert_eq!(file_b, 40, "b.bin must send all 40 chunks");
        let first_b = sent.iter().position(|f| *f == 6).unwrap();
        let last_b = sent.iter().rposition(|f| *f == 6).unwrap();
        assert!(
            sent[first_b..=last_b].contains(&4),
            "file streams did not interleave: {sent:?}"
        );

        // Byte-level progress fired at checkpoint cadence, chunk-aligned.
        let seen = bytes_seen.lock().unwrap();
        assert!(!seen.is_empty(), "no BytesAcked events observed");
        assert!(
            seen.iter().all(|(_, b)| b % 1024 == 0),
            "unaligned: {seen:?}"
        );
        assert!(
            seen.iter()
                .map(|(_, b)| *b)
                .max()
                .is_some_and(|m| m >= 16 * 1024),
            "expected at least one cadence event past 16 chunks: {seen:?}"
        );
    }

    /// The sender vanishes after the manifest: the receiver fails, and no
    /// empty `.akpart` litter remains in the destination.
    #[tokio::test]
    async fn failed_receive_discards_empty_part_files() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle);
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(5);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        stream
            .send_frame(&hello_frame("ghost", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
        let data = b"hello";
        let digest: [u8; 32] = *blake3::hash(data).as_bytes();
        let entry = WireEntry {
            rel_path: "ghost.txt".to_owned(),
            is_dir: false,
            size: data.len() as u64,
            mtime_unix_ms: 0,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: digest,
            chunk_digests: vec![digest],
        };
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id: [1; 16],
                    root_name: "ghost".to_owned(),
                    file_count: 1,
                    total_bytes: data.len() as u64,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // Accept
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id: [1; 16],
                    page_index: 0,
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // ResumeRequest: sinks exist now
                                                           // Vanish before any chunk.
        drop(stream);
        channel.close(0, b"test done");
        drop(endpoint);

        let err = receiver
            .await
            .unwrap()
            .expect_err("receiver must fail after the sender vanished");
        assert!(
            matches!(err, EngineError::PeerClosed | EngineError::Stream(_)),
            "got {err:?}"
        );
        let parts: Vec<_> = fs::read_dir(dst.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .to_string_lossy()
                    .ends_with(ak_storage::PART_SUFFIX)
            })
            .collect();
        assert!(parts.is_empty(), "empty part files survived: {parts:?}");
    }

    /// Regression for the 4,097-file defect, now per frame: a ResumeRequest
    /// frame covering a full shard of `max_resume_entries` entries must
    /// decode on the sender under engine limits.
    #[test]
    fn engine_limits_fit_resume_requests_for_maximum_jobs() {
        let limits = engine_limits();
        assert_eq!(limits.max_resume_entries, ak_wire::MAX_RESUME_ENTRIES);
        // One frame with the worst-case per-frame entry count, announcing a
        // following frame.
        let files: Vec<ResumeEntry> = (0..limits.max_resume_entries as u32)
            .map(|file_index| ResumeEntry {
                file_index,
                verified_prefix_chunks: 0,
            })
            .collect();
        let mut buf = Vec::new();
        Frame {
            version: ak_wire::VERSION,
            message: Message::ResumeRequest {
                job_id: [7; 16],
                files,
                more: true,
            },
        }
        .encode(&mut buf);
        assert!(buf.len() < ak_wire::MAX_FRAME_BYTES);
        let decoded = Frame::decode(&buf, &limits).expect("max-size ResumeRequest decodes");
        match decoded.message {
            Message::ResumeRequest { files, more, .. } => {
                assert_eq!(files.len(), 65_535);
                assert!(more);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Sharded ResumeRequest exchange (single-frame job cap removed)
    // ------------------------------------------------------------------

    /// The shard function: every non-final frame carries more = true, no
    /// frame exceeds the cap, an empty answer yields exactly one empty final
    /// frame, and concatenation reproduces the input.
    #[test]
    fn resume_request_frames_shard_within_the_cap() {
        let cap = engine_limits().max_resume_entries as usize;
        for total in [0usize, 1, cap - 1, cap, cap + 1, 2 * cap + 7] {
            let files: Vec<ResumeEntry> = (0..total as u32)
                .map(|i| ResumeEntry {
                    file_index: i,
                    verified_prefix_chunks: u64::from(i) * 3,
                })
                .collect();
            let frames = resume_request_frames([9; 16], &files, cap);
            assert!(!frames.is_empty(), "total {total}: at least one frame");
            let mut reunited = Vec::new();
            for (n, message) in frames.iter().enumerate() {
                let Message::ResumeRequest {
                    job_id,
                    files: shard,
                    more,
                } = message
                else {
                    panic!("total {total}: frame {n} is not a ResumeRequest");
                };
                assert_eq!(*job_id, [9; 16], "total {total}: frame {n} job id");
                assert!(
                    shard.len() <= cap,
                    "total {total}: frame {n} holds {} > {cap}",
                    shard.len()
                );
                assert_eq!(
                    *more,
                    n + 1 < frames.len(),
                    "total {total}: frame {n} more flag"
                );
                reunited.extend_from_slice(shard);
            }
            assert_eq!(
                reunited, files,
                "total {total}: concatenation must reproduce the input"
            );
        }
    }

    /// A resume frame naming a different job aborts the exchange; the
    /// matching job id accumulates until a more = false frame ends it.
    #[test]
    fn resume_collector_validates_job_ids_and_completion() {
        let mut collector = ResumeCollector::new([1; 16], &one_file_manifest(8), &engine_limits());
        assert!(
            !collector
                .push(
                    [1; 16],
                    vec![ResumeEntry {
                        file_index: 0,
                        verified_prefix_chunks: 1,
                    }],
                    true
                )
                .unwrap(),
            "more = true must not finish the exchange"
        );
        assert_eq!(collector.entries.len(), 1);
        assert!(matches!(
            collector.push([2; 16], Vec::new(), false),
            Err(EngineError::PeerError { .. })
        ));
        assert!(
            collector.push([1; 16], Vec::new(), false).unwrap(),
            "more = false completes the exchange"
        );
        assert_eq!(collector.entries.len(), 1);
    }

    /// 100,000-entry job, unit level (no socket): shard, push every frame
    /// through the real wire encode/decode, and fold the decoded frames into
    /// the sender's accumulator loop (which now cross-validates every entry
    /// against a manifest of exactly this shape). Every entry must arrive in
    /// order and no frame may exceed the per-frame cap. Real 100k-file
    /// transfers are deliberately not exercised here (I/O cost);
    /// release-time manual runs own that boundary.
    #[test]
    fn one_hundred_thousand_resume_entries_arrive_sharded() {
        const TOTAL: usize = 100_000;
        const CHUNKS_PER_FILE: u64 = 4;
        let limits = engine_limits();
        let cap = usize::from(limits.max_resume_entries);
        let job_id = [0xA5; 16];
        let files: Vec<ResumeEntry> = (0..TOTAL as u32)
            .map(|i| ResumeEntry {
                file_index: i,
                verified_prefix_chunks: u64::from(i) % CHUNKS_PER_FILE,
            })
            .collect();
        // The sender's manifest for this answer: one 4-chunk file per entry.
        let manifest = Manifest {
            chunk_size: NonZeroU32::new(1024).unwrap(),
            entries: (0..TOTAL)
                .map(|i| {
                    FileEntry::new(
                        &format!("f{i}.bin"),
                        CHUNKS_PER_FILE * 1024,
                        0,
                        [7; 32],
                        vec![[7; 32]; CHUNKS_PER_FILE as usize],
                    )
                    .unwrap()
                })
                .collect(),
        };

        let frames = resume_request_frames(job_id, &files, cap);
        assert_eq!(
            frames.len(),
            TOTAL.div_ceil(cap),
            "100k entries need two shards at cap {cap}"
        );

        // Simulated sender receive loop: decode each frame, fold it in.
        let mut collector = ResumeCollector::new(job_id, &manifest, &limits);
        let mut final_frame = false;
        for message in frames {
            let mut buf = Vec::new();
            Frame {
                version: ak_wire::VERSION,
                message,
            }
            .encode(&mut buf);
            assert!(
                buf.len() < ak_wire::MAX_FRAME_BYTES,
                "shard frame must fit the frame cap"
            );
            match Frame::decode(&buf, &limits)
                .expect("shard frame decodes under engine limits")
                .message
            {
                Message::ResumeRequest {
                    job_id: frame_job_id,
                    files: shard,
                    more,
                } => {
                    assert!(
                        shard.len() <= cap,
                        "single frame exceeded the per-frame cap"
                    );
                    if collector
                        .push(frame_job_id, shard, more)
                        .expect("frame accepted")
                    {
                        final_frame = true;
                        break;
                    }
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
        assert!(final_frame, "exchange must end with a more = false frame");
        assert_eq!(
            collector.entries, files,
            "all 100,000 entries must arrive in order"
        );
    }

    // ------------------------------------------------------------------
    // Round 17: record id vs content id (review item 4)
    // ------------------------------------------------------------------

    /// A one-file manifest with `chunks` 1 KiB chunks (unit-test fixture).
    fn one_file_manifest(chunks: usize) -> Manifest {
        Manifest {
            chunk_size: NonZeroU32::new(1024).unwrap(),
            entries: vec![FileEntry::new(
                "one.bin",
                chunks as u64 * 1024,
                0,
                [3; 32],
                vec![[3; 32]; chunks],
            )
            .unwrap()],
        }
    }

    /// The record id is a pure function of (content, peer, direction, root),
    /// distinct from the content id, with separators normalized.
    #[test]
    fn record_id_binds_peer_direction_and_root() {
        let manifest = one_file_manifest(2);
        let content = job_id_for("one.bin", &manifest);
        let fp_a = DeviceKeyPair::generate().fingerprint();
        let fp_b = DeviceKeyPair::generate().fingerprint();
        let a = record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/src"));
        assert_eq!(
            a,
            record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/src")),
            "same inputs must derive the same record id"
        );
        assert_ne!(
            a,
            record_id_for(&content, &fp_b, Direction::Send, Path::new("C:/data/src")),
            "another peer must get another record"
        );
        assert_ne!(
            a,
            record_id_for(
                &content,
                &fp_a,
                Direction::Receive,
                Path::new("C:/data/src")
            ),
            "the opposite direction must get another record"
        );
        assert_ne!(
            a,
            record_id_for(&content, &fp_a, Direction::Send, Path::new("C:/data/other")),
            "another root must get another record"
        );
        assert_ne!(
            a, content,
            "the record id must differ from the wire content id"
        );
        assert_eq!(
            a,
            record_id_for(&content, &fp_a, Direction::Send, Path::new("C:\\data\\src")),
            "equivalent path spellings (separator style) must agree"
        );
    }

    /// Same source sent to two different peers, serially, with one shared
    /// sender checkpoint directory: two distinct Send records must survive
    /// (the content-id keying of old overwrote one with the other).
    #[tokio::test]
    async fn same_source_to_two_peers_keeps_two_sender_records() {
        let src = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("shared.bin"), &payload);
        let sender_key = DeviceKeyPair::generate();
        let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
        let checkpoint = tempfile::tempdir().unwrap();

        for round in 0..2 {
            let dst = tempfile::tempdir().unwrap();
            let receiver_key = DeviceKeyPair::generate();
            let this_fp = receiver_key.fingerprint();
            let mut recv = ReceiveSpec::new(
                dst.path().to_path_buf(),
                ak_identity::tls::make_self_signed(&receiver_key),
            );
            recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
            let mut send_spec = SendSpec::new(
                src.path().to_path_buf(),
                addr_rx.await.unwrap(),
                this_fp,
                sender_bundle.clone(),
            );
            send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
            send_spec.checkpoint_dir = checkpoint.path().to_path_buf();
            send(send_spec, &noop)
                .await
                .unwrap_or_else(|e| panic!("round {round} send failed: {e:?}"));
            receiver
                .await
                .unwrap()
                .unwrap_or_else(|e| panic!("round {round} receive failed: {e:?}"));
        }

        let store = FileCheckpointStore::open(checkpoint.path()).unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), 2, "records: {records:?}");
        assert!(
            records
                .iter()
                .all(|r| r.direction == Direction::Send && r.state == "completed"),
            "records: {records:?}"
        );
        assert_ne!(
            records[0].job_id, records[1].job_id,
            "the two peers must keep distinct record ids"
        );
    }

    /// One machine sends and receives through one shared checkpoint
    /// directory (the loopback case): the send record and the receive
    /// record must be separate files, not one overwriting the other.
    #[tokio::test]
    async fn loopback_send_and_receive_keep_separate_records() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = tempfile::tempdir().unwrap();
        write(&src.path().join("loop.bin"), b"same machine, two roles");
        let sender_key = DeviceKeyPair::generate();
        let receiver_key = DeviceKeyPair::generate();
        let mut send_spec = SendSpec::new(
            src.path().to_path_buf(),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            receiver_key.fingerprint(),
            ak_identity::tls::make_self_signed(&sender_key),
        );
        send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
        send_spec.checkpoint_dir = checkpoint.path().to_path_buf();
        let mut recv = ReceiveSpec::new(
            dst.path().to_path_buf(),
            ak_identity::tls::make_self_signed(&receiver_key),
        );
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.chunk_size = send_spec.chunk_size;
        recv.checkpoint_dir = checkpoint.path().to_path_buf();

        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        send(send_spec, &noop).await.expect("loopback send");
        receiver.await.unwrap().expect("loopback receive");

        let store = FileCheckpointStore::open(checkpoint.path()).unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), 2, "records: {records:?}");
        assert_eq!(
            records
                .iter()
                .filter(|r| r.direction == Direction::Send)
                .count(),
            1,
            "records: {records:?}"
        );
        assert_eq!(
            records
                .iter()
                .filter(|r| r.direction == Direction::Receive)
                .count(),
            1,
            "records: {records:?}"
        );
        assert_ne!(
            records[0].job_id, records[1].job_id,
            "send and receive records must not share one key"
        );
    }

    // ------------------------------------------------------------------
    // Round 17: committed-file retry idempotency (review item 5)
    // ------------------------------------------------------------------

    /// Specs with identities held fixed across runs, so both runs derive the
    /// same record id (same content, peer, direction, root).
    fn fixed_specs(
        src: &Path,
        dst: &Path,
        checkpoint: &Path,
        sender_key: &DeviceKeyPair,
        receiver_key: &DeviceKeyPair,
    ) -> (SendSpec, ReceiveSpec) {
        let mut send_spec = SendSpec::new(
            src.to_path_buf(),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            receiver_key.fingerprint(),
            ak_identity::tls::make_self_signed(sender_key),
        );
        send_spec.chunk_size = NonZeroU32::new(1024).unwrap();
        send_spec.checkpoint_dir = checkpoint.to_path_buf();
        let mut recv = ReceiveSpec::new(
            dst.to_path_buf(),
            ak_identity::tls::make_self_signed(receiver_key),
        );
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.chunk_size = send_spec.chunk_size;
        recv.checkpoint_dir = checkpoint.to_path_buf();
        (send_spec, recv)
    }

    /// A fully completed job retried with the same record must be a no-op:
    /// both sides succeed, the resume reports cover every chunk (full
    /// prefix via the committed-file skip), nothing is retransmitted, and
    /// the committed files are not touched again (no rename conflict, no
    /// duplicate copies). Old behavior: the receiver prepared a fresh part
    /// file for the already-committed target and finish() died on the
    /// conflict policy.
    #[tokio::test]
    async fn completed_job_retry_is_idempotent_and_reports_full_resume() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let checkpoint = tempfile::tempdir().unwrap();
        let payloads: Vec<(String, Vec<u8>)> = (0..2)
            .map(|f| {
                (
                    format!("retry-{f}.bin"),
                    (0..8192u32)
                        .map(|i| ((i % 249 + f * 31) % 256) as u8)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        for (name, payload) in &payloads {
            write(&src.path().join(name), payload);
        }
        let sender_key = DeviceKeyPair::generate();
        let receiver_key = DeviceKeyPair::generate();

        // Run 1: the normal full transfer.
        {
            let (mut send_spec, recv) = fixed_specs(
                src.path(),
                dst.path(),
                checkpoint.path(),
                &sender_key,
                &receiver_key,
            );
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
            send_spec.peer = addr_rx.await.unwrap();
            let report = send(send_spec, &noop).await.expect("first run sends");
            assert_eq!(report.resumed_chunks, 0);
            receiver.await.unwrap().expect("first run receives");
        }
        for (name, payload) in &payloads {
            assert_eq!(&fs::read(dst.path().join(name)).unwrap(), payload);
        }

        // Run 2: the retry of the same record.
        let (mut send_spec, recv) = fixed_specs(
            src.path(),
            dst.path(),
            checkpoint.path(),
            &sender_key,
            &receiver_key,
        );
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        send_spec.peer = addr_rx.await.unwrap();
        let send_report = send(send_spec, &noop)
            .await
            .expect("retry must not fail on the committed files");
        let recv_report = receiver
            .await
            .unwrap()
            .expect("retry receive must not fail on the committed files");

        let total_chunks: u64 = 8 + 8; // two 8 KiB files at 1 KiB chunks
        assert_eq!(
            send_report.resumed_chunks, total_chunks,
            "sender must skip every chunk via the full-prefix resume answer"
        );
        assert_eq!(
            recv_report.resumed_chunks, total_chunks,
            "receiver must report the committed files as fully resumed"
        );
        let (files, bytes) = count_entries(dst.path());
        assert_eq!(
            (files, bytes),
            (2, 16_384),
            "exactly the two committed files, no rename duplicates: {files} files"
        );
        for (name, payload) in &payloads {
            assert_eq!(&fs::read(dst.path().join(name)).unwrap(), payload);
        }
        let parts: Vec<_> = fs::read_dir(dst.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .to_string_lossy()
                    .ends_with(ak_storage::PART_SUFFIX)
            })
            .collect();
        assert!(
            parts.is_empty(),
            "retry must leave no part files: {parts:?}"
        );
    }

    // ------------------------------------------------------------------
    // Round 17: P1-1 chunk size limits (engine side, defense in depth)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn send_refuses_chunk_size_above_the_engine_limit_before_connecting() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("f.txt"), b"x");
        let (mut send_spec, _, _) = specs(src.path(), dst.path(), 16);
        send_spec.chunk_size = NonZeroU32::new(16 << 20).unwrap(); // 16 MiB > 8 MiB
        send_spec.peer = SocketAddr::from(([127, 0, 0, 1], 9)); // nothing there
        let started = Instant::now();
        let err = send(send_spec, &noop)
            .await
            .expect_err("oversized chunk size must fail the spec");
        assert!(
            matches!(&err, EngineError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
            "got: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must refuse before any connection attempt, took {:?}",
            started.elapsed()
        );
    }

    /// The receiver must refuse an Offer whose chunk_size is zero or above
    /// the engine limit BEFORE sending Accept — a legal frame carrying an
    /// absurd chunk size must never reach the storage layer's per-chunk
    /// allocations. The peer is told with an Error frame; whichever layer
    /// refuses first (ak-wire's decoder may also reject), the job must die.
    #[tokio::test]
    async fn receiver_refuses_offer_chunk_size_outside_engine_limits() {
        for bad in [0u32, (8u32 << 20) + 1, u32::MAX] {
            let dst = tempfile::tempdir().unwrap();
            let receiver_key = DeviceKeyPair::generate();
            let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
            let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
            let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
            recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
            recv.idle_timeout = Duration::from_secs(5);
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
            let addr = addr_rx.await.unwrap();

            let endpoint =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
                    .await
                    .unwrap();
            let channel = endpoint
                .connect(addr, &receiver_key.fingerprint())
                .await
                .unwrap();
            let mut stream = channel.open_bi().await.unwrap();
            let limits = engine_limits();
            stream
                .send_frame(&hello_frame("bad-offer", &sender_bundle, CAP_RESUME))
                .await
                .unwrap();
            let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Offer {
                        job_id: [3; 16],
                        root_name: "bad".to_owned(),
                        file_count: 1,
                        total_bytes: 4,
                        chunk_size: bad,
                    },
                })
                .await
                .unwrap();
            // The reply must be a refusal — an Error frame here, or a closed
            // stream if the wire decoder refused the frame first. Never an
            // Accept.
            match tokio::time::timeout(Duration::from_secs(5), stream.recv_frame(&limits)).await {
                Ok(Ok(Frame {
                    message: Message::Error { code, .. },
                    ..
                })) => assert_eq!(code, ErrorCode::ProtocolUnsupported),
                Ok(Ok(frame)) => panic!("chunk_size {bad}: expected refusal, got {frame:?}"),
                Ok(Err(_)) => {} // decoder/connection refused; equally fine
                Err(_) => panic!("chunk_size {bad}: receiver stayed silent"),
            }
            let outcome = receiver.await.unwrap();
            assert!(
                outcome.is_err(),
                "chunk_size {bad}: receiver must fail, got {outcome:?}"
            );
            assert_eq!(
                count_entries(dst.path()),
                (0, 0),
                "nothing may be written for a refused offer"
            );
        }
    }

    // ------------------------------------------------------------------
    // Round 17: ManifestPage cross-frame validation (review item 3)
    // ------------------------------------------------------------------

    /// Wire entry fixture with real digests for `payload` at `chunk_size`.
    fn chunked_wire_entry(name: &str, payload: &[u8], chunk_size: usize) -> WireEntry {
        let chunk_digests: Vec<[u8; 32]> = payload
            .chunks(chunk_size)
            .map(|c| *blake3::hash(c).as_bytes())
            .collect();
        WireEntry {
            rel_path: name.to_owned(),
            is_dir: false,
            size: payload.len() as u64,
            mtime_unix_ms: 0,
            digest_alg: ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole_digest: *blake3::hash(payload).as_bytes(),
            chunk_digests,
        }
    }

    #[test]
    fn manifest_assembler_validates_pages_against_the_offer() {
        let limits = engine_limits();
        let a = chunked_wire_entry("a.bin", &[1, 2, 3, 4], 2);
        let b = chunked_wire_entry("b.bin", &[5, 6, 7], 2);
        let bytes = a.size + b.size;

        // Happy path: two consecutive pages, totals reconciled.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
        assert!(!m.push([1; 16], 0, false, vec![a.clone()]).unwrap());
        assert!(m.push([1; 16], 1, true, vec![b.clone()]).unwrap());
        assert_eq!(m.entries.len(), 2);

        // Wrong job on a page.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
        assert!(m.push([2; 16], 0, true, vec![a.clone()]).is_err());

        // Page order: does not start at 0.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
        assert!(m.push([1; 16], 1, false, vec![a.clone()]).is_err());

        // Page order: repeats an index.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
        m.push([1; 16], 0, false, vec![a.clone()]).unwrap();
        assert!(m.push([1; 16], 0, true, vec![b.clone()]).is_err());

        // More entries than the offer announced.
        let mut m = ManifestAssembler::new([1; 16], 1, a.size, &limits);
        assert!(m
            .push([1; 16], 0, true, vec![a.clone(), b.clone()])
            .is_err());

        // Last page with fewer entries than announced.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes, &limits);
        m.push([1; 16], 0, false, vec![a.clone()]).unwrap();
        assert!(m.push([1; 16], 1, true, Vec::new()).is_err());

        // Entry sizes disagree with the offered total bytes.
        let mut m = ManifestAssembler::new([1; 16], 2, bytes + 1, &limits);
        assert!(!m.push([1; 16], 0, false, vec![a.clone()]).unwrap());
        assert!(m.push([1; 16], 1, true, vec![b.clone()]).is_err());

        // Per-page entry budget (engine double-check on top of the decoder).
        let cap = usize::from(limits.max_entries_per_page);
        let mut m = ManifestAssembler::new([1; 16], cap as u64 + 1, 0, &limits);
        let oversized: Vec<WireEntry> = (0..=cap)
            .map(|i| chunked_wire_entry(&format!("e{i}.bin"), b"", 1))
            .collect();
        assert!(m.push([1; 16], 0, false, oversized).is_err());
    }

    /// End-to-end wiring proof: a real receiver fed manifest pages whose
    /// page_index does not start at 0 must abort with a protocol error and
    /// write nothing.
    #[tokio::test]
    async fn receiver_rejects_manifest_pages_out_of_order() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(5);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
            .await
            .unwrap();
        let channel = endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let mut stream = channel.open_bi().await.unwrap();
        let limits = engine_limits();
        stream
            .send_frame(&hello_frame("bad-pages", &sender_bundle, CAP_RESUME))
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // receiver Hello
        let entry = chunked_wire_entry("a.bin", &[1, 2, 3, 4], 2);
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::Offer {
                    job_id: [4; 16],
                    root_name: "bad-pages".to_owned(),
                    file_count: 1,
                    total_bytes: entry.size,
                    chunk_size: 1024,
                },
            })
            .await
            .unwrap();
        let _ = stream.recv_frame(&limits).await.unwrap(); // Accept
        stream
            .send_frame(&Frame {
                version: ak_wire::VERSION,
                message: Message::ManifestPage {
                    job_id: [4; 16],
                    page_index: 1, // must be 0
                    last_page: true,
                    entries: vec![entry],
                },
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let err = receiver
            .await
            .unwrap()
            .expect_err("out-of-order manifest pages must fail the receive");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        assert_eq!(count_entries(dst.path()), (0, 0));
    }

    // ------------------------------------------------------------------
    // Round 17: ResumeCollector cross-validation (review item 3)
    // ------------------------------------------------------------------

    #[test]
    fn resume_collector_cross_validates_against_the_manifest() {
        let limits = engine_limits();
        let manifest = Manifest {
            chunk_size: NonZeroU32::new(1024).unwrap(),
            entries: vec![
                FileEntry::new("a.bin", 2048, 0, [1; 32], vec![[1; 32]; 2]).unwrap(),
                FileEntry::directory("sub").unwrap(),
                FileEntry::new("b.bin", 1024, 0, [2; 32], vec![[2; 32]; 1]).unwrap(),
            ],
        };

        // Happy path: valid entries across two frames.
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        assert!(!c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: 2,
                }],
                true
            )
            .unwrap());
        assert!(c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 2,
                    verified_prefix_chunks: 1
                }],
                false
            )
            .unwrap());
        assert_eq!(c.entries.len(), 2);

        // Out-of-range file index.
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        assert!(c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 3,
                    verified_prefix_chunks: 0,
                }],
                false
            )
            .is_err());

        // Directory entries are never resumable.
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        assert!(c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 1,
                    verified_prefix_chunks: 0,
                }],
                false
            )
            .is_err());

        // Repeated file index.
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        c.push(
            [7; 16],
            vec![ResumeEntry {
                file_index: 0,
                verified_prefix_chunks: 1,
            }],
            true,
        )
        .unwrap();
        assert!(c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: 1,
                }],
                false
            )
            .is_err());

        // Prefix beyond the file's real chunk count.
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        assert!(c
            .push(
                [7; 16],
                vec![ResumeEntry {
                    file_index: 0,
                    verified_prefix_chunks: 3,
                }],
                false
            )
            .is_err());

        // Per-frame entry budget (above limits.max_resume_entries in one
        // frame; the decoder enforces its own bound, this is the engine's).
        let cap = usize::from(limits.max_resume_entries);
        let oversized: Vec<ResumeEntry> = (0..=cap as u32)
            .map(|file_index| ResumeEntry {
                file_index,
                verified_prefix_chunks: 0,
            })
            .collect();
        let mut c = ResumeCollector::new([7; 16], &manifest, &limits);
        assert!(c.push([7; 16], oversized, false).is_err());
    }

    // ------------------------------------------------------------------
    // Round 17: ACK idempotency (review item 3)
    // ------------------------------------------------------------------

    #[test]
    fn ack_ledger_confirms_once_and_counts_duplicates_and_unknowns() {
        let mut ledger = AckLedger::default();
        ledger.track(0, 0);
        ledger.track(3, 7);
        assert_eq!(ledger.apply(0, 0), AckOutcome::Confirmed);
        // Duplicate of an already-confirmed chunk: ignored, counted.
        assert_eq!(ledger.apply(0, 0), AckOutcome::DuplicateOrUnknown);
        // Chunk we never sent: ignored, counted.
        assert_eq!(ledger.apply(9, 9), AckOutcome::DuplicateOrUnknown);
        assert_eq!(ledger.apply(3, 7), AckOutcome::Confirmed);
        assert_eq!(ledger.duplicates, 2);
        assert!(ledger.inflight.is_empty(), "every tracked chunk retired");
    }

    /// Phantom acks must not fast-forward the window: with window = 1 the
    /// peer acks chunks 0..6 honestly, then answers chunk 7 only with
    /// duplicates of earlier acks. The sender must NOT let those duplicates
    /// retire chunk 7's outstanding slot — it must time out instead of
    /// completing (old behavior: any ack decremented the window and the
    /// transfer "completed" with an unacked chunk).
    #[tokio::test]
    async fn duplicate_acks_do_not_retire_unacked_chunks() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        write(&src.path().join("phantom.bin"), &payload);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
        send_spec.in_flight_window = 1;
        send_spec.idle_timeout = Duration::from_millis(600);
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut stream = channel.accept_bi().await.unwrap();
            let job_id = fake_receiver_preamble(&mut stream, &bundle, CAP_RESUME).await;
            let limits = engine_limits();
            let mut seen = 0u64;
            let mut finalized = false;
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    // Tolerant read: the sender aborting on the idle timeout
                    // (the expected outcome) drops the connection mid-loop.
                    let frame = match stream.recv_frame(&limits).await {
                        Ok(frame) => frame,
                        Err(_) => break,
                    };
                    match frame.message {
                        Message::Chunk {
                            file_index,
                            chunk_index,
                            ..
                        } => {
                            seen += 1;
                            if chunk_index < 7 {
                                stream
                                    .send_frame(&Frame {
                                        version: ak_wire::VERSION,
                                        message: Message::ChunkAck {
                                            job_id,
                                            file_index,
                                            chunk_index,
                                        },
                                    })
                                    .await
                                    .unwrap();
                            } else {
                                // Only duplicates for the last chunk.
                                let _ = stream
                                    .send_frame(&Frame {
                                        version: ak_wire::VERSION,
                                        message: Message::ChunkAck {
                                            job_id,
                                            file_index: 0,
                                            chunk_index: 0,
                                        },
                                    })
                                    .await;
                                let _ = stream
                                    .send_frame(&Frame {
                                        version: ak_wire::VERSION,
                                        message: Message::ChunkAck {
                                            job_id,
                                            file_index: 0,
                                            chunk_index: 0,
                                        },
                                    })
                                    .await;
                            }
                        }
                        Message::Finalize { .. } => {
                            finalized = true;
                            // Old behavior reached here: phantom acks had
                            // opened the window and the sender finished.
                            stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::FinalAck {
                                        job_id,
                                        verified: true,
                                    },
                                })
                                .await
                                .unwrap();
                            break;
                        }
                        other => panic!("unexpected frame: {other:?}"),
                    }
                }
            })
            .await;
            (seen, finalized)
        });

        let err = send(send_spec, &noop)
            .await
            .expect_err("phantom acks must not complete the transfer");
        assert!(
            matches!(err, EngineError::IdleTimeout(_)),
            "the unacked chunk 7 must keep the sender waiting, got {err:?}"
        );
        let (seen, finalized) = fake.await.unwrap();
        assert_eq!(seen, 8, "the fake peer must have seen every chunk");
        assert!(!finalized, "the sender must never reach Finalize");
    }

    /// Multistream tolerance: a receiver that prepends an unknown ack and
    /// double-acks every real chunk (sloppy but harmless peer) must not
    /// break the sender — duplicates and unknowns are ignored and counted,
    /// and the transfer completes with every chunk sent exactly once.
    #[tokio::test]
    async fn multistream_tolerates_duplicate_and_unknown_acks() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload_a: Vec<u8> = (0..8192u32).map(|i| (i % 249) as u8).collect();
        let payload_b: Vec<u8> = (0..4096u32).map(|i| (i % 199) as u8).collect();
        write(&src.path().join("a/one.bin"), &payload_a);
        write(&src.path().join("b/two.bin"), &payload_b);
        let (mut send_spec, recv_spec, receiver_fp) = specs(src.path(), dst.path(), 1);
        send_spec.multistream = true;
        send_spec.in_flight_window = 4;
        let bundle = recv_spec.identity;
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &bundle)
            .await
            .unwrap();
        send_spec.peer = endpoint.local_addr().unwrap();
        send_spec.pinned = receiver_fp;

        let sent: Arc<std::sync::Mutex<Vec<(u32, u64)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&sent);
        let progress = move |event: Progress| {
            if let Progress::ChunkSent {
                file_index,
                chunk_index,
            } = event
            {
                log.lock().unwrap().push((file_index, chunk_index));
            }
        };

        let fake = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let mut control = channel.accept_bi().await.unwrap();
            let job_id =
                fake_receiver_preamble(&mut control, &bundle, CAP_RESUME | CAP_MULTISTREAM).await;
            let limits = engine_limits();
            // Two file streams follow the control stream, in open order.
            let mut acked = 0u64;
            for _ in 0..2 {
                let mut stream = channel.accept_bi().await.unwrap();
                // An unknown ack before anything else: must be ignored.
                let _ = stream
                    .send_frame(&Frame {
                        version: ak_wire::VERSION,
                        message: Message::ChunkAck {
                            job_id,
                            file_index: 99,
                            chunk_index: 99,
                        },
                    })
                    .await;
                loop {
                    match stream.recv_frame(&limits).await {
                        Ok(Frame {
                            message:
                                Message::Chunk {
                                    file_index,
                                    chunk_index,
                                    ..
                                },
                            ..
                        }) => {
                            acked += 1;
                            stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::ChunkAck {
                                        job_id,
                                        file_index,
                                        chunk_index,
                                    },
                                })
                                .await
                                .unwrap();
                            // The duplicate: ignored by the ledger.
                            let _ = stream
                                .send_frame(&Frame {
                                    version: ak_wire::VERSION,
                                    message: Message::ChunkAck {
                                        job_id,
                                        file_index,
                                        chunk_index,
                                    },
                                })
                                .await;
                        }
                        Ok(_) => panic!("expected only chunks on a file stream"),
                        Err(_) => break, // sender half-closed after last ack
                    }
                }
            }
            match control.recv_frame(&limits).await.unwrap().message {
                Message::Finalize { .. } => {}
                other => panic!("expected Finalize, got {other:?}"),
            }
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::FinalAck {
                        job_id,
                        verified: true,
                    },
                })
                .await
                .unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if control.recv_frame(&limits).await.is_err() {
                        break;
                    }
                }
            })
            .await;
            acked
        });

        let report = send(send_spec, &progress)
            .await
            .expect("duplicate/unknown acks must not break the sender");
        let acked = fake.await.unwrap();
        assert_eq!(report.files, 4); // 2 files + 2 directory entries
        assert_eq!(acked, 8 + 4, "every chunk must be acked once");
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 12, "no chunk may be re-sent: {sent:?}");
        let mut unique = sent.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), sent.len(), "chunk sends must be unique");
    }

    // ------------------------------------------------------------------
    // Round 17: P1-2 control-stream activity budget (timing test)
    // ------------------------------------------------------------------

    /// P1-2 regression: a multistream transfer whose data phase alone
    /// outlasts the idle budget many times over must still complete. The
    /// control stream carries no frame between the ResumeRequest and the
    /// Finalize (by design — the sender finalizes only after every file
    /// stream finished), so the old independent control-read timeout killed
    /// healthy transfers `idle` seconds into the data phase. Fixed engine:
    /// data-stream events refresh one task-wide budget. Here: idle = 2 s,
    /// one chunk every 300 ms, data phase > 6 s (3x idle), and the receiver
    /// must verify and commit everything.
    #[tokio::test]
    async fn multistream_transfer_outlasting_idle_budget_completes() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(2);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        const FILES: usize = 2;
        const CHUNKS: usize = 11;
        const CHUNK_BYTES: usize = 1024;
        const DELAY: Duration = Duration::from_millis(300);
        let payloads: Vec<Vec<u8>> = (0..FILES)
            .map(|f| {
                (0..CHUNKS * CHUNK_BYTES)
                    .map(|i| ((i % 251 + f * 37) % 256) as u8)
                    .collect()
            })
            .collect();
        let entries: Vec<WireEntry> = payloads
            .iter()
            .enumerate()
            .map(|(f, p)| chunked_wire_entry(&format!("paced-{f}.bin"), p, CHUNK_BYTES))
            .collect();
        let receiver_fp = receiver_key.fingerprint();
        let verify_payloads = payloads.clone();

        // Fake multistream sender: control stream for the handshake, then
        // one paced stream per file (send chunk, await its ack, sleep).
        let fake = tokio::spawn(async move {
            let endpoint =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
                    .await
                    .unwrap();
            let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
            let mut control = channel.open_bi().await.unwrap();
            let limits = engine_limits();
            control
                .send_frame(&hello_frame(
                    "paced",
                    &sender_bundle,
                    CAP_RESUME | CAP_PAUSE_RESUME | CAP_MULTISTREAM,
                ))
                .await
                .unwrap();
            let _ = control.recv_frame(&limits).await.unwrap(); // receiver Hello
            let job_id = [0xA5; 16];
            let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Offer {
                        job_id,
                        root_name: "paced".to_owned(),
                        file_count: entries.len() as u64,
                        total_bytes,
                        chunk_size: CHUNK_BYTES as u32,
                    },
                })
                .await
                .unwrap();
            match control.recv_frame(&limits).await.unwrap().message {
                Message::Accept { .. } => {}
                other => panic!("expected Accept, got {other:?}"),
            }
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ManifestPage {
                        job_id,
                        page_index: 0,
                        last_page: true,
                        entries: entries.clone(),
                    },
                })
                .await
                .unwrap();
            match control.recv_frame(&limits).await.unwrap().message {
                Message::ResumeRequest { more, .. } => assert!(!more),
                other => panic!("expected ResumeRequest, got {other:?}"),
            }

            let started = Instant::now();
            for (file_index, payload) in payloads.iter().enumerate() {
                let mut stream = channel.open_bi().await.unwrap();
                for (chunk_index, data) in payload.chunks(CHUNK_BYTES).enumerate() {
                    stream
                        .send_frame(&Frame {
                            version: ak_wire::VERSION,
                            message: Message::Chunk {
                                job_id,
                                file_index: file_index as u32,
                                chunk_index: chunk_index as u64,
                                data: data.to_vec(),
                            },
                        })
                        .await
                        .unwrap();
                    match stream.recv_frame(&limits).await.unwrap().message {
                        Message::ChunkAck {
                            chunk_index: acked, ..
                        } => assert_eq!(acked, chunk_index as u64),
                        other => panic!("expected ChunkAck, got {other:?}"),
                    }
                    tokio::time::sleep(DELAY).await;
                }
                stream.finish_send().unwrap();
            }
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Finalize { job_id },
                })
                .await
                .unwrap();
            match tokio::time::timeout(Duration::from_secs(30), control.recv_frame(&limits)).await {
                Ok(Ok(Frame {
                    message: Message::FinalAck { verified, .. },
                    ..
                })) => assert!(verified, "receiver must verify the paced transfer"),
                Ok(Ok(frame)) => panic!("expected FinalAck, got {frame:?}"),
                Ok(Err(e)) => panic!("control stream died before FinalAck: {e}"),
                Err(_) => panic!("no FinalAck within 30s of Finalize"),
            }
            started.elapsed()
        });

        let recv_report = receiver
            .await
            .unwrap()
            .expect("a multistream receive must survive its data phase outlasting idle");
        let elapsed = fake.await.unwrap();
        assert!(
            elapsed >= Duration::from_secs(6),
            "the data phase must span > 6s (3x the 2s idle budget), took {elapsed:?}"
        );
        assert_eq!(recv_report.files, FILES as u64);
        assert_eq!(recv_report.bytes, (CHUNKS * CHUNK_BYTES * FILES) as u64);
        assert_eq!(recv_report.resumed_chunks, 0);
        for (f, payload) in verify_payloads.iter().enumerate() {
            assert_eq!(
                fs::read(dst.path().join(format!("paced-{f}.bin"))).unwrap(),
                *payload,
                "file {f} must be byte-identical"
            );
        }
    }

    /// Cross-frame per-stream consistency: a file stream that announced
    /// file 0 must not deliver chunks claiming another file — the receiver
    /// must abort with a protocol error instead of writing them (old code
    /// acked them under the first file's identity).
    #[tokio::test]
    async fn multistream_file_stream_rejects_foreign_file_index() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
        let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
        let mut recv = ReceiveSpec::new(dst.path().to_path_buf(), receiver_bundle.clone());
        recv.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        recv.idle_timeout = Duration::from_secs(5);
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver = tokio::spawn(receive_with_addr(recv, &noop, addr_tx));
        let addr = addr_rx.await.unwrap();

        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let entry = chunked_wire_entry("owned.bin", &payload, 1024);
        let receiver_fp = receiver_key.fingerprint();
        let fake = tokio::spawn(async move {
            let endpoint =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_bundle)
                    .await
                    .unwrap();
            let channel = endpoint.connect(addr, &receiver_fp).await.unwrap();
            let mut control = channel.open_bi().await.unwrap();
            let limits = engine_limits();
            control
                .send_frame(&hello_frame(
                    "smuggler",
                    &sender_bundle,
                    CAP_RESUME | CAP_MULTISTREAM,
                ))
                .await
                .unwrap();
            let _ = control.recv_frame(&limits).await.unwrap();
            let job_id = [0xB7; 16];
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Offer {
                        job_id,
                        root_name: "smuggler".to_owned(),
                        file_count: 1,
                        total_bytes: entry.size,
                        chunk_size: 1024,
                    },
                })
                .await
                .unwrap();
            let _ = control.recv_frame(&limits).await.unwrap(); // Accept
            control
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::ManifestPage {
                        job_id,
                        page_index: 0,
                        last_page: true,
                        entries: vec![entry],
                    },
                })
                .await
                .unwrap();
            let _ = control.recv_frame(&limits).await.unwrap(); // ResumeRequest

            // One stream announcing file 0, then a chunk claiming file 1.
            let mut stream = channel.open_bi().await.unwrap();
            let (c0, rest) = payload.split_at(1024);
            stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Chunk {
                        job_id,
                        file_index: 0,
                        chunk_index: 0,
                        data: c0.to_vec(),
                    },
                })
                .await
                .unwrap();
            let _ = stream.recv_frame(&limits).await.unwrap(); // ack for chunk 0
            let _ = stream
                .send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Chunk {
                        job_id,
                        file_index: 1, // foreign file on this stream
                        chunk_index: 1,
                        data: rest[..1024].to_vec(),
                    },
                })
                .await;
            // Hold briefly so the receiver's failure is not a race with a
            // dropped connection.
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let err = receiver
            .await
            .unwrap()
            .expect_err("a foreign file index on a file stream must fail the receive");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        fake.await.unwrap();
        assert!(
            !dst.path().join("owned.bin").exists(),
            "nothing may be committed"
        );
    }

    // ------------------------------------------------------------------
    // Wave 21: on-channel admission handshake + per-task staging
    // ------------------------------------------------------------------

    /// A fixed admission context for the on-channel tests: one task id,
    /// one runtime-minted permit, one code group, one registry, one
    /// pairing relation. Built through the validating constructor like
    /// production code.
    fn test_admission_ctx(task_id: [u8; 16]) -> AdmissionCtx {
        AdmissionCtx::new(task_id, [0x5C; 16], [0x77; 32], [0x91; 32], [0xB3; 16])
            .expect("test admission context is valid")
    }

    /// A fake wire peer holding one open bidirectional stream plus the
    /// objects that keep the connection alive. Used to hand-write
    /// admission frames against the real engine over real loopback QUIC.
    struct FakePeer {
        tx: DirectSendStream,
        rx: DirectRecvStream,
        _channel: DirectChannel,
        _endpoint: DirectEndpoint,
    }

    fn wire(message: Message) -> Frame {
        Frame {
            version: ak_wire::VERSION,
            message,
        }
    }

    fn now_ms_for_test() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Bind a loopback endpoint, then spawn the accept + Hello exchange
    /// (fake speaks first with `capabilities`) so the engine sender can
    /// start connecting while it runs. Returns the address to connect to,
    /// the fake's pinned fingerprint, and the join handle that yields the
    /// fake's stream halves once the Hellos are through.
    async fn fake_receiver_side(
        capabilities: u16,
    ) -> (tokio::task::JoinHandle<FakePeer>, SocketAddr, Fingerprint) {
        let key = DeviceKeyPair::generate();
        let identity = ak_identity::tls::make_self_signed(&key);
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
            .await
            .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let fingerprint = key.fingerprint();
        let task = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let stream = channel.accept_bi().await.unwrap();
            let (mut tx, mut rx) = stream.split();
            tx.send_frame(&hello_frame("fake-recv", &identity, capabilities))
                .await
                .unwrap();
            let hello = rx.recv_frame(&engine_limits()).await.unwrap();
            assert!(
                matches!(hello.message, Message::Hello { .. }),
                "engine must hello first: {:?}",
                hello.message
            );
            FakePeer {
                tx,
                rx,
                _channel: channel,
                _endpoint: endpoint,
            }
        });
        (task, addr, fingerprint)
    }

    /// Dial the engine's receiver endpoint, open the (sender-side) stream,
    /// exchange Hello, and hand the fake's halves to the caller.
    async fn fake_sender_side(
        addr: SocketAddr,
        pinned: Fingerprint,
        capabilities: u16,
    ) -> FakePeer {
        let key = DeviceKeyPair::generate();
        let identity = ak_identity::tls::make_self_signed(&key);
        let endpoint = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &identity)
            .await
            .unwrap();
        let channel = endpoint.connect(addr, &pinned).await.unwrap();
        let stream = channel.open_bi().await.unwrap();
        let (mut tx, mut rx) = stream.split();
        tx.send_frame(&hello_frame("fake-send", &identity, capabilities))
            .await
            .unwrap();
        let hello = rx.recv_frame(&engine_limits()).await.unwrap();
        assert!(
            matches!(hello.message, Message::Hello { .. }),
            "engine must hello first: {:?}",
            hello.message
        );
        FakePeer {
            tx,
            rx,
            _channel: channel,
            _endpoint: endpoint,
        }
    }

    fn on_channel_send_spec(
        src: &Path,
        peer: SocketAddr,
        pinned: Fingerprint,
        identity: TlsBundle,
    ) -> SendSpec {
        let mut spec = SendSpec::new(src.to_path_buf(), peer, pinned, identity);
        spec.checkpoint_dir = std::env::temp_dir().join(format!(
            "ak-tests-onchannel-send-{}-{}",
            std::process::id(),
            now_ms_for_test()
        ));
        spec
    }

    fn on_channel_recv_spec(dst: &Path, identity: TlsBundle) -> ReceiveSpec {
        let mut spec = ReceiveSpec::new(dst.to_path_buf(), identity);
        spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
        spec.checkpoint_dir = std::env::temp_dir().join(format!(
            "ak-tests-onchannel-recv-{}-{}",
            std::process::id(),
            now_ms_for_test()
        ));
        spec
    }

    /// Read the next TaskOpen from the fake's stream (the engine sender
    /// just produced one) through the admission-frame reader, so the
    /// wave-23 113-byte payload passes the header budget gate. Returns
    /// every field: job, permit, group, registry, pairing, direction.
    async fn read_task_open(
        fake: &mut FakePeer,
    ) -> (JobId, [u8; 16], [u8; 32], [u8; 32], [u8; 16], u8) {
        match recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(5))
            .await
            .unwrap()
            .message
        {
            Message::TaskOpen {
                job_id,
                permit_id,
                group,
                group_registry,
                pairing_id,
                direction,
            } => (
                job_id,
                permit_id,
                group,
                group_registry,
                pairing_id,
                direction,
            ),
            other => panic!("expected TaskOpen, got {other:?}"),
        }
    }

    /// Read the next Admission from the fake's stream through the
    /// admission-frame reader (the wave-23 38-byte payload).
    async fn read_admission(fake: &mut FakePeer) -> (JobId, [u8; 16], bool, AdmissionReason, u32) {
        match recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(5))
            .await
            .unwrap()
            .message
        {
            Message::Admission {
                job_id,
                permit_id,
                granted,
                reason,
                lease_ms,
            } => (job_id, permit_id, granted, reason, lease_ms),
            other => panic!("expected Admission, got {other:?}"),
        }
    }

    /// Spawn the engine sender toward `addr` with the test admission
    /// context (the caller plays the receiving wire peer).
    fn spawn_on_channel_sender(
        src: PathBuf,
        addr: SocketAddr,
        pinned: Fingerprint,
        task_id: [u8; 16],
    ) -> tokio::task::JoinHandle<Result<Report, EngineError>> {
        tokio::spawn(async move {
            let key = DeviceKeyPair::generate();
            let identity = ak_identity::tls::make_self_signed(&key);
            let spec = on_channel_send_spec(&src, addr, pinned, identity);
            let dialer =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &spec.identity)
                    .await
                    .unwrap();
            let channel = dialer.connect(spec.peer, &spec.pinned).await.unwrap();
            send_on_channel(channel, spec, test_admission_ctx(task_id), &noop).await
        })
    }

    /// After the engine sender failed, prove no further frame (no Offer,
    /// no ManifestPage, no Chunk) can still arrive: the connection is gone.
    async fn assert_wire_silent_after(fake: &mut FakePeer) {
        let observed = tokio::time::timeout(Duration::from_secs(5), async {
            let mut frames = Vec::new();
            loop {
                match fake.rx.recv_frame(&engine_limits()).await {
                    Ok(frame) => frames.push(frame.message),
                    Err(_) => break frames,
                }
            }
        })
        .await
        .expect("the closed connection must end the read loop");
        assert!(
            observed.is_empty(),
            "frames arrived after the denial: {observed:?}"
        );
    }

    /// Granted full round trip: real send_on_channel against real
    /// receive_on_channel, admission granted, file bytes identical, the
    /// decider saw exactly the wire admission context (group + registry +
    /// pairing, wave 23), the grant carried the bounded lease, and the
    /// per-task staging tree was discarded at the terminal state.
    #[tokio::test]
    async fn on_channel_admission_granted_completes_full_round_trip() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..30_000u32).map(|i| (i % 199) as u8).collect();
        write(&src.path().join("granted.bin"), &payload);

        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let receiver_fp = receiver_key.fingerprint();
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();

        let task_id: [u8; 16] = [0x42; 16];
        let ctx = test_admission_ctx(task_id);
        let seen_requests: Arc<std::sync::Mutex<Vec<AdmissionRequest>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let decider_requests = Arc::clone(&seen_requests);
        let decider = move |request: &AdmissionRequest| {
            decider_requests.lock().unwrap().push(*request);
            AdmissionDecision::Admit
        };

        let recv_spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            receive_on_channel(channel, recv_spec, &decider, &noop).await
        });

        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, receiver_fp, task_id);

        let recv_report = receiver.await.unwrap().expect("receive completes");
        let send_report = sender.await.unwrap().expect("send completes");
        assert_eq!(send_report.files, 1);
        assert_eq!(send_report.bytes, payload.len() as u64);
        assert_eq!(recv_report.bytes, payload.len() as u64);
        assert_eq!(fs::read(dst.path().join("granted.bin")).unwrap(), payload);
        // Wave 23: the decider saw the complete admission context — the
        // code group, the registry and the pairing relation the sender's
        // AdmissionCtx carried.
        assert_eq!(
            *seen_requests.lock().unwrap(),
            vec![AdmissionRequest {
                group: ctx.group,
                group_registry: ctx.group_registry,
                pairing_id: ctx.pairing_id,
            }]
        );
        // Terminal-state cleanup: the task's staging tree is gone.
        assert!(
            !ak_storage::staging_path(dst.path(), task_id).exists(),
            "staging tree must be discarded after completion"
        );
    }

    /// A SerialWait denial fails the send immediately with the dedicated
    /// error and not one post-denial frame (no Offer) is ever observable.
    #[tokio::test]
    async fn on_channel_deny_serial_wait_fails_immediately_without_offer() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"deny me");
        let task_id: [u8; 16] = [0x11; 16];

        let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
        let mut fake = fake_task.await.unwrap();

        let (job_id, permit_id, group, registry, pairing, direction) =
            read_task_open(&mut fake).await;
        assert_eq!(job_id, task_id);
        assert_eq!(permit_id, [0x5C; 16], "attempt 0 runs on the ctx permit");
        assert_eq!(group, [0x77; 32]);
        assert_eq!(registry, [0x91; 32]);
        assert_eq!(pairing, [0xB3; 16]);
        assert_eq!(direction, ak_wire::DIRECTION_SEND);
        fake.tx
            .send_frame(&wire(Message::Admission {
                job_id,
                permit_id,
                granted: false,
                reason: AdmissionReason::SerialWait,
                lease_ms: 0,
            }))
            .await
            .unwrap();

        let err = sender
            .await
            .unwrap()
            .expect_err("SerialWait denial must fail the send");
        assert!(
            matches!(
                err,
                EngineError::AdmissionDenied {
                    reason: AdmissionReason::SerialWait
                }
            ),
            "got: {err:?}"
        );
        assert_wire_silent_after(&mut fake).await;
    }

    /// A Capacity denial ends the handshake after exactly one TaskOpen
    /// (no resubmit — only StalePermit retries) and stays wire-silent.
    #[tokio::test]
    async fn on_channel_deny_capacity_is_final_and_silent() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"capacity");
        let task_id: [u8; 16] = [0x22; 16];

        let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
        let mut fake = fake_task.await.unwrap();

        let (job_id, permit_id, _, _, _, _) = read_task_open(&mut fake).await;
        fake.tx
            .send_frame(&wire(Message::Admission {
                job_id,
                permit_id,
                granted: false,
                reason: AdmissionReason::Capacity,
                lease_ms: 0,
            }))
            .await
            .unwrap();

        let err = sender.await.unwrap().expect_err("Capacity denial fails");
        assert!(
            matches!(
                err,
                EngineError::AdmissionDenied {
                    reason: AdmissionReason::Capacity
                }
            ),
            "got: {err:?}"
        );
        assert_wire_silent_after(&mut fake).await;
    }

    /// A StalePermit denial makes the sender resubmit a fresh TaskOpen
    /// (new permit id); the granted second attempt continues into the
    /// frozen path (the fake then Rejects the Offer — proving the protocol
    /// resumed after admission, and only after it).
    #[tokio::test]
    async fn on_channel_stale_permit_resubmits_fresh_task_open() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"stale permit");
        let task_id: [u8; 16] = [0x33; 16];

        let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
        let mut fake = fake_task.await.unwrap();

        // First attempt: stale.
        let (job_id, permit_a, _, _, _, _) = read_task_open(&mut fake).await;
        assert_eq!(permit_a, [0x5C; 16], "attempt 0 runs on the ctx permit");
        fake.tx
            .send_frame(&wire(Message::Admission {
                job_id,
                permit_id: permit_a,
                granted: false,
                reason: AdmissionReason::StalePermit,
                lease_ms: 0,
            }))
            .await
            .unwrap();
        // Second attempt: grant, and expect the AdmissionAck.
        let (job_id, permit_b, _, _, _, _) = read_task_open(&mut fake).await;
        assert_ne!(permit_a, permit_b, "resubmit must use a fresh permit id");
        assert_eq!(job_id, task_id);
        fake.tx
            .send_frame(&wire(Message::Admission {
                job_id,
                permit_id: permit_b,
                granted: true,
                reason: AdmissionReason::Ok,
                lease_ms: ADMISSION_LEASE_MS,
            }))
            .await
            .unwrap();
        match fake.rx.recv_frame(&engine_limits()).await.unwrap().message {
            Message::AdmissionAck {
                job_id: acked_job,
                permit_id,
            } => {
                assert_eq!(acked_job, task_id);
                assert_eq!(permit_id, permit_b);
            }
            other => panic!("expected AdmissionAck, got {other:?}"),
        }
        // The frozen path now runs: the next frame is the Offer; the fake
        // rejects it and the sender surfaces the plain rejection.
        match fake.rx.recv_frame(&engine_limits()).await.unwrap().message {
            Message::Offer { job_id, .. } => {
                fake.tx
                    .send_frame(&wire(Message::Reject {
                        job_id,
                        reason: ak_core::error::ErrorCode::TargetRevoked,
                    }))
                    .await
                    .unwrap();
            }
            other => panic!("expected Offer after admission, got {other:?}"),
        }
        let err = sender
            .await
            .unwrap()
            .expect_err("the fake rejected the offer");
        assert!(matches!(err, EngineError::Rejected(_)), "got: {err:?}");
    }

    /// A peer whose Hello lacks CAP_SCHEDULER cannot silently downgrade a
    /// scheduled send: PROTOCOL_UNSUPPORTED, before any TaskOpen is sent.
    #[tokio::test]
    async fn on_channel_peer_without_scheduler_cap_is_protocol_unsupported() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"no cap");
        let task_id: [u8; 16] = [0x44; 16];

        let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME).await;
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
        let mut fake = fake_task.await.unwrap();

        let err = sender
            .await
            .unwrap()
            .expect_err("peer without CAP_SCHEDULER must be refused");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        // No TaskOpen ever left: the fake sees the connection close with
        // nothing following its own Hello.
        assert_wire_silent_after(&mut fake).await;
    }

    /// Receiver side of a denial: the decider's reason is answered on the
    /// wire (granted = false) and the local call fails with the same
    /// reason; no receive body ran (no Offer was ever read).
    #[tokio::test]
    async fn receive_on_channel_denial_is_answered_on_the_wire() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Deny(AdmissionReason::Capacity);
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        let task_id: [u8; 16] = [0x55; 16];
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [9; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        let (job_id, permit_id, granted, reason, lease_ms) = read_admission(&mut fake).await;
        assert_eq!(job_id, task_id);
        assert_eq!(permit_id, [9; 16]);
        assert!(!granted);
        assert_eq!(reason, AdmissionReason::Capacity);
        assert_eq!(lease_ms, 0, "denials carry no lease");
        let err = receiver
            .await
            .unwrap()
            .expect_err("the decider denied the task");
        assert!(
            matches!(
                err,
                EngineError::AdmissionDenied {
                    reason: AdmissionReason::Capacity
                }
            ),
            "got: {err:?}"
        );
        // Nothing of the receive body ran: no destination file, no staging.
        assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
    }

    /// A grant is not enough: the receiver waits for the sender's
    /// AdmissionAck before the body — with it missing, the idle timeout
    /// fires (no Offer is read in the meantime).
    #[tokio::test]
    async fn receive_on_channel_grant_waits_for_admission_ack() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
        spec.idle_timeout = Duration::from_secs(1);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        let task_id: [u8; 16] = [0x66; 16];
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [4; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        let (_, _, granted, reason, lease_ms) = read_admission(&mut fake).await;
        assert!(granted);
        assert_eq!(reason, AdmissionReason::Ok);
        assert_eq!(
            lease_ms, ADMISSION_LEASE_MS,
            "grants carry the bounded lease"
        );
        // Deliberately never send the AdmissionAck.
        let err = receiver
            .await
            .unwrap()
            .expect_err("missing AdmissionAck must time out");
        assert!(
            matches!(err, EngineError::IdleTimeout(_) | EngineError::Stream(_)),
            "got: {err:?}"
        );
        assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
    }

    /// The all-zero task id is the staging sentinel: it is refused as a
    /// protocol violation, not handed to the decider or the staging layer.
    #[tokio::test]
    async fn receive_on_channel_refuses_the_zero_task_sentinel() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: [0u8; 16],
                permit_id: [1; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        let err = receiver
            .await
            .unwrap()
            .expect_err("zero task id must be refused");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // Wave 23: on-channel hardening (review P1-4)
    // ------------------------------------------------------------------

    /// AdmissionCtx::new is the controlled constructor: a valid identity
    /// set builds the context, and every all-zero field is refused with the
    /// dedicated error naming the offender — the context cannot be
    /// assembled by field assignment from outside the crate.
    #[test]
    fn admission_ctx_constructor_validates_every_identity_field() {
        let ctx = AdmissionCtx::new([1; 16], [2; 16], [3; 32], [4; 32], [5; 16])
            .expect("a fully non-zero identity set is valid");
        assert_eq!(
            ctx,
            AdmissionCtx {
                task_id: [1; 16],
                permit_id: [2; 16],
                group: [3; 32],
                group_registry: [4; 32],
                pairing_id: [5; 16],
            }
        );
        for (field, attempt) in [
            (
                "task_id",
                AdmissionCtx::new([0; 16], [2; 16], [3; 32], [4; 32], [5; 16]),
            ),
            (
                "permit_id",
                AdmissionCtx::new([1; 16], [0; 16], [3; 32], [4; 32], [5; 16]),
            ),
            (
                "group_registry",
                AdmissionCtx::new([1; 16], [2; 16], [3; 32], [0; 32], [5; 16]),
            ),
            (
                "pairing_id",
                AdmissionCtx::new([1; 16], [2; 16], [3; 32], [4; 32], [0; 16]),
            ),
        ] {
            match attempt {
                Err(EngineError::InvalidAdmissionCtx { field: named }) => {
                    assert_eq!(named, field, "the error must name the offending field");
                }
                other => panic!("all-zero {field} must be refused, got {other:?}"),
            }
        }
        // The all-zero group is deliberately NOT refused by the constructor
        // (the wave-23 contract lists task/permit/registry/pairing); the
        // receiver-side decider remains the authority over code groups.
        assert!(AdmissionCtx::new([1; 16], [2; 16], [0; 32], [4; 32], [5; 16]).is_ok());
    }

    /// A runtime-injected channel whose TLS peer is NOT the pinned
    /// fingerprint is refused as the very first step of `send_on_channel`
    /// (review P1-4): PeerMismatch, connection closed with the dedicated
    /// code, and not one protocol byte (no stream, no Hello, no TaskOpen).
    #[tokio::test]
    async fn send_on_channel_refuses_a_channel_to_the_wrong_peer() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"wrong peer");
        let task_id: [u8; 16] = [0x88; 16];

        // A real loopback peer the channel actually connects to...
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let fp = receiver_key.fingerprint();

        // ...but the spec pins a DIFFERENT device's fingerprint.
        let pinned = DeviceKeyPair::generate().fingerprint();
        let pinned_for_spec = pinned;
        let sender = tokio::spawn(async move {
            let key = DeviceKeyPair::generate();
            let identity = ak_identity::tls::make_self_signed(&key);
            let spec = on_channel_send_spec(src.path(), addr, pinned_for_spec, identity);
            let dialer =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &spec.identity)
                    .await
                    .unwrap();
            // Connect to the REAL peer so the channel itself is healthy —
            // only the pinned identity check can refuse it.
            let channel = dialer.connect(spec.peer, &fp).await.unwrap();
            send_on_channel(channel, spec, test_admission_ctx(task_id), &noop).await
        });

        // The peer observes the refusal as a closed connection on which no
        // stream was ever opened — not even the engine's Hello moved.
        let observer = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let stream = channel.accept_bi().await;
            assert!(
                stream.is_err(),
                "no stream may be opened after a pinned-identity refusal"
            );
        });

        let err = sender
            .await
            .unwrap()
            .expect_err("a channel to the wrong peer must be refused");
        match &err {
            EngineError::PeerMismatch { actual, expected } => {
                assert_eq!(actual, &fp.to_hex(), "actual peer is the connected one");
                assert_eq!(expected, &pinned.to_hex());
            }
            other => panic!("expected PeerMismatch, got {other:?}"),
        }
        observer.await.unwrap();
    }

    /// The reverse direction of the send-side refusal (the gap review
    /// P1-4 called out): `receive_on_channel` exists for scheduled tasks,
    /// so a legacy sender whose Hello lacks CAP_SCHEDULER must NOT be
    /// silently downgraded into the unadmitted receive body. It fails with
    /// PROTOCOL_UNSUPPORTED, the channel closes, and nothing is received.
    #[tokio::test]
    async fn receive_on_channel_refuses_a_sender_without_scheduler_cap() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        // A legacy sender: no CAP_SCHEDULER, straight to its Offer. Inline
        // dial (not fake_sender_side): the engine's refusal-close races the
        // fake's read of the engine's Hello, so both outcomes are accepted
        // — what the test pins down is the refusal itself, below.
        let sender_key = DeviceKeyPair::generate();
        let sender_identity = ak_identity::tls::make_self_signed(&sender_key);
        let sender_endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_identity)
                .await
                .unwrap();
        let channel = sender_endpoint
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();
        let stream = channel.open_bi().await.unwrap();
        let (mut tx, mut rx) = stream.split();
        tx.send_frame(&hello_frame("fake-send", &sender_identity, CAP_RESUME))
            .await
            .unwrap();
        let _ = rx.recv_frame(&engine_limits()).await;
        let mut fake = FakePeer {
            tx,
            rx,
            _channel: channel,
            _endpoint: sender_endpoint,
        };
        // The Offer write itself may also race the refusal-close; what
        // matters is that the engine never reads it as an unadmitted
        // receive.
        let _ = fake
            .tx
            .send_frame(&wire(Message::Offer {
                job_id: [0x9; 16],
                root_name: "legacy".into(),
                file_count: 1,
                total_bytes: 1,
                chunk_size: 1024,
            }))
            .await;

        let err = receiver
            .await
            .unwrap()
            .expect_err("a legacy sender must not be downgraded into the receive body");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        // The engine's own Hello was its last byte; the connection is gone
        // and nothing was written to the destination.
        assert_wire_silent_after(&mut fake).await;
        assert_eq!(
            fs::read_dir(dst.path()).unwrap().count(),
            0,
            "no file may land from a refused legacy sender"
        );
    }

    /// Wire-level fail-closed checks in front of the decider (wave 23): a
    /// TaskOpen with the all-zero pairing id, the all-zero registry, or a
    /// recv-direction tag is a protocol violation — the decider is never
    /// consulted, the call fails with PROTOCOL_UNSUPPORTED and no staging
    /// appears. Mirrors `AdmissionCtx::new` on the sending side.
    #[tokio::test]
    async fn receive_on_channel_refuses_sentinel_and_wrong_direction_task_opens() {
        for case in ["zero pairing id", "zero registry", "recv direction"] {
            let dst = tempfile::tempdir().unwrap();
            let receiver_key = DeviceKeyPair::generate();
            let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
            let endpoint =
                DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                    .await
                    .unwrap();
            let addr = endpoint.local_addr().unwrap();
            let spec = on_channel_recv_spec(dst.path(), receiver_identity);
            let consulted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = Arc::clone(&consulted);
            let receiver = tokio::spawn(async move {
                let channel = endpoint.accept(&|_| true).await.unwrap();
                let decider = move |_: &AdmissionRequest| {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    AdmissionDecision::Admit
                };
                receive_on_channel(channel, spec, &decider, &noop).await
            });

            let mut fake =
                fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER)
                    .await;
            let task_id: [u8; 16] = [0x99; 16];
            fake.tx
                .send_frame(&wire(Message::TaskOpen {
                    job_id: task_id,
                    permit_id: [7; 16],
                    group: [0x77; 32],
                    group_registry: match case {
                        "zero registry" => [0u8; 32],
                        _ => [0x91; 32],
                    },
                    pairing_id: match case {
                        "zero pairing id" => [0u8; 16],
                        _ => [0xB3; 16],
                    },
                    direction: match case {
                        "recv direction" => ak_wire::DIRECTION_RECV,
                        _ => ak_wire::DIRECTION_SEND,
                    },
                }))
                .await
                .unwrap();

            let err = receiver.await.unwrap().unwrap_err();
            assert!(
                matches!(
                    &err,
                    EngineError::PeerError {
                        code: ErrorCode::ProtocolUnsupported,
                        ..
                    }
                ),
                "{case}: got {err:?}"
            );
            assert!(
                !consulted.load(std::sync::atomic::Ordering::SeqCst),
                "{case}: the decider must not be consulted"
            );
            assert!(
                !ak_storage::staging_path(dst.path(), task_id).exists(),
                "{case}: no staging may appear"
            );
        }
    }

    /// A wave-20-length TaskOpen (96-byte payload, before pairing_id and
    /// direction existed) is rejected on a real channel — the same-type
    /// shorter frame runs out mid-field; the two versions do not
    /// interoperate (both ends must ship the same version).
    #[tokio::test]
    async fn receive_on_channel_refuses_wave_20_length_task_open() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec, &decider, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        // Hand-crafted wave-20 frame: job(16) + permit(16) + group(32)
        // + registry(32) = 96 payload bytes, no pairing_id/direction.
        let task_id: [u8; 16] = [0xAA; 16];
        let mut payload = Vec::new();
        payload.extend_from_slice(&task_id);
        payload.extend_from_slice(&[7u8; 16]);
        payload.extend_from_slice(&[0x77u8; 32]);
        payload.extend_from_slice(&[0x91u8; 32]);
        assert_eq!(payload.len(), 96);
        let mut bytes = vec![ak_wire::VERSION];
        bytes.extend_from_slice(&ak_wire::TYPE_TASK_OPEN.to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&payload);
        fake.tx.send_bytes(&bytes).await.unwrap();

        let err = receiver
            .await
            .unwrap()
            .expect_err("the wave-20 frame must be rejected");
        assert!(
            matches!(err, EngineError::Stream(_)),
            "decode must reject the stale length, got {err:?}"
        );
        assert!(!ak_storage::staging_path(dst.path(), task_id).exists());
    }

    /// A grant must carry a positive lease (wave 23): the sender refuses a
    /// granted Admission with lease_ms = 0 as a protocol violation and
    /// stays wire-silent afterwards.
    #[tokio::test]
    async fn on_channel_granted_admission_with_zero_lease_is_refused() {
        let src = tempfile::tempdir().unwrap();
        write(&src.path().join("f.bin"), b"no lease");
        let task_id: [u8; 16] = [0xBB; 16];

        let (fake_task, addr, fp) = fake_receiver_side(CAP_RESUME | CAP_SCHEDULER).await;
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, fp, task_id);
        let mut fake = fake_task.await.unwrap();

        let (job_id, permit_id, _, _, _, _) = read_task_open(&mut fake).await;
        fake.tx
            .send_frame(&wire(Message::Admission {
                job_id,
                permit_id,
                granted: true,
                reason: AdmissionReason::Ok,
                lease_ms: 0,
            }))
            .await
            .unwrap();

        let err = sender
            .await
            .unwrap()
            .expect_err("a granted admission with a zero lease must be refused");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        // No AdmissionAck, no Offer: the handshake failed closed.
        assert_wire_silent_after(&mut fake).await;
    }

    /// Hand-written full send protocol against receive_on_channel with a
    /// poisoned chunk digest: the verified part file must live inside the
    /// task's staging tree (not beside the final name), and a retry of the
    /// same task id resumes from it (verified prefix 1) and completes with
    /// the tree discarded.
    #[tokio::test]
    async fn scheduled_receive_stages_parts_and_resumes_within_the_task_tree() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let pinned = receiver_key.fingerprint();

        const CHUNK: usize = 4096;
        let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
        let (a, b) = payload.split_at(CHUNK);
        let digest = |bytes: &[u8]| {
            let mut out = [0u8; 32];
            out.copy_from_slice(blake3::hash(bytes).as_bytes());
            out
        };
        let whole = digest(&payload);
        let chunk_digests = vec![digest(a), digest(b)];
        let task_id: [u8; 16] = [0xAB; 16];
        let limits = engine_limits();

        // ---- run 1: correct chunk 0, manifest poisons chunk 1's digest.
        let mut spec1 = on_channel_recv_spec(dst.path(), receiver_identity.clone());
        spec1.idle_timeout = Duration::from_secs(5);
        let receiver1 = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec1, &decider, &noop).await
        });
        let mut fake = fake_sender_side(addr, pinned, CAP_RESUME | CAP_SCHEDULER).await;
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [2; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        let (_, _, granted, _, _) = read_admission(&mut fake).await;
        assert!(granted);
        fake.tx
            .send_frame(&wire(Message::AdmissionAck {
                job_id: task_id,
                permit_id: [2; 16],
            }))
            .await
            .unwrap();
        let job_id: JobId = [0xCD; 16];
        fake.tx
            .send_frame(&wire(Message::Offer {
                job_id,
                root_name: "poison".into(),
                file_count: 1,
                total_bytes: payload.len() as u64,
                chunk_size: CHUNK as u32,
            }))
            .await
            .unwrap();
        match fake.rx.recv_frame(&limits).await.unwrap().message {
            Message::Accept { job_id: acked } => assert_eq!(acked, job_id),
            other => panic!("expected Accept, got {other:?}"),
        }
        let entry = ak_core::manifest::FileEntry::with_digest_alg(
            "staged.bin",
            payload.len() as u64,
            1_700_000_000_000,
            ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole,
            chunk_digests.clone(),
        )
        .unwrap();
        let mut wire_entry = WireEntry::from(&entry);
        wire_entry.chunk_digests[1] = [0xEE; 32]; // poisoned
        fake.tx
            .send_frame(&wire(Message::ManifestPage {
                job_id,
                page_index: 0,
                last_page: true,
                entries: vec![wire_entry],
            }))
            .await
            .unwrap();
        match fake.rx.recv_frame(&limits).await.unwrap().message {
            Message::ResumeRequest { files, more, .. } => {
                assert!(!more);
                assert_eq!(files[0].verified_prefix_chunks, 0, "fresh task");
            }
            other => panic!("expected ResumeRequest, got {other:?}"),
        }
        for (index, data) in [(0u64, a), (1u64, b)] {
            fake.tx
                .send_frame(&wire(Message::Chunk {
                    job_id,
                    file_index: 0,
                    chunk_index: index,
                    data: data.to_vec(),
                }))
                .await
                .unwrap();
        }
        let err = receiver1
            .await
            .unwrap()
            .expect_err("poisoned chunk digest must fail the receive");
        assert!(
            !matches!(err, EngineError::AdmissionDenied { .. }),
            "admission itself succeeded: {err:?}"
        );

        // Staging landing point: chunk 0 verified inside the task's tree.
        let staged_part = ak_storage::staging_path(dst.path(), task_id).join("staged.bin.akpart");
        assert!(
            staged_part.exists(),
            "verified part must live in the task staging tree"
        );
        assert_eq!(
            fs::metadata(&staged_part).unwrap().len(),
            CHUNK as u64,
            "exactly the verified prefix (chunk 0) survived"
        );
        assert!(
            !dst.path().join("staged.bin").exists(),
            "nothing committed beside the final name"
        );
        assert!(
            !dst.path().join("staged.bin.akpart").exists(),
            "no legacy same-directory part file may appear for a scheduled task"
        );

        // ---- run 2: same task id, corrected digest, resume + completion.
        // Run 1's endpoint moved into its spawn, so run 2 accepts on a
        // fresh endpoint bound to a fresh port (same destination dir, which
        // is what carries the staged prefix).
        let receiver_key2 = DeviceKeyPair::generate();
        let receiver_identity2 = ak_identity::tls::make_self_signed(&receiver_key2);
        let endpoint2 =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity2)
                .await
                .unwrap();
        let addr2 = endpoint2.local_addr().unwrap();
        let mut spec2 = on_channel_recv_spec(dst.path(), receiver_identity2);
        spec2.idle_timeout = Duration::from_secs(5);
        let receiver2 = tokio::spawn(async move {
            let channel = endpoint2.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel(channel, spec2, &decider, &noop).await
        });

        let mut fake2 = fake_sender_side(
            addr2,
            receiver_key2.fingerprint(),
            CAP_RESUME | CAP_SCHEDULER,
        )
        .await;
        fake2
            .tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [3; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        let (_, _, granted, _, _) = read_admission(&mut fake2).await;
        assert!(granted);
        fake2
            .tx
            .send_frame(&wire(Message::AdmissionAck {
                job_id: task_id,
                permit_id: [3; 16],
            }))
            .await
            .unwrap();
        fake2
            .tx
            .send_frame(&wire(Message::Offer {
                job_id,
                root_name: "poison".into(),
                file_count: 1,
                total_bytes: payload.len() as u64,
                chunk_size: CHUNK as u32,
            }))
            .await
            .unwrap();
        match fake2.rx.recv_frame(&limits).await.unwrap().message {
            Message::Accept { job_id: acked } => assert_eq!(acked, job_id),
            other => panic!("expected Accept, got {other:?}"),
        }
        fake2
            .tx
            .send_frame(&wire(Message::ManifestPage {
                job_id,
                page_index: 0,
                last_page: true,
                entries: vec![WireEntry::from(&entry)],
            }))
            .await
            .unwrap();
        match fake2.rx.recv_frame(&limits).await.unwrap().message {
            Message::ResumeRequest { files, more, .. } => {
                assert!(!more);
                assert_eq!(
                    files[0].verified_prefix_chunks, 1,
                    "retry of the same task id must resume from the staged prefix"
                );
            }
            other => panic!("expected ResumeRequest, got {other:?}"),
        }
        fake2
            .tx
            .send_frame(&wire(Message::Chunk {
                job_id,
                file_index: 0,
                chunk_index: 1,
                data: b.to_vec(),
            }))
            .await
            .unwrap();
        match fake2.rx.recv_frame(&limits).await.unwrap().message {
            Message::ChunkAck {
                job_id: acked,
                file_index,
                chunk_index,
            } => {
                assert_eq!(acked, job_id);
                assert_eq!(file_index, 0);
                assert_eq!(chunk_index, 1);
            }
            other => panic!("expected ChunkAck, got {other:?}"),
        }
        fake2
            .tx
            .send_frame(&wire(Message::Finalize { job_id }))
            .await
            .unwrap();
        match fake2.rx.recv_frame(&limits).await.unwrap().message {
            Message::FinalAck {
                job_id: acked,
                verified,
            } => {
                assert_eq!(acked, job_id);
                assert!(verified);
            }
            other => panic!("expected FinalAck, got {other:?}"),
        }
        let report = receiver2.await.unwrap().expect("run 2 completes");
        assert_eq!(report.bytes, payload.len() as u64);
        assert_eq!(report.resumed_chunks, 1, "one chunk was resumed");
        assert_eq!(fs::read(dst.path().join("staged.bin")).unwrap(), payload);
        assert!(
            !ak_storage::staging_path(dst.path(), task_id).exists(),
            "terminal state discards the task's staging tree"
        );
    }

    // ------------------------------------------------------------------
    // Wave 28: receive-side transfer consent (TransferConsent hook)
    // ------------------------------------------------------------------

    /// One scheduled receive driven by the hand-written fake up to (and
    /// just past) the Offer. Holds the live channel so the connection
    /// outlives the harness unless the receiver itself ends it.
    struct ConsentHarness {
        receiver: tokio::task::JoinHandle<Result<Report, EngineError>>,
        tx: Option<DirectSendStream>,
        rx: Option<DirectRecvStream>,
        _channel: DirectChannel,
        _endpoint: DirectEndpoint,
        job_id: JobId,
        task_id: [u8; 16],
        dst: tempfile::TempDir,
        checkpoint_dir: PathBuf,
        _ckpt_keepalive: tempfile::TempDir,
        /// Set when `early_reader` was requested: a task blocked on the
        /// stream BEFORE the Offer moved, collecting every frame the
        /// receiver sends until the stream ends. This is the only
        /// deterministic way to observe a refusal frame — the receiver
        /// closes the connection right after sending it, so a reader that
        /// starts afterwards races the CONNECTION_CLOSE and loses.
        frames: Option<tokio::task::JoinHandle<Vec<Message>>>,
    }

    impl ConsentHarness {
        async fn send(&mut self, message: Message) {
            self.tx
                .as_mut()
                .expect("harness tx")
                .send_frame(&wire(message))
                .await
                .unwrap();
        }

        /// Next receiver frame, skipping the receiver's Hello (identity
        /// bearing, irrelevant to the frame comparisons here).
        async fn next_frame(&mut self) -> Message {
            let rx = self.rx.as_mut().expect("harness rx");
            let frame = rx.recv_frame(&engine_limits()).await.unwrap();
            match frame.message {
                Message::Hello { .. } => rx.recv_frame(&engine_limits()).await.unwrap().message,
                message => message,
            }
        }
    }

    /// Bind a receiver running `receive_on_channel_consent` with an
    /// always-admitting decider, exchange Hello/TaskOpen/Admission/Ack and
    /// send the Offer. `consent` is the hook under test; `early_reader`
    /// additionally parks a frame reader on the stream before the Offer
    /// (see [`ConsentHarness::frames`]).
    async fn consent_harness(
        consent: Option<TransferConsent>,
        offered_files: u64,
        offered_bytes: u64,
        early_reader: bool,
    ) -> ConsentHarness {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
        spec.idle_timeout = Duration::from_secs(5);
        // Isolated checkpoint dir: the shared helper's millisecond-stamped
        // path collides between parallel tests, and the zero-disk-write
        // assertion below must see only this run's writes.
        let ckpt = tempfile::tempdir().unwrap();
        spec.checkpoint_dir = ckpt.path().to_path_buf();
        let checkpoint_dir = spec.checkpoint_dir.clone();
        let receiver = tokio::spawn(async move {
            let channel = endpoint.accept(&|_| true).await.unwrap();
            let decider = |_: &AdmissionRequest| AdmissionDecision::Admit;
            receive_on_channel_consent(channel, spec, &decider, &noop, consent).await
        });
        let fake = fake_sender_side(addr, receiver_key.fingerprint(), CAP_SCHEDULER).await;
        let FakePeer {
            tx,
            mut rx,
            _channel,
            _endpoint,
        } = fake;
        let mut tx = tx;
        let task_id: [u8; 16] = [0x5C; 16];
        tx.send_frame(&wire(Message::TaskOpen {
            job_id: task_id,
            permit_id: [2; 16],
            group: [0x77; 32],
            group_registry: [0x91; 32],
            pairing_id: [0xB3; 16],
            direction: ak_wire::DIRECTION_SEND,
        }))
        .await
        .unwrap();
        // Admission (granted) — read through the admission-frame reader.
        match recv_admission_frame(&mut rx, &engine_limits(), Duration::from_secs(5))
            .await
            .unwrap()
            .message
        {
            Message::Admission { granted, .. } => {
                assert!(granted, "the admission decider always admits here")
            }
            other => panic!("expected Admission, got {other:?}"),
        }
        tx.send_frame(&wire(Message::AdmissionAck {
            job_id: task_id,
            permit_id: [2; 16],
        }))
        .await
        .unwrap();
        let job_id: JobId = [0xCD; 16];
        let (frames, rx) = if early_reader {
            (
                Some(tokio::spawn(async move {
                    let mut seen = Vec::new();
                    while let Ok(frame) = rx.recv_frame(&engine_limits()).await {
                        seen.push(frame.message);
                    }
                    seen
                })),
                None,
            )
        } else {
            (None, Some(rx))
        };
        tx.send_frame(&wire(Message::Offer {
            job_id,
            root_name: "consent-demo".into(),
            file_count: offered_files,
            total_bytes: offered_bytes,
            chunk_size: 4096,
        }))
        .await
        .unwrap();
        ConsentHarness {
            receiver,
            tx: Some(tx),
            rx,
            _channel,
            _endpoint,
            job_id,
            task_id,
            dst,
            checkpoint_dir,
            _ckpt_keepalive: ckpt,
            frames,
        }
    }

    /// A declining consent must: answer exactly one DECLINED error frame
    /// (existing Error-frame semantics — the closed code set maps DECLINED
    /// onto LocalPermissionDenied with the frozen message), never send an
    /// Accept, fail locally with `ConsentDeclined`, close the connection,
    /// and leave nothing on disk (no part file, no final file, no
    /// checkpoint record).
    #[tokio::test]
    async fn consent_decline_sends_declined_error_and_writes_nothing() {
        let harness = consent_harness(
            Some(Arc::new(|_: &ConsentInfo| ConsentAnswer::Now(false))),
            2,
            4096,
            true,
        )
        .await;

        // The wire answer, observed by the early reader: exactly one
        // DECLINED error frame and then the connection ends.
        let frames = harness.frames.expect("early reader").await.unwrap();
        assert_eq!(
            frames,
            vec![Message::Error {
                code: DECLINED_WIRE_CODE,
                message: DECLINED_MESSAGE.to_string(),
            }],
            "the DECLINED frame and nothing else (no Accept)"
        );

        let err = harness
            .receiver
            .await
            .unwrap()
            .expect_err("the declining consent must fail the receive");
        match err {
            EngineError::ConsentDeclined { job_id } => assert_eq!(job_id, harness.job_id),
            other => panic!("expected ConsentDeclined, got {other:?}"),
        }

        // Zero disk writes: the destination holds no part file and no
        // final file (staged and legacy landing points both checked), and
        // the checkpoint store wrote no record.
        assert!(
            harness.dst.path().read_dir().unwrap().next().is_none(),
            "destination must stay empty"
        );
        assert!(
            !ak_storage::staging_path(harness.dst.path(), harness.task_id).exists(),
            "no per-task staging tree"
        );
        let records = harness
            .checkpoint_dir
            .read_dir()
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert_eq!(records, 0, "no checkpoint record may exist");
    }

    /// An accepting consent sees exactly the offered summary (job id, root
    /// name, file count, total bytes) — the numbers the UI prompt shows.
    #[tokio::test]
    async fn consent_callback_receives_the_offered_summary() {
        let seen = Arc::new(std::sync::Mutex::new(None::<ConsentInfo>));
        let sink = Arc::clone(&seen);
        let mut harness = consent_harness(
            Some(Arc::new(move |info: &ConsentInfo| {
                *sink.lock().unwrap() = Some(info.clone());
                ConsentAnswer::Now(true)
            })),
            2,
            12_345,
            false,
        )
        .await;
        // The consent accepted: the next frame is the plain Accept.
        match harness.next_frame().await {
            Message::Accept { job_id } => assert_eq!(job_id, harness.job_id),
            other => panic!("an accepting consent must Accept, got {other:?}"),
        }
        let info = seen.lock().unwrap().clone().expect("callback ran");
        assert_eq!(info.job_id, harness.job_id);
        assert_eq!(info.root_name, "consent-demo");
        assert_eq!(info.file_count, 2);
        assert_eq!(info.total_bytes, 12_345);
        // The receiver now waits for the manifest; hang the fake up and
        // collect the failure — anything but ConsentDeclined.
        harness.tx = None;
        drop(harness.rx.take());
        let outcome = harness.receiver.await.unwrap();
        assert!(
            !matches!(outcome, Err(EngineError::ConsentDeclined { .. })),
            "accepting consent must not decline: {outcome:?}"
        );
    }

    /// The non-blocking shape (wave 29, review P2-07): a
    /// `ConsentAnswer::Pending` parks the receive at the Offer point while
    /// the callback itself already returned — the decision arrives through
    /// the oneshot — and a decider that vanishes (every sender half
    /// dropped) fails closed as a declination.
    #[tokio::test]
    async fn consent_pending_resolves_asynchronously_and_fails_closed() {
        // Resolved-accept leg: the receiver stays parked at the Offer until
        // the out-of-band decision arrives.
        let (decide_tx, decide_rx) = tokio::sync::oneshot::channel::<bool>();
        let parked = std::sync::Mutex::new(Some(decide_rx));
        let mut harness = consent_harness(
            Some(Arc::new(move |_: &ConsentInfo| {
                match parked.lock().unwrap().take() {
                    Some(pending) => ConsentAnswer::Pending(pending),
                    None => ConsentAnswer::Now(false),
                }
            })),
            1,
            10,
            false,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        decide_tx.send(true).unwrap();
        match harness.next_frame().await {
            Message::Accept { job_id } => assert_eq!(job_id, harness.job_id),
            other => panic!("a resolved-accept pending consent must Accept, got {other:?}"),
        }

        // Fail-closed leg: every decider drops without answering.
        let (gone_tx, gone_rx) = tokio::sync::oneshot::channel::<bool>();
        drop(gone_tx);
        let vanished = std::sync::Mutex::new(Some(gone_rx));
        let harness = consent_harness(
            Some(Arc::new(move |_: &ConsentInfo| {
                match vanished.lock().unwrap().take() {
                    Some(pending) => ConsentAnswer::Pending(pending),
                    None => ConsentAnswer::Now(false),
                }
            })),
            1,
            10,
            false,
        )
        .await;
        let outcome = harness.receiver.await.unwrap();
        assert!(
            matches!(outcome, Err(EngineError::ConsentDeclined { .. })),
            "a vanished decider must fail closed as a declination: {outcome:?}"
        );
    }

    /// `Some(always-true)` must be byte-for-byte the `None` path: the
    /// receiver's wire frames (after the identity-bearing Hello) and the
    /// final report of one complete transfer are identical between the
    /// frozen entry and the consented entry.
    #[tokio::test]
    async fn consent_accept_path_is_byte_identical_to_the_default() {
        const CHUNK: usize = 4096;
        let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
        let digest = |bytes: &[u8]| {
            let mut out = [0u8; 32];
            out.copy_from_slice(blake3::hash(bytes).as_bytes());
            out
        };
        let (a, b) = payload.split_at(CHUNK);
        let entry = ak_core::manifest::FileEntry::with_digest_alg(
            "consent-identical.bin",
            payload.len() as u64,
            1_700_000_000_000,
            ak_core::manifest::DIGEST_ALG_BLAKE3,
            digest(&payload),
            vec![digest(a), digest(b)],
        )
        .unwrap();

        /// One complete hand-written scheduled transfer; collects every
        /// receiver frame after its Hello plus the final report.
        async fn one_run(
            consent: Option<TransferConsent>,
            payload: &[u8],
            entry: &ak_core::manifest::FileEntry,
        ) -> (Vec<Message>, Report) {
            let mut harness = consent_harness(consent, 1, payload.len() as u64, false).await;
            let mut frames = Vec::new();
            frames.push(harness.next_frame().await); // Accept
            harness
                .send(Message::ManifestPage {
                    job_id: harness.job_id,
                    page_index: 0,
                    last_page: true,
                    entries: vec![WireEntry::from(entry)],
                })
                .await;
            frames.push(harness.next_frame().await); // ResumeRequest
            for (index, data) in [(0u64, &payload[..CHUNK]), (1u64, &payload[CHUNK..])] {
                harness
                    .send(Message::Chunk {
                        job_id: harness.job_id,
                        file_index: 0,
                        chunk_index: index,
                        data: data.to_vec(),
                    })
                    .await;
                frames.push(harness.next_frame().await); // ChunkAck
            }
            harness
                .send(Message::Finalize {
                    job_id: harness.job_id,
                })
                .await;
            frames.push(harness.next_frame().await); // FinalAck
            let report = harness.receiver.await.unwrap().expect("run completes");
            (frames, report)
        }

        let (plain_frames, plain_report) = one_run(None, &payload, &entry).await;
        let consent = Some(Arc::new(|_: &ConsentInfo| ConsentAnswer::Now(true)) as TransferConsent);
        let (consent_frames, consent_report) = one_run(consent, &payload, &entry).await;

        // Every protocol frame after the Hello is identical.
        assert_eq!(
            plain_frames, consent_frames,
            "an accepting consent changes nothing on the wire"
        );
        assert_eq!(
            plain_frames.len(),
            5,
            "Accept, ResumeRequest, 2x ChunkAck, FinalAck"
        );
        // The reports agree on everything deterministic (duration and the
        // peer fingerprint are per-run by construction).
        assert_eq!(plain_report.job_id, consent_report.job_id);
        assert_eq!(plain_report.files, consent_report.files);
        assert_eq!(plain_report.bytes, consent_report.bytes);
        assert_eq!(plain_report.resumed_chunks, consent_report.resumed_chunks);
        assert_eq!(plain_report.roots, consent_report.roots);
    }

    // ------------------------------------------------------------------
    // Wave 27: daemon scheduler handshake + pre-admitted receive
    // ------------------------------------------------------------------

    /// Spawn the daemon head for the wave-27 tests: accept one connection
    /// and run `channel_scheduler_handshake`, yielding the (still
    /// caller-owned) channel next to the handshake result.
    fn spawn_daemon_handshake(
        endpoint: DirectEndpoint,
        spec: ReceiveSpec,
    ) -> tokio::task::JoinHandle<Result<(SchedulerHandshake, DirectChannel), EngineError>> {
        tokio::spawn(async move {
            let mut channel = endpoint.accept(&|_| true).await.unwrap();
            let handshake = channel_scheduler_handshake(&mut channel, &spec).await?;
            Ok((handshake, channel))
        })
    }

    /// A CAP_SCHEDULER peer's TaskOpen comes back through
    /// `SchedulerHandshake::Scheduler` with every admission field byte
    /// for byte what the peer put on the wire (wave 27: the daemon gets
    /// the raw fields to bind the code group BEFORE admission answers).
    #[tokio::test]
    async fn scheduler_handshake_reads_a_scheduler_peers_task_open_byte_exact() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);

        let daemon = spawn_daemon_handshake(endpoint, spec);
        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        let task_id: [u8; 16] = [0x42; 16];
        let permit: [u8; 16] = [0x5C; 16];
        let group: [u8; 32] = [0x77; 32];
        let registry: [u8; 32] = [0x91; 32];
        let pairing: [u8; 16] = [0xB3; 16];
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: permit,
                group,
                group_registry: registry,
                pairing_id: pairing,
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();

        let (handshake, _channel) = daemon.await.unwrap().expect("handshake completes");
        match handshake {
            SchedulerHandshake::Scheduler { open } => {
                assert_eq!(open.job_id(), task_id, "job_id byte exact");
                assert_eq!(open.permit_id(), permit, "permit_id byte exact");
                assert_eq!(open.group(), group, "group byte exact");
                assert_eq!(open.group_registry(), registry, "group_registry byte exact");
                assert_eq!(open.pairing_id(), pairing, "pairing_id byte exact");
                assert_eq!(open.direction(), ak_wire::DIRECTION_SEND);
                // The convenience view carries the same pairing fields.
                assert_eq!(
                    open.admission_request(),
                    AdmissionRequest {
                        group,
                        group_registry: registry,
                        pairing_id: pairing,
                    }
                );
            }
            SchedulerHandshake::Legacy { .. } => {
                panic!("a CAP_SCHEDULER peer must take the Scheduler arm")
            }
        }
        // The handshake did NOT answer admission: nothing of the
        // Admission/Ack exchange may have moved yet.
        if let Ok(Ok(frame)) = tokio::time::timeout(
            Duration::from_secs(1),
            recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_millis(200)),
        )
        .await
        {
            panic!("handshake must stay silent, saw {:?}", frame.message);
        }
    }

    /// A peer whose Hello lacks CAP_SCHEDULER takes the Legacy arm with
    /// the peer's capability bits — and no TaskOpen is ever read or
    /// expected from it.
    #[tokio::test]
    async fn scheduler_handshake_returns_legacy_for_a_legacy_peer() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);

        let daemon = spawn_daemon_handshake(endpoint, spec);
        // Legacy sender: no CAP_SCHEDULER bit, no TaskOpen — it would
        // proceed straight to its Offer.
        let mut fake = fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME).await;

        let (handshake, _channel) = daemon.await.unwrap().expect("handshake completes");
        match handshake {
            SchedulerHandshake::Legacy {
                peer_capabilities,
                stream,
            } => {
                assert_eq!(peer_capabilities, CAP_RESUME);
                // The Hello-exchanged stream rides along (wave 27
                // integrator fix): dropping it here is exactly what the
                // daemon must NOT do — receive_after_hello consumes it.
                drop(stream);
            }
            SchedulerHandshake::Scheduler { .. } => {
                panic!("a legacy peer must take the Legacy arm")
            }
        }
        // Nothing of the admission exchange may have been answered and no
        // file protocol ran on this channel from the engine side.
        if let Ok(Ok(frame)) = tokio::time::timeout(
            Duration::from_secs(1),
            recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_millis(200)),
        )
        .await
        {
            panic!("legacy arm must stay silent, saw {:?}", frame.message);
        }
    }

    /// The handshake re-runs the receive path's authorize check: a
    /// channel whose fingerprint the callback refuses fails with the
    /// accept path's `ConnectError::Unauthorized` semantics.
    #[tokio::test]
    async fn scheduler_handshake_refuses_an_unauthorized_peer() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let mut spec = on_channel_recv_spec(dst.path(), receiver_identity);
        spec.authorize = Box::new(|_| false);

        let daemon = spawn_daemon_handshake(endpoint, spec);
        // The peer dials and connects; the refusal happens on the entry's
        // identity re-check — before any stream or Hello, so the dial
        // itself succeeds and no Hello exchange is attempted (the
        // connection is closed under the peer right after).
        let sender_key = DeviceKeyPair::generate();
        let sender_identity = ak_identity::tls::make_self_signed(&sender_key);
        let dialer = DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &sender_identity)
            .await
            .unwrap();
        let _channel = dialer
            .connect(addr, &receiver_key.fingerprint())
            .await
            .unwrap();

        let err = match daemon.await.unwrap() {
            // DirectChannel is not Debug, so no expect_err here.
            Err(error) => error,
            Ok(_) => panic!("the refused fingerprint must fail the handshake"),
        };
        assert!(
            matches!(
                err,
                EngineError::Connect(ak_net::ConnectError::Unauthorized)
            ),
            "got: {err:?}"
        );
    }

    /// Full daemon round trip (wave 27): accept → handshake → upper-layer
    /// group binding → pre-admitted receive, against a real
    /// `send_on_channel` sender. An 8 KiB file moves, the granted
    /// Admission + AdmissionAck exchange completes, the bytes land
    /// identical and the per-task staging tree is discarded at the
    /// terminal state.
    #[tokio::test]
    async fn preadmitted_round_trip_completes_an_8kib_transfer() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 241) as u8).collect();
        write(&src.path().join("round.bin"), &payload);

        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let pinned = receiver_key.fingerprint();
        let recv_spec = on_channel_recv_spec(dst.path(), receiver_identity);
        let seen: Arc<std::sync::Mutex<Vec<Progress>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let events = Arc::clone(&seen);

        let receiver = tokio::spawn(async move {
            let mut channel = endpoint.accept(&|_| true).await.unwrap();
            let open = match channel_scheduler_handshake(&mut channel, &recv_spec)
                .await
                .unwrap()
            {
                SchedulerHandshake::Scheduler { open } => open,
                SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
            };
            // Upper-layer analog (ak-session): bind the retained fields
            // to the code group before admitting.
            assert_eq!(
                open.admission_request(),
                AdmissionRequest {
                    group: [0x77; 32],
                    group_registry: [0x91; 32],
                    pairing_id: [0xB3; 16],
                }
            );
            let progress = move |event: Progress| events.lock().unwrap().push(event);
            receive_on_channel_preadmitted(channel, recv_spec, open, &progress).await
        });

        let task_id: [u8; 16] = [0xD1; 16];
        let sender = spawn_on_channel_sender(src.path().to_path_buf(), addr, pinned, task_id);

        let recv_report = receiver
            .await
            .unwrap()
            .expect("preadmitted receive completes");
        let send_report = sender.await.unwrap().expect("send completes");
        assert_eq!(send_report.files, 1);
        assert_eq!(send_report.bytes, payload.len() as u64);
        assert_eq!(recv_report.bytes, payload.len() as u64);
        assert_eq!(fs::read(dst.path().join("round.bin")).unwrap(), payload);
        assert!(
            !ak_storage::staging_path(dst.path(), task_id).exists(),
            "terminal state discards the task's staging tree"
        );
        // The transfer entry reported the connection phase (Connected
        // names the peer the channel verified — the sender device).
        let events = seen.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Progress::Connected { .. })),
            "the pre-admitted entry reports Connected: {events:?}"
        );
        assert!(events.contains(&Progress::Done));
    }

    /// Per-task staging isolation across the daemon path: a task whose
    /// receive fails mid-transfer keeps its verified part in ITS staging
    /// tree (keyed by its own job_id), and a second task admitted right
    /// after — through the same handshake/preadmitted pair — completes,
    /// cleans up only its own tree and leaves the first task's staged
    /// prefix untouched.
    #[tokio::test]
    async fn preadmitted_stages_isolate_by_job_id_and_cleanup_is_per_task() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let pinned = receiver_key.fingerprint();

        const CHUNK: usize = 4096;
        let payload: Vec<u8> = (0..(CHUNK * 2) as u32).map(|i| (i % 251) as u8).collect();
        let (a, b) = payload.split_at(CHUNK);
        let digest = |bytes: &[u8]| {
            let mut out = [0u8; 32];
            out.copy_from_slice(blake3::hash(bytes).as_bytes());
            out
        };
        let whole = digest(&payload);
        let chunk_digests = vec![digest(a), digest(b)];
        let limits = engine_limits();

        // ---- task A (hand-driven): admitted through the daemon pair,
        // first chunk verifies, second digest is poisoned → the receive
        // fails with the verified prefix left in A's staging tree.
        let task_a: [u8; 16] = [0xC1; 16];
        let mut spec_a = on_channel_recv_spec(dst.path(), receiver_identity.clone());
        spec_a.idle_timeout = Duration::from_secs(5);
        let receiver_a = tokio::spawn(async move {
            let mut channel = endpoint.accept(&|_| true).await.expect("task A connects");
            let open = match channel_scheduler_handshake(&mut channel, &spec_a)
                .await
                .unwrap()
            {
                SchedulerHandshake::Scheduler { open } => open,
                SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
            };
            receive_on_channel_preadmitted(channel, spec_a, open, &noop).await
        });
        let mut fake_a = fake_sender_side(addr, pinned, CAP_RESUME | CAP_SCHEDULER).await;
        fake_a
            .tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_a,
                permit_id: [2; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_SEND,
            }))
            .await
            .unwrap();
        // The pre-admitted entry answers the granted admission (bounded
        // lease) and waits for the ack — the daemon flow, split in two.
        let (granted_job, granted_permit, granted, reason, lease_ms) =
            read_admission(&mut fake_a).await;
        assert_eq!(granted_job, task_a);
        assert_eq!(granted_permit, [2; 16]);
        assert!(granted);
        assert_eq!(reason, AdmissionReason::Ok);
        assert_eq!(
            lease_ms, ADMISSION_LEASE_MS,
            "grant carries the bounded lease"
        );
        fake_a
            .tx
            .send_frame(&wire(Message::AdmissionAck {
                job_id: task_a,
                permit_id: [2; 16],
            }))
            .await
            .unwrap();

        // Frozen receive body, hand-written: poisoned chunk-1 digest.
        let wire_job: JobId = [0xCE; 16];
        fake_a
            .tx
            .send_frame(&wire(Message::Offer {
                job_id: wire_job,
                root_name: "iso-a".into(),
                file_count: 1,
                total_bytes: payload.len() as u64,
                chunk_size: CHUNK as u32,
            }))
            .await
            .unwrap();
        match fake_a.rx.recv_frame(&limits).await.unwrap().message {
            Message::Accept { job_id: acked } => assert_eq!(acked, wire_job),
            other => panic!("expected Accept, got {other:?}"),
        }
        let entry = ak_core::manifest::FileEntry::with_digest_alg(
            "iso-a.bin",
            payload.len() as u64,
            1_700_000_000_000,
            ak_core::manifest::DIGEST_ALG_BLAKE3,
            whole,
            chunk_digests,
        )
        .unwrap();
        let mut wire_entry = WireEntry::from(&entry);
        wire_entry.chunk_digests[1] = [0xEE; 32]; // poisoned
        fake_a
            .tx
            .send_frame(&wire(Message::ManifestPage {
                job_id: wire_job,
                page_index: 0,
                last_page: true,
                entries: vec![wire_entry],
            }))
            .await
            .unwrap();
        match fake_a.rx.recv_frame(&limits).await.unwrap().message {
            Message::ResumeRequest { files, more, .. } => {
                assert!(!more);
                assert_eq!(files[0].verified_prefix_chunks, 0, "fresh task");
            }
            other => panic!("expected ResumeRequest, got {other:?}"),
        }
        for (index, data) in [(0u64, a), (1u64, b)] {
            fake_a
                .tx
                .send_frame(&wire(Message::Chunk {
                    job_id: wire_job,
                    file_index: 0,
                    chunk_index: index,
                    data: data.to_vec(),
                }))
                .await
                .unwrap();
        }
        let err_a = receiver_a
            .await
            .unwrap()
            .expect_err("task A's poisoned digest must fail the receive");
        assert!(
            !matches!(err_a, EngineError::AdmissionDenied { .. }),
            "admission itself succeeded: {err_a:?}"
        );
        // Task A's verified prefix lives in ITS staging tree.
        let staged_a = ak_storage::staging_path(dst.path(), task_a).join("iso-a.bin.akpart");
        assert!(staged_a.exists(), "A's part must live in A's staging tree");
        assert_eq!(
            fs::metadata(&staged_a).unwrap().len(),
            CHUNK as u64,
            "exactly A's verified prefix survived"
        );

        // ---- task B (real sender): completes through the same daemon
        // pair into the same destination directory. Task A's endpoint
        // moved into its spawn, so B listens on a fresh endpoint bound
        // to the same identity (same trick as the staged-resume test's
        // run 2 — the destination dir is what carries the staged state).
        let src_b = tempfile::tempdir().unwrap();
        let payload_b: Vec<u8> = (0..5000u32).map(|i| (i % 199) as u8).collect();
        write(&src_b.path().join("iso-b.bin"), &payload_b);
        let task_b: [u8; 16] = [0xC2; 16];
        let endpoint_b =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr_b = endpoint_b.local_addr().unwrap();
        let sender_b = spawn_on_channel_sender(src_b.path().to_path_buf(), addr_b, pinned, task_b);
        let spec_b = on_channel_recv_spec(dst.path(), receiver_identity);
        let receiver_b = tokio::spawn(async move {
            let mut channel = endpoint_b.accept(&|_| true).await.expect("task B connects");
            let open = match channel_scheduler_handshake(&mut channel, &spec_b)
                .await
                .unwrap()
            {
                SchedulerHandshake::Scheduler { open } => open,
                SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
            };
            assert_eq!(open.job_id(), task_b, "B's TaskOpen names B's task");
            receive_on_channel_preadmitted(channel, spec_b, open, &noop).await
        });

        let report_b = receiver_b.await.unwrap().expect("task B completes");
        sender_b.await.unwrap().expect("task B's send completes");
        assert_eq!(report_b.bytes, payload_b.len() as u64);
        assert_eq!(fs::read(dst.path().join("iso-b.bin")).unwrap(), payload_b);
        // B's terminal state discarded only B's tree...
        assert!(
            !ak_storage::staging_path(dst.path(), task_b).exists(),
            "B's staging tree is discarded at completion"
        );
        // ...while A's staged prefix survives untouched (per-task
        // isolation: B never swept, resumed or collided with A's parts).
        assert!(
            staged_a.exists(),
            "A's staged prefix must survive B's terminal cleanup"
        );
        assert_eq!(fs::metadata(&staged_a).unwrap().len(), CHUNK as u64);
    }

    /// The pre-admitted entry refuses a retained TaskOpen whose direction
    /// tag says the peer is receiving (it must be the sender): the same
    /// PROTOCOL_UNSUPPORTED violation the combined path produces, no
    /// Admission is ever answered and nothing lands.
    #[tokio::test]
    async fn preadmitted_refuses_a_recv_direction_task_open() {
        let dst = tempfile::tempdir().unwrap();
        let receiver_key = DeviceKeyPair::generate();
        let receiver_identity = ak_identity::tls::make_self_signed(&receiver_key);
        let endpoint =
            DirectEndpoint::bind(SocketAddr::from(([127, 0, 0, 1], 0)), &receiver_identity)
                .await
                .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let spec = on_channel_recv_spec(dst.path(), receiver_identity);

        let receiver = tokio::spawn(async move {
            let mut channel = endpoint.accept(&|_| true).await.unwrap();
            let open = match channel_scheduler_handshake(&mut channel, &spec)
                .await
                .unwrap()
            {
                SchedulerHandshake::Scheduler { open } => open,
                SchedulerHandshake::Legacy { .. } => panic!("scheduler peer took the Legacy arm"),
            };
            receive_on_channel_preadmitted(channel, spec, open, &noop).await
        });

        let mut fake =
            fake_sender_side(addr, receiver_key.fingerprint(), CAP_RESUME | CAP_SCHEDULER).await;
        let task_id: [u8; 16] = [0xEE; 16];
        fake.tx
            .send_frame(&wire(Message::TaskOpen {
                job_id: task_id,
                permit_id: [7; 16],
                group: [0x77; 32],
                group_registry: [0x91; 32],
                pairing_id: [0xB3; 16],
                direction: ak_wire::DIRECTION_RECV,
            }))
            .await
            .unwrap();

        let err = receiver
            .await
            .unwrap()
            .expect_err("a recv-direction TaskOpen must be refused");
        assert!(
            matches!(
                &err,
                EngineError::PeerError {
                    code: ErrorCode::ProtocolUnsupported,
                    ..
                }
            ),
            "got: {err:?}"
        );
        assert!(
            !ak_storage::staging_path(dst.path(), task_id).exists(),
            "no staging may appear for a refused direction"
        );
        assert_eq!(
            fs::read_dir(dst.path()).unwrap().count(),
            0,
            "no file may land from a refused task"
        );
        // No Admission was answered: the fake sees the closed connection,
        // never a granted frame.
        if let Ok(Ok(frame)) = tokio::time::timeout(
            Duration::from_secs(2),
            recv_admission_frame(&mut fake.rx, &engine_limits(), Duration::from_secs(1)),
        )
        .await
        {
            panic!("no Admission may be answered, saw {:?}", frame.message);
        }
    }
}
