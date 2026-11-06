//! Inbound `TaskOpen` admission validation against local Ready relations
//! (wave 27, final-review P1-02; scheduler.md §6).
//!
//! The resident daemon accepts one direct connection per inbound task and
//! must decide *which scheduling group* that connection belongs to. The
//! sender's `TaskOpen` self-reports `(group, group_registry, pairing_id)`;
//! per contract §6 ("不能允许客户端任意填写陌生 `code_id` 获得另一条队列")
//! those bytes may only ever select a group that a **locally stored, Ready
//! pairing relation** already pins. [`AdmissionValidator::resolve`] is that
//! check: it ignores the self-reported group for scheduling purposes and
//! returns a [`CodeGroupKey`] rebuilt from the relation's own record.
//!
//! Fact source (chosen after reading `crate::pairing`): the `ak-store`
//! [`PairingJournal`](ak_store::PairingJournal) — the
//! `<id_dir>/pairing/<pairing>.akpair` records the orchestrator writes. It
//! is the only local store that binds all three identity dimensions of a
//! relation together:
//!
//! * `pairing_id` keys the record (file name and first field);
//! * `code_id` is an immutable identity field of the record;
//! * `peer_public_key` (the peer's Ed25519 device key) is pinned once and
//!   derives the peer fingerprint exactly as the orchestrator does
//!   (`Fingerprint::from_public_key`);
//! * `phase == Applied` is precisely "the finished relation was written
//!   into the local trust book" — the local meaning of `PairReady`.
//!
//! The alternatives cannot express the relation: `peers.book` is keyed by
//! fingerprint only (no code, no pairing granularity) and its
//! `<book>.pairings` sidecar maps `pairing_id → fingerprint` with no code
//! or registry identity. The registry dimension is not in the journal
//! record (a device talks to exactly one pinned registry per identity
//! directory), so the validator carries the expected [`RegistryId`] — the
//! same identity `RuntimeConfig::registry_id` holds — and a
//! `group_registry` that differs from it is a group-domain mismatch.
//!
//! Wire-form convention: `TaskOpen.group[32]` is the pairing `code_id[16]`
//! in its 32-byte form. This module freezes that form as
//! [`code_group_wire_form`] (BLAKE3 of the 16 code bytes — the same
//! derivation family `Fingerprint::from_public_key` uses for device keys).
//! The sender-side daemon builds its `AdmissionCtx::group` with this exact
//! function, and `resolve` accepts a claimed group only when it equals the
//! wire form of the *relation's* `code_id`.
use std::path::Path;

use ak_store::{PairingJournal, PairingPhase};
use thiserror::Error;

use crate::runtime::transport::{PairingId, PeerFingerprint};
use crate::scheduler::key::{CodeGroupKey, CodeId, RegistryId};

/// Bound an error detail before embedding it in a reject (same policy as
/// the runtime error taxonomy).
fn sanitize(detail: &str) -> String {
    let mut out: String = detail
        .chars()
        .take(120)
        .map(|c| match c as u32 {
            0x00..=0x1F | 0x7F..=0x9F => '?',
            _ => c,
        })
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// The `code_id[16]` in its wire 32-byte form (`TaskOpen.group`): the
/// BLAKE3 digest of the code id. Frozen wave-27 convention shared by the
/// sender side (which fills `AdmissionCtx::group` with it) and
/// [`AdmissionValidator::resolve`] (which only accepts a claimed group
/// equal to the wire form of the relation's own `code_id`).
pub fn code_group_wire_form(code_id: &[u8; 16]) -> [u8; 32] {
    *blake3::hash(code_id).as_bytes()
}

/// The identity tuple a wire `TaskOpen` carries (scheduler.md §6), as seen
/// by the receiving daemon after
/// `ak_engine::channel_scheduler_handshake` decoded it.
///
/// Plain data on purpose: the validator checks relations, not engine
/// types, so this half of the module stays compilable and testable
/// independently of the wave-27 engine admission entries. The engine-side
/// `TaskOpenAdmission` converts into it in
/// [`crate::runtime::channel_executor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskOpenClaim {
    /// The sender's scheduler task identity (`job_id[16]`).
    pub job_id: [u8; 16],
    /// The sender's tentative permit (`permit_id[16]`).
    pub permit_id: [u8; 16],
    /// Claimed code group (`group[32]`, the `code_id` wire form).
    pub group: [u8; 32],
    /// Claimed owning registry (`group_registry[32]`).
    pub group_registry: [u8; 32],
    /// Claimed pairing relation (`pairing_id[16]`).
    pub pairing_id: [u8; 16],
    /// Direction tag (0 = the opener sends). Enforced by the engine
    /// handshake before a claim can exist; carried for diagnostics.
    pub direction: u8,
}

impl TaskOpenClaim {
    /// Assemble a claim from raw wire fields (the shape the engine entry
    /// returns); no validation happens here —
    /// [`AdmissionValidator::resolve`](AdmissionValidator::resolve) is the
    /// validating step.
    pub fn new(
        job_id: [u8; 16],
        permit_id: [u8; 16],
        group: [u8; 32],
        group_registry: [u8; 32],
        pairing_id: [u8; 16],
        direction: u8,
    ) -> Self {
        TaskOpenClaim {
            job_id,
            permit_id,
            group,
            group_registry,
            pairing_id,
            direction,
        }
    }
}

/// Why an inbound `TaskOpen` was refused (contract §6 step 2).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdmissionReject {
    /// A wire identity was the all-zero sentinel, which can never name a
    /// real task, permit, code group, registry or relation. The field
    /// names the offender.
    #[error("zero identity in field '{field}'")]
    ZeroIdentity {
        /// The offending wire field.
        field: &'static str,
    },
    /// No local pairing record exists for the claimed `pairing_id`.
    #[error("no local pairing relation for the claimed pairing id")]
    UnknownRelation,
    /// A record exists but its relation has not reached the local Ready
    /// state (`Applied` with the peer key pinned) — a crashed or
    /// still-running pairing transaction must not admit transfers.
    #[error("pairing relation exists but is not Ready")]
    NotReady,
    /// The channel's verified peer fingerprint is not the fingerprint the
    /// relation pinned (wrong device using a stolen pairing id).
    #[error("peer fingerprint does not match the pairing relation")]
    FingerprintMismatch,
    /// The claimed group (code or registry half) is not the group the
    /// relation belongs to — the contract §6 "stranger `code_id`" case:
    /// a client may not self-report its way into another queue.
    #[error("claimed group does not match the pairing relation")]
    GroupMismatch,
    /// The local pairing store could not be read. Fail closed: an
    /// unreadable store admits nothing (the detail is bounded and
    /// sanitized).
    #[error("pairing store unavailable: {detail}")]
    Store {
        /// Bounded sanitized I/O detail.
        detail: String,
    },
}

/// A validated inbound admission: the scheduling group and relation the
/// daemon binds the accepted connection to. Every byte comes from the
/// local relation record (plus the constructor registry), never from the
/// claim beyond the identity match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAdmission {
    /// `(registry, code_id)` of the validated relation — the only group
    /// key the connection may run under.
    pub group: CodeGroupKey,
    /// The validated pairing relation.
    pub pairing_id: PairingId,
    /// The channel's verified peer (echoed for logging/spec labels).
    pub peer: PeerFingerprint,
}

/// Validates inbound `TaskOpen` claims against the local pairing journal
/// (see the module docs for the fact-source choice).
///
/// One validator per daemon/identity directory;
/// [`AdmissionValidator::resolve`](Self::resolve) is synchronous
/// (a bounded directory read of one small record file) and may be called
/// from the accept loop before the connection is bound into a
/// [`TransferSpec`](crate::runtime::TransferSpec).
pub struct AdmissionValidator {
    journal: PairingJournal,
    registry_id: RegistryId,
}

impl AdmissionValidator {
    /// Open the validator over the identity directory's pairing journal
    /// (`<id_dir>/pairing`, the same directory
    /// [`PairingOrchestrator`](crate::pairing::PairingOrchestrator) owns)
    /// and the pinned registry identity every relation of this device
    /// lives under.
    pub fn open(id_dir: &Path, registry_id: RegistryId) -> std::io::Result<Self> {
        Ok(Self::over_journal(
            PairingJournal::open(&id_dir.join(crate::pairing::JOURNAL_SUBDIR))?,
            registry_id,
        ))
    }

    /// Build the validator over an already-open journal.
    pub fn over_journal(journal: PairingJournal, registry_id: RegistryId) -> Self {
        AdmissionValidator {
            journal,
            registry_id,
        }
    }

    /// Validate one inbound claim. On success the returned
    /// [`ResolvedAdmission`] carries the group key rebuilt from the local
    /// relation record — the claimed group bytes are only ever *compared*,
    /// never adopted (contract §6).
    pub fn resolve(
        &self,
        peer: &PeerFingerprint,
        claim: &TaskOpenClaim,
    ) -> Result<ResolvedAdmission, AdmissionReject> {
        // Sentinel pass first (contract §6 step 2): the all-zero values are
        // reserved and can never name a real task/permit/group/registry or
        // relation, whatever the journal would say.
        for (field, zero) in [
            ("pairing_id", claim.pairing_id == [0u8; 16]),
            ("group", claim.group == [0u8; 32]),
            ("group_registry", claim.group_registry == [0u8; 32]),
            ("job_id", claim.job_id == [0u8; 16]),
            ("permit_id", claim.permit_id == [0u8; 16]),
        ] {
            if zero {
                return Err(AdmissionReject::ZeroIdentity { field });
            }
        }

        let record = match self.journal.load(&claim.pairing_id) {
            // Fail closed on an unreadable store: an I/O error must not be
            // silently read as "no relation".
            Ok(Some(record)) => record,
            Ok(None) => return Err(AdmissionReject::UnknownRelation),
            Err(err) => {
                return Err(AdmissionReject::Store {
                    detail: sanitize(&err.to_string()),
                })
            }
        };
        if record.phase != PairingPhase::Applied {
            return Err(AdmissionReject::NotReady);
        }
        let peer_key = match record.peer_public_key {
            Some(key) => key,
            // Applied without a pinned peer key cannot happen through the
            // orchestrator (both flows pin before commit); treat it as
            // unfinished rather than guess a peer.
            None => return Err(AdmissionReject::NotReady),
        };
        let pinned = ak_identity::Fingerprint::from_public_key(&peer_key);
        if pinned.as_bytes() != peer.as_bytes() {
            return Err(AdmissionReject::FingerprintMismatch);
        }
        // The relation's own code is the group; the claim must agree with
        // it byte-for-byte in the frozen wire form. The all-zero code id is
        // additionally the daemon's anonymous-Local sentinel and can never
        // key a real relation group.
        if record.code_id == [0u8; 16]
            || claim.group != code_group_wire_form(&record.code_id)
            || claim.group_registry != *self.registry_id.as_bytes()
        {
            return Err(AdmissionReject::GroupMismatch);
        }
        Ok(ResolvedAdmission {
            group: CodeGroupKey::new(self.registry_id, CodeId::from_bytes(record.code_id)),
            pairing_id: PairingId::from_bytes(record.pairing_id),
            peer: *peer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_store::PendingPairing;

    /// A journal under a temp dir plus the validator over it.
    fn validator(registry: [u8; 32]) -> (tempfile::TempDir, AdmissionValidator) {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_dir = dir.path().join(crate::pairing::JOURNAL_SUBDIR);
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        let validator = AdmissionValidator::over_journal(
            PairingJournal::open(&journal_dir).expect("journal opens"),
            RegistryId::from_bytes(registry),
        );
        (dir, validator)
    }

    /// Seed one relation record in the given phase, returning the pairing
    /// id used.
    fn seed_relation(
        dir: &Path,
        pairing_id: [u8; 16],
        code_id: [u8; 16],
        peer_key: [u8; 32],
        applied: bool,
    ) {
        let journal =
            PairingJournal::open(&dir.join(crate::pairing::JOURNAL_SUBDIR)).expect("journal");
        journal
            .record_pending(&PendingPairing {
                pairing_id,
                code_id,
                lookup_digest: [0x44; 32],
                secret_ref: [0x55; 16],
                peer_public_key: Some(peer_key),
            })
            .expect("pending recorded");
        if applied {
            journal.mark_committed(&pairing_id).expect("committed");
            journal.mark_applied(&pairing_id).expect("applied");
        }
    }

    fn applied_validator_with_relation() -> (
        tempfile::TempDir,
        AdmissionValidator,
        [u8; 16],
        [u8; 16],
        PeerFingerprint,
    ) {
        let registry = [0x11; 32];
        let (dir, validator) = validator(registry);
        let key = ak_identity::DeviceKeyPair::generate();
        let peer_fp = PeerFingerprint::from_bytes(*key.fingerprint().as_bytes());
        let pairing_id = [0xA1; 16];
        let code_id = [0xC3; 16];
        seed_relation(dir.path(), pairing_id, code_id, key.public_key(), true);
        (dir, validator, pairing_id, code_id, peer_fp)
    }

    /// The claim a legitimate sender of this relation would put on the
    /// wire (group in the frozen wire form, registry = the local one).
    fn matching_claim(
        pairing_id: [u8; 16],
        code_id: [u8; 16],
        registry: [u8; 32],
    ) -> TaskOpenClaim {
        TaskOpenClaim::new(
            [0x42; 16],
            [0x5C; 16],
            code_group_wire_form(&code_id),
            registry,
            pairing_id,
            0,
        )
    }

    #[test]
    fn resolve_accepts_an_applied_relation_and_rebuilds_the_group() {
        let (_dir, validator, pairing_id, code_id, peer_fp) =
            applied_validator_with_relation();
        let claim = matching_claim(pairing_id, code_id, [0x11; 32]);
        let resolved = validator
            .resolve(&peer_fp, &claim)
            .expect("an applied relation with matching identities admits");
        // The group is rebuilt from the record, not adopted from the claim:
        // same bytes here, but the source is the relation.
        assert_eq!(
            resolved.group,
            CodeGroupKey::new(RegistryId::from_bytes([0x11; 32]), CodeId::from_bytes(code_id))
        );
        assert_eq!(resolved.pairing_id, PairingId::from_bytes(pairing_id));
        assert_eq!(resolved.peer, peer_fp);
    }

    #[test]
    fn resolve_rejects_every_zero_identity_field() {
        let (_dir, validator, pairing_id, code_id, peer_fp) =
            applied_validator_with_relation();
        let base = matching_claim(pairing_id, code_id, [0x11; 32]);
        for (claim, field) in [
            (
                TaskOpenClaim {
                    pairing_id: [0; 16],
                    ..base
                },
                "pairing_id",
            ),
            (
                TaskOpenClaim {
                    group: [0; 32],
                    ..base
                },
                "group",
            ),
            (
                TaskOpenClaim {
                    group_registry: [0; 32],
                    ..base
                },
                "group_registry",
            ),
            (
                TaskOpenClaim {
                    job_id: [0; 16],
                    ..base
                },
                "job_id",
            ),
            (
                TaskOpenClaim {
                    permit_id: [0; 16],
                    ..base
                },
                "permit_id",
            ),
        ] {
            match validator.resolve(&peer_fp, &claim) {
                Err(AdmissionReject::ZeroIdentity { field: seen }) => {
                    assert_eq!(seen, field);
                }
                other => panic!("expected ZeroIdentity({field}), got {other:?}"),
            }
        }
    }

    #[test]
    fn resolve_rejects_an_unknown_relation() {
        let (_dir, validator, _pairing_id, code_id, peer_fp) =
            applied_validator_with_relation();
        // A pairing id with no record at all.
        let claim = matching_claim([0x99; 16], code_id, [0x11; 32]);
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::UnknownRelation
        );
    }

    #[test]
    fn resolve_rejects_relations_that_are_not_ready() {
        let registry = [0x11; 32];
        let (dir, validator) = validator(registry);
        let key = ak_identity::DeviceKeyPair::generate();
        let peer_fp = PeerFingerprint::from_bytes(*key.fingerprint().as_bytes());
        // Pending (never committed): not Ready.
        let pending_id = [0x01; 16];
        seed_relation(dir.path(), pending_id, [0xC3; 16], key.public_key(), false);
        let claim = matching_claim(pending_id, [0xC3; 16], registry);
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::NotReady
        );
        // Committed but not applied: still not Ready locally.
        let journal = PairingJournal::open(&dir.path().join(crate::pairing::JOURNAL_SUBDIR))
            .expect("journal");
        journal.mark_committed(&pending_id).expect("committed");
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::NotReady
        );
    }

    #[test]
    fn resolve_rejects_a_peer_the_relation_never_pinned() {
        let (_dir, validator, pairing_id, code_id, _peer_fp) =
            applied_validator_with_relation();
        // A different device's channel using the stolen pairing id.
        let other = ak_identity::DeviceKeyPair::generate();
        let other_fp = PeerFingerprint::from_bytes(*other.fingerprint().as_bytes());
        let claim = matching_claim(pairing_id, code_id, [0x11; 32]);
        assert_eq!(
            validator.resolve(&other_fp, &claim).unwrap_err(),
            AdmissionReject::FingerprintMismatch
        );
    }

    /// Contract §6: a client must not self-report a stranger `code_id` (or
    /// a foreign registry) into a different queue. The claim below carries
    /// a *well-formed* group of another code — the exact attack — and is
    /// refused without ever yielding a group key.
    #[test]
    fn resolve_rejects_a_stranger_code_group() {
        let (_dir, validator, pairing_id, _code_id, peer_fp) =
            applied_validator_with_relation();
        let stranger_code = [0x77; 16];
        let claim = matching_claim(pairing_id, stranger_code, [0x11; 32]);
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::GroupMismatch
        );
    }

    #[test]
    fn resolve_rejects_a_foreign_registry() {
        let (_dir, validator, pairing_id, code_id, peer_fp) =
            applied_validator_with_relation();
        let claim = matching_claim(pairing_id, code_id, [0x22; 32]);
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::GroupMismatch
        );
    }

    /// The all-zero code id is the daemon's anonymous-Local sentinel; even
    /// a record that somehow carried it can never become a relation group.
    #[test]
    fn resolve_rejects_a_zero_code_id_record() {
        let registry = [0x11; 32];
        let (dir, validator) = validator(registry);
        let key = ak_identity::DeviceKeyPair::generate();
        let peer_fp = PeerFingerprint::from_bytes(*key.fingerprint().as_bytes());
        let pairing_id = [0x0D; 16];
        seed_relation(dir.path(), pairing_id, [0u8; 16], key.public_key(), true);
        let claim = matching_claim(pairing_id, [0u8; 16], registry);
        assert_eq!(
            validator.resolve(&peer_fp, &claim).unwrap_err(),
            AdmissionReject::GroupMismatch
        );
    }

    #[test]
    fn code_group_wire_form_is_stable_and_code_specific() {
        let a = code_group_wire_form(&[1; 16]);
        let b = code_group_wire_form(&[2; 16]);
        assert_eq!(a, code_group_wire_form(&[1; 16]), "deterministic");
        assert_ne!(a, b, "distinct per code");
        assert_ne!(a, [0u8; 32], "never the zero sentinel");
        // The frozen value: BLAKE3 of the 16 code bytes.
        assert_eq!(a, *blake3::hash(&[1u8; 16]).as_bytes());
    }

    /// The validator fails closed when the journal directory is unreadable
    /// garbage rather than silently treating it as "no relations".
    #[test]
    fn resolve_fails_closed_on_an_unreadable_store() {
        let registry = [0x11; 32];
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_dir = dir.path().join(crate::pairing::JOURNAL_SUBDIR);
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        // A file where the record would live, but with junk content.
        std::fs::write(journal_dir.join("01010101010101010101010101010101.akpair"), b"junk")
            .expect("seed junk record");
        let validator = AdmissionValidator::over_journal(
            PairingJournal::open(&journal_dir).expect("journal opens"),
            RegistryId::from_bytes(registry),
        );
        let claim = matching_claim([1; 16], [1; 16], registry);
        match validator.resolve(&PeerFingerprint::from_bytes([9; 32]), &claim) {
            Err(AdmissionReject::Store { .. }) => {}
            other => panic!("expected Store, got {other:?}"),
        }
    }
}
