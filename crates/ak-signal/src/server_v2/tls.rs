//! TLS 1.3 configuration for the v2 control plane, following the ak-net
//! discipline (`crates/ak-net/src/tls.rs`): the registry presents a
//! self-signed Ed25519 certificate whose SubjectPublicKeyInfo **is** the
//! 32-byte [`RegistryId`] (the registry key's public half, built via
//! `ak_identity::tls::make_self_signed` — rcgen under the hood), and
//! clients pin that key directly. Fail closed: no webpki roots, no
//! hostname check, no fallback verifier, TLS 1.3 only.
//!
//! Client authentication is **not** done with certificates: the client
//! authenticates by signing Hello v2 over the TLS exporter of this very
//! connection ([`super::hello2`]), which binds the device key to the
//! channel. The server therefore requires no client certificate.
//!
//! ALPN: both sides speak exactly `ak-signal/2`; a mismatched ALPN (or
//! none at all) is refused by [`check_alpn`].

use std::io;
use std::sync::Arc;

use ak_identity::tls::TlsBundle;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::version::TLS13;
use rustls::ServerConfig as RustlsServerConfig;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::RegistryId;

/// The single ALPN protocol of the v2 control plane.
pub const ALPN_V2: &[u8] = b"ak-signal/2";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn config_io_err(context: &str, err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {err}"))
}

fn alpn_protocols() -> Vec<Vec<u8>> {
    vec![ALPN_V2.to_vec()]
}

fn cert_reject(reason: &str) -> rustls::Error {
    rustls::Error::General(format!("rejected registry certificate: {reason}"))
}

/// Verify a self-signed Ed25519 certificate and return its embedded
/// public key (`ak-identity` checks the certificate's self-signature).
fn verify_self_signed(end_entity: &CertificateDer<'_>) -> Result<[u8; 32], rustls::Error> {
    ak_identity::tls::self_signed_public_key(end_entity.as_ref())
        .map_err(|why| cert_reject(&why.to_string()))
}

/// Client-side verifier: the server certificate must be self-signed
/// Ed25519 whose public key equals the pinned [`RegistryId`] byte for
/// byte. No unknown pin is ever accepted (there is no unpinned mode —
/// that is the discipline that makes the pin meaningful).
#[derive(Debug)]
struct PinnedRegistryVerifier {
    pinned: RegistryId,
    schemes: WebPkiSupportedAlgorithms,
}

impl PinnedRegistryVerifier {
    fn provider(&self) -> &WebPkiSupportedAlgorithms {
        &self.schemes
    }
}

impl rustls::client::danger::ServerCertVerifier for PinnedRegistryVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let public_key = verify_self_signed(end_entity)?;
        if public_key != self.pinned {
            return Err(rustls::Error::General(
                "registry certificate failed pinning: key mismatch".into(),
            ));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, self.provider())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, self.provider())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider().supported_schemes()
    }
}

fn private_key(bundle: &TlsBundle) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from(PrivatePkcs8KeyDer::from(bundle.key_pkcs8_der.clone()))
}

/// Server config for the registry listener: the self-signed registry
/// certificate, TLS 1.3 only, no client certificates (Hello v2 carries
/// client authentication), ALPN `ak-signal/2`.
pub fn server_config(bundle: &TlsBundle) -> io::Result<TlsAcceptor> {
    let config = RustlsServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(|e| config_io_err("selecting TLS 1.3", e))?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(bundle.cert_der.clone())],
            private_key(bundle),
        )
        .map_err(|e| config_io_err("registry certificate or key rejected", e))?;
    let mut config = config;
    config.alpn_protocols = alpn_protocols();
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Client config pinning one registry key. There is deliberately no
/// unpinned variant: an unknown pin is a failed handshake, never a
/// silently accepted one.
pub fn client_config(pinned_registry: &RegistryId) -> io::Result<TlsConnector> {
    let verifier = Arc::new(PinnedRegistryVerifier {
        pinned: *pinned_registry,
        schemes: provider().signature_verification_algorithms,
    });
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(|e| config_io_err("selecting TLS 1.3", e))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let mut config = config;
    config.alpn_protocols = alpn_protocols();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// The negotiated ALPN must be exactly `ak-signal/2`; anything else
/// (including none) fails the connection. Used on the server side after
/// `accept`; the rustls client side fails mismatches on its own.
pub fn check_alpn(negotiated: Option<&[u8]>) -> Result<(), io::Error> {
    match negotiated {
        Some(proto) if proto == ALPN_V2 => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "connection did not negotiate ALPN ak-signal/2",
        )),
    }
}

/// Dial one v2 control connection to `addr`, pinning `registry`.
///
/// Convenience for tests and A3's dispatcher: builds the pinned config,
/// connects, and completes the TLS 1.3 handshake. Hello v2 (the
/// application-layer authentication) is the caller's next step — see
/// [`super::hello2`].
pub async fn connect_pinned(
    addr: std::net::SocketAddr,
    registry: &RegistryId,
) -> io::Result<ClientTlsStream<TcpStream>> {
    let tcp = TcpStream::connect(addr).await?;
    tcp.set_nodelay(true)?;
    let connector = client_config(registry)?;
    let server_name = ServerName::IpAddress(addr.ip().into());
    connector.connect(server_name, tcp).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::DeviceKeyPair;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn registry_bundle() -> (DeviceKeyPair, TlsBundle) {
        let key = DeviceKeyPair::generate();
        let bundle = ak_identity::tls::make_self_signed(&key);
        (key, bundle)
    }

    #[tokio::test]
    async fn handshake_succeeds_with_the_right_pin_and_fails_with_a_wrong_one() {
        let (key, bundle) = registry_bundle();
        let registry_id = key.public_key();
        let acceptor = server_config(&bundle).unwrap();

        // Right pin: handshake completes and ALPN negotiates.
        let connector = client_config(&registry_id).unwrap();
        let (client_tcp, server_tcp) = tokio::io::duplex(4096);
        let server_acceptor = acceptor.clone();
        let server = tokio::spawn(async move { server_acceptor.accept(server_tcp).await });
        let client = connector.connect(ServerName::try_from("localhost").unwrap(), client_tcp);
        let (client_stream, server_stream) =
            tokio::try_join!(client, async { server.await.unwrap() })
                .expect("handshake with the right pin");
        assert_eq!(
            client_stream.get_ref().1.alpn_protocol(),
            Some(ALPN_V2),
            "client negotiated v2 ALPN"
        );
        check_alpn(server_stream.get_ref().1.alpn_protocol()).unwrap();

        // Wrong pin: the handshake fails on the client side.
        let wrong = DeviceKeyPair::generate().public_key();
        let connector = client_config(&wrong).unwrap();
        let (client_tcp, server_tcp) = tokio::io::duplex(4096);
        let server_acceptor = acceptor.clone();
        let server = tokio::spawn(async move { server_acceptor.accept(server_tcp).await });
        let client = connector.connect(ServerName::try_from("localhost").unwrap(), client_tcp);
        let outcome = tokio::try_join!(client, async { server.await.unwrap() });
        assert!(outcome.is_err(), "wrong pin must fail the handshake");
    }

    #[tokio::test]
    async fn both_sides_derive_the_same_tls_exporter() {
        let (key, bundle) = registry_bundle();
        let acceptor = server_config(&bundle).unwrap();
        let connector = client_config(&key.public_key()).unwrap();
        let (client_tcp, server_tcp) = tokio::io::duplex(4096);
        let server_acceptor = acceptor.clone();
        let server = tokio::spawn(async move { server_acceptor.accept(server_tcp).await });
        let client = connector.connect(ServerName::try_from("localhost").unwrap(), client_tcp);
        let (client_stream, server_stream) =
            tokio::try_join!(client, async { server.await.unwrap() }).unwrap();

        let client_exporter =
            super::super::hello2::exporter_secret_client(client_stream.get_ref().1).unwrap();
        let server_exporter =
            super::super::hello2::exporter_secret_server(server_stream.get_ref().1).unwrap();
        assert_eq!(client_exporter, server_exporter);
        // And another connection derives a different exporter, so Hello
        // signatures are channel-bound.
        let (client2_tcp, server2_tcp) = tokio::io::duplex(4096);
        let server2_acceptor = acceptor.clone();
        let server2 = tokio::spawn(async move { server2_acceptor.accept(server2_tcp).await });
        let client2 = connector.connect(ServerName::try_from("localhost").unwrap(), client2_tcp);
        let (client2_stream, _server2_stream) =
            tokio::try_join!(client2, async { server2.await.unwrap() }).unwrap();
        let client2_exporter =
            super::super::hello2::exporter_secret_client(client2_stream.get_ref().1).unwrap();
        assert_ne!(client_exporter, client2_exporter);
    }

    #[tokio::test]
    async fn plaintext_is_not_tls_and_alpn_check_refuses_none() {
        // ALPN gate: no negotiation is not acceptable.
        assert!(check_alpn(None).is_err());
        assert!(check_alpn(Some(b"http/1.1")).is_err());
        assert!(check_alpn(Some(ALPN_V2)).is_ok());

        // A plaintext client sending garbage bytes fails the TLS accept.
        let (_key, bundle) = registry_bundle();
        let acceptor = server_config(&bundle).unwrap();
        let (mut plain, server_tcp) = tokio::io::duplex(64);
        plain.write_all(b"not a tls record").await.unwrap();
        let outcome = acceptor.accept(server_tcp).await;
        assert!(outcome.is_err());
        let _ = plain.read(&mut [0u8; 8]).await;
    }
}
