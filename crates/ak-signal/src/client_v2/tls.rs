//! TLS and Hello-v2 signing material for the v2 control-plane client.
//!
//! Discipline mirrors `ak-net::tls` (ADR-001/ADR-002): self-signed Ed25519
//! certificate verified by key pin, no webpki roots, no hostname check, no
//! fallback verifier, TLS 1.3 only, ring provider. On top of the transport
//! pin the client cross-checks that the `AuthChallenge.registry_id` equals
//! the pinned registry public key, so both the TLS identity and the
//! protocol-level registry binding must agree.
//!
//! Wave-22 constant alignment (A2/A3 reconciliation, server_v2 is
//! authoritative): the ALPN, exporter label and pin semantics match
//! `ak_signal::server_v2::{tls, hello2}` exactly, so `ClientV2::connect`
//! interoperates with `serve_v2` byte for byte.
//!
//! Hello-v2 signature domain (frozen by `docs/contracts/pairing-flow.md` §3):
//!
//! ```text
//! "AKHELLO2" || registry_id[32] || TLS_exporter[32]
//!           || version:u16 || device_id[32]
//!           || name_len:u16 || name || timestamp_ms:u64 || challenge[32]
//! ```

use std::io;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::DigitallySignedStruct;

use super::error::ClientV2Error;
use crate::protocol_v2::{DeviceId, Nonce32, RegistryId};

/// ALPN protocol identifier for the v2 control plane. The server must offer
/// exactly this value; aligned with `server_v2::tls::ALPN_V2` (wave 22,
/// server is authoritative).
pub const CTRL_ALPN: &[u8] = b"ak-signal/2";

/// TLS exporter label bound into the Hello-v2 signature. Aligned with
/// `server_v2::hello2::EXPORTER_LABEL` (wave 22, server is authoritative):
/// both sides must derive the exporter with this label.
pub const HELLO_EXPORTER_LABEL: &[u8] = b"AK control hello exporter";

/// Exported keying material length folded into the Hello signature.
pub const HELLO_EXPORTER_LEN: usize = 32;

/// Signature domain separator, frozen by the pairing-flow contract.
const HELLO_DOMAIN: &[u8] = b"AKHELLO2";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn cert_reject(reason: &str) -> rustls::Error {
    rustls::Error::General(format!("rejected registry certificate: {reason}"))
}

/// Client-side verifier: the server (registry) certificate must be a valid
/// self-signed Ed25519 certificate whose raw 32-byte public key equals the
/// pinned [`RegistryId`] byte for byte — `server_v2::tls` semantics (the
/// certificate key **is** the registry id). There is no
/// accept-unknown-pin path and no downgrade to plaintext.
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

impl ServerCertVerifier for PinnedRegistryVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let public_key = ak_identity::tls::self_signed_public_key(end_entity.as_ref())
            .map_err(|why| cert_reject(&why.to_string()))?;
        if public_key != self.pinned {
            return Err(cert_reject(
                "certificate key does not match the pinned registry id",
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, self.provider())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, self.provider())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider().supported_schemes()
    }
}

/// Build the pinned rustls client config for one registry id (the registry
/// key's raw 32-byte public half).
///
/// TLS 1.3 only, ring provider, ALPN [`CTRL_ALPN`], custom verifier that
/// accepts exactly the pinned self-signed Ed25519 certificate (its
/// SubjectPublicKeyInfo **is** the [`RegistryId`] — `server_v2::tls`
/// semantics). The client presents no certificate: device authentication is
/// the signed Hello (challenge-bound), not TLS client auth.
pub fn client_config(pinned: &RegistryId) -> io::Result<rustls::ClientConfig> {
    let verifier = PinnedRegistryVerifier {
        pinned: *pinned,
        schemes: provider().signature_verification_algorithms,
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("TLS 1.3: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![CTRL_ALPN.to_vec()];
    Ok(config)
}

/// Validate the Hello device name against the wire rules (≤64 bytes, no
/// C0/DEL/C1 control characters) before anything is signed or sent.
pub fn check_display_name(name: &str) -> Result<(), ClientV2Error> {
    let bytes = name.as_bytes();
    if bytes.len() > crate::protocol_v2::MAX_NAME_BYTES {
        return Err(ClientV2Error::BadName {
            max: crate::protocol_v2::MAX_NAME_BYTES,
        });
    }
    if bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7F || (0x80..=0x9F).contains(&b))
    {
        return Err(ClientV2Error::BadName {
            max: crate::protocol_v2::MAX_NAME_BYTES,
        });
    }
    Ok(())
}

/// Canonical bytes the Hello-v2 Ed25519 signature covers:
/// `"AKHELLO2" || registry_id || tls_exporter || Hello fields without the
/// signature` (pairing-flow §3). Public so the server side (A2) and tests
/// verify against the exact same construction.
pub fn hello_v2_signing_bytes(
    registry_id: &RegistryId,
    tls_exporter: &[u8],
    version: u16,
    device_id: &DeviceId,
    name: &str,
    timestamp_ms: u64,
    challenge: &Nonce32,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(8 + 32 + tls_exporter.len() + 2 + 32 + 2 + name.len() + 8 + 32);
    out.extend_from_slice(HELLO_DOMAIN);
    out.extend_from_slice(registry_id);
    out.extend_from_slice(tls_exporter);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(device_id);
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&timestamp_ms.to_be_bytes());
    out.extend_from_slice(challenge);
    out
}

/// Protocol version carried in the v2 Hello (`CtrlMessage::Hello.version`,
/// same number as the frame header byte).
pub fn hello_version() -> u16 {
    u16::from(crate::protocol_v2::VERSION)
}

/// The TLS server name for a pinned connection: the socket's IP literal —
/// identity comes from the key pin, never from DNS names.
pub fn server_name(ip: std::net::IpAddr) -> ServerName<'static> {
    ServerName::IpAddress(ip.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol_v2::VERSION;

    #[test]
    fn signing_bytes_layout_is_canonical() {
        let bytes = hello_v2_signing_bytes(
            &[0x11; 32],
            &[0x22; 32],
            2,
            &[0x33; 32],
            "dev",
            7,
            &[0x44; 32],
        );
        // 8 + 32 + 32 + 2 + 32 + (2+3) + 8 + 32
        assert_eq!(bytes.len(), 151);
        assert_eq!(&bytes[..8], b"AKHELLO2");
        assert_eq!(&bytes[8..40], &[0x11; 32]); // registry id
        assert_eq!(&bytes[40..72], &[0x22; 32]); // TLS exporter
        assert_eq!(&bytes[72..74], &[0, 2]); // version
        assert_eq!(&bytes[74..106], &[0x33; 32]); // device id
        assert_eq!(u16::from_be_bytes([bytes[106], bytes[107]]), 3); // name len
        assert_eq!(&bytes[108..111], b"dev");
        assert_eq!(u64::from_be_bytes(bytes[111..119].try_into().unwrap()), 7);
        assert_eq!(&bytes[119..151], &[0x44; 32]); // challenge
    }

    #[test]
    fn name_rules_reject_control_chars_and_oversize() {
        assert!(check_display_name("laptop-a").is_ok());
        assert!(check_display_name("a\u{7}b").is_err());
        assert!(check_display_name(&"x".repeat(65)).is_err());
        assert!(check_display_name(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn hello_version_matches_frame_version() {
        assert_eq!(hello_version(), u16::from(VERSION));
    }
}
