use crate::{Error, Result, ALPN};
use ring::digest::{digest, SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;

const MAX_DER_BYTES: usize = 16 * 1024;

/// Local secret-bearing identity. No Debug, serialization or implicit cloning.
pub struct LocalIdentity {
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl LocalIdentity {
    /// The caller supplies a locally owned certificate and PKCS#8 private key.
    /// Key/certificate agreement is validated when the endpoint is configured.
    pub fn from_der(certificate: Vec<u8>, pkcs8: Vec<u8>) -> Result<Self> {
        if certificate.is_empty()
            || certificate.len() > MAX_DER_BYTES
            || pkcs8.is_empty()
            || pkcs8.len() > MAX_DER_BYTES
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            certificate: CertificateDer::from(certificate),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)),
        })
    }
}

/// An explicitly paired, locally trusted full leaf certificate (not a CA list).
/// Constructing this value is a trust decision, never a discovery-message parser.
#[derive(Clone)]
pub struct PairedPeer {
    certificate: CertificateDer<'static>,
    id: [u8; 32],
}

impl PairedPeer {
    pub fn from_certificate_der(certificate: Vec<u8>) -> Result<Self> {
        if certificate.is_empty() || certificate.len() > MAX_DER_BYTES {
            return Err(Error::Configuration);
        }
        let mut id = [0; 32];
        id.copy_from_slice(digest(&SHA256, &certificate).as_ref());
        let certificate = CertificateDer::from(certificate);
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(certificate.clone())
            .map_err(|_| Error::Configuration)?;
        Ok(Self { certificate, id })
    }

    pub fn id(&self) -> [u8; 32] {
        self.id
    }

    /// Called only after rustls has verified the handshake signature and chain.
    pub(crate) fn matches(&self, chain: &[CertificateDer<'_>]) -> bool {
        chain.len() == 1 && chain[0].as_ref() == self.certificate.as_ref()
    }
}

pub(crate) fn tls_configs(
    local: &LocalIdentity,
    peer: &PairedPeer,
) -> Result<(rustls::ServerConfig, rustls::ClientConfig)> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(peer.certificate.clone())
        .map_err(|_| Error::Configuration)?;
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots.clone()),
        provider.clone(),
    )
    .build()
    .map_err(|_| Error::Configuration)?;
    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Error::Configuration)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![local.certificate.clone()], local.key.clone_key())
        .map_err(|_| Error::Configuration)?;
    server.alpn_protocols = vec![ALPN.to_vec()];
    server.max_early_data_size = 0;
    server.send_tls13_tickets = 0;
    server.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});

    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Error::Configuration)?
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![local.certificate.clone()], local.key.clone_key())
        .map_err(|_| Error::Configuration)?;
    client.alpn_protocols = vec![ALPN.to_vec()];
    client.enable_early_data = false;
    client.resumption = rustls::client::Resumption::disabled();
    Ok((server, client))
}

pub(crate) fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.max_concurrent_bidi_streams(1_u32.into());
    config.max_concurrent_uni_streams(0_u32.into());
    config.stream_receive_window(16_384_u32.into());
    config.receive_window(65_536_u32.into());
    config.send_window(65_536);
    config.crypto_buffer_size(16_384);
    config.datagram_receive_buffer_size(None);
    config.datagram_send_buffer_size(0);
    config.keep_alive_interval(None);
    config.max_idle_timeout(Some(quinn::VarInt::from_u32(15_000).into()));
    Arc::new(config)
}

pub(crate) fn quic_configs(
    local: &LocalIdentity,
    peer: &PairedPeer,
) -> Result<(quinn::ServerConfig, quinn::ClientConfig)> {
    let (server, client) = tls_configs(local, peer)?;
    let server = quinn::crypto::rustls::QuicServerConfig::try_from(server)
        .map_err(|_| Error::Configuration)?;
    let client = quinn::crypto::rustls::QuicClientConfig::try_from(client)
        .map_err(|_| Error::Configuration)?;
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(server));
    server.transport_config(transport_config());
    server.migration(false);
    server.max_incoming(4);
    server.incoming_buffer_size(16_384);
    server.incoming_buffer_size_total(65_536);
    let mut client = quinn::ClientConfig::new(Arc::new(client));
    client.transport_config(transport_config());
    Ok((server, client))
}
