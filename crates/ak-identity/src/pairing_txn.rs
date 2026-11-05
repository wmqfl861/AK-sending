//! Typed pairing transaction: transcript, endpoint proofs, and verification
//! (plan A.4 §5 / `docs/contracts/protocol-v2.md` §5, plan A.5 §3–§4).
//!
//! The 256-byte [`PairTranscript`] binds everything a pairing decision
//! rests on — registry identity, code, pairing/request ids, lookup digest,
//! both device identities, both nonces, and the deadline. Each endpoint
//! proves (a) knowledge of the invite-code secret via a domain-separated
//! keyed-BLAKE3 MAC over the full transcript and (b) ownership of its
//! device key via an Ed25519 signature over `role domain || transcript ||
//! mac`:
//!
//! ```text
//! owner_mac    = MAC(auth_key, "AK owner proof v1"    || transcript)
//! redeemer_mac = MAC(auth_key, "AK redeemer proof v1" || transcript)
//! signature    = Ed25519.Sign(device_key, role domain || transcript || mac)
//! ```
//!
//! [`PairingRecord::verify_transcript`] fails with an **independent error
//! variant per attack**: substituted public key, wrong invite code
//! (MAC mismatch), replayed/foreign nonce, and expired proof, plus a
//! transcript-digest integrity check. Success returns a [`VerifiedPairing`]
//! whose fields are only obtainable through verification — the input for
//! `PeerBookStore::commit_paired_peer` (plan A.5 §5) and the A6 pairing
//! orchestration.
//!
//! This is a pairing-handshake transcript, never file-content data: no
//! paths, names, manifests, or transfer progress enter these types.

use crate::{DeviceKeyPair, Fingerprint};
use rand::{rngs::OsRng, RngCore};

/// Request id (CSPRNG, 16 bytes) — protocol-v2 type alias.
pub type RequestId = [u8; 16];
/// Server-assigned invite-code id — protocol-v2 type alias.
pub type CodeId = [u8; 16];
/// Pairing transaction id (CSPRNG, 16 bytes) — protocol-v2 type alias.
pub type PairingId = [u8; 16];
/// Coordination registry identity — protocol-v2 type alias.
pub type RegistryId = [u8; 32];
/// Device identity: a 32-byte Ed25519 public key.
pub type DeviceId = [u8; 32];
/// 32-byte digest (transcript digest, lookup digest).
pub type Digest32 = [u8; 32];
/// 32-byte CSPRNG nonce.
pub type Nonce32 = [u8; 32];
/// 32-byte keyed-BLAKE3 MAC over a proof domain and the transcript.
pub type Mac32 = [u8; 32];
/// 64-byte Ed25519 signature.
pub type Signature64 = [u8; 64];

/// MAC/signature role domain for the code owner (plan A.4 §5, verbatim).
pub const OWNER_PROOF_DOMAIN: &str = "AK owner proof v1";
/// MAC/signature role domain for the code redeemer (plan A.4 §5, verbatim).
pub const REDEEMER_PROOF_DOMAIN: &str = "AK redeemer proof v1";
/// Fixed byte length of the canonical transcript encoding.
pub const PAIR_TRANSCRIPT_LEN: usize = 256;

/// Why a pairing record failed verification. Each variant corresponds to a
/// distinct attack surface; none of them echo record contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingVerifyError {
    #[error("transcript digest does not match the transcript contents")]
    TranscriptDigestMismatch,
    #[error("pairing proof deadline has passed")]
    Expired,
    #[error("transcript nonce does not match the nonce of this pairing attempt")]
    NonceMismatch,
    #[error("proof MAC does not verify under the invite code authentication key")]
    InviteCodeMismatch,
    #[error("proof signature does not verify under the claimed device public key")]
    PublicKeyMismatch,
}

/// The frozen 256-byte pairing transcript (plan A.4 §5). Field order in
/// [`PairTranscript::to_bytes`] is the wire order; integers are big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairTranscript {
    pub registry_id: RegistryId,
    pub code_id: CodeId,
    pub code_revision: u64,
    pub pairing_id: PairingId,
    pub request_id: RequestId,
    pub lookup_digest: Digest32,
    pub owner_device_id: DeviceId,
    pub redeemer_device_id: DeviceId,
    pub owner_nonce: Nonce32,
    pub redeemer_nonce: Nonce32,
    pub deadline_ms: u64,
}

impl PairTranscript {
    /// Canonical big-endian encoding, exactly 256 bytes:
    /// `registry_id[32] | code_id[16] | code_revision:u64 | pairing_id[16] |
    /// request_id[16] | lookup_digest[32] | owner_device_id[32] |
    /// redeemer_device_id[32] | owner_nonce[32] | redeemer_nonce[32] |
    /// deadline_ms:u64`.
    pub fn to_bytes(&self) -> [u8; PAIR_TRANSCRIPT_LEN] {
        fn put(out: &mut [u8], cursor: &mut usize, bytes: &[u8]) {
            out[*cursor..*cursor + bytes.len()].copy_from_slice(bytes);
            *cursor += bytes.len();
        }
        let mut out = [0u8; PAIR_TRANSCRIPT_LEN];
        let mut cursor = 0usize;
        put(&mut out, &mut cursor, &self.registry_id);
        put(&mut out, &mut cursor, &self.code_id);
        put(&mut out, &mut cursor, &self.code_revision.to_be_bytes());
        put(&mut out, &mut cursor, &self.pairing_id);
        put(&mut out, &mut cursor, &self.request_id);
        put(&mut out, &mut cursor, &self.lookup_digest);
        put(&mut out, &mut cursor, &self.owner_device_id);
        put(&mut out, &mut cursor, &self.redeemer_device_id);
        put(&mut out, &mut cursor, &self.owner_nonce);
        put(&mut out, &mut cursor, &self.redeemer_nonce);
        put(&mut out, &mut cursor, &self.deadline_ms.to_be_bytes());
        out
    }

    /// Decode the canonical 256-byte encoding.
    pub fn from_bytes(bytes: &[u8; PAIR_TRANSCRIPT_LEN]) -> Self {
        fn take<const N: usize>(bytes: &[u8], cursor: &mut usize) -> [u8; N] {
            let mut out = [0u8; N];
            out.copy_from_slice(&bytes[*cursor..*cursor + N]);
            *cursor += N;
            out
        }
        let mut cursor = 0usize;
        PairTranscript {
            registry_id: take::<32>(bytes, &mut cursor),
            code_id: take::<16>(bytes, &mut cursor),
            code_revision: u64::from_be_bytes(take::<8>(bytes, &mut cursor)),
            pairing_id: take::<16>(bytes, &mut cursor),
            request_id: take::<16>(bytes, &mut cursor),
            lookup_digest: take::<32>(bytes, &mut cursor),
            owner_device_id: take::<32>(bytes, &mut cursor),
            redeemer_device_id: take::<32>(bytes, &mut cursor),
            owner_nonce: take::<32>(bytes, &mut cursor),
            redeemer_nonce: take::<32>(bytes, &mut cursor),
            deadline_ms: u64::from_be_bytes(take::<8>(bytes, &mut cursor)),
        }
    }

    /// `BLAKE3(canonical transcript bytes)` — the `transcript_digest` carried
    /// by `PairProof`/`PairPrepared`/`PairCommit`/`PairApplied`/`PairReady`.
    pub fn transcript_digest(&self) -> Digest32 {
        blake3::hash(&self.to_bytes()).into()
    }
}

/// `MAC = keyed_BLAKE3(auth_key, domain || transcript)` using the library
/// keyed mode; no hand-rolled MAC construction.
fn proof_mac(auth_key: &[u8; 32], domain: &str, transcript: &[u8; PAIR_TRANSCRIPT_LEN]) -> Mac32 {
    let mut hasher = blake3::Hasher::new_keyed(auth_key);
    hasher.update(domain.as_bytes());
    hasher.update(transcript);
    *hasher.finalize().as_bytes()
}

/// Signed message: `role domain || canonical transcript || mac`.
fn signed_message(domain: &str, transcript: &[u8; PAIR_TRANSCRIPT_LEN], mac: &[u8; 32]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + PAIR_TRANSCRIPT_LEN + 32);
    message.extend_from_slice(domain.as_bytes());
    message.extend_from_slice(transcript);
    message.extend_from_slice(mac);
    message
}

/// Constant-time equality for 32-byte MAC material; folds all byte
/// differences before deciding (same approach as `tls` pin comparison).
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// One endpoint's fixed 96-byte proof: MAC(auth_key) over the transcript
/// plus the device-key signature over `role domain || transcript || mac`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairProofData {
    pub mac: Mac32,
    pub signature: Signature64,
}

/// A complete pairing record: the transcript, its digest, and both
/// endpoints' proofs, as carried by `PairCommit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub transcript: PairTranscript,
    pub transcript_digest: Digest32,
    pub owner_proof: PairProofData,
    pub redeemer_proof: PairProofData,
}

/// What the verifier knows independently of the record: the local invite
/// code's `auth_key`, the nonces this endpoint generated/observed for this
/// pairing attempt, and its wall clock.
///
/// `Debug` is hand-written (review 2026-11-05, P2): `auth_key` is derived
/// from the invite-code secret and must never reach logs. The nonces and
/// the clock are public transcript material and stay visible.
#[derive(Clone, Copy)]
pub struct VerifyExpectations<'a> {
    /// [`InviteCode::auth_key`] of the locally held invite code.
    pub auth_key: &'a [u8; 32],
    /// The owner nonce this pairing attempt must show.
    pub expected_owner_nonce: &'a Nonce32,
    /// The redeemer nonce this pairing attempt must show.
    pub expected_redeemer_nonce: &'a Nonce32,
    /// Verifier's current time in Unix milliseconds; the record is expired
    /// when `now_unix_ms > transcript.deadline_ms`.
    pub now_unix_ms: u64,
}

impl std::fmt::Debug for VerifyExpectations<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyExpectations")
            .field("auth_key", &"<redacted: invite auth key>")
            .field("expected_owner_nonce", &self.expected_owner_nonce)
            .field("expected_redeemer_nonce", &self.expected_redeemer_nonce)
            .field("now_unix_ms", &self.now_unix_ms)
            .finish()
    }
}

impl PairingRecord {
    /// Assemble a record from locally held material: computes the
    /// transcript digest, both domain-separated MACs under `auth_key`, and
    /// both device signatures. Used by the owner when answering a redeem
    /// and by tests/fakes; wire peers instead build the struct from
    /// received fields and call [`PairingRecord::verify_transcript`].
    pub fn build(
        transcript: PairTranscript,
        auth_key: &[u8; 32],
        owner: &DeviceKeyPair,
        redeemer: &DeviceKeyPair,
    ) -> Self {
        let bytes = transcript.to_bytes();
        let owner_mac = proof_mac(auth_key, OWNER_PROOF_DOMAIN, &bytes);
        let redeemer_mac = proof_mac(auth_key, REDEEMER_PROOF_DOMAIN, &bytes);
        PairingRecord {
            transcript_digest: transcript.transcript_digest(),
            owner_proof: PairProofData {
                mac: owner_mac,
                signature: owner.sign(&signed_message(OWNER_PROOF_DOMAIN, &bytes, &owner_mac)),
            },
            redeemer_proof: PairProofData {
                mac: redeemer_mac,
                signature: redeemer.sign(&signed_message(
                    REDEEMER_PROOF_DOMAIN,
                    &bytes,
                    &redeemer_mac,
                )),
            },
            transcript,
        }
    }

    /// Assemble from already-received pieces (e.g. decoded `PairCommit`
    /// fields). No check happens here; verification is
    /// [`PairingRecord::verify_transcript`].
    pub fn from_pieces(
        transcript: PairTranscript,
        transcript_digest: Digest32,
        owner_proof: PairProofData,
        redeemer_proof: PairProofData,
    ) -> Self {
        PairingRecord {
            transcript,
            transcript_digest,
            owner_proof,
            redeemer_proof,
        }
    }

    /// Verify the whole record against the verifier's independent
    /// expectations. Checks run in a fixed order, each failing with its own
    /// variant of [`PairingVerifyError`]:
    ///
    /// 1. `transcript_digest` matches the transcript (tamper/integrity),
    /// 2. deadline not passed (`Expired`),
    /// 3. both nonces match this attempt (`NonceMismatch`),
    /// 4. both MACs verify under the invite code's `auth_key`
    ///    (`InviteCodeMismatch` — wrong or substituted code),
    /// 5. both signatures verify under the device keys the transcript
    ///    claims (`PublicKeyMismatch` — key substitution or role
    ///    reflection).
    ///
    /// On success returns a [`VerifiedPairing`]; there is no other public
    /// way to construct one.
    pub fn verify_transcript(
        &self,
        expectations: &VerifyExpectations<'_>,
    ) -> Result<VerifiedPairing, PairingVerifyError> {
        if self.transcript_digest != self.transcript.transcript_digest() {
            return Err(PairingVerifyError::TranscriptDigestMismatch);
        }
        if expectations.now_unix_ms > self.transcript.deadline_ms {
            return Err(PairingVerifyError::Expired);
        }
        if self.transcript.owner_nonce != *expectations.expected_owner_nonce
            || self.transcript.redeemer_nonce != *expectations.expected_redeemer_nonce
        {
            return Err(PairingVerifyError::NonceMismatch);
        }
        let bytes = self.transcript.to_bytes();
        let expected_owner_mac = proof_mac(expectations.auth_key, OWNER_PROOF_DOMAIN, &bytes);
        if !constant_time_eq_32(&self.owner_proof.mac, &expected_owner_mac) {
            return Err(PairingVerifyError::InviteCodeMismatch);
        }
        let expected_redeemer_mac = proof_mac(expectations.auth_key, REDEEMER_PROOF_DOMAIN, &bytes);
        if !constant_time_eq_32(&self.redeemer_proof.mac, &expected_redeemer_mac) {
            return Err(PairingVerifyError::InviteCodeMismatch);
        }
        if !DeviceKeyPair::verify(
            &self.transcript.owner_device_id,
            &signed_message(OWNER_PROOF_DOMAIN, &bytes, &self.owner_proof.mac),
            &self.owner_proof.signature,
        ) {
            return Err(PairingVerifyError::PublicKeyMismatch);
        }
        if !DeviceKeyPair::verify(
            &self.transcript.redeemer_device_id,
            &signed_message(REDEEMER_PROOF_DOMAIN, &bytes, &self.redeemer_proof.mac),
            &self.redeemer_proof.signature,
        ) {
            return Err(PairingVerifyError::PublicKeyMismatch);
        }
        Ok(VerifiedPairing {
            record: self.clone(),
        })
    }
}

/// Proof-of-verification result. Only obtainable from
/// [`PairingRecord::verify_transcript`], so downstream code (A6
/// orchestration, `PeerBookStore` callers) cannot accidentally treat an
/// unverified record as a pairing decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPairing {
    record: PairingRecord,
}

impl VerifiedPairing {
    /// The pairing transaction id this record committed to.
    pub fn pairing_id(&self) -> PairingId {
        self.record.transcript.pairing_id
    }

    /// Digest of the verified transcript (`PairCommit`/`PairReady`
    /// correlation key).
    pub fn transcript_digest(&self) -> Digest32 {
        self.record.transcript_digest
    }

    /// The invite code's lookup digest this pairing ran under.
    pub fn lookup_digest(&self) -> Digest32 {
        self.record.transcript.lookup_digest
    }

    /// Owner (code holder) device public key.
    pub fn owner_device_id(&self) -> DeviceId {
        self.record.transcript.owner_device_id
    }

    /// Redeemer (code enterer) device public key.
    pub fn redeemer_device_id(&self) -> DeviceId {
        self.record.transcript.redeemer_device_id
    }

    /// Fingerprint of the owner device, computed locally from its public
    /// key — never taken from a server-supplied string.
    pub fn owner_fingerprint(&self) -> Fingerprint {
        Fingerprint::from_public_key(&self.owner_device_id())
    }

    /// Fingerprint of the redeemer device, computed locally from its public
    /// key.
    pub fn redeemer_fingerprint(&self) -> Fingerprint {
        Fingerprint::from_public_key(&self.redeemer_device_id())
    }

    /// Read access to the verified record.
    pub fn record(&self) -> &PairingRecord {
        &self.record
    }

    /// Consume and expose the verified record.
    pub fn into_record(self) -> PairingRecord {
        self.record
    }
}

/// A fresh 32-byte CSPRNG nonce for pairing proofs (plan A.2: never
/// `unique_bytes`, which is clock/counter derived).
pub fn random_nonce32() -> Nonce32 {
    let mut nonce = [0u8; 32];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// A fresh 16-byte CSPRNG id (request/pairing ids).
pub fn random_id16() -> [u8; 16] {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invite::InviteCode;

    struct Fixture {
        transcript: PairTranscript,
        auth_key: [u8; 32],
        owner: DeviceKeyPair,
        redeemer: DeviceKeyPair,
        record: PairingRecord,
    }

    fn deadline_future() -> u64 {
        4_102_444_800_000 // 2100-01-01T00:00:00Z
    }

    fn fixture() -> Fixture {
        let owner = DeviceKeyPair::generate();
        let redeemer = DeviceKeyPair::generate();
        let code = InviteCode::generate();
        let transcript = PairTranscript {
            registry_id: [0x11; 32],
            code_id: [0x22; 16],
            code_revision: 7,
            pairing_id: [0x33; 16],
            request_id: [0x44; 16],
            lookup_digest: code.lookup_digest(),
            owner_device_id: owner.public_key(),
            redeemer_device_id: redeemer.public_key(),
            owner_nonce: [0x55; 32],
            redeemer_nonce: [0x66; 32],
            deadline_ms: deadline_future(),
        };
        let auth_key = code.auth_key();
        let record = PairingRecord::build(transcript, &auth_key, &owner, &redeemer);
        Fixture {
            transcript,
            auth_key,
            owner,
            redeemer,
            record,
        }
    }

    fn expectations(fx: &Fixture) -> VerifyExpectations<'_> {
        VerifyExpectations {
            auth_key: &fx.auth_key,
            expected_owner_nonce: &fx.transcript.owner_nonce,
            expected_redeemer_nonce: &fx.transcript.redeemer_nonce,
            now_unix_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn transcript_canonical_layout_is_256_bytes_and_roundtrips() {
        let fx = fixture();
        let bytes = fx.transcript.to_bytes();

        assert_eq!(bytes.len(), PAIR_TRANSCRIPT_LEN);
        assert_eq!(&bytes[0..32], &fx.transcript.registry_id);
        assert_eq!(&bytes[32..48], &fx.transcript.code_id);
        assert_eq!(&bytes[48..56], &7u64.to_be_bytes());
        assert_eq!(&bytes[56..72], &fx.transcript.pairing_id);
        assert_eq!(&bytes[72..88], &fx.transcript.request_id);
        assert_eq!(&bytes[88..120], &fx.transcript.lookup_digest);
        assert_eq!(&bytes[120..152], &fx.transcript.owner_device_id);
        assert_eq!(&bytes[152..184], &fx.transcript.redeemer_device_id);
        assert_eq!(&bytes[184..216], &fx.transcript.owner_nonce);
        assert_eq!(&bytes[216..248], &fx.transcript.redeemer_nonce);
        assert_eq!(&bytes[248..256], &fx.transcript.deadline_ms.to_be_bytes());

        assert_eq!(
            PairTranscript::from_bytes(&bytes),
            fx.transcript,
            "canonical encoding roundtrips"
        );
    }

    #[test]
    fn transcript_digest_is_blake3_of_canonical_bytes() {
        let fx = fixture();
        let mut expected = [0u8; 32];
        expected.copy_from_slice(blake3::hash(&fx.transcript.to_bytes()).as_bytes());
        assert_eq!(fx.transcript.transcript_digest(), expected);
        // A one-bit transcript change changes the digest.
        let mut tampered = fx.transcript;
        tampered.code_revision += 1;
        assert_ne!(tampered.transcript_digest(), expected);
    }

    #[test]
    fn built_record_verifies_and_exposes_pairing_identity() {
        let fx = fixture();
        let verified = fx.record.verify_transcript(&expectations(&fx)).unwrap();
        assert_eq!(verified.pairing_id(), fx.transcript.pairing_id);
        assert_eq!(
            verified.transcript_digest(),
            fx.transcript.transcript_digest()
        );
        assert_eq!(verified.lookup_digest(), fx.transcript.lookup_digest);
        assert_eq!(verified.owner_device_id(), fx.owner.public_key());
        assert_eq!(verified.redeemer_device_id(), fx.redeemer.public_key());
    }

    #[test]
    fn tampered_transcript_is_rejected_by_digest() {
        let fx = fixture();
        let mut record = fx.record.clone();
        // Swap in a different pairing id without updating the digest.
        record.transcript.pairing_id = [0xAA; 16];
        assert_eq!(
            record.verify_transcript(&expectations(&fx)),
            Err(PairingVerifyError::TranscriptDigestMismatch)
        );
    }

    #[test]
    fn record_past_deadline_is_expired_and_boundary_inclusive() {
        let fx = fixture();
        let mut exp = expectations(&fx);
        exp.now_unix_ms = fx.transcript.deadline_ms;
        assert!(fx.record.verify_transcript(&exp).is_ok());
        exp.now_unix_ms = fx.transcript.deadline_ms + 1;
        assert_eq!(
            fx.record.verify_transcript(&exp),
            Err(PairingVerifyError::Expired)
        );
    }

    #[test]
    fn unexpected_nonce_is_rejected() {
        let fx = fixture();
        let wrong_owner = [0x01u8; 32];
        let wrong_redeemer = [0x02u8; 32];

        let mut exp = expectations(&fx);
        exp.expected_owner_nonce = &wrong_owner;
        assert_eq!(
            fx.record.verify_transcript(&exp),
            Err(PairingVerifyError::NonceMismatch)
        );

        let mut exp = expectations(&fx);
        exp.expected_redeemer_nonce = &wrong_redeemer;
        assert_eq!(
            fx.record.verify_transcript(&exp),
            Err(PairingVerifyError::NonceMismatch)
        );
    }

    #[test]
    fn different_invite_code_is_rejected() {
        let fx = fixture();
        let foreign_key = InviteCode::generate().auth_key();
        let mut exp = expectations(&fx);
        exp.auth_key = &foreign_key;
        assert_eq!(
            fx.record.verify_transcript(&exp),
            Err(PairingVerifyError::InviteCodeMismatch)
        );
    }

    #[test]
    fn substituted_owner_key_is_rejected() {
        let fx = fixture();
        // Malicious coordination service substitutes the owner key in the
        // transcript; to reach the signature check the digest and both
        // MACs are recomputed over the tampered transcript (an insider
        // holding the code secret can do that), but it cannot forge the
        // original owner key's signature.
        let mut transcript = fx.transcript;
        transcript.owner_device_id = DeviceKeyPair::generate().public_key();
        let bytes = transcript.to_bytes();
        let owner_mac = proof_mac(&fx.auth_key, OWNER_PROOF_DOMAIN, &bytes);
        let redeemer_mac = proof_mac(&fx.auth_key, REDEEMER_PROOF_DOMAIN, &bytes);
        let record = PairingRecord::from_pieces(
            transcript,
            transcript.transcript_digest(),
            PairProofData {
                mac: owner_mac,
                signature: fx.record.owner_proof.signature,
            },
            PairProofData {
                mac: redeemer_mac,
                signature: fx.redeemer.sign(&signed_message(
                    REDEEMER_PROOF_DOMAIN,
                    &bytes,
                    &redeemer_mac,
                )),
            },
        );
        assert_eq!(
            record.verify_transcript(&expectations(&fx)),
            Err(PairingVerifyError::PublicKeyMismatch)
        );
    }

    #[test]
    fn role_reflected_proof_is_rejected() {
        let fx = fixture();
        // The redeemer key signs the owner proof (role reflection): MAC is
        // correct, signature is valid Ed25519, but under the wrong key.
        let bytes = fx.transcript.to_bytes();
        let reflected = PairingRecord::from_pieces(
            fx.transcript,
            fx.transcript.transcript_digest(),
            PairProofData {
                mac: fx.record.owner_proof.mac,
                signature: fx.redeemer.sign(&signed_message(
                    OWNER_PROOF_DOMAIN,
                    &bytes,
                    &fx.record.owner_proof.mac,
                )),
            },
            fx.record.redeemer_proof,
        );
        assert_eq!(
            reflected.verify_transcript(&expectations(&fx)),
            Err(PairingVerifyError::PublicKeyMismatch)
        );
    }

    #[test]
    fn verified_pairing_exposes_peer_fingerprints() {
        let fx = fixture();
        let verified = fx.record.verify_transcript(&expectations(&fx)).unwrap();
        assert_eq!(
            verified.owner_fingerprint(),
            Fingerprint::from_public_key(&fx.owner.public_key())
        );
        assert_eq!(
            verified.redeemer_fingerprint(),
            Fingerprint::from_public_key(&fx.redeemer.public_key())
        );
    }

    #[test]
    fn random_helpers_produce_full_length_unique_values() {
        let a = random_nonce32();
        let b = random_nonce32();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);

        let id_a = random_id16();
        let id_b = random_id16();
        assert_eq!(id_a.len(), 16);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn verify_expectations_debug_redacts_the_auth_key() {
        // Fixed distinctive bytes: 0xAB = 171 decimal, 0x0B = 11 — none of
        // the nonce bytes (0x11/0x22) collide with the key bytes, so the
        // decimal-array form of the key cannot appear via another field.
        let auth_key = [0xABu8; 32];
        let owner_nonce = [0x11u8; 32];
        let redeemer_nonce = [0x22u8; 32];
        let exp = VerifyExpectations {
            auth_key: &auth_key,
            expected_owner_nonce: &owner_nonce,
            expected_redeemer_nonce: &redeemer_nonce,
            now_unix_ms: 42,
        };
        let rendered = format!("{exp:?}");
        assert!(rendered.contains("redacted"), "marks the field: {rendered}");
        assert!(!rendered.contains("171"), "key bytes leaked: {rendered}");
        let key_hex: String = auth_key.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!rendered.contains(&key_hex), "key hex leaked: {rendered}");
        // The public material stays visible for debugging.
        assert!(rendered.contains("now_unix_ms: 42"), "{rendered}");
        assert!(rendered.contains("expected_owner_nonce"), "{rendered}");
    }
}
