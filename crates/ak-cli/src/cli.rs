//! The clap command line: `Cli` and its subcommand enums.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

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
pub(super) struct Cli {
    /// Identity directory holding device.key, peers.book, checkpoints
    /// (default: `<home>/.ak-sending`).
    #[arg(long, global = true)]
    pub(super) id_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
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
pub(super) enum PairCommand {
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
pub(super) enum InviteCommand {
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
pub(super) enum JobsCommand {
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
