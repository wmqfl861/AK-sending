//! Command-line prototype for AK-sending (single binary, many verbs).
//!
//! Verbs: `keygen`, `fingerprint`, `receive`, `send`, `discover`,
//! `signal-server`, `probe`. The binary never talks to any file-carrying
//! server; servers started here are coordination-only (ADR-001).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use ak_core::handle::ConflictPolicy;
use ak_engine::{Progress, ReceiveSpec, Report, SendSpec};
use ak_identity::tls::TlsBundle;
use ak_identity::{DeviceKeyPair, Fingerprint};

#[derive(Debug, Parser)]
#[command(name = "ak-cli", version, about = "AK-sending direct-only prototype")]
struct Cli {
    /// Identity directory (default: <home>/.ak-sending).
    #[arg(long, global = true)]
    id_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or show the device key, fingerprint, and pairing code.
    Keygen,
    /// Print the current fingerprint and pairing code.
    Fingerprint,
    /// Receive one transfer into a directory.
    Receive {
        /// Destination directory (must exist).
        #[arg(long, default_value = ".")]
        out: PathBuf,
        /// UDP port to listen on (0 = auto).
        #[arg(long, default_value_t = 7100)]
        port: u16,
        /// Only accept this peer fingerprint (64 hex or 6-digit pairing code).
        #[arg(long)]
        expect: Option<String>,
        /// Overwrite policy: error (default) or rename.
        #[arg(long, default_value = "error")]
        conflict: String,
    },
    /// Send files or a directory to a paired peer.
    Send {
        /// Files or one directory to send.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Peer address ip:port.
        #[arg(long)]
        to: SocketAddr,
        /// Peer fingerprint (64 hex or 6-digit pairing code), pinned for this transfer.
        #[arg(long)]
        fp: String,
        /// Local UDP port (0 = auto).
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value = "error")]
        conflict: String,
    },
    /// Discover peers on the local network via mDNS (bounded browse).
    Discover {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// Run the coordination server (signaling + STUN; no file routes).
    SignalServer {
        #[arg(long, default_value_t = 7100)]
        tcp_port: u16,
        #[arg(long, default_value_t = 7101)]
        stun_port: u16,
    },
    /// Public direct-connect probe: register, exchange candidates, punch, QUIC.
    Probe {
        #[arg(long)]
        server: SocketAddr,
        /// Peer device id (64-hex fingerprint) to punch toward.
        #[arg(long)]
        peer: String,
        /// Skip the STUN mapping query.
        #[arg(long, default_value_t = false)]
        no_stun: bool,
    },
}

fn id_dir(cli: &Cli) -> PathBuf {
    let base = cli.id_dir.clone().unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".ak-sending")
    });
    std::fs::create_dir_all(&base).ok();
    base
}

fn parse_fingerprint(text: &str) -> Result<Fingerprint> {
    let trimmed = text.trim();
    if trimmed.len() == 64 {
        Fingerprint::from_hex(trimmed).context("fingerprint must be 64 hex characters")
    } else {
        bail!("unknown fingerprint format: expected 64 hex characters (pairing-code pinning is interactive-only in receive --expect)")
    }
}

/// `--expect` accepts 64-hex or a 6-digit pairing code matched against the
/// actually connected peer (compared after the handshake).
type FpMatcher = Arc<dyn Fn(&Fingerprint) -> bool + Send + Sync>;

fn expect_matcher(text: Option<&str>) -> Result<FpMatcher> {
    let Some(text) = text else {
        return Ok(Arc::new(|_| true));
    };
    let trimmed = text.trim();
    let matcher: Arc<dyn Fn(&Fingerprint) -> bool + Send + Sync> = if trimmed.len() == 64 {
        let pinned = Fingerprint::from_hex(trimmed).context("invalid --expect fingerprint")?;
        Arc::new(move |fp| fp == &pinned)
    } else {
        // Six digits: accept any peer whose pairing code matches the typed one.
        let typed = trimmed.to_string();
        Arc::new(move |fp| ak_identity::verify_pairing_code(fp, &typed))
    };
    Ok(matcher)
}

fn conflict_policy(text: &str) -> Result<ConflictPolicy> {
    match text {
        "error" => Ok(ConflictPolicy::Fail),
        "rename" => Ok(ConflictPolicy::Rename),
        other => bail!("unknown conflict policy {other:?}; use error|rename"),
    }
}

fn checkpoint_dir(id: &Path) -> PathBuf {
    id.join("checkpoints")
}

fn load_identity(id: &Path) -> Result<(DeviceKeyPair, TlsBundle)> {
    let (key, created) = DeviceKeyPair::load_or_create(&id.join("device.key"))
        .with_context(|| format!("cannot load device key in {}", id.display()))?;
    if created {
        eprintln!("created new device key in {}", id.join("device.key").display());
    }
    let bundle = ak_identity::tls::make_self_signed(&key);
    Ok((key, bundle))
}

fn human_bytes(bytes: u64) -> String {
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

fn print_progress(direction: &str, progress: &Progress) {
    match progress {
        Progress::Connected { peer } => println!("[{direction}] connected; peer {peer}"),
        Progress::Offered { files, bytes } => {
            println!("[{direction}] offered {files} entries, {}", human_bytes(*bytes))
        }
        Progress::Accepted { files, bytes } => {
            println!("[{direction}] accepted {files} entries, {}", human_bytes(*bytes))
        }
        Progress::Resuming { chunks } => println!("[{direction}] resuming after {chunks} verified chunks"),
        Progress::FileStarted { path, bytes, .. } => {
            println!("[{direction}] {path} ({})", human_bytes(*bytes))
        }
        Progress::FileCommitted { path, .. } => println!("[{direction}] committed {path}"),
        Progress::FinalAck { verified } => println!("[{direction}] final ack verified={verified}"),
        Progress::Done => println!("[{direction}] done"),
        Progress::ManifestReceived => println!("[{direction}] manifest received"),
        _ => {}
    }
}

fn progress_printer(direction: &'static str) -> Arc<dyn Fn(Progress) + Send + Sync> {
    Arc::new(move |progress: Progress| {
        if std::env::var_os("AK_QUIET").is_none() {
            print_progress(direction, &progress);
        }
    })
}

fn report_summary(report: &Report) {
    println!(
        "job {} complete: {} entries, {}, resumed {} chunks, peer {}",
        report.job_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        report.files,
        human_bytes(report.bytes),
        report.resumed_chunks,
        report.peer_fingerprint.as_deref().unwrap_or("?")
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let id = id_dir(&cli);
    match cli.command {
        Command::Keygen => {
            let (key, _bundle) = load_identity(&id)?;
            let fp = key.fingerprint();
            println!("device id: {}", hex_of(&key.public_key()));
            println!("fingerprint: {}", fp.to_hex());
            println!("pairing code: {}", ak_identity::pairing_code(&fp));
            Ok(())
        }
        Command::Fingerprint => {
            let (key, _) = load_identity(&id)?;
            let fp = key.fingerprint();
            println!("device id: {}", hex_of(&key.public_key()));
            println!("fingerprint: {}", fp.to_hex());
            println!("pairing code: {}", ak_identity::pairing_code(&fp));
            Ok(())
        }
        Command::Receive { out, port, expect, conflict } => {
            let (key, bundle) = load_identity(&id)?;
            let fp = key.fingerprint();
            let matcher = expect_matcher(expect.as_deref())?;
            let mut spec = ReceiveSpec::new(out.clone(), bundle);
            spec.bind = SocketAddr::from(([0, 0, 0, 0], port));
            spec.conflict = conflict_policy(&conflict)?;
            spec.checkpoint_dir = checkpoint_dir(&id);
            spec.device_name = whoami();
            let authorize = {
                let matcher = Arc::clone(&matcher);
                move |peer: &Fingerprint| -> bool { matcher(peer) }
            };
            spec.authorize = Box::new(authorize);
            let printer = progress_printer("recv");
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            let receiver = tokio::spawn({
                let spec = spec;
                let printer = printer;
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await }
            });
            let addr = addr_rx.await.context("receiver failed to bind")?;
            println!("receiving into {}", out.display());
            println!("address: {addr}");
            println!("fingerprint: {}", fp.to_hex());
            println!("pairing code: {}", ak_identity::pairing_code(&fp));
            println!("waiting for sender (Ctrl-C stops)...");
            let report = receiver.await??;
            report_summary(&report);
            Ok(())
        }
        Command::Send { paths, to, fp, port, conflict } => {
            let (key, bundle) = load_identity(&id)?;
            let pinned = parse_fingerprint(&fp)?;
            if paths.len() > 1 {
                bail!("send exactly one file or one directory per run in this prototype");
            }
            let root = paths[0].canonicalize().with_context(|| paths[0].display().to_string())?;
            let mut spec = SendSpec::new(root, to, pinned, bundle);
            spec.bind = SocketAddr::from(([0, 0, 0, 0], port));
            spec.conflict = conflict_policy(&conflict)?;
            spec.checkpoint_dir = checkpoint_dir(&id);
            spec.device_name = whoami();
            let _ = key;
            let printer = progress_printer("send");
            let report = ak_engine::send(spec, &*printer).await?;
            report_summary(&report);
            Ok(())
        }
        Command::Discover { seconds } => {
            let (key, _) = load_identity(&id)?;
            let fp = key.fingerprint();
            let mut mdns = ak_discovery::MdnsDiscovery::new().context("mDNS daemon unavailable")?;
            mdns.advertise(7100, &fp, &whoami())
                .context("mDNS advertise failed (multicast may be blocked)")?;
            let nets: Vec<_> = ak_net::classify::local_nets()?
                .into_iter()
                .map(|net| ak_discovery::ak_net_shim::Net { addr: net.addr, prefix_len: net.prefix_len })
                .collect();
            let found = mdns.browse(Duration::from_secs(seconds))?;
            let ranked = ak_discovery::rank(found, &nets);
            for candidate in ranked {
                println!(
                    "{}\t{}\t{:?}",
                    candidate.addr,
                    candidate.fingerprint.map(|f| f.to_hex()).unwrap_or_else(|| "-".into()),
                    candidate.source
                );
            }
            Ok(())
        }
        Command::SignalServer { tcp_port, stun_port } => {
            let config = ak_signal::server::Config {
                tcp: SocketAddr::from(([0, 0, 0, 0], tcp_port)),
                stun_udp: SocketAddr::from(([0, 0, 0, 0], stun_port)),
                ..ak_signal::server::Config::default()
            };
            println!("signaling on {} (STUN {}); no file routes exist", config.tcp, config.stun_udp);
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let server = tokio::spawn(async move { ak_signal::server::run(config, shutdown_rx).await });
            tokio::signal::ctrl_c().await.ok();
            let _ = shutdown_tx.send(true);
            server.await??;
            Ok(())
        }
        Command::Probe { server, peer, no_stun } => {
            probe_command(&id, server, peer, no_stun).await
        }
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "ak-device".into())
}

async fn probe_command(id: &Path, server: SocketAddr, peer: String, no_stun: bool) -> Result<()> {
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
    println!("device id: {}", hex_of(&key.public_key()));
    println!("fingerprint: {}", key.fingerprint().to_hex());

    // 1. Bind the transfer UDP socket first: every mapped/public candidate
    //    must belong to the socket we would actually transfer on.
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let local = socket.local_addr()?;
    println!("transfer socket: {local}");

    // 2. STUN mapping via the same socket (coordination plane, no data).
    let mut candidates: Vec<SocketAddr> = vec![local];
    if !no_stun {
        let stun_addr = SocketAddr::new(server.ip(), server.port() + 1);
        match tokio::time::timeout(Duration::from_secs(6), ak_signal::stun::query_on(&socket, stun_addr, Duration::from_secs(5))).await {
            Ok(Ok(mapped)) => {
                println!("STUN mapped address: {mapped}");
                candidates.push(mapped);
            }
            Ok(Err(error)) => println!("STUN query failed: {error}"),
            Err(_) => println!("STUN query timed out"),
        }
    }

    // 3. Signaling: register candidates and open a session toward the peer.
    let client = ak_signal::client::SignalClient::connect(server, &key, &whoami())
        .await
        .context("signaling connect failed")?;
    client.register(&candidates).await?;
    println!("registered {} candidates", candidates.len());
    client.connect_peer(target).await?;

    // 4. Exchange signals until we see the peer's candidates, then punch.
    let mut peer_candidates: Vec<SocketAddr> = Vec::new();
    let mut punched = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next()).await??;
        match message {
            ak_signal::codec::CtrlMessage::SessionOpened { session, peer } => {
                println!("session open with peer {}", hex_of(&peer));
                client
                    .send_signal(session, ak_signal::codec::PeerSignal::Candidates(candidates.clone()))
                    .await?;
            }
            ak_signal::codec::CtrlMessage::Signal { signal, .. } => match signal {
                ak_signal::codec::PeerSignal::Candidates(list) => {
                    peer_candidates = list;
                    println!("peer candidates: {peer_candidates:?}");
                    let report = ak_net::holepunch::probe(&socket, &peer_candidates, Duration::from_millis(200), Duration::from_secs(5)).await?;
                    println!(
                        "punch: sent {} recv {} reachable {:?} from {:?}",
                        report.packets_sent, report.packets_received, report.reachable, report.peer_observed_from
                    );
                    if !report.reachable.is_empty() {
                        punched = true;
                        // 5. Real verification: QUIC handshake with pinned identity.
                        let endpoint = ak_net::DirectEndpoint::bind(local, &bundle).await?;
                        match endpoint.connect(report.reachable[0], &Fingerprint::from_public_key(&target)).await {
                            Ok(channel) => {
                                println!("QUIC handshake OK on {:?} path={:?} peer={}", report.reachable[0], channel.path(), channel.peer_fingerprint().to_hex());
                            }
                            Err(error) => println!("QUIC handshake failed after punch: {error}"),
                        }
                        break;
                    }
                }
                other => println!("signal: {other:?}"),
            },
            other => println!("server: {other:?}"),
        }
    }
    if !punched && peer_candidates.is_empty() {
        println!("no peer candidates received; peer may be offline or behind an unreachable network");
    }
    let _ = client.close().await;
    println!("probe finished (direct-only: no relay fallback exists)");
    Ok(())
}

fn hex_to_bytes(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("odd-length hex string");
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0]).with_context(|| format!("invalid hex digit '{}'", pair[0] as char))?;
        let low = hex_digit(pair[1]).with_context(|| format!("invalid hex digit '{}'", pair[1] as char))?;
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
mod tests {
    use super::*;

    #[test]
    fn hex_helpers_round_trip() {
        assert_eq!(hex_to_bytes("00ff10").unwrap(), vec![0, 255, 16]);
        assert!(hex_to_bytes("0").is_err());
        assert!(hex_to_bytes("zz").is_err());
    }

    #[test]
    fn conflict_policy_parses_known_values() {
        assert_eq!(conflict_policy("error").unwrap(), ConflictPolicy::Fail);
        assert_eq!(conflict_policy("rename").unwrap(), ConflictPolicy::Rename);
        assert!(conflict_policy("overwrite").is_err());
    }

    #[test]
    fn fingerprint_parser_rejects_junk() {
        assert!(parse_fingerprint("not-a-fingerprint").is_err());
        assert!(parse_fingerprint(&"x".repeat(64)).is_err());
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.00 GiB");
    }
}
