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
//! 6. Per file: receiver finish (whole digest, fsync, atomic rename), for
//!    up to `VERIFY_PARALLEL_FILES` files at a time (batch Q review).
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
//!   verify pass of a large job. A single large file's own pass (the
//!   prepare-time re-hash, finish()'s whole-digest pass and flush) runs on
//!   the blocking pool while the same signal keeps going on the timer
//!   (review 2026-11-19). In the verify phase every file's pass runs
//!   there, up to `VERIFY_PARALLEL_FILES` at a time (batch Q review,
//!   2026-11-20); after a failure no further pass starts and the running
//!   ones are waited for before the receive returns;
//! - sender liveness (batch S review, 2026-11-20): the sender's final
//!   source check — one stat per source file, right before Finalize — runs
//!   on the blocking pool, and when both Hellos carry
//!   [`ak_wire::CAP_SENDER_KEEPALIVE`] the sender sends a no-op
//!   `Control::Resume` at the same cadence meanwhile, which the receiver
//!   accepts and ignores until Finalize. Either end without the bit keeps
//!   the frozen behavior (no signal, and the frame stays unexpected);
//! - storage full mid-transfer (batch S review): when a chunk write fails
//!   because the destination is out of space or quota, the receiver first
//!   sends `Error{StorageFull}` where the sender reads its acks (the ack
//!   stream, or in multistream mode the failing file's stream) and holds
//!   the connection briefly so the sender can read it; the sender reports
//!   [`EngineError::PeerError`] with that code instead of a lost
//!   connection. Wire format unchanged; every engine version already maps
//!   the frame.
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
//! (a) checkpoint batches — on every `CHECKPOINT_EVERY_ACKS`-th
//! acknowledged chunk, at most once per `CHUNK_CHECKPOINT_INTERVAL`
//! (review 2026-11-19), the verified prefix is persisted to the
//! checkpoint store, and once more right before the verify phase; and
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
    scheduler_frames_allowed, sender_keepalive_allowed, AdmissionReason, Control, Frame, JobId,
    Limits, Message, ResumeEntry, WireEntry, CAP_MULTISTREAM, CAP_PAUSE_RESUME, CAP_RESUME,
    CAP_SCHEDULER, CAP_SENDER_KEEPALIVE, DIRECTION_SEND,
};

mod handshake;
mod manifest;
mod receiver;
mod recv_multi;
mod recv_single;
mod sender;

use handshake::*;
use manifest::*;
pub use receiver::*;
use recv_multi::*;
use recv_single::*;
pub use sender::*;

/// Manifest pages are assembled under this byte budget so every page fits a
/// frame comfortably below `ak_wire::MAX_FRAME_BYTES`.
const PAGE_BUDGET_BYTES: usize = 4 << 20;
/// Progress cadence, in acknowledged chunks: every this many acks the
/// sender and the receiver report `BytesAcked`, and the receiver considers
/// a checkpoint save (see [`ChunkCheckpointCadence`]).
const CHECKPOINT_EVERY_ACKS: u64 = 16;
/// Least time between two chunk-phase checkpoint saves, on both sides
/// (review 2026-11-19). A save rewrites the whole record, whose size grows
/// with the file count, so the sender's save after every file and the
/// receiver's save on every 16th ack cost O(files²) bytes over a
/// many-file job — most of its chunk phase. Neither side's resume trusts
/// these saves: the receiver re-hashes its part files, and the sender
/// resumes from the receiver's ResumeRequest.
const CHUNK_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// The receiver's chunk-phase cadence: progress on every
/// [`CHECKPOINT_EVERY_ACKS`]-th ack as before, a checkpoint save on such
/// an ack only once [`CHUNK_CHECKPOINT_INTERVAL`] has passed since the
/// last save. The acks that never reach a save are covered by the verify
/// phase, which saves once before it starts.
struct ChunkCheckpointCadence {
    acks: u64,
    last_save: Instant,
}

/// What one acknowledged chunk asks of the receive loop.
#[derive(Debug, PartialEq, Eq)]
struct CadenceTick {
    progress: bool,
    save: bool,
}

impl ChunkCheckpointCadence {
    fn new(now: Instant) -> Self {
        ChunkCheckpointCadence {
            acks: 0,
            last_save: now,
        }
    }

    fn on_ack(&mut self, now: Instant) -> CadenceTick {
        self.acks += 1;
        if self.acks < CHECKPOINT_EVERY_ACKS {
            return CadenceTick {
                progress: false,
                save: false,
            };
        }
        self.acks = 0;
        let save = now.saturating_duration_since(self.last_save) >= CHUNK_CHECKPOINT_INTERVAL;
        if save {
            self.last_save = now;
        }
        CadenceTick {
            progress: true,
            save,
        }
    }
}
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

/// Size from which a file's whole-content pass on the receiver — the
/// prepare-time re-hash of a resumed part or of an already committed
/// final, and finish()'s whole-digest pass plus `sync_all` — runs on the
/// blocking pool while the receive task keeps signalling liveness (review
/// 2026-11-19). Between files the cadence check covers smaller files: at
/// even 100 MB/s such a pass takes well under the liveness interval. The
/// verify phase runs every file's finish() on the blocking pool
/// ([`VERIFY_PARALLEL_FILES`]); there this size only keeps two such files
/// from running at once.
const OFFLOAD_WHOLE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Files the verify phase verifies and commits at once (batch Q review,
/// 2026-11-20). One file's finish() — open, whole-file hash, flush, link —
/// mostly waits on the file system, and one after another they took most
/// of a 65,537-file job on Windows (P handoff §9); running several at a
/// time overlaps those waits. Each file's own order is unchanged: its data
/// is flushed before its final name appears. Files of at least
/// [`OFFLOAD_WHOLE_FILE_BYTES`] never run beside each other, so at most one
/// multi-gigabyte pass competes for the disk.
const VERIFY_PARALLEL_FILES: usize = 8;

/// Liveness cadence of one transfer end: a quarter of this end's own idle
/// budget (the peer normally runs the same default), never more than
/// [`LIVENESS_MAX_INTERVAL`]. The receiver signals at this cadence while
/// it prepares and verifies, the sender while it re-checks its sources.
fn liveness_interval(idle: Duration) -> Duration {
    (idle / 4).min(LIVENESS_MAX_INTERVAL)
}

/// Run one long, blocking pass (a whole-file pass, or the sender's source
/// check) on the blocking pool and send `keepalive` every `every` until it
/// finishes (review 2026-11-19).
///
/// One file's pass — hashing a multi-gigabyte file, flushing it — cannot
/// be split at file boundaries, so the between-file cadence of the
/// prepare and verify loops alone left the sender's idle budget to cover
/// the whole pass: on an ordinary disk a single file of a few gigabytes
/// made the sender give up while the receiver committed the file. The
/// receive task is also no longer blocked, so the transport keeps
/// running (a long blocking pass on a single-thread runtime used to
/// starve QUIC's own timers). `last_signal` is the time of the last frame
/// this receive sent; it moves with every keepalive. With `keepalive =
/// None` (a peer that cannot take one) the pass is still offloaded but
/// nothing is sent.
///
/// A failed keepalive stops the signalling but not the pass, which cannot
/// be interrupted anyway: the pass's result comes back as usual, and the
/// caller's next regular frame reports the broken connection and runs the
/// usual cleanup (so a sink prepared meanwhile is not leaked as litter).
///
/// The receiver's prepare loop uses it, and so does the sender's final
/// source check (batch S review: one stat per source file, so it grows
/// with the file count); the verify phase runs its passes, several at a
/// time, through its own loop with the same signalling
/// ([`VERIFY_PARALLEL_FILES`]).
async fn offload_with_liveness<T: Send + 'static>(
    tx: &mut DirectSendStream,
    mut keepalive: Option<&Frame>,
    every: Duration,
    last_signal: &mut Instant,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, EngineError> {
    let mut work = tokio::task::spawn_blocking(work);
    loop {
        let due = tokio::time::Instant::from_std(*last_signal + every);
        tokio::select! {
            joined = &mut work => {
                return joined.map_err(|join| {
                    EngineError::Io(std::io::Error::other(format!(
                        "blocking pass did not complete: {join}"
                    )))
                });
            }
            () = tokio::time::sleep_until(due) => {
                if let Some(frame) = keepalive {
                    if tx.send_frame(frame).await.is_err() {
                        keepalive = None;
                    }
                }
                *last_signal = Instant::now();
            }
        }
    }
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

/// Run one receive to completion (see [`receive_with_addr`]).
pub async fn receive(
    spec: ReceiveSpec,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, EngineError> {
    let (_tx, _rx) = tokio::sync::oneshot::channel::<SocketAddr>();
    receive_with_addr(spec, progress, _tx).await
}

/// Unit-test seams for two conditions a test cannot produce cheaply: a
/// final source check longer than the peer's idle budget (it would take
/// hundreds of thousands of files) and a disk that fills up mid-transfer.
/// Compiled into the unit tests only. Every entry is keyed by the job's
/// root name — a test's own temporary directory name — so tests running
/// in parallel never see each other's entries.
#[cfg(test)]
mod seams {
    use super::JobId;
    use std::sync::Mutex;
    use std::time::Duration;

    static SLOW_SOURCE_CHECK: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());
    static FULL_AFTER_WRITES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
    /// Receives armed from `FULL_AFTER_WRITES` (by job id): chunk writes
    /// left before the simulated full disk.
    static ARMED: Mutex<Vec<(JobId, u64)>> = Mutex::new(Vec::new());

    /// Make the final source check of sends rooted at `root_name` take
    /// `delay` longer.
    pub(crate) fn slow_source_check(root_name: &str, delay: Duration) {
        SLOW_SOURCE_CHECK
            .lock()
            .unwrap()
            .push((root_name.to_owned(), delay));
    }

    pub(super) fn source_check_delay(names: &[String]) -> Option<Duration> {
        SLOW_SOURCE_CHECK
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| names.contains(name))
            .map(|(_, delay)| *delay)
    }

    /// Let receives of jobs rooted at `root_name` store `writes` chunks,
    /// then fail every further chunk write as a full disk does.
    pub(crate) fn fill_disk_after(root_name: &str, writes: u64) {
        FULL_AFTER_WRITES
            .lock()
            .unwrap()
            .push((root_name.to_owned(), writes));
    }

    pub(super) fn arm_receive(root_name: &str, job_id: JobId) {
        let writes = FULL_AFTER_WRITES
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| name == root_name)
            .map(|(_, writes)| *writes);
        if let Some(writes) = writes {
            ARMED.lock().unwrap().push((job_id, writes));
        }
    }

    pub(super) fn chunk_write(job_id: &JobId) -> std::io::Result<()> {
        let mut armed = ARMED.lock().unwrap();
        match armed.iter_mut().find(|(armed_job, _)| armed_job == job_id) {
            Some((_, 0)) => Err(std::io::Error::from(std::io::ErrorKind::StorageFull)),
            Some((_, left)) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
