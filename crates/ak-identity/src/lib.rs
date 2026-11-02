//! Device identity for AK-sending.
//!
//! Each device holds one Ed25519 key pair. The public key is the device
//! identity; its BLAKE3 digest is the fingerprint used for pairing and
//! certificate pinning. Private key material never leaves the device and is
//! never written into coordination-plane messages.
#![forbid(unsafe_code)]

use std::io;
use std::path::Path;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;
pub const FINGERPRINT_LEN: usize = 32;

/// BLAKE3(public key). Displayed, compared, and pinned everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; FINGERPRINT_LEN]);

impl Fingerprint {
    pub fn from_public_key(public: &[u8; PUBLIC_KEY_LEN]) -> Self {
        let digest = blake3::hash(public);
        let mut bytes = [0u8; FINGERPRINT_LEN];
        bytes.copy_from_slice(&digest.as_bytes()[..FINGERPRINT_LEN]);
        Fingerprint(bytes)
    }

    pub fn from_bytes(bytes: [u8; FINGERPRINT_LEN]) -> Self {
        Fingerprint(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; FINGERPRINT_LEN] {
        &self.0
    }

    /// 64 lowercase hex characters, stable display form.
    pub fn to_hex(&self) -> String {
        todo!("owned by ak-identity agent")
    }

    /// Parse 64 hex characters (case-insensitive, no separators).
    pub fn from_hex(text: &str) -> Option<Self> {
        todo!("owned by ak-identity agent")
    }

    /// First 4 bytes as 8 hex characters, for compact UI display.
    pub fn short_id(&self) -> String {
        todo!("owned by ak-identity agent")
    }
}

/// Human-verifiable pairing code derived from the fingerprint: "123-456".
pub fn pairing_code(fingerprint: &Fingerprint) -> String {
    todo!("owned by ak-identity agent")
}

/// Check a user-typed pairing code (digits and optional dash/space separators).
pub fn verify_pairing_code(fingerprint: &Fingerprint, code: &str) -> bool {
    todo!("owned by ak-identity agent")
}

/// The device key pair. Persistence is a 32-byte seed file, written atomically.
#[derive(Clone)]
pub struct DeviceKeyPair {
    signing: ed25519_dalek::SigningKey,
}

impl DeviceKeyPair {
    pub fn generate() -> Self {
        todo!("owned by ak-identity agent")
    }

    /// Load the seed file, or create and persist a new key. The bool says
    /// whether a new key was created.
    pub fn load_or_create(path: &Path) -> io::Result<(Self, bool)> {
        todo!("owned by ak-identity agent")
    }

    /// Write the seed atomically (temp file, fsync, rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        todo!("owned by ak-identity agent")
    }

    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        todo!("owned by ak-identity agent")
    }

    pub fn fingerprint(&self) -> Fingerprint {
        todo!("owned by ak-identity agent")
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        todo!("owned by ak-identity agent")
    }

    /// Stateless Ed25519 verification used by peers and the coordination plane.
    pub fn verify(public: &[u8; PUBLIC_KEY_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
        todo!("owned by ak-identity agent")
    }
}

/// One paired peer, persisted locally. No secrets, only public data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRecord {
    pub fingerprint: Fingerprint,
    pub name: String,
    pub paired_unix_ms: u64,
}

/// Local paired-peer list with atomic file persistence.
#[derive(Debug, Default)]
pub struct PeerBook {
    peers: Vec<PeerRecord>,
}

impl PeerBook {
    pub fn load(path: &Path) -> io::Result<Self> {
        todo!("owned by ak-identity agent")
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        todo!("owned by ak-identity agent")
    }

    pub fn upsert(&mut self, record: PeerRecord) {
        todo!("owned by ak-identity agent")
    }

    pub fn get(&self, fingerprint: &Fingerprint) -> Option<&PeerRecord> {
        todo!("owned by ak-identity agent")
    }

    pub fn remove(&mut self, fingerprint: &Fingerprint) -> bool {
        todo!("owned by ak-identity agent")
    }

    pub fn iter(&self) -> impl Iterator<Item = &PeerRecord> + '_ {
        self.peers.iter()
    }
}

/// Certificate helpers for the direct QUIC channel.
pub mod tls {
    use super::{DeviceKeyPair, Fingerprint};

    /// A self-signed certificate plus its PKCS#8 key, ready for rustls/quinn.
    #[derive(Clone)]
    pub struct TlsBundle {
        pub cert_der: Vec<u8>,
        pub key_pkcs8_der: Vec<u8>,
        pub fingerprint: Fingerprint,
    }

    /// Create the self-signed certificate for this device key. The certificate
    /// carries the Ed25519 public key in its SubjectPublicKeyInfo.
    pub fn make_self_signed(key: &DeviceKeyPair) -> TlsBundle {
        todo!("owned by ak-identity agent")
    }

    #[derive(Debug, thiserror::Error)]
    pub enum CertError {
        #[error("malformed certificate")]
        Malformed,
        #[error("certificate is not self-signed or signature invalid")]
        BadSignature,
        #[error("certificate public key does not match the pinned key")]
        KeyMismatch,
    }

    /// Verify a peer certificate against a pinned Ed25519 public key:
    /// parse minimal DER, require the SPKI key to equal `pinned_public`
    /// (constant-time compare) and the self-signature to verify.
    pub fn verify_pinned_certificate(cert_der: &[u8], pinned_public: &[u8; 32]) -> Result<(), CertError> {
        todo!("owned by ak-identity agent")
    }

    /// Extract the Ed25519 public key from a self-signed certificate after
    /// verifying its self-signature; used by the accepting side to learn the
    /// peer fingerprint before consulting the authorize callback.
    pub fn self_signed_public_key(cert_der: &[u8]) -> Result<[u8; 32], CertError> {
        todo!("owned by ak-identity agent")
    }
}
