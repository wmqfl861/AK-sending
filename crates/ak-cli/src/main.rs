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

mod args;
mod bench;
mod cli;
mod jobs;
mod output;
mod pair_jobs;
mod peers;
mod receive_cmd;
mod send_cmd;
mod services;

use args::*;
use bench::*;
use cli::*;
use jobs::*;
use output::*;
use pair_jobs::*;
use peers::*;
use receive_cmd::*;
use send_cmd::*;
use services::*;

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
