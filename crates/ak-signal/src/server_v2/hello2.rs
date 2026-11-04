//! The frozen signature domains of the v2 control plane — the exact byte
//! layouts the server verifies and the client (A3's dispatcher) must sign.
//!
//! # Hello v2 (contract `pairing-flow.md` §3)
//!
//! ```text
//! signature = Ed25519.Sign(device_key,
//!     "AKHELLO2" || registry_id[32] || exporter[32] ||
//!     version:u16 || device_id[32] || name(u16 len + bytes) ||
//!     timestamp_ms:u64 || challenge[32])
//! ```
//!
//! `exporter` is the RFC 5705/RFC 8446 exporter value of the TLS
//! connection, 32 bytes, label [`EXPORTER_LABEL`], no context. rustls
//! 0.23 exposes `ConnectionCommon::export_keying_material`, so plan A.5's
//! exporter binding is implemented as specified (no fallback to
//! certificate-pinning-only). A Hello signature valid on one TLS
//! connection therefore never verifies on another — replay across
//! connections is impossible even before the replay cache.
//!
//! # Redeem request (contract §6: request MAC covers issuer, lookup, B
//! key, request id, B nonce, deadline)
//!
//! The `request_mac` is keyed by the invite `auth_key` and is verifiable
//! only by the endpoints. The server verifies the *signature* over the
//! same field set plus the MAC:
//!
//! ```text
//! "AK redeem request v1" || registry_id[32] || lookup_digest[32] ||
//! redeemer_device_id[32] || request_id[16] || redeemer_nonce[32] ||
//! deadline_ms:u64 || request_mac[32]
//! ```
//!
//! # Prepared / Applied confirmations
//!
//! ```text
//! PairPrepared.signature: "AK prepared v1" || pairing_id[16] || transcript_digest[32]
//! PairApplied.signature:  "AK applied v1"  || pairing_id[16] ||
//!                        transcript_digest[32] || commit_revision:u64
//! ```
//!
//! The owner/redeemer transcript proofs use ak-identity's frozen domains
//! (`ak_identity::pairing_txn::OWNER_PROOF_DOMAIN` /
//! `REDEEMER_PROOF_DOMAIN`) — see [`proof_signed_message`].
//!
//! All integers are big-endian; fixed arrays carry no length prefix;
//! strings use the wire convention (u16 byte length + UTF-8). None of
//! these messages carry secrets: lookup digests and MACs are already
//! server-visible material, and `auth_key` never appears.

use std::io;

use ak_identity::pairing_txn::{
    DeviceId, Digest32, Nonce32, PairingId, RegistryId, RequestId, Signature64,
};
use ak_identity::DeviceKeyPair;

use crate::protocol_v2::MAX_NAME_BYTES;

/// Hello v2 prefix (contract §3, verbatim).
pub const HELLO2_PREFIX: &[u8; 8] = b"AKHELLO2";
/// TLS exporter label for the Hello-v2 binding (A2/A3 shared constant).
pub const EXPORTER_LABEL: &[u8; 25] = b"AK control hello exporter";
/// Exporter output length.
pub const EXPORTER_BYTES: usize = 32;
/// Redeem-request signature domain.
pub const REDEEM_DOMAIN: &[u8; 20] = b"AK redeem request v1";
/// `PairPrepared.signature` domain.
pub const PREPARED_DOMAIN: &[u8; 14] = b"AK prepared v1";
/// `PairApplied.signature` domain.
pub const APPLIED_DOMAIN: &[u8; 13] = b"AK applied v1";

/// TLS exporter secret for one connection: 32 bytes, label
/// [`EXPORTER_LABEL`], no context. Both endpoints of the same TLS
/// connection derive the same value; no other party can (rustls 0.23
/// exposes `export_keying_material` on `ConnectionCommon` — plan A.5 as
/// specified, no certificate-only fallback).
pub fn exporter_secret_server(conn: &rustls::ServerConnection) -> io::Result<[u8; EXPORTER_BYTES]> {
    let mut out = [0u8; EXPORTER_BYTES];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, None)
        .map_err(|e| io::Error::other(format!("exporter: {e}")))?;
    Ok(out)
}

/// Client-side twin of [`exporter_secret_server`] (same label, same
/// length): the client signs its Hello v2 over exactly this value.
pub fn exporter_secret_client(conn: &rustls::ClientConnection) -> io::Result<[u8; EXPORTER_BYTES]> {
    let mut out = [0u8; EXPORTER_BYTES];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, None)
        .map_err(|e| io::Error::other(format!("exporter: {e}")))?;
    Ok(out)
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    // Mirrors the wire encoding; callers pass codec-validated names, and
    // the cap is re-checked so a buggy caller cannot build an unbounded
    // message silently.
    debug_assert!(s.len() <= MAX_NAME_BYTES);
    let len = s.len().min(u16::MAX as usize);
    put_u16(out, len as u16);
    out.extend_from_slice(&s.as_bytes()[..len]);
}

/// The signed message behind Hello v2 (see the module docs for the frozen
/// layout). `name` is the wire-validated device name.
#[allow(clippy::too_many_arguments)]
pub fn hello2_message(
    registry_id: &RegistryId,
    exporter: &[u8; EXPORTER_BYTES],
    version: u16,
    device_id: &DeviceId,
    name: &str,
    timestamp_ms: u64,
    challenge: &Nonce32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 32 + 32 + 2 + 32 + 2 + MAX_NAME_BYTES + 8 + 32);
    out.extend_from_slice(HELLO2_PREFIX);
    out.extend_from_slice(registry_id);
    out.extend_from_slice(exporter);
    put_u16(&mut out, version);
    out.extend_from_slice(device_id);
    put_str(&mut out, name);
    put_u64(&mut out, timestamp_ms);
    out.extend_from_slice(challenge);
    out
}

/// The signed message behind `InviteRedeem.signature` (module docs).
pub fn redeem_message(
    registry_id: &RegistryId,
    lookup_digest: &Digest32,
    redeemer_device_id: &DeviceId,
    request_id: &RequestId,
    redeemer_nonce: &Nonce32,
    deadline_ms: u64,
    request_mac: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + 32 + 32 + 32 + 16 + 32 + 8 + 32);
    out.extend_from_slice(REDEEM_DOMAIN);
    out.extend_from_slice(registry_id);
    out.extend_from_slice(lookup_digest);
    out.extend_from_slice(redeemer_device_id);
    out.extend_from_slice(request_id);
    out.extend_from_slice(redeemer_nonce);
    put_u64(&mut out, deadline_ms);
    out.extend_from_slice(request_mac);
    out
}

/// The signed message behind `PairPrepared.signature`.
pub fn prepared_message(pairing_id: &PairingId, transcript_digest: &Digest32) -> Vec<u8> {
    let mut out = Vec::with_capacity(15 + 16 + 32);
    out.extend_from_slice(PREPARED_DOMAIN);
    out.extend_from_slice(pairing_id);
    out.extend_from_slice(transcript_digest);
    out
}

/// The signed message behind `PairApplied.signature`.
pub fn applied_message(
    pairing_id: &PairingId,
    transcript_digest: &Digest32,
    commit_revision: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + 16 + 32 + 8);
    out.extend_from_slice(APPLIED_DOMAIN);
    out.extend_from_slice(pairing_id);
    out.extend_from_slice(transcript_digest);
    put_u64(&mut out, commit_revision);
    out
}

/// The signed message behind an endpoint transcript proof
/// (`ak-identity` frozen role domains):
/// `domain || canonical transcript[256] || mac[32]`.
pub fn proof_signed_message(domain: &str, transcript: &[u8; 256], mac: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(domain.len() + 256 + 32);
    out.extend_from_slice(domain.as_bytes());
    out.extend_from_slice(transcript);
    out.extend_from_slice(mac);
    out
}

/// `BLAKE3(canonical transcript bytes)` — the `transcript_digest` carried
/// by `PairProof`/`PairPrepared`/`PairCommit`/`PairApplied`/`PairReady`.
/// Computed via ak-identity's frozen implementation so the server and
/// the endpoints can never disagree.
pub fn transcript_digest(transcript: &[u8; 256]) -> Digest32 {
    ak_identity::pairing_txn::PairTranscript::from_bytes(transcript).transcript_digest()
}

/// Verify one Ed25519 signature over a server-constructed message. All
/// verification on this plane goes through here so there is exactly one
/// place that decides what "verified" means.
pub fn verify(device_id: &DeviceId, message: &[u8], signature: &Signature64) -> bool {
    DeviceKeyPair::verify(device_id, message, signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_identity::pairing_txn::OWNER_PROOF_DOMAIN;

    fn key() -> DeviceKeyPair {
        DeviceKeyPair::generate()
    }

    #[test]
    fn hello2_message_layout_is_frozen() {
        let registry = [0x11u8; 32];
        let exporter = [0x22u8; 32];
        let device = [0x33u8; 32];
        let challenge = [0x44u8; 32];
        let message = hello2_message(
            &registry, &exporter, 2, &device, "laptop", 7_000, &challenge,
        );

        // "AKHELLO2" || registry || exporter || version || device ||
        // name(u16+bytes) || timestamp || challenge — exact offsets.
        assert_eq!(&message[0..8], b"AKHELLO2");
        assert_eq!(&message[8..40], &registry);
        assert_eq!(&message[40..72], &exporter);
        assert_eq!(&message[72..74], &2u16.to_be_bytes());
        assert_eq!(&message[74..106], &device);
        assert_eq!(&message[106..108], &6u16.to_be_bytes());
        assert_eq!(&message[108..114], b"laptop");
        assert_eq!(&message[114..122], &7_000u64.to_be_bytes());
        assert_eq!(&message[122..154], &challenge);
        assert_eq!(message.len(), 154);
    }

    #[test]
    fn hello2_signature_verifies_and_tampering_fails() {
        let device = key();
        let registry = [0x11u8; 32];
        let exporter = [0x22u8; 32];
        let challenge = [0x44u8; 32];
        let id = device.public_key();
        let message = hello2_message(&registry, &exporter, 2, &id, "a", 5, &challenge);
        let signature = device.sign(&message);

        assert!(verify(&id, &message, &signature));
        // Different exporter (another TLS connection) invalidates it.
        let other_exporter = [0x99u8; 32];
        let other = hello2_message(&registry, &other_exporter, 2, &id, "a", 5, &challenge);
        assert!(!verify(&id, &other, &signature));
        // Any field flip invalidates it.
        let tampered = hello2_message(&registry, &exporter, 3, &id, "a", 5, &challenge);
        assert!(!verify(&id, &tampered, &signature));
        let tampered = hello2_message(&registry, &exporter, 2, &id, "a", 6, &challenge);
        assert!(!verify(&id, &tampered, &signature));
        // A different registry id invalidates it (wrong server).
        let wrong_registry = [0x00u8; 32];
        let tampered = hello2_message(&wrong_registry, &exporter, 2, &id, "a", 5, &challenge);
        assert!(!verify(&id, &tampered, &signature));
        // A different challenge nonce invalidates it (stale AuthChallenge).
        let tampered = hello2_message(&registry, &exporter, 2, &id, "a", 5, &[0u8; 32]);
        assert!(!verify(&id, &tampered, &signature));
    }

    #[test]
    fn redeem_message_covers_every_anti_replay_field() {
        let registry = [0x11u8; 32];
        let lookup = [0x22u8; 32];
        let redeemer = [0x33u8; 32];
        let request = [0x44u8; 16];
        let nonce = [0x55u8; 32];
        let mac = [0x66u8; 32];
        let message = redeem_message(&registry, &lookup, &redeemer, &request, &nonce, 9, &mac);

        assert_eq!(&message[0..20], b"AK redeem request v1");
        assert_eq!(&message[20..52], &registry);
        assert_eq!(&message[52..84], &lookup);
        assert_eq!(&message[84..116], &redeemer);
        assert_eq!(&message[116..132], &request);
        assert_eq!(&message[132..164], &nonce);
        assert_eq!(&message[164..172], &9u64.to_be_bytes());
        assert_eq!(&message[172..204], &mac);
        assert_eq!(message.len(), 204);

        // Flipping the deadline or lookup must change the bytes signed.
        let other = redeem_message(&registry, &lookup, &redeemer, &request, &nonce, 10, &mac);
        assert_ne!(message, other);
        let other = redeem_message(&registry, &[0u8; 32], &redeemer, &request, &nonce, 9, &mac);
        assert_ne!(message, other);
    }

    #[test]
    fn prepared_and_applied_messages_bind_ids_and_revision() {
        let pairing = [0x77u8; 16];
        let digest = [0x88u8; 32];
        let prepared = prepared_message(&pairing, &digest);
        assert_eq!(&prepared[0..14], b"AK prepared v1");
        assert_eq!(&prepared[14..30], &pairing);
        assert_eq!(&prepared[30..62], &digest);

        let applied = applied_message(&pairing, &digest, 4);
        assert_eq!(&applied[0..13], b"AK applied v1");
        assert_eq!(&applied[13..29], &pairing);
        assert_eq!(&applied[29..61], &digest);
        assert_eq!(&applied[61..69], &4u64.to_be_bytes());
        assert_ne!(applied_message(&pairing, &digest, 5), applied);
    }

    #[test]
    fn proof_message_matches_ak_identity_domain_usage() {
        let transcript = [0xABu8; 256];
        let mac = [0xCDu8; 32];
        let message = proof_signed_message(OWNER_PROOF_DOMAIN, &transcript, &mac);
        assert_eq!(message.len(), OWNER_PROOF_DOMAIN.len() + 256 + 32);
        assert_eq!(
            &message[..OWNER_PROOF_DOMAIN.len()],
            OWNER_PROOF_DOMAIN.as_bytes()
        );
        assert_eq!(&message[OWNER_PROOF_DOMAIN.len()..][..256], &transcript);
        // ak-identity's verifier accepts a signature over exactly these
        // bytes for the same role domain.
        let owner = key();
        let signature = owner.sign(&message);
        assert!(verify(&owner.public_key(), &message, &signature));
    }
}
