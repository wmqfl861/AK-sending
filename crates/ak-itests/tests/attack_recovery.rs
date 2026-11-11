//! Attack- and interruption-observable recovery tests (wave 24, review
//! 2026-11-05 "最值得做的改进" #2): what an honest device can OBSERVE after
//! a hostile registry substitution or a plain mid-flight crash, proven on
//! the real v2 registry (TLS + SQLite) and — where the product path is the
//! point — real `ak-cli` processes.
//!
//! Covered:
//!
//! * **Malicious registry, swapped keys**: a registry whose stored commit
//!   carries an attacker-swapped device key and attacker self-signed
//!   proofs (built through the registry's own public store API — exactly
//!   the freedom a hostile registry has over its own database) cannot
//!   pass recovery: the re-sent `PairCommit` fails the local
//!   authenticated-transcript pin, no trust is booked, the journal stays
//!   `pending`, and the server never sees a `PairApplied`.
//! * **Honest interruption (control group)**: the same crash checkpoint
//!   recovered against an honest registry reaches `Ready` on BOTH sides —
//!   both `peers.book` files cross-record, the journal reaches `applied`,
//!   the authoritative state is `Ready`.
//! * **Legacy journal**: an entry with neither a code hint nor a pin is
//!   refused outright (the server's self-reported state is never the only
//!   evidence); the journal stays `pending`, no book appears, the server
//!   stays `Committed`.
//! * **Secret hygiene through the Debug/error/output chain**:
//!   `format!("{:?}", CreatedInvite)` never contains the bearer code or
//!   the auth key; the CLI output chain of the owner-side invite store
//!   (`invite create` / `invite list`) never echoes the bearer secret
//!   outside `create`'s deliberate display line.
//! * **Registry shutdown keeps established trust**: killing the registry
//!   leaves both committed `peers.book` files byte-identical; a fresh
//!   redeem against the dead registry fails and writes nothing.
//! * **Idempotent network retry**: replaying the same invite-claim
//!   `request_id` registers exactly ONE code (server-side count via the
//!   store), and replaying the same redeem `request_id` returns the SAME
//!   pairing id — exactly one server-side transaction.
#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ak_identity::invite::InviteCode;
use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairTranscript, PairingVerifyError, OWNER_PROOF_DOMAIN,
    REDEEMER_PROOF_DOMAIN,
};
use ak_identity::DeviceKeyPair;
use ak_session::pairing::{
    redeem_request_mac, CreatedInvite, OrchestratorConfig, PairFlowError, PairingOrchestrator,
    RegistryClient, BOOK_NAME,
};
use ak_signal::protocol_v2::message::PAIR_UNAVAILABLE;
use ak_signal::protocol_v2::{PairingState, RedeemRequest, StateFilter};
use ak_signal::server_v2::engine::serve_v2;
use ak_signal::server_v2::hello2;
use ak_signal::server_v2::store::{BeginRedeem, PreparedDecision, RegistryStore};
use ak_signal::server_v2::Limits;
use ak_store::{PairingJournal, PairingPhase, PendingPairing};
use tokio::net::TcpListener as TokioListener;
use tokio::sync::watch;

// ---- in-process registry (file-backed so the test can hold a second
// store handle — the stand-in for "what a malicious registry stores") ----

struct TestRegistry {
    addr: std::net::SocketAddr,
    registry_id: [u8; 32],
    shutdown: watch::Sender<bool>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn spawn_registry(db: &Path) -> TestRegistry {
    let registry_key = DeviceKeyPair::generate();
    let listener = TokioListener::bind("127.0.0.1:0")
        .await
        .expect("bind v2 port");
    let addr = listener.local_addr().expect("local addr");
    let store = RegistryStore::open(db).expect("open registry store");
    let (shutdown, shutdown_rx) = watch::channel(false);
    let key = registry_key.clone();
    tokio::spawn(async move {
        let _ = serve_v2(listener, &key, store, Limits::default(), shutdown_rx).await;
    });
    TestRegistry {
        addr,
        registry_id: registry_key.public_key(),
        shutdown,
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn test_config() -> OrchestratorConfig {
    OrchestratorConfig {
        redeem_window_ms: 30_000,
        ready_wait_ms: 10_000,
        claim_conflict_retries: 8,
    }
}

/// One in-process device: real registry client + orchestrator on `dir`.
async fn connect_device(
    registry: &TestRegistry,
    name: &str,
    key: &DeviceKeyPair,
    dir: &Path,
) -> PairingOrchestrator {
    std::fs::create_dir_all(dir).expect("create device dir");
    let client = RegistryClient::connect(registry.addr, registry.registry_id, key, name)
        .await
        .expect("device connects");
    PairingOrchestrator::open(client, key.clone(), dir, test_config()).expect("open orchestrator")
}

fn book_text(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(BOOK_NAME)).ok()
}

fn book_contains(dir: &Path, hex: &str) -> bool {
    book_text(dir).is_some_and(|text| text.contains(hex))
}

/// Poll a predicate on a 50 ms cadence until it holds or `budget` elapses.
async fn wait_for(mut probe: impl FnMut() -> bool, budget: Duration, what: &str) {
    let deadline = Instant::now() + budget;
    while !probe() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---- authenticated-transcript pin sidecar (frozen on-disk format) -----------

/// The `<pairing hex>.akdgm` sidecar the orchestrator's private
/// `write_digest_sidecar` persists before every `pending` journal entry.
/// The exact frozen format is replicated here so the redeemer-side crash
/// checkpoint can be staged for the malicious-registry scenario (the
/// orchestrator's own unit tests prove the format the production writer
/// emits; recovery reads it back with the same parser).
fn write_pin_sidecar(journal_dir: &Path, pairing_id: &[u8; 16], digest: &[u8; 32]) {
    std::fs::create_dir_all(journal_dir).expect("journal dir");
    let text = format!(
        "AKDGM1\npairing {}\ndigest {}\n",
        hex_16(pairing_id),
        hex_bytes(digest)
    );
    let path = journal_dir.join(format!("{}.akdgm", hex_16(pairing_id)));
    std::fs::write(&path, text.as_bytes())
        .unwrap_or_else(|err| panic!("cannot write pin {}: {err}", path.display()));
}

fn hex_16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Load the journal records of one identity dir (assertion helper).
fn journal_records(dir: &Path) -> Vec<ak_store::PairingRecord> {
    PairingJournal::open(&dir.join("pairing"))
        .expect("journal opens")
        .load_all()
        .expect("journal loads")
}

// ---- 1: malicious registry swaps the device keys -----------------------------

/// The P1-1 attack shape at the integration boundary. A redeemer crashed
/// right after verifying an honest `PairCommit`; its local evidence is the
/// authenticated-transcript pin. The registry then substitutes its stored
/// commit for one whose transcript names the ATTACKER's key, with proofs
/// the attacker self-signs (the MACs cannot be forged without the invite
/// secret — and do not need to be, because the pre-fix degraded check only
/// verified signatures under the transcript's own self-declared keys).
///
/// Observables after recovery:
/// * `recover(&[])` fails with `Verify(TranscriptDigestMismatch)`;
/// * the journal entry stays `pending` and `peers.book` is never written;
/// * the server-side transaction stays `Committed` with no redeemer
///   `PairApplied` — the refusal happened before anything was sent.
#[tokio::test]
async fn swapped_key_commit_from_a_malicious_registry_is_refused_by_the_pin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;
    // The test's second store handle on the same SQLite file: the
    // malicious registry's freedom to store whatever it wants.
    let evil = RegistryStore::open(&db).expect("probe store opens the same db");

    let owner_key = DeviceKeyPair::generate();
    let redeemer_key = DeviceKeyPair::generate();
    let attacker_key = DeviceKeyPair::generate();
    let a_dir = tmp.path().join("owner");
    let b_dir = tmp.path().join("redeemer");
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;

    // A REAL code claimed over the wire (the transaction below redeems it).
    let invite: CreatedInvite = a.create_invite(0).await.expect("claim invite");
    assert_ne!(invite.code_id, [0u8; 16], "sanity: code id assigned");

    // The transcript the redeemer authenticated pre-crash: every binding
    // real (registry id, the wire-claimed code, both real device keys).
    let transcript = PairTranscript {
        registry_id: registry.registry_id,
        code_id: invite.code_id,
        code_revision: invite.revision,
        pairing_id: random_id16(),
        request_id: random_id16(),
        lookup_digest: invite.lookup_digest,
        owner_device_id: owner_key.public_key(),
        redeemer_device_id: redeemer_key.public_key(),
        owner_nonce: random_nonce32(),
        redeemer_nonce: random_nonce32(),
        deadline_ms: now_unix_ms() + 600_000,
    };
    let pin = transcript.transcript_digest();

    // The attacker's substitution: same transcript, OWNER KEY SWAPPED to
    // the attacker's, both proofs self-signed under the swapped keys with
    // unforgeable-as-forged (all-zero) MACs.
    let mut swapped = transcript;
    swapped.owner_device_id = attacker_key.public_key();
    let swapped_bytes = swapped.to_bytes();
    let mac = [0u8; 32];
    let owner_proof = ak_signal::protocol_v2::PairProof {
        mac,
        signature: attacker_key.sign(&hello2::proof_signed_message(
            OWNER_PROOF_DOMAIN,
            &swapped_bytes,
            &mac,
        )),
    };
    let redeemer_proof = ak_signal::protocol_v2::PairProof {
        mac,
        signature: attacker_key.sign(&hello2::proof_signed_message(
            REDEEMER_PROOF_DOMAIN,
            &swapped_bytes,
            &mac,
        )),
    };
    // The attack's whole point: the forged signature DOES verify under the
    // key the tampered transcript names — self-attestation the pre-fix
    // check happily accepted (proven here so the refusal below is known to
    // come from the pin, not from a broken signature).
    assert!(DeviceKeyPair::verify(
        &swapped.owner_device_id,
        &hello2::proof_signed_message(OWNER_PROOF_DOMAIN, &swapped_bytes, &mac),
        &owner_proof.signature
    ));
    assert_ne!(
        swapped.transcript_digest(),
        pin,
        "the swapped transcript cannot reproduce the pinned digest"
    );

    // Stage the malicious committed transaction server-side through the
    // registry's own store API (begin → challenge → proof → prepared).
    let now = now_unix_ms();
    let code = evil
        .find_active_code(&invite.lookup_digest, now)
        .expect("code lookup")
        .expect("the wire-claimed code is active");
    let redeem = RedeemRequest {
        request_id: transcript.request_id,
        lookup_digest: invite.lookup_digest,
        redeemer_nonce: transcript.redeemer_nonce,
        deadline_ms: transcript.deadline_ms,
        request_mac: [0x07; 32], // the store persists, never verifies, MACs
        signature: redeemer_key.sign(&[0xA5; 32]), // arbitrary bound bytes
    };
    assert_eq!(
        evil.begin_pairing(
            &transcript.pairing_id,
            &redeemer_key.public_key(),
            &redeem,
            &code,
            None, // wave-28: endpoint blob (this attack path carries none)
            now
        )
        .expect("begin"),
        BeginRedeem::Begun
    );
    assert!(
        evil.store_owner_challenge(
            &transcript.pairing_id,
            &owner_key.public_key(),
            &swapped_bytes,
            &swapped.transcript_digest(),
            &owner_proof,
            now
        )
        .expect("store challenge"),
        "challenge stored"
    );
    assert!(
        evil.store_redeemer_proof(
            &transcript.pairing_id,
            &redeemer_key.public_key(),
            &swapped.transcript_digest(),
            &redeemer_proof,
            now
        )
        .expect("store proof"),
        "proof stored"
    );
    assert_eq!(
        evil.store_prepared_and_try_commit(
            &transcript.pairing_id,
            &owner_key.public_key(),
            &[0x11; 64],
            &swapped.transcript_digest(),
            now
        )
        .expect("owner prepared"),
        PreparedDecision::WaitingOther
    );
    assert_eq!(
        evil.store_prepared_and_try_commit(
            &transcript.pairing_id,
            &redeemer_key.public_key(),
            &[0x22; 64],
            &swapped.transcript_digest(),
            now
        )
        .expect("redeemer prepared commits"),
        PreparedDecision::Committed { commit_revision: 1 }
    );

    // The redeemer's honest pre-crash local state: pending journal entry
    // plus the pin of the transcript it authenticated.
    PairingJournal::open(&b_dir.join("pairing"))
        .expect("journal opens")
        .record_pending(&PendingPairing {
            pairing_id: transcript.pairing_id,
            code_id: invite.code_id,
            lookup_digest: invite.lookup_digest,
            secret_ref: random_id16(),
            peer_public_key: Some(owner_key.public_key()),
        })
        .expect("stage pending entry");
    write_pin_sidecar(&b_dir.join("pairing"), &transcript.pairing_id, &pin);

    // Recovery over the real wire: the registry re-sends the tampered
    // commit, the pin refuses it.
    let b = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;
    let err = b
        .recover(&[])
        .await
        .expect_err("swapped-key commit must be refused");
    match &err {
        PairFlowError::Verify(PairingVerifyError::TranscriptDigestMismatch) => {}
        other => panic!("expected the pinned-digest refusal, got {other:?}"),
    }

    // Observable damage: NONE. Journal still pending, no trust booked.
    let records = journal_records(&b_dir);
    assert_eq!(records.len(), 1, "exactly the one staged entry");
    assert_eq!(records[0].phase, PairingPhase::Pending);
    assert!(!b_dir.join(BOOK_NAME).exists(), "no peers.book may appear");
    // Server-side: the transaction is still Committed with no applied
    // signature from this redeemer — nothing was confirmed after refusal.
    let snapshot = evil
        .pairing_snapshot(&transcript.pairing_id)
        .expect("snapshot")
        .expect("transaction exists");
    assert_eq!(snapshot.state, PairingState::Committed);
    assert!(
        snapshot.redeemer_applied_signature.is_none(),
        "the refused recovery never sent PairApplied"
    );
    assert_eq!(snapshot.transcript, Some(swapped_bytes));
}

// ---- 2: honest interruption recovered to Ready on both sides (control) ------

/// The same crash checkpoint against the honest registry: the re-sent
/// commit reproduces the pin byte-for-byte, the redeemer finishes
/// `PairApplied`, and BOTH sides end Ready — the redeemer's book records
/// the owner, the owner's book records the redeemer, the journal reaches
/// `applied`, and the authoritative state is `Ready`.
#[tokio::test]
async fn honest_interrupt_recovery_matches_the_pin_and_reaches_ready_on_both_sides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;
    let probe = RegistryStore::open(&db).expect("probe store");

    let owner_key = DeviceKeyPair::generate();
    let redeemer_key = DeviceKeyPair::generate();
    let a_dir = tmp.path().join("owner");
    let b_dir = tmp.path().join("redeemer");
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;

    let invite = a.create_invite(0).await.expect("claim invite");
    a.spawn_owner_loop();

    // Redeem to the verified commit, then "crash" (connection closed,
    // orchestrator dropped) before any local apply.
    let b = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;
    let committed = b
        .redeem_to_commit(&invite.display)
        .await
        .expect("redeem to commit");
    b.client().close().await;
    drop(b);

    // The owner side finishes on its own: its trust book gains the
    // redeemer (written during its finish_committed, before PairReady).
    let redeemer_hex = redeemer_key.fingerprint().to_hex();
    wait_for(
        || book_contains(&a_dir, &redeemer_hex),
        Duration::from_secs(20),
        "the owner book to record the redeemer",
    )
    .await;

    // Restart the redeemer WITHOUT the code: recovery must succeed purely
    // on the local pin (the digest sidecar the honest flow wrote).
    let b2 = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;
    let applied = b2.recover(&[]).await.expect("recovery via the pin");
    assert_eq!(applied, vec![committed.pairing_id]);

    // Both sides Ready: both books cross-record, journal applied.
    let owner_hex = owner_key.fingerprint().to_hex();
    assert!(book_contains(&b_dir, &owner_hex), "redeemer book has owner");
    assert!(
        book_contains(&a_dir, &redeemer_hex),
        "owner book has redeemer"
    );
    let records = journal_records(&b_dir);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].phase, PairingPhase::Applied);

    // Authoritative state: Ready (both applied), committed revision kept.
    let snapshot = probe
        .pairing_snapshot(&committed.pairing_id)
        .expect("snapshot")
        .expect("transaction exists");
    assert_eq!(snapshot.state, PairingState::Ready);
    assert_eq!(
        snapshot.commit_revision,
        Some(committed.commit_revision),
        "the re-verified commit kept its revision"
    );

    // Idempotent: a second recovery pass has nothing left to do.
    let again = b2.recover(&[]).await.expect("second recovery");
    assert!(again.is_empty(), "{again:?}");
}

// ---- 3: legacy journal entry without pin or code is refused -------------------

/// A pre-pin journal (the review's fail-closed requirement): the entry
/// exists, the operator supplies no code, and no authenticated-transcript
/// digest was ever persisted. Even against an HONEST registry whose
/// re-sent commit is perfectly valid, recovery must refuse — the server's
/// self-reported `Committed` state is never the only evidence.
#[tokio::test]
async fn legacy_journal_without_sidecar_or_code_is_refused_not_degraded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;
    let probe = RegistryStore::open(&db).expect("probe store");

    let owner_key = DeviceKeyPair::generate();
    let redeemer_key = DeviceKeyPair::generate();
    let a_dir = tmp.path().join("owner");
    let b_dir = tmp.path().join("redeemer");
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;
    let invite = a.create_invite(0).await.expect("claim invite");
    a.spawn_owner_loop();

    let b = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;
    let committed = b
        .redeem_to_commit(&invite.display)
        .await
        .expect("redeem to commit");
    b.client().close().await;
    drop(b);

    // Strip the pin: the exact state a pre-fix (legacy) journal would be
    // in after the crash.
    let pin_path = b_dir
        .join("pairing")
        .join(format!("{}.akdgm", hex_16(&committed.pairing_id)));
    assert!(
        pin_path.is_file(),
        "the honest flow wrote the pin: {pin_path:?}"
    );
    std::fs::remove_file(&pin_path).expect("strip the pin");

    let b2 = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;
    let err = b2
        .recover(&[])
        .await
        .expect_err("no code and no pin must be refused");
    match &err {
        PairFlowError::Local(msg) => assert_eq!(
            msg,
            "cannot verify without invite code or authenticated transcript"
        ),
        other => panic!("expected the explicit refusal, got {other:?}"),
    }

    // Nothing moved: pending entry, no book, and the authoritative state
    // never reached Ready — only the OWNER's apply is recorded (Applied),
    // the refused redeemer contributed nothing.
    let records = journal_records(&b_dir);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].phase, PairingPhase::Pending);
    assert!(!b_dir.join(BOOK_NAME).exists());
    let snapshot = probe
        .pairing_snapshot(&committed.pairing_id)
        .expect("snapshot")
        .expect("transaction exists");
    assert_ne!(snapshot.state, PairingState::Ready);
    assert!(
        snapshot.redeemer_applied_signature.is_none(),
        "the refused recovery never sent its PairApplied: {snapshot:?}"
    );
}

// ---- 4: secret hygiene through Debug and the CLI output chain -----------------

/// Two-layer sentinel:
///
/// * in-process, `format!("{:?}", CreatedInvite)` — no bearer code, no
///   `AKI1-` token, no auth-key hex, no raw lookup-digest hex (only the
///   documented placeholders and the masked hint);
/// * the observable output chain of the owner-side invite store through
///   the real CLI: `invite list` prints the masked hint but never the
///   plaintext code, and neither `create` nor `list` ever echoes an
///   `AKI1-` bearer on stderr. (`StoredInvite`'s Debug redaction itself
///   is unit-pinned inside ak-cli; from this crate the reachable
///   observable is the process output chain.)
#[tokio::test]
async fn debug_and_cli_output_chains_never_leak_invite_secrets() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;
    let a_dir = tmp.path().join("owner");
    let owner_key = DeviceKeyPair::generate();
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;
    let invite = a.create_invite(0).await.expect("claim invite");

    // In-process Debug sentinel over the exact created secret material.
    let code = InviteCode::parse(&invite.display).expect("display parses");
    let rendered = format!("{invite:?}");
    assert!(
        !rendered.contains(&invite.display),
        "Debug must not leak the bearer code: {rendered}"
    );
    let without_hint = rendered.replace(&invite.hint, "");
    assert!(
        !without_hint.contains("AKI1-"),
        "outside the masked hint, Debug carries no code token: {rendered}"
    );
    assert!(
        !rendered.contains(&hex_bytes(&code.auth_key())),
        "Debug must not leak the auth key hex: {rendered}"
    );
    assert!(
        !rendered.contains(&hex_bytes(&code.lookup_digest())),
        "only the digest placeholder belongs in Debug: {rendered}"
    );
    assert!(rendered.contains("redacted"), "the redaction is marked");
    assert!(
        rendered.contains(&invite.hint),
        "the masked hint stays visible: {rendered}"
    );

    // The error chain is bounded the same way: a revoked-code redeem
    // failure carries fixed codes only, never code material.
    a.revoke(invite.code_id).await.expect("revoke");
    let b_key = DeviceKeyPair::generate();
    let b = {
        let dir = tmp.path().join("redeemer");
        std::fs::create_dir_all(&dir).expect("b dir");
        let client =
            RegistryClient::connect(registry.addr, registry.registry_id, &b_key, "redeemer-b")
                .await
                .expect("b connects");
        PairingOrchestrator::open(
            client,
            b_key,
            &dir,
            OrchestratorConfig {
                // Same slow-runner headroom as task_pairing (was 1.5s/5s).
                redeem_window_ms: 10_000,
                ready_wait_ms: 30_000,
                claim_conflict_retries: 8,
            },
        )
        .expect("b orchestrator")
    };
    let err = b
        .redeem(&invite.display)
        .await
        .expect_err("revoked code fails");
    match &err {
        PairFlowError::Unavailable { code } => assert_eq!(*code, PAIR_UNAVAILABLE),
        other => panic!("expected the unified unavailable, got {other:?}"),
    }
    for chain in [format!("{err}"), format!("{err:?}")] {
        assert!(
            !chain.contains("AKI1-") && !chain.contains(&hex_bytes(&code.auth_key())),
            "the error chain must not carry invite material: {chain}"
        );
    }

    // CLI observable chain: the owner-side store through real processes.
    let server_dir = tmp.path().join("server");
    let cli_dir = tmp.path().join("cli-owner");
    std::fs::create_dir_all(&server_dir).expect("server dir");
    std::fs::create_dir_all(&cli_dir).expect("cli dir");
    let reg_proc = RegistryProc::spawn(&server_dir, &tmp.path().join("cli-registry.db"));
    let created = run_cli(
        &[
            "invite",
            "create",
            "--long-lived",
            "--id-dir",
            &cli_dir.to_string_lossy(),
            "--server",
            &reg_proc.addr.to_string(),
            "--registry-fp",
            &reg_proc.fp_hex,
        ],
        Duration::from_secs(30),
    );
    assert!(created.success, "invite create failed:\n{}", created.stderr);
    let display = created
        .stdout
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("AKI1-")
                .map(|rest| format!("AKI1-{rest}"))
        })
        .expect("create prints the display code once (deliberate UX)");
    let parsed = InviteCode::parse(&display).expect("printed code parses");

    let list = run_cli(
        &["invite", "list", "--id-dir", &cli_dir.to_string_lossy()],
        Duration::from_secs(30),
    );
    assert!(list.success, "invite list failed:\n{}", list.stderr);
    // The list row shows the masked hint and never the bearer secret.
    assert!(
        list.stdout.contains(&parsed.code_hint()),
        "the masked hint is the visible secret view: {}",
        list.stdout
    );
    assert!(
        !list.stdout.contains(&display),
        "invite list must never print the plaintext code: {}",
        list.stdout
    );
    // stderr of both verbs stays free of any bearer token.
    for (what, stderr) in [("create", &created.stderr), ("list", &list.stderr)] {
        assert!(
            !stderr.contains("AKI1-"),
            "{what} stderr must never echo the bearer code: {stderr}"
        );
    }
}

// ---- 5: registry shutdown keeps established trust -----------------------------

/// Killing the registry is not a trust revocation: both committed
/// `peers.book` files stay byte-identical, a fresh redeem against the dead
/// registry fails at connect and leaves no state behind.
#[tokio::test]
async fn registry_shutdown_preserves_established_pairing_trust() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;

    let owner_key = DeviceKeyPair::generate();
    let redeemer_key = DeviceKeyPair::generate();
    let a_dir = tmp.path().join("owner");
    let b_dir = tmp.path().join("redeemer");
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;
    let b = connect_device(&registry, "redeemer-b", &redeemer_key, &b_dir).await;

    let invite = a.create_invite(0).await.expect("claim invite");
    a.spawn_owner_loop();
    b.redeem(&invite.display).await.expect("full pairing");
    let owner_hex = owner_key.fingerprint().to_hex();
    let redeemer_hex = redeemer_key.fingerprint().to_hex();
    assert!(book_contains(&a_dir, &redeemer_hex));
    assert!(book_contains(&b_dir, &owner_hex));

    // Snapshot both books, then kill the registry.
    let a_book = book_text(&a_dir).expect("owner book");
    let b_book = book_text(&b_dir).expect("redeemer book");
    let dead_addr = registry.addr;
    let dead_id = registry.registry_id;
    registry.shutdown.send(true).expect("send shutdown");
    drop(registry);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Established trust is untouched — byte-identical books.
    assert_eq!(book_text(&a_dir).as_deref(), Some(a_book.as_str()));
    assert_eq!(book_text(&b_dir).as_deref(), Some(b_book.as_str()));

    // A fresh redeem against the dead registry fails at connect and
    // records nothing locally.
    let c_key = DeviceKeyPair::generate();
    let c_dir = tmp.path().join("stranger");
    std::fs::create_dir_all(&c_dir).expect("c dir");
    let connect = RegistryClient::connect(dead_addr, dead_id, &c_key, "stranger-c").await;
    assert!(
        connect.is_err(),
        "connecting to the dead registry must fail"
    );
    assert!(
        !c_dir.join(BOOK_NAME).exists(),
        "no trust may appear from a failed redeem"
    );
    assert!(
        journal_records(&c_dir).is_empty(),
        "no journal entries for a failed redeem"
    );
}

// ---- 6: same request id replay is idempotent server-side ----------------------

/// A dropped reply retried with the SAME request id registers exactly one
/// code (server-side count through the store) and — on the redeem side —
/// replays the SAME pairing id, while a genuinely different attempt gets a
/// fresh one. One code, one pairing transaction: no duplicate state from
/// network retries.
#[tokio::test]
async fn same_request_id_replay_registers_one_code_and_one_pairing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("registry.db");
    let registry = spawn_registry(&db).await;
    let probe = RegistryStore::open(&db).expect("probe store");

    let owner_key = DeviceKeyPair::generate();
    let a_dir = tmp.path().join("owner");
    let a = connect_device(&registry, "owner-a", &owner_key, &a_dir).await;

    // The network-retry shape: the claim reply was lost, the same
    // request id is re-sent.
    let request_id = random_id16();
    let first = a
        .create_invite_with_request_id(request_id, 0)
        .await
        .expect("first claim");
    let replay = a
        .create_invite_with_request_id(request_id, 0)
        .await
        .expect("replayed claim");
    assert_eq!(first.code_id, replay.code_id, "same code id");
    assert_eq!(first.display, replay.display, "same code secret");
    assert_eq!(first.revision, replay.revision, "same revision");
    assert_eq!(first.lookup_digest, replay.lookup_digest);

    // Server-side: exactly ONE code row for this owner.
    let codes = probe
        .list(
            &owner_key.public_key(),
            &[0u8; 16],
            255,
            StateFilter::ALL,
            RegistryStore::now_ms(),
        )
        .expect("list codes");
    assert_eq!(codes.len(), 1, "the retry registered exactly one code");
    assert_eq!(codes[0].code_id, first.code_id);
    assert_eq!(codes[0].revision, first.revision);

    // Redeem idempotency: the same (redeemer, request_id) replays the
    // ORIGINAL pairing id; a different attempt gets a fresh one.
    let b_key = DeviceKeyPair::generate();
    let b_client =
        RegistryClient::connect(registry.addr, registry.registry_id, &b_key, "redeemer-b")
            .await
            .expect("b connects");
    let parsed = InviteCode::parse(&first.display).expect("code parses");
    let request = signed_redeem(
        &b_key,
        &registry.registry_id,
        &parsed,
        now_unix_ms() + 3_000,
    );
    let ack1 = b_client
        .invite_redeem(request.clone())
        .await
        .expect("first redeem pending");
    let ack2 = b_client
        .invite_redeem(request)
        .await
        .expect("replayed redeem pending");
    assert_eq!(
        ack1.pairing_id, ack2.pairing_id,
        "the replay reuses the original pairing id"
    );

    // Exactly one server-side transaction for the replayed attempt, still
    // pending (nothing was committed by a retry).
    let snapshot = probe
        .pairing_snapshot(&ack1.pairing_id)
        .expect("snapshot")
        .expect("the replayed pairing exists exactly once");
    assert_eq!(snapshot.state, PairingState::Pending);
    // A ghost pairing id (the shape a SECOND registration would have) is
    // absent — one attempt, one transaction.
    assert!(
        probe.pairing_snapshot(&random_id16()).unwrap().is_none(),
        "no stray second transaction"
    );

    // Sanity that the store is not simply accepting everything: a fresh
    // claim (fresh request id, fresh secret) registers a SECOND, distinct
    // code — the idempotence above is keyed by the request id alone.
    let fresh = a.create_invite(0).await.expect("fresh claim");
    assert_ne!(fresh.code_id, first.code_id);
    assert_ne!(fresh.display, first.display);
}

/// A fully signed redeem request for `code` (same construction as the
/// orchestrator; the server checks the signature, never the MAC).
fn signed_redeem(
    key: &DeviceKeyPair,
    registry_id: &[u8; 32],
    code: &InviteCode,
    deadline: u64,
) -> RedeemRequest {
    let request_id = random_id16();
    let nonce = random_nonce32();
    let request_mac = redeem_request_mac(
        &code.auth_key(),
        registry_id,
        &code.lookup_digest(),
        &key.public_key(),
        &request_id,
        &nonce,
        deadline,
    );
    let signature = key.sign(&hello2::redeem_message(
        registry_id,
        &code.lookup_digest(),
        &key.public_key(),
        &request_id,
        &nonce,
        deadline,
        &request_mac,
    ));
    RedeemRequest {
        request_id,
        lookup_digest: code.lookup_digest(),
        redeemer_nonce: nonce,
        deadline_ms: deadline,
        request_mac,
        signature,
    }
}

// ---- CLI plumbing (registry-server / invite verbs as real processes) ---------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root two levels up")
        .to_path_buf()
}

fn cargo_bin() -> PathBuf {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return PathBuf::from(cargo);
    }
    std::env::var_os("USERPROFILE")
        .map(|home| {
            PathBuf::from(home).join(".cargo").join(if cfg!(windows) {
                "bin\\cargo.exe"
            } else {
                "bin/cargo"
            })
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn cli_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let root = workspace_root();
        let target_dir = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let exe = target_dir.join("debug").join(if cfg!(windows) {
            "ak-cli.exe"
        } else {
            "ak-cli"
        });
        let output = Command::new(cargo_bin())
            .args(["build", "-p", "ak-cli"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|err| panic!("cannot run cargo build -p ak-cli: {err}"));
        assert!(
            output.status.success(),
            "cargo build -p ak-cli failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(exe.exists(), "cargo build produced no {}", exe.display());
        exe
    })
    .clone()
}

struct RunOutcome {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_cli(args: &[&str], timeout: Duration) -> RunOutcome {
    let cli = cli_exe();
    let mut child = Command::new(&cli)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn ({:?} {:?}): {err}", cli, args));
    let stdout_handle = {
        let pipe = child.stdout.take().expect("stdout piped");
        std::thread::spawn(move || read_all(pipe))
    };
    let stderr_handle = {
        let pipe = child.stderr.take().expect("stderr piped");
        std::thread::spawn(move || read_all(pipe))
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try wait") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    RunOutcome {
        success: status.is_some_and(|status| status.success()),
        stdout: stdout_handle.join().expect("stdout reader"),
        stderr: stderr_handle.join().expect("stderr reader"),
    }
}

fn read_all(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

/// A resident CLI child (killed on drop).
struct Proc {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
}

impl Proc {
    fn spawn(args: &[&str]) -> Proc {
        let cli = cli_exe();
        let mut child = Command::new(&cli)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|err| panic!("spawn ({:?} {:?}): {err}", cli, args));
        let stdout = pipe_lines(child.stdout.take().expect("stdout piped"));
        // Drain stderr too (into the void) so a chatty child can never
        // block on a full pipe.
        let _ = pipe_lines(child.stderr.take().expect("stderr piped"));
        Proc { child, stdout }
    }

    fn wait_stdout(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let lines = self
                .stdout
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            for line in lines {
                if line.contains(needle) {
                    return line;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no stdout line containing {needle:?} within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pipe_lines(stream: impl Read + Send + 'static) -> Arc<Mutex<Vec<String>>> {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&sink);
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(text) => writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(text),
                Err(_) => break,
            }
        }
    });
    sink
}

/// The registry-server subprocess; its pin is parsed from stdout.
struct RegistryProc {
    /// Owning the process keeps the server alive (and kills it on drop).
    #[allow(dead_code)]
    proc: Proc,
    addr: std::net::SocketAddr,
    fp_hex: String,
}

impl RegistryProc {
    fn spawn(server_dir: &Path, db: &Path) -> RegistryProc {
        let port = free_port();
        let proc = Proc::spawn(&[
            "registry-server",
            "--port",
            &port.to_string(),
            "--db-path",
            &db.to_string_lossy(),
            "--id-dir",
            &server_dir.to_string_lossy(),
        ]);
        let line = proc.wait_stdout(
            "registry id (pin with --registry-fp): ",
            Duration::from_secs(30),
        );
        let fp_hex = line
            .rsplit(' ')
            .next()
            .expect("hex id last token")
            .to_ascii_lowercase();
        assert_eq!(fp_hex.len(), 64, "registry id line: {line}");
        RegistryProc {
            proc,
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            fp_hex,
        }
    }
}
