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
//!   top-level directory per root);
//! - receiver liveness (review 2026-11-18): the receiver sends its sharded
//!   ResumeRequest answer while it prepares the sinks (a `more = true`
//!   shard, possibly empty, at least every `liveness_interval`), and
//!   during the verify phase it sends a no-op `Control::Resume` to a peer
//!   that negotiated `CAP_PAUSE_RESUME` at the same cadence, so the
//!   sender's per-frame idle budget never has to cover a whole prepare or
//!   verify pass of a large job.
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
/// Longest the receiver stays silent toward the sender while it works
/// through a phase that sends no frames of its own (review 2026-11-18):
/// preparing every sink before the ResumeRequest answer, and verifying
/// and committing every file between Finalize and FinalAck. The sender
/// waits for each frame under its idle budget (default 30 s), while both
/// phases grow with the file count, so a large job on a slow disk
/// outlasted the budget and the sender gave up on a healthy receiver.
const LIVENESS_MAX_INTERVAL: Duration = Duration::from_secs(5);
/// Checkpoint cadence of the verify phase. The record is rewritten whole
/// on every save, so saving after every file cost O(files²) bytes.
/// Commit safety does not depend on it: a retry finds a committed file
/// through the storage layer's whole-digest check of the final name.
const VERIFY_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// Liveness cadence of one receive: a quarter of the receiver's own idle
/// budget (the sender normally runs the same default), never more than
/// [`LIVENESS_MAX_INTERVAL`].
fn liveness_interval(idle: Duration) -> Duration {
    (idle / 4).min(LIVENESS_MAX_INTERVAL)
}
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
/// QUIC application-error code for a failed multistream receive (M6, batch
/// M 2026-11-17): `0xA0` protocol family, `0x03` multistream abort. The
/// multistream receiver's control reader (deliberately never cancelled:
/// quinn reads are not cancel-safe) and its per-stream tasks hold stream
/// handles, and a quinn connection stays open while any handle lives — with
/// keep-alive PINGs it never idles out. Without an explicit close the
/// sender therefore never learned that the receive had failed and could
/// block forever in a flow-controlled write. The single-stream path needs
/// no code: its handles all drop with the failing call.
const MULTISTREAM_ABORT_CLOSE_CODE: u32 = 0xA0_03;

/// Optional end-of-transfer transport-stats sink (2026-11-13 review §5.2
/// follow-up): invoked exactly once per successful transfer with an
/// [`ak_net::ChannelStats`] snapshot taken from the transfer's QUIC
/// connection right before the engine returns its [`Report`] — the
/// congestion-window end value, congestion events, and loss/packet counters
/// that are otherwise unreachable through the engine path (the engine owns
/// the connection; callers never see the `quinn::Connection`).
///
/// Informational plumbing for benches and operators: the transfer never
/// depends on the sink, failures inside it do not affect the transfer, and
/// `None` (the default of every spec) keeps engine behavior identical.
/// Wrapped in a newtype (not a bare type alias) so the specs keep their
/// `Debug` derive — the closure itself is not `Debug`.
///
/// Field mapping and accounting limits: see `ak_net::ChannelStats` — `cwnd`
/// is the end-of-transfer sample, and quinn 0.11 exposes no received-packet
/// counter (the receive-side count is `rx_datagrams`).
#[derive(Clone)]
pub struct ChannelStatsSink(std::sync::Arc<dyn Fn(&ak_net::ChannelStats) + Send + Sync>);

impl ChannelStatsSink {
    /// Wrap one stats consumer; the engine calls it with the snapshot at
    /// transfer completion (see the type docs).
    pub fn new(sink: impl Fn(&ak_net::ChannelStats) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(sink))
    }

    /// Deliver one snapshot (engine-internal: called once per successful
    /// transfer, immediately before the `Report` is returned).
    fn notify(&self, stats: &ak_net::ChannelStats) {
        (self.0)(stats);
    }
}

impl std::fmt::Debug for ChannelStatsSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelStatsSink(..)")
    }
}

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
    /// Optional end-of-transfer transport-stats snapshot hook (see
    /// [`ChannelStatsSink`]); `None` (the default) keeps behavior
    /// identical. Notified on every completed send, including retry
    /// passes that re-run the body on a fresh connection.
    pub stats_sink: Option<ChannelStatsSink>,
    /// Adopted pre-bound UDP socket for the dial (CR-P1-05, review
    /// 2026-11-11 §4.5: both sides' probing/punching must happen on THE
    /// socket that will carry QUIC). Crate-private by design — the ONLY
    /// way in is the validating [`SendSpec::adopt_socket`] — so an
    /// external builder cannot leave `bind` and the adopted socket
    /// disagreeing. Shared behind an `Arc` so the derived `Clone` of the
    /// spec (retry passes re-build specs) keeps ONE OS socket; `None`
    /// (the default) keeps the historical self-bind of
    /// [`SendSpec::bind`] byte for byte.
    adopted_socket: Option<std::sync::Arc<std::net::UdpSocket>>,
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
            stats_sink: None,
            adopted_socket: None,
        }
    }

    /// Append another file or directory to this send job (multi-root send).
    /// Root names must be unique across the job; see [`SendSpec::extra_roots`].
    pub fn add_root(&mut self, path: PathBuf) {
        self.extra_roots.push(path);
    }

    /// Adopt a caller-owned, already-bound UDP socket for the QUIC dial
    /// (CR-P1-05, review 2026-11-11 §4.5): when set, [`send`] builds its
    /// endpoint on THIS socket via [`ak_net::DirectEndpoint::dial_with_socket`]
    /// instead of binding [`SendSpec::bind`], so a NAT mapping the caller
    /// opened on it (a same-socket STUN query, a probe round) survives
    /// into the QUIC Initial. The socket is shared behind an `Arc` (a
    /// cloned spec — retry passes rebuild them — dials from the same OS
    /// socket; each actual [`send`] hands quinn a `try_clone` of it, a
    /// duplicate handle with the same local port). `None` (never called)
    /// keeps the self-bind path and its wire behavior byte-identical.
    ///
    /// The adopted socket dials both families when it is the dual-stack
    /// `[::]` bind (quinn maps plain-v4 peers to `::ffff:` form
    /// internally); a v4-only socket must be given v4 peers as usual.
    pub fn adopt_socket(&mut self, socket: std::net::UdpSocket) {
        self.adopted_socket = Some(std::sync::Arc::new(socket));
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
    /// Optional end-of-transfer transport-stats snapshot hook (see
    /// [`ChannelStatsSink`]); `None` (the default) keeps behavior
    /// identical. Notified on every completed receive.
    pub stats_sink: Option<ChannelStatsSink>,
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
            stats_sink: None,
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
async fn connect_bounded(
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

/// Receiver-side builder of the sharded resume answer while the sinks are
/// being prepared (review 2026-11-18). Instead of answering only after
/// the whole prepare pass, a `more = true` shard leaves as soon as it
/// reaches the per-frame cap, or when the liveness interval has passed
/// since the last frame — then possibly empty, which the sender's
/// [`ResumeCollector`] accepts as "keep waiting". Whatever is left goes
/// out through [`resume_request_frames`] as the final `more = false`
/// frame once the pass is over.
struct ResumeShards {
    pending: Vec<ResumeEntry>,
    max_per_frame: usize,
    every: Duration,
    last_frame: Instant,
}

impl ResumeShards {
    fn new(max_per_frame: usize, every: Duration, now: Instant) -> Self {
        ResumeShards {
            pending: Vec::new(),
            max_per_frame: max_per_frame.max(1),
            every,
            last_frame: now,
        }
    }

    fn push(&mut self, entry: ResumeEntry) {
        self.pending.push(entry);
    }

    /// The `more = true` shard to send at `now`, if one is due: the
    /// pending entries once they fill a frame, or whatever is pending
    /// (possibly nothing) once the liveness interval has passed.
    fn take_due(&mut self, now: Instant) -> Option<Vec<ResumeEntry>> {
        let full = self.pending.len() >= self.max_per_frame;
        let quiet_too_long = now.saturating_duration_since(self.last_frame) >= self.every;
        if full || quiet_too_long {
            self.last_frame = now;
            Some(std::mem::take(&mut self.pending))
        } else {
            None
        }
    }

    /// The entries for the final frame(s).
    fn into_rest(self) -> Vec<ResumeEntry> {
        self.pending
    }
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
    // CR-P1-05: an adopted socket (same-socket STUN/probe lifecycle, see
    // [`SendSpec::adopt_socket`]) carries the caller's NAT mapping into
    // the QUIC dial; no adoption keeps the historical self-bind. The Arc
    // keeps the OS socket alive across retry passes; this send hands
    // quinn one duplicated handle of it (same local port).
    let endpoint = match &spec.adopted_socket {
        Some(socket) => {
            let handle = socket.try_clone().map_err(EngineError::from)?;
            DirectEndpoint::dial_with_socket(handle, &spec.identity).await?
        }
        None => DirectEndpoint::bind(spec.bind, &spec.identity).await?,
    };
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

    // End-of-transfer transport-stats snapshot (see `ChannelStatsSink`):
    // taken after the last payload byte was acked (FinalAck above) and
    // before the report leaves the engine, so `cwnd` reads the
    // end-of-transfer window. Informational only; skipped when no sink.
    if let Some(sink) = &spec.stats_sink {
        sink.notify(&channel.stats());
    }

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
    //
    // The answer is sent while the sinks are prepared (review 2026-11-18):
    // a `more = true` shard leaves whenever it fills a frame or the
    // liveness interval has passed, so the sender's per-frame idle budget
    // never has to cover the whole prepare pass (see LIVENESS_MAX_INTERVAL).
    let mut sinks: Vec<Option<ak_storage::LocalFileSink>> =
        Vec::with_capacity(job.manifest.entries.len());
    let mut shards = ResumeShards::new(
        usize::from(limits.max_resume_entries),
        liveness_interval(spec.idle_timeout),
        Instant::now(),
    );
    let mut resumed_chunks: u64 = 0;
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
        let signal_liveness = (peer_caps & CAP_PAUSE_RESUME) != 0;
        let liveness = liveness_interval(spec.idle_timeout);
        let mut last_signal = Instant::now();
        for (index, sink_slot) in sinks.iter_mut().enumerate() {
            if signal_liveness && last_signal.elapsed() >= liveness {
                tx.send_frame(&Frame {
                    version: ak_wire::VERSION,
                    message: Message::Control {
                        job_id,
                        control: Control::Resume,
                    },
                })
                .await?;
                // Let the transport (and a peer sharing this runtime) run.
                tokio::task::yield_now().await;
                last_signal = Instant::now();
            }
            if let Some(mut sink) = sink_slot.take() {
                if job.manifest.entries[index].is_dir {
                    sink.finish().map_err(EngineError::Io)?;
                    continue;
                }
                sink.finish().map_err(EngineError::Io)?;
                record.files[index].verified_prefix_chunks =
                    job.manifest.entries[index].chunk_digests.len() as u64;
                if last_save.elapsed() >= VERIFY_CHECKPOINT_INTERVAL {
                    record.updated_unix_ms = now_ms();
                    store.save(&record).map_err(EngineError::Checkpoint)?;
                    last_save = Instant::now();
                }
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
    /// An accepted per-file stream delivered its first frame (read by the
    /// stream's own opener task, never inline in the driver loop — see
    /// [`multistream_receive`]); the driver validates it and starts the
    /// stream's receive task.
    Opened {
        stream: DirectStream,
        first: Frame,
    },
    /// A stream task or the control stream failed.
    Failed(EngineError),
    /// Finalize observed on the control stream.
    Finalize,
    Cancelled,
}

/// Abort handles of a multistream receive's helper tasks (control reader,
/// control monitor, per-stream openers and receive tasks). Dropping the
/// guard aborts every task that is still running, so a multistream receive
/// that fails or is cancelled (its future dropped, e.g. the receiving task
/// aborted) cannot leave stream handles behind (M6, batch M 2026-11-17).
/// Those handles kept the QUIC connection — and its keep-alives — alive, so
/// the sender waited out its whole idle budget (`IdleTimeout(30s)`, the
/// property suite's historical Windows failure, reproduced on Linux) or,
/// blocked in a flow-controlled write, forever. A completed receive
/// disarms the guard: its tasks then run exactly as before (the control
/// reader keeps draining through the FinalAck hold).
#[derive(Default)]
struct ReceiveTaskGuard(Vec<tokio::task::AbortHandle>);

impl ReceiveTaskGuard {
    fn track(&mut self, task: &tokio::task::JoinHandle<()>) {
        self.0.push(task.abort_handle());
    }

    /// Forget finished tasks so a long job holds one handle per live task.
    fn prune(&mut self) {
        self.0.retain(|task| !task.is_finished());
    }

    /// The receive completed: leave the remaining tasks running.
    fn disarm(mut self) {
        self.0.clear();
    }
}

impl Drop for ReceiveTaskGuard {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
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
    let mut tasks = ReceiveTaskGuard::default();
    let reader = tokio::spawn(async move {
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
    tasks.track(&reader);

    // Control monitor: forwards Finalize/Cancel/Error into the driver's
    // event channel, then keeps draining silently so the connection is
    // held until the sender half-closes after reading the FinalAck. All
    // frames must name this job (cross-frame validation).
    let control_tx = events_tx.clone();
    let monitor = tokio::spawn(async move {
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
    tasks.track(&monitor);

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
                        tasks.prune();
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
                    StreamEvent::Opened { stream, first } => {
                        // The first frame on a new stream names its file and
                        // must belong to this job.
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
                        tasks.track(&receive_stream_task(
                            stream,
                            sink,
                            file_index,
                            first,
                            job_id,
                            idle,
                            events_tx.clone(),
                        ));
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
                    Ok(Ok(stream)) => {
                        last_activity = Instant::now();
                        let _ = activity_tx.try_send(());
                        // M6 (batch M, 2026-11-17): the first frame on a new
                        // stream is read by the stream's own opener task,
                        // never inline here. While the driver awaited one
                        // stream's first frame it could neither accept nor
                        // let anything read the sender's other concurrently
                        // opened streams, whose buffered data still counts
                        // against QUIC's connection-level receive window
                        // (credit returns only as the application reads).
                        // Once streams x (chunk + header) exceeded that
                        // window (2 streams at 4 MiB chunks, 4 at 2 MiB,
                        // against 8 MiB) the first frame could never
                        // complete: a deadlock, which the release
                        // chunk-size sweep hit at 4096 KiB / multistream on
                        // Linux and Windows alike.
                        open_streams += 1;
                        let events = events_tx.clone();
                        let opener = tokio::spawn(async move {
                            let mut stream = stream;
                            let event = match tokio::time::timeout(
                                idle,
                                stream.recv_frame(&limits),
                            )
                            .await
                            {
                                Ok(Ok(first)) => StreamEvent::Opened { stream, first },
                                Ok(Err(e)) => StreamEvent::Failed(EngineError::Stream(e)),
                                Err(_) => StreamEvent::Failed(EngineError::IdleTimeout(idle)),
                            };
                            let _ = events.send(event).await;
                        });
                        tasks.track(&opener);
                    }
                }
            }
        }
    }
    tasks.disarm();
    Ok(events_rx)
}

/// Drive one accepted per-file stream: write/verify/ack every chunk until
/// the sender half-closes, then hand the sink back to the driver. Returns
/// the task's handle so the driver's [`ReceiveTaskGuard`] can abort it.
fn receive_stream_task(
    mut stream: DirectStream,
    mut sink: ak_storage::LocalFileSink,
    file_index: u32,
    first: Frame,
    job_id: JobId,
    idle: Duration,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
) -> tokio::task::JoinHandle<()> {
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
    })
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
mod tests;
