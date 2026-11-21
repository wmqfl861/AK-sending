//! `receive`: listen addresses and candidates, the receive loop, and the
//! send-root checks shared with `send`.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

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
pub(super) async fn receive_command(
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
        let mut receiver =
            tokio::spawn(
                async move { ak_engine::receive_with_addr(spec, &*printer, addr_tx).await },
            );
        if first {
            let addr = bound_address(addr_rx, &mut receiver).await?;
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

/// Wait for a spawned engine receive to announce its bound address. A
/// receive that fails before it binds (missing output directory, port in
/// use, ...) only drops the address channel, so its own error is taken from
/// the task: the bare "channel closed" used to hide the cause (batch S
/// review; S1 met it with a missing `--out` directory).
pub(crate) async fn bound_address(
    addr_rx: tokio::sync::oneshot::Receiver<SocketAddr>,
    receiver: &mut tokio::task::JoinHandle<Result<Report, EngineError>>,
) -> Result<SocketAddr> {
    match addr_rx.await {
        Ok(addr) => Ok(addr),
        Err(_) => match receiver.await {
            Ok(Err(error)) => Err(anyhow::Error::new(error).context("receiver failed to start")),
            Ok(Ok(_)) => bail!("receiver finished before it announced an address"),
            Err(join) => bail!("receiver task failed: {join}"),
        },
    }
}

/// Canonicalize the send paths: the first is the primary root, the rest
/// become extra roots in the order given (the engine enforces unique root
/// names across the whole job).
pub(super) fn send_roots(paths: &[PathBuf]) -> Result<(PathBuf, Vec<PathBuf>)> {
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
pub(super) fn duplicate_roots_error(err: &EngineError) -> Option<&std::io::Error> {
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
