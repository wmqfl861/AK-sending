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

use ak_identity::tls::TlsBundle;
use ak_identity::Fingerprint;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::DigitallySignedStruct;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};

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

/// Build the QUIC server config: our certificate, mandatory client
/// certificates verified as self-signed Ed25519, ALPN `ak/1`.
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
    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic)))
}

/// Build a QUIC client config presenting our certificate. With `pinned`, the
/// server certificate must match that fingerprint; without it the config
/// fails every handshake (see `UnpinnedServerCertVerifier`).
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
    Ok(quinn::ClientConfig::new(Arc::new(quic)))
}
