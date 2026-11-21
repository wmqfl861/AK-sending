//! `discover`, `invite`, receiving after pairing, and the registry and STUN
//! servers.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

pub(super) async fn discover_command(id: &Path, seconds: u64, json: bool) -> Result<()> {
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
pub(super) fn discover_json(candidates: &[ak_discovery::PeerCandidate], book: &PeerBook) -> String {
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
pub(super) async fn invite_command(id: &Path, cmd: InviteCommand) -> Result<()> {
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
    let addr = bound_address(addr_rx, &mut receiver).await?;
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
pub(super) fn registry_db_path(id: &Path) -> PathBuf {
    id.join("registry.db")
}

/// Build the server_v2 config from the flags (pure; unit-tested).
pub(super) fn registry_server_config(
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
pub(super) fn registry_pin(registry: &DeviceKeyPair) -> String {
    hex_of(&registry.public_key())
}

/// The pin of an EXISTING `<id-dir>/registry.key`, never creating one (see
/// [`DeviceKeyPair::load_existing`]).
pub(super) fn registry_id_of(id: &Path) -> Result<String> {
    let key_path = id.join("registry.key");
    let registry = DeviceKeyPair::load_existing(&key_path)
        .with_context(|| format!("no usable registry identity at {}", key_path.display()))?;
    Ok(registry_pin(&registry))
}

/// `ak-cli registry-id`: print the pin of the existing registry identity as
/// ONE line holding exactly 64 lowercase hex characters (machine-readable:
/// the deploy script compares it before switching services over).
pub(super) fn registry_id_command(id: &Path) -> Result<()> {
    let pin = registry_id_of(id)?;
    println!("{pin}");
    Ok(())
}

/// `ak-cli registry-server`: load/create `<id-dir>/registry.key`, bind the
/// listener and open the store, then print the bound address and the
/// registry pin, and serve until Ctrl-C (a failed bind, store or serve
/// loop ends the command with its error). TLS-only: no plaintext invite
/// service exists to fall back to.
pub(super) async fn registry_server_command(
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
pub(super) async fn stun_server_command(port: Option<u16>, bind: Option<BindArg>) -> Result<()> {
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
