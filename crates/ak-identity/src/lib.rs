//! Device identity for AK-sending.
//!
//! Each device holds one Ed25519 key pair. The public key is the device
//! identity; its BLAKE3 digest is the fingerprint used for pairing and
//! certificate pinning. Private key material never leaves the device and is
//! never written into coordination-plane messages.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;
pub const FINGERPRINT_LEN: usize = 32;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const SEED_LEN: usize = 32;

/// BLAKE3(public key). Displayed, compared, and pinned everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; FINGERPRINT_LEN]);

/// Decode one hex character; separators and other bytes are rejected.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX_LOWER[(byte >> 4) as usize] as char);
        out.push(HEX_LOWER[(byte & 0x0f) as usize] as char);
    }
    out
}

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
        hex_lower(&self.0)
    }

    /// Parse 64 hex characters (case-insensitive, no separators).
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != FINGERPRINT_LEN * 2 {
            return None;
        }
        let mut out = [0u8; FINGERPRINT_LEN];
        for (i, slot) in out.iter_mut().enumerate() {
            let hi = hex_value(bytes[i * 2])?;
            let lo = hex_value(bytes[i * 2 + 1])?;
            *slot = (hi << 4) | lo;
        }
        Some(Fingerprint(out))
    }

    /// First 4 bytes as 8 hex characters, for compact UI display.
    pub fn short_id(&self) -> String {
        hex_lower(&self.0[..4])
    }
}

/// Human-verifiable pairing code derived from the fingerprint: "123-456".
pub fn pairing_code(fingerprint: &Fingerprint) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ak-pairing-code");
    hasher.update(fingerprint.as_bytes());
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
    format!("{:03}-{:03}", value / 1_000, value % 1_000)
}

/// Check a user-typed pairing code (digits and optional dash/space separators).
pub fn verify_pairing_code(fingerprint: &Fingerprint, code: &str) -> bool {
    let mut digits = [0u8; 6];
    let mut count = 0usize;
    for &byte in code.as_bytes() {
        match byte {
            b'-' | b' ' => {}
            b'0'..=b'9' => {
                if count == 6 {
                    return false;
                }
                digits[count] = byte;
                count += 1;
            }
            _ => return false,
        }
    }
    if count != 6 {
        return false;
    }
    pairing_code(fingerprint)
        .bytes()
        .filter(|b| *b != b'-')
        .zip(digits.iter())
        .all(|(expected, typed)| expected == *typed)
}

/// The device key pair. Persistence is a 32-byte seed file, written atomically.
#[derive(Clone)]
pub struct DeviceKeyPair {
    signing: ed25519_dalek::SigningKey,
}

impl DeviceKeyPair {
    pub fn generate() -> Self {
        Self { signing: SigningKey::generate(&mut OsRng) }
    }

    fn from_seed(seed: [u8; SEED_LEN]) -> Self {
        Self { signing: SigningKey::from_bytes(&seed) }
    }

    /// Load the seed file, or create and persist a new key. The bool says
    /// whether a new key was created.
    pub fn load_or_create(path: &Path) -> io::Result<(Self, bool)> {
        match fs::read(path) {
            Ok(seed) => {
                if seed.len() != SEED_LEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{}: seed file must hold exactly {} bytes, found {}",
                            path.display(),
                            SEED_LEN,
                            seed.len()
                        ),
                    ));
                }
                let mut bytes = [0u8; SEED_LEN];
                bytes.copy_from_slice(&seed);
                Ok((Self::from_seed(bytes), false))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let key = Self::generate();
                key.save(path)?;
                Ok((key, true))
            }
            Err(err) => Err(err),
        }
    }

    /// Write the seed atomically (temp file, fsync, rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        atomic_write(path, &self.signing.to_bytes())
    }

    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::from_public_key(&self.public_key())
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.signing.sign(message).to_bytes()
    }

    /// Stateless Ed25519 verification used by peers and the coordination plane.
    pub fn verify(public: &[u8; PUBLIC_KEY_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
        let Ok(verifying) = VerifyingKey::from_bytes(public) else {
            return false;
        };
        verifying.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature)).is_ok()
    }
}

/// Write `bytes` to `path` atomically: same-directory temp file, fsync,
/// rename. Windows may refuse to rename onto an existing destination that is
/// open, so remove the destination and retry once before giving up.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent)?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: path has no file name", path.display()),
        )
    })?;
    let mut temp_name = file_name.to_os_string();
    temp_name.push(".tmp");
    let temp_path = parent.join(temp_name);

    let write = || -> io::Result<()> {
        #[cfg(unix)]
        let mut options = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = fs::OpenOptions::new();
            options.mode(0o600);
            options
        };
        #[cfg(not(unix))]
        let mut options = fs::OpenOptions::new();
        let mut file = options.write(true).create(true).truncate(true).open(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(err) = write() {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    if fs::rename(&temp_path, path).is_err() {
        if let Err(err) = fs::remove_file(path) {
            if err.kind() != io::ErrorKind::NotFound {
                let _ = fs::remove_file(&temp_path);
                return Err(err);
            }
        }
        if let Err(err) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(err);
        }
    }
    Ok(())
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

const PEERBOOK_MAGIC: &str = "AKPEERS1";

/// One line of the peer book: fingerprint hex, name, paired timestamp.
/// Tab and newline bytes in a name would break the format, so they become spaces.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| matches!(c, '\t' | '\n' | '\r').then_some(' ').unwrap_or(c))
        .collect()
}

impl PeerBook {
    /// Load a saved peer book. A missing file is an error (kind `NotFound`);
    /// callers start from `PeerBook::default()` for a fresh device.
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut lines = text.lines();
        let header = lines.next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format!("{}: peer book is empty", path.display()))
        })?;
        if header.trim_end_matches('\r') != PEERBOOK_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: unexpected peer book header", path.display()),
            ));
        }

        let mut book = PeerBook::default();
        for (index, line) in lines.enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let line_no = index + 2;
            let invalid = |reason: &str| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: line {line_no}: {reason}", path.display()),
                )
            };
            let mut fields = line.split('\t');
            let (fp_hex, name, ts) = match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(fp), Some(name), Some(ts), None) => (fp, name, ts),
                _ => return Err(invalid("expected fingerprint, name, timestamp fields")),
            };
            let fingerprint = Fingerprint::from_hex(fp_hex)
                .ok_or_else(|| invalid("fingerprint must be 64 hex characters"))?;
            let paired_unix_ms: u64 = ts
                .parse()
                .map_err(|_| invalid("paired timestamp must be an unsigned integer"))?;
            book.upsert(PeerRecord { fingerprint, name: name.to_owned(), paired_unix_ms });
        }
        Ok(book)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut out = String::with_capacity(PEERBOOK_MAGIC.len() + 1 + self.peers.len() * 80);
        out.push_str(PEERBOOK_MAGIC);
        out.push('\n');
        for peer in &self.peers {
            out.push_str(&peer.fingerprint.to_hex());
            out.push('\t');
            out.push_str(&sanitize_name(&peer.name));
            out.push('\t');
            out.push_str(&peer.paired_unix_ms.to_string());
            out.push('\n');
        }
        atomic_write(path, out.as_bytes())
    }

    pub fn upsert(&mut self, record: PeerRecord) {
        match self.peers.iter_mut().find(|peer| peer.fingerprint == record.fingerprint) {
            Some(slot) => *slot = record,
            None => self.peers.push(record),
        }
    }

    pub fn get(&self, fingerprint: &Fingerprint) -> Option<&PeerRecord> {
        self.peers.iter().find(|peer| &peer.fingerprint == fingerprint)
    }

    pub fn remove(&mut self, fingerprint: &Fingerprint) -> bool {
        let len = self.peers.len();
        self.peers.retain(|peer| &peer.fingerprint != fingerprint);
        self.peers.len() != len
    }

    pub fn iter(&self) -> impl Iterator<Item = &PeerRecord> + '_ {
        self.peers.iter()
    }
}

/// Certificate helpers for the direct QUIC channel.
pub mod tls {
    use super::{DeviceKeyPair, Fingerprint};
    use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// DER content bytes of the Ed25519 algorithm OID (1.3.101.115).
    const ED25519_OID: [u8; 3] = [0x2B, 0x65, 0x70];
    /// Roughly ten years of validity in whole days.
    const TEN_YEAR_DAYS: i64 = 3653;
    /// Clamp the wall clock so certificate dates stay inside rcgen's range
    /// even on a wildly wrong system clock.
    const MAX_CLOCK_DAYS: i64 = 2_500_000;

    /// A self-signed certificate plus its PKCS#8 key, ready for rustls/quinn.
    /// Debug never exposes key bytes beyond lengths.
    #[derive(Debug, Clone)]
    pub struct TlsBundle {
        pub cert_der: Vec<u8>,
        pub key_pkcs8_der: Vec<u8>,
        pub fingerprint: Fingerprint,
    }

    /// Build the fixed 48-byte PKCS#8 DER for an Ed25519 seed
    /// (RFC 5958 + RFC 8410):
    /// `30 2E 02 01 00 30 05 06 03 2B 65 70 04 22 04 20 <seed32>`.
    fn seed_pkcs8_der(seed: &[u8; 32]) -> Vec<u8> {
        const PREFIX: [u8; 16] = [
            0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x04, 0x22, 0x04,
            0x20,
        ];
        let mut der = Vec::with_capacity(PREFIX.len() + seed.len());
        der.extend_from_slice(&PREFIX);
        der.extend_from_slice(seed);
        der
    }

    /// Whole days since the Unix epoch, clamped to a sane certificate range.
    fn current_unix_days() -> i64 {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0);
        secs.div_euclid(86_400).clamp(0, MAX_CLOCK_DAYS)
    }

    /// Civil (year, month, day) from days since the Unix epoch; Howard
    /// Hinnant's algorithm, valid for the whole range we can pass in.
    fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let year = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (year + i64::from(month <= 2), month, day)
    }

    fn build_bundle(signing: &SigningKey) -> Result<TlsBundle, rcgen::Error> {
        let seed = signing.to_bytes();
        let pkcs8 = seed_pkcs8_der(&seed);
        let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &rustls_pki_types::PrivatePkcs8KeyDer::from(pkcs8.as_slice()),
            &rcgen::PKCS_ED25519,
        )?;

        let days = current_unix_days();
        let (y0, m0, d0) = civil_from_days(days - 1);
        let (y1, m1, d1) = civil_from_days(days + TEN_YEAR_DAYS);
        let mut params = rcgen::CertificateParams::default();
        params.not_before = rcgen::date_time_ymd(y0 as i32, m0 as u8, d0 as u8);
        params.not_after = rcgen::date_time_ymd(y1 as i32, m1 as u8, d1 as u8);

        let certificate = params.self_signed(&key_pair)?;
        let public = signing.verifying_key().to_bytes();
        Ok(TlsBundle {
            cert_der: certificate.der().to_vec(),
            key_pkcs8_der: pkcs8,
            fingerprint: Fingerprint::from_public_key(&public),
        })
    }

    /// Create the self-signed certificate for this device key. The certificate
    /// carries the Ed25519 public key in its SubjectPublicKeyInfo.
    pub fn make_self_signed(key: &DeviceKeyPair) -> TlsBundle {
        // Every fallible step above receives fully controlled, fixed-size
        // input (our own seed and a clamped ten-year window), so rcgen cannot
        // realistically fail; the frozen infallible signature leaves no
        // channel to surface an error.
        match build_bundle(&key.signing) {
            Ok(bundle) => bundle,
            Err(_) => unreachable!("rcgen failed on fixed valid Ed25519 PKCS#8 input"),
        }
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

    /// One parsed DER tag-length-value element. `full` covers header plus
    /// content, which is exactly the byte range an X.509 signature commits to.
    struct Tlv<'a> {
        tag: u8,
        content: &'a [u8],
        full: &'a [u8],
    }

    const TAG_BIT_STRING: u8 = 0x03;
    const TAG_OID: u8 = 0x06;
    const TAG_SEQUENCE: u8 = 0x30;

    /// Minimal DER cursor: walks definite-length TLV elements, rejecting
    /// truncated input, indefinite lengths and non-minimal length encodings.
    struct DerReader<'a> {
        input: &'a [u8],
        pos: usize,
    }

    impl<'a> DerReader<'a> {
        fn new(input: &'a [u8]) -> Self {
            DerReader { input, pos: 0 }
        }

        fn done(&self) -> bool {
            self.pos == self.input.len()
        }

        /// Read the next element starting at `self.pos`, advancing past it.
        fn next(&mut self) -> Result<Tlv<'a>, CertError> {
            let start = self.pos;
            let (&tag, rest) =
                self.input.get(start..).and_then(|s| s.split_first()).ok_or(CertError::Malformed)?;
            let (&len_byte, mut rest) = rest.split_first().ok_or(CertError::Malformed)?;
            let mut header_len = 2usize;
            let len = if len_byte & 0x80 == 0 {
                len_byte as usize
            } else {
                // Definite long form, at most 4 length bytes, minimal encoding.
                let count = (len_byte & 0x7f) as usize;
                if count == 0 || count > 4 {
                    return Err(CertError::Malformed);
                }
                let mut len = 0usize;
                for _ in 0..count {
                    let (&byte, tail) = rest.split_first().ok_or(CertError::Malformed)?;
                    if len == 0 && byte == 0 {
                        return Err(CertError::Malformed); // leading zero length byte
                    }
                    rest = tail;
                    len = (len << 8) | byte as usize;
                    if len > self.input.len() {
                        return Err(CertError::Malformed);
                    }
                }
                header_len = 2 + count;
                len
            };
            let content_start = start + header_len;
            let end = content_start
                .checked_add(len)
                .filter(|end| *end <= self.input.len())
                .ok_or(CertError::Malformed)?;
            let content = &self.input[content_start..end];
            self.pos = end;
            Ok(Tlv { tag, content, full: &self.input[start..end] })
        }
    }

    /// Split an X.509 certificate into the full TBS certificate bytes and the
    /// 64-byte Ed25519 signature bits.
    fn split_certificate(der: &[u8]) -> Result<(Tlv<'_>, [u8; 64]), CertError> {
        let mut top = DerReader::new(der);
        let outer = top.next()?;
        if outer.tag != TAG_SEQUENCE || !top.done() {
            return Err(CertError::Malformed);
        }
        let mut inner = DerReader::new(outer.content);
        let tbs = inner.next()?;
        let alg = inner.next()?;
        let sig = inner.next()?;
        if tbs.tag != TAG_SEQUENCE || alg.tag != TAG_SEQUENCE || !inner.done() {
            return Err(CertError::Malformed);
        }
        let bits = bit_string_content(&sig)?;
        let signature: [u8; 64] =
            bits.try_into().map_err(|_| CertError::Malformed)?; // Ed25519 signatures are 64 bytes
        Ok((tbs, signature))
    }

    /// Content bytes of a BIT STRING with zero unused bits.
    fn bit_string_content<'a>(tlv: &'a Tlv<'a>) -> Result<&'a [u8], CertError> {
        if tlv.tag != TAG_BIT_STRING {
            return Err(CertError::Malformed);
        }
        let (&unused, bits) = tlv.content.split_first().ok_or(CertError::Malformed)?;
        if unused != 0 {
            return Err(CertError::Malformed);
        }
        Ok(bits)
    }

    /// Find the Ed25519 SubjectPublicKeyInfo inside a TBS certificate and
    /// return its 32-byte public key. The search is structural: only a
    /// SEQUENCE of (Ed25519 AlgorithmIdentifier, 32-byte BIT STRING) matches,
    /// so issuer/subject/validity elements and extensions cannot shadow it.
    fn spki_ed25519_public_key(tbs_content: &[u8]) -> Result<[u8; 32], CertError> {
        let mut reader = DerReader::new(tbs_content);
        while !reader.done() {
            let child = reader.next()?;
            if child.tag != TAG_SEQUENCE {
                continue;
            }
            let mut spki = DerReader::new(child.content);
            let Ok(alg) = spki.next() else { continue };
            if alg.tag != TAG_SEQUENCE {
                continue;
            }
            let mut alg_reader = DerReader::new(alg.content);
            let Ok(oid) = alg_reader.next() else { continue };
            if oid.tag != TAG_OID || oid.content != ED25519_OID {
                continue;
            }
            let Ok(key) = spki.next() else { continue };
            let bits = bit_string_content(&key)?;
            return bits.try_into().map_err(|_| CertError::Malformed);
        }
        Err(CertError::Malformed)
    }

    /// Constant-time equality for fixed-size key material; folds all byte
    /// differences before deciding, with no `subtle` dependency.
    fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }

    fn verify_self_signature(public: &[u8; 32], tbs_der: &[u8], signature: &[u8; 64]) -> Result<(), CertError> {
        let verifying = VerifyingKey::from_bytes(public).map_err(|_| CertError::Malformed)?;
        verifying
            .verify_strict(tbs_der, &Signature::from_bytes(signature))
            .map_err(|_| CertError::BadSignature)
    }

    /// Verify a peer certificate against a pinned Ed25519 public key:
    /// parse minimal DER, require the SPKI key to equal `pinned_public`
    /// (constant-time compare) and the self-signature to verify.
    pub fn verify_pinned_certificate(cert_der: &[u8], pinned_public: &[u8; 32]) -> Result<(), CertError> {
        let (tbs, signature) = split_certificate(cert_der)?;
        let public = spki_ed25519_public_key(tbs.content)?;
        if !constant_time_eq(&public, pinned_public) {
            return Err(CertError::KeyMismatch);
        }
        verify_self_signature(&public, tbs.full, &signature)
    }

    /// Extract the Ed25519 public key from a self-signed certificate after
    /// verifying its self-signature; used by the accepting side to learn the
    /// peer fingerprint before consulting the authorize callback.
    pub fn self_signed_public_key(cert_der: &[u8]) -> Result<[u8; 32], CertError> {
        let (tbs, signature) = split_certificate(cert_der)?;
        let public = spki_ed25519_public_key(tbs.content)?;
        verify_self_signature(&public, tbs.full, &signature)?;
        Ok(public)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Pseudo-random bytes from a tiny LCG; only used to feed hostile
        /// input, never as key material.
        fn lcg_bytes(seed: &mut u64, len: usize) -> Vec<u8> {
            let mut out = vec![0u8; len];
            for slot in out.iter_mut() {
                *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                *slot = (*seed >> 33) as u8;
            }
            out
        }

        #[test]
        fn self_signed_cert_verifies_against_own_key() {
            let key = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);
            let public = key.public_key();

            assert_eq!(bundle.fingerprint, key.fingerprint());
            assert_eq!(self_signed_public_key(&bundle.cert_der).unwrap(), public);
            assert!(verify_pinned_certificate(&bundle.cert_der, &public).is_ok());
            // The DER is non-trivial and looks like a certificate.
            assert!(bundle.cert_der.first() == Some(&0x30) && bundle.cert_der.len() > 100);
        }

        #[test]
        fn cert_with_wrong_pin_is_key_mismatch() {
            let key = DeviceKeyPair::generate();
            let other = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);

            assert!(matches!(
                verify_pinned_certificate(&bundle.cert_der, &other.public_key()),
                Err(CertError::KeyMismatch)
            ));
            assert!(self_signed_public_key(&bundle.cert_der).is_ok());
        }

        #[test]
        fn two_devices_produce_distinct_certificates() {
            let a = DeviceKeyPair::generate();
            let b = DeviceKeyPair::generate();
            let cert_a = make_self_signed(&a);
            let cert_b = make_self_signed(&b);

            assert_ne!(cert_a.cert_der, cert_b.cert_der);
            assert!(matches!(
                verify_pinned_certificate(&cert_b.cert_der, &a.public_key()),
                Err(CertError::KeyMismatch)
            ));
        }

        #[test]
        fn tampered_signature_is_bad_signature() {
            let key = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);
            let public = key.public_key();

            let mut tampered = bundle.cert_der.clone();
            let last = tampered.len() - 1;
            tampered[last] ^= 0x01;
            assert!(matches!(
                verify_pinned_certificate(&tampered, &public),
                Err(CertError::BadSignature)
            ));
            assert!(matches!(self_signed_public_key(&tampered), Err(CertError::BadSignature)));
        }

        #[test]
        fn tampered_certificate_bytes_are_rejected() {
            let key = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);
            let public = key.public_key();

            for index in [1usize, 8, 20, 60] {
                let mut tampered = bundle.cert_der.clone();
                tampered[index] ^= 0xFF;
                let result = verify_pinned_certificate(&tampered, &public);
                assert!(result.is_err(), "tampering byte {index} was accepted");
            }
        }

        #[test]
        fn malformed_and_hostile_certificates_are_rejected_without_panicking() {
            let key = DeviceKeyPair::generate();
            let public = key.public_key();
            let good = make_self_signed(&key).cert_der;

            let truncated = good[..good.len() - 1].to_vec();
            let mut trailing = good.clone();
            trailing.push(0x00);
            let mut deep_nesting = vec![0x30u8; 256];
            deep_nesting.push(0x00);
            let hostile = [
                Vec::new(),
                vec![0x30],
                vec![0x30, 0x00],
                vec![0x30, 0x80],                          // indefinite length
                vec![0x30, 0x81, 0x00],                    // non-minimal length
                vec![0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF],  // huge length
                vec![0x30, 0x84, 0x7F, 0xFF, 0xFF, 0xFF],  // length beyond input
                vec![0x31, 0x00],                          // wrong outer tag
                vec![0xFF; 512],                           // garbage
                truncated,
                trailing,
                deep_nesting,
                lcg_bytes(&mut 0x1234_5678, 300),
            ];
            for input in &hostile {
                assert!(
                    matches!(verify_pinned_certificate(input, &public), Err(CertError::Malformed)),
                    "input accepted: {} bytes starting {:02x?}",
                    input.len(),
                    input.iter().take(8).collect::<Vec<_>>()
                );
                assert!(matches!(self_signed_public_key(input), Err(CertError::Malformed)));
            }

            // Larger pseudo-random blobs never validate either.
            let mut seed = 0x9E37_79B9;
            for _ in 0..50 {
                let len = (seed % 700) as usize;
                let blob = lcg_bytes(&mut seed, len);
                assert!(verify_pinned_certificate(&blob, &public).is_err());
            }
        }

        #[test]
        fn pkcs8_layout_is_exact_and_parses_in_rcgen() {
            let seed: [u8; 32] = core::array::from_fn(|i| i as u8);
            let der = seed_pkcs8_der(&seed);

            let mut expected = vec![
                0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x04, 0x22,
                0x04, 0x20,
            ];
            expected.extend_from_slice(&seed);
            assert_eq!(der, expected);
            assert_eq!(der.len(), 48);

            let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
                &rustls_pki_types::PrivatePkcs8KeyDer::from(der.as_slice()),
                &rcgen::PKCS_ED25519,
            )
            .unwrap();
            let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
            assert_eq!(key_pair.public_key_raw(), public.as_slice());
        }

        #[test]
        fn bundle_pkcs8_round_trips_through_rcgen() {
            let key = DeviceKeyPair::generate();
            let bundle = make_self_signed(&key);
            let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
                &rustls_pki_types::PrivatePkcs8KeyDer::from(bundle.key_pkcs8_der.as_slice()),
                &rcgen::PKCS_ED25519,
            )
            .unwrap();
            assert_eq!(key_pair.public_key_raw(), key.public_key().as_slice());
        }

        #[test]
        fn constant_time_compare_detects_single_bit_differences() {
            let a = [0x5Au8; 32];
            let mut b = a;
            assert!(constant_time_eq(&a, &b));
            b[31] ^= 0x01;
            assert!(!constant_time_eq(&a, &b));
            b[31] ^= 0x01;
            b[0] ^= 0x80;
            assert!(!constant_time_eq(&a, &b));
        }

        #[test]
        fn civil_date_conversion_matches_known_dates() {
            assert_eq!(civil_from_days(0), (1970, 1, 1));
            assert_eq!(civil_from_days(3_652), (1980, 1, 1));
            assert_eq!(civil_from_days(19_723), (2024, 1, 1));
            assert_eq!(civil_from_days(19_782), (2024, 2, 29));
            assert_eq!(civil_from_days(20_666), (2026, 8, 1));
            assert_eq!(civil_from_days(20_759), (2026, 11, 2));
        }
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique directory under the system temp dir, removed on drop. Keeps
    /// test key material out of the repository.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ak-identity-{tag}-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::TempDir;
    use super::*;
    use std::fs;

    fn peer(name: &str, paired_unix_ms: u64) -> (Fingerprint, PeerRecord) {
        let fingerprint = DeviceKeyPair::generate().fingerprint();
        (fingerprint, PeerRecord { fingerprint, name: name.to_owned(), paired_unix_ms })
    }

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = DeviceKeyPair::generate().fingerprint();
        let hex = fp.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(Fingerprint::from_hex(&hex), Some(fp));
        assert_eq!(Fingerprint::from_hex(&hex.to_uppercase()), Some(fp));
        assert_eq!(Fingerprint::from_bytes(*fp.as_bytes()), fp);
        assert_eq!(fp.short_id(), hex[..8]);
        assert_eq!(fp.short_id().len(), 8);
    }

    #[test]
    fn fingerprint_hex_rejects_malformed_input() {
        let hex = DeviceKeyPair::generate().fingerprint().to_hex();
        assert_eq!(Fingerprint::from_hex(""), None);
        assert_eq!(Fingerprint::from_hex(&hex[..63]), None);
        assert_eq!(Fingerprint::from_hex(&format!("{hex}0")), None);
        assert_eq!(Fingerprint::from_hex(&hex.repeat(8)), None);
        let mut non_hex = hex.clone();
        non_hex.replace_range(3..4, "g");
        assert_eq!(Fingerprint::from_hex(&non_hex), None);
        let mut with_nul = hex.clone();
        with_nul.replace_range(0..1, "\0");
        assert_eq!(Fingerprint::from_hex(&with_nul), None);
        assert_eq!(Fingerprint::from_hex(&format!("{} {}", &hex[..32], &hex[32..])), None);
    }

    #[test]
    fn fingerprint_is_blake3_prefix_of_public_key() {
        let key = DeviceKeyPair::generate();
        let digest = blake3::hash(&key.public_key());
        let mut expected = [0u8; FINGERPRINT_LEN];
        expected.copy_from_slice(&digest.as_bytes()[..FINGERPRINT_LEN]);
        assert_eq!(key.fingerprint().as_bytes(), &expected);
    }

    #[test]
    fn distinct_keys_have_distinct_fingerprints() {
        let a = DeviceKeyPair::generate();
        let b = DeviceKeyPair::generate();
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn pairing_code_format_matches_specified_derivation() {
        let fp = DeviceKeyPair::generate().fingerprint();
        let code = pairing_code(&fp);
        let bytes = code.as_bytes();
        assert_eq!(bytes.len(), 7);
        assert_eq!(bytes[3], b'-');
        assert!(bytes[..3].iter().all(|b| b.is_ascii_digit()));
        assert!(bytes[4..].iter().all(|b| b.is_ascii_digit()));

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ak-pairing-code");
        hasher.update(fp.as_bytes());
        let digest = hasher.finalize();
        let d = digest.as_bytes();
        let value = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
        assert_eq!(code, format!("{:03}-{:03}", value / 1_000, value % 1_000));
        assert_eq!(pairing_code(&fp), code);
    }

    #[test]
    fn verify_pairing_code_accepts_separator_variants() {
        let fp = DeviceKeyPair::generate().fingerprint();
        let code = pairing_code(&fp);
        let digits: String = code.chars().filter(|c| *c != '-').collect();

        assert!(verify_pairing_code(&fp, &code));
        assert!(verify_pairing_code(&fp, &digits));
        assert!(verify_pairing_code(&fp, &format!("{} {}", &digits[..3], &digits[3..])));
        assert!(verify_pairing_code(&fp, &format!("{}-{}", &digits[..3], &digits[3..])));
        assert!(verify_pairing_code(&fp, &format!(" {}-{}  ", &digits[..3], &digits[3..])));
    }

    #[test]
    fn verify_pairing_code_rejects_wrong_input() {
        let fp = DeviceKeyPair::generate().fingerprint();
        let code = pairing_code(&fp);
        let wrong = pairing_code(&DeviceKeyPair::generate().fingerprint());
        let digits: String = code.chars().filter(|c| *c != '-').collect();

        assert!(!verify_pairing_code(&fp, &wrong));
        assert!(!verify_pairing_code(&fp, ""));
        assert!(!verify_pairing_code(&fp, "0000000"));
        assert!(!verify_pairing_code(&fp, "12345"));
        assert!(!verify_pairing_code(&fp, &digits[..5]));
        assert!(!verify_pairing_code(&fp, &format!("{digits}9")));
        assert!(!verify_pairing_code(&fp, "abc-def"));
        assert!(!verify_pairing_code(&fp, "12345x"));
        assert!(!verify_pairing_code(&fp, "12345+"));
        assert!(!verify_pairing_code(&fp, "١٢٣٤٥٦"));
    }

    #[test]
    fn keypair_sign_and_verify_roundtrip() {
        let key = DeviceKeyPair::generate();
        let message = b"coordinate but never relay";
        let signature = key.sign(message);

        assert_eq!(signature.len(), SIGNATURE_LEN);
        assert!(DeviceKeyPair::verify(&key.public_key(), message, &signature));
        assert!(!DeviceKeyPair::verify(&key.public_key(), b"coordinate but never relaY", &signature));

        let other = DeviceKeyPair::generate();
        assert!(!DeviceKeyPair::verify(&other.public_key(), message, &signature));

        let mut tampered = signature;
        tampered[0] ^= 0x01;
        assert!(!DeviceKeyPair::verify(&key.public_key(), message, &tampered));

        // Invalid public key encodings must not panic.
        let _ = DeviceKeyPair::verify(&[0xFFu8; PUBLIC_KEY_LEN], message, &signature);
    }

    #[test]
    fn keypair_save_load_roundtrip() {
        let dir = TempDir::new("keypair-save");
        let path = dir.path().join("device.seed");
        let key = DeviceKeyPair::generate();

        key.save(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 32);

        let (loaded, created_new) = DeviceKeyPair::load_or_create(&path).unwrap();
        assert!(!created_new);
        assert_eq!(loaded.public_key(), key.public_key());
        assert_eq!(loaded.fingerprint(), key.fingerprint());
        assert_eq!(loaded.sign(b"msg"), key.sign(b"msg"));
    }

    #[test]
    fn keypair_load_or_create_reports_new_key_once() {
        let dir = TempDir::new("keypair-create");
        let path = dir.path().join("seed.bin");

        let (first, created_new) = DeviceKeyPair::load_or_create(&path).unwrap();
        assert!(created_new);
        assert!(path.exists());

        let (second, created_new) = DeviceKeyPair::load_or_create(&path).unwrap();
        assert!(!created_new);
        assert_eq!(first.public_key(), second.public_key());
    }

    #[test]
    fn keypair_rejects_mis_sized_seed_files() {
        let dir = TempDir::new("keypair-badseed");
        for len in [0usize, 31, 33, 64, 4096] {
            let path = dir.path().join(format!("seed-{len}.bin"));
            fs::write(&path, vec![7u8; len]).unwrap();
            // DeviceKeyPair is intentionally not Debug (it holds key material),
            // so unwrap_err is not available on the success type.
            let err = match DeviceKeyPair::load_or_create(&path) {
                Ok(_) => panic!("accepted mis-sized seed file ({len} bytes)"),
                Err(err) => err,
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "len {len}");
            assert!(err.to_string().contains(&len.to_string()), "reason lacks size: {err}");
            assert!(err.to_string().contains("seed"), "reason lacks context: {err}");
        }
    }

    #[test]
    fn atomic_save_leaves_no_temp_files_and_overwrites() {
        let dir = TempDir::new("atomic");
        let path = dir.path().join("device.seed");
        let key = DeviceKeyPair::generate();

        key.save(&path).unwrap();
        key.save(&path).unwrap(); // destination now exists

        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].to_string_lossy(), "device.seed");

        let (reloaded, _) = DeviceKeyPair::load_or_create(&path).unwrap();
        assert_eq!(reloaded.public_key(), key.public_key());
    }

    #[test]
    fn peerbook_upsert_get_remove() {
        let (fp1, laptop) = peer("Laptop", 1_000);
        let (fp2, phone) = peer("Phone", 2_000);
        let (fp3, _) = peer("Tablet", 3_000);

        let mut book = PeerBook::default();
        assert_eq!(book.iter().count(), 0);

        book.upsert(laptop);
        book.upsert(phone.clone());
        assert_eq!(book.iter().count(), 2);

        book.upsert(PeerRecord { fingerprint: fp1, name: "Laptop Pro".to_owned(), paired_unix_ms: 3_000 });
        assert_eq!(book.iter().count(), 2); // replace, not append
        assert_eq!(book.get(&fp1).map(|r| r.name.as_str()), Some("Laptop Pro"));
        assert_eq!(book.get(&fp1).map(|r| r.paired_unix_ms), Some(3_000));
        assert_eq!(book.get(&fp2), Some(&phone));
        assert!(book.get(&fp3).is_none());

        assert!(book.remove(&fp2));
        assert!(!book.remove(&fp2));
        assert!(book.get(&fp2).is_none());
        assert_eq!(book.iter().count(), 1);
    }

    #[test]
    fn peerbook_save_load_roundtrip_sanitizes_names() {
        let dir = TempDir::new("peerbook-roundtrip");
        let path = dir.path().join("peers.txt");

        let (fp1, rec1) = peer("desk\ttop\nlaptop", 1_700_000_000_000);
        let (fp2, rec2) = peer("Ünïcode 设备", 42);
        let (fp3, rec3) = peer("", 0);

        let mut book = PeerBook::default();
        book.upsert(rec1);
        book.upsert(rec2);
        book.upsert(rec3);
        book.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("AKPEERS1\n"));
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains('\t'));

        let loaded = PeerBook::load(&path).unwrap();
        assert_eq!(loaded.iter().count(), 3);
        assert_eq!(loaded.get(&fp1).map(|r| r.name.as_str()), Some("desk top laptop"));
        assert_eq!(loaded.get(&fp2).map(|r| r.paired_unix_ms), Some(42));
        assert_eq!(loaded.get(&fp3).map(|r| r.name.as_str()), Some(""));
    }

    #[test]
    fn peerbook_save_is_atomic_and_rewritable() {
        let dir = TempDir::new("peerbook-atomic");
        let path = dir.path().join("peers.txt");

        let (fp1, rec1) = peer("one", 1);
        let (fp2, rec2) = peer("two", 2);
        let mut book = PeerBook::default();
        book.upsert(rec1);
        book.save(&path).unwrap();
        book.upsert(rec2);
        book.save(&path).unwrap(); // rewrite over existing destination

        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries.len(), 1);
        let loaded = PeerBook::load(&path).unwrap();
        assert_eq!(loaded.iter().count(), 2);
        assert!(loaded.get(&fp1).is_some() && loaded.get(&fp2).is_some());
    }

    #[test]
    fn peerbook_rejects_malformed_files() {
        let dir = TempDir::new("peerbook-bad");
        let fp_hex = DeviceKeyPair::generate().fingerprint().to_hex();
        // Distinctive sentinel name: if it ever shows up in an error message,
        // field content is leaking into errors.
        const SENTINEL: &str = "alice-sentinel-name";
        let cases: Vec<(&str, String)> = vec![
            ("empty", String::new()),
            ("wrong header", "NOTPEERS\n".to_owned()),
            ("missing header", format!("{fp_hex}\t{SENTINEL}\t1\n")),
            ("bad fingerprint", format!("AKPEERS1\nzz..\t{SENTINEL}\t1\n")),
            ("short fingerprint", format!("AKPEERS1\n{}\t{SENTINEL}\t1\n", "ab".repeat(31))),
            ("uppercase fp but bad ts", format!("AKPEERS1\n{}\t{SENTINEL}\tnope\n", fp_hex.to_uppercase())),
            ("negative timestamp", format!("AKPEERS1\n{fp_hex}\t{SENTINEL}\t-5\n")),
            ("overflow timestamp", format!("AKPEERS1\n{fp_hex}\t{SENTINEL}\t99999999999999999999999\n")),
            ("too few fields", format!("AKPEERS1\n{fp_hex}\t{SENTINEL}\n")),
            ("too many fields", format!("AKPEERS1\n{fp_hex}\t{SENTINEL}\t1\textra\n")),
            ("no fields", "AKPEERS1\n\n\nnope\n".to_owned()),
        ];
        for (label, content) in cases {
            let path = dir.path().join(format!("case-{}.txt", label.replace(' ', "-")));
            fs::write(&path, content).unwrap();
            let err = PeerBook::load(&path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "case {label}: {err}");
            // Whole-file errors mention the file; line errors add a line number.
            let message = err.to_string();
            assert!(message.contains("peer book") || message.contains("line"), "case {label}: {message}");
            assert!(!message.contains(SENTINEL), "case {label} leaks field content: {message}");
            assert!(!message.contains(&fp_hex), "case {label} leaks fingerprint: {message}");
        }
    }

    #[test]
    fn peerbook_tolerates_blank_lines_and_crlf() {
        let dir = TempDir::new("peerbook-blank");
        let path = dir.path().join("peers.txt");
        let (fp, rec) = peer("peer", 7);

        let mut book = PeerBook::default();
        book.upsert(rec);
        book.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("AKPEERS1\r\n\r\n{}\r\n", text.lines().nth(1).unwrap())).unwrap();

        let loaded = PeerBook::load(&path).unwrap();
        assert_eq!(loaded.iter().count(), 1);
        assert_eq!(loaded.get(&fp).map(|r| r.paired_unix_ms), Some(7));
    }

    #[test]
    fn peerbook_missing_file_is_not_found() {
        let dir = TempDir::new("peerbook-missing");
        let err = PeerBook::load(&dir.path().join("nope.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn empty_peerbook_saves_header_only() {
        let dir = TempDir::new("peerbook-empty");
        let path = dir.path().join("peers.txt");
        PeerBook::default().save(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "AKPEERS1\n");
        assert_eq!(PeerBook::load(&path).unwrap().iter().count(), 0);
    }

    #[test]
    fn seed_paths_without_parent_directory_are_created() {
        // Relative path with no directory component still saves (parent = ".").
        let dir = TempDir::new("relative");
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = DeviceKeyPair::load_or_create(Path::new("seed.bin"));
        std::env::set_current_dir(previous).unwrap();
        let (_, created_new) = result.unwrap();
        assert!(created_new);
        assert!(dir.path().join("seed.bin").exists());
    }
}
