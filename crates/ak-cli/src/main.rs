//! Command-line prototype for AK-sending (single binary, many verbs).
//!
//! Verbs: `keygen`, `fingerprint`, `pair`, `receive`, `daemon`, `send`,
//! `discover`, `jobs`, `stun-server`, `bench`, `doctor`, `completions`,
//! `registry-server`, `invite`, `task` — plus the legacy plaintext
//! `signal-server` and `probe`, which exist only in builds with the
//! `legacy-plaintext-signal` feature (G5 / E1 frozen model; the default
//! build compiles neither). The
//! binary never talks to any file-carrying server; servers started here are
//! coordination-only (ADR-001).
//!
//! Pairing model: the local peer book (`peers.book` in the id directory)
//! holds fingerprint/name pairs. `receive` defaults to accepting only paired
//! fingerprints; a successful `send` records the receiver trust-on-first-use.
//! `pair export` / `pair import` move that PUBLIC pairing data between
//! machines as an AKPAIR1 text block; device.key (the private seed) is never
//! part of any export.
#![forbid(unsafe_code)]

mod daemon;
mod doctor;
mod errors;
mod invite;
mod task_cli;

use std::future::Future;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
// G5 / E1: `Instant` is consumed only by the gated `probe` command at
// crate level (the tests module imports its own).
#[cfg(feature = "legacy-plaintext-signal")]
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};

use ak_core::handle::ConflictPolicy;
// G5 / E1: consumed only by the gated `probe` command's path reporting.
#[cfg(feature = "legacy-plaintext-signal")]
use ak_core::policy::PathKind;
use ak_engine::{EngineError, Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::tls::TlsBundle;
use ak_identity::{DeviceKeyPair, Fingerprint, PeerBook, PeerRecord};

// ---------------------------------------------------------------------------
// --version payload (two-machine version alignment)
// ---------------------------------------------------------------------------

/// Wire protocol facts come straight from ak-wire now (single source of
/// truth; the version test pins the shipped values).
const WIRE_VERSION: u8 = ak_wire::VERSION;
/// Every capability this build speaks (the engine advertises exactly these).
const WIRE_CAPS: u16 = ak_wire::CAP_RESUME
    | ak_wire::CAP_PAUSE_RESUME
    | ak_wire::CAP_MULTISTREAM
    | ak_wire::CAP_SENDER_KEEPALIVE;

/// Compile profile of this binary (cargo's default profiles only).
fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// Build-time source revision stamped by `crates/ak-cli/build.rs`
/// (CR-P2-09): the short git sha of the commit being built, suffixed
/// `-dirty` when the working tree differs from HEAD, or `unknown` when
/// the binary was built from a source export without git (the build
/// script degrades instead of failing the build; CI may inject the value
/// through the same-named environment variable).
fn build_git_sha() -> &'static str {
    env!("AK_GIT_SHA")
}

/// `--version` payload (clap prepends the program name itself): workspace
/// version, wire protocol version with capability bits, the compile
/// profile, and the source revision -- enough for two machines to confirm
/// they run compatible builds before debugging a transfer, and for ops to
/// pin what the production registry server runs (CR-P2-09).
fn version_line_with(git_sha: &str) -> String {
    format!(
        "{} (wire v{}, caps 0x{WIRE_CAPS:04x} \
         [resume|pause-resume|multistream|sender-keepalive], {} build, git {git_sha})",
        env!("CARGO_PKG_VERSION"),
        WIRE_VERSION,
        build_profile()
    )
}

/// The payload with the sha this binary was actually built from.
fn version_line() -> String {
    version_line_with(build_git_sha())
}

/// The same line as a `&'static str` for clap's `long_version`, which only
/// accepts 'static strings. Built once per process; the single small
/// allocation is intentionally leaked.
fn version_line_static() -> &'static str {
    static LINE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    LINE.get_or_init(|| Box::leak(version_line().into_boxed_str()))
}

#[derive(Debug, Parser)]
#[command(
    name = "ak-cli",
    version,
    long_version = version_line_static(),
    // CR-P2-09: every subcommand answers `--version` with the same payload
    // (`ak-cli registry-server --version` pins what the server runs).
    propagate_version = true,
    about = "AK-sending direct-only prototype",
    after_help = "Typical flow:\n  1. ak-cli pair code                 (on the receiving device)\n  2. ak-cli receive --port 7100       (accepts paired peers only)\n  3. ak-cli send <path> [more paths...] --to <ip:7100[,ip2:7100...]> --fp <fingerprint>\n\nFingerprints accept the full 64 hex characters or a unique 8-63 character\nprefix of a peer already in peers.book. Multiple send paths travel in one\njob; each keeps its own top-level name on the receiver (paths with the same\nname must be sent in separate runs). --to accepts comma-separated address\ncandidates (private/LAN addresses are tried first, each with a 3s connect\ntimeout before the next is attempted).\nAll transfers are direct device-to-device; no server ever carries file data."
)]
struct Cli {
    /// Identity directory holding device.key, peers.book, checkpoints
    /// (default: `<home>/.ak-sending`).
    #[arg(long, global = true)]
    id_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or show the device key, fingerprint, and pairing code.
    Keygen,
    /// Print the current fingerprint and pairing code (same as keygen on an
    /// existing identity directory).
    Fingerprint,
    /// Manage the peer book: paired device fingerprints (peers.book).
    Pair {
        #[command(subcommand)]
        cmd: PairCommand,
    },
    /// Pairing invite codes (wave 21): create/list/revoke bearer codes on
    /// the v2 registry, and redeem one to pair with its owner (then wait
    /// receiving into --out).
    Invite {
        #[command(subcommand)]
        cmd: InviteCommand,
    },
    /// v1 minimal task codes (wave 32): one random code = one task,
    /// exactly two sides, generated on demand, disconnect ends the task,
    /// zero configuration (the GUI's task model as CLI commands).
    Task {
        #[command(subcommand)]
        cmd: task_cli::TaskCommand,
    },
    /// Run the authoritative v2 pairing registry (TLS + SQLite invite
    /// store; default port 7200). Print its registry id so devices can
    /// pin it with --registry-fp. No file routes exist on this server.
    RegistryServer {
        /// TLS listen port (default 7200, the frozen v2 port).
        #[arg(long)]
        port: Option<u16>,
        /// Listen address override: bare IP (default 0.0.0.0) or full
        /// `ip:port` (its port wins over --port, with a notice).
        #[arg(long, value_parser = bind_arg_parser)]
        bind: Option<BindArg>,
        /// Registry database file (default `<id-dir>/registry.db`).
        #[arg(long)]
        db_path: Option<PathBuf>,
    },
    /// Print the registry id (the 64-hex pin clients set with
    /// --registry-fp) of an EXISTING `<id-dir>/registry.key` — exactly the
    /// id `registry-server` serves for that directory. Read-only: never
    /// creates the directory or a key, opens no socket and no database;
    /// a missing or invalid key is an error.
    RegistryId,
    /// Receive transfers into a directory. By default keeps serving one job
    /// after another until Ctrl-C; --once handles exactly one job.
    Receive {
        /// Destination directory (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// UDP port to listen on (0 = auto; default 7100).
        #[arg(long)]
        port: Option<u16>,
        /// Listen address override: a bare IP (`0.0.0.0` default, `::`,
        /// `192.168.1.5` -- the port comes from --port) or a full
        /// `ip:port`/`[v6]:port` (`[::]:7100` -- its port wins over --port,
        /// with a notice). IPv6 listeners need the peer to reach this host over
        /// IPv6 or an IPv4-mapped address; binding itself is left to the
        /// engine (no relay fallback exists).
        #[arg(long, value_parser = bind_arg_parser)]
        bind: Option<BindArg>,
        /// Accept policy: omitted = only fingerprints in peers.book;
        /// 'any' = accept every peer (warning: unauthenticated);
        /// 64-hex fingerprint (or unique 8-63 character peers.book prefix)
        /// = pin that one peer;
        /// 6-digit pairing code = accept the peer whose code matches.
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Handle exactly one transfer, then exit (default: loop until Ctrl-C).
        #[arg(long, default_value_t = false)]
        once: bool,
        /// LAN-only mode: refuse non-local paths (no internet direct transfer).
        #[arg(long, default_value_t = false)]
        no_internet: bool,
        /// Idle timeout in seconds: abort when no frame arrives within this
        /// budget (idle timeout, not total run time). Applies to every loop
        /// iteration.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// Accept N transfers CONCURRENTLY from different senders (one-shot
        /// concurrent mode; overrides the sequential loop).
        #[arg(long, value_parser = clap::value_parser!(u64).range(2..))]
        jobs: Option<u64>,
    },
    /// Resident receive mode: advertise this device over mDNS (same TXT as
    /// `discover` shows) and keep accepting job after job until Ctrl-C.
    /// Unlike `receive`, a failed or timed-out job does not end the daemon:
    /// the failure is reported and the loop keeps listening. `--json-lines`
    /// turns stdout into an NDJSON event stream (listening/job_started/
    /// job_done/error/stopped) for GUI and script consumers.
    Daemon {
        /// Destination directory (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// UDP port to listen on; 0 = auto-assign (the default, because the
        /// concrete port is published via mDNS and the listening event).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Accept policy, same shapes as `receive --expect`: omitted =
        /// fingerprints in peers.book only; 'any'; a 64-hex fingerprint (or
        /// unique 8-63 character book prefix); a 6-digit pairing code.
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Idle timeout in seconds: frame-gap budget per job (not total run
        /// time; waiting between senders is unbounded like `receive`).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// Device name published in the mDNS TXT and shown to senders
        /// (default: the local user name, as `receive` reports).
        #[arg(long)]
        name: Option<String>,
        /// Emit NDJSON events on stdout instead of human text; progress
        /// rendering is suppressed and informational notes move to stderr.
        #[arg(long, default_value_t = false)]
        json_lines: bool,
        /// Redeem an invite code (AKI1-...) at startup, pair with its
        /// owner, then keep receiving residently (wave 21).
        #[arg(long)]
        invite_code: Option<String>,
        /// Enable the owner-side pairing service: connect the pinned
        /// registry and answer redeem offers for codes in
        /// `<id-dir>/invites.store` (wave 21; `ak-cli invite create`
        /// populates that store).
        #[arg(long, default_value_t = false)]
        pairing: bool,
        /// Same-code-group concurrency pushed to the scheduler runtime via
        /// SessionHandle::set_policy (wave 21): serial | parallel.
        #[arg(long, value_parser = ["serial", "parallel"])]
        same_code: Option<String>,
        /// Cross-code-group concurrency: serial | parallel.
        #[arg(long, value_parser = ["serial", "parallel"])]
        cross_code: Option<String>,
        /// Registry endpoint ip:port (with --registry-fp; persisted).
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        /// Registry identity pin, 64 hex characters (with --server).
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Send files and/or directories to a peer. The first path is the
    /// primary root; further paths join the same job, each keeping its own
    /// top-level name on the receiver (root names must be unique per run).
    /// On success the peer's fingerprint is recorded in peers.book (trust on
    /// first use).
    Send {
        /// Files/directories to send: the first is the primary root, any
        /// further paths are extra roots in the same job.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Peer address ip:port, or comma-separated candidates
        /// (`--to "1.2.3.4:7000,5.6.7.8:7000"`; IPv6 as `[v6]:port`).
        /// Private/LAN addresses are tried before public ones and each
        /// address gets a 3-second connect timeout before the next is
        /// attempted (wave 25 multi-NIC fallback).
        #[arg(
            long,
            required = true,
            value_delimiter = ',',
            value_parser = to_addr_parser
        )]
        to: Vec<SocketAddr>,
        /// Peer fingerprint pinned for this transfer: 64 hex characters, or
        /// a unique 8-63 character prefix of a peer already in peers.book.
        /// Get it from the peer device via `ak-cli pair code`.
        #[arg(long)]
        fp: String,
        /// Local UDP port (0 = auto).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Overwrite policy on the receiver side: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Transfer files on parallel QUIC streams when the peer supports it.
        #[arg(long, default_value_t = false)]
        multistream: bool,
        /// LAN-only mode: refuse non-local paths (no internet direct transfer).
        #[arg(long, default_value_t = false)]
        no_internet: bool,
        /// Idle timeout in seconds: abort when no frame arrives within this
        /// budget (idle timeout, not total run time).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },
    /// Discover peers on the local network via mDNS (bounded browse) and show
    /// pairing status for each candidate.
    Discover {
        /// Seconds to browse before reporting.
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Emit one machine-readable JSON array instead of the text table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show or clean transfer job checkpoints (resumable state records in
    /// `<id-dir>/checkpoints`). Part files (.akpart) are never touched.
    Jobs {
        #[command(subcommand)]
        cmd: Option<JobsCommand>,
    },
    /// Environment self-check for two-machine LAN debugging (REVIEW 4.3):
    /// identity, local networks, UDP bind, mDNS, optional STUN, disk space.
    /// Every probe is timeout-bounded; failures are reported (ok/warn/skip),
    /// never fatal.
    Doctor {
        /// STUN server ip:port to query (3s timeout). Omitted = skip the
        /// check (the default; LAN debugging does not need it).
        #[arg(long)]
        stun: Option<SocketAddr>,
        /// mDNS advertise+browse window in seconds (best effort).
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=30))]
        mdns_seconds: u64,
        /// Emit machine-readable JSON instead of the text report.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Run the coordination server (signaling + STUN; no file routes).
    ///
    /// G5 / E1 (frozen legacy model): this subcommand — the old plaintext
    /// TCP 7100 signaling server — is only compiled with the
    /// `legacy-plaintext-signal` feature. The standalone STUN responder
    /// (`stun-server`, CR-P2-13) covers UDP 7101 in every build.
    #[cfg(feature = "legacy-plaintext-signal")]
    SignalServer {
        /// TCP signaling port (default 7100).
        #[arg(long)]
        tcp_port: Option<u16>,
        /// STUN UDP port (default 7101).
        #[arg(long)]
        stun_port: Option<u16>,
        /// Listen address override for the TCP signaling endpoint: a bare IP
        /// (default 0.0.0.0; port comes from --tcp-port) or a full
        /// `ip:port`/`[v6]:port` (`[::]:7100`; its port wins over --tcp-port,
        /// with a notice). IPv6 listens need v6-reachable clients (or
        /// v4-mapped on dual-stack hosts) for signaling to work.
        #[arg(long, value_parser = bind_arg_parser)]
        bind: Option<BindArg>,
        /// Listen address override for the STUN UDP endpoint, same shapes as
        /// --bind (default 0.0.0.0 + --stun-port).
        #[arg(long, value_parser = bind_arg_parser)]
        stun_bind: Option<BindArg>,
    },
    /// Run a standalone STUN binding responder (RFC 5389 subset, UDP only;
    /// no file routes, never relays anything). CR-P2-13: the same responder
    /// loop the signal-server runs, as its own process, so retiring the old
    /// signaling process does not silently disable STUN on UDP 7101.
    StunServer {
        /// UDP port to answer Binding requests on (default 7101, the port
        /// clients and the old signal-server already use).
        #[arg(long)]
        port: Option<u16>,
        /// Listen address override: a bare IP (default 0.0.0.0; port comes
        /// from --port) or a full `ip:port`/`[v6]:port` (`[::]:7101`; its
        /// port wins over --port, with a notice).
        #[arg(long, value_parser = bind_arg_parser)]
        bind: Option<BindArg>,
    },
    /// Public direct-connect probe: register, exchange candidates, punch, QUIC.
    ///
    /// G5 / E1 (frozen legacy model): `probe` is the client of the old
    /// plaintext signal plane (Hello-authenticated TCP register/session
    /// exchanges via `ak_signal::client`); it is only compiled with the
    /// `legacy-plaintext-signal` feature, together with `signal-server`.
    #[cfg(feature = "legacy-plaintext-signal")]
    Probe {
        #[arg(long)]
        server: SocketAddr,
        /// Peer device id (64-hex fingerprint) to punch toward.
        #[arg(long)]
        peer: String,
        /// Skip the STUN mapping query.
        #[arg(long, default_value_t = false)]
        no_stun: bool,
        /// Probe over a dual-stack IPv6 socket (v4 peers via ::ffff mapping);
        /// falls back to the plain v4 socket when the host cannot.
        #[arg(long, default_value_t = false)]
        dual_stack: bool,
        /// Emit the whole run as one JSON object on stdout (progress logs go
        /// to stderr) for scripted probing; exit-code semantics are unchanged.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// In-process loopback benchmark: generate files, then push them through
    /// the real engine over 127.0.0.1 QUIC (evidence generator; no server
    /// carries file data). Default runs a single-stream and a multistream
    /// round and compares them.
    Bench {
        /// Total bytes to transfer, split evenly across --files files.
        #[arg(long, default_value_t = 268_435_456, value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
        /// Number of source files the data is split across.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..))]
        files: u32,
        /// Chunk size in KiB (1..=8192, the engine decode budget).
        #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=8192))]
        chunk_kb: u32,
        /// Select the multistream round with --once; without --once both
        /// rounds always run and this flag has no extra effect.
        #[arg(long, default_value_t = false)]
        multistream: bool,
        /// Run exactly one round (single-stream by default, multistream with
        /// --multistream) instead of both plus a comparison.
        #[arg(long, default_value_t = false)]
        once: bool,
        /// Emit machine-readable JSON instead of the text report.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Generate a shell completion script (bash/zsh/fish/powershell) on
    /// stdout; pipe it into your shell's completion directory, e.g.
    /// `ak-cli completions bash > /etc/bash_completion.d/ak-cli`.
    Completions {
        /// Target shell.
        shell: ShellKind,
    },
}

#[derive(Debug, Subcommand)]
enum PairCommand {
    /// Add (or rename) a paired peer in peers.book.
    Add {
        /// Peer fingerprint, 64 hex characters (see `pair code` on the peer).
        fingerprint: String,
        /// Friendly local name for the peer book entry.
        #[arg(long)]
        name: Option<String>,
    },
    /// List paired peers: name, short id, full fingerprint, paired time.
    List,
    /// Remove a paired peer from peers.book.
    Remove {
        /// Peer fingerprint: 64 hex characters, or a unique 8-63 character
        /// prefix of a peers.book entry.
        fingerprint: String,
    },
    /// Print this device's fingerprint and pairing code so the peer side can
    /// pair with this device.
    Code,
    /// Print an AKPAIR1 text block with this device's PUBLIC pairing data
    /// (device id, fingerprint, pairing code, every peers.book entry) for
    /// `pair import` on another machine. device.key / private key material
    /// is NEVER included.
    Export,
    /// Merge an AKPAIR1 block (from `pair export` on the peer device) into
    /// this device's peers.book: peers upsert by fingerprint keeping the
    /// newer paired time; malformed lines are counted, never fatal.
    Import {
        /// The AKPAIR1 text block, e.g. `"$(cat akpair.txt)"`.
        text: String,
    },
}

/// The `ak-cli invite` subcommand family (wave 21, B7). Every verb talks
/// to the pinned v2 registry; `--server`/`--registry-fp` (given together)
/// persist the pin into `<id-dir>/registry.pin` and later invocations
/// reuse it.
#[derive(Debug, Subcommand)]
enum InviteCommand {
    /// Generate and register one invite code on the registry, persist its
    /// bearer secret locally, and print the display form (AKI1-...).
    Create {
        /// Code lifetime in seconds (60..=2592000).
        #[arg(long, conflicts_with = "long_lived")]
        ttl_secs: Option<u32>,
        /// No expiry (long-lived code).
        #[arg(long, default_value_t = false)]
        long_lived: bool,
        /// Friendly local label for the code (display only).
        #[arg(long)]
        name: Option<String>,
        /// Registry endpoint ip:port (with --registry-fp; persisted).
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        /// Registry identity pin, 64 hex characters (with --server).
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// List this device's invite codes as the registry knows them (state,
    /// expiry, revision, masked hint). Never prints the plaintext code.
    List {
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Revoke one invite code (32-hex code id from `invite list`) and
    /// forget the local secret.
    Revoke {
        /// Code id, 32 hex characters.
        code_id: String,
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
    /// Redeem an invite code (AKI1-...), pair with its owner, then wait
    /// receiving into --out (the owner lands in peers.book first, so the
    /// accept policy is book-only).
    Redeem {
        /// The invite code in its canonical AKI1-... form.
        code: String,
        /// Destination directory for received transfers (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// Handle exactly one transfer after pairing, then exit.
        #[arg(long, default_value_t = false)]
        once: bool,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
        /// Idle timeout in seconds per job.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        #[arg(long, requires = "registry_fp")]
        server: Option<SocketAddr>,
        #[arg(long, requires = "server")]
        registry_fp: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    /// List checkpointed jobs, most recently updated first.
    List,
    /// Show one job's checkpoint in detail: direction, state, root, peer,
    /// totals and every file's verified prefix (first 20 file rows, then a
    /// count). Errors clearly when the job id is unknown.
    Status {
        /// Job id, 32 hex characters (copy from `ak-cli jobs list`).
        job: String,
    },
    /// Delete checkpoint record files (.akjob). Part files (.akpart) survive
    /// so interrupted transfers can still resume.
    Clean {
        /// Delete every checkpoint record, finished or not.
        #[arg(long, default_value_t = false)]
        all: bool,
        /// Delete only records of finished transfers (state completed).
        #[arg(long, default_value_t = false)]
        completed: bool,
    },
}

// ---------------------------------------------------------------------------
// --bind value shapes (receive / signal-server listen overrides)
// ---------------------------------------------------------------------------

/// Parsed form of a `--bind` / `--stun-bind` value: either a bare IP whose
/// port comes from the verb's port flag, or a full socket address carrying
/// its own port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindArg {
    /// Bare IPv4/IPv6 address (`0.0.0.0`, `::`, `192.168.1.5`).
    Ip(IpAddr),
    /// Full ip:port / `[v6]:port` (`0.0.0.0:7100`, `[::]:7101`).
    Addr(SocketAddr),
}

impl BindArg {
    /// Accept a full socket address first (`[::]:7100`, `1.2.3.4:5`), then a
    /// bare IP (`::`, `0.0.0.0`); anything else is an explicit error naming
    /// the expected shapes. Whitespace is trimmed like every other value.
    fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        if let Ok(addr) = trimmed.parse::<SocketAddr>() {
            return Ok(BindArg::Addr(addr));
        }
        if let Ok(ip) = trimmed.parse::<IpAddr>() {
            return Ok(BindArg::Ip(ip));
        }
        bail!(
            "invalid bind address {trimmed:?}: expected a bare IP (`::`, `0.0.0.0`, \
             `192.168.1.5`) or ip:port (`[::]:7100`, `0.0.0.0:7100`)"
        )
    }

    /// The final listen address, combining a bare IP with `fallback_port`.
    fn into_addr(self, fallback_port: u16) -> SocketAddr {
        match self {
            BindArg::Ip(ip) => SocketAddr::new(ip, fallback_port),
            BindArg::Addr(addr) => addr,
        }
    }
}

/// clap value parser bridging [`BindArg::parse`] errors into help output.
fn bind_arg_parser(text: &str) -> Result<BindArg, String> {
    BindArg::parse(text).map_err(|err| err.to_string())
}

/// clap value parser for one `--to` address item (the flag itself splits on
/// commas via `value_delimiter`). Whitespace around an item is trimmed so
/// `--to "1.2.3.4:7000, 5.6.7.8:7000"` parses; anything that is not a full
/// socket address is an explicit error naming the expected shapes.
fn to_addr_parser(text: &str) -> Result<SocketAddr, String> {
    let trimmed = text.trim();
    trimmed.parse::<SocketAddr>().map_err(|_| {
        format!(
            "invalid peer address {trimmed:?}: expected ip:port (`1.2.3.4:7100`) or \
             [v6]:port (`[2001:db8::1]:7100`); comma-separate multiple candidates"
        )
    })
}

/// Combine a bind flag with the (optional, explicitly given) port flag and
/// the verb's default port into the final listen address. When the bind
/// value carries its own port AND a different explicit port flag was given,
/// the bind port wins and a notice naming the ignored flag is returned;
/// bare-IP binds combine with the port flag by design (no notice).
fn resolve_listen_addr(
    bind: Option<BindArg>,
    bind_flag: &str,
    port_flag: Option<u16>,
    port_flag_name: &str,
    default_port: u16,
) -> (SocketAddr, Option<String>) {
    match bind {
        None => (
            SocketAddr::from(([0, 0, 0, 0], port_flag.unwrap_or(default_port))),
            None,
        ),
        Some(arg @ BindArg::Ip(_)) => (arg.into_addr(port_flag.unwrap_or(default_port)), None),
        Some(BindArg::Addr(addr)) => {
            let note = match port_flag {
                Some(explicit) if explicit != addr.port() => Some(format!(
                    "note: {bind_flag} includes its own port {}; ignoring \
                     {port_flag_name} {explicit}",
                    addr.port()
                )),
                _ => None,
            };
            (addr, note)
        }
    }
}

// ---------------------------------------------------------------------------
// completions (clap_complete; the four shells users asked for)
// ---------------------------------------------------------------------------

/// The shells `ak-cli completions` generates (clap_complete also speaks
/// elvish, which nobody requested; keeping our own enum pins the CLI
/// surface independently of the generator's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl ShellKind {
    /// Map onto the generator's shell enum (same names, one type boundary).
    fn generator_shell(self) -> clap_complete::aot::Shell {
        match self {
            ShellKind::Bash => clap_complete::aot::Shell::Bash,
            ShellKind::Zsh => clap_complete::aot::Shell::Zsh,
            ShellKind::Fish => clap_complete::aot::Shell::Fish,
            ShellKind::Powershell => clap_complete::aot::Shell::PowerShell,
        }
    }
}

/// Render one shell's completion script into a string (shared by the
/// command and the tests; generation writes nothing on its own).
fn completions_script(shell: ShellKind) -> Result<String> {
    let mut cmd = Cli::command();
    let mut buf: Vec<u8> = Vec::new();
    clap_complete::aot::generate(shell.generator_shell(), &mut cmd, "ak-cli", &mut buf);
    String::from_utf8(buf).context("completion script must be UTF-8")
}

/// `completions <shell>`: print the script so shells can consume the pipe.
fn completions_command(shell: ShellKind) -> Result<()> {
    let script = completions_script(shell)?;
    print!("{script}");
    let _ = std::io::stdout().flush();
    Ok(())
}

fn id_dir(cli: &Cli) -> PathBuf {
    let base = id_dir_path(cli);
    std::fs::create_dir_all(&base).ok();
    base
}

/// The identity directory `id_dir` resolves to, WITHOUT creating it (the
/// read-only `registry-id` must not leave a directory behind).
fn id_dir_path(cli: &Cli) -> PathBuf {
    cli.id_dir.clone().unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".ak-sending")
    })
}

fn parse_fingerprint(text: &str) -> Result<Fingerprint> {
    let trimmed = text.trim();
    if trimmed.len() == 64 {
        Fingerprint::from_hex(trimmed).context("fingerprint must be 64 hex characters")
    } else {
        bail!(
            "unknown fingerprint format: expected 64 hex characters \
             (print it on the peer device with `ak-cli pair code`)"
        )
    }
}

/// One accepted fingerprint-argument shape for [`resolve_fingerprint_arg`].
const FP_MIN_PREFIX: usize = 8;

/// Resolve a fingerprint argument shared by `send --fp`, `pair remove` and
/// `receive --expect`: a full 64-hex value parses directly (no book access
/// needed); an 8..63 hex-character prefix must match exactly one peers.book
/// entry and is completed to that entry. Zero matches, ambiguity and
/// malformed input are explicit errors.
fn resolve_fingerprint_arg(text: &str, book_path: &Path) -> Result<Fingerprint> {
    let trimmed = text.trim();
    let is_hex = !trimmed.is_empty() && trimmed.bytes().all(|b: u8| b.is_ascii_hexdigit());
    if !is_hex {
        bail!(
            "unknown fingerprint format: expected 64 hex characters \
             (print it on the peer device with `ak-cli pair code`)"
        );
    }
    if trimmed.len() == 64 {
        return Fingerprint::from_hex(trimmed).context("fingerprint must be 64 hex characters");
    }
    if (FP_MIN_PREFIX..64).contains(&trimmed.len()) {
        let prefix = trimmed.to_ascii_lowercase();
        let book = load_peer_book(book_path)?;
        let hits: Vec<&PeerRecord> = book
            .iter()
            .filter(|record| record.fingerprint.to_hex().starts_with(&prefix))
            .collect();
        return match hits.as_slice() {
            [only] => Ok(only.fingerprint),
            [] => bail!(
                "fingerprint prefix {prefix} not found in the peer book {}; the sending \
                 side needs the full 64-hex fingerprint (see `ak-cli pair list`)",
                book_path.display()
            ),
            many => bail!(
                "fingerprint prefix {prefix} is ambiguous: matches {} peers ({}); add more \
                 hex characters or use the full 64-hex fingerprint",
                many.len(),
                many.iter()
                    .map(|record| record.fingerprint.short_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    bail!(
        "fingerprint must be the full 64 hex characters or a unique prefix of \
         {FP_MIN_PREFIX} to 63 characters (got {})",
        trimmed.len()
    );
}

// ---------------------------------------------------------------------------
// Peer book helpers
// ---------------------------------------------------------------------------

pub(crate) fn peer_book_path(id: &Path) -> PathBuf {
    id.join("peers.book")
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Load peers.book; a missing file means "no peers yet", corruption is fatal.
pub(crate) fn load_peer_book(path: &Path) -> Result<PeerBook> {
    match PeerBook::load(path) {
        Ok(book) => Ok(book),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(PeerBook::default()),
        Err(err) => Err(err).with_context(|| format!("cannot load peer book {}", path.display())),
    }
}

/// Insert a peer or update its name. Returns true when newly added; an
/// existing entry keeps its original paired time unless renamed.
fn upsert_peer(book: &mut PeerBook, fingerprint: &Fingerprint, name: Option<&str>) -> bool {
    if let Some(existing) = book.get(fingerprint) {
        if let Some(name) = name {
            book.upsert(PeerRecord {
                fingerprint: *fingerprint,
                name: name.to_owned(),
                paired_unix_ms: existing.paired_unix_ms,
            });
        }
        return false;
    }
    book.upsert(PeerRecord {
        fingerprint: *fingerprint,
        name: name.unwrap_or("").to_owned(),
        paired_unix_ms: now_unix_ms(),
    });
    true
}

/// `pair add`: persist the fingerprint, return it plus whether it is new.
fn pair_add(id: &Path, fingerprint_text: &str, name: Option<&str>) -> Result<(Fingerprint, bool)> {
    let fingerprint = parse_fingerprint(fingerprint_text)?;
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let added = upsert_peer(&mut book, &fingerprint, name);
    book.save(&path)
        .with_context(|| format!("cannot save {}", path.display()))?;
    Ok((fingerprint, added))
}

/// `pair remove`: drop the fingerprint (full 64-hex or a unique peers.book
/// prefix); error when it was never paired.
fn pair_remove(id: &Path, fingerprint_text: &str) -> Result<Fingerprint> {
    let path = peer_book_path(id);
    let fingerprint = resolve_fingerprint_arg(fingerprint_text, &path)?;
    let mut book = load_peer_book(&path)?;
    if !book.remove(&fingerprint) {
        bail!(
            "fingerprint {} is not in {}; nothing removed (see `pair list`)",
            fingerprint.short_id(),
            path.display()
        );
    }
    book.save(&path)
        .with_context(|| format!("cannot save {}", path.display()))?;
    Ok(fingerprint)
}

/// After a successful send, record the peer trust-on-first-use. Returns true
/// when the book gained a new entry (existing entries keep their name).
fn record_peer_after_send(id: &Path, fingerprint: &Fingerprint) -> Result<bool> {
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let added = upsert_peer(&mut book, fingerprint, None);
    if added {
        book.save(&path)
            .with_context(|| format!("cannot save {}", path.display()))?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// pair export / import (AKPAIR1 public-only pairing block)
// ---------------------------------------------------------------------------

/// First line of every exported pairing block (format version tag).
const PAIR_EXPORT_MAGIC: &str = "AKPAIR1";

/// The PUBLIC content of one AKPAIR1 block: who exported it (informational
/// on import; never merged into anything) plus the peer entries to merge.
/// SECURITY RED LINE (AGENTS.md): this format carries only public values --
/// device id, fingerprint, pairing code, peer book entries. The device.key
/// file (the 32-byte private seed) must never appear in it; tests assert a
/// sentinel seed's hex is absent from every export.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedPairBlock {
    /// Exporting device's public key, 64 hex characters.
    device: Option<String>,
    /// Exporting device's fingerprint, 64 hex characters.
    fingerprint: Option<String>,
    /// Exporting device's pairing code, "ddd-ddd".
    code: Option<String>,
    /// Valid peer lines, already deduped in-block (newer paired time wins).
    peers: Vec<PeerRecord>,
}

/// `pair export`: render this device's public pairing data as an AKPAIR1
/// block. Everything in the output is already shown by `pair code` and
/// `pair list`; no private material is read besides loading the key pair
/// whose public half becomes the `device` line.
fn pair_export_text(id: &Path) -> Result<String> {
    let (key, _) = load_identity(id)?;
    let fingerprint = key.fingerprint();
    let book = load_peer_book(&peer_book_path(id))?;
    let mut out = String::from(PAIR_EXPORT_MAGIC);
    out.push('\n');
    out.push_str(&format!("device {}\n", hex_of(&key.public_key())));
    out.push_str(&format!("fingerprint {}\n", fingerprint.to_hex()));
    out.push_str(&format!(
        "code {}\n",
        ak_identity::pairing_code(&fingerprint)
    ));
    for peer in book.iter() {
        out.push_str(&format!(
            "peer {} {} {}\n",
            peer.fingerprint.to_hex(),
            peer.paired_unix_ms,
            export_name(&peer.name)
        ));
    }
    Ok(out)
}

/// Tab/newline bytes would break the one-line-per-record format, so they
/// become spaces exactly like ak-identity does when saving peers.book.
fn export_name(name: &str) -> String {
    name.chars()
        .map(|c| matches!(c, '\t' | '\n' | '\r').then_some(' ').unwrap_or(c))
        .collect()
}

/// Upsert `record` by fingerprint keeping the NEWER paired time; an exact
/// tie keeps the existing entry, so importing a block twice never rewrites
/// the book. The whole winning record (name included) replaces the loser.
fn upsert_newer(book: &mut PeerBook, record: PeerRecord) {
    match book.get(&record.fingerprint) {
        Some(existing) if existing.paired_unix_ms >= record.paired_unix_ms => {}
        _ => book.upsert(record),
    }
}

/// Parse one `peer <fingerprint> <paired_unix_ms> [name...]` line. The name
/// is everything after the single space following the timestamp (it may be
/// empty or contain spaces); names carrying tab/newline bytes can never have
/// come from our exporter and are rejected.
fn parse_peer_line(rest: &str) -> Option<PeerRecord> {
    let mut parts = rest.splitn(3, ' ');
    let fingerprint = Fingerprint::from_hex(parts.next()?)?;
    let paired_unix_ms: u64 = parts.next()?.parse().ok()?;
    let name = parts.next().unwrap_or("");
    if name.contains('\t') || name.contains('\n') || name.contains('\r') {
        return None;
    }
    Some(PeerRecord {
        fingerprint,
        name: name.to_owned(),
        paired_unix_ms,
    })
}

/// Pairing codes are always "ddd-ddd"; anything else cannot be a code line.
fn pairing_code_shape_ok(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 7
        && bytes[..3].iter().all(|b| b.is_ascii_digit())
        && bytes[3] == b'-'
        && bytes[4..].iter().all(|b| b.is_ascii_digit())
}

/// Parse an AKPAIR1 block. The first line must be the magic (anything else
/// is a fatal error: the block is not ours to interpret); every later line
/// is validated individually, with malformed lines counted (never fatal) so
/// a partially corrupted block still merges its intact records.
fn parse_pair_block(text: &str) -> Result<(ParsedPairBlock, usize)> {
    let mut lines = text.lines();
    match lines.next().map(|line| line.trim_end_matches('\r')) {
        Some(PAIR_EXPORT_MAGIC) => {}
        Some(other) => bail!(
            "not an AKPAIR1 pairing block (first line is {other:?}); generate it with \
             `ak-cli pair export` on the source device"
        ),
        None => bail!("empty input: expected an AKPAIR1 pairing block"),
    }

    let mut block = ParsedPairBlock::default();
    let mut staged = PeerBook::default();
    let mut bad = 0usize;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue; // tolerate blank lines and trailing newline noise
        }
        if let Some(rest) = line.strip_prefix("device ") {
            let value = rest.trim();
            if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
                block.device = Some(value.to_ascii_lowercase());
            } else {
                bad += 1;
            }
        } else if let Some(rest) = line.strip_prefix("fingerprint ") {
            match Fingerprint::from_hex(rest.trim()) {
                Some(_) => block.fingerprint = Some(rest.trim().to_ascii_lowercase()),
                None => bad += 1,
            }
        } else if let Some(rest) = line.strip_prefix("code ") {
            let value = rest.trim();
            if pairing_code_shape_ok(value) {
                block.code = Some(value.to_owned());
            } else {
                bad += 1;
            }
        } else if let Some(rest) = line.strip_prefix("peer ") {
            match parse_peer_line(rest) {
                Some(record) => upsert_newer(&mut staged, record),
                None => bad += 1,
            }
        } else {
            bad += 1;
        }
    }
    block.peers = staged.iter().cloned().collect();
    Ok((block, bad))
}

/// What one `pair import` changed, for both the user report and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
struct PairImportOutcome {
    /// Distinct valid peer records the block carried (after in-block dedup).
    peers: usize,
    added: usize,
    updated: usize,
    unchanged: usize,
    /// Malformed non-header lines skipped.
    bad: usize,
    /// Short id (first 8 hex) of the exporting device, when the block says.
    source_short: Option<String>,
}

/// `pair import`: validate the block, merge its peers into peers.book by
/// fingerprint keeping the newer paired time, and report what happened. The
/// book is rewritten only when something actually changed.
fn pair_import_text(id: &Path, text: &str) -> Result<PairImportOutcome> {
    let (block, bad) = parse_pair_block(text)?;
    let path = peer_book_path(id);
    let mut book = load_peer_book(&path)?;
    let mut outcome = PairImportOutcome {
        bad,
        source_short: block.device.as_ref().map(|hex| hex[..8].to_owned()),
        ..PairImportOutcome::default()
    };
    for record in block.peers {
        outcome.peers += 1;
        match book.get(&record.fingerprint) {
            None => {
                book.upsert(record);
                outcome.added += 1;
            }
            Some(existing) if existing.paired_unix_ms >= record.paired_unix_ms => {
                outcome.unchanged += 1;
            }
            Some(_) => {
                book.upsert(record);
                outcome.updated += 1;
            }
        }
    }
    if outcome.added + outcome.updated > 0 {
        book.save(&path)
            .with_context(|| format!("cannot save {}", path.display()))?;
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Receive accept policy (--expect and the paired-peers default)
// ---------------------------------------------------------------------------

/// Accept policy for `receive`. The book policy re-reads peers.book on every
/// connection so `pair add` takes effect without restarting the loop.
#[derive(Debug, Clone)]
enum ExpectPolicy {
    Any,
    Pinned(Fingerprint),
    PairingCode(String),
    Book(PathBuf),
}

impl ExpectPolicy {
    fn parse(text: &str, book_path: &Path) -> Result<Self> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(ExpectPolicy::Any);
        }
        // Explicit `book` (ffi-v2 §6 note: keep the omitted-default
        // semantics but accept the word callers copy from the contract).
        if trimmed.eq_ignore_ascii_case("book") {
            return Ok(ExpectPolicy::Book(book_path.to_path_buf()));
        }
        let all_code_chars = trimmed
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'-' | b' '));
        let digits: String = trimmed
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .collect();
        if all_code_chars && digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(ExpectPolicy::PairingCode(trimmed.to_owned()));
        }
        // Full 64-hex values and unique peers.book prefixes share one path.
        let pinned = resolve_fingerprint_arg(trimmed, book_path)
            .with_context(|| format!("invalid --expect {trimmed:?}"))?;
        Ok(ExpectPolicy::Pinned(pinned))
    }

    fn allows(&self, peer: &Fingerprint) -> bool {
        match self {
            ExpectPolicy::Any => true,
            ExpectPolicy::Pinned(pinned) => peer == pinned,
            ExpectPolicy::PairingCode(code) => ak_identity::verify_pairing_code(peer, code),
            ExpectPolicy::Book(path) => PeerBook::load(path)
                .map(|book| book.get(peer).is_some())
                .unwrap_or(false),
        }
    }

    /// Engine-facing consent callback: one shared evaluation path, cloned per
    /// loop iteration so book edits apply to later connections.
    fn authorize(&self) -> Box<dyn Fn(&Fingerprint) -> bool + Send + Sync> {
        let policy = self.clone();
        Box::new(move |peer| policy.allows(peer))
    }
}

/// Resolve the receive accept policy. With --expect omitted, peers.book must
/// hold at least one peer; otherwise the user must authorize explicitly.
fn resolve_expect(text: Option<&str>, book_path: &Path) -> Result<ExpectPolicy> {
    match text {
        Some(text) => ExpectPolicy::parse(text, book_path),
        None => {
            let book = load_peer_book(book_path)?;
            if book.iter().next().is_none() {
                bail!(
                    "no paired peers in {}: pair first with \
                     `ak-cli pair add <peer-fingerprint> --name NAME` (print it on the peer \
                     device with `ak-cli pair code`), or authorize this receive explicitly \
                     with `--expect <fingerprint|any>`",
                    book_path.display()
                );
            }
            Ok(ExpectPolicy::Book(book_path.to_path_buf()))
        }
    }
}

// ---------------------------------------------------------------------------
// Job checkpoints (read-only view of the AKJOB1 files the engine writes)
// ---------------------------------------------------------------------------

/// One `file <index> <size> <verified prefix>` line of an AKJOB1 record
/// (the on-disk twin of ak-store's FileCheckpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileProgress {
    file_index: u32,
    size: u64,
    verified_prefix_chunks: u64,
}

/// The display-relevant slice of one `*.akjob` checkpoint record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobEntry {
    job_hex: String,
    direction: &'static str,
    root: String,
    peer_short: Option<String>,
    chunk_size: u32,
    total_files: u64,
    total_bytes: u64,
    state: String,
    created_unix_ms: u64,
    updated_unix_ms: u64,
    files: Vec<FileProgress>,
}

fn job_field<'a>(lines: &mut impl Iterator<Item = &'a str>, key: &str) -> Option<&'a str> {
    lines.next()?.strip_prefix(key)?.strip_prefix(' ')
}

/// Parse one AKJOB1 record (the format ak-store's FileCheckpointStore
/// writes). Unparsable input yields None; callers skip such files.
fn parse_job_record(text: &str) -> Option<JobEntry> {
    let mut lines = text.lines();
    if lines.next()? != "AKJOB1" {
        return None;
    }
    let job_hex = job_field(&mut lines, "job")?.to_owned();
    if hex_to_bytes(&job_hex).ok()?.len() != 16 {
        return None;
    }
    let direction = match job_field(&mut lines, "dir")? {
        "send" => "send",
        "receive" => "receive",
        _ => return None,
    };
    let root_hex = job_field(&mut lines, "root")?;
    let root = String::from_utf8(hex_to_bytes(root_hex).ok()?).ok()?;
    let peer = job_field(&mut lines, "peer")?;
    let peer_short = if peer == "-" {
        None
    } else {
        Fingerprint::from_hex(peer)?;
        Some(peer[..8].to_owned())
    };
    let chunk_size: u32 = job_field(&mut lines, "chunk")?.parse().ok()?;
    let total = job_field(&mut lines, "total")?;
    let mut total_parts = total.split(' ');
    let total_files: u64 = total_parts.next()?.parse().ok()?;
    let total_bytes: u64 = total_parts.next()?.parse().ok()?;
    let state = job_field(&mut lines, "state")?.to_owned();
    let created_unix_ms: u64 = job_field(&mut lines, "created")?.parse().ok()?;
    let updated_unix_ms: u64 = job_field(&mut lines, "updated")?.parse().ok()?;
    let mut files = Vec::new();
    for line in lines {
        let rest = line.strip_prefix("file ")?;
        let mut parts = rest.split(' ');
        let file_index: u32 = parts.next()?.parse().ok()?;
        let size: u64 = parts.next()?.parse().ok()?;
        let verified_prefix_chunks: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        files.push(FileProgress {
            file_index,
            size,
            verified_prefix_chunks,
        });
    }
    Some(JobEntry {
        job_hex,
        direction,
        root,
        peer_short,
        chunk_size,
        total_files,
        total_bytes,
        state,
        created_unix_ms,
        updated_unix_ms,
        files,
    })
}

/// All parsable `*.akjob` files under `dir`, newest first, with their paths.
fn list_job_files(dir: &Path) -> Vec<(PathBuf, JobEntry)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(record) = parse_job_record(&text) {
            out.push((path, record));
        }
    }
    out.sort_by_key(|(_, record)| std::cmp::Reverse(record.updated_unix_ms));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanScope {
    All,
    Completed,
}

/// Delete checkpoint record files per scope. Returns (removed, kept).
/// Part files (.akpart) are never touched, so interrupted transfers stay
/// resumable even after `--all`.
fn clean_job_records(dir: &Path, scope: CleanScope) -> Result<(usize, usize)> {
    let mut removed = 0usize;
    let mut kept = 0usize;
    match scope {
        CleanScope::All => {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((0, 0));
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("cannot read {}", dir.display()))
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
        CleanScope::Completed => {
            for (path, record) in list_job_files(dir) {
                if record.state != "completed" {
                    kept += 1;
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => kept += 1,
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
    }
    Ok((removed, kept))
}

// ---------------------------------------------------------------------------
// Small display helpers
// ---------------------------------------------------------------------------

/// Whole days since epoch -> civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

fn format_utc_ms(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Two-line fingerprint display used everywhere: short 8 first, full second.
pub(crate) fn fingerprint_lines(label: &str, fingerprint: &Fingerprint) -> String {
    format!(
        "{label}: {}\n{label} (full): {}",
        fingerprint.short_id(),
        fingerprint.to_hex()
    )
}

fn peer_display_name(name: &str) -> &str {
    if name.is_empty() {
        "(unnamed)"
    } else {
        name
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Escape a string for embedding in a JSON string literal.
pub(crate) fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => out.push(other),
        }
    }
    out
}

fn source_name(source: ak_discovery::CandidateSource) -> &'static str {
    match source {
        ak_discovery::CandidateSource::Manual => "manual",
        ak_discovery::CandidateSource::Signaling => "signaling",
        ak_discovery::CandidateSource::Mdns => "mdns",
    }
}

/// The fixed text printed for the probe SameEgress hint.
// G5 / E1: the probe (legacy plaintext signal client) is feature-gated;
// its one-shot notice helper is gated with it.
#[cfg(feature = "legacy-plaintext-signal")]
const SAME_EGRESS_NOTICE: &str = "same egress detected: prefer LAN candidates first";

/// One-shot guard for repeated events that should be announced exactly once:
/// [`OnceNotice::take`] returns the notice text the first time and None for
/// every later hit.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Default)]
struct OnceNotice {
    shown: bool,
}

#[cfg(feature = "legacy-plaintext-signal")]
impl OnceNotice {
    fn new() -> Self {
        OnceNotice { shown: false }
    }

    fn take(&mut self) -> Option<&'static str> {
        if self.shown {
            None
        } else {
            self.shown = true;
            Some(SAME_EGRESS_NOTICE)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared command plumbing
// ---------------------------------------------------------------------------

fn conflict_policy(text: &str) -> Result<ConflictPolicy> {
    match text {
        "error" => Ok(ConflictPolicy::Fail),
        "rename" => Ok(ConflictPolicy::Rename),
        other => bail!("unknown conflict policy {other:?}; use error|rename"),
    }
}

/// Map the `--timeout` flag (seconds) onto the engine idle timeout. The flag
/// is an idle budget, not a total run time; clap already rejects 0.
fn idle_timeout_from_secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn checkpoint_dir(id: &Path) -> PathBuf {
    id.join("checkpoints")
}

pub(crate) fn load_identity(id: &Path) -> Result<(DeviceKeyPair, TlsBundle)> {
    let (key, created) = DeviceKeyPair::load_or_create(&id.join("device.key"))
        .with_context(|| format!("cannot load device key in {}", id.display()))?;
    if created {
        eprintln!(
            "created new device key in {}",
            id.join("device.key").display()
        );
    }
    let bundle = ak_identity::tls::make_self_signed(&key);
    Ok((key, bundle))
}

fn print_identity(key: &DeviceKeyPair) {
    let fingerprint = key.fingerprint();
    println!("device id: {}", hex_of(&key.public_key()));
    println!("{}", fingerprint_lines("fingerprint", &fingerprint));
    println!("pairing code: {}", ak_identity::pairing_code(&fingerprint));
}

fn print_progress(direction: &str, progress: &Progress) {
    match progress {
        Progress::Connected { peer } => println!("[{direction}] connected; peer {peer}"),
        Progress::Offered { files, bytes } => {
            println!(
                "[{direction}] offered {files} entries, {}",
                human_bytes(*bytes)
            )
        }
        Progress::Accepted { files, bytes } => {
            println!(
                "[{direction}] accepted {files} entries, {}",
                human_bytes(*bytes)
            )
        }
        Progress::Resuming { chunks } => {
            println!("[{direction}] resuming after {chunks} verified chunks")
        }
        Progress::FileStarted { path, bytes, .. } => {
            println!("[{direction}] {path} ({})", human_bytes(*bytes))
        }
        Progress::FileCommitted { path, .. } => println!("[{direction}] committed {path}"),
        Progress::Paused => println!("[{direction}] paused by peer; holding (no fallback exists)"),
        Progress::Resumed => println!("[{direction}] resumed"),
        Progress::FinalAck { verified } => println!("[{direction}] final ack verified={verified}"),
        Progress::Done => println!("[{direction}] done"),
        Progress::ManifestReceived => println!("[{direction}] manifest received"),
        _ => {}
    }
}

/// Terminal width the byte-progress line is clipped/padded to (80-col
/// friendly; the padding also erases leftovers of a previous, longer line).
const PROGRESS_LINE_WIDTH: usize = 79;

/// Clip text to `width` display columns on a char boundary, marking the cut
/// with "..." when the width allows it.
fn clip_line(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width >= 3 {
        let mut clipped: String = text.chars().take(width - 3).collect();
        clipped.push_str("...");
        clipped
    } else {
        text.chars().take(width).collect()
    }
}

/// Byte-level progress renders as one `\r`-refreshed line instead of one
/// println per event (the engine emits BytesAcked every 16 acks, which used
/// to flood the terminal). Whole-line events (Done, file-level, ...) first
/// erase an open progress line so log lines stay readable.
pub(crate) struct ProgressLine {
    direction: &'static str,
    open: std::sync::atomic::AtomicBool,
}

impl ProgressLine {
    pub(crate) fn new(direction: &'static str) -> Self {
        ProgressLine {
            direction,
            open: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn show(&self, progress: &Progress) {
        if let Progress::BytesAcked {
            file_index,
            cumulative_bytes,
            ..
        } = progress
        {
            let text = clip_line(
                &format!(
                    "[{}] file {file_index}: {} acknowledged",
                    self.direction,
                    human_bytes(*cumulative_bytes)
                ),
                PROGRESS_LINE_WIDTH,
            );
            let pad = " ".repeat(PROGRESS_LINE_WIDTH.saturating_sub(text.chars().count()));
            print!("\r{text}{pad}");
            let _ = std::io::stdout().flush();
            self.open.store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        // Whole-line event: clear any open progress line, keep it whole.
        if self.open.swap(false, std::sync::atomic::Ordering::Relaxed) {
            print!("\r{}\r", " ".repeat(PROGRESS_LINE_WIDTH));
        }
        print_progress(self.direction, progress);
    }
}

pub(crate) fn progress_printer(direction: &'static str) -> Arc<dyn Fn(Progress) + Send + Sync> {
    let line = Arc::new(ProgressLine::new(direction));
    Arc::new(move |progress: Progress| {
        if std::env::var_os("AK_QUIET").is_none() {
            line.show(&progress);
        }
    })
}

/// Progress adapter for concurrent receive: job-prefixed whole-line events
/// only (per-job single-line refresh would garble when jobs interleave, so
/// byte-level ticks are skipped in this mode).
fn progress_printer_multi() -> Arc<dyn Fn(u64, Progress) + Send + Sync> {
    Arc::new(move |job: u64, progress: Progress| {
        if std::env::var_os("AK_QUIET").is_some() {
            return;
        }
        if matches!(progress, Progress::BytesAcked { .. }) {
            return;
        }
        let direction = format!("recv#{job}");
        print_progress(&direction, &progress);
    })
}

fn short_of(peer: &Option<String>) -> String {
    peer.as_deref()
        .map(|hex| hex.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "?".to_owned())
}

/// One-line root listing for the summary: `roots: a, b, c` (primary first,
/// then extras in add order). None when the report carries no roots.
fn roots_line(report: &Report) -> Option<String> {
    if report.roots.is_empty() {
        None
    } else {
        Some(format!("roots: {}", report.roots.join(", ")))
    }
}

pub(crate) fn report_summary(report: &Report) {
    println!(
        "job {} complete: {} entries, {}, resumed {} chunks, peer {}",
        report
            .job_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        report.files,
        human_bytes(report.bytes),
        report.resumed_chunks,
        short_of(&report.peer_fingerprint)
    );
    if let Some(line) = roots_line(report) {
        println!("{line}");
    }
}

// ---------------------------------------------------------------------------
// bench: in-process loopback throughput measurement (evidence generator)
// ---------------------------------------------------------------------------

/// Fixed PRNG seed: identical bench runs generate identical bytes, so
/// numbers are comparable across runs and machines.
const BENCH_SEED: u64 = 0x00C0_FFEE_1234_5678;
/// Printed with every bench result so nobody mistakes loopback for a NIC.
const BENCH_DISCLAIMER: &str = "loopback throughput; real-NIC measurement needs two machines";
/// Idle budget for the bench engine runs; generous because a bench must
/// measure throughput, not trigger timeouts on a busy machine.
const BENCH_IDLE: Duration = Duration::from_secs(120);

/// Deterministic xorshift64 filler (Marsaglia). Fast and dependency-free;
/// NOT cryptographic -- it only needs pseudo-random bytes to exercise the
/// hashing, framing and QUIC paths.
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    /// A zero state would freeze the generator; map it to a fixed nonzero
    /// seed so every input produces a working stream.
    fn new(seed: u64) -> Self {
        Xorshift64 {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Fill `buf` with little-endian words of the generator stream; a
    /// trailing partial word takes the low bytes.
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

/// Split `total` bytes into exactly `files` sizes: even quotient, remainder
/// spread one byte at a time over the first files. Always sums to `total`.
fn bench_file_sizes(total: u64, files: u32) -> Vec<u64> {
    let base = total / u64::from(files);
    let mut rem = total % u64::from(files);
    (0..files)
        .map(|_| {
            if rem > 0 {
                rem -= 1;
                base + 1
            } else {
                base
            }
        })
        .collect()
}

/// Best-effort scoped temp directory for bench artifacts (source tree,
/// per-round destinations and checkpoints). The tempfile crate is a
/// dev-dependency of this crate and cannot be used here, hence the manual
/// guard.
struct BenchTempDir(PathBuf);

impl BenchTempDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "ak-cli-bench-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("cannot create bench dir {}", path.display()))?;
        Ok(BenchTempDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for BenchTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stream `size` bytes of PRNG output into one file in 1 MiB steps (the
/// default 256 MiB must never be materialized in memory as a whole).
fn write_bench_file(path: &Path, size: u64, prng: &mut Xorshift64) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let step = remaining.min(buf.len() as u64) as usize;
        prng.fill(&mut buf[..step]);
        file.write_all(&buf[..step])?;
        remaining -= step as u64;
    }
    Ok(())
}

/// Generate the flat bench source tree (bench-0000.bin, ...). Per-file seeds
/// are distinct so files do not share prefixes.
fn write_bench_source(src: &Path, sizes: &[u64]) -> Result<()> {
    std::fs::create_dir_all(src).with_context(|| format!("cannot create {}", src.display()))?;
    for (index, size) in sizes.iter().enumerate() {
        let path = src.join(format!("bench-{index:04}.bin"));
        let seed = BENCH_SEED
            ^ (index as u64)
                .wrapping_add(1)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut prng = Xorshift64::new(seed);
        write_bench_file(&path, *size, &mut prng)?;
    }
    Ok(())
}

fn bench_quiet_progress(_: Progress) {}

/// One completed bench round. `duration` is the engine's own Report.duration
/// (send side), so the measurement covers exactly what the engine measured.
#[derive(Debug, Clone)]
struct BenchRound {
    mode: &'static str,
    duration: Duration,
    bytes: u64,
}

/// Throughput of a round in decimal MB/s and binary GiB/s.
fn throughput(bytes: u64, duration: Duration) -> (f64, f64) {
    let secs = duration.as_secs_f64().max(1e-9);
    (
        bytes as f64 / 1_000_000.0 / secs,
        bytes as f64 / 1_073_741_824.0 / secs,
    )
}

/// Run one full sender->receiver transfer of `src` through the real engine
/// over loopback QUIC (fresh key pair, destination and checkpoint dirs per
/// round so nothing resumes from a previous round). Both sides run inside
/// this process; the data path is still the full QUIC + TLS + engine stack.
async fn bench_round(
    src: &Path,
    dst: &Path,
    checkpoints: &Path,
    chunk_kb: u32,
    multistream: bool,
) -> Result<BenchRound> {
    let sender_key = DeviceKeyPair::generate();
    let receiver_key = DeviceKeyPair::generate();
    let sender_bundle = ak_identity::tls::make_self_signed(&sender_key);
    let receiver_bundle = ak_identity::tls::make_self_signed(&receiver_key);
    let receiver_fp = receiver_key.fingerprint();
    let chunk = NonZeroU32::new(chunk_kb.saturating_mul(1024))
        .context("chunk size must be at least 1 KiB")?;

    let mut recv_spec = ReceiveSpec::new(dst.to_path_buf(), receiver_bundle);
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.chunk_size = chunk;
    recv_spec.checkpoint_dir = checkpoints.to_path_buf();
    recv_spec.device_name = "ak-bench".into();
    recv_spec.idle_timeout = BENCH_IDLE;
    // Loopback still pins both identities: only our own sender may connect.
    let sender_fp = sender_key.fingerprint();
    recv_spec.authorize = Box::new(move |fp: &Fingerprint| *fp == sender_fp);

    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move {
        ak_engine::receive_with_addr(recv_spec, &bench_quiet_progress, addr_tx).await
    });
    let addr = addr_rx
        .await
        .context("bench receiver failed to bind its loopback port")?;

    let mut send_spec = SendSpec::new(src.to_path_buf(), addr, receiver_fp, sender_bundle);
    send_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    send_spec.chunk_size = chunk;
    send_spec.checkpoint_dir = checkpoints.to_path_buf();
    send_spec.device_name = "ak-bench".into();
    send_spec.idle_timeout = BENCH_IDLE;
    send_spec.multistream = multistream;
    let report = ak_engine::send(send_spec, &bench_quiet_progress)
        .await
        .context("bench send failed")?;
    let recv_report = receiver
        .await
        .context("bench receiver task failed")?
        .context("bench receive failed")?;

    // A Report already implies whole-digest verification on the receive
    // side; the byte totals must also agree between both ends.
    if report.bytes != recv_report.bytes {
        bail!(
            "bench integrity mismatch: sent {} bytes but received {}",
            report.bytes,
            recv_report.bytes
        );
    }
    Ok(BenchRound {
        mode: if multistream { "multistream" } else { "single" },
        duration: report.duration,
        bytes: report.bytes,
    })
}

/// Machine-readable bench result (reuses the JSON string escaper).
fn bench_json(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    rounds: &[BenchRound],
    ratio: Option<f64>,
) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"bytes\": {bytes},\n"));
    out.push_str(&format!("  \"files\": {files},\n"));
    out.push_str(&format!("  \"chunk_kb\": {chunk_kb},\n"));
    out.push_str("  \"rounds\": [\n");
    for (index, round) in rounds.iter().enumerate() {
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        let comma = if index + 1 < rounds.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"mode\": \"{}\", \"duration_ms\": {:.3}, \"bytes\": {}, \
             \"mb_per_s\": {:.3}, \"gib_per_s\": {:.6}}}{}\n",
            json_escape(round.mode),
            round.duration.as_secs_f64() * 1000.0,
            round.bytes,
            mb_per_s,
            gib_per_s,
            comma
        ));
    }
    out.push_str("  ],\n");
    match ratio {
        Some(ratio) => out.push_str(&format!("  \"ratio\": {ratio:.4},\n")),
        None => out.push_str("  \"ratio\": null,\n"),
    }
    out.push_str(&format!(
        "  \"note\": \"{}\"\n}}",
        json_escape(BENCH_DISCLAIMER)
    ));
    out
}

async fn bench_command(
    bytes: u64,
    files: u32,
    chunk_kb: u32,
    multistream: bool,
    once: bool,
    json: bool,
) -> Result<()> {
    // Default: both rounds plus a comparison. --once runs exactly one round
    // (single-stream, or multistream when --multistream is also given).
    let run_single = !once || !multistream;
    let run_multi = !once || multistream;
    let mut plan: Vec<(&'static str, bool)> = Vec::new();
    if run_single {
        plan.push(("single-stream", false));
    }
    if run_multi {
        plan.push(("multistream", true));
    }

    let dir = BenchTempDir::new()?;
    let src = dir.path().join("src");
    let sizes = bench_file_sizes(bytes, files);
    eprintln!(
        "ak-cli bench: {} across {} files, chunk {chunk_kb} KiB; \
         generating deterministic source into {}",
        human_bytes(bytes),
        sizes.len(),
        src.display()
    );
    write_bench_source(&src, &sizes)?;

    let total = plan.len();
    let mut rounds: Vec<BenchRound> = Vec::new();
    for (index, (label, round_multistream)) in plan.iter().enumerate() {
        let dst = dir.path().join(format!("dst-{index}"));
        let checkpoints = dir.path().join(format!("ckpt-{index}"));
        std::fs::create_dir_all(&dst)
            .with_context(|| format!("cannot create {}", dst.display()))?;
        std::fs::create_dir_all(&checkpoints)
            .with_context(|| format!("cannot create {}", checkpoints.display()))?;
        eprintln!("round {}/{total} ({label}): running...", index + 1);
        let round = bench_round(&src, &dst, &checkpoints, chunk_kb, *round_multistream).await?;
        let (mb_per_s, gib_per_s) = throughput(round.bytes, round.duration);
        eprintln!(
            "round {}/{total} ({label}): {} in {:.3} s -> {:.2} MB/s ({:.4} GiB/s)",
            index + 1,
            human_bytes(round.bytes),
            round.duration.as_secs_f64(),
            mb_per_s,
            gib_per_s
        );
        rounds.push(round);
    }

    // Compare only when both modes ran (rounds[0]=single, rounds[1]=multi).
    let ratio = if rounds.len() == 2 {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        Some(multi_mb / single_mb.max(1e-9))
    } else {
        None
    };

    if json {
        println!("{}", bench_json(bytes, files, chunk_kb, &rounds, ratio));
        return Ok(());
    }
    if let Some(ratio) = ratio {
        let (single_mb, _) = throughput(rounds[0].bytes, rounds[0].duration);
        let (multi_mb, _) = throughput(rounds[1].bytes, rounds[1].duration);
        println!(
            "comparison: multistream {ratio:.2}x single-stream \
             ({multi_mb:.2} vs {single_mb:.2} MB/s)"
        );
    }
    println!("note: {BENCH_DISCLAIMER}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn pair_command(id: &Path, cmd: PairCommand) -> Result<()> {
    match cmd {
        PairCommand::Add { fingerprint, name } => {
            let (fp, added) = pair_add(id, &fingerprint, name.as_deref())?;
            let shown = name
                .as_deref()
                .map(str::to_owned)
                .unwrap_or_else(|| "(unnamed)".to_owned());
            if added {
                println!("paired {shown} ({})", fp.short_id());
            } else {
                println!(
                    "updated {shown} ({}) -- entry already existed",
                    fp.short_id()
                );
            }
            println!("fingerprint (full): {}", fp.to_hex());
            println!(
                "that peer's pairing code must be {} -- verify it on the peer device",
                ak_identity::pairing_code(&fp)
            );
            Ok(())
        }
        PairCommand::List => {
            let path = peer_book_path(id);
            let book = load_peer_book(&path)?;
            let count = book.iter().count();
            if count == 0 {
                println!("no paired peers yet; use `pair add <peer-fingerprint> --name NAME`");
                return Ok(());
            }
            println!("{count} paired peer(s) in {}", path.display());
            for peer in book.iter() {
                println!(
                    "{}  {}  paired {}",
                    peer_display_name(&peer.name),
                    peer.fingerprint.short_id(),
                    format_utc_ms(peer.paired_unix_ms)
                );
                println!("  fingerprint (full): {}", peer.fingerprint.to_hex());
            }
            Ok(())
        }
        PairCommand::Remove { fingerprint } => {
            let fp = pair_remove(id, &fingerprint)?;
            println!(
                "unpaired {} (removed from {})",
                fp.short_id(),
                peer_book_path(id).display()
            );
            Ok(())
        }
        PairCommand::Code => {
            let (key, _) = load_identity(id)?;
            print_identity(&key);
            Ok(())
        }
        PairCommand::Export => {
            // stdout carries ONLY the block so `ak-cli pair export > file`
            // round-trips through `pair import "$(cat file)"` untouched.
            let text = pair_export_text(id)?;
            print!("{text}");
            let _ = std::io::stdout().flush();
            Ok(())
        }
        PairCommand::Import { text } => {
            let outcome = pair_import_text(id, &text)?;
            let source = outcome
                .source_short
                .clone()
                .unwrap_or_else(|| "(unknown device)".to_owned());
            println!(
                "imported {} peer(s) from device {source} into {}: {} added, {} updated, \
                 {} unchanged",
                outcome.peers,
                peer_book_path(id).display(),
                outcome.added,
                outcome.updated,
                outcome.unchanged
            );
            if outcome.bad > 0 {
                println!(
                    "skipped {} malformed line(s); every valid line was still merged",
                    outcome.bad
                );
            }
            Ok(())
        }
    }
}

/// Per-file rows `jobs status` prints before folding the rest into a count.
const JOB_STATUS_FILE_ROWS: usize = 20;

/// Render the `jobs status <job-hex>` detail view for one checkpoint: the
/// record fields plus every file's verified prefix (first
/// [`JOB_STATUS_FILE_ROWS`] rows, then a count). Unknown or malformed job
/// ids are explicit errors.
fn jobs_status_text(dir: &Path, job_text: &str) -> Result<String> {
    let wanted = job_text.trim().to_ascii_lowercase();
    if !hex_to_bytes(&wanted).is_ok_and(|bytes| bytes.len() == 16) {
        bail!("job id must be 32 hex characters, got {job_text:?}");
    }
    let Some((_, record)) = list_job_files(dir)
        .into_iter()
        .find(|(_, record)| record.job_hex == wanted)
    else {
        bail!(
            "no checkpoint for job {wanted} in {} (see `ak-cli jobs list`)",
            dir.display()
        );
    };
    let mut out = format!("job {}\n", record.job_hex);
    out.push_str(&format!("direction: {}\n", record.direction));
    out.push_str(&format!("state:     {}\n", record.state));
    out.push_str(&format!("root:      {}\n", record.root));
    out.push_str(&format!(
        "peer:      {}\n",
        record.peer_short.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "totals:    {} files, {} bytes ({})\n",
        record.total_files,
        record.total_bytes,
        human_bytes(record.total_bytes)
    ));
    out.push_str(&format!("chunk:     {} bytes\n", record.chunk_size));
    out.push_str(&format!(
        "created:   {}\nupdated:   {}\n",
        format_utc_ms(record.created_unix_ms),
        format_utc_ms(record.updated_unix_ms)
    ));
    out.push_str(&format!(
        "files:     {} checkpointed file(s), verified prefix in chunks\n",
        record.files.len()
    ));
    for file in record.files.iter().take(JOB_STATUS_FILE_ROWS) {
        out.push_str(&format!(
            "  file {}: {} bytes, {} verified chunk(s)\n",
            file.file_index, file.size, file.verified_prefix_chunks
        ));
    }
    let hidden = record.files.len().saturating_sub(JOB_STATUS_FILE_ROWS);
    if hidden > 0 {
        out.push_str(&format!("  ... and {hidden} more file(s) not shown\n"));
    }
    Ok(out)
}

fn jobs_command(id: &Path, cmd: Option<JobsCommand>) -> Result<()> {
    let dir = checkpoint_dir(id);
    match cmd {
        None | Some(JobsCommand::List) => {
            let records = list_job_files(&dir);
            if records.is_empty() {
                println!("no job checkpoints in {}", dir.display());
                return Ok(());
            }
            println!("{} job checkpoint(s) in {}", records.len(), dir.display());
            for (_, record) in &records {
                println!(
                    "{} {} {} ({} files, {}) updated {}",
                    record.job_hex,
                    record.direction,
                    record.state,
                    record.total_files,
                    human_bytes(record.total_bytes),
                    format_utc_ms(record.updated_unix_ms)
                );
                println!(
                    "  root {} peer {}",
                    record.root,
                    record.peer_short.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        Some(JobsCommand::Status { job }) => {
            print!("{}", jobs_status_text(&dir, &job)?);
            Ok(())
        }
        Some(JobsCommand::Clean { all, completed }) => {
            if all && completed {
                bail!("choose one of --all or --completed, not both");
            }
            let scope = if all {
                CleanScope::All
            } else if completed {
                CleanScope::Completed
            } else {
                bail!(
                    "jobs clean needs --all or --completed (part files .akpart are never touched)"
                );
            };
            let (removed, kept) = clean_job_records(&dir, scope)?;
            println!(
                "removed {removed} checkpoint record(s), {kept} kept, in {}; \
                 .akpart part files are untouched",
                dir.display()
            );
            Ok(())
        }
    }
}

/// Default UDP port `receive` listens on when neither --bind nor --port
/// names one.
const DEFAULT_RECEIVE_PORT: u16 = 7100;

/// Firewall notice printed by every listener verb (`receive`, `daemon`).
pub(crate) const FIREWALL_NOTE: &str = "note: Windows may prompt to allow ak-cli through the \
                                       firewall on first listen; granting it only opens this \
                                       direct transfer port";

/// Fully-resolved receive-loop parameters shared by `receive` and `daemon`:
/// one identity, one accept policy, one destination. Every loop iteration
/// builds a fresh [`ReceiveSpec`] from this config, so identity/admission
/// wiring can never drift between the two verbs.
pub(crate) struct ReceiveLoopConfig {
    pub(crate) out: PathBuf,
    pub(crate) listen: SocketAddr,
    pub(crate) bundle: TlsBundle,
    pub(crate) fingerprint: Fingerprint,
    pub(crate) conflict: ConflictPolicy,
    pub(crate) checkpoints: PathBuf,
    pub(crate) device_name: String,
    pub(crate) idle: Duration,
    pub(crate) allow_internet: bool,
    pub(crate) policy: ExpectPolicy,
}

impl ReceiveLoopConfig {
    /// Resolve everything both receive loops share: device identity, accept
    /// policy, conflict policy, checkpoint directory and idle budget. The
    /// only per-verb inputs are the destination, listen address, device
    /// name and the LAN-only flag.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve(
        id: &Path,
        out: PathBuf,
        listen: SocketAddr,
        expect: Option<&str>,
        conflict_text: &str,
        no_internet: bool,
        timeout_secs: u64,
        device_name: String,
    ) -> Result<Self> {
        let (key, bundle) = load_identity(id)?;
        let policy = resolve_expect(expect, &peer_book_path(id))?;
        Ok(Self {
            out,
            listen,
            bundle,
            fingerprint: key.fingerprint(),
            conflict: conflict_policy(conflict_text)?,
            checkpoints: checkpoint_dir(id),
            device_name,
            idle: idle_timeout_from_secs(timeout_secs),
            allow_internet: !no_internet,
            policy,
        })
    }

    /// One loop iteration's engine spec. `authorize` is re-created per call
    /// so the peers.book policy re-reads the book on every connection
    /// (`pair add` takes effect without restarting the loop).
    pub(crate) fn iteration_spec(&self) -> ReceiveSpec {
        let mut spec = ReceiveSpec::new(self.out.clone(), self.bundle.clone());
        spec.bind = self.listen;
        spec.conflict = self.conflict;
        spec.checkpoint_dir = self.checkpoints.clone();
        spec.device_name = self.device_name.clone();
        spec.idle_timeout = self.idle;
        spec.policy.allow_internet = self.allow_internet;
        spec.authorize = self.policy.authorize();
        spec
    }
}

/// The accept-policy startup notice lines, shared by `receive` and
/// `daemon` so both verbs describe the same admission semantics.
pub(crate) fn expect_policy_lines(policy: &ExpectPolicy) -> Vec<String> {
    match policy {
        ExpectPolicy::Any => vec![
            "warning: --expect any accepts EVERY peer; anyone who can reach \
             this port may send files"
                .to_owned(),
        ],
        ExpectPolicy::Pinned(pinned) => vec![format!(
            "accept policy: only peer {} (its pairing code must be {})",
            pinned.short_id(),
            ak_identity::pairing_code(pinned)
        )],
        ExpectPolicy::PairingCode(code) => {
            vec![format!(
                "accept policy: only the peer presenting pairing code {code}"
            )]
        }
        ExpectPolicy::Book(path) => match PeerBook::load(path) {
            Ok(book) => vec![format!(
                "accept policy: paired peers only ({} in {})",
                book.iter().count(),
                path.display()
            )],
            Err(_) => vec![format!(
                "accept policy: paired peers only ({})",
                path.display()
            )],
        },
    }
}

// ---------------------------------------------------------------------------
// Address candidates (wave 25): what a sender can dial on this host
// ---------------------------------------------------------------------------

/// One address a sender can try on this host: a dialable interface address
/// (or the exact bound endpoint itself) carrying the listen port, plus its
/// classification label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AddressCandidate {
    pub(crate) addr: SocketAddr,
    /// "lan" (RFC1918 private / IPv6 unique-local), "public", or
    /// "loopback" (an explicit loopback bind); display-only.
    pub(crate) label: &'static str,
}

/// Display label for one IPv4 interface address.
fn ipv4_label(v4: std::net::Ipv4Addr) -> &'static str {
    if v4.is_loopback() {
        "loopback"
    } else if ak_net::classify::is_ipv4_private(v4) {
        "lan"
    } else {
        "public"
    }
}

/// True for IPv6 unique-local addresses (`fc00::/7`), the v6 counterpart of
/// RFC1918 private space.
fn is_ipv6_ula(v6: &std::net::Ipv6Addr) -> bool {
    v6.segments()[0] & 0xfe00 == 0xfc00
}

/// Display label for one IPv6 interface address.
fn ipv6_label(v6: &std::net::Ipv6Addr) -> &'static str {
    if v6.is_loopback() {
        "loopback"
    } else if is_ipv6_ula(v6) {
        "lan"
    } else {
        "public"
    }
}

/// The dialable candidates for `listen` — the receiver's actual bound
/// endpoint — given the local interface networks (review P2-02: the list
/// must reflect what is really listening, not just the port):
///
/// * **IPv4 wildcard** (`0.0.0.0:port`): every dialable IPv4 interface
///   address carrying the port (no loopback — `local_nets` drops loopback
///   interfaces, this is the defensive second lock — no link-local
///   `169.254.x`), deduped, interface order preserved;
/// * **IPv6 wildcard** (`[::]:port`): the IPv6 interface addresses (no
///   loopback, no link-local `fe80::` — those need a zone id to dial). A
///   plain `[::]` bind is v6-only on most hosts (`IPV6_V6ONLY` is not
///   portably queryable), so no unverified v4 compatibility is claimed;
/// * **any specific bind** (a concrete IP of either family, loopback
///   included): exactly that endpoint and nothing else — publishing other
///   interfaces' addresses would advertise endpoints nobody listens on.
///
/// Pure over `nets` so the filtering itself is testable without interfaces.
pub(crate) fn candidates_from_nets(
    nets: &[ak_net::classify::Net],
    listen: SocketAddr,
) -> Vec<AddressCandidate> {
    let port = listen.port();
    match listen.ip() {
        IpAddr::V4(wildcard) if wildcard.is_unspecified() => {
            let mut out: Vec<AddressCandidate> = Vec::new();
            for net in nets {
                let IpAddr::V4(v4) = net.addr else {
                    continue; // IPv6 is not dialable through a v4 wildcard
                };
                if v4.is_loopback() || v4.is_link_local() {
                    continue;
                }
                if out
                    .iter()
                    .any(|candidate| candidate.addr.ip() == IpAddr::V4(v4))
                {
                    continue;
                }
                out.push(AddressCandidate {
                    addr: SocketAddr::new(IpAddr::V4(v4), port),
                    label: ipv4_label(v4),
                });
            }
            out
        }
        IpAddr::V4(v4) => vec![AddressCandidate {
            addr: listen,
            label: ipv4_label(v4),
        }],
        IpAddr::V6(wildcard) if wildcard.is_unspecified() => {
            let mut out: Vec<AddressCandidate> = Vec::new();
            for net in nets {
                let IpAddr::V6(v6) = net.addr else {
                    continue; // IPv4 is not claimed for a v6 wildcard bind
                };
                if v6.is_loopback() || ak_net::classify::is_ipv6_link_local(v6) {
                    continue;
                }
                if out
                    .iter()
                    .any(|candidate| candidate.addr.ip() == IpAddr::V6(v6))
                {
                    continue;
                }
                out.push(AddressCandidate {
                    addr: SocketAddr::new(IpAddr::V6(v6), port),
                    label: ipv6_label(&v6),
                });
            }
            out
        }
        IpAddr::V6(v6) => vec![AddressCandidate {
            addr: listen,
            label: ipv6_label(&v6),
        }],
    }
}

/// Live version of [`candidates_from_nets`] over the real interfaces and the
/// receiver's actual bound endpoint: what a sender can truly dial given
/// that bind. Enumeration failure (or no compatible interface) yields an
/// empty list: the candidates block is a hint, the bound listen address is
/// always printed.
pub(crate) fn address_candidates_for(listen: SocketAddr) -> Vec<AddressCandidate> {
    ak_net::classify::local_nets()
        .map(|nets| candidates_from_nets(&nets, listen))
        .unwrap_or_default()
}

/// The daemon's fixed `0.0.0.0:port` bind form of [`address_candidates_for`]
/// (the daemon binds the IPv4 wildcard, so the port alone identifies the
/// bind; kept as its own entry so daemon.rs needs no endpoint plumbing).
pub(crate) fn address_candidates(port: u16) -> Vec<AddressCandidate> {
    address_candidates_for(SocketAddr::from(([0, 0, 0, 0], port)))
}

/// The human candidates block ("address candidates (try these):" plus one
/// aligned line per address). Empty input renders no lines at all.
pub(crate) fn address_candidates_lines(candidates: &[AddressCandidate]) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let width = candidates
        .iter()
        .map(|candidate| candidate.addr.to_string().len())
        .max()
        .unwrap_or(0);
    let mut lines = vec!["address candidates (try these):".to_owned()];
    for candidate in candidates {
        lines.push(format!(
            "  {:<width$} ({})",
            candidate.addr, candidate.label
        ));
    }
    lines
}

/// The `candidates` values of the daemon's NDJSON `listening` event.
pub(crate) fn address_candidates_json(candidates: &[AddressCandidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.addr.to_string())
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn receive_command(
    id: &Path,
    out: PathBuf,
    port: Option<u16>,
    bind: Option<BindArg>,
    expect: Option<String>,
    conflict: String,
    once: bool,
    no_internet: bool,
    timeout: u64,
    jobs: Option<u64>,
) -> Result<()> {
    let (listen, bind_note) =
        resolve_listen_addr(bind, "--bind", port, "--port", DEFAULT_RECEIVE_PORT);
    if let Some(note) = bind_note {
        println!("{note}");
    }
    if listen.is_ipv6() {
        println!(
            "note: listening on IPv6 {listen}; peers must reach this host over IPv6 \
             or an IPv4-mapped address (no relay fallback exists)"
        );
    }
    let config = ReceiveLoopConfig::resolve(
        id,
        out.clone(),
        listen,
        expect.as_deref(),
        &conflict,
        no_internet,
        timeout,
        whoami(),
    )?;
    let fp = config.fingerprint;
    for line in expect_policy_lines(&config.policy) {
        println!("{line}");
    }
    println!("{FIREWALL_NOTE}");

    let mut first = true;
    // Concurrent mode: accept `jobs` senders on one endpoint at once.
    if let Some(expected) = jobs.map(|count| count as usize) {
        let spec = config.iteration_spec();
        println!(
            "receiving {expected} concurrent transfer(s) into {}",
            out.display()
        );
        println!("fingerprint (full): {}", fp.to_hex());
        let printer = progress_printer_multi();
        let results = ak_engine::serve_concurrently(spec, expected, &*printer).await;
        let mut completed = 0usize;
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Ok(report) => {
                    completed += 1;
                    println!("job #{index}:");
                    report_summary(&report);
                }
                Err(error) => println!("job #{index}: failed: {error}"),
            }
        }
        if completed == expected {
            println!("concurrent receive finished: {completed}/{expected} completed");
            return Ok(());
        }
        bail!("concurrent receive finished: {completed}/{expected} completed");
    }
    // The previous iteration's QUIC endpoint releases its UDP socket
    // asynchronously after drop, so an immediate rebind can hit AddrInUse.
    // Retry briefly instead of failing the whole loop.
    const MAX_REBIND_RETRIES: u32 = 50;
    let mut rebind_retries = 0u32;
    loop {
        let spec = config.iteration_spec();
        let printer = progress_printer("recv");
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await },
            );
        if first {
            let addr = addr_rx.await.context("receiver failed to bind")?;
            println!("receiving into {}", out.display());
            println!("address: {addr}");
            // Wave 25: 0.0.0.0:PORT is useless to a sender on a multi-NIC
            // host (VPN + real NIC) -- also list what the peer can dial
            // given the actual bind (every compatible interface address for
            // a wildcard bind, the bound endpoint itself for a specific
            // one), labeled lan/public.
            for line in address_candidates_lines(&address_candidates_for(addr)) {
                println!("{line}");
            }
            println!("{}", fingerprint_lines("fingerprint", &fp));
            println!("pairing code: {}", ak_identity::pairing_code(&fp));
            println!("waiting for sender (Ctrl-C stops)...");
            first = false;
        } else {
            // Bind errors surface through the task result below.
            let _ = addr_rx.await;
        }
        let outcome = tokio::select! {
            joined = receiver => joined,
            _ = tokio::signal::ctrl_c() => {
                println!("ctrl-c: stopped receiving (partial files stay resumable)");
                return Ok(());
            }
        };
        match outcome {
            Ok(Ok(report)) => {
                report_summary(&report);
                if once {
                    return Ok(());
                }
                rebind_retries = 0;
                println!("waiting for next sender (Ctrl-C stops)...");
            }
            Ok(Err(EngineError::Connect(ak_net::ConnectError::Unauthorized))) if !once => {
                println!("rejected a peer outside the accept policy; still waiting (Ctrl-C stops)");
            }
            Ok(Err(EngineError::Io(err)))
                if !once
                    && err.kind() == std::io::ErrorKind::AddrInUse
                    && rebind_retries < MAX_REBIND_RETRIES =>
            {
                rebind_retries += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(join) => bail!("receiver task failed: {join}"),
        }
    }
}

/// Canonicalize the send paths: the first is the primary root, the rest
/// become extra roots in the order given (the engine enforces unique root
/// names across the whole job).
fn send_roots(paths: &[PathBuf]) -> Result<(PathBuf, Vec<PathBuf>)> {
    let Some(first) = paths.first() else {
        bail!("send needs at least one file or directory");
    };
    let primary = first
        .canonicalize()
        .with_context(|| first.display().to_string())?;
    let extras = paths[1..]
        .iter()
        .map(|path| {
            path.canonicalize()
                .with_context(|| path.display().to_string())
        })
        .collect::<Result<Vec<PathBuf>>>()?;
    Ok((primary, extras))
}

/// The engine's duplicate-root refusal as an io error, so the CLI can reword
/// it into a friendly hint. Any other InvalidInput (e.g. an unusable root
/// name) passes through untouched.
fn duplicate_roots_error(err: &EngineError) -> Option<&std::io::Error> {
    match err {
        EngineError::Io(io)
            if io.kind() == std::io::ErrorKind::InvalidInput
                && io.to_string().starts_with("duplicate transfer root names") =>
        {
            Some(io)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// send: multi-address candidates with automatic fallback (wave 25)
// ---------------------------------------------------------------------------

/// Default per-address connect deadline of the send fallback (seconds):
/// an address whose bind/connect/handshake phase has not completed within
/// this budget is abandoned for the next candidate. The budget starts when
/// the engine reports `Progress::Connecting` — i.e. after the synchronous
/// source scan — and no longer applies once the transfer is connected (see
/// [`attempt_send_to`] and [`drive_with_connect_budget`]).
///
/// 10s (was 3s): the QUIC+TLS handshake needs several round trips, and a
/// real cross-continent run through a VPN exit measured over 3s on a slow
/// day — BOTH wave-25 (3s around the whole future) and the split-budget
/// wave-26 driver failed W→L at exactly 3s while the reverse direction
/// (China→local, fewer RTTs through the direct path) kept working. LAN
/// candidates still connect in milliseconds, so a larger budget costs
/// nothing on fast paths; `--connect-timeout` tunes it per environment.
const SEND_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Brief pause between address attempts: the failed attempt's QUIC endpoint
/// releases its UDP socket asynchronously after drop, so an immediate rebind
/// on an explicit --port could hit AddrInUse (same reasoning as the receive
/// loop's rebind retries).
const SEND_ATTEMPT_PAUSE: Duration = Duration::from_millis(150);

/// Sender-side LAN preference for one target: RFC1918 private IPv4 (and any
/// loopback) sorts before public IPv4 and IPv6. Reuses
/// `ak_net::classify::is_ipv4_private`; the classification is a routing
/// hint, never an authorization.
fn send_target_prefers_lan(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => ak_net::classify::is_ipv4_private(v4),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Dedupe the --to list, normalize v4-mapped candidates to plain IPv4
/// (`[::ffff:1.2.3.4]:port` dials as `1.2.3.4:port` — `ak_net`'s connect
/// bridges families for dual-stack sockets, but the engine's send-path
/// family fast-fail compares raw families, so the dialable plain form is
/// the canonical one), then stably sort LAN-first. Stable: the user's
/// order survives within the LAN group and within the public group.
pub(crate) fn order_send_targets(targets: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut ordered: Vec<SocketAddr> = Vec::with_capacity(targets.len());
    for addr in targets {
        let addr = ak_net::classify::expand_mapped(addr);
        if !ordered.contains(&addr) {
            ordered.push(addr);
        }
    }
    ordered.sort_by_key(|addr| !send_target_prefers_lan(addr));
    ordered
}

/// Local wildcard bind matching the candidate's address family (review
/// P2-01): the engine rejects a local/peer family mismatch before dialing,
/// so an IPv6 candidate must bind `[::]:port` (same-family wildcard), not
/// `0.0.0.0:port`. The explicit `--port` semantics survive unchanged —
/// same port, family-matched wildcard — and the per-attempt pause
/// ([`SEND_ATTEMPT_PAUSE`]) still gives the previous attempt's socket time
/// to release the port.
pub(crate) fn send_bind_for(addr: SocketAddr, port: u16) -> SocketAddr {
    if addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), port)
    } else {
        SocketAddr::from(([0, 0, 0, 0], port))
    }
}

/// Why one address attempt failed (wave 25 fallback bookkeeping).
#[derive(Debug)]
struct AttemptFailure {
    addr: SocketAddr,
    /// One-line reason for the aggregate error and the progress line.
    reason: String,
    /// The original engine error (kept for the friendly-hint mapping).
    err: EngineError,
    /// True when the engine had already reported `Connected`: a
    /// transfer-phase failure another address cannot fix.
    after_connect: bool,
    /// Send-path hint that overrides [`errors::friendly_send_hint`] for
    /// this failure (wave 27: the scheduler-admission BUSY copy). `None`
    /// everywhere except an admission denial. Boxed to keep the struct
    /// under the `result_large_err` budget (the hint exists on exactly
    /// one failure shape; the indirection is free where it is absent).
    hint: Option<Box<str>>,
}

impl AttemptFailure {
    /// The connect-deadline failure of [`attempt_send_to`].
    fn connect_timeout(addr: SocketAddr) -> Self {
        AttemptFailure {
            addr,
            reason: format!(
                "timeout after {}s (no QUIC handshake)",
                SEND_CONNECT_TIMEOUT.as_secs()
            ),
            err: EngineError::IdleTimeout(SEND_CONNECT_TIMEOUT),
            after_connect: false,
            hint: None,
        }
    }

    /// Short tag for the `[send] failed (...)` progress line.
    fn short(&self) -> String {
        if self.reason.starts_with("timeout") {
            "timeout".to_owned()
        } else {
            clip_line(self.reason.split(['\n', '\r']).next().unwrap_or(""), 60).to_owned()
        }
    }
}

/// Single-line rendering of an engine error for progress/summary lines
/// (multi-line Display output would garble the fallback report).
pub(crate) fn one_line_reason(err: &EngineError) -> String {
    let text = err.to_string();
    let first = text.split(['\n', '\r']).next().unwrap_or("");
    clip_line(first, 120).to_owned()
}

/// Connect-phase bookkeeping for one send attempt (review P1-01). The
/// engine invokes its progress callback synchronously inside the polled
/// future, so every mark below is set while the driver task is inside
/// `send.poll()` itself — there is no cross-task race, and the recorded
/// instant is exactly when the engine moved from preparation to binding.
struct ConnectPhaseMarks {
    /// When `Progress::Connecting` fired: `prepare_send` (the synchronous
    /// full-source scan: open every source, whole + chunk digests, sender
    /// checkpoint) is done and bind/connect/handshake start now.
    connecting_at: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Wakes [`drive_with_connect_budget`] when the connect phase starts.
    /// `Notify` stores a permit when there is no waiter yet, so the signal
    /// survives whichever order poll and callback run in.
    connecting: tokio::sync::Notify,
    /// Set on `Progress::Connected`: the handshake completed and the
    /// transfer is running under its own idle budget.
    connected: std::sync::atomic::AtomicBool,
}

impl ConnectPhaseMarks {
    fn new() -> Self {
        Self {
            connecting_at: std::sync::Mutex::new(None),
            connecting: tokio::sync::Notify::new(),
            connected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Progress-callback side: record the phase transitions the budget
    /// driver needs. `Connecting` marks the scan/connect boundary *before*
    /// waking the driver, so the recorded instant is always available.
    fn mark(&self, progress: &Progress) {
        match progress {
            Progress::Connecting => {
                *self.connecting_at.lock().unwrap() = Some(tokio::time::Instant::now());
                self.connecting.notify_one();
            }
            Progress::Connected { .. } => {
                self.connected
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Drive one send attempt under split budgets (review P1-01: the 3-second
/// wave-25 deadline used to wrap the whole engine future, so a large or
/// slow-storage scan exhausted it before the first handshake byte):
///
/// * **before the connect-phase signal** there is no deadline at all — the
///   engine's synchronous source scan (`prepare_send`) must not burn the
///   connect budget, whatever the directory size or storage speed;
/// * **from the connect-phase signal** (bind/connect/QUIC+TLS handshake)
///   the attempt gets `budget`, starting exactly at the engine's
///   `Connecting` report;
/// * **once connected** the deadline no longer applies: the transfer is
///   governed by the engine's own idle budget and legitimately outlives
///   the connect budget.
///
/// `Err(())` means the connect phase exceeded its budget without a
/// handshake; the caller renders the "no QUIC handshake" failure. The
/// budget is a parameter (production passes [`SEND_CONNECT_TIMEOUT`]) so
/// the split itself is testable with synthetic futures and small delays
/// instead of real multi-second scans. Generic over the driven future for
/// the same reason.
async fn drive_with_connect_budget<S>(
    marks: &ConnectPhaseMarks,
    mut send: std::pin::Pin<&mut S>,
    budget: Duration,
) -> Result<<S as Future>::Output, ()>
where
    S: Future + ?Sized,
{
    // Phase 1 — preparation: no deadline. The engine signals the boundary
    // from inside its own poll (see ConnectPhaseMarks); an attempt that
    // finishes or fails before ever connecting (invalid spec, duplicate
    // roots, bind failure) completes through the second branch untouched.
    let connect_started = tokio::select! {
        _ = marks.connecting.notified() => {
            marks
                .connecting_at
                .lock()
                .unwrap()
                .expect("Connecting instant is recorded before notify")
        }
        done = send.as_mut() => return Ok(done),
    };
    // Phase 2 — bind/connect/handshake: the budget counted from the
    // engine's own Connecting report, so scan time cannot leak in.
    match tokio::time::timeout_at(connect_started + budget, send.as_mut()).await {
        Ok(done) => Ok(done),
        Err(_deadline_elapsed) => {
            if marks.connected.load(std::sync::atomic::Ordering::Relaxed) {
                // Connected inside the deadline; from here the transfer is
                // governed by its own idle budget, not the connect deadline.
                Ok(send.await)
            } else {
                Err(())
            }
        }
    }
}

/// One full engine send against a single address, split-timed by
/// [`drive_with_connect_budget`]: the synchronous source scan is unbudgeted,
/// bind/connect/handshake gets [`SEND_CONNECT_TIMEOUT`], and a connected
/// transfer runs to completion unbounded (a real transfer legitimately
/// outlives 3 seconds). Phase marks are set from the progress callback,
/// which the engine invokes synchronously inside the polled future, so
/// there is no cross-task race.
///
/// `AttemptFailure` carries the original `EngineError` (and, since wave
/// 27, the admission BUSY hint) so the aggregate error keeps the full
/// chain; at 128 bytes it sits exactly on clippy's `result_large_err`
/// budget. Boxing the error would hide the concrete variant behind a
/// deref at every match site, so the shape is kept and the lint allowed
/// here (single site; the struct never nests).
#[allow(clippy::result_large_err)]
async fn attempt_send_to(
    spec: SendSpec,
    printer: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Report, AttemptFailure> {
    let addr = spec.peer;
    let marks = Arc::new(ConnectPhaseMarks::new());
    let phase_marks = marks.clone();
    let wrapped = move |progress: Progress| {
        phase_marks.mark(&progress);
        printer(progress);
    };
    let send = ak_engine::send(spec, &wrapped);
    tokio::pin!(send);
    let result = match drive_with_connect_budget(&marks, send.as_mut(), SEND_CONNECT_TIMEOUT).await
    {
        Ok(result) => result,
        Err(()) => return Err(AttemptFailure::connect_timeout(addr)),
    };
    let after_connect = marks.connected.load(std::sync::atomic::Ordering::Relaxed);
    result.map_err(|err| AttemptFailure {
        addr,
        reason: one_line_reason(&err),
        err,
        after_connect,
        hint: None,
    })
}

/// Drive the wave-25 address fallback: try each target in order through
/// `attempt`. A connect-phase failure moves to the next address; a failure
/// after the connection was established is returned immediately (another
/// address cannot fix a protocol/transfer failure); the first success
/// returns its report. Total failure returns every (address, reason) pair
/// for the aggregate error. Progress lines print only for multi-address
/// runs, so single-address output stays identical to the classic CLI.
async fn run_send_fallback<F, Fut>(
    targets: &[SocketAddr],
    mut attempt: F,
) -> Result<Report, Vec<AttemptFailure>>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = Result<Report, AttemptFailure>>,
{
    let total = targets.len();
    let mut failures: Vec<AttemptFailure> = Vec::new();
    for (index, addr) in targets.iter().copied().enumerate() {
        if total > 1 && index == 0 {
            println!("[send] trying 1/{total}: {addr}...");
        }
        match attempt(addr).await {
            Ok(report) => return Ok(report),
            Err(failure) => {
                let next = index + 1;
                if total > 1 && next < total {
                    // The spec's announcement line doubles as the "trying"
                    // line for the next candidate.
                    println!(
                        "[send] failed ({}), trying {}/{total}: {}...",
                        failure.short(),
                        next + 1,
                        targets[next]
                    );
                    tokio::time::sleep(SEND_ATTEMPT_PAUSE).await;
                } else if total > 1 {
                    println!("[send] failed ({})", failure.short());
                }
                let after_connect = failure.after_connect;
                failures.push(failure);
                if after_connect {
                    // This address connected; the transfer itself failed.
                    // No other candidate can help -- surface it now.
                    return Err(failures);
                }
            }
        }
    }
    Err(failures)
}

/// Build the final error from the fallback failures. A single-address run
/// keeps the classic wording exactly (engine error, friendly hint as
/// context); a multi-address run lists every attempted address with its own
/// reason (or, for a post-connect failure, names the connected address and
/// the untried candidates).
fn send_failures_error(failures: Vec<AttemptFailure>, targets: &[SocketAddr]) -> anyhow::Error {
    // Address-independent input problems keep their dedicated friendly copy
    // regardless of how many addresses were tried.
    for failure in &failures {
        if let Some(io) = duplicate_roots_error(&failure.err) {
            return anyhow::Error::msg(format!(
                "two paths have the same name: please send them in separate runs ({io})"
            ));
        }
    }
    let mut failures = failures;
    let last = failures
        .pop()
        .expect("run_send_fallback returns at least one failure");
    // Wave 27: an admission-denial failure carries its own BUSY/retry copy
    // (scheduler.md §7); every other failure keeps the generic mapping.
    let hint = last
        .hint
        .as_deref()
        .map(str::to_owned)
        .or_else(|| errors::friendly_send_hint(&last.err));
    let base = anyhow::Error::from(last.err);
    if targets.len() == 1 {
        // Single address: byte-for-byte the classic error path.
        return match hint {
            Some(hint) => base.context(hint),
            None => base,
        };
    }
    let mut message = if last.after_connect {
        let untried: Vec<String> = targets
            .iter()
            .filter(|addr| !failures.iter().any(|f| f.addr == **addr) && **addr != last.addr)
            .map(|addr| addr.to_string())
            .collect();
        let untried = if untried.is_empty() {
            String::new()
        } else {
            format!(" (untried: {})", untried.join(", "))
        };
        format!(
            "connected to {} but the transfer failed: {}{untried}",
            last.addr, last.reason
        )
    } else {
        let mut message = format!("all {} peer address attempts failed:", targets.len());
        for failure in &failures {
            message.push_str(&format!("\n  {}: {}", failure.addr, failure.reason));
        }
        message.push_str(&format!("\n  {}: {}", last.addr, last.reason));
        message
    };
    if let Some(hint) = &hint {
        message.push_str(&format!("\n{hint}"));
    }
    base.context(message)
}

// ---------------------------------------------------------------------------
// send: scheduler admission over a Ready pairing relation (wave 27, P1-02)
// ---------------------------------------------------------------------------

/// One Ready pairing relation of this identity directory that matches the
/// pinned send target — the facts the scheduler admission context needs
/// (scheduler.md §6): which code group the task runs under and which
/// pairing relation selected the peer.
///
/// Source of truth (read-only, CLI side):
///
/// * the relation itself — the `ak-store` pairing journal
///   (`<id-dir>/pairing/<pairing hex>.akpair`, written by the wave-21/22
///   pairing flows): `phase applied` marks the relation whose `PairReady`
///   completed and whose entry landed in `peers.book` (the journal's
///   `applied` IS the local Ready mark — see
///   [`ak_session::pairing::PairingOrchestrator::finish_committed`]);
///   the record's `peer_public_key` is the peer device's Ed25519 key,
///   whose `Fingerprint::from_public_key` equals the TLS fingerprint the
///   send pins;
/// * the registry half — `<id-dir>/registry.pin` (`AKREG1 <addr> <64hex>`):
///   the pinned registry id is exactly `TaskOpen.group_registry` (the
///   registry certificate key IS the registry id, see
///   `ak_session::pairing::client`).
///
/// A relation without a usable registry pin cannot form a valid
/// `AdmissionCtx` (`group_registry` must name the real registry), so the
/// lookup returns `None` and the send keeps the legacy protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadyRelation {
    /// `TaskOpen.pairing_id`: the pairing transaction that produced the
    /// relation (one code may be redeemed by several devices — the task
    /// must name which relation it runs on, scheduler.md §2).
    pairing_id: [u8; 16],
    /// The invite code identity of the relation (`code_id[16]`); travels
    /// as `TaskOpen.group` through the frozen 32-byte wire form
    /// [`ak_session::runtime::code_group_wire_form`] (BLAKE3 of the 16
    /// code bytes — the wave-27 convention shared with the receiving
    /// daemon's [`ak_session::runtime::AdmissionValidator`]).
    code_id: [u8; 16],
    /// `TaskOpen.group_registry`: the pinned registry identity.
    registry_id: [u8; 32],
}

/// The registry identity pinned under `id` (`registry.pin`), when one
/// exists. Absent file → `None` (this device never talked to a registry,
/// so no admission context is possible). A present-but-corrupt pin is an
/// error, never silently ignored (the fail-closed discipline of
/// [`invite::RegistrySettings::parse`]).
fn pinned_registry_id(id: &Path) -> Result<Option<[u8; 32]>> {
    let pin_path = id.join(invite::REGISTRY_PIN_FILE);
    match std::fs::read_to_string(&pin_path) {
        Ok(text) => Ok(Some(
            invite::RegistrySettings::parse(&text)
                .with_context(|| format!("corrupt {}", pin_path.display()))?
                .registry_id,
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("cannot read {}", pin_path.display())))
        }
    }
}

/// Owner-side revocation knowledge (best effort, pure local files): a code
/// this device itself created is revoked when `invite revoke` removed its
/// line from `<id-dir>/invites.store` — a store that exists but no longer
/// lists the code means the operator killed it. A redeemer has no local
/// revocation state (the store is owner-side only), so a missing store
/// never disqualifies a relation; the receiving owner's admission decider
/// remains the authority on whether the code still stands.
fn code_revoked_locally(id: &Path, code_id: &[u8; 16]) -> bool {
    let Ok(text) = std::fs::read_to_string(id.join(invite::INVITE_STORE_FILE)) else {
        return false;
    };
    let prefix = format!("AKINV1 {}", hex_of(code_id));
    !text
        .lines()
        .any(|line| line.trim_start().starts_with(&prefix))
}

/// Find the Ready pairing relation of this identity directory that matches
/// `pinned` (wave 27, scheduler.md §6). Read-only: nothing is created,
/// nothing is written. Facts and selection rule:
///
/// * candidates are journal records with `phase applied` whose
///   `peer_public_key` derives exactly the pinned fingerprint;
/// * among several relations for the same peer (a re-pair, or several
///   codes redeemed with the same device) the most recently updated
///   applied record wins — its code group is the freshest pairing state;
///   ties break on `pairing_id` bytes so the choice is deterministic;
/// * owner-revoked codes (see [`code_revoked_locally`]) are skipped;
/// * a corrupt journal fails closed (the ak-store contract: a journal that
///   lies is worse than one that refuses) — the operator fixes the state
///   instead of the send silently dropping to the unadmitted path;
/// * no registry pin → `Ok(None)` (legacy protocol; see
///   [`ReadyRelation`]).
fn ready_relation_for(id: &Path, pinned: &Fingerprint) -> Result<Option<ReadyRelation>> {
    let journal_dir = id.join(ak_session::pairing::JOURNAL_SUBDIR);
    if !journal_dir.exists() {
        // Fresh identity directory: no pairing has ever run here. Do not
        // even open the journal (PairingJournal::open would create it).
        return Ok(None);
    }
    let Some(registry_id) = pinned_registry_id(id)? else {
        return Ok(None);
    };
    let journal = ak_store::PairingJournal::open(&journal_dir).with_context(|| {
        format!(
            "cannot open the pairing journal at {}",
            journal_dir.display()
        )
    })?;
    let records = journal
        .load_all()
        .context("the pairing journal is corrupt; fix it before sending (fail closed)")?;
    let mut candidates: Vec<&ak_store::PairingRecord> = records
        .iter()
        .filter(|record| record.phase == ak_store::PairingPhase::Applied)
        .filter(|record| {
            record
                .peer_public_key
                .is_some_and(|key| Fingerprint::from_public_key(&key) == *pinned)
        })
        .filter(|record| !code_revoked_locally(id, &record.code_id))
        .collect();
    // Most recent Ready first; pairing-id bytes break exact ties so two
    // records updated within the same millisecond still choose stably.
    candidates.sort_by(|a, b| {
        b.updated_unix_ms
            .cmp(&a.updated_unix_ms)
            .then_with(|| a.pairing_id.cmp(&b.pairing_id))
    });
    Ok(candidates.first().map(|record| ReadyRelation {
        pairing_id: record.pairing_id,
        code_id: record.code_id,
        registry_id,
    }))
}

/// Bounded BUSY copy for an admission denial (scheduler.md §7: "远端准入
/// 队列满：返回有界 BUSY/retry hint"). The one-shot CLI keeps no local
/// queue item, so the honest advice is: nothing was sent, retry the same
/// command after the stated condition clears.
fn admission_denied_hint(reason: ak_wire::AdmissionReason) -> String {
    use ak_wire::AdmissionReason;
    match reason {
        AdmissionReason::SerialWait => {
            "对端忙（BUSY）：同一邀请码下已有任务在传输（serial_wait）。\
            本次未发送任何数据；请等对端当前任务结束后重跑同一命令重试"
                .to_owned()
        }
        AdmissionReason::Capacity => "对端忙（BUSY）：对端的准入队列或并发上限已满（capacity）。\
            本次未发送任何数据；请稍后重跑同一命令重试"
            .to_owned(),
        AdmissionReason::StalePermit => {
            "对端忙（BUSY）：准入许可反复过期（stale_permit，已按有界预算重试）。\
            本次未发送任何数据；请稍后重跑同一命令重试"
                .to_owned()
        }
        AdmissionReason::Ok => {
            "对端忙（BUSY）：准入被拒。本次未发送任何数据；请稍后重跑同一命令重试".to_owned()
        }
    }
}

/// Outcome of one admission-capable send attempt against one address.
#[derive(Debug)]
enum AdmissionAttempt {
    /// The admission handshake ran and the whole transfer completed.
    Completed(Report),
    /// The connected peer's Hello did not carry `CAP_SCHEDULER` (a legacy
    /// receiver): the engine refused the scheduled task with
    /// `PROTOCOL_UNSUPPORTED` before any TaskOpen byte moved (the error
    /// itself is carried for the caller's diagnostics line). The caller
    /// may retry the same address on the legacy protocol — the frozen v1
    /// wire both ends still share (scheduler.md §6: old ends keep the old
    /// protocol; the refusal itself is what proves no silent downgrade of
    /// a scheduler-capable peer happened).
    LegacyPeer(EngineError),
    /// Any other failure: connect-phase errors (next address may help)
    /// and post-connect failures (another address cannot fix them).
    Failed(AttemptFailure),
}

/// One scheduler-admission send attempt against a single address (wave
/// 27): bind + connect under the same [`SEND_CONNECT_TIMEOUT`] budget the
/// legacy per-address attempt uses (the budget covers exactly the
/// bind/QUIC+TLS handshake phase — admission runs after the connection is
/// established, so the budget semantics of wave 26 are unchanged), then
/// hand the connected channel to [`ak_engine::send_on_channel`] with the
/// [`ak_engine::AdmissionCtx`] built from the Ready relation.
///
/// Error discipline (scheduler.md §6):
///
/// * an admission denial (capacity/serial_wait, or a stale-permit budget
///   exhausted) is a terminal BUSY failure — `after_connect = true`, so
///   the fallback driver stops: the same peer would deny on every
///   address, and there is no fallback to the unadmitted path;
/// * `PROTOCOL_UNSUPPORTED` from the on-channel entry means exactly "the
///   peer Hello lacked CAP_SCHEDULER" (the engine checks the capability
///   locally, before any TaskOpen) — surfaced as [`AdmissionAttempt::LegacyPeer`];
/// * everything else from `send_on_channel` happened after the connection
///   was established, so it is `after_connect = true` like the legacy
///   attempt's post-connect failures.
async fn attempt_send_admission_to(
    spec: SendSpec,
    relation: &ReadyRelation,
    printer: &(dyn Fn(Progress) + Send + Sync),
) -> AdmissionAttempt {
    let addr = spec.peer;
    // AdmissionCtx identities: task and permit ids are minted fresh per
    // invocation (scheduler.md §11: a user submission is a new task
    // instance with a random persistent id; the file-protocol wire job id
    // is content-derived inside the engine and not derivable before the
    // handshake without duplicating the engine's private source scan).
    // The all-zero sentinel cannot occur from `random_id16`; the
    // validating constructor is still the only way the ctx is built.
    let ctx = match ak_engine::AdmissionCtx::new(
        ak_identity::pairing_txn::random_id16(),
        ak_identity::pairing_txn::random_id16(),
        ak_session::runtime::code_group_wire_form(&relation.code_id),
        relation.registry_id,
        relation.pairing_id,
    ) {
        Ok(ctx) => ctx,
        Err(err) => {
            return AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: one_line_reason(&err),
                err,
                after_connect: false,
                hint: None,
            })
        }
    };
    // Bind + connect (QUIC + pinned TLS handshake) under the wave-26
    // connect budget. The source scan runs inside send_on_channel after
    // the channel exists, so unlike the legacy path it cannot eat into
    // this budget at all.
    let connect = async {
        let endpoint = ak_net::DirectEndpoint::bind(spec.bind, &spec.identity).await?;
        endpoint.connect(spec.peer, &spec.pinned).await
    };
    let channel = match tokio::time::timeout(SEND_CONNECT_TIMEOUT, connect).await {
        Ok(Ok(channel)) => channel,
        Ok(Err(err)) => {
            let err = EngineError::Connect(err);
            return AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: one_line_reason(&err),
                err,
                after_connect: false,
                hint: None,
            });
        }
        Err(_deadline_elapsed) => {
            return AdmissionAttempt::Failed(AttemptFailure::connect_timeout(addr))
        }
    };
    match ak_engine::send_on_channel(channel, spec, ctx, printer).await {
        Ok(report) => AdmissionAttempt::Completed(report),
        Err(EngineError::AdmissionDenied { reason }) => {
            let hint = admission_denied_hint(reason);
            AdmissionAttempt::Failed(AttemptFailure {
                addr,
                reason: format!("remote admission denied ({:?}); nothing was sent", reason),
                err: EngineError::AdmissionDenied { reason },
                after_connect: true,
                hint: Some(Box::from(hint)),
            })
        }
        Err(
            err @ EngineError::PeerError {
                code: ak_core::error::ErrorCode::ProtocolUnsupported,
                ..
            },
        ) => {
            // Exactly "peer does not advertise CAP_SCHEDULER": a legacy
            // receiver. No scheduler frames were sent; the frozen v1
            // protocol is the shared language for this pair.
            AdmissionAttempt::LegacyPeer(err)
        }
        Err(err) => AdmissionAttempt::Failed(AttemptFailure {
            addr,
            reason: one_line_reason(&err),
            err,
            after_connect: true,
            hint: None,
        }),
    }
}

// Options arrive flat from clap; grouping them into a struct here would only
// mirror the enum variant one-to-one (same allowance as the engine drivers).
#[allow(clippy::too_many_arguments)]
async fn send_command(
    id: &Path,
    paths: Vec<PathBuf>,
    to: Vec<SocketAddr>,
    fp_text: String,
    port: u16,
    conflict: String,
    multistream: bool,
    no_internet: bool,
    timeout: u64,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let pinned = resolve_fingerprint_arg(&fp_text, &peer_book_path(id))?;
    let (root, extras) = send_roots(&paths)?;
    let targets = order_send_targets(to);
    println!(
        "sending {} path(s) to peer {} (its pairing code must be {})",
        1 + extras.len(),
        pinned.short_id(),
        ak_identity::pairing_code(&pinned)
    );
    if targets.len() > 1 {
        println!(
            "peer addresses (LAN first): {}",
            targets
                .iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let conflict = conflict_policy(&conflict)?;
    let checkpoints = checkpoint_dir(id);
    let idle = idle_timeout_from_secs(timeout);
    let device_name = whoami();
    let printer = progress_printer("send");
    let _ = key;
    // Wave 27 (scheduler.md §6): when this identity directory holds a
    // Ready pairing relation matching the pinned peer, each address
    // attempt first asks the peer for scheduler admission
    // (TaskOpen → Admission → AdmissionAck) and only then transfers. The
    // lookup is read-only; without a relation (an explicit manual
    // fingerprint transfer) the send keeps the legacy protocol unchanged.
    let relation = ready_relation_for(id, &pinned)?;
    if let Some(relation) = &relation {
        println!(
            "[send] ready pairing relation {} (code {}): requesting scheduler admission",
            &hex_of(&relation.pairing_id)[..8],
            &hex_of(&relation.code_id)[..8]
        );
    }
    let admission = relation.as_ref();
    // One engine send per address; the spec is rebuilt per attempt because
    // SendSpec carries the peer address (and the engine consumes the spec).
    // The shared pieces are borrowed (not moved) so the closure stays FnMut
    // across attempts, and every attempt clones its own spec inputs — no
    // spec state survives one attempt to pollute the next. The engine's
    // prepared scan, however, is private to one `ak_engine::send` call and
    // cannot be shared across candidates from the CLI (see the P1-01 note
    // in reports/codex-review-2026-11-05-wave25.md: fixing that repetition
    // needs an ak-engine entry change, out of scope here).
    let (root, extras, bundle, checkpoints, device_name) =
        (&root, &extras, &bundle, &checkpoints, &device_name);
    let printer = printer.as_ref();
    let attempt = |addr: SocketAddr| async move {
        let mut spec = SendSpec::new(root.clone(), addr, pinned, bundle.clone());
        for extra in extras.iter() {
            spec.add_root(extra.clone());
        }
        // Family-matched wildcard per candidate (review P2-01): an IPv6
        // candidate binds [::]:port, an IPv4 candidate 0.0.0.0:port — the
        // engine's family fast-fail would otherwise kill every IPv6
        // candidate before the handshake.
        spec.bind = send_bind_for(addr, port);
        spec.conflict = conflict;
        spec.checkpoint_dir = checkpoints.clone();
        spec.device_name = device_name.clone();
        spec.idle_timeout = idle;
        spec.multistream = multistream;
        spec.policy.allow_internet = !no_internet;
        if let Some(relation) = admission {
            match attempt_send_admission_to(spec.clone(), relation, printer).await {
                AdmissionAttempt::Completed(report) => return Ok(report),
                AdmissionAttempt::LegacyPeer(err) => {
                    // Legacy receiver (no CAP_SCHEDULER in its Hello): the
                    // engine refused the scheduled task before any TaskOpen
                    // byte moved. Explicit, observable retry on the frozen
                    // v1 protocol both ends share — never a silent path
                    // choice.
                    println!("[send] {addr}: {}", one_line_reason(&err));
                    println!("[send] retrying {addr} on the legacy transfer protocol");
                    tokio::time::sleep(SEND_ATTEMPT_PAUSE).await;
                }
                // An admission denial (BUSY) or any other post-connect
                // failure: terminal for this run — the same peer would
                // answer the same way on every address, and there is no
                // fallback to the unadmitted path.
                AdmissionAttempt::Failed(failure) => return Err(failure),
            }
        }
        attempt_send_to(spec, printer).await
    };
    let report = match run_send_fallback(&targets, attempt).await {
        Ok(report) => report,
        Err(failures) => return Err(send_failures_error(failures, &targets)),
    };
    report_summary(&report);
    // TOFU: remember a successfully reached receiver.
    if record_peer_after_send(id, &pinned)? {
        println!(
            "recorded peer {} in {} (trust on first use; its pairing code is {})",
            pinned.short_id(),
            peer_book_path(id).display(),
            ak_identity::pairing_code(&pinned)
        );
    } else {
        println!("peer {} already paired", pinned.short_id());
    }
    Ok(())
}

async fn discover_command(id: &Path, seconds: u64, json: bool) -> Result<()> {
    let (key, _) = load_identity(id)?;
    let fp = key.fingerprint();
    let book = load_peer_book(&peer_book_path(id))?;
    let mut mdns = ak_discovery::MdnsDiscovery::new().context("mDNS daemon unavailable")?;
    mdns.advertise(7100, &fp, &whoami())
        .context("mDNS advertise failed (multicast may be blocked)")?;
    let nets: Vec<_> = ak_net::classify::local_nets()?
        .into_iter()
        .map(|net| ak_discovery::ak_net_shim::Net {
            addr: net.addr,
            prefix_len: net.prefix_len,
        })
        .collect();
    let found = mdns.browse(Duration::from_secs(seconds))?;
    let ranked = ak_discovery::rank(found, &nets);
    if json {
        println!("{}", discover_json(&ranked, &book));
        return Ok(());
    }
    println!("address\tfingerprint\tpairing\tsource");
    for candidate in ranked {
        let short = candidate
            .fingerprint
            .map(|f| f.short_id())
            .unwrap_or_else(|| "-".to_owned());
        let pairing = match &candidate.fingerprint {
            Some(peer_fp) => match book.get(peer_fp) {
                Some(record) if !record.name.is_empty() => record.name.clone(),
                Some(_) => "paired".to_owned(),
                None => "unpaired".to_owned(),
            },
            None => "no-fingerprint".to_owned(),
        };
        println!(
            "{}\t{short}\t{pairing}\t{}",
            candidate.addr,
            source_name(candidate.source)
        );
    }
    Ok(())
}

/// Machine-readable JSON array of ranked discovery candidates.
/// `name` is the peers.book name when paired, else null.
fn discover_json(candidates: &[ak_discovery::PeerCandidate], book: &PeerBook) -> String {
    let mut out = String::from("[\n");
    for (index, candidate) in candidates.iter().enumerate() {
        let fingerprint = match &candidate.fingerprint {
            Some(fp) => format!("\"{}\"", fp.to_hex()),
            None => "null".to_owned(),
        };
        let name = match &candidate.fingerprint {
            Some(fp) => book.get(fp).map(|record| record.name.clone()),
            None => None,
        };
        let name_json = match name {
            Some(name) if !name.is_empty() => format!("\"{}\"", json_escape(&name)),
            _ => "null".to_owned(),
        };
        let paired = candidate
            .fingerprint
            .is_some_and(|fp| book.get(&fp).is_some());
        let comma = if index + 1 < candidates.len() {
            ","
        } else {
            ""
        };
        out.push_str(&format!(
            "  {{\"addr\": \"{}\", \"fingerprint\": {fingerprint}, \"paired\": {paired}, \
             \"name\": {name_json}, \"source\": \"{}\"}}{comma}\n",
            candidate.addr,
            source_name(candidate.source)
        ));
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------------
// invite (wave 21, B7): pair-invite subcommand family
// ---------------------------------------------------------------------------

/// `ak-cli invite ...`: thin printing layer over the `invite` module
/// cores (which are unit-tested without a TTY).
async fn invite_command(id: &Path, cmd: InviteCommand) -> Result<()> {
    match cmd {
        InviteCommand::Create {
            ttl_secs,
            long_lived,
            name,
            server,
            registry_fp,
        } => {
            let ttl = if long_lived {
                invite::TtlChoice::LongLived
            } else {
                invite::TtlChoice::Secs(ttl_secs.unwrap_or(600))
            };
            ttl.ttl_secs()?; // fail fast on out-of-range before connecting
            let settings = invite::resolve_registry(id, server, registry_fp)?;
            let stored = invite::invite_create(id, ttl, &settings).await?;
            println!("invite code (bearer credential — treat like a password):");
            println!("  {}", stored.display);
            println!(
                "code id {} revision {} expires {}",
                stored
                    .code_id
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                stored.revision,
                if stored.expires_ms == 0 {
                    "never (long-lived)".to_owned()
                } else {
                    format_utc_ms(stored.expires_ms)
                }
            );
            if let Some(name) = name {
                println!("local label: {name}");
            }
            println!("pairing answer: keep `ak-cli daemon --pairing` running to redeem it");
            Ok(())
        }
        InviteCommand::List {
            server,
            registry_fp,
        } => {
            let settings = invite::resolve_registry(id, server, registry_fp)?;
            let rows = invite::invite_list_rows(id, &settings).await?;
            if rows.is_empty() {
                println!("no invite codes (create one with `ak-cli invite create`)");
                return Ok(());
            }
            println!("code-id (32hex)  state    revision  expires              hint");
            for row in rows {
                let id_hex: String = row.code_id.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "{id_hex}  {:<7}  {:<8}   {:<20}  {}",
                    format!("{:?}", row.state).to_lowercase(),
                    row.revision,
                    if row.expires_ms == 0 {
                        "never".to_owned()
                    } else {
                        format_utc_ms(row.expires_ms)
                    },
                    row.hint.as_deref().unwrap_or("-"),
                );
            }
            Ok(())
        }
        InviteCommand::Revoke {
            code_id,
            server,
            registry_fp,
        } => {
            let settings = invite::resolve_registry(id, server, registry_fp)?;
            invite::invite_revoke_by_hex(id, &code_id, &settings).await?;
            println!("invite {code_id} revoked (server confirmed; local secret forgotten)");
            Ok(())
        }
        InviteCommand::Redeem {
            code,
            out,
            once,
            conflict,
            timeout,
            server,
            registry_fp,
        } => {
            let settings = invite::resolve_registry(id, server, registry_fp)?;
            let verified = invite::redeem_and_pair(id, &code, &settings).await?;
            let peer = verified.owner_fingerprint(); // the code owner is the peer
            println!("paired with the code owner:");
            println!("{}", fingerprint_lines("owner", &peer));
            println!(
                "waiting for transfers into {} (Ctrl-C stops; --once handles one job)",
                out.display()
            );
            if once {
                receive_one_after_pairing(id, out, &conflict, timeout).await
            } else {
                receive_loop_after_pairing(id, out, &conflict, timeout).await
            }
        }
    }
}

/// One `receive`-style job with the book accept policy (the owner was just
/// committed into peers.book by the pairing).
async fn receive_one_after_pairing(
    id: &Path,
    out: PathBuf,
    conflict: &str,
    timeout: u64,
) -> Result<()> {
    let listen = SocketAddr::from(([0, 0, 0, 0], 0));
    let config = ReceiveLoopConfig::resolve(
        id,
        out.clone(),
        listen,
        None,
        conflict,
        false,
        timeout,
        whoami(),
    )?;
    for line in expect_policy_lines(&config.policy) {
        println!("{line}");
    }
    let spec = config.iteration_spec();
    let printer = progress_printer("recv");
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let mut receiver =
        tokio::spawn(async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await });
    let addr = addr_rx.await.context("receiver failed to bind")?;
    println!(
        "receiving one transfer into {} (listening on {addr})",
        out.display()
    );
    println!("fingerprint (full): {}", config.fingerprint.to_hex());
    // A connection that closes before offering a job is not a transfer
    // (wave 27: a scheduler-capable sender's first attempt against this
    // legacy endpoint ends exactly that way, and it retries on a fresh
    // connection). Keep waiting on the SAME port until one real job, an
    // error worth surfacing, or the overall `--timeout` budget elapses.
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout.max(1));
    let report = loop {
        match receiver.await.context("receiver task failed")? {
            Ok(report) => break report,
            Err(ak_engine::EngineError::PeerClosed) if std::time::Instant::now() < deadline => {
                // Rebind on the port the sender was told about; the
                // ephemeral first bind must not change underneath a
                // retrying peer. The previous endpoint releases its UDP
                // socket asynchronously after drop, so an immediate rebind
                // can hit AddrInUse — bounded backoff, same reasoning as
                // the receive loop's rebind retries.
                tokio::time::sleep(Duration::from_millis(150)).await;
                let mut attempt = 0u32;
                loop {
                    let retry = ReceiveLoopConfig::resolve(
                        id,
                        out.clone(),
                        addr,
                        None,
                        conflict,
                        false,
                        timeout,
                        whoami(),
                    )?;
                    let spec = retry.iteration_spec();
                    let printer = progress_printer("recv");
                    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
                    let task = tokio::spawn(async move {
                        ak_engine::receive_with_addr(spec, &*printer, addr_tx).await
                    });
                    if addr_rx.await.is_ok() {
                        receiver = task;
                        break;
                    }
                    // Bind failed: settle the task, back off, retry.
                    let _ = task.await;
                    attempt += 1;
                    if attempt >= 8 || std::time::Instant::now() >= deadline {
                        bail!("cannot rebind the receive port {addr} after a pre-offer close");
                    }
                    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                }
                continue;
            }
            Err(other) => return Err(other.into()),
        }
    };
    report_summary(&report);
    Ok(())
}

/// The same sequential loop `receive` runs (Ctrl-C stops), reached after a
/// successful redeem+pair.
async fn receive_loop_after_pairing(
    id: &Path,
    out: PathBuf,
    conflict: &str,
    timeout: u64,
) -> Result<()> {
    let listen = SocketAddr::from(([0, 0, 0, 0], 0));
    let config = ReceiveLoopConfig::resolve(
        id,
        out.clone(),
        listen,
        None,
        conflict,
        false,
        timeout,
        whoami(),
    )?;
    for line in expect_policy_lines(&config.policy) {
        println!("{line}");
    }
    println!("{FIREWALL_NOTE}");
    println!("fingerprint (full): {}", config.fingerprint.to_hex());
    const MAX_REBIND_RETRIES: u32 = 50;
    let mut rebind_retries = 0u32;
    loop {
        let spec = config.iteration_spec();
        let printer = progress_printer("recv");
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let receiver =
            tokio::spawn(
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await },
            );
        let _ = addr_rx.await;
        match receiver.await.context("receiver task failed")? {
            Ok(report) => {
                rebind_retries = 0;
                report_summary(&report);
                println!("waiting for next sender (Ctrl-C stops)...");
            }
            Err(ak_engine::EngineError::Io(err))
                if err.kind() == std::io::ErrorKind::AddrInUse
                    && rebind_retries < MAX_REBIND_RETRIES =>
            {
                rebind_retries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(error) => {
                println!("job failed: {error}; still waiting (Ctrl-C stops)");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// registry-server (wave 21, B7): the authoritative v2 pairing registry
// ---------------------------------------------------------------------------

/// Default registry SQLite location when --db-path is omitted.
fn registry_db_path(id: &Path) -> PathBuf {
    id.join("registry.db")
}

/// Build the server_v2 config from the flags (pure; unit-tested).
fn registry_server_config(
    id: &Path,
    port: Option<u16>,
    bind: Option<BindArg>,
    db_path: Option<PathBuf>,
) -> (ak_signal::server_v2::Config, Option<String>) {
    const DEFAULT_REGISTRY_PORT: u16 = ak_signal::server_v2::config::DEFAULT_V2_PORT;
    let (listen, note) = resolve_listen_addr(bind, "--bind", port, "--port", DEFAULT_REGISTRY_PORT);
    let config = ak_signal::server_v2::Config {
        listen,
        db_path: db_path.unwrap_or_else(|| registry_db_path(id)),
        // G6 (P2-09 ServerInfo): the revision a client can query over the
        // wire — the same source as the version line (`0.1.0+<sha>`),
        // sanitized to the 32-byte wire budget by the engine (a dirty or
        // over-long build stamp degrades to "unknown" there).
        revision: Some(format!("{}+{}", env!("CARGO_PKG_VERSION"), build_git_sha())),
        ..ak_signal::server_v2::Config::default()
    };
    (config, note)
}

/// The registry id clients pin with `--registry-fp`: the lowercase hex of
/// the registry identity's Ed25519 public key. ONE formatter for both
/// `registry-server` (what is served) and `registry-id` (what would be
/// served), so the two can never drift apart.
fn registry_pin(registry: &DeviceKeyPair) -> String {
    hex_of(&registry.public_key())
}

/// The pin of an EXISTING `<id-dir>/registry.key`, never creating one (see
/// [`DeviceKeyPair::load_existing`]).
fn registry_id_of(id: &Path) -> Result<String> {
    let key_path = id.join("registry.key");
    let registry = DeviceKeyPair::load_existing(&key_path)
        .with_context(|| format!("no usable registry identity at {}", key_path.display()))?;
    Ok(registry_pin(&registry))
}

/// `ak-cli registry-id`: print the pin of the existing registry identity as
/// ONE line holding exactly 64 lowercase hex characters (machine-readable:
/// the deploy script compares it before switching services over).
fn registry_id_command(id: &Path) -> Result<()> {
    let pin = registry_id_of(id)?;
    println!("{pin}");
    Ok(())
}

/// `ak-cli registry-server`: load/create `<id-dir>/registry.key`, bind the
/// listener and open the store, then print the bound address and the
/// registry pin, and serve until Ctrl-C (a failed bind, store or serve
/// loop ends the command with its error). TLS-only: no plaintext invite
/// service exists to fall back to.
async fn registry_server_command(
    id: &Path,
    port: Option<u16>,
    bind: Option<BindArg>,
    db_path: Option<PathBuf>,
) -> Result<()> {
    // CR-P2-09 (version observability): the production server must be
    // identifiable from its own output. Initialize logging (Info default,
    // `RUST_LOG` overrides) so the engine's own info lines -- including
    // the `v2 registry server listening` startup line -- reach the
    // journal/stderr, then LOG the binary identity once at startup; the
    // same payload answers `ak-cli registry-server --version`.
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .try_init();
    log::info!("registry-server version: {}", version_line());
    let key_path = id.join("registry.key");
    let (registry, created) = DeviceKeyPair::load_or_create(&key_path)?;
    if created {
        println!("registry identity written to {}", key_path.display());
    }
    let (config, note) = registry_server_config(id, port, bind, db_path);
    if let Some(note) = note {
        println!("{note}");
    }
    // Ready before announced (2026-11-19 review): bind and open the store
    // FIRST. Deploy checks, scripts and the itests start clients as soon
    // as the pin line appears, so it must mean "accepting connections";
    // a port in use or an unusable database fails the command here
    // instead of leaving a process that printed its pin but never
    // listens. The address printed is the bound one (`--port 0` works).
    let (listener, store) = ak_signal::server_v2::engine::bind_v2(&config)
        .await
        .context("registry-server cannot start")?;
    let listen = listener
        .local_addr()
        .context("registry-server listener address")?;
    let registry_id = registry_pin(&registry);
    println!(
        "v2 pairing registry on tls {listen} (db {}); no file routes exist",
        config.db_path.display()
    );
    println!("registry id (pin with --registry-fp): {registry_id}");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut server = tokio::spawn(async move {
        ak_signal::server_v2::engine::serve_v2_with_revision(
            listener,
            &registry,
            store,
            config.limits,
            config.revision.as_deref(),
            shutdown_rx,
        )
        .await
    });
    // A serve loop that ends on its own ends the command with its error
    // rather than idling until Ctrl-C.
    let ended_early = tokio::select! {
        _ = tokio::signal::ctrl_c() => None,
        ended = &mut server => Some(ended),
    };
    match ended_early {
        None => {
            let _ = shutdown_tx.send(true);
            server.await??;
            Ok(())
        }
        Some(ended) => {
            ended??;
            bail!("the registry server stopped without a shutdown request")
        }
    }
}

/// `ak-cli stun-server` (CR-P2-13): a standalone UDP STUN binding responder
/// — one process, one socket. It runs the same `ak_signal::stun::serve_stun`
/// loop the old signal-server runs (zero drift between the two paths), so
/// the old signaling process can be retired without silently disabling
/// STUN, which fails open and would otherwise go unnoticed. Coordination
/// only: it answers Binding requests with the sender's reflexive address
/// and never relays any payload (ADR-001).
async fn stun_server_command(port: Option<u16>, bind: Option<BindArg>) -> Result<()> {
    // Same logging contract as registry-server (CR-P2-09): initialize
    // env_logger (Info default, `RUST_LOG` overrides) so the responder's
    // own `stun responder listening on udp …` startup line reaches the
    // journal/stderr, and log the binary identity once at startup; the
    // same payload answers `ak-cli stun-server --version`.
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .try_init();
    log::info!("stun-server version: {}", version_line());
    const DEFAULT_STUN_PORT: u16 = 7101;
    let (addr, note) = resolve_listen_addr(bind, "--bind", port, "--port", DEFAULT_STUN_PORT);
    if let Some(note) = note {
        println!("{note}");
    }
    let socket = tokio::net::UdpSocket::bind(addr)
        .await
        .with_context(|| format!("binding STUN udp {addr}"))?;
    println!(
        "stun responder on udp {}; no file routes exist",
        socket.local_addr()?
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(ak_signal::stun::serve_stun(socket, shutdown_rx));
    tokio::signal::ctrl_c().await.ok();
    let _ = shutdown_tx.send(true);
    server.await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // M-N1 mitigation (batch N): widen this process's fd headroom before
    // any receive path can prepare part files. Pure mitigation — the real
    // fix keeps at most one open part file per active stream — so a None
    // return (nothing to raise / non-Unix / failed raise) is fine.
    if let Some(limit) = ak_storage::raise_nofile_soft_limit_to_hard() {
        log::info!("RLIMIT_NOFILE soft limit raised to {limit}");
    }
    let cli = Cli::parse();
    // `registry-id` is read-only by contract: it resolves the identity
    // directory WITHOUT the create_dir_all every other command performs.
    let id = if matches!(cli.command, Command::RegistryId) {
        id_dir_path(&cli)
    } else {
        id_dir(&cli)
    };
    match cli.command {
        Command::Keygen => {
            let (key, _) = load_identity(&id)?;
            print_identity(&key);
            Ok(())
        }
        Command::Fingerprint => {
            let (key, _) = load_identity(&id)?;
            print_identity(&key);
            Ok(())
        }
        Command::Pair { cmd } => pair_command(&id, cmd),
        Command::Invite { cmd } => invite_command(&id, cmd).await,
        Command::Task { cmd } => task_cli::task_command(&id, cmd).await,
        Command::RegistryServer {
            port,
            bind,
            db_path,
        } => registry_server_command(&id, port, bind, db_path).await,
        Command::RegistryId => registry_id_command(&id),
        Command::Receive {
            out,
            port,
            bind,
            expect,
            conflict,
            once,
            no_internet,
            timeout,
            jobs,
        } => {
            receive_command(
                &id,
                out,
                port,
                bind,
                expect,
                conflict,
                once,
                no_internet,
                timeout,
                jobs,
            )
            .await
        }
        Command::Daemon {
            out,
            port,
            expect,
            conflict,
            timeout,
            name,
            json_lines,
            invite_code,
            pairing,
            same_code,
            cross_code,
            server,
            registry_fp,
        } => {
            daemon::daemon_command(
                &id,
                daemon::DaemonOptions {
                    out,
                    port,
                    expect,
                    conflict,
                    timeout,
                    name,
                    json_lines,
                    invite_code,
                    pairing,
                    same_code,
                    cross_code,
                    server,
                    registry_fp,
                },
            )
            .await
        }
        Command::Send {
            paths,
            to,
            fp,
            port,
            conflict,
            multistream,
            no_internet,
            timeout,
        } => {
            send_command(
                &id,
                paths,
                to,
                fp,
                port,
                conflict,
                multistream,
                no_internet,
                timeout,
            )
            .await
        }
        Command::Discover { seconds, json } => discover_command(&id, seconds, json).await,
        Command::Jobs { cmd } => jobs_command(&id, cmd),
        Command::Doctor {
            stun,
            mdns_seconds,
            json,
        } => doctor::doctor_command(&id, stun, mdns_seconds, json).await,
        // G5 / E1: the legacy plaintext signal server; feature-gated with
        // its Command variant (default builds have no such subcommand).
        #[cfg(feature = "legacy-plaintext-signal")]
        Command::SignalServer {
            tcp_port,
            stun_port,
            bind,
            stun_bind,
        } => {
            const DEFAULT_SIGNAL_TCP_PORT: u16 = 7100;
            const DEFAULT_SIGNAL_STUN_PORT: u16 = 7101;
            let (tcp_addr, tcp_note) = resolve_listen_addr(
                bind,
                "--bind",
                tcp_port,
                "--tcp-port",
                DEFAULT_SIGNAL_TCP_PORT,
            );
            let (stun_addr, stun_note) = resolve_listen_addr(
                stun_bind,
                "--stun-bind",
                stun_port,
                "--stun-port",
                DEFAULT_SIGNAL_STUN_PORT,
            );
            for note in [tcp_note, stun_note].into_iter().flatten() {
                println!("{note}");
            }
            let config = ak_signal::server::Config {
                tcp: tcp_addr,
                stun_udp: stun_addr,
                ..ak_signal::server::Config::default()
            };
            println!(
                "signaling on {} (STUN {}); no file routes exist",
                config.tcp, config.stun_udp
            );
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let server =
                tokio::spawn(async move { ak_signal::server::run(config, shutdown_rx).await });
            tokio::signal::ctrl_c().await.ok();
            let _ = shutdown_tx.send(true);
            server.await??;
            Ok(())
        }
        Command::StunServer { port, bind } => stun_server_command(port, bind).await,
        // G5 / E1: the legacy signal-plane probe; gated with its variant.
        #[cfg(feature = "legacy-plaintext-signal")]
        Command::Probe {
            server,
            peer,
            no_stun,
            dual_stack,
            json,
        } => probe_command(&id, server, peer, no_stun, dual_stack, json).await,
        Command::Bench {
            bytes,
            files,
            chunk_kb,
            multistream,
            once,
            json,
        } => bench_command(bytes, files, chunk_kb, multistream, once, json).await,
        Command::Completions { shell } => completions_command(shell),
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "ak-device".into())
}

// ---------------------------------------------------------------------------
// probe: structured run record shared by text output and --json
//
// G5 / E1 (frozen legacy model): everything in this section belongs to the
// legacy plaintext signal-plane probe and is compiled only with the
// `legacy-plaintext-signal` feature.
// ---------------------------------------------------------------------------

/// STUN mapping query outcome for one probe run.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeStun {
    /// --no-stun was given.
    Skipped,
    /// Mapping learned (address included).
    Ok(SocketAddr),
    /// Server answered with an error (message included).
    Failed(String),
    /// No answer within the budget.
    TimedOut,
}

/// One hole-punch round, mirroring ak_net::holepunch's PunchReport.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Clone, Default)]
struct ProbePunch {
    packets_sent: u64,
    packets_received: u64,
    reachable: Vec<SocketAddr>,
    peer_observed_from: Option<SocketAddr>,
}

/// QUIC verification round after a successful punch.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Clone, Default)]
struct ProbeHandshake {
    attempted: bool,
    ok: bool,
    path: Option<&'static str>,
    peer_fingerprint: Option<String>,
    error: Option<String>,
}

/// Everything one probe run learned, collected in BOTH output modes so
/// `--json` can emit one stable object at the end. Field names are a
/// script-facing contract: extend, never rename.
#[cfg(feature = "legacy-plaintext-signal")]
#[derive(Debug, Clone)]
struct ProbeRun {
    server: String,
    peer: String,
    device_id: String,
    fingerprint: String,
    stun: ProbeStun,
    registered_candidates: usize,
    candidates: Vec<String>,
    session_open: bool,
    session_peer: Option<String>,
    peer_candidates: Vec<String>,
    same_egress: bool,
    punch: Option<ProbePunch>,
    role: Option<&'static str>,
    handshake: ProbeHandshake,
    conclusion: &'static str,
}

#[cfg(feature = "legacy-plaintext-signal")]
impl Default for ProbeRun {
    fn default() -> Self {
        ProbeRun {
            server: String::new(),
            peer: String::new(),
            device_id: String::new(),
            fingerprint: String::new(),
            stun: ProbeStun::Skipped,
            registered_candidates: 0,
            candidates: Vec::new(),
            session_open: false,
            session_peer: None,
            peer_candidates: Vec::new(),
            same_egress: false,
            punch: None,
            role: None,
            handshake: ProbeHandshake::default(),
            conclusion: "no-peer-candidates",
        }
    }
}

/// Stable JSON name for the engine's path classification.
#[cfg(feature = "legacy-plaintext-signal")]
fn path_kind_name(path: PathKind) -> &'static str {
    match path {
        PathKind::LocalDirect => "local-direct",
        PathKind::InternetDirect => "internet-direct",
        // A relay path can never occur (direct-only); reported for completeness.
        PathKind::Relay => "relay",
        PathKind::Unknown => "unknown",
    }
}

/// JSON string-array of already-stringified values ([] when empty).
pub(crate) fn json_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_owned();
    }
    let inner: Vec<String> = items
        .iter()
        .map(|item| format!("\"{}\"", json_escape(item)))
        .collect();
    format!("[{}]", inner.join(", "))
}

#[cfg(feature = "legacy-plaintext-signal")]
fn json_opt_string(value: &Option<String>) -> String {
    match value {
        Some(text) => format!("\"{}\"", json_escape(text)),
        None => "null".to_owned(),
    }
}

/// One probe run as a single JSON object (the only stdout content in --json
/// mode). Shape mirrors ProbeRun; see that struct for the field contract.
#[cfg(feature = "legacy-plaintext-signal")]
fn probe_json(run: &ProbeRun) -> String {
    let stun = match &run.stun {
        ProbeStun::Skipped => {
            "{\"status\": \"skipped\", \"mapped\": null, \"error\": null}".to_owned()
        }
        ProbeStun::Ok(addr) => {
            format!("{{\"status\": \"ok\", \"mapped\": \"{addr}\", \"error\": null}}")
        }
        ProbeStun::TimedOut => {
            "{\"status\": \"timeout\", \"mapped\": null, \"error\": null}".to_owned()
        }
        ProbeStun::Failed(message) => format!(
            "{{\"status\": \"failed\", \"mapped\": null, \"error\": \"{}\"}}",
            json_escape(message)
        ),
    };
    let punch = match &run.punch {
        None => "null".to_owned(),
        Some(round) => {
            let reachable: Vec<String> = round
                .reachable
                .iter()
                .map(|addr| format!("\"{addr}\""))
                .collect();
            let observed = match round.peer_observed_from {
                Some(addr) => format!("\"{addr}\""),
                None => "null".to_owned(),
            };
            format!(
                "{{\"packets_sent\": {}, \"packets_received\": {}, \"reachable\": [{}], \
                 \"peer_observed_from\": {}}}",
                round.packets_sent,
                round.packets_received,
                reachable.join(", "),
                observed
            )
        }
    };
    let role = match run.role {
        Some(role) => format!("\"{role}\""),
        None => "null".to_owned(),
    };
    let path = match run.handshake.path {
        Some(path) => format!("\"{path}\""),
        None => "null".to_owned(),
    };
    format!(
        "{{\n\
           \"server\": \"{}\",\n\
           \"peer\": \"{}\",\n\
           \"device_id\": \"{}\",\n\
           \"fingerprint\": \"{}\",\n\
           \"stun\": {},\n\
           \"registered_candidates\": {},\n\
           \"candidates\": {},\n\
           \"session_open\": {},\n\
           \"session_peer\": {},\n\
           \"peer_candidates\": {},\n\
           \"same_egress\": {},\n\
           \"punch\": {},\n\
           \"role\": {},\n\
           \"handshake\": {{\"attempted\": {}, \"ok\": {}, \"path\": {}, \
           \"peer_fingerprint\": {}, \"error\": {}}},\n\
           \"conclusion\": \"{}\"\n\
         }}",
        json_escape(&run.server),
        json_escape(&run.peer),
        json_escape(&run.device_id),
        json_escape(&run.fingerprint),
        stun,
        run.registered_candidates,
        json_string_array(&run.candidates),
        run.session_open,
        json_opt_string(&run.session_peer),
        json_string_array(&run.peer_candidates),
        run.same_egress,
        punch,
        role,
        run.handshake.attempted,
        run.handshake.ok,
        path,
        json_opt_string(&run.handshake.peer_fingerprint),
        json_opt_string(&run.handshake.error),
        run.conclusion
    )
}

/// Probe progress goes to stdout in text mode and to stderr in --json mode,
/// so JSON stdout carries exactly one object.
#[cfg(feature = "legacy-plaintext-signal")]
fn probe_emit(json: bool, line: &str) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

#[cfg(feature = "legacy-plaintext-signal")]
#[allow(clippy::too_many_arguments)]
async fn probe_command(
    id: &Path,
    server: SocketAddr,
    peer: String,
    no_stun: bool,
    dual_stack: bool,
    json: bool,
) -> Result<()> {
    let (key, bundle) = load_identity(id)?;
    let peer_id_hex = peer.trim().to_ascii_lowercase();
    let target: [u8; 32] = {
        let bytes = hex_to_bytes(&peer_id_hex).context("peer must be 64 hex characters")?;
        let mut id = [0u8; 32];
        if bytes.len() != 32 {
            bail!("peer fingerprint must be 64 hex characters");
        }
        id.copy_from_slice(&bytes);
        id
    };
    probe_emit(json, &format!("device id: {}", hex_of(&key.public_key())));
    probe_emit(json, &fingerprint_lines("fingerprint", &key.fingerprint()));

    // 1. Bind the transfer UDP socket first: every mapped/public candidate
    //    must belong to the socket we would actually transfer on.
    let socket = if dual_stack {
        match ak_net::holepunch::bind_dual_stack_udp(0) {
            Ok(std_socket) => {
                probe_emit(
                    json,
                    "dual-stack probe socket (v4 peers via ::ffff mapping)",
                );
                tokio::net::UdpSocket::from_std(std_socket)?
            }
            Err(error) => {
                probe_emit(
                    json,
                    &format!("dual-stack bind unavailable ({error}); falling back to v4"),
                );
                tokio::net::UdpSocket::bind("0.0.0.0:0").await?
            }
        }
    } else {
        tokio::net::UdpSocket::bind("0.0.0.0:0").await?
    };
    let local = socket.local_addr()?;
    probe_emit(json, &format!("transfer socket: {local}"));

    // 2. Local candidates: real interface addresses plus loopback (never the
    //    unspecified 0.0.0.0 bind address, which is not dialable), then the
    //    STUN mapping via the same socket (coordination plane, no data).
    let mut candidates: Vec<SocketAddr> = ak_net::classify::local_nets()?
        .into_iter()
        .map(|net| SocketAddr::new(net.addr, local.port()))
        .collect();
    candidates.push(SocketAddr::from(([127, 0, 0, 1], local.port())));
    let mut stun_state = ProbeStun::Skipped;
    if !no_stun {
        let stun_addr = SocketAddr::new(server.ip(), server.port() + 1);
        match tokio::time::timeout(
            Duration::from_secs(6),
            ak_signal::stun::query_on(&socket, stun_addr, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(mapped)) => {
                stun_state = ProbeStun::Ok(mapped);
                probe_emit(json, &format!("STUN mapped address: {mapped}"));
                candidates.push(mapped);
            }
            Ok(Err(error)) => {
                stun_state = ProbeStun::Failed(error.to_string());
                probe_emit(json, &format!("STUN query failed: {error}"));
            }
            Err(_) => {
                stun_state = ProbeStun::TimedOut;
                probe_emit(json, "STUN query timed out");
            }
        }
    }

    // 3. Signaling: register candidates and open a session toward the peer.
    let client = ak_signal::client::SignalClient::connect(server, &key, &whoami())
        .await
        .context("signaling connect failed")?;
    candidates.sort();
    candidates.dedup();
    client.register(&candidates).await?;
    let registered = candidates.len();
    probe_emit(json, &format!("registered {registered} candidates"));
    client.connect_peer(target).await?;

    // 4. Exchange signals until we see the peer's candidates, then punch.
    let mut peer_candidates: Vec<SocketAddr> = Vec::new();
    let mut reachable: Option<SocketAddr> = None;
    let mut session_peer_hex: Option<String> = None;
    let mut same_egress = OnceNotice::new();
    let mut same_egress_seen = false;
    let mut punch_data: Option<ProbePunch> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    'signal: while Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next()).await??;
        match message {
            ak_signal::codec::CtrlMessage::SessionOpened { session, peer } => {
                session_peer_hex = Some(hex_of(&peer));
                probe_emit(json, &format!("session open with peer {}", hex_of(&peer)));
                client
                    .send_signal(
                        session,
                        ak_signal::codec::PeerSignal::Candidates(candidates.clone()),
                    )
                    .await?;
            }
            ak_signal::codec::CtrlMessage::Signal { signal, .. } => match signal {
                ak_signal::codec::PeerSignal::Candidates(list) => {
                    peer_candidates = list;
                    probe_emit(json, &format!("peer candidates: {peer_candidates:?}"));
                    let report = ak_net::holepunch::probe(
                        &socket,
                        &peer_candidates,
                        Duration::from_millis(200),
                        Duration::from_secs(5),
                    )
                    .await?;
                    probe_emit(
                        json,
                        &format!(
                            "punch: sent {} recv {} reachable {:?} from {:?}",
                            report.packets_sent,
                            report.packets_received,
                            report.reachable,
                            report.peer_observed_from
                        ),
                    );
                    punch_data = Some(ProbePunch {
                        packets_sent: report.packets_sent,
                        packets_received: report.packets_received,
                        reachable: report.reachable.clone(),
                        peer_observed_from: report.peer_observed_from,
                    });
                    if let Some(first) = report.reachable.first() {
                        reachable = Some(*first);
                        break 'signal;
                    }
                }
                ak_signal::codec::PeerSignal::SameEgress => {
                    // The server may repeat the hint; announce it once.
                    same_egress_seen = true;
                    if let Some(text) = same_egress.take() {
                        probe_emit(json, text);
                    }
                }
                other => probe_emit(json, &format!("signal: {other:?}")),
            },
            other => probe_emit(json, &format!("server: {other:?}")),
        }
    }
    let mut role: Option<&'static str> = None;
    let mut handshake_data = ProbeHandshake::default();
    if let Some(target_addr) = reachable {
        // 5. Real verification: QUIC handshake with pinned identity on the
        // SAME socket that punched (keeps NAT pinholes valid). Role is
        // assigned deterministically from the two device ids so both sides
        // agree without extra signaling: smaller key connects.
        let our_key = key.public_key();
        let pinned = Fingerprint::from_public_key(&target);
        let std_socket = socket.into_std()?;
        let endpoint = ak_net::DirectEndpoint::bind_with_socket(std_socket, &bundle).await?;
        handshake_data.attempted = true;
        let handshake = if our_key < target {
            role = Some("client");
            probe_emit(json, &format!("role: client, dialing {target_addr}"));
            endpoint.connect(target_addr, &pinned).await
        } else {
            role = Some("server");
            probe_emit(json, "role: server, waiting on the punched path");
            let authorize = move |fp: &Fingerprint| fp == &pinned;
            match tokio::time::timeout(Duration::from_secs(15), endpoint.accept(&authorize)).await {
                Ok(result) => result,
                Err(_) => Err(ak_net::ConnectError::Timeout(Duration::from_secs(15))),
            }
        };
        match handshake {
            Ok(channel) => {
                handshake_data.ok = true;
                handshake_data.path = Some(path_kind_name(channel.path()));
                handshake_data.peer_fingerprint = Some(channel.peer_fingerprint().to_hex());
                probe_emit(
                    json,
                    &format!(
                        "QUIC handshake OK path={:?} peer={}",
                        channel.path(),
                        channel.peer_fingerprint().to_hex()
                    ),
                );
            }
            Err(error) => {
                handshake_data.error = Some(error.to_string());
                probe_emit(json, &format!("QUIC handshake failed after punch: {error}"));
            }
        }
    } else if peer_candidates.is_empty() {
        probe_emit(
            json,
            "no peer candidates received; peer may be offline or behind an unreachable network",
        );
    } else {
        probe_emit(
            json,
            "no candidate answered probes; this network pair may not support direct \
             connections (no relay fallback exists)",
        );
    }
    let _ = client.close().await;
    let conclusion = if reachable.is_some() {
        if handshake_data.ok {
            "connected"
        } else {
            "handshake-failed"
        }
    } else if peer_candidates.is_empty() {
        "no-peer-candidates"
    } else {
        "no-reachable-candidate"
    };
    let run = ProbeRun {
        server: server.to_string(),
        peer: peer_id_hex,
        device_id: hex_of(&key.public_key()),
        fingerprint: key.fingerprint().to_hex(),
        stun: stun_state,
        registered_candidates: registered,
        candidates: candidates.iter().map(|addr| addr.to_string()).collect(),
        session_open: session_peer_hex.is_some(),
        session_peer: session_peer_hex,
        peer_candidates: peer_candidates
            .iter()
            .map(|addr| addr.to_string())
            .collect(),
        same_egress: same_egress_seen,
        punch: punch_data,
        role,
        handshake: handshake_data,
        conclusion,
    };
    probe_emit(
        json,
        "probe finished (direct-only: no relay fallback exists)",
    );
    if json {
        println!("{}", probe_json(&run));
    }
    Ok(())
}

fn hex_to_bytes(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("odd-length hex string");
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0])
            .with_context(|| format!("invalid hex digit '{}'", pair[0] as char))?;
        let low = hex_digit(pair[1])
            .with_context(|| format!("invalid hex digit '{}'", pair[1] as char))?;
        out.push(high * 16 + low);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
