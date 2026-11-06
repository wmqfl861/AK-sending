//! The authoritative invite registry: three SQLite tables plus a bounded
//! idempotency journal, per `docs/contracts/storage-schema.md` part 1
//! (`server-persistence` feature; rusqlite bundled, the workspace-locked
//! version, so clients that never enable this feature do not link a
//! server-side SQLite).
//!
//! # Tables
//!
//! `invite_codes` is the contract DDL verbatim (storage-schema.md §2):
//! the primary key on `lookup_digest` **is** the claim linearization —
//! concurrent claims of one code collide on the unique index inside one
//! `BEGIN IMMEDIATE` transaction and exactly one wins
//! (`pairing-flow.md` §5). Tombstones are permanent: rows are never
//! deleted, `expired`/`revoked` never return to `active`, and a revoked
//! or expired code is never re-issued.
//!
//! `pairing_transactions` stores the fixed 256-byte transcript, both
//! endpoint proofs and the Prepared/Commit/Applied lifecycle
//! (storage-schema.md §3). The commit path re-checks the code state
//! inside the same transaction that writes `committed`, so a revoke
//! either linearizes before (pairing aborts) or after (pairing is kept)
//! — never both.
//!
//! `pairing_relations` records completed pairings (owner/redeemer keys,
//! `code_id`, `pairing_id`) under a per-owner storage quota
//! (storage-schema.md §4).
//!
//! `request_journal` is the bounded idempotency record (§5): same owner +
//! `request_id` + content replays the original response frame; same id
//! with different content is `REQUEST_CONFLICT`. Entries are pruned by
//! age and count on every insert.
//!
//! The store never sees plaintext invite codes, `auth_key`s, MAC keys,
//! file paths, or any payload data.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::protocol_v2::{
    CodeId, CtrlFrame, CtrlMessage, DeviceId, Digest32, InviteState, InviteSummary, PairProof,
    PairingId, PairingState, RedeemRequest, RequestId, Role, Signature64, StateFilter,
};
use crate::server_v2::config::{
    INVITE_CLAIMS_PER_HOUR, INVITE_TTL_MAX_SECS, INVITE_TTL_MIN_SECS, JOURNAL_MAX_ROWS,
    JOURNAL_RETENTION, MAX_ACTIVE_INVITES_PER_OWNER, MAX_PENDING_REDEEM_GLOBAL,
    MAX_PENDING_REDEEM_PER_OWNER, MAX_RELATIONS_PER_OWNER,
};

/// Registry schema version (`PRAGMA user_version`). Bump only with a
/// migration that preserves tombstones and uniqueness records.
///
/// Wave-28 bumped 1 → 2: `invite_codes` gained `owner_endpoint BLOB NULL`
/// and `max_redeems INTEGER NOT NULL DEFAULT 0`; `pairing_transactions`
/// gained `owner_endpoint BLOB NULL` and `redeemer_endpoint BLOB NULL`
/// (both snapshotted at redeem time). The v1→v2 migration is additive
/// `ALTER TABLE ... ADD COLUMN` statements — tombstones, uniqueness
/// records and every pre-existing row survive verbatim — and runs as one
/// atomic, re-entrant transaction (see [`migrate_v1_to_v2`]).
pub const SCHEMA_VERSION: i64 = 2;

/// Why a store operation failed at the storage layer. Business outcomes
/// are typed decisions, not errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("registry storage error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("registry schema version {0} is newer than this build supports")]
    SchemaVersion(i64),
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn blob<const N: usize>(value: Option<Vec<u8>>) -> Option<[u8; N]> {
    value.and_then(|v| <[u8; N]>::try_from(v.as_slice()).ok())
}

fn blob_req<const N: usize>(value: Vec<u8>) -> rusqlite::Result<[u8; N]> {
    <[u8; N]>::try_from(value.as_slice()).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            N,
            rusqlite::types::Type::Blob,
            format!("column blob is {} bytes, expected {N}", value.len()).into(),
        )
    })
}

// ---- DDL -----------------------------------------------------------------

/// `invite_codes` — contract DDL (storage-schema.md §2) plus the wave-28
/// columns `owner_endpoint` / `max_redeems`. SQLite reads `BLOB(32)` as
/// BLOB affinity; length is checked app-side (endpoint blobs are bounded
/// by the codec at 512 bytes). No plaintext code, no `auth_key`, no MAC
/// material — ever.
const DDL_INVITE_CODES: &str = "
CREATE TABLE IF NOT EXISTS invite_codes (
  lookup_digest       BLOB(32) PRIMARY KEY,
  code_id             BLOB(16) UNIQUE NOT NULL,
  owner_device_id     BLOB(32) NOT NULL,
  created_unix_ms     INTEGER NOT NULL,
  expires_unix_ms     INTEGER NULL,
  state               TEXT NOT NULL CHECK (state IN ('active','revoked','expired')),
  revision            INTEGER NOT NULL,
  create_request_id   BLOB(16) NOT NULL,
  owner_endpoint      BLOB NULL,
  max_redeems         INTEGER NOT NULL DEFAULT 0,
  UNIQUE (owner_device_id, create_request_id)
);";

/// `pairing_transactions` — fixed transcript, both proofs, and the
/// Prepared/Commit/Applied lifecycle (storage-schema.md §3), plus the
/// wave-28 endpoint columns: `owner_endpoint` is snapshotted from the
/// code row and `redeemer_endpoint` from the redeem frame when the
/// transaction begins, so the commit/offer paths re-serve them verbatim.
/// Proof columns fill in as endpoint messages arrive; `commit_revision`
/// is the code revision at commit time (== the transcript's
/// `code_revision` binding).
const DDL_PAIRING_TRANSACTIONS: &str = "
CREATE TABLE IF NOT EXISTS pairing_transactions (
  pairing_id                  BLOB(16) PRIMARY KEY,
  request_id                  BLOB(16) NOT NULL,
  code_id                     BLOB(16) NOT NULL,
  lookup_digest               BLOB(32) NOT NULL,
  owner_device_id             BLOB(32) NOT NULL,
  redeemer_device_id          BLOB(32) NOT NULL,
  state                       TEXT NOT NULL CHECK (state IN ('pending','prepared','committed','applied','ready','aborted')),
  transcript                  BLOB(256),
  transcript_digest           BLOB(32),
  owner_proof_mac             BLOB(32),
  owner_proof_signature       BLOB(64),
  redeemer_proof_mac          BLOB(32),
  redeemer_proof_signature    BLOB(64),
  owner_prepared_signature    BLOB(64),
  redeemer_prepared_signature BLOB(64),
  commit_revision             INTEGER,
  owner_applied_signature     BLOB(64),
  redeemer_applied_signature  BLOB(64),
  redeemer_nonce              BLOB(32) NOT NULL,
  request_mac                 BLOB(32) NOT NULL,
  redeemer_signature          BLOB(64) NOT NULL,
  deadline_unix_ms            INTEGER NOT NULL,
  code_revision               INTEGER NOT NULL,
  owner_endpoint              BLOB NULL,
  redeemer_endpoint           BLOB NULL,
  created_unix_ms             INTEGER NOT NULL,
  updated_unix_ms             INTEGER NOT NULL
);";

/// `pairing_relations` — completed pairings (storage-schema.md §4). One
/// row per completed owner↔redeemer pairing under one code; the same
/// code may yield several relations (A↔B, A↔C), never redeemer↔redeemer.
const DDL_PAIRING_RELATIONS: &str = "
CREATE TABLE IF NOT EXISTS pairing_relations (
  pairing_id         BLOB(16) PRIMARY KEY,
  code_id            BLOB(16) NOT NULL,
  lookup_digest      BLOB(32) NOT NULL,
  owner_device_id    BLOB(32) NOT NULL,
  redeemer_device_id BLOB(32) NOT NULL,
  transcript_digest  BLOB(32) NOT NULL,
  commit_revision    INTEGER NOT NULL,
  completed_unix_ms  INTEGER NOT NULL
);";

/// `request_journal` — bounded idempotency records (storage-schema.md §5).
const DDL_REQUEST_JOURNAL: &str = "
CREATE TABLE IF NOT EXISTS request_journal (
  owner_device_id BLOB(32) NOT NULL,
  request_id      BLOB(16) NOT NULL,
  request_kind    TEXT NOT NULL CHECK (request_kind IN ('claim','revoke')),
  request_body    BLOB NOT NULL,
  response        BLOB NOT NULL,
  created_unix_ms INTEGER NOT NULL,
  PRIMARY KEY (owner_device_id, request_id)
);";

const DDL_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_invite_owner_state ON invite_codes(owner_device_id, state);
CREATE INDEX IF NOT EXISTS idx_pairing_owner_state ON pairing_transactions(owner_device_id, state);
CREATE INDEX IF NOT EXISTS idx_pairing_redeemer_state ON pairing_transactions(redeemer_device_id, state);
CREATE INDEX IF NOT EXISTS idx_pairing_code ON pairing_transactions(code_id);
CREATE INDEX IF NOT EXISTS idx_relations_owner ON pairing_relations(owner_device_id);";

// ---- outcomes -------------------------------------------------------------

/// Result of a claim transaction (`pairing-flow.md` §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Fresh registration committed.
    Claimed {
        code_id: CodeId,
        created_ms: u64,
        /// 0 = long-lived.
        expires_ms: u64,
        revision: u64,
    },
    /// Same request id replayed: the exact original response frame.
    Replay(Vec<u8>),
    /// Same owner + request id, different content.
    RequestConflict,
    /// Same lookup digest under a different registration request.
    CodeConflict,
    /// Owner exceeded the per-hour claim budget.
    RateLimited,
    /// Owner exceeded the active-invite quota.
    ResourceLimit,
}

/// Result of a revoke: success carries the aborted uncommitted pairings
/// so the engine can notify both endpoints (`pairing-flow.md` §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeOutcome {
    Revoked {
        revision: u64,
        revoked_ms: u64,
        aborted: Vec<AbortedPairing>,
    },
    /// Same request id replayed: the exact original response frame.
    Replay(Vec<u8>),
    RequestConflict,
    /// `expected_revision` mismatch (optimistic lock).
    RevisionConflict,
    /// Unknown code, someone else's code, or a terminal state — all
    /// indistinguishable (no existence oracle).
    Unavailable,
}

/// A pairing transaction aborted before commit; the engine notifies the
/// (possibly offline) participants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortedPairing {
    pub pairing_id: PairingId,
    pub owner: DeviceId,
    pub redeemer: DeviceId,
}

/// Raw column shape of the active-code lookup (`find_active_code`):
/// code_id, owner, revision, owner endpoint, redeem cap.
type ActiveCodeRow = (Vec<u8>, Vec<u8>, i64, Option<Vec<u8>>, i64);

/// A live, redeemable code as found by lookup. `owner_endpoint` /
/// `max_redeems` are the wave-28 columns (None / 0 for legacy claims).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCode {
    pub code_id: CodeId,
    pub owner: DeviceId,
    pub revision: u64,
    /// Owner connection settings claimed with the code (wave-28).
    pub owner_endpoint: Option<Vec<u8>>,
    /// 0 = unlimited (legacy default); ≥1 caps committed pairings.
    pub max_redeems: u32,
}

/// Result of registering a redeem attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginRedeem {
    Begun,
    OwnerPendingLimit,
    GlobalPendingLimit,
    /// Wave-28 single-redemption cap: the code already has
    /// `max_redeems` committed (or later) pairings.
    RedeemExhausted,
}

/// Everything the engine needs to re-serve one pending redeem offer to a
/// (re)connecting owner. `redeemer_endpoint` is the redeemer's wave-28
/// connection settings, re-attached to `InviteRedeemOffered`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOffer {
    pub pairing_id: PairingId,
    pub code_id: CodeId,
    pub code_revision: u64,
    pub redeemer: DeviceId,
    pub redeem: RedeemRequest,
    pub redeemer_endpoint: Option<Vec<u8>>,
}

/// Result of storing one endpoint's `PairPrepared` (with the commit
/// attempt folded in — both sides prepared ⇒ commit in the same tx).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedDecision {
    /// First side prepared; waiting for the other.
    WaitingOther,
    /// Commit persisted; `PairCommit` must be (re)sent.
    Committed { commit_revision: u64 },
    /// Transaction already reached a later state; resend the matching
    /// fixed message (PairCommit / PairReady).
    AlreadyFinal(PairingState),
    /// Code revoked/expired (or pairing aborted) — pairing failed.
    Unavailable,
    /// Wave-28 cap recheck at the commit linearization point (P1-03):
    /// competing pendings already filled `max_redeems`; this pairing was
    /// aborted inside the same transaction (aborted never consumes the
    /// cap) and both participants must get the explicit failure.
    RedeemExhausted,
    /// Owner hit the pairing-relation storage quota.
    ResourceLimit,
    /// Unknown pairing, wrong sender, or digest mismatch.
    NotFound,
}

/// Result of storing one endpoint's `PairApplied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedDecision {
    /// First side applied; waiting for the other.
    WaitingOther,
    /// Both applied; relation recorded; `PairReady` must be (re)sent.
    Ready,
    /// Replay after ready — resend `PairReady`.
    AlreadyReady,
    /// `commit_revision` mismatch.
    RevisionConflict,
    /// Unknown pairing, wrong state/sender, or digest mismatch.
    NotFound,
}

/// Full durable view of one pairing transaction. `owner_endpoint` /
/// `redeemer_endpoint` are the wave-28 connection settings snapshotted
/// at redeem time (served with `PairCommit` / `InviteRedeemOffered`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingSnapshot {
    pub pairing_id: PairingId,
    pub request_id: RequestId,
    pub code_id: CodeId,
    pub lookup_digest: Digest32,
    pub owner: DeviceId,
    pub redeemer: DeviceId,
    pub state: PairingState,
    pub deadline_ms: u64,
    pub code_revision: u64,
    pub commit_revision: Option<u64>,
    pub transcript: Option<[u8; 256]>,
    pub transcript_digest: Option<Digest32>,
    pub redeemer_nonce: [u8; 32],
    pub owner_proof: Option<PairProof>,
    pub redeemer_proof: Option<PairProof>,
    pub owner_prepared_signature: Option<Signature64>,
    pub redeemer_prepared_signature: Option<Signature64>,
    pub owner_applied_signature: Option<Signature64>,
    pub redeemer_applied_signature: Option<Signature64>,
    pub owner_endpoint: Option<Vec<u8>>,
    pub redeemer_endpoint: Option<Vec<u8>>,
}

/// Result of aborting a pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortDecision {
    Aborted(AbortedPairing),
    /// Committed transactions must be recovered, not dropped.
    NotAbortable,
    NotFound,
}

/// Lazy sweeper report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DueReport {
    pub expired_codes: usize,
    pub aborted: Vec<AbortedPairing>,
}

// ---- the store -------------------------------------------------------------

/// Single-connection authoritative registry. Every method runs inside one
/// `BEGIN IMMEDIATE` transaction on the one connection (mutex-serialized
/// in-process, SQLite-write-lock-serialized across processes —
/// single-active-server discipline, storage-schema.md §1). Share it via
/// `Arc`, never by cloning.
#[derive(Debug)]
pub struct RegistryStore {
    conn: Mutex<Connection>,
}

impl RegistryStore {
    /// Open (or create) the registry at `path`. Sets durable commit
    /// semantics, creates the schema, and refuses a database whose
    /// schema version is newer than this build.
    pub fn open(path: &Path) -> Result<RegistryStore, StoreError> {
        Self::init(Connection::open(path)?)
    }

    /// Open a private in-memory registry (tests, throwaway registries).
    pub fn open_in_memory() -> Result<RegistryStore, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<RegistryStore, StoreError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // Durable commits: the authoritative registry must never report a
        // claim/commit accepted before the write reaches the disk
        // (storage-schema.md §1).
        conn.pragma_update(None, "synchronous", "FULL")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            // Fresh database: schema creation and the version stamp commit
            // atomically (an interrupted first open leaves version 0 and no
            // tables, and `IF NOT EXISTS` makes a rerun safe anyway).
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(&format!(
                "{DDL_INVITE_CODES}\n{DDL_PAIRING_TRANSACTIONS}\n{DDL_PAIRING_RELATIONS}\n{DDL_REQUEST_JOURNAL}\n{DDL_INDEXES}"
            ))?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        } else if version == 1 {
            migrate_v1_to_v2(&mut conn)?;
        } else if version != SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion(version));
        }
        Ok(RegistryStore {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock means a panic inside a critical section; the
        // SQLite transaction was rolled back and the connection is fine.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wall clock in ms (engine symmetry; parameterizable in tests).
    pub fn now_ms() -> u64 {
        now_unix_ms()
    }

    /// Lazily move due codes to `expired` (never back to `active`).
    /// In-transaction expiry checks in redeem/commit/list still run; the
    /// sweeper only accelerates cleanup (`pairing-flow.md` §8).
    pub fn expire_codes_lazy(&self, now_ms: u64) -> Result<usize, StoreError> {
        let conn = self.lock();
        Ok(Self::expire_codes_within(&conn, now_ms)?)
    }

    fn expire_codes_within(conn: &Connection, now_ms: u64) -> rusqlite::Result<usize> {
        conn.execute(
            "UPDATE invite_codes SET state='expired', revision=revision+1
             WHERE state='active' AND expires_unix_ms IS NOT NULL AND expires_unix_ms <= ?1",
            params![now_ms as i64],
        )
    }

    /// Register one invite code (`pairing-flow.md` §5). Wave-28: the
    /// claim may carry the owner endpoint blob (stored on the code row,
    /// served to redeemers with `PairCommit`) and a redeem cap (0 =
    /// unlimited legacy semantics). Both are part of the idempotency
    /// body: same `request_id` + same content replays the original
    /// `InviteClaimed`, different content is `REQUEST_CONFLICT`. With
    /// both at their defaults the body is byte-identical to the
    /// pre-wave-28 canonical form, so claims journaled by a v1 server
    /// still replay after the upgrade (P2-14, see [`claim_body`]); the
    /// one historical divergence — wave-28 journaled default claims with
    /// an explicit all-zero tail (41 bytes) — is folded into the
    /// comparison, not the storage (R29-P2-02, see
    /// [`claim_bodies_equivalent`]).
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        &self,
        owner: &DeviceId,
        request_id: &RequestId,
        lookup_digest: &Digest32,
        ttl_secs: u32,
        max_redeems: u32,
        owner_endpoint: Option<&[u8]>,
        now_ms: u64,
    ) -> Result<ClaimOutcome, StoreError> {
        if ttl_secs != 0 && !(INVITE_TTL_MIN_SECS..=INVITE_TTL_MAX_SECS).contains(&ttl_secs) {
            // Out-of-range TTL must never persist; the engine rejects it
            // with a protocol error before reaching the store.
            return Ok(ClaimOutcome::RequestConflict);
        }
        let body = claim_body(lookup_digest, ttl_secs, max_redeems, owner_endpoint);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Idempotency first: same owner+request_id replays or conflicts.
        // The comparison tolerates the wave-28 41-byte default layout
        // (R29-P2-02) without rewriting any stored row.
        if let Some((stored_body, response)) = journal_get(&tx, owner, request_id)? {
            return Ok(if claim_bodies_equivalent(&stored_body, &body) {
                ClaimOutcome::Replay(response)
            } else {
                ClaimOutcome::RequestConflict
            });
        }
        // Quotas (contract §11).
        let active: i64 = tx.query_row(
            "SELECT COUNT(*) FROM invite_codes WHERE owner_device_id=?1 AND state='active'",
            params![owner.as_slice()],
            |row| row.get(0),
        )?;
        if active >= MAX_ACTIVE_INVITES_PER_OWNER as i64 {
            return Ok(ClaimOutcome::ResourceLimit);
        }
        let recent: i64 = tx.query_row(
            "SELECT COUNT(*) FROM invite_codes WHERE owner_device_id=?1 AND created_unix_ms > ?2",
            params![owner.as_slice(), now_ms.saturating_sub(3_600_000) as i64],
            |row| row.get(0),
        )?;
        if recent >= INVITE_CLAIMS_PER_HOUR as i64 {
            return Ok(ClaimOutcome::RateLimited);
        }
        // Same lookup under any earlier registration: collision. The
        // UNIQUE index below remains the linearization backstop for
        // multi-writer deployments; this check makes the outcome exact.
        let lookup_taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM invite_codes WHERE lookup_digest=?1",
                params![lookup_digest.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        if lookup_taken.is_some() {
            return Ok(ClaimOutcome::CodeConflict);
        }
        // Same owner+create_request_id with an evicted journal row: the
        // invite_codes UNIQUE(owner, create_request_id) still applies.
        let prior_lookup: Option<Vec<u8>> = tx
            .query_row(
                "SELECT lookup_digest FROM invite_codes
                 WHERE owner_device_id=?1 AND create_request_id=?2",
                params![owner.as_slice(), request_id.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(prior) = prior_lookup {
            return Ok(if prior.as_slice() == lookup_digest.as_slice() {
                // Original journal entry was pruned; reconstruct the
                // replay response from the row so the retry still gets
                // its original result, then re-journal it.
                let row: (Vec<u8>, i64, Option<i64>, i64) = tx.query_row(
                    "SELECT code_id, created_unix_ms, expires_unix_ms, revision
                     FROM invite_codes WHERE owner_device_id=?1 AND create_request_id=?2",
                    params![owner.as_slice(), request_id.as_slice()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let code_id = blob_req::<16>(row.0)?;
                let created = row.1.max(0) as u64;
                let expires = row.2.map(|v| v.max(0) as u64).unwrap_or(0);
                let revision = row.3.max(0) as u64;
                let response = encode_response(&CtrlMessage::InviteClaimed {
                    request_id: *request_id,
                    code_id,
                    created_ms: created,
                    expires_ms: expires,
                    revision,
                })?;
                journal_put(&tx, owner, request_id, "claim", &body, &response, now_ms)?;
                tx.commit()?;
                return Ok(ClaimOutcome::Replay(response));
            } else {
                ClaimOutcome::RequestConflict
            });
        }
        let expires_unix_ms: Option<i64> = if ttl_secs == 0 {
            None
        } else {
            Some(now_ms as i64 + i64::from(ttl_secs) * 1000)
        };
        let code_id = fresh_code_id(&tx)?;
        let response = encode_response(&CtrlMessage::InviteClaimed {
            request_id: *request_id,
            code_id,
            created_ms: now_ms,
            expires_ms: expires_unix_ms.map(|v| v.max(0) as u64).unwrap_or(0),
            revision: 1,
        })?;
        tx.execute(
            "INSERT INTO invite_codes
             (lookup_digest, code_id, owner_device_id, created_unix_ms, expires_unix_ms,
              state, revision, create_request_id, owner_endpoint, max_redeems)
             VALUES (?1,?2,?3,?4,?5,'active',1,?6,?7,?8)",
            params![
                lookup_digest.as_slice(),
                code_id.as_slice(),
                owner.as_slice(),
                now_ms as i64,
                expires_unix_ms,
                request_id.as_slice(),
                owner_endpoint,
                max_redeems as i64
            ],
        )?;
        journal_put(&tx, owner, request_id, "claim", &body, &response, now_ms)?;
        tx.commit()?;
        Ok(ClaimOutcome::Claimed {
            code_id,
            created_ms: now_ms,
            expires_ms: expires_unix_ms.map(|v| v.max(0) as u64).unwrap_or(0),
            revision: 1,
        })
    }

    /// One page of the owner's own codes, ordered by `code_id` after the
    /// cursor (all-zero cursor = start). The state filter is applied in
    /// SQL so paging never under-fills a page. Only defined filter bits
    /// reach here (codec-enforced).
    pub fn list(
        &self,
        owner: &DeviceId,
        cursor: &CodeId,
        limit: u8,
        filter: StateFilter,
        now_ms: u64,
    ) -> Result<Vec<InviteSummary>, StoreError> {
        let conn = self.lock();
        Self::expire_codes_within(&conn, now_ms)?;
        // Fixed literals only — the IN list is derived from the closed
        // filter mask, never from user text.
        let mut states = String::new();
        if filter.contains(InviteState::Active) {
            states.push_str("'active',");
        }
        if filter.contains(InviteState::Revoked) {
            states.push_str("'revoked',");
        }
        if filter.contains(InviteState::Expired) {
            states.push_str("'expired',");
        }
        let states = states.trim_end_matches(',');
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT code_id, state, created_unix_ms, expires_unix_ms, revision
             FROM invite_codes
             WHERE owner_device_id=?1 AND code_id > ?2 AND state IN ({states})
             ORDER BY code_id LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![owner.as_slice(), cursor.as_slice(), i64::from(limit)],
            |row| {
                let state: String = row.get(1)?;
                Ok(InviteSummary {
                    code_id: blob_req::<16>(row.get(0)?).ok().unwrap_or_default(),
                    state: db_state(&state),
                    created_ms: row.get::<_, i64>(2)?.max(0) as u64,
                    expires_ms: row
                        .get::<_, Option<i64>>(3)?
                        .map(|v| v.max(0) as u64)
                        .unwrap_or(0),
                    revision: row.get::<_, i64>(4)?.max(0) as u64,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Owner-initiated revoke with the optimistic revision lock
    /// (`protocol-v2.md` §4; revoke/commit ordering per §8).
    pub fn revoke(
        &self,
        owner: &DeviceId,
        request_id: &RequestId,
        code_id: &CodeId,
        expected_revision: u64,
        now_ms: u64,
    ) -> Result<RevokeOutcome, StoreError> {
        let body = revoke_body(code_id, expected_revision);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_body, response)) = journal_get(&tx, owner, request_id)? {
            return Ok(if stored_body == body {
                RevokeOutcome::Replay(response)
            } else {
                RevokeOutcome::RequestConflict
            });
        }
        let row: Option<(Vec<u8>, String, i64)> = tx
            .query_row(
                "SELECT owner_device_id, state, revision FROM invite_codes WHERE code_id=?1",
                params![code_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((row_owner, state, revision)) = row else {
            // Unknown code — the same answer as someone else's code.
            return Ok(RevokeOutcome::Unavailable);
        };
        if row_owner.as_slice() != owner.as_slice() || state != "active" {
            return Ok(RevokeOutcome::Unavailable);
        }
        if revision != expected_revision as i64 {
            return Ok(RevokeOutcome::RevisionConflict);
        }
        let changed = tx.execute(
            "UPDATE invite_codes SET state='revoked', revision=revision+1
             WHERE code_id=?1 AND revision=?2 AND state='active'",
            params![code_id.as_slice(), expected_revision as i64],
        )?;
        if changed != 1 {
            return Ok(RevokeOutcome::RevisionConflict);
        }
        // Revoke linearized first: abort every uncommitted pairing under
        // this code (pairing-flow.md §8).
        let aborted = abort_pending_for_code(&tx, code_id, now_ms)?;
        let response = encode_response(&CtrlMessage::InviteRevoked {
            request_id: *request_id,
            code_id: *code_id,
            revision: expected_revision + 1,
            revoked_ms: now_ms,
        })?;
        journal_put(&tx, owner, request_id, "revoke", &body, &response, now_ms)?;
        tx.commit()?;
        Ok(RevokeOutcome::Revoked {
            revision: expected_revision + 1,
            revoked_ms: now_ms,
            aborted,
        })
    }

    /// Find a live code by lookup digest (redeem path), including the
    /// wave-28 endpoint/cap columns.
    pub fn find_active_code(
        &self,
        lookup_digest: &Digest32,
        now_ms: u64,
    ) -> Result<Option<ActiveCode>, StoreError> {
        let conn = self.lock();
        Self::expire_codes_within(&conn, now_ms)?;
        let row: Option<ActiveCodeRow> = conn
            .query_row(
                "SELECT code_id, owner_device_id, revision, owner_endpoint, max_redeems
                 FROM invite_codes
                 WHERE lookup_digest=?1 AND state='active'
                   AND (expires_unix_ms IS NULL OR expires_unix_ms > ?2)",
                params![lookup_digest.as_slice(), now_ms as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        Ok(row.and_then(|(code, owner, revision, owner_endpoint, max_redeems)| {
            Some(ActiveCode {
                code_id: blob::<16>(Some(code))?,
                owner: blob::<32>(Some(owner))?,
                revision: revision.max(0) as u64,
                owner_endpoint,
                max_redeems: max_redeems.max(0) as u32,
            })
        }))
    }

    // -- pairing_transactions ------------------------------------------------

    /// Register a redeem attempt as a pending transaction. `redeemer` is
    /// the authenticated connection's device id (never client-supplied);
    /// `redeemer_endpoint` is the redeemer's wave-28 connection settings.
    /// Wave-28 cap: when the code row carries `max_redeems ≥ 1` and that
    /// many committed (or later) pairings already exist, the attempt is
    /// refused up front with [`BeginRedeem::RedeemExhausted`] — aborted or
    /// expired attempts never consume the budget.
    pub fn begin_pairing(
        &self,
        pairing_id: &PairingId,
        redeemer: &DeviceId,
        redeem: &RedeemRequest,
        code: &ActiveCode,
        redeemer_endpoint: Option<&[u8]>,
        now_ms: u64,
    ) -> Result<BeginRedeem, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if code.max_redeems > 0 {
            let committed: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pairing_transactions
                 WHERE code_id=?1 AND state IN ('committed','applied','ready')",
                params![code.code_id.as_slice()],
                |row| row.get(0),
            )?;
            if committed >= i64::from(code.max_redeems) {
                return Ok(BeginRedeem::RedeemExhausted);
            }
        }
        let owner_pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pairing_transactions
             WHERE owner_device_id=?1 AND state='pending'",
            params![code.owner.as_slice()],
            |row| row.get(0),
        )?;
        if owner_pending >= MAX_PENDING_REDEEM_PER_OWNER as i64 {
            return Ok(BeginRedeem::OwnerPendingLimit);
        }
        let global_pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pairing_transactions WHERE state='pending'",
            [],
            |row| row.get(0),
        )?;
        if global_pending >= MAX_PENDING_REDEEM_GLOBAL as i64 {
            return Ok(BeginRedeem::GlobalPendingLimit);
        }
        tx.execute(
            "INSERT INTO pairing_transactions
             (pairing_id, request_id, code_id, lookup_digest, owner_device_id, redeemer_device_id,
              state, redeemer_nonce, request_mac, redeemer_signature, deadline_unix_ms,
              code_revision, owner_endpoint, redeemer_endpoint, created_unix_ms, updated_unix_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?8,?9,?10,?11,?12,?13,?14,?14)",
            params![
                pairing_id.as_slice(),
                redeem.request_id.as_slice(),
                code.code_id.as_slice(),
                redeem.lookup_digest.as_slice(),
                code.owner.as_slice(),
                redeemer.as_slice(),
                redeem.redeemer_nonce.as_slice(),
                redeem.request_mac.as_slice(),
                redeem.signature.as_slice(),
                redeem.deadline_ms as i64,
                code.revision as i64,
                code.owner_endpoint,
                redeemer_endpoint,
                now_ms as i64
            ],
        )?;
        tx.commit()?;
        Ok(BeginRedeem::Begun)
    }

    /// Pending offers to (re)push when the owner (re)connects: pending
    /// transactions whose owner proof has not arrived yet.
    pub fn pending_offers_for_owner(
        &self,
        owner: &DeviceId,
    ) -> Result<Vec<PendingOffer>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT pairing_id, code_id, code_revision, redeemer_device_id,
                    request_id, lookup_digest, redeemer_nonce, deadline_unix_ms,
                    request_mac, redeemer_signature, redeemer_endpoint
             FROM pairing_transactions
             WHERE owner_device_id=?1 AND state='pending' AND owner_proof_mac IS NULL
             ORDER BY created_unix_ms",
        )?;
        let rows = stmt.query_map(params![owner.as_slice()], map_pending_offer)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Stored-but-unanswered owner challenges to (re)push when the
    /// redeemer (re)connects.
    pub fn pending_challenges_for_redeemer(
        &self,
        redeemer: &DeviceId,
    ) -> Result<Vec<(PairingId, [u8; 256], PairProof)>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT pairing_id, transcript, owner_proof_mac, owner_proof_signature
             FROM pairing_transactions
             WHERE redeemer_device_id=?1 AND state='pending'
               AND transcript IS NOT NULL AND redeemer_proof_mac IS NULL
             ORDER BY created_unix_ms",
        )?;
        let rows = stmt.query_map(params![redeemer.as_slice()], |row| {
            Ok((
                blob_req::<16>(row.get::<_, Vec<u8>>(0)?)?,
                blob_req::<256>(row.get::<_, Vec<u8>>(1)?)?,
                PairProof {
                    mac: blob_req::<32>(row.get::<_, Vec<u8>>(2)?)?,
                    signature: blob_req::<64>(row.get::<_, Vec<u8>>(3)?)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Stored redeemer proofs to (re)forward when the owner reconnects.
    pub fn pending_proofs_for_owner(
        &self,
        owner: &DeviceId,
    ) -> Result<Vec<(PairingId, Digest32, PairProof)>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT pairing_id, transcript_digest, redeemer_proof_mac, redeemer_proof_signature
             FROM pairing_transactions
             WHERE owner_device_id=?1 AND state='pending'
               AND transcript_digest IS NOT NULL AND redeemer_proof_mac IS NOT NULL
             ORDER BY created_unix_ms",
        )?;
        let rows = stmt.query_map(params![owner.as_slice()], |row| {
            Ok((
                blob_req::<16>(row.get::<_, Vec<u8>>(0)?)?,
                blob_req::<32>(row.get::<_, Vec<u8>>(1)?)?,
                PairProof {
                    mac: blob_req::<32>(row.get::<_, Vec<u8>>(2)?)?,
                    signature: blob_req::<64>(row.get::<_, Vec<u8>>(3)?)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Store the owner's transcript + proof (PairChallenge). `false`:
    /// unknown pairing, wrong owner, or not pending.
    pub fn store_owner_challenge(
        &self,
        pairing_id: &PairingId,
        owner: &DeviceId,
        transcript: &[u8; 256],
        transcript_digest: &Digest32,
        proof: &PairProof,
        now_ms: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE pairing_transactions
             SET transcript=?1, transcript_digest=?2, owner_proof_mac=?3,
                 owner_proof_signature=?4, updated_unix_ms=?5
             WHERE pairing_id=?6 AND owner_device_id=?7 AND state='pending'",
            params![
                transcript.as_slice(),
                transcript_digest.as_slice(),
                proof.mac.as_slice(),
                proof.signature.as_slice(),
                now_ms as i64,
                pairing_id.as_slice(),
                owner.as_slice()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Store the redeemer's proof (PairProof). Requires the stored
    /// transcript digest to match — transcript/key/code substitution is
    /// rejected here (`pairing-flow.md` §7).
    pub fn store_redeemer_proof(
        &self,
        pairing_id: &PairingId,
        redeemer: &DeviceId,
        transcript_digest: &Digest32,
        proof: &PairProof,
        now_ms: u64,
    ) -> Result<bool, StoreError> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE pairing_transactions
             SET redeemer_proof_mac=?1, redeemer_proof_signature=?2, updated_unix_ms=?3
             WHERE pairing_id=?4 AND redeemer_device_id=?5 AND state='pending'
               AND transcript_digest=?6",
            params![
                proof.mac.as_slice(),
                proof.signature.as_slice(),
                now_ms as i64,
                pairing_id.as_slice(),
                redeemer.as_slice(),
                transcript_digest.as_slice()
            ],
        )?;
        Ok(changed == 1)
    }

    /// Store one side's Prepared signature and, when both sides are in,
    /// commit in the same transaction (rechecking code state and the
    /// relation quota). This is the revoke-vs-commit serialization point.
    pub fn store_prepared_and_try_commit(
        &self,
        pairing_id: &PairingId,
        sender: &DeviceId,
        signature: &Signature64,
        transcript_digest: &Digest32,
        now_ms: u64,
    ) -> Result<PreparedDecision, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pairing_tx_row(&tx, pairing_id)?;
        let Some(row) = row else {
            return Ok(PreparedDecision::NotFound);
        };
        if (row.owner != *sender && row.redeemer != *sender)
            || row.transcript_digest != Some(*transcript_digest)
        {
            return Ok(PreparedDecision::NotFound);
        }
        match db_pairing_state(&row.state) {
            PairingState::Committed | PairingState::Applied => {
                return Ok(PreparedDecision::AlreadyFinal(db_pairing_state(&row.state)));
            }
            PairingState::Ready => return Ok(PreparedDecision::AlreadyFinal(PairingState::Ready)),
            PairingState::Aborted => return Ok(PreparedDecision::Unavailable),
            PairingState::Pending | PairingState::Prepared => {}
        }
        let is_owner = row.owner == *sender;
        let column = if is_owner {
            "owner_prepared_signature"
        } else {
            "redeemer_prepared_signature"
        };
        tx.execute(
            &format!(
                "UPDATE pairing_transactions SET {column}=?1, updated_unix_ms=?2
                 WHERE pairing_id=?3"
            ),
            params![signature.as_slice(), now_ms as i64, pairing_id.as_slice()],
        )?;
        let have_owner = row.owner_prepared_signature.is_some() || is_owner;
        let have_redeemer = row.redeemer_prepared_signature.is_some() || !is_owner;
        if !(have_owner && have_redeemer) {
            tx.commit()?;
            return Ok(PreparedDecision::WaitingOther);
        }
        // Both prepared. Commit recheck in the same transaction: the code
        // must still be active and unexpired (a revoke or expiry that
        // linearized first ⇒ this pairing fails).
        let code: Option<(String, Option<i64>, i64)> = tx
            .query_row(
                "SELECT state, expires_unix_ms, max_redeems FROM invite_codes WHERE code_id=?1",
                params![row.code_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let alive = matches!(&code, Some((state, expires, _)) if state == "active"
            && expires.map(|e| e > now_ms as i64).unwrap_or(true));
        if !alive {
            tx.execute(
                "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
                 WHERE pairing_id=?2",
                params![now_ms as i64, pairing_id.as_slice()],
            )?;
            tx.commit()?;
            return Ok(PreparedDecision::Unavailable);
        }
        // Wave-28 cap recheck at the commit linearization point (P1-03).
        // `begin_pairing` counts only committed pairings, so several
        // pendings may coexist under a capped code; the cap must bind
        // exactly here, inside the write transaction that would flip this
        // row to `committed`. The count excludes this pairing (still
        // pending/prepared), so one of the racers wins and the losers are
        // aborted — and aborted never consumes the cap, leaving the same
        // `request_id` replay semantics untouched.
        let max_redeems = code.as_ref().map(|&(_, _, cap)| cap).unwrap_or(0);
        if max_redeems > 0 {
            let committed: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pairing_transactions
                 WHERE code_id=?1 AND state IN ('committed','applied','ready')",
                params![row.code_id.as_slice()],
                |row| row.get(0),
            )?;
            if committed >= max_redeems {
                tx.execute(
                    "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
                     WHERE pairing_id=?2",
                    params![now_ms as i64, pairing_id.as_slice()],
                )?;
                tx.commit()?;
                return Ok(PreparedDecision::RedeemExhausted);
            }
        }
        let relations: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pairing_relations WHERE owner_device_id=?1",
            params![row.owner.as_slice()],
            |r| r.get(0),
        )?;
        if relations >= MAX_RELATIONS_PER_OWNER as i64 {
            tx.execute(
                "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
                 WHERE pairing_id=?2",
                params![now_ms as i64, pairing_id.as_slice()],
            )?;
            tx.commit()?;
            return Ok(PreparedDecision::ResourceLimit);
        }
        let code_revision: i64 = tx.query_row(
            "SELECT revision FROM invite_codes WHERE code_id=?1",
            params![row.code_id.as_slice()],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE pairing_transactions
             SET state='committed', commit_revision=?1, updated_unix_ms=?2
             WHERE pairing_id=?3",
            params![code_revision, now_ms as i64, pairing_id.as_slice()],
        )?;
        tx.commit()?;
        Ok(PreparedDecision::Committed {
            commit_revision: code_revision.max(0) as u64,
        })
    }

    /// Store one side's Applied confirmation; both sides in ⇒ Ready and
    /// the pairing relation is recorded.
    pub fn store_applied(
        &self,
        pairing_id: &PairingId,
        sender: &DeviceId,
        signature: &Signature64,
        transcript_digest: &Digest32,
        commit_revision: u64,
        now_ms: u64,
    ) -> Result<AppliedDecision, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = pairing_tx_row(&tx, pairing_id)?;
        let Some(row) = row else {
            return Ok(AppliedDecision::NotFound);
        };
        if (row.owner != *sender && row.redeemer != *sender)
            || row.transcript_digest != Some(*transcript_digest)
        {
            return Ok(AppliedDecision::NotFound);
        }
        match db_pairing_state(&row.state) {
            PairingState::Ready => return Ok(AppliedDecision::AlreadyReady),
            PairingState::Committed | PairingState::Applied => {}
            _ => return Ok(AppliedDecision::NotFound),
        }
        if row.commit_revision != Some(commit_revision as i64) {
            return Ok(AppliedDecision::RevisionConflict);
        }
        let is_owner = row.owner == *sender;
        let column = if is_owner {
            "owner_applied_signature"
        } else {
            "redeemer_applied_signature"
        };
        tx.execute(
            &format!(
                "UPDATE pairing_transactions SET {column}=?1, updated_unix_ms=?2
                 WHERE pairing_id=?3"
            ),
            params![signature.as_slice(), now_ms as i64, pairing_id.as_slice()],
        )?;
        let have_owner = row.owner_applied_signature.is_some() || is_owner;
        let have_redeemer = row.redeemer_applied_signature.is_some() || !is_owner;
        if !(have_owner && have_redeemer) {
            if db_pairing_state(&row.state) == PairingState::Committed {
                tx.execute(
                    "UPDATE pairing_transactions SET state='applied', updated_unix_ms=?1
                     WHERE pairing_id=?2",
                    params![now_ms as i64, pairing_id.as_slice()],
                )?;
            }
            tx.commit()?;
            return Ok(AppliedDecision::WaitingOther);
        }
        let digest = row
            .transcript_digest
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.execute(
            "INSERT OR IGNORE INTO pairing_relations
             (pairing_id, code_id, lookup_digest, owner_device_id, redeemer_device_id,
              transcript_digest, commit_revision, completed_unix_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                pairing_id.as_slice(),
                row.code_id.as_slice(),
                row.lookup_digest.as_slice(),
                row.owner.as_slice(),
                row.redeemer.as_slice(),
                digest.as_slice(),
                commit_revision as i64,
                now_ms as i64
            ],
        )?;
        tx.execute(
            "UPDATE pairing_transactions SET state='ready', updated_unix_ms=?1
             WHERE pairing_id=?2",
            params![now_ms as i64, pairing_id.as_slice()],
        )?;
        tx.commit()?;
        Ok(AppliedDecision::Ready)
    }

    /// Full snapshot (status queries, commit/ready resends).
    pub fn pairing_snapshot(
        &self,
        pairing_id: &PairingId,
    ) -> Result<Option<PairingSnapshot>, StoreError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT code_id, lookup_digest, owner_device_id, redeemer_device_id,
                        state, deadline_unix_ms, code_revision, commit_revision,
                        transcript, transcript_digest, redeemer_nonce,
                        owner_proof_mac, owner_proof_signature,
                        redeemer_proof_mac, redeemer_proof_signature,
                        owner_prepared_signature, redeemer_prepared_signature,
                        owner_applied_signature, redeemer_applied_signature,
                        request_id, owner_endpoint, redeemer_endpoint
                 FROM pairing_transactions WHERE pairing_id=?1",
                params![pairing_id.as_slice()],
                map_pairing_snapshot,
            )
            .optional()?;
        Ok(row.map(|mut snapshot| {
            snapshot.pairing_id = *pairing_id;
            snapshot
        }))
    }

    /// Abort an uncommitted transaction (participant request or sweeper).
    pub fn abort_pairing(
        &self,
        pairing_id: &PairingId,
        now_ms: u64,
    ) -> Result<AbortDecision, StoreError> {
        let conn = self.lock();
        let row: Option<(Vec<u8>, Vec<u8>, String)> = conn
            .query_row(
                "SELECT owner_device_id, redeemer_device_id, state
                 FROM pairing_transactions WHERE pairing_id=?1",
                params![pairing_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((owner, redeemer, state)) = row else {
            return Ok(AbortDecision::NotFound);
        };
        if state != "pending" && state != "prepared" {
            return Ok(AbortDecision::NotAbortable);
        }
        conn.execute(
            "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
             WHERE pairing_id=?2",
            params![now_ms as i64, pairing_id.as_slice()],
        )?;
        Ok(AbortDecision::Aborted(AbortedPairing {
            pairing_id: *pairing_id,
            owner: blob::<32>(Some(owner)).unwrap_or_default(),
            redeemer: blob::<32>(Some(redeemer)).unwrap_or_default(),
        }))
    }

    /// Lazy sweeper: expire due codes and abort overdue uncommitted
    /// transactions. Committed transactions are never dropped here
    /// (`pairing-flow.md` §8).
    pub fn expire_due(&self, now_ms: u64) -> Result<DueReport, StoreError> {
        let conn = self.lock();
        let expired_codes = Self::expire_codes_within(&conn, now_ms)?;
        let mut stmt = conn.prepare(
            "SELECT pairing_id, owner_device_id, redeemer_device_id
             FROM pairing_transactions
             WHERE state='pending' AND deadline_unix_ms <= ?1",
        )?;
        let due: Vec<AbortedPairing> = stmt
            .query_map(params![now_ms as i64], |row| {
                Ok(AbortedPairing {
                    pairing_id: blob_req::<16>(row.get(0)?).ok().unwrap_or_default(),
                    owner: blob_req::<32>(row.get(1)?).ok().unwrap_or_default(),
                    redeemer: blob_req::<32>(row.get(2)?).ok().unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if due.is_empty() {
            return Ok(DueReport {
                expired_codes,
                aborted: due,
            });
        }
        conn.execute(
            "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
             WHERE state='pending' AND deadline_unix_ms <= ?2",
            params![now_ms as i64, now_ms as i64],
        )?;
        Ok(DueReport {
            expired_codes,
            aborted: due,
        })
    }

    /// Does a completed pairing relation exist for exactly this triple
    /// and endpoint pair? (v2 session gate, contract §10.)
    pub fn relation_exists(
        &self,
        pairing_id: &PairingId,
        code_id: &CodeId,
        a: &DeviceId,
        b: &DeviceId,
    ) -> Result<bool, StoreError> {
        let conn = self.lock();
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM pairing_relations
                 WHERE pairing_id=?1 AND code_id=?2
                   AND ((owner_device_id=?3 AND redeemer_device_id=?4)
                     OR (owner_device_id=?4 AND redeemer_device_id=?3))",
                params![
                    pairing_id.as_slice(),
                    code_id.as_slice(),
                    a.as_slice(),
                    b.as_slice()
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

// ---- helpers ----------------------------------------------------------------

/// Wave-28 migration (schema 1 → 2), atomic and re-entrant (P2-13).
///
/// All four additive `ALTER TABLE` statements and the `user_version`
/// stamp run inside **one** write transaction: an interrupted migration
/// (crash, power loss, failed later statement) rolls back completely, so
/// the file is either pristine v1 or fully migrated v2 — never the
/// half-migrated layout that used to make every subsequent startup fail
/// with "duplicate column". A database already half-migrated by the
/// pre-fix non-atomic code is recognized and *completed* rather than
/// rejected: each `ADD COLUMN` runs only where its column is missing, so
/// reruns converge and no tombstone, uniqueness record or pre-existing
/// row is ever touched.
fn migrate_v1_to_v2(conn: &mut Connection) -> Result<(), StoreError> {
    const COLUMNS: [(&str, &str, &str); 4] = [
        (
            "invite_codes",
            "owner_endpoint",
            "ALTER TABLE invite_codes ADD COLUMN owner_endpoint BLOB NULL",
        ),
        (
            "invite_codes",
            "max_redeems",
            "ALTER TABLE invite_codes ADD COLUMN max_redeems INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "pairing_transactions",
            "owner_endpoint",
            "ALTER TABLE pairing_transactions ADD COLUMN owner_endpoint BLOB NULL",
        ),
        (
            "pairing_transactions",
            "redeemer_endpoint",
            "ALTER TABLE pairing_transactions ADD COLUMN redeemer_endpoint BLOB NULL",
        ),
    ];
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Fixed literals only (no user input ever reaches these statements).
    for (table, column, ddl) in COLUMNS {
        let present: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name='{column}'"),
            [],
            |row| row.get(0),
        )?;
        if present == 0 {
            tx.execute(ddl, [])?;
        }
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Internal row shape reused by the prepared/applied paths.
struct TxRow {
    owner: DeviceId,
    redeemer: DeviceId,
    code_id: CodeId,
    lookup_digest: Digest32,
    state: String,
    transcript_digest: Option<Digest32>,
    commit_revision: Option<i64>,
    owner_prepared_signature: Option<Signature64>,
    redeemer_prepared_signature: Option<Signature64>,
    owner_applied_signature: Option<Signature64>,
    redeemer_applied_signature: Option<Signature64>,
}

fn pairing_tx_row(
    tx: &rusqlite::Transaction<'_>,
    pairing_id: &PairingId,
) -> rusqlite::Result<Option<TxRow>> {
    tx.query_row(
        "SELECT owner_device_id, redeemer_device_id, code_id, lookup_digest, state,
                transcript_digest, commit_revision,
                owner_prepared_signature, redeemer_prepared_signature,
                owner_applied_signature, redeemer_applied_signature
         FROM pairing_transactions WHERE pairing_id=?1",
        params![pairing_id.as_slice()],
        |row| {
            Ok(TxRow {
                owner: blob_req::<32>(row.get(0)?)?,
                redeemer: blob_req::<32>(row.get(1)?)?,
                code_id: blob_req::<16>(row.get(2)?)?,
                lookup_digest: blob_req::<32>(row.get(3)?)?,
                state: row.get(4)?,
                transcript_digest: blob::<32>(row.get(5)?),
                commit_revision: row.get(6)?,
                owner_prepared_signature: blob::<64>(row.get(7)?),
                redeemer_prepared_signature: blob::<64>(row.get(8)?),
                owner_applied_signature: blob::<64>(row.get(9)?),
                redeemer_applied_signature: blob::<64>(row.get(10)?),
            })
        },
    )
    .optional()
}

fn map_pairing_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairingSnapshot> {
    let state: String = row.get(4)?;
    Ok(PairingSnapshot {
        pairing_id: [0u8; 16], // patched by the caller from the query key
        code_id: blob_req::<16>(row.get::<_, Vec<u8>>(0)?)?,
        lookup_digest: blob_req::<32>(row.get::<_, Vec<u8>>(1)?)?,
        owner: blob_req::<32>(row.get::<_, Vec<u8>>(2)?)?,
        redeemer: blob_req::<32>(row.get::<_, Vec<u8>>(3)?)?,
        state: db_pairing_state(&state),
        deadline_ms: row.get::<_, i64>(5)?.max(0) as u64,
        code_revision: row.get::<_, i64>(6)?.max(0) as u64,
        commit_revision: row.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
        transcript: blob::<256>(row.get::<_, Option<Vec<u8>>>(8)?),
        transcript_digest: blob::<32>(row.get::<_, Option<Vec<u8>>>(9)?),
        redeemer_nonce: blob_req::<32>(row.get::<_, Vec<u8>>(10)?)?,
        owner_proof: pair_proof(
            blob::<32>(row.get::<_, Option<Vec<u8>>>(11)?),
            blob::<64>(row.get::<_, Option<Vec<u8>>>(12)?),
        ),
        redeemer_proof: pair_proof(
            blob::<32>(row.get::<_, Option<Vec<u8>>>(13)?),
            blob::<64>(row.get::<_, Option<Vec<u8>>>(14)?),
        ),
        owner_prepared_signature: blob::<64>(row.get::<_, Option<Vec<u8>>>(15)?),
        redeemer_prepared_signature: blob::<64>(row.get::<_, Option<Vec<u8>>>(16)?),
        owner_applied_signature: blob::<64>(row.get::<_, Option<Vec<u8>>>(17)?),
        redeemer_applied_signature: blob::<64>(row.get::<_, Option<Vec<u8>>>(18)?),
        request_id: blob_req::<16>(row.get::<_, Vec<u8>>(19)?)?,
        owner_endpoint: row.get(20)?,
        redeemer_endpoint: row.get(21)?,
    })
}

const fn pair_proof(mac: Option<[u8; 32]>, signature: Option<[u8; 64]>) -> Option<PairProof> {
    match (mac, signature) {
        (Some(mac), Some(signature)) => Some(PairProof { mac, signature }),
        _ => None,
    }
}

fn map_pending_offer(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingOffer> {
    Ok(PendingOffer {
        pairing_id: blob_req::<16>(row.get::<_, Vec<u8>>(0)?)?,
        code_id: blob_req::<16>(row.get::<_, Vec<u8>>(1)?)?,
        code_revision: row.get::<_, i64>(2)?.max(0) as u64,
        redeemer: blob_req::<32>(row.get::<_, Vec<u8>>(3)?)?,
        redeem: RedeemRequest {
            request_id: blob_req::<16>(row.get::<_, Vec<u8>>(4)?)?,
            lookup_digest: blob_req::<32>(row.get::<_, Vec<u8>>(5)?)?,
            redeemer_nonce: blob_req::<32>(row.get::<_, Vec<u8>>(6)?)?,
            deadline_ms: row.get::<_, i64>(7)?.max(0) as u64,
            request_mac: blob_req::<32>(row.get::<_, Vec<u8>>(8)?)?,
            signature: blob_req::<64>(row.get::<_, Vec<u8>>(9)?)?,
        },
        redeemer_endpoint: row.get(10)?,
    })
}

fn db_state(text: &str) -> InviteState {
    match text {
        "active" => InviteState::Active,
        "revoked" => InviteState::Revoked,
        _ => InviteState::Expired,
    }
}

fn db_pairing_state(text: &str) -> PairingState {
    match text {
        "pending" => PairingState::Pending,
        "prepared" => PairingState::Prepared,
        "committed" => PairingState::Committed,
        "applied" => PairingState::Applied,
        "ready" => PairingState::Ready,
        _ => PairingState::Aborted,
    }
}

fn abort_pending_for_code(
    tx: &rusqlite::Transaction<'_>,
    code_id: &CodeId,
    now_ms: u64,
) -> rusqlite::Result<Vec<AbortedPairing>> {
    let mut stmt = tx.prepare(
        "SELECT pairing_id, owner_device_id, redeemer_device_id FROM pairing_transactions
         WHERE code_id=?1 AND state IN ('pending','prepared')",
    )?;
    let aborted: Vec<AbortedPairing> = stmt
        .query_map(params![code_id.as_slice()], |row| {
            Ok(AbortedPairing {
                pairing_id: blob_req::<16>(row.get(0)?)?,
                owner: blob_req::<32>(row.get(1)?)?,
                redeemer: blob_req::<32>(row.get(2)?)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if !aborted.is_empty() {
        tx.execute(
            "UPDATE pairing_transactions SET state='aborted', updated_unix_ms=?1
             WHERE code_id=?2 AND state IN ('pending','prepared')",
            params![now_ms as i64, code_id.as_slice()],
        )?;
    }
    Ok(aborted)
}

/// Canonical claim idempotency body, stable across schema versions
/// (P2-14).
///
/// The pre-wave-28 body was exactly `lookup_digest[32] + ttl_secs[4]`
/// (36 bytes). That layout **is** the canonical body whenever the
/// wave-28 extensions sit at their defaults (`max_redeems == 0`, no
/// endpoint blob): a claim journaled by a v1 server must replay the
/// original `InviteClaimed` after the upgrade, byte-for-byte. Only a
/// non-default extension appends the unambiguous v2 tail
/// (`max_redeems[4] + endpoint_presence[1] [+ blob]`); a v2 body is
/// therefore never 36 bytes and can never collide with a legacy one,
/// while genuinely different extension content still yields
/// `REQUEST_CONFLICT`.
fn claim_body(
    lookup_digest: &Digest32,
    ttl_secs: u32,
    max_redeems: u32,
    owner_endpoint: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(41);
    body.extend_from_slice(lookup_digest);
    body.extend_from_slice(&ttl_secs.to_be_bytes());
    if max_redeems > 0 || owner_endpoint.is_some() {
        body.extend_from_slice(&max_redeems.to_be_bytes());
        body.push(u8::from(owner_endpoint.is_some()));
        if let Some(endpoint) = owner_endpoint {
            body.extend_from_slice(endpoint);
        }
    }
    body
}

/// Compare a journaled claim body against the canonical body of an
/// incoming retry, folding in the one historical layout divergence
/// (R29-P2-02).
///
/// wave-28 journaled **every** claim with the explicit extension tail,
/// so a default extension (`max_redeems == 0`, no endpoint) was stored
/// as 41 bytes: `digest[32] + ttl[4] + cap=0[4] + presence=0[1]`. Since
/// wave-29 the canonical body for defaults is again the 36-byte v1
/// layout. A 41-byte body whose 5-byte tail is all zero can only be
/// such a wave-28 record — v1 never wrote a tail, and every non-default
/// tail has a non-zero cap or a non-zero presence byte — so it compares
/// equal to its own 36-byte prefix. All other bodies compare
/// byte-for-byte: genuinely different ttl, cap, or endpoint content
/// still yields `REQUEST_CONFLICT`. Stored rows are never rewritten;
/// only this comparison normalizes.
fn claim_bodies_equivalent(stored: &[u8], incoming: &[u8]) -> bool {
    fn canonical(body: &[u8]) -> &[u8] {
        match body {
            [prefix @ .., 0x00, 0x00, 0x00, 0x00, 0x00] if prefix.len() == 36 => prefix,
            other => other,
        }
    }
    canonical(stored) == canonical(incoming)
}

fn revoke_body(code_id: &CodeId, expected_revision: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(24);
    body.extend_from_slice(code_id);
    body.extend_from_slice(&expected_revision.to_be_bytes());
    body
}

fn journal_get(
    tx: &rusqlite::Transaction<'_>,
    owner: &DeviceId,
    request_id: &RequestId,
) -> rusqlite::Result<Option<(Vec<u8>, Vec<u8>)>> {
    tx.query_row(
        "SELECT request_body, response FROM request_journal
         WHERE owner_device_id=?1 AND request_id=?2",
        params![owner.as_slice(), request_id.as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

fn journal_put(
    tx: &rusqlite::Transaction<'_>,
    owner: &DeviceId,
    request_id: &RequestId,
    kind: &str,
    body: &[u8],
    response: &[u8],
    now_ms: u64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO request_journal
         (owner_device_id, request_id, request_kind, request_body, response, created_unix_ms)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            owner.as_slice(),
            request_id.as_slice(),
            kind,
            body,
            response,
            now_ms as i64
        ],
    )?;
    // Bounded journal: drop entries past the retention window, then trim
    // to the row cap keeping the newest (storage-schema.md §5).
    let floor = now_ms.saturating_sub(JOURNAL_RETENTION.as_millis() as u64);
    tx.execute(
        "DELETE FROM request_journal WHERE created_unix_ms < ?1",
        params![floor as i64],
    )?;
    tx.execute(
        "DELETE FROM request_journal WHERE (owner_device_id, request_id) IN (
           SELECT owner_device_id, request_id FROM request_journal
           ORDER BY created_unix_ms DESC, rowid DESC LIMIT -1 OFFSET ?1)",
        params![JOURNAL_MAX_ROWS as i64],
    )?;
    Ok(())
}

fn fresh_code_id(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<CodeId> {
    for _ in 0..8 {
        let id = ak_identity::pairing_txn::random_id16();
        let taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM invite_codes WHERE code_id=?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_none() {
            return Ok(id);
        }
    }
    Err(rusqlite::Error::InvalidParameterName(
        "code id space exhausted".into(),
    ))
}

fn encode_response(message: &CtrlMessage) -> rusqlite::Result<Vec<u8>> {
    let mut out = Vec::new();
    CtrlFrame::new(message.clone())
        .try_encode_as(Role::Server, &mut out)
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(
                format!("journal response encode failed: {e}").into(),
            )
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(seed: u8) -> DeviceId {
        [seed; 32]
    }

    fn lookup(seed: u8) -> Digest32 {
        [seed + 1; 32]
    }

    #[test]
    fn schema_version_is_stamped_and_reopen_refuses_newer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        let store = RegistryStore::open(&path).unwrap();
        drop(store);
        let store = RegistryStore::open(&path).unwrap();
        let conn = store.lock();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        conn.execute("PRAGMA user_version = 99", []).unwrap();
        drop(conn);
        let outcome = RegistryStore::open(&path);
        assert!(matches!(outcome, Err(StoreError::SchemaVersion(99))));
    }

    #[test]
    fn invite_codes_ddl_matches_the_contract_verbatim() {
        let store = RegistryStore::open_in_memory().unwrap();
        let conn = store.lock();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='invite_codes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Normalize whitespace and the IF NOT EXISTS marker (SQLite does
        // not keep it in sqlite_master) so the stored DDL compares cleanly
        // with the frozen contract text.
        let stored: String = sql
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let frozen: String = DDL_INVITE_CODES
            .trim()
            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(stored.trim_end_matches(';'), frozen.trim_end_matches(';'));
    }

    #[test]
    fn claim_commits_and_replays_idempotently() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let first = store
            .claim(&owner(1), &[7; 16], &lookup(1), 0, 0, None, now)
            .unwrap();
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = first
        else {
            panic!("first claim must succeed: {first:?}");
        };
        assert_eq!(revision, 1);
        assert_ne!(code_id, [0u8; 16]);

        // Same request id + same content: the exact original response.
        let retry = store
            .claim(&owner(1), &[7; 16], &lookup(1), 0, 0, None, now + 10)
            .unwrap();
        match retry {
            ClaimOutcome::Replay(bytes) => {
                let frame = CtrlFrame::decode(&bytes).unwrap();
                assert!(matches!(frame.message, CtrlMessage::InviteClaimed { .. }));
            }
            other => panic!("retry must replay: {other:?}"),
        }

        // Same request id, different content: conflict.
        let conflict = store
            .claim(&owner(1), &[7; 16], &lookup(9), 0, 0, None, now + 20)
            .unwrap();
        assert_eq!(conflict, ClaimOutcome::RequestConflict);
    }

    #[test]
    fn same_lookup_different_request_is_a_code_conflict() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        assert!(matches!(
            store
                .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
                .unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
        // Another owner, another request, same code digest.
        assert_eq!(
            store
                .claim(&owner(2), &[2; 16], &lookup(1), 0, 0, None, now)
                .unwrap(),
            ClaimOutcome::CodeConflict
        );
        // Same owner, another request, same digest.
        assert_eq!(
            store
                .claim(&owner(1), &[3; 16], &lookup(1), 0, 0, None, now)
                .unwrap(),
            ClaimOutcome::CodeConflict
        );
    }

    #[test]
    fn ttl_out_of_contract_range_never_persists() {
        let store = RegistryStore::open_in_memory().unwrap();
        assert_eq!(
            store.claim(&owner(1), &[1; 16], &lookup(1), 59, 0, None, 1).unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(1), &[1; 16], &lookup(1), INVITE_TTL_MAX_SECS + 1, 0, None, 1)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // 60 s is the minimum legal TTL; long-lived is 0.
        assert!(matches!(
            store.claim(&owner(1), &[2; 16], &lookup(2), 60, 0, None, 1).unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
        assert!(matches!(
            store.claim(&owner(1), &[3; 16], &lookup(3), 0, 0, None, 1).unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
    }

    #[test]
    fn revoke_uses_optimistic_lock_and_is_unified_for_foreign_codes() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed { code_id, .. } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        // Wrong revision.
        assert_eq!(
            store.revoke(&owner(1), &[8; 16], &code_id, 5, now).unwrap(),
            RevokeOutcome::RevisionConflict
        );
        // Foreign owner and unknown code: identical answer.
        assert_eq!(
            store.revoke(&owner(2), &[9; 16], &code_id, 1, now).unwrap(),
            RevokeOutcome::Unavailable
        );
        assert_eq!(
            store
                .revoke(&owner(1), &[10; 16], &[0xEE; 16], 1, now)
                .unwrap(),
            RevokeOutcome::Unavailable
        );
        // Correct owner and revision.
        assert_eq!(
            store
                .revoke(&owner(1), &[11; 16], &code_id, 1, now + 5)
                .unwrap(),
            RevokeOutcome::Revoked {
                revision: 2,
                revoked_ms: now + 5,
                aborted: vec![]
            }
        );
        // Revoked stays revoked (second revoke with the old revision
        // fails; with the new revision the state check fails).
        assert_eq!(
            store
                .revoke(&owner(1), &[12; 16], &code_id, 1, now + 6)
                .unwrap(),
            RevokeOutcome::Unavailable
        );
        assert_eq!(
            store
                .revoke(&owner(1), &[13; 16], &code_id, 2, now + 6)
                .unwrap(),
            RevokeOutcome::Unavailable
        );
        // The revoked code is gone from lookup and never resurrects.
        assert_eq!(store.find_active_code(&lookup(1), now + 7).unwrap(), None);
    }

    #[test]
    fn lazy_expiry_is_terminal_and_bumps_revision() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            expires_ms,
            code_id,
            ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 60, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(expires_ms, now + 60_000);
        assert!(store
            .find_active_code(&lookup(1), now + 1000)
            .unwrap()
            .is_some());
        let expired = store.expire_codes_lazy(now + 60_000).unwrap();
        assert_eq!(expired, 1);
        assert_eq!(
            store.find_active_code(&lookup(1), now + 60_001).unwrap(),
            None
        );
        // Owner still sees the tombstone, expired, revision bumped.
        let listed = store
            .list(&owner(1), &[0; 16], 10, StateFilter::ALL, now + 60_000)
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, InviteState::Expired);
        assert_eq!(listed[0].revision, 2);
        assert_eq!(listed[0].code_id, code_id);
    }

    #[test]
    fn list_filters_states_and_pages_by_code_id() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let mut ids = Vec::new();
        for i in 0..3u8 {
            let ClaimOutcome::Claimed { code_id, .. } = store
                .claim(&owner(1), &[i + 1; 16], &lookup(i), 0, 0, None, now + i as u64)
                .unwrap()
            else {
                unreachable!()
            };
            ids.push(code_id);
        }
        ids.sort();
        // Active only: all three, in code_id order, paged by 2.
        let page1 = store
            .list(&owner(1), &[0; 16], 2, StateFilter::ACTIVE, now)
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].code_id, ids[0]);
        let page2 = store
            .list(&owner(1), &page1[1].code_id, 2, StateFilter::ACTIVE, now)
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].code_id, ids[2]);
        // Revoke one; the active filter excludes it, the revoked filter
        // finds only it.
        store.revoke(&owner(1), &[9; 16], &ids[1], 1, now).unwrap();
        let active = store
            .list(&owner(1), &[0; 16], 32, StateFilter::ACTIVE, now)
            .unwrap();
        assert_eq!(active.len(), 2);
        let revoked = store
            .list(&owner(1), &[0; 16], 32, StateFilter::REVOKED, now)
            .unwrap();
        assert_eq!(revoked.len(), 1);
        assert_eq!(revoked[0].code_id, ids[1]);
    }

    #[test]
    fn begin_pairing_persists_and_offers_survive_reconnect_lookups() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 120_000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let pairing_id = [0xAA; 16];
        assert_eq!(
            store
                .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        // The pending offer is visible for the owner's catch-up push.
        let offers = store.pending_offers_for_owner(&owner(1)).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].pairing_id, pairing_id);
        assert_eq!(offers[0].redeemer, owner(2));
        assert_eq!(offers[0].redeem, redeem);
        assert_eq!(offers[0].code_revision, 1);
    }

    #[test]
    fn prepared_then_commit_then_ready_records_relation() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 120_000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let pairing_id = [0xBB; 16];
        store
            .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
            .unwrap();
        // Owner challenge with the transcript digest.
        let transcript = [0x11u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        assert!(store
            .store_owner_challenge(
                &pairing_id,
                &owner(1),
                &transcript,
                &digest,
                &PairProof {
                    mac: [6; 32],
                    signature: [7; 64],
                },
                now
            )
            .unwrap());
        assert!(store
            .store_redeemer_proof(
                &pairing_id,
                &owner(2),
                &digest,
                &PairProof {
                    mac: [8; 32],
                    signature: [9; 64],
                },
                now
            )
            .unwrap());
        // Wrong digest on prepared is refused.
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_id, &owner(1), &[1; 64], &[0; 32], now)
                .unwrap(),
            PreparedDecision::NotFound
        );
        // Owner prepares: waiting for redeemer.
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_id, &owner(1), &[1; 64], &digest, now)
                .unwrap(),
            PreparedDecision::WaitingOther
        );
        // Redeamer prepares: commit happens atomically.
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_id, &owner(2), &[2; 64], &digest, now)
                .unwrap(),
            PreparedDecision::Committed { commit_revision: 1 }
        );
        // Prepared replay after commit resends the fixed commit.
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_id, &owner(1), &[1; 64], &digest, now)
                .unwrap(),
            PreparedDecision::AlreadyFinal(PairingState::Committed)
        );
        // Applied: first waits, second completes with the relation.
        assert_eq!(
            store
                .store_applied(&pairing_id, &owner(1), &[3; 64], &digest, 1, now)
                .unwrap(),
            AppliedDecision::WaitingOther
        );
        assert_eq!(
            store
                .store_applied(&pairing_id, &owner(2), &[4; 64], &digest, 1, now)
                .unwrap(),
            AppliedDecision::Ready
        );
        assert!(store
            .relation_exists(&pairing_id, &code_id, &owner(1), &owner(2))
            .unwrap());
        let snapshot = store.pairing_snapshot(&pairing_id).unwrap().unwrap();
        assert_eq!(snapshot.state, PairingState::Ready);
        assert_eq!(snapshot.commit_revision, Some(1));
        // Applied replay after ready.
        assert_eq!(
            store
                .store_applied(&pairing_id, &owner(2), &[4; 64], &digest, 1, now)
                .unwrap(),
            AppliedDecision::AlreadyReady
        );
    }

    #[test]
    fn revoke_between_proofs_and_commit_fails_the_pairing() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 120_000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let pairing_id = [0xCC; 16];
        store
            .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
            .unwrap();
        let transcript = [0x22u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        store
            .store_owner_challenge(
                &pairing_id,
                &owner(1),
                &transcript,
                &digest,
                &PairProof {
                    mac: [6; 32],
                    signature: [7; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_redeemer_proof(
                &pairing_id,
                &owner(2),
                &digest,
                &PairProof {
                    mac: [8; 32],
                    signature: [9; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_prepared_and_try_commit(&pairing_id, &owner(1), &[1; 64], &digest, now)
            .unwrap();
        // Revoke lands between Prepared and the second Prepared: the
        // commit attempt must abort the pairing.
        let revoked = store
            .revoke(&owner(1), &[9; 16], &code_id, 1, now + 1)
            .unwrap();
        assert!(matches!(revoked, RevokeOutcome::Revoked { .. }));
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_id, &owner(2), &[2; 64], &digest, now + 2)
                .unwrap(),
            PreparedDecision::Unavailable
        );
        let snapshot = store.pairing_snapshot(&pairing_id).unwrap().unwrap();
        assert_eq!(snapshot.state, PairingState::Aborted);
    }

    #[test]
    fn expire_due_aborts_overdue_pending_and_never_committed() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 1000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let pairing_id = [0xDD; 16];
        store
            .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
            .unwrap();
        let report = store.expire_due(now + 999).unwrap();
        assert!(report.aborted.is_empty());
        let report = store.expire_due(now + 1000).unwrap();
        assert_eq!(report.aborted.len(), 1);
        assert_eq!(report.aborted[0].pairing_id, pairing_id);
        assert_eq!(
            store.pairing_snapshot(&pairing_id).unwrap().unwrap().state,
            PairingState::Aborted
        );
        // Committed transactions are never swept: commit one with a past
        // deadline and confirm expire_due leaves it alone.
        let redeem2 = RedeemRequest {
            request_id: [6; 16],
            deadline_ms: now + 2000,
            ..redeem
        };
        let pairing2 = [0xEE; 16];
        store
            .begin_pairing(&pairing2, &owner(3), &redeem2, &code, None, now)
            .unwrap();
        let transcript = [0u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        store
            .store_owner_challenge(
                &pairing2,
                &owner(1),
                &transcript,
                &digest,
                &PairProof {
                    mac: [0; 32],
                    signature: [0; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_redeemer_proof(
                &pairing2,
                &owner(3),
                &digest,
                &PairProof {
                    mac: [0; 32],
                    signature: [0; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_prepared_and_try_commit(&pairing2, &owner(1), &[1; 64], &digest, now)
            .unwrap();
        store
            .store_prepared_and_try_commit(&pairing2, &owner(3), &[2; 64], &digest, now)
            .unwrap();
        let report = store.expire_due(now + 9999).unwrap();
        assert!(report.aborted.is_empty());
        assert_eq!(
            store.pairing_snapshot(&pairing2).unwrap().unwrap().state,
            PairingState::Committed
        );
    }

    #[test]
    fn abort_pairing_refuses_committed_transactions() {
        let store = RegistryStore::open_in_memory().unwrap();
        assert_eq!(
            store.abort_pairing(&[0xEF; 16], 1).unwrap(),
            AbortDecision::NotFound
        );
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 120_000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let pairing_id = [0xAB; 16];
        store
            .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
            .unwrap();
        let transcript = [0u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        store
            .store_owner_challenge(
                &pairing_id,
                &owner(1),
                &transcript,
                &digest,
                &PairProof {
                    mac: [0; 32],
                    signature: [0; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_redeemer_proof(
                &pairing_id,
                &owner(2),
                &digest,
                &PairProof {
                    mac: [0; 32],
                    signature: [0; 64],
                },
                now,
            )
            .unwrap();
        store
            .store_prepared_and_try_commit(&pairing_id, &owner(1), &[1; 64], &digest, now)
            .unwrap();
        store
            .store_prepared_and_try_commit(&pairing_id, &owner(2), &[2; 64], &digest, now)
            .unwrap();
        assert_eq!(
            store.abort_pairing(&pairing_id, now).unwrap(),
            AbortDecision::NotAbortable
        );
    }

    #[test]
    fn claim_persists_endpoint_and_cap_with_replay_semantics() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let endpoint = vec![0xA7; 128];
        let first = store
            .claim(&owner(1), &[7; 16], &lookup(1), 0, 1, Some(&endpoint), now)
            .unwrap();
        assert!(matches!(first, ClaimOutcome::Claimed { .. }));
        // The lookup path serves the wave-28 columns.
        let code = store.find_active_code(&lookup(1), now).unwrap().unwrap();
        assert_eq!(code.owner_endpoint.as_deref(), Some(&endpoint[..]));
        assert_eq!(code.max_redeems, 1);
        // Same request + same content: replay (idempotency carries the
        // new fields in the body).
        let retry = store
            .claim(&owner(1), &[7; 16], &lookup(1), 0, 1, Some(&endpoint), now + 10)
            .unwrap();
        assert!(matches!(retry, ClaimOutcome::Replay(_)));
        // Same request, different endpoint or cap: REQUEST_CONFLICT.
        assert_eq!(
            store
                .claim(&owner(1), &[7; 16], &lookup(1), 0, 1, Some(&[0x00; 8]), now + 20)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(1), &[7; 16], &lookup(1), 0, 2, Some(&endpoint), now + 30)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(1), &[7; 16], &lookup(1), 0, 1, None, now + 40)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // Legacy claim (no endpoint, cap 0) stores the defaults.
        assert!(matches!(
            store
                .claim(&owner(2), &[8; 16], &lookup(2), 0, 0, None, now)
                .unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
        let legacy = store.find_active_code(&lookup(2), now).unwrap().unwrap();
        assert_eq!(legacy.owner_endpoint, None);
        assert_eq!(legacy.max_redeems, 0);
    }

    #[test]
    fn begin_pairing_snapshots_both_endpoint_blobs() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let owner_blob = vec![0x11; 64];
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, Some(&owner_blob), now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: Some(owner_blob.clone()),
            max_redeems: 0,
        };
        let redeem = RedeemRequest {
            request_id: [2; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [3; 32],
            deadline_ms: now + 120_000,
            request_mac: [4; 32],
            signature: [5; 64],
        };
        let redeemer_blob = vec![0x22; 96];
        let pairing_id = [0x9E; 16];
        assert_eq!(
            store
                .begin_pairing(&pairing_id, &owner(2), &redeem, &code, Some(&redeemer_blob), now)
                .unwrap(),
            BeginRedeem::Begun
        );
        // The offer served back to the owner carries the redeemer blob.
        let offers = store.pending_offers_for_owner(&owner(1)).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].redeemer_endpoint.as_deref(), Some(&redeemer_blob[..]));
        // The snapshot carries both sides for the commit path.
        let snapshot = store.pairing_snapshot(&pairing_id).unwrap().unwrap();
        assert_eq!(snapshot.owner_endpoint.as_deref(), Some(&owner_blob[..]));
        assert_eq!(snapshot.redeemer_endpoint.as_deref(), Some(&redeemer_blob[..]));
    }

    #[test]
    fn max_redeems_counts_committed_pairings_only() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 1, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 1,
        };
        let redeem = |seed: u8, deadline: u64| RedeemRequest {
            request_id: [seed; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [seed + 1; 32],
            deadline_ms: deadline,
            request_mac: [seed + 2; 32],
            signature: [seed + 3; 64],
        };
        let pairing_a = [0xCA; 16];
        let pairing_b = [0xCB; 16];
        // First redeemer begins: allowed.
        assert_eq!(
            store
                .begin_pairing(&pairing_a, &owner(2), &redeem(2, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        // A second pending attempt does NOT consume the cap (only
        // committed pairings count).
        assert_eq!(
            store
                .begin_pairing(&pairing_b, &owner(3), &redeem(6, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        // Abort B; A completes through commit (challenge + proof first).
        store.abort_pairing(&pairing_b, now).unwrap();
        let transcript = [0x33u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        assert!(store
            .store_owner_challenge(
                &pairing_a,
                &owner(1),
                &transcript,
                &digest,
                &PairProof { mac: [6; 32], signature: [7; 64] },
                now
            )
            .unwrap());
        assert!(store
            .store_redeemer_proof(
                &pairing_a,
                &owner(2),
                &digest,
                &PairProof { mac: [8; 32], signature: [9; 64] },
                now
            )
            .unwrap());
        for side in [owner(1), owner(2)] {
            store
                .store_prepared_and_try_commit(&pairing_a, &side, &[9; 64], &digest, now)
                .unwrap();
        }
        assert_eq!(
            store.pairing_snapshot(&pairing_a).unwrap().unwrap().state,
            PairingState::Committed
        );
        // Cap reached: a new redeemer is refused up front.
        assert_eq!(
            store
                .begin_pairing(&[0xCC; 16], &owner(4), &redeem(10, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::RedeemExhausted
        );
        // Unlimited codes (legacy default) keep admitting.
        assert!(matches!(
            store
                .claim(&owner(5), &[2; 16], &lookup(5), 0, 0, None, now)
                .unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
    }

    /// Drive one pending pairing through challenge + redeemer proof and
    /// both Prepared messages; the second Prepared returns the commit
    /// decision (or the explicit failure the commit recheck produced).
    fn drive_to_commit_decision(
        store: &RegistryStore,
        pairing_id: &PairingId,
        owner: &DeviceId,
        redeemer: &DeviceId,
        now: u64,
    ) -> PreparedDecision {
        let transcript = [0x44u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        assert!(store
            .store_owner_challenge(
                pairing_id,
                owner,
                &transcript,
                &digest,
                &PairProof {
                    mac: [6; 32],
                    signature: [7; 64],
                },
                now,
            )
            .unwrap());
        assert!(store
            .store_redeemer_proof(
                pairing_id,
                redeemer,
                &digest,
                &PairProof {
                    mac: [8; 32],
                    signature: [9; 64],
                },
                now,
            )
            .unwrap());
        assert_eq!(
            store
                .store_prepared_and_try_commit(pairing_id, owner, &[1; 64], &digest, now)
                .unwrap(),
            PreparedDecision::WaitingOther
        );
        store
            .store_prepared_and_try_commit(pairing_id, redeemer, &[2; 64], &digest, now)
            .unwrap()
    }

    /// P1-03: two pendings begin under a `max_redeems=1` code (pending
    /// never consumes the cap), then race to Commit — exactly one wins;
    /// the loser is aborted with the explicit `RedeemExhausted` decision
    /// and the cap stays consumed by the single winner.
    #[test]
    fn commit_recheck_lets_only_one_of_two_racing_pendings_win() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 1, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 1,
        };
        let redeem = |seed: u8, deadline: u64| RedeemRequest {
            request_id: [seed; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [seed + 1; 32],
            deadline_ms: deadline,
            request_mac: [seed + 2; 32],
            signature: [seed + 3; 64],
        };
        // Both pendings are admitted — pending consumes nothing.
        let pairing_a = [0xDA; 16];
        let pairing_b = [0xDB; 16];
        assert_eq!(
            store
                .begin_pairing(&pairing_a, &owner(2), &redeem(2, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        assert_eq!(
            store
                .begin_pairing(&pairing_b, &owner(3), &redeem(6, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        // First racer commits.
        assert_eq!(
            drive_to_commit_decision(&store, &pairing_a, &owner(1), &owner(2), now),
            PreparedDecision::Committed { commit_revision: 1 }
        );
        // Second racer: the cap recheck inside the commit transaction
        // aborts it with the explicit result — not a second Commit.
        assert_eq!(
            drive_to_commit_decision(&store, &pairing_b, &owner(1), &owner(3), now),
            PreparedDecision::RedeemExhausted
        );
        assert_eq!(
            store.pairing_snapshot(&pairing_a).unwrap().unwrap().state,
            PairingState::Committed
        );
        assert_eq!(
            store.pairing_snapshot(&pairing_b).unwrap().unwrap().state,
            PairingState::Aborted
        );
        // The single committed winner consumed the whole cap: a new
        // redeemer is refused up front.
        assert_eq!(
            store
                .begin_pairing(&[0xDC; 16], &owner(4), &redeem(10, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::RedeemExhausted
        );
        // Prepared replays on the aborted loser map to the unified
        // unavailable shape (no resurrection).
        let digest = crate::server_v2::hello2::transcript_digest(&[0x44; 256]);
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_b, &owner(1), &[1; 64], &digest, now)
                .unwrap(),
            PreparedDecision::Unavailable
        );
    }

    /// P1-03 with `max_redeems > 1`: the cap binds at commit, admitting
    /// exactly that many committed pairings and refusing the next racer.
    #[test]
    fn commit_recheck_binds_caps_above_one() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 2, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 2,
        };
        let redeem = |seed: u8| RedeemRequest {
            request_id: [seed; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [seed + 1; 32],
            deadline_ms: now + 120_000,
            request_mac: [seed + 2; 32],
            signature: [seed + 3; 64],
        };
        let pendings = [
            ([0xE1; 16], owner(2), 2u8),
            ([0xE2; 16], owner(3), 3u8),
            ([0xE3; 16], owner(4), 4u8),
        ];
        for (pairing, redeemer, seed) in pendings {
            assert_eq!(
                store
                    .begin_pairing(&pairing, &redeemer, &redeem(seed), &code, None, now)
                    .unwrap(),
                BeginRedeem::Begun
            );
        }
        assert_eq!(
            drive_to_commit_decision(&store, &[0xE1; 16], &owner(1), &owner(2), now),
            PreparedDecision::Committed { commit_revision: 1 }
        );
        assert_eq!(
            drive_to_commit_decision(&store, &[0xE2; 16], &owner(1), &owner(3), now),
            PreparedDecision::Committed { commit_revision: 1 }
        );
        assert_eq!(
            drive_to_commit_decision(&store, &[0xE3; 16], &owner(1), &owner(4), now),
            PreparedDecision::RedeemExhausted
        );
        assert_eq!(
            store.pairing_snapshot(&[0xE3; 16]).unwrap().unwrap().state,
            PairingState::Aborted
        );
    }

    /// P1-03 companions: aborted and swept (expired) pendings never
    /// consume the cap — a later pending still commits within it; and a
    /// revoke that aborts a pending keeps the commit-vs-revoke ordering
    /// (revoke first ⇒ commit refuses; the aborted attempt counts
    /// nothing).
    #[test]
    fn aborted_expired_and_revoked_pendings_do_not_consume_the_cap() {
        let store = RegistryStore::open_in_memory().unwrap();
        let now = 1_000_000u64;
        let ClaimOutcome::Claimed {
            code_id, revision, ..
        } = store
            .claim(&owner(1), &[1; 16], &lookup(1), 0, 1, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code = ActiveCode {
            code_id,
            owner: owner(1),
            revision,
            owner_endpoint: None,
            max_redeems: 1,
        };
        let redeem = |seed: u8, deadline: u64| RedeemRequest {
            request_id: [seed; 16],
            lookup_digest: lookup(1),
            redeemer_nonce: [seed + 1; 32],
            deadline_ms: deadline,
            request_mac: [seed + 2; 32],
            signature: [seed + 3; 64],
        };
        // A: explicitly aborted; B: swept after its deadline; C: commits.
        let pairing_a = [0xF1; 16];
        let pairing_b = [0xF2; 16];
        let pairing_c = [0xF3; 16];
        assert_eq!(
            store
                .begin_pairing(&pairing_a, &owner(2), &redeem(2, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        assert_eq!(
            store
                .begin_pairing(&pairing_b, &owner(3), &redeem(6, now + 1_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        assert!(matches!(
            store.abort_pairing(&pairing_a, now).unwrap(),
            AbortDecision::Aborted(_)
        ));
        let report = store.expire_due(now + 1_000).unwrap();
        assert_eq!(report.aborted.len(), 1);
        assert_eq!(
            report.aborted[0].pairing_id, pairing_b,
            "the swept pending is B, not the aborted A"
        );
        // C is admitted: A and B consumed nothing.
        assert_eq!(
            store
                .begin_pairing(&pairing_c, &owner(4), &redeem(10, now + 120_000), &code, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        assert_eq!(
            drive_to_commit_decision(&store, &pairing_c, &owner(1), &owner(4), now),
            PreparedDecision::Committed { commit_revision: 1 }
        );
        // Revoke racing the cap on a fresh capped code: proofs land, the
        // owner prepares, then the revoke aborts the pending before the
        // deciding Prepared — the commit attempt must fail the pairing
        // (the aborted attempt consumed no cap).
        let ClaimOutcome::Claimed {
            code_id: code_id2,
            revision: rev2,
            ..
        } = store
            .claim(&owner(5), &[2; 16], &lookup(5), 0, 1, None, now)
            .unwrap()
        else {
            unreachable!()
        };
        let code2 = ActiveCode {
            code_id: code_id2,
            owner: owner(5),
            revision: rev2,
            owner_endpoint: None,
            max_redeems: 1,
        };
        let pairing_d = [0xF4; 16];
        assert_eq!(
            store
                .begin_pairing(&pairing_d, &owner(6), &redeem(20, now + 120_000), &code2, None, now)
                .unwrap(),
            BeginRedeem::Begun
        );
        let transcript = [0x45u8; 256];
        let digest = crate::server_v2::hello2::transcript_digest(&transcript);
        assert!(store
            .store_owner_challenge(
                &pairing_d,
                &owner(5),
                &transcript,
                &digest,
                &PairProof {
                    mac: [6; 32],
                    signature: [7; 64],
                },
                now,
            )
            .unwrap());
        assert!(store
            .store_redeemer_proof(
                &pairing_d,
                &owner(6),
                &digest,
                &PairProof {
                    mac: [8; 32],
                    signature: [9; 64],
                },
                now,
            )
            .unwrap());
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_d, &owner(5), &[1; 64], &digest, now)
                .unwrap(),
            PreparedDecision::WaitingOther
        );
        assert!(matches!(
            store.revoke(&owner(5), &[3; 16], &code_id2, 1, now + 1).unwrap(),
            RevokeOutcome::Revoked { .. }
        ));
        assert_eq!(
            store
                .store_prepared_and_try_commit(&pairing_d, &owner(6), &[2; 64], &digest, now + 2)
                .unwrap(),
            PreparedDecision::Unavailable
        );
        assert_eq!(
            store.pairing_snapshot(&pairing_d).unwrap().unwrap().state,
            PairingState::Aborted
        );
        // The revoked code admits no fresh redeem regardless of cap.
        assert_eq!(store.find_active_code(&lookup(5), now + 3).unwrap(), None);
    }

    /// P2-14: the canonical claim body for default extensions is exactly
    /// the pre-wave-28 36-byte layout; non-default extensions append the
    /// unambiguous v2 tail.
    #[test]
    fn claim_body_is_legacy_bytes_for_default_extensions() {
        let legacy = claim_body(&lookup(1), 600, 0, None);
        assert_eq!(legacy.len(), 36);
        assert_eq!(&legacy[..32], &lookup(1));
        assert_eq!(&legacy[32..], &600u32.to_be_bytes());
        // Endpoint presence alone extends the body.
        let with_endpoint = claim_body(&lookup(1), 600, 0, Some(&[0xAB; 16]));
        assert_eq!(with_endpoint.len(), 41 + 16);
        assert_eq!(&with_endpoint[..36], &legacy[..]);
        // A cap alone extends the body.
        let with_cap = claim_body(&lookup(1), 600, 2, None);
        assert_eq!(with_cap.len(), 41);
        assert_eq!(&with_cap[..36], &legacy[..]);
        assert_ne!(with_cap, with_endpoint);
        // Different ttl/cap/endpoint ⇒ different bodies in every mix.
        assert_ne!(claim_body(&lookup(1), 601, 0, None), legacy);
        assert_ne!(claim_body(&lookup(2), 600, 0, None), legacy);
    }

    /// The exact body bytes a wave-28 server journaled for a claim: the
    /// extension tail was appended unconditionally (commit `7458e9f`),
    /// so a default extension produced 41 bytes ending in
    /// `cap=0[4] + presence=0[1]`. Used to fixture historical rows
    /// (R29-P2-02); this build's [`claim_body`] is the canonical form.
    fn wave28_claim_body(
        lookup_digest: &Digest32,
        ttl_secs: u32,
        max_redeems: u32,
        owner_endpoint: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(41);
        body.extend_from_slice(lookup_digest);
        body.extend_from_slice(&ttl_secs.to_be_bytes());
        body.extend_from_slice(&max_redeems.to_be_bytes());
        body.push(u8::from(owner_endpoint.is_some()));
        if let Some(endpoint) = owner_endpoint {
            body.extend_from_slice(endpoint);
        }
        body
    }

    /// R29-P2-02: the claim-body comparison folds wave-28's 41-byte
    /// default layout into the 36-byte canonical form — and nothing
    /// else: non-default tails stay byte-exact.
    #[test]
    fn claim_bodies_equivalent_folds_only_the_wave28_default_tail() {
        let canonical = claim_body(&lookup(1), 600, 0, None);
        assert_eq!(canonical.len(), 36);
        let wave28_default = wave28_claim_body(&lookup(1), 600, 0, None);
        assert_eq!(wave28_default.len(), 41);
        // The two layouts name the same canonical request (both orders).
        assert!(claim_bodies_equivalent(&wave28_default, &canonical));
        assert!(claim_bodies_equivalent(&canonical, &wave28_default));
        // Genuinely different extension or base content never matches.
        assert!(!claim_bodies_equivalent(
            &wave28_default,
            &claim_body(&lookup(1), 600, 1, None)
        ));
        assert!(!claim_bodies_equivalent(
            &wave28_default,
            &claim_body(&lookup(1), 600, 0, Some(&[0x0E; 32]))
        ));
        assert!(!claim_bodies_equivalent(
            &wave28_default,
            &claim_body(&lookup(1), 900, 0, None)
        ));
        assert!(!claim_bodies_equivalent(
            &wave28_default,
            &claim_body(&lookup(2), 600, 0, None)
        ));
        // Non-default wave-28 rows keep byte-exact comparison: identical
        // current encoding matches, any change conflicts.
        let wave28_cap2 = wave28_claim_body(&lookup(1), 600, 2, None);
        assert_eq!(wave28_cap2, claim_body(&lookup(1), 600, 2, None));
        assert!(claim_bodies_equivalent(
            &wave28_cap2,
            &claim_body(&lookup(1), 600, 2, None)
        ));
        assert!(!claim_bodies_equivalent(
            &wave28_cap2,
            &claim_body(&lookup(1), 600, 3, None)
        ));
        assert!(!claim_bodies_equivalent(
            &wave28_cap2,
            &claim_body(&lookup(1), 600, 2, Some(&[0x0E; 8]))
        ));
    }

    /// Create a pre-wave-28 (schema v1) database exactly as waves 18-27
    /// wrote it (no endpoint columns, no cap column) holding one active
    /// code row, and optionally one legacy request_journal entry.
    fn create_v1_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE invite_codes (
               lookup_digest       BLOB(32) PRIMARY KEY,
               code_id             BLOB(16) UNIQUE NOT NULL,
               owner_device_id     BLOB(32) NOT NULL,
               created_unix_ms     INTEGER NOT NULL,
               expires_unix_ms     INTEGER NULL,
               state               TEXT NOT NULL,
               revision            INTEGER NOT NULL,
               create_request_id   BLOB(16) NOT NULL,
               UNIQUE (owner_device_id, create_request_id)
             );
             CREATE TABLE pairing_transactions (
               pairing_id                  BLOB(16) PRIMARY KEY,
               request_id                  BLOB(16) NOT NULL,
               code_id                     BLOB(16) NOT NULL,
               lookup_digest               BLOB(32) NOT NULL,
               owner_device_id             BLOB(32) NOT NULL,
               redeemer_device_id          BLOB(32) NOT NULL,
               state                       TEXT NOT NULL,
               transcript                  BLOB(256),
               transcript_digest           BLOB(32),
               owner_proof_mac             BLOB(32),
               owner_proof_signature       BLOB(64),
               redeemer_proof_mac          BLOB(32),
               redeemer_proof_signature    BLOB(64),
               owner_prepared_signature    BLOB(64),
               redeemer_prepared_signature BLOB(64),
               commit_revision             INTEGER,
               owner_applied_signature     BLOB(64),
               redeemer_applied_signature  BLOB(64),
               redeemer_nonce              BLOB(32) NOT NULL,
               request_mac                 BLOB(32) NOT NULL,
               redeemer_signature          BLOB(64) NOT NULL,
               deadline_unix_ms            INTEGER NOT NULL,
               code_revision               INTEGER NOT NULL,
               created_unix_ms             INTEGER NOT NULL,
               updated_unix_ms             INTEGER NOT NULL
             );
             CREATE TABLE pairing_relations (
               pairing_id         BLOB(16) PRIMARY KEY,
               code_id            BLOB(16) NOT NULL,
               lookup_digest      BLOB(32) NOT NULL,
               owner_device_id    BLOB(32) NOT NULL,
               redeemer_device_id BLOB(32) NOT NULL,
               transcript_digest  BLOB(32) NOT NULL,
               commit_revision    INTEGER NOT NULL,
               completed_unix_ms  INTEGER NOT NULL
             );
             CREATE TABLE request_journal (
               owner_device_id BLOB(32) NOT NULL,
               request_id      BLOB(16) NOT NULL,
               request_kind    TEXT NOT NULL,
               request_body    BLOB NOT NULL,
               response        BLOB NOT NULL,
               created_unix_ms INTEGER NOT NULL,
               PRIMARY KEY (owner_device_id, request_id)
             );
             PRAGMA user_version = 1;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO invite_codes
             (lookup_digest, code_id, owner_device_id, created_unix_ms, expires_unix_ms,
              state, revision, create_request_id)
             VALUES (?1,?2,?3,1000,NULL,'active',1,?4)",
            params![
                &lookup(1).as_slice(),
                &[0x42u8; 16].as_slice(),
                &owner(1).as_slice(),
                &[9u8; 16].as_slice()
            ],
        )
        .unwrap();
    }

    #[test]
    fn v1_database_migrates_to_v2_preserving_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        create_v1_database(&path);
        // Reopening with the wave-28 build migrates 1 → 2 additively.
        let store = RegistryStore::open(&path).unwrap();
        {
            let conn = store.lock();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
        }
        // The v1 row survived and reads with the legacy defaults.
        let code = store.find_active_code(&lookup(1), 2_000).unwrap().unwrap();
        assert_eq!(code.code_id, [0x42u8; 16]);
        assert_eq!(code.owner_endpoint, None);
        assert_eq!(code.max_redeems, 0);
        // The migrated tables accept wave-28 writes.
        assert!(matches!(
            store
                .claim(&owner(1), &[1; 16], &lookup(9), 0, 1, Some(&[0x0F; 32]), 2_000)
                .unwrap(),
            ClaimOutcome::Claimed { .. }
        ));
    }

    /// P2-13: a migration interrupted mid-flight (as the pre-fix
    /// non-atomic DDL could leave it — some columns added, user_version
    /// still 1) must be recognized and completed on the next startup,
    /// preserving every row. Interruptions under the fixed code roll back
    /// wholesale, so reruns always converge.
    #[test]
    fn interrupted_migration_resumes_and_completes() {
        let partial_states: [&[&str]; 3] = [
            // Only the first ALTER landed before the crash.
            &["ALTER TABLE invite_codes ADD COLUMN owner_endpoint BLOB NULL"],
            // First three landed.
            &[
                "ALTER TABLE invite_codes ADD COLUMN owner_endpoint BLOB NULL",
                "ALTER TABLE invite_codes ADD COLUMN max_redeems INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE pairing_transactions ADD COLUMN owner_endpoint BLOB NULL",
            ],
            // All four landed but the version stamp never did.
            &[
                "ALTER TABLE invite_codes ADD COLUMN owner_endpoint BLOB NULL",
                "ALTER TABLE invite_codes ADD COLUMN max_redeems INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE pairing_transactions ADD COLUMN owner_endpoint BLOB NULL",
                "ALTER TABLE pairing_transactions ADD COLUMN redeemer_endpoint BLOB NULL",
            ],
        ];
        for (index, applied) in partial_states.iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("reg.sqlite");
            create_v1_database(&path);
            {
                let conn = Connection::open(&path).unwrap();
                for statement in applied.iter() {
                    conn.execute_batch(statement).unwrap();
                }
                let version: i64 = conn
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(version, 1, "case {index}: still stamped v1");
            }
            // Restart completes the migration instead of failing on the
            // already-present columns.
            let store = RegistryStore::open(&path)
                .unwrap_or_else(|e| panic!("case {index}: reopen must migrate, got {e:?}"));
            {
                let conn = store.lock();
                let version: i64 = conn
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(version, SCHEMA_VERSION, "case {index}");
            }
            // The pre-existing row survived untouched.
            let code = store.find_active_code(&lookup(1), 2_000).unwrap().unwrap();
            assert_eq!(code.code_id, [0x42u8; 16], "case {index}");
            assert_eq!(code.owner_endpoint, None, "case {index}");
            assert_eq!(code.max_redeems, 0, "case {index}");
            // The completed layout accepts wave-28 writes.
            assert!(matches!(
                store
                    .claim(&owner(7), &[8; 16], &lookup(8), 0, 1, None, 2_000)
                    .unwrap(),
                ClaimOutcome::Claimed { .. }
            ));
        }
    }

    /// P2-14: a claim journaled by a pre-wave-28 server (36-byte body:
    /// digest + ttl) whose response never reached the client must replay
    /// the original `InviteClaimed` after the upgrade — while a genuinely
    /// different request (other ttl, or newly added extension content)
    /// still conflicts.
    #[test]
    fn upgrade_replays_legacy_claim_journal_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        create_v1_database(&path);
        // The pre-upgrade server journaled this claim with the legacy
        // canonical body and the encoded InviteClaimed response.
        let legacy_body = claim_body(&lookup(7), 600, 0, None);
        assert_eq!(legacy_body.len(), 36);
        let legacy_response = encode_response(&CtrlMessage::InviteClaimed {
            request_id: [0x77; 16],
            code_id: [0x42u8; 16],
            created_ms: 1000,
            expires_ms: 1000 + 600_000,
            revision: 1,
        })
        .unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO request_journal
                 (owner_device_id, request_id, request_kind, request_body, response, created_unix_ms)
                 VALUES (?1,?2,'claim',?3,?4,1000)",
                params![
                    &owner(2).as_slice(),
                    &[0x77u8; 16].as_slice(),
                    &legacy_body,
                    &legacy_response
                ],
            )
            .unwrap();
        }
        let store = RegistryStore::open(&path).unwrap();
        // The same request retried after the upgrade: the exact original
        // answer, not REQUEST_CONFLICT.
        match store
            .claim(&owner(2), &[0x77; 16], &lookup(7), 600, 0, None, 9_000)
            .unwrap()
        {
            ClaimOutcome::Replay(bytes) => assert_eq!(bytes, legacy_response),
            other => panic!("legacy journal entry must replay, got {other:?}"),
        }
        // Genuinely different content under the same id still conflicts.
        assert_eq!(
            store
                .claim(&owner(2), &[0x77; 16], &lookup(7), 900, 0, None, 9_001)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(2), &[0x77; 16], &lookup(8), 600, 0, None, 9_002)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // Newly added extension content under the same id is a change of
        // content, not a replay.
        assert_eq!(
            store
                .claim(&owner(2), &[0x77; 16], &lookup(7), 600, 1, None, 9_003)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(2), &[0x77; 16], &lookup(7), 600, 0, Some(&[0x0E; 32]), 9_004)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
    }

    /// R29-P2-02: wave-28 journaled default-extension claims with the
    /// explicit all-zero tail (41 bytes) — the schema-v2 library an
    /// actual wave-28 deployment holds. After upgrading that library to
    /// this build, retrying the original request must replay the
    /// original `InviteClaimed` (not REQUEST_CONFLICT), the stored row
    /// must stay untouched (no disk rewrite), and genuinely different
    /// content must still conflict.
    #[test]
    fn wave28_default_journal_rows_replay_after_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        let now = 1_000_000u64;
        let wave28_body = wave28_claim_body(&lookup(3), 600, 0, None);
        assert_eq!(wave28_body.len(), 41);
        let original_response = {
            let store = RegistryStore::open(&path).unwrap();
            assert!(matches!(
                store
                    .claim(&owner(3), &[0x33; 16], &lookup(3), 600, 0, None, now)
                    .unwrap(),
                ClaimOutcome::Claimed { .. }
            ));
            let conn = store.lock();
            // Rewrite the journal body to exactly what the wave-28 build
            // persisted for this request (unconditional extension tail).
            conn.execute(
                "UPDATE request_journal SET request_body=?3
                 WHERE owner_device_id=?1 AND request_id=?2",
                params![
                    owner(3).as_slice(),
                    &[0x33u8; 16].as_slice(),
                    &wave28_body
                ],
            )
            .unwrap();
            let response: Vec<u8> = conn
                .query_row(
                    "SELECT response FROM request_journal
                     WHERE owner_device_id=?1 AND request_id=?2",
                    params![owner(3).as_slice(), &[0x33u8; 16].as_slice()],
                    |row| row.get(0),
                )
                .unwrap();
            response
        };
        // Upgrade: the same database file reopened by this build.
        let store = RegistryStore::open(&path).unwrap();
        // The original request retried: the exact original answer.
        match store
            .claim(&owner(3), &[0x33; 16], &lookup(3), 600, 0, None, now + 1)
            .unwrap()
        {
            ClaimOutcome::Replay(bytes) => assert_eq!(bytes, original_response),
            other => panic!("wave-28 default journal row must replay, got {other:?}"),
        }
        // Replay never rewrote the historical row: still the 41B layout.
        {
            let conn = store.lock();
            let body: Vec<u8> = conn
                .query_row(
                    "SELECT request_body FROM request_journal
                     WHERE owner_device_id=?1 AND request_id=?2",
                    params![owner(3).as_slice(), &[0x33u8; 16].as_slice()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(body, wave28_body);
        }
        // The same id now carrying a non-default extension (cap=1) is a
        // change of content, not a replay.
        assert_eq!(
            store
                .claim(&owner(3), &[0x33; 16], &lookup(3), 600, 1, None, now + 2)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // Likewise a newly added endpoint, a different ttl, a different
        // digest.
        assert_eq!(
            store
                .claim(&owner(3), &[0x33; 16], &lookup(3), 600, 0, Some(&[0x0D; 32]), now + 3)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(3), &[0x33; 16], &lookup(3), 900, 0, None, now + 4)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(3), &[0x33; 16], &lookup(4), 600, 0, None, now + 5)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // And the replay answer stays stable across further retries.
        assert!(matches!(
            store
                .claim(&owner(3), &[0x33; 16], &lookup(3), 600, 0, None, now + 6)
                .unwrap(),
            ClaimOutcome::Replay(_)
        ));
    }

    /// R29-P2-02: a store holding a mix of journal layouts — a wave-28
    /// 41B default row, a current 36B default row, and a non-default 41B
    /// row — replays each request id against its own original answer;
    /// no layout leaks into another row's comparison.
    #[test]
    fn mixed_journal_layouts_replay_independently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        let now = 2_000_000u64;
        let mut expected = std::collections::HashMap::new();
        {
            let store = RegistryStore::open(&path).unwrap();
            // r1: 41B default row as wave-28 wrote it (rewritten below).
            assert!(matches!(
                store
                    .claim(&owner(4), &[0x51; 16], &lookup(5), 600, 0, None, now)
                    .unwrap(),
                ClaimOutcome::Claimed { .. }
            ));
            // r2: 36B default row as this build writes it.
            assert!(matches!(
                store
                    .claim(&owner(4), &[0x52; 16], &lookup(6), 600, 0, None, now)
                    .unwrap(),
                ClaimOutcome::Claimed { .. }
            ));
            // r3: non-default extension — byte-identical under wave-28
            // and this build.
            assert!(matches!(
                store
                    .claim(&owner(4), &[0x53; 16], &lookup(7), 600, 2, None, now)
                    .unwrap(),
                ClaimOutcome::Claimed { .. }
            ));
            let conn = store.lock();
            conn.execute(
                "UPDATE request_journal SET request_body=?3
                 WHERE owner_device_id=?1 AND request_id=?2",
                params![
                    owner(4).as_slice(),
                    &[0x51u8; 16].as_slice(),
                    wave28_claim_body(&lookup(5), 600, 0, None)
                ],
            )
            .unwrap();
            for seed in [0x51u8, 0x52, 0x53] {
                let response: Vec<u8> = conn
                    .query_row(
                        "SELECT response FROM request_journal
                         WHERE owner_device_id=?1 AND request_id=?2",
                        params![owner(4).as_slice(), &[seed; 16].as_slice()],
                        |row| row.get(0),
                    )
                    .unwrap();
                expected.insert(seed, response);
            }
        }
        let store = RegistryStore::open(&path).unwrap();
        // Each layout replays its own original answer with matching
        // content: 41B default, 36B default, non-default tail.
        assert_eq!(
            store
                .claim(&owner(4), &[0x51; 16], &lookup(5), 600, 0, None, now + 1)
                .unwrap(),
            ClaimOutcome::Replay(expected[&0x51].clone())
        );
        assert_eq!(
            store
                .claim(&owner(4), &[0x52; 16], &lookup(6), 600, 0, None, now + 2)
                .unwrap(),
            ClaimOutcome::Replay(expected[&0x52].clone())
        );
        assert_eq!(
            store
                .claim(&owner(4), &[0x53; 16], &lookup(7), 600, 2, None, now + 3)
                .unwrap(),
            ClaimOutcome::Replay(expected[&0x53].clone())
        );
        // Crossing layouts under the same id is still a conflict: a
        // default retry against the non-default row...
        assert_eq!(
            store
                .claim(&owner(4), &[0x53; 16], &lookup(7), 600, 0, None, now + 4)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // ...a cap=1 retry against either default row (41B or 36B)...
        assert_eq!(
            store
                .claim(&owner(4), &[0x51; 16], &lookup(5), 600, 1, None, now + 5)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        assert_eq!(
            store
                .claim(&owner(4), &[0x52; 16], &lookup(6), 600, 1, None, now + 6)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
        // ...and swapping base content between the two default rows' ids.
        assert_eq!(
            store
                .claim(&owner(4), &[0x51; 16], &lookup(6), 600, 0, None, now + 7)
                .unwrap(),
            ClaimOutcome::RequestConflict
        );
    }

    #[test]
    fn restart_reopen_preserves_codes_and_pending_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        let now = 1_000_000u64;
        let pairing_id = [0x9A; 16];
        let code_id;
        {
            let store = RegistryStore::open(&path).unwrap();
            let ClaimOutcome::Claimed {
                code_id: id,
                revision,
                ..
            } = store
                .claim(&owner(1), &[1; 16], &lookup(1), 0, 0, None, now)
                .unwrap()
            else {
                unreachable!()
            };
            code_id = id;
            let code = ActiveCode {
                code_id: id,
                owner: owner(1),
                revision,
                owner_endpoint: None,
                max_redeems: 0,
            };
            let redeem = RedeemRequest {
                request_id: [2; 16],
                lookup_digest: lookup(1),
                redeemer_nonce: [3; 32],
                deadline_ms: now + 120_000,
                request_mac: [4; 32],
                signature: [5; 64],
            };
            store
                .begin_pairing(&pairing_id, &owner(2), &redeem, &code, None, now)
                .unwrap();
        }
        // Process restart: same file, fresh connection.
        let store = RegistryStore::open(&path).unwrap();
        let found = store
            .find_active_code(&lookup(1), now + 1)
            .unwrap()
            .unwrap();
        assert_eq!(found.code_id, code_id);
        let offers = store.pending_offers_for_owner(&owner(1)).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].pairing_id, pairing_id);
        // The claimed code is not re-issuable after restart either.
        assert_eq!(
            store
                .claim(&owner(9), &[8; 16], &lookup(1), 0, 0, None, now + 2)
                .unwrap(),
            ClaimOutcome::CodeConflict
        );
    }
}
