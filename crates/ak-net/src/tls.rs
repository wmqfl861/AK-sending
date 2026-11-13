//! rustls/quinn configuration for the direct channel.
//!
//! Both sides present the self-signed Ed25519 certificate from `ak-identity`
//! and verify the peer's certificate themselves: the client pins the expected
//! device fingerprint, the server requires any valid self-signed Ed25519
//! certificate (the `authorize` callback then decides per fingerprint).
//! Everything here fails closed: no webpki roots, no hostname check, no
//! fallback verifier, TLS 1.3 only.
use std::io;
use std::sync::Arc;
use std::time::Duration;

use ak_identity::tls::TlsBundle;
use ak_identity::Fingerprint;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::DigitallySignedStruct;

use crate::ALPN;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn alpn_protocols() -> Vec<Vec<u8>> {
    ALPN.iter().map(|p| p.as_bytes().to_vec()).collect()
}

fn config_io_err(context: &str, err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {err}"))
}

fn cert_reject(reason: &str) -> rustls::Error {
    rustls::Error::General(format!("rejected peer certificate: {reason}"))
}

/// Verify a self-signed Ed25519 certificate via the canonical `ak-identity`
/// helper (self-signature checked there) and return its public key.
fn verify_self_signed(end_entity: &CertificateDer<'_>) -> Result<[u8; 32], rustls::Error> {
    ak_identity::tls::self_signed_public_key(end_entity.as_ref())
        .map_err(|why| cert_reject(&why.to_string()))
}

/// Shared signature-verification body for all verifiers below: this is what
/// proves the peer controls the private key belonging to the certificate
/// (TLS 1.3 CertificateVerify), using the ring-backed algorithms.
macro_rules! delegate_signature_verification {
    () => {
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, self.provider())
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, self.provider())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider().supported_schemes()
        }
    };
}

/// Client-side verifier: the server certificate must be self-signed Ed25519
/// whose key fingerprint equals the pinned device fingerprint.
#[derive(Debug)]
struct PinnedServerCertVerifier {
    pinned: Fingerprint,
    schemes: WebPkiSupportedAlgorithms,
}

impl PinnedServerCertVerifier {
    fn provider(&self) -> &WebPkiSupportedAlgorithms {
        &self.schemes
    }
}

impl rustls::client::danger::ServerCertVerifier for PinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let public_key = verify_self_signed(end_entity)?;
        let fingerprint = Fingerprint::from_public_key(&public_key);
        if fingerprint != self.pinned {
            return Err(rustls::Error::General(
                "peer certificate failed pinning: fingerprint mismatch".into(),
            ));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    delegate_signature_verification!();
}

/// Client-side placeholder used for the endpoint's *default* client config.
/// It has no pin, so it rejects every server: real connections must go
/// through [`crate::DirectEndpoint::connect`], which builds a per-peer
/// pinned config. This keeps `Endpoint::connect` unusable as a bypass.
#[derive(Debug)]
struct UnpinnedServerCertVerifier {
    schemes: WebPkiSupportedAlgorithms,
}

impl UnpinnedServerCertVerifier {
    fn provider(&self) -> &WebPkiSupportedAlgorithms {
        &self.schemes
    }
}

impl rustls::client::danger::ServerCertVerifier for UnpinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Err(rustls::Error::General(
            "no pinned fingerprint configured for this connection".into(),
        ))
    }

    delegate_signature_verification!();
}

/// Server-side verifier: clients must present a valid self-signed Ed25519
/// certificate. The per-peer decision happens later in
/// [`crate::DirectEndpoint::accept`] via the `authorize` callback.
#[derive(Debug)]
struct SelfSignedClientCertVerifier {
    schemes: WebPkiSupportedAlgorithms,
}

impl SelfSignedClientCertVerifier {
    fn provider(&self) -> &WebPkiSupportedAlgorithms {
        &self.schemes
    }
}

impl rustls::server::danger::ClientCertVerifier for SelfSignedClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        verify_self_signed(end_entity)?;
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    delegate_signature_verification!();
}

fn private_key(bundle: &TlsBundle) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from(PrivatePkcs8KeyDer::from(bundle.key_pkcs8_der.clone()))
}

/// INV-004 policy flag: this stack disables QUIC active migration on every
/// endpoint it builds. Locked to `true`; `quinn_server_config` feeds it into
/// the server config and `DirectEndpoint::active_migration_disabled` reports
/// it back as a public read-only getter.
///
/// API note (verified against the locked registry sources, quinn 0.11.12 +
/// quinn-proto 0.11.19): the older `TransportConfig::disable_active_migration`
/// public field no longer exists in this version. The replacement is
/// `quinn::ServerConfig::migration(false)`, which:
///
/// 1. makes the server advertise the RFC 9000 `disable_active_migration`
///    transport parameter in the handshake (quinn-proto `transport_parameters`
///    builds the parameter from `!server_config.migration`), so a compliant
///    peer MUST NOT actively migrate, and
/// 2. makes the transport itself discard migration-initiating datagrams from
///    unrecognized remote addresses (quinn-proto `connection` drops them when
///    `!side.remote_may_migrate()`), so a non-compliant peer's migration
///    attempt never rebinds the connection.
///
/// There is no client-side knob in this quinn version: a quinn client never
/// initiates active migration, and the server-advertised parameter binds any
/// compliant client. Defense in depth for a live connection is
/// `DirectChannel::ensure_path_stable`, which watches
/// `Connection::remote_address()` and closes defensively on change.
pub(crate) const MIGRATION_DISABLED: bool = true;

// ---------------------------------------------------------------------------
// CR-P2-04: explicit QUIC transport tuning — one constant table, both roles
// ---------------------------------------------------------------------------
//
// quinn-proto 0.11.19 defaults (`src/config/transport.rs`,
// `impl Default for TransportConfig`) are: `max_idle_timeout = 30 s`
// (RFC 9308 § 3.2), `keep_alive_interval = None`, per-stream receive window
// 1.25 MB (sized for 12.5 MB/s at 100 ms RTT), connection `receive_window =
// VarInt::MAX`, `send_window = 10 MB`. The 30 s idle default collides exactly
// with the 30 s accept-confirmation window in ak-ffi (`task.rs`) and ak-cli
// (`task_cli.rs`): while the receiving user is still deciding, no packet
// flows, and the transport dies at the same moment the confirmation window
// closes — the sender's 120 s idle budget is unreachable and the window
// length cannot be raised (finding E2) until the connection outlives it.
// Every constant below exists to break that collision; the window length
// itself stays an owner decision (review § 8, E2).

/// CR-P2-04: while established, send a QUIC PING whenever no packet has been
/// *received* for this long. PINGs are ack-eliciting and quinn resets the
/// idle timer on every authenticated packet received (quinn-proto 0.11.19
/// `on_packet_authenticated` → `reset_idle_timeout`), so each probe refreshes
/// the peer's timer and the returning ACK refreshes ours — a fully idle
/// connection (no streams, no data, both sides silent) stays up indefinitely
/// instead of counting down to [`MAX_IDLE_TIMEOUT`]. 10 s fits five retries
/// into the 60 s idle budget (five consecutive probe losses tolerated) while
/// costing two PING datagrams plus their ACKs every 10 s — a few dozen bytes
/// per second on an otherwise dead-silent link.
pub(crate) const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// CR-P2-04: maximum inactivity before the transport gives up on the
/// connection (quinn default: 30 s). 60 s doubles the headroom over the 30 s
/// accept-confirmation flow and, with [`KEEP_ALIVE_INTERVAL`], is never
/// reached in practice — it is the fallback for a peer that stopped
/// responding entirely. Negotiation note (RFC 9000 § 10.1): the effective
/// idle timeout is `min` of the two endpoints' advertised values
/// (quinn-proto `negotiate_max_idle_timeout`), so BOTH roles must advertise
/// 60 s — which is why [`transport_config`] is the one table fed into both
/// [`quinn_server_config`] and [`quinn_client_config`]; a default-configured
/// peer would cap the negotiation at 30 s.
pub(crate) const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// CR-P2-04: per-stream flow-control receive window in bytes (quinn default:
/// 1.25 MB). Bandwidth-delay-product sizing: a stream can only have this many
/// bytes in flight, so at 100 ms RTT the default caps one stream at
/// 12.5 MB/s (~100 Mbps; ~25 MB/s at 50 ms) — the review's measured complaint
/// for single-stream public-internet transfers. 8 MiB / 100 ms ≈ 84 MB/s
/// (~670 Mbps) with headroom over the 100–500 Mbps WAN paths this app targets.
pub(crate) const STREAM_RECEIVE_WINDOW_BYTES: u32 = 8 * 1024 * 1024;

/// CR-P2-04: connection-wide flow-control receive window in bytes (quinn
/// default: `VarInt::MAX`). Explicitly bounded instead: it is the hard cap on
/// how much data a peer may park in our buffers across all streams, so the
/// default is a memory-exhaustion gift to a malicious or buggy peer once
/// [`STREAM_RECEIVE_WINDOW_BYTES`] is raised. Equal to the per-stream window
/// on purpose — this app opens 1–2 streams per transfer, so one transfer may
/// use the whole connection window; `receive_window ≥ stream_receive_window`
/// keeps multi-stream traffic unstarved.
pub(crate) const RECEIVE_WINDOW_BYTES: u32 = 8 * 1024 * 1024;

/// CR-P2-04: connection-wide send window in bytes — the cap on unacknowledged
/// application writes we retain for retransmission (quinn default: 10 MB).
/// Kept comparable to the receive windows (8 MiB) so a fast sender on a fast
/// path is not throttled below the receiver's advertised window. Worst-case
/// transport memory per connection is thereby bounded at
/// [`RECEIVE_WINDOW_BYTES`] + [`SEND_WINDOW_BYTES`] = 16 MiB.
pub(crate) const SEND_WINDOW_BYTES: u32 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Experimental window override (2026-11-13 review §5.2 follow-up): the netem
// benchmark's default-window A/B arm needs to run the SAME product config
// path with quinn's 1.25 MiB default windows instead of the CR-P2-04 table.
// CR-P3-16 (review 2026-11-13-batchH §4): the whole override exists ONLY in
// `exp-knobs` builds (ak-net's cargo feature, default off — release builds
// must not enable it); a default build's [`exp_window_override_bytes`] is
// the constant `None` (bit-identical to the env-unset branch) and the
// variable's NAME is not compiled in at all (release `strings` stay clean).
// ---------------------------------------------------------------------------

/// Environment variable of the hidden debug window override (precedent:
/// `AK_EXP_NO_TTL` / `AK_EXP_SEND_DELAY_MS` — default off, never set in
/// production). See [`exp_window_override_bytes`].
#[cfg(feature = "exp-knobs")]
const EXP_QUIC_WINDOW_KIB_ENV: &str = "AK_EXP_QUIC_WINDOW_KIB";

/// Largest accepted override in KiB: `kib * 1024` must still fit the `u32`
/// window API (`u32::MAX / 1024` = 4194303, far above any sane window).
#[cfg(any(test, feature = "exp-knobs"))]
const EXP_WINDOW_KIB_MAX: u64 = (u32::MAX as u64) / 1024;

/// Last override state announced on stderr: `0` = constant table (default,
/// also the initial state so production never prints anything),
/// `1..=EXP_WINDOW_KIB_MAX` = that many KiB, `u64::MAX` = last value was
/// invalid. Swapped (not loaded+stored) so exactly one thread prints per
/// state change even when endpoint builds race.
#[cfg(feature = "exp-knobs")]
static LAST_ANNOUNCED_WINDOW_KIB: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Pure resolver behind [`exp_window_override_bytes`]: one optional raw
/// environment value into `(announcement state, window bytes)`. `None` and
/// every invalid value (non-numeric, 0, above [`EXP_WINDOW_KIB_MAX`]) keep
/// the CR-P2-04 constant table; only a plain decimal KiB count overrides.
/// Unit-testable without touching process-global environment state.
#[cfg(any(test, feature = "exp-knobs"))]
fn resolve_exp_window(raw: Option<&str>) -> (u64, Option<u32>) {
    match raw.map(str::parse::<u64>) {
        None => (0, None),
        Some(Ok(kib)) if (1..=EXP_WINDOW_KIB_MAX).contains(&kib) => {
            (kib, Some((kib * 1024) as u32))
        }
        Some(_) => (u64::MAX, None),
    }
}

/// CR-P2-04 follow-up (2026-11-13 review §5.2): hidden debug parameter,
/// DEFAULT OFF — never set in production. `AK_EXP_QUIC_WINDOW_KIB=<kib>`
/// overrides ALL THREE flow-control windows of the CR-P2-04 constant table
/// ([`STREAM_RECEIVE_WINDOW_BYTES`], [`RECEIVE_WINDOW_BYTES`],
/// [`SEND_WINDOW_BYTES`]) with one KiB value, so the netem benchmark can
/// run its default-window A/B arm (1280 KiB = quinn's 1.25 MiB defaults)
/// through the same product [`transport_config`] path instead of a
/// bench-private transport config.
///
/// Read per [`transport_config`] call — every endpoint build re-resolves
/// it (the bench flips the variable between passes and each pass builds
/// fresh endpoints and connections; nothing caches across connections).
/// UNSET (the production default): the constant table applies bit for bit,
/// zero behavior change, zero output. Set-and-valid: one stderr line per
/// state change announces the experimental override. Set-but-invalid: one
/// stderr line announces the refusal — a mistyped benchmark arm must never
/// silently measure the default windows.
///
/// CR-P3-16: the environment read compiles ONLY under `exp-knobs` (the
/// netem benchmark's build passes the feature); a default build returns
/// `None` — bit-identical to the UNSET branch, and no `AK_EXP_*` string
/// exists in the binary for a stray environment variable to match.
fn exp_window_override_bytes() -> Option<u32> {
    #[cfg(feature = "exp-knobs")]
    {
        let raw = std::env::var(EXP_QUIC_WINDOW_KIB_ENV).ok();
        let (state, bytes) = resolve_exp_window(raw.as_deref());
        if LAST_ANNOUNCED_WINDOW_KIB.swap(state, std::sync::atomic::Ordering::Relaxed) != state {
            match (&raw, bytes) {
                (None, _) => eprintln!(
                    "[ak-net] {EXP_QUIC_WINDOW_KIB_ENV} cleared - CR-P2-04 constant flow-control windows restored"
                ),
                (Some(text), Some(bytes)) => eprintln!(
                    "[ak-net] EXPERIMENTAL {EXP_QUIC_WINDOW_KIB_ENV}={text}: overriding all three CR-P2-04 flow-control windows with {bytes} bytes ({text} KiB) - hidden debug knob, never a production setting"
                ),
                (Some(text), None) => eprintln!(
                    "[ak-net] EXPERIMENTAL {EXP_QUIC_WINDOW_KIB_ENV}='{text}' is not a usable window (want an integer 1..={EXP_WINDOW_KIB_MAX}, in KiB) - keeping the CR-P2-04 constant windows"
                ),
            }
        }
        bytes
    }
    #[cfg(not(feature = "exp-knobs"))]
    {
        None
    }
}

/// Build the [`quinn::TransportConfig`] shared by every server and client
/// config this module produces, from the CR-P2-04 constant table above.
///
/// Everything not in the table stays at the quinn default (Cubic congestion
/// control, MTU discovery, 100 concurrent bidi/uni streams, 16 KiB crypto
/// buffer): no field data justifies touching those yet — the netem throughput
/// benchmark proposed in the review's fix plan is the follow-up that would.
///
/// Window constants are `u32` so `VarInt::from_u32` is infallible here; the
/// QUIC VarInt ceiling (2^62-1) is never a constraint for windows this small.
/// The one runtime deviation from the constant table is the hidden
/// [`exp_window_override_bytes`] A/B knob — unset (production), the table
/// below applies bit for bit.
pub(crate) fn transport_config() -> quinn::TransportConfig {
    // Same value for all three windows keeps the table's invariant
    // (`receive_window == stream_receive_window`, send window comparable)
    // under the override too.
    let (stream_window, conn_window, send_window) = match exp_window_override_bytes() {
        Some(bytes) => (bytes, bytes, u64::from(bytes)),
        None => (
            STREAM_RECEIVE_WINDOW_BYTES,
            RECEIVE_WINDOW_BYTES,
            u64::from(SEND_WINDOW_BYTES),
        ),
    };
    let mut config = quinn::TransportConfig::default();
    config
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL))
        .max_idle_timeout(Some(
            quinn::IdleTimeout::try_from(MAX_IDLE_TIMEOUT)
                .expect("60 s idle timeout fits the QUIC VarInt bound"),
        ))
        .stream_receive_window(quinn::VarInt::from_u32(stream_window))
        .receive_window(quinn::VarInt::from_u32(conn_window))
        .send_window(send_window);
    config
}

/// Build the QUIC server config: our certificate, mandatory client
/// certificates verified as self-signed Ed25519, ALPN `ak/1`, and QUIC active
/// migration disabled at the transport-parameter level (INV-004; see
/// [`MIGRATION_DISABLED`]). CR-P2-04: carries the shared transport tuning
/// table via [`transport_config`].
pub(crate) fn quinn_server_config(identity: &TlsBundle) -> io::Result<quinn::ServerConfig> {
    let verifier = Arc::new(SelfSignedClientCertVerifier {
        schemes: provider().signature_verification_algorithms,
    });
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| config_io_err("selecting TLS 1.3", e))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![CertificateDer::from(identity.cert_der.clone())],
            private_key(identity),
        )
        .map_err(|e| config_io_err("server certificate or key rejected", e))?;
    config.alpn_protocols = alpn_protocols();
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(config))
        .map_err(|e| config_io_err("building QUIC server config", e))?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic));
    // INV-004: reject connection migration at the handshake-parameter level.
    server_config.migration(!MIGRATION_DISABLED);
    // CR-P2-04: explicit transport tuning (idle/keep-alive/flow-control).
    server_config.transport_config(Arc::new(transport_config()));
    Ok(server_config)
}

/// Build a QUIC client config presenting our certificate. With `pinned`, the
/// server certificate must match that fingerprint; without it the config
/// fails every handshake (see `UnpinnedServerCertVerifier`).
///
/// Migration note (INV-004): the client side has no `disable_active_migration`
/// transport-config knob in quinn 0.11.12 (see [`MIGRATION_DISABLED`]); a
/// quinn client does not actively migrate, and every endpoint built here also
/// runs as a server with `migration(false)`, whose advertised
/// `disable_active_migration` parameter binds this client. `DirectChannel`
/// still watches `remote_address()` defensively
/// (`DirectChannel::ensure_path_stable`).
///
/// CR-P2-04: carries the same transport tuning table as the server config
/// (via [`transport_config`]) — the negotiated idle timeout is the minimum of
/// both endpoints' advertised values, so the client role must advertise it
/// too or the server-side 60 s is silently cut back down.
pub(crate) fn quinn_client_config(
    identity: &TlsBundle,
    pinned: Option<&Fingerprint>,
) -> io::Result<quinn::ClientConfig> {
    let schemes = provider().signature_verification_algorithms;
    let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match pinned {
        Some(fingerprint) => Arc::new(PinnedServerCertVerifier {
            pinned: *fingerprint,
            schemes,
        }),
        None => Arc::new(UnpinnedServerCertVerifier { schemes }),
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| config_io_err("selecting TLS 1.3", e))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(
            vec![CertificateDer::from(identity.cert_der.clone())],
            private_key(identity),
        )
        .map_err(|e| config_io_err("client certificate or key rejected", e))?;
    config.alpn_protocols = alpn_protocols();
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(config))
        .map_err(|e| config_io_err("building QUIC client config", e))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic));
    // CR-P2-04: explicit transport tuning (idle/keep-alive/flow-control).
    client_config.transport_config(Arc::new(transport_config()));
    Ok(client_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure resolver only (no environment mutation: ak-net's unit tests run
    // multi-threaded in one process, so a set_var here could race the
    // transport-config paths of parallel tests; the live env plumbing is
    // exercised end-to-end by the bench).
    #[test]
    fn exp_window_resolver_bounds() {
        // Unset (production): constant table, state 0, never announced.
        assert_eq!(resolve_exp_window(None), (0, None));
        // The A/B arm's value: 1280 KiB = quinn's 1.25 MiB default windows.
        assert_eq!(resolve_exp_window(Some("1280")), (1280, Some(1_310_720)));
        // Leading zeros/whitespace are not accepted (str::parse is exact).
        assert_eq!(resolve_exp_window(Some(" 1280")), (u64::MAX, None));
        // 0, negatives, non-numeric, overflow: refused loudly, table kept.
        assert_eq!(resolve_exp_window(Some("0")), (u64::MAX, None));
        assert_eq!(resolve_exp_window(Some("-5")), (u64::MAX, None));
        assert_eq!(resolve_exp_window(Some("8MiB")), (u64::MAX, None));
        assert_eq!(
            resolve_exp_window(Some("99999999999999999999")),
            (u64::MAX, None)
        );
        // The largest accepted value still fits the u32 window API.
        let max_kib = EXP_WINDOW_KIB_MAX;
        assert_eq!(
            resolve_exp_window(Some(&max_kib.to_string())),
            (max_kib, Some(u32::MAX - 1023))
        );
        assert_eq!(
            resolve_exp_window(Some(&(max_kib + 1).to_string())),
            (u64::MAX, None)
        );
    }
}
