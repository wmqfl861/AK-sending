//! server_v2 integration tests (A2): real rustls TLS on loopback, two
//! independent pinned clients per scenario, and the full pairing timeline
//! of `docs/contracts/pairing-flow.md` §6 driven end to end.
//!
//! Layer: L2 (real TLS + real SQLite over loopback, single process).
//!
//! Coverage required by the A2 task: claim/list/revoke round trips,
//! concurrent same-code claim linearization, idempotent claim retries,
//! redeem timelines (owner online → immediate challenge; owner offline →
//! pending, then catch-up push on Hello), unified `PAIR_UNAVAILABLE` for
//! unknown/revoked/expired codes, per-connection and global redeem
//! budgets, commit-vs-abort race, revoke-before-commit failure,
//! server-restart recovery, transcript key/code substitution rejection,
//! and digest-consistency rejection.

#![cfg(feature = "server-persistence")]

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ak_identity::invite::InviteCode;
use ak_identity::pairing_txn::{
    random_id16, random_nonce32, PairProofData, PairTranscript, PairingRecord,
};
use ak_identity::DeviceKeyPair;
use ak_signal::protocol_v2::{
    AbortReason, CodeId, CtrlFrame, CtrlMessage, InviteState, PairProof, PairingId, PairingState,
    PeerSignal, RedeemRequest, RequestId, Role, StateFilter, CODE_CONFLICT, CURSOR_END,
    PAIR_UNAVAILABLE, PROTOCOL_UNSUPPORTED, RATE_LIMITED, REQUEST_CONFLICT, REVISION_CONFLICT,
};
use ak_signal::server_v2::engine;
use ak_signal::server_v2::framing::{read_frame, write_message};
use ak_signal::server_v2::store::RegistryStore;
use ak_signal::server_v2::{hello2, tls, CAPABILITIES};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::timeout;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---- harness -----------------------------------------------------------------

struct TestServer {
    addr: std::net::SocketAddr,
    registry: DeviceKeyPair,
    db_path: PathBuf,
    shutdown: watch::Sender<bool>,
}

impl TestServer {
    fn registry_id(&self) -> [u8; 32] {
        self.registry.public_key()
    }

    /// Fresh registry key over an in-memory store.
    async fn spawn_memory() -> TestServer {
        Self::spawn_at(PathBuf::from(":memory:"), DeviceKeyPair::generate()).await
    }

    /// A specific registry key and database file (restart tests reuse both).
    async fn spawn_at(db_path: PathBuf, registry: DeviceKeyPair) -> TestServer {
        let store = if db_path.as_os_str() == ":memory:" {
            RegistryStore::open_in_memory().unwrap()
        } else {
            RegistryStore::open(&db_path).unwrap()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown, rx) = watch::channel(false);
        let task_key = registry.clone();
        tokio::spawn(async move {
            let _ = engine::serve_v2(listener, &task_key, store, Default::default(), rx).await;
        });
        TestServer {
            addr,
            registry,
            db_path,
            shutdown,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown.send(true);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One pinned, Hello-authenticated v2 control client.
struct Client {
    stream: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    key: DeviceKeyPair,
}

impl Client {
    fn device_id(&self) -> [u8; 32] {
        self.key.public_key()
    }

    async fn connect(server: &TestServer, key: DeviceKeyPair, name: &str) -> Client {
        let mut stream = tls::connect_pinned(server.addr, &server.registry_id())
            .await
            .expect("tls handshake with pinned registry");
        let bytes = read_frame(&mut stream)
            .await
            .unwrap()
            .expect("auth challenge");
        let CtrlMessage::AuthChallenge {
            registry_id,
            nonce,
            expires_ms,
        } = CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
        else {
            panic!("first server frame must be AuthChallenge");
        };
        assert_eq!(registry_id, server.registry_id());
        assert!(expires_ms > now_ms());
        let exporter = hello2::exporter_secret_client(stream.get_ref().1).unwrap();
        let timestamp = now_ms();
        let signed = hello2::hello2_message(
            &registry_id,
            &exporter,
            2,
            &key.public_key(),
            name,
            timestamp,
            &nonce,
        );
        write_message(
            &mut stream,
            &CtrlMessage::Hello {
                version: 2,
                device_id: key.public_key(),
                name: name.to_string(),
                timestamp_ms: timestamp,
                challenge: nonce,
                signature: key.sign(&signed),
            },
            Role::Client,
        )
        .await
        .unwrap();
        let mut client = Client { stream, key };
        match client.recv().await {
            CtrlMessage::Welcome {
                session_ttl_secs,
                capabilities,
            } => {
                assert_eq!(capabilities, CAPABILITIES);
                assert!(session_ttl_secs > 0);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
        client
    }

    async fn send(&mut self, message: &CtrlMessage) {
        write_message(&mut self.stream, message, Role::Client)
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> CtrlMessage {
        let bytes = timeout(RECV_TIMEOUT, read_frame(&mut self.stream))
            .await
            .expect("frame within timeout")
            .expect("io error")
            .expect("unexpected eof");
        CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
    }

    /// Receive the next `PairError`, asserting it is one.
    async fn recv_pair_error(&mut self) -> u16 {
        match self.recv().await {
            CtrlMessage::PairError { code, .. } => code,
            other => panic!("expected PairError, got {other:?}"),
        }
    }

    async fn closed(&mut self) -> bool {
        matches!(
            timeout(RECV_TIMEOUT, read_frame(&mut self.stream)).await,
            Ok(Ok(None)) | Ok(Err(_))
        )
    }
}

// ---- pairing flow helpers ------------------------------------------------------

fn redeem_request(
    key: &DeviceKeyPair,
    registry: &[u8; 32],
    invite: &InviteCode,
    deadline: u64,
) -> RedeemRequest {
    let request_id = random_id16();
    let redeemer_nonce = random_nonce32();
    let request_mac = invite.auth_key(); // endpoint-computed MAC; server never checks it
    let signature = key.sign(&hello2::redeem_message(
        registry,
        &invite.lookup_digest(),
        &key.public_key(),
        &request_id,
        &redeemer_nonce,
        deadline,
        &request_mac,
    ));
    RedeemRequest {
        request_id,
        lookup_digest: invite.lookup_digest(),
        redeemer_nonce,
        deadline_ms: deadline,
        request_mac,
        signature,
    }
}

async fn claim(client: &mut Client, invite: &InviteCode, ttl_secs: u32) -> (CodeId, u64) {
    let request_id = random_id16();
    client
        .send(&CtrlMessage::InviteClaim {
            request_id,
            lookup_digest: invite.lookup_digest(),
            ttl_secs,
        })
        .await;
    match client.recv().await {
        CtrlMessage::InviteClaimed {
            request_id: rid,
            code_id,
            revision,
            ..
        } => {
            assert_eq!(rid, request_id);
            (code_id, revision)
        }
        other => panic!("expected InviteClaimed, got {other:?}"),
    }
}

/// Send `InviteRedeem`; return `(request_id, pairing_id, redeem)` after
/// the uniform `InviteRedeemPending` reply.
async fn redeem(
    client: &mut Client,
    registry: &[u8; 32],
    invite: &InviteCode,
) -> (RequestId, PairingId, RedeemRequest) {
    let deadline = now_ms() + 8_000;
    let request = redeem_request(&client.key, registry, invite, deadline);
    let request_id = request.request_id;
    client
        .send(&CtrlMessage::InviteRedeem(request.clone()))
        .await;
    match client.recv().await {
        CtrlMessage::InviteRedeemPending {
            request_id: rid,
            pairing_id,
            deadline_ms,
        } => {
            assert_eq!(rid, request_id);
            assert_eq!(deadline_ms, deadline);
            (request_id, pairing_id, request)
        }
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    }
}

fn proof_wire(proof: &PairProofData) -> PairProof {
    PairProof {
        mac: proof.mac,
        signature: proof.signature,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_material(
    registry: &[u8; 32],
    owner: &DeviceKeyPair,
    redeemer_key: &DeviceKeyPair,
    invite: &InviteCode,
    code_id: CodeId,
    code_revision: u64,
    pairing_id: PairingId,
    redeem: &RedeemRequest,
) -> (PairTranscript, PairingRecord) {
    let transcript = PairTranscript {
        registry_id: *registry,
        code_id,
        code_revision,
        pairing_id,
        request_id: redeem.request_id,
        lookup_digest: invite.lookup_digest(),
        owner_device_id: owner.public_key(),
        redeemer_device_id: redeemer_key.public_key(),
        owner_nonce: random_nonce32(),
        redeemer_nonce: redeem.redeemer_nonce,
        deadline_ms: redeem.deadline_ms,
    };
    let record = PairingRecord::build(transcript, &invite.auth_key(), owner, redeemer_key);
    (transcript, record)
}

/// Redeem → offer → PairChallenge → PairProof, stopping before Prepared.
async fn flow_to_proofs(
    server: &TestServer,
    owner: &mut Client,
    redeemer: &mut Client,
    invite: &InviteCode,
    code_id: CodeId,
    code_revision: u64,
) -> (PairingId, PairTranscript, PairingRecord) {
    let (request_id, pairing_id, request) = redeem(redeemer, &server.registry_id(), invite).await;
    // The owner receives the offer for the online case. (Callers using the
    // offline timeline consume the offer themselves and use
    // `flow_from_offer` instead.)
    consume_offer(owner, pairing_id, request_id, code_id).await;
    flow_from_offer(
        server,
        owner,
        redeemer,
        invite,
        code_id,
        code_revision,
        pairing_id,
        request,
    )
    .await
}

/// Consume the `InviteRedeemOffered` the owner just received.
async fn consume_offer(
    owner: &mut Client,
    pairing_id: PairingId,
    request_id: RequestId,
    code_id: CodeId,
) {
    match owner.recv().await {
        CtrlMessage::InviteRedeemOffered {
            code_id: offered,
            pairing_id: caught,
            redeem,
            ..
        } => {
            assert_eq!(offered, code_id);
            assert_eq!(caught, pairing_id);
            assert_eq!(redeem.request_id, request_id);
        }
        other => panic!("expected InviteRedeemOffered, got {other:?}"),
    }
}

/// Owner answers an already-delivered offer through PairProof.
#[allow(clippy::too_many_arguments)]
async fn flow_from_offer(
    server: &TestServer,
    owner: &mut Client,
    redeemer: &mut Client,
    invite: &InviteCode,
    code_id: CodeId,
    code_revision: u64,
    pairing_id: PairingId,
    redeem: RedeemRequest,
) -> (PairingId, PairTranscript, PairingRecord) {
    let (transcript, record) = build_material(
        &server.registry_id(),
        &owner.key,
        &redeemer.key,
        invite,
        code_id,
        code_revision,
        pairing_id,
        &redeem,
    );
    let digest = transcript.transcript_digest();
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: transcript.to_bytes(),
            owner_proof: proof_wire(&record.owner_proof),
        })
        .await;
    match redeemer.recv().await {
        CtrlMessage::PairChallenge {
            transcript: relayed,
            owner_proof,
        } => {
            assert_eq!(relayed, transcript.to_bytes());
            assert_eq!(owner_proof, proof_wire(&record.owner_proof));
        }
        other => panic!("expected PairChallenge, got {other:?}"),
    }
    redeemer
        .send(&CtrlMessage::PairProof {
            pairing_id,
            transcript_digest: digest,
            redeemer_proof: proof_wire(&record.redeemer_proof),
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairProof {
            transcript_digest, ..
        } => assert_eq!(transcript_digest, digest),
        other => panic!("expected PairProof, got {other:?}"),
    }
    (pairing_id, transcript, record)
}

fn prepared_message(client: &Client, pairing_id: &PairingId, digest: &[u8; 32]) -> CtrlMessage {
    CtrlMessage::PairPrepared {
        pairing_id: *pairing_id,
        transcript_digest: *digest,
        signature: client
            .key
            .sign(&hello2::prepared_message(pairing_id, digest)),
    }
}

fn applied_message(
    client: &Client,
    pairing_id: &PairingId,
    digest: &[u8; 32],
    commit_revision: u64,
) -> CtrlMessage {
    CtrlMessage::PairApplied {
        pairing_id: *pairing_id,
        transcript_digest: *digest,
        commit_revision,
        signature: client.key.sign(&hello2::applied_message(
            pairing_id,
            digest,
            commit_revision,
        )),
    }
}

/// Both sides Prepared → both receive `PairCommit`; returns its revision.
async fn drive_commit(
    owner: &mut Client,
    redeemer: &mut Client,
    pairing_id: &PairingId,
    digest: &[u8; 32],
) -> u64 {
    owner
        .send(&prepared_message(owner, pairing_id, digest))
        .await;
    redeemer
        .send(&prepared_message(redeemer, pairing_id, digest))
        .await;
    let mut revision = 0;
    for client in [&mut *owner, &mut *redeemer] {
        match client.recv().await {
            CtrlMessage::PairCommit {
                transcript,
                commit_revision,
                ..
            } => {
                assert_eq!(
                    transcript.to_vec(),
                    PairTranscript::from_bytes(&transcript).to_bytes().to_vec()
                );
                revision = commit_revision;
            }
            other => panic!("expected PairCommit, got {other:?}"),
        }
    }
    revision
}

/// Both sides Applied → both receive `PairReady`.
async fn drive_ready(
    owner: &mut Client,
    redeemer: &mut Client,
    pairing_id: &PairingId,
    digest: &[u8; 32],
    commit_revision: u64,
) {
    owner
        .send(&applied_message(owner, pairing_id, digest, commit_revision))
        .await;
    redeemer
        .send(&applied_message(
            redeemer,
            pairing_id,
            digest,
            commit_revision,
        ))
        .await;
    for client in [&mut *owner, &mut *redeemer] {
        match client.recv().await {
            CtrlMessage::PairReady {
                pairing_id: id,
                commit_revision: r,
                ..
            } => {
                assert_eq!(id, *pairing_id);
                assert_eq!(r, commit_revision);
            }
            other => panic!("expected PairReady, got {other:?}"),
        }
    }
}

// ---- TLS + Hello ----------------------------------------------------------------

#[tokio::test]
async fn wrong_registry_pin_fails_the_handshake() {
    let server = TestServer::spawn_memory().await;
    let wrong = DeviceKeyPair::generate().public_key();
    assert!(
        tls::connect_pinned(server.addr, &wrong).await.is_err(),
        "a wrong registry pin must never complete"
    );
    assert!(
        tls::connect_pinned(server.addr, &server.registry_id())
            .await
            .is_ok(),
        "the real pin connects"
    );
    server.stop().await;
}

#[tokio::test]
async fn hello_replay_is_rejected_across_and_within_connections() {
    let server = TestServer::spawn_memory().await;
    let key = DeviceKeyPair::generate();

    // Capture the exact Hello frame bytes of a first connection.
    let mut first = tls::connect_pinned(server.addr, &server.registry_id())
        .await
        .unwrap();
    let bytes = read_frame(&mut first).await.unwrap().unwrap();
    let CtrlMessage::AuthChallenge { nonce, .. } =
        CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
    else {
        panic!("expected AuthChallenge");
    };
    let exporter = hello2::exporter_secret_client(first.get_ref().1).unwrap();
    // One timestamp sample for both the signed message and the frame:
    // two draws would differ on a millisecond tick and fail verification.
    let timestamp = now_ms();
    let signed = hello2::hello2_message(
        &server.registry_id(),
        &exporter,
        2,
        &key.public_key(),
        "a",
        timestamp,
        &nonce,
    );
    let mut frame = Vec::new();
    CtrlFrame::new(CtrlMessage::Hello {
        version: 2,
        device_id: key.public_key(),
        name: "a".to_string(),
        timestamp_ms: timestamp,
        challenge: nonce,
        signature: key.sign(&signed),
    })
    .try_encode_as(Role::Client, &mut frame)
    .unwrap();
    first.write_all(&frame).await.unwrap();
    let reply = read_frame(&mut first).await.unwrap().unwrap();
    assert!(matches!(
        CtrlFrame::decode_as(&reply, Role::Client).unwrap().message,
        CtrlMessage::Welcome { .. }
    ));

    // Replay those exact bytes on a fresh TLS connection: the new
    // AuthChallenge nonce differs, so the echo check rejects it.
    let mut second = tls::connect_pinned(server.addr, &server.registry_id())
        .await
        .unwrap();
    let _ = read_frame(&mut second).await.unwrap().unwrap();
    second.write_all(&frame).await.unwrap();
    assert!(second_closed(&mut second).await, "replayed Hello rejected");
    drop(first);

    // A duplicate Hello on an authenticated connection also disconnects.
    let mut client = Client::connect(&server, key, "a").await;
    client
        .send(&CtrlMessage::Hello {
            version: 2,
            device_id: client.device_id(),
            name: "a".to_string(),
            timestamp_ms: now_ms(),
            challenge: [0; 32],
            signature: [0; 64],
        })
        .await;
    assert!(client.closed().await, "second Hello rejected");
    server.stop().await;
}

async fn second_closed(
    stream: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
) -> bool {
    matches!(
        timeout(RECV_TIMEOUT, read_frame(stream)).await,
        Ok(Ok(None)) | Ok(Err(_))
    )
}

#[tokio::test]
async fn hello_with_stale_timestamp_is_rejected() {
    let server = TestServer::spawn_memory().await;
    let key = DeviceKeyPair::generate();
    let mut stream = tls::connect_pinned(server.addr, &server.registry_id())
        .await
        .unwrap();
    let bytes = read_frame(&mut stream).await.unwrap().unwrap();
    let CtrlMessage::AuthChallenge { nonce, .. } =
        CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
    else {
        panic!("expected AuthChallenge");
    };
    let exporter = hello2::exporter_secret_client(stream.get_ref().1).unwrap();
    let stale = now_ms() - 60_000;
    let signed = hello2::hello2_message(
        &server.registry_id(),
        &exporter,
        2,
        &key.public_key(),
        "a",
        stale,
        &nonce,
    );
    write_message(
        &mut stream,
        &CtrlMessage::Hello {
            version: 2,
            device_id: key.public_key(),
            name: "a".to_string(),
            timestamp_ms: stale,
            challenge: nonce,
            signature: key.sign(&signed),
        },
        Role::Client,
    )
    .await
    .unwrap();
    let mut client = Client { stream, key };
    assert!(client.closed().await, "stale Hello rejected");
    server.stop().await;
}

#[tokio::test]
async fn hello_signed_over_a_foreign_exporter_is_rejected() {
    let server = TestServer::spawn_memory().await;
    let key = DeviceKeyPair::generate();
    let mut stream = tls::connect_pinned(server.addr, &server.registry_id())
        .await
        .unwrap();
    let bytes = read_frame(&mut stream).await.unwrap().unwrap();
    let CtrlMessage::AuthChallenge { nonce, .. } =
        CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
    else {
        panic!("expected AuthChallenge");
    };
    // Signed over a fabricated exporter (as if captured on another
    // connection): the server's real exporter differs.
    let fake_exporter = [0x5A; 32];
    let signed = hello2::hello2_message(
        &server.registry_id(),
        &fake_exporter,
        2,
        &key.public_key(),
        "a",
        now_ms(),
        &nonce,
    );
    write_message(
        &mut stream,
        &CtrlMessage::Hello {
            version: 2,
            device_id: key.public_key(),
            name: "a".to_string(),
            timestamp_ms: now_ms(),
            challenge: nonce,
            signature: key.sign(&signed),
        },
        Role::Client,
    )
    .await
    .unwrap();
    let mut client = Client { stream, key };
    assert!(
        client.closed().await,
        "Hello bound to a foreign exporter rejected"
    );
    server.stop().await;
}

#[tokio::test]
async fn ping_pong_roundtrip() {
    let server = TestServer::spawn_memory().await;
    let mut client = Client::connect(&server, DeviceKeyPair::generate(), "p").await;
    let nonce = [7u8; 16];
    client.send(&CtrlMessage::Ping { nonce }).await;
    match client.recv().await {
        CtrlMessage::Pong { nonce: echoed } => assert_eq!(echoed, nonce),
        other => panic!("expected Pong, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn v1_frames_are_refused_on_the_v2_port() {
    let server = TestServer::spawn_memory().await;
    let mut stream = tls::connect_pinned(server.addr, &server.registry_id())
        .await
        .unwrap();
    let _ = read_frame(&mut stream).await.unwrap().unwrap();
    // A v1-coded header (version byte 1) inside the TLS stream.
    stream.write_all(&[1u8, 0x01, 0x00, 0x00]).await.unwrap();
    assert!(second_closed(&mut stream).await, "no downgrade to v1");
    server.stop().await;
}

// ---- claim / list / revoke -------------------------------------------------------

#[tokio::test]
async fn claim_list_roundtrip_with_paging_and_filters() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut code_ids = Vec::new();
    for _ in 0..3 {
        let invite = InviteCode::generate();
        let (code_id, revision) = claim(&mut owner, &invite, 0).await;
        assert_eq!(revision, 1);
        code_ids.push(code_id);
    }
    code_ids.sort();

    let request_id = random_id16();
    owner
        .send(&CtrlMessage::InviteList {
            request_id,
            cursor: [0; 16],
            limit: 2,
            state_filter: StateFilter::ALL,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::InviteListPage {
            request_id: rid,
            next_cursor,
            entries,
        } => {
            assert_eq!(rid, request_id);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].code_id, code_ids[0]);
            assert_eq!(entries[0].state, InviteState::Active);
            assert_ne!(next_cursor, CURSOR_END);
            owner
                .send(&CtrlMessage::InviteList {
                    request_id,
                    cursor: next_cursor,
                    limit: 2,
                    state_filter: StateFilter::ALL,
                })
                .await;
            match owner.recv().await {
                CtrlMessage::InviteListPage {
                    next_cursor,
                    entries,
                    ..
                } => {
                    assert_eq!(entries.len(), 1);
                    assert_eq!(entries[0].code_id, code_ids[2]);
                    assert_eq!(next_cursor, CURSOR_END);
                }
                other => panic!("expected second page, got {other:?}"),
            }
        }
        other => panic!("expected first page, got {other:?}"),
    }

    // Revoke the middle code; filters split by state.
    let revoke_request = random_id16();
    owner
        .send(&CtrlMessage::InviteRevoke {
            request_id: revoke_request,
            code_id: code_ids[1],
            expected_revision: 1,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::InviteRevoked {
            request_id,
            revision,
            ..
        } => {
            assert_eq!(request_id, revoke_request);
            assert_eq!(revision, 2);
        }
        other => panic!("expected InviteRevoked, got {other:?}"),
    }
    owner
        .send(&CtrlMessage::InviteList {
            request_id,
            cursor: [0; 16],
            limit: 32,
            state_filter: StateFilter::ACTIVE,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::InviteListPage { entries, .. } => {
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().all(|e| e.state == InviteState::Active));
        }
        other => panic!("expected active page, got {other:?}"),
    }
    owner
        .send(&CtrlMessage::InviteList {
            request_id,
            cursor: [0; 16],
            limit: 32,
            state_filter: StateFilter::REVOKED,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::InviteListPage { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].code_id, code_ids[1]);
        }
        other => panic!("expected revoked page, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn claim_retry_is_idempotent_and_conflicting_retry_is_rejected() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let invite = InviteCode::generate();
    let request_id = random_id16();

    owner
        .send(&CtrlMessage::InviteClaim {
            request_id,
            lookup_digest: invite.lookup_digest(),
            ttl_secs: 0,
        })
        .await;
    let first = match owner.recv().await {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };

    // Network-timeout retry with the same request id and content: the
    // exact original result (same code id), not a new registration.
    owner
        .send(&CtrlMessage::InviteClaim {
            request_id,
            lookup_digest: invite.lookup_digest(),
            ttl_secs: 0,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::InviteClaimed { code_id, .. } => assert_eq!(code_id, first),
        other => panic!("expected replayed InviteClaimed, got {other:?}"),
    }

    // Same request id, different content: REQUEST_CONFLICT.
    owner
        .send(&CtrlMessage::InviteClaim {
            request_id,
            lookup_digest: InviteCode::generate().lookup_digest(),
            ttl_secs: 0,
        })
        .await;
    assert_eq!(owner.recv_pair_error().await, REQUEST_CONFLICT);
    server.stop().await;
}

#[tokio::test]
async fn concurrent_claims_of_one_code_have_exactly_one_winner() {
    let server = TestServer::spawn_memory().await;
    let invite = InviteCode::generate();
    let mut clients = Vec::new();
    for _ in 0..8 {
        clients.push(Client::connect(&server, DeviceKeyPair::generate(), "c").await);
    }
    // Fire all claims, then collect all replies: the unique index under
    // one transaction linearizes them.
    for client in &mut clients {
        client
            .send(&CtrlMessage::InviteClaim {
                request_id: random_id16(),
                lookup_digest: invite.lookup_digest(),
                ttl_secs: 0,
            })
            .await;
    }
    let mut claimed = 0;
    let mut conflicts = 0;
    for client in &mut clients {
        match client.recv().await {
            CtrlMessage::InviteClaimed { .. } => claimed += 1,
            CtrlMessage::PairError { code, .. } => {
                assert_eq!(code, CODE_CONFLICT);
                conflicts += 1;
            }
            other => panic!("unexpected claim reply {other:?}"),
        }
    }
    assert_eq!(claimed, 1, "exactly one registration may win");
    assert_eq!(conflicts, 7);
    server.stop().await;
}

#[tokio::test]
async fn revoke_conflicts_and_unified_unavailability() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let invite = InviteCode::generate();
    let (code_id, _) = claim(&mut owner, &invite, 0).await;

    // Wrong revision: optimistic-lock rejection.
    owner
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id,
            expected_revision: 99,
        })
        .await;
    assert_eq!(owner.recv_pair_error().await, REVISION_CONFLICT);

    // A different device sees the same "unavailable" answer for a code it
    // does not own — and for one that does not exist at all.
    let mut stranger = Client::connect(&server, DeviceKeyPair::generate(), "s").await;
    stranger
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id,
            expected_revision: 1,
        })
        .await;
    let foreign = stranger.recv_pair_error().await;
    stranger
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id: [0xEE; 16],
            expected_revision: 1,
        })
        .await;
    let unknown = stranger.recv_pair_error().await;
    assert_eq!(foreign, PAIR_UNAVAILABLE);
    assert_eq!(unknown, PAIR_UNAVAILABLE, "no existence oracle");
    server.stop().await;
}

#[tokio::test]
async fn out_of_range_ttl_is_refused() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    for bad_ttl in [30u32, 31 * 24 * 3600] {
        owner
            .send(&CtrlMessage::InviteClaim {
                request_id: random_id16(),
                lookup_digest: InviteCode::generate().lookup_digest(),
                ttl_secs: bad_ttl,
            })
            .await;
        assert_eq!(owner.recv_pair_error().await, PROTOCOL_UNSUPPORTED);
    }
    // Minimum (60 s) and long-lived (0) both register.
    claim(&mut owner, &InviteCode::generate(), 60).await;
    claim(&mut owner, &InviteCode::generate(), 0).await;
    server.stop().await;
}

// ---- redeem timelines --------------------------------------------------------------

#[tokio::test]
async fn redeem_with_owner_online_runs_the_full_timeline_to_ready() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;
    let digest = transcript.transcript_digest();

    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;

    // The relation is durable: a status query now says Ready.
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Ready),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn redeem_while_owner_offline_waits_and_catches_up_on_hello() {
    let server = TestServer::spawn_memory().await;
    let owner_key = DeviceKeyPair::generate();
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;

    // Owner claims, then goes offline before the redeem.
    let mut owner = Client::connect(&server, owner_key.clone(), "o").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    drop(owner);

    // Redeamer redeems: Pending, the same shape as any other redeem.
    let (request_id, pairing_id, redeem_request) =
        redeem(&mut redeemer, &server.registry_id(), &invite).await;

    // Owner reconnects: Welcome, then the deferred InviteRedeemOffered.
    let mut owner = Client::connect(&server, owner_key, "o").await;
    consume_offer(&mut owner, pairing_id, request_id, code_id).await;

    // The timeline completes from the recovered offer.
    let (pairing_id, transcript, _record) = flow_from_offer(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
        pairing_id,
        redeem_request,
    )
    .await;
    let digest = transcript.transcript_digest();
    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;
    server.stop().await;
}

#[tokio::test]
async fn unknown_revoked_and_expired_codes_all_yield_the_same_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let server =
        TestServer::spawn_at(dir.path().join("reg.sqlite"), DeviceKeyPair::generate()).await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;

    // One revoked code.
    let revoked = InviteCode::generate();
    let (revoked_id, _) = claim(&mut owner, &revoked, 0).await;
    owner
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id: revoked_id,
            expected_revision: 1,
        })
        .await;
    assert!(matches!(
        owner.recv().await,
        CtrlMessage::InviteRevoked { .. }
    ));

    // One expired code: minimum TTL claimed, then aged out through a
    // second store connection (the in-operation lazy checks read the same
    // rows). Unknown codes need no setup.
    let expired = InviteCode::generate();
    claim(&mut owner, &expired, 60).await;
    let external = RegistryStore::open(&server.db_path).unwrap();
    external.expire_codes_lazy(now_ms() + 61_000).unwrap();
    drop(external);

    // All three redemptions answer identically: Pending now, the unified
    // PAIR_UNAVAILABLE at the (short) deadline.
    let mut seen = Vec::new();
    for invite in [InviteCode::generate(), revoked, expired] {
        let deadline = now_ms() + 1_500;
        let request = redeem_request(&redeemer.key, &server.registry_id(), &invite, deadline);
        redeemer.send(&CtrlMessage::InviteRedeem(request)).await;
        match redeemer.recv().await {
            CtrlMessage::InviteRedeemPending { .. } => {}
            other => panic!("expected uniform Pending, got {other:?}"),
        }
        match redeemer.recv().await {
            CtrlMessage::PairError { code, .. } => seen.push(code),
            other => panic!("expected deferred PairError, got {other:?}"),
        }
    }
    assert!(
        seen.iter().all(|code| *code == PAIR_UNAVAILABLE),
        "unified unavailable, saw {seen:?}"
    );
    assert_eq!(seen.len(), 3);
    server.stop().await;
}

#[tokio::test]
async fn per_connection_redeem_budget_is_enforced_immediately() {
    let server = TestServer::spawn_memory().await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    // 10 per connection per minute (frozen constant): attempts 1..=10
    // answer Pending, the 11th is an immediate RATE_LIMITED.
    for attempt in 0..11 {
        let deadline = now_ms() + 5_000;
        let invite = InviteCode::generate();
        let request = redeem_request(&redeemer.key, &server.registry_id(), &invite, deadline);
        redeemer.send(&CtrlMessage::InviteRedeem(request)).await;
        let reply = redeemer.recv().await;
        if attempt < 10 {
            assert!(
                matches!(reply, CtrlMessage::InviteRedeemPending { .. }),
                "attempt {attempt} should be pending: {reply:?}"
            );
        } else {
            match reply {
                CtrlMessage::PairError {
                    code,
                    retry_after_ms,
                    ..
                } => {
                    assert_eq!(code, RATE_LIMITED);
                    assert!(retry_after_ms > 0);
                }
                other => panic!("11th redeem must be rate limited, got {other:?}"),
            }
        }
    }
    server.stop().await;
}

#[tokio::test]
async fn global_redeem_budget_is_enforced_across_connections() {
    let server = TestServer::spawn_memory().await;
    // 7 connections x 9 attempts = 63 attempts against a global budget of
    // 60 per minute: exactly the last 3 are RATE_LIMITED.
    let mut clients = Vec::new();
    for _ in 0..7 {
        clients.push(Client::connect(&server, DeviceKeyPair::generate(), "g").await);
    }
    let mut pending = 0usize;
    let mut limited = 0usize;
    for round in 0..9 {
        for client in &mut clients {
            let deadline = now_ms() + 5_000;
            let invite = InviteCode::generate();
            let request = redeem_request(&client.key, &server.registry_id(), &invite, deadline);
            client.send(&CtrlMessage::InviteRedeem(request)).await;
        }
        for client in &mut clients {
            match client.recv().await {
                CtrlMessage::InviteRedeemPending { .. } => pending += 1,
                CtrlMessage::PairError { code, .. } => {
                    assert_eq!(code, RATE_LIMITED);
                    limited += 1;
                }
                other => panic!("round {round}: unexpected {other:?}"),
            }
        }
    }
    assert_eq!(pending, 60, "global budget admits exactly 60 per minute");
    assert_eq!(limited, 3);
    server.stop().await;
}

// ---- commit / abort / revoke races -------------------------------------------------

#[tokio::test]
async fn commit_wins_against_a_late_abort() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;
    let digest = transcript.transcript_digest();

    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;

    // One side tries to abort a committed transaction: refused.
    redeemer
        .send(&CtrlMessage::PairAbort {
            request_id: random_id16(),
            pairing_id,
            reason: AbortReason::User,
        })
        .await;
    assert_eq!(
        redeemer.recv_pair_error().await,
        REVISION_CONFLICT,
        "committed transactions survive aborts"
    );

    // The pairing still completes to Ready.
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;
    server.stop().await;
}

#[tokio::test]
async fn revoke_before_commit_fails_the_pairing() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;
    let digest = transcript.transcript_digest();

    // Revoke lands before both Prepared arrived.
    owner
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id,
            expected_revision: 1,
        })
        .await;
    assert!(matches!(
        owner.recv().await,
        CtrlMessage::InviteRevoked { .. }
    ));

    // Both sides still try to prepare: the commit recheck fails and both
    // participants learn the pairing is unavailable — once from the
    // revoke's abort notification, once from their own Prepared attempt.
    owner
        .send(&prepared_message(&owner, &pairing_id, &digest))
        .await;
    redeemer
        .send(&prepared_message(&redeemer, &pairing_id, &digest))
        .await;
    for _ in 0..2 {
        assert_eq!(owner.recv_pair_error().await, PAIR_UNAVAILABLE);
        assert_eq!(redeemer.recv_pair_error().await, PAIR_UNAVAILABLE);
    }

    // The transaction is aborted.
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => {
            assert_eq!(state, PairingState::Aborted)
        }
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn transcript_key_substitution_is_rejected() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, _transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;

    // The owner now answers a second redeem of the same code with a
    // transcript naming a substituted (attacker) redeemer key. All proofs
    // are rebuilt consistently — the server must still reject because the
    // transcript's redeemer is not the authenticated redeemer.
    let attacker = DeviceKeyPair::generate();
    let (request_id2, pairing_id2, redeem2) =
        redeem(&mut redeemer, &server.registry_id(), &invite).await;
    consume_offer(&mut owner, pairing_id2, request_id2, code_id).await;
    let (bad_transcript, bad_record) = build_material(
        &server.registry_id(),
        &owner.key,
        &attacker,
        &invite,
        code_id,
        code_revision,
        pairing_id2,
        &redeem2,
    );
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: bad_transcript.to_bytes(),
            owner_proof: proof_wire(&bad_record.owner_proof),
        })
        .await;

    // The substituted transaction never advances past Pending…
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id: pairing_id2,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Pending),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    // …and the honest first pairing is untouched.
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Pending),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn transcript_code_substitution_is_rejected() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite_a = InviteCode::generate();
    let invite_b = InviteCode::generate();
    let (code_a, revision_a) = claim(&mut owner, &invite_a, 0).await;
    let (code_b, revision_b) = claim(&mut owner, &invite_b, 0).await;
    let (_request_id, pairing_id, redeem_a) =
        redeem(&mut redeemer, &server.registry_id(), &invite_a).await;
    consume_offer(&mut owner, pairing_id, _request_id, code_a).await;

    // The transcript names code B although the redeem ran under code A.
    let (bad_transcript, bad_record) = build_material(
        &server.registry_id(),
        &owner.key,
        &redeemer.key,
        &invite_b,
        code_b,
        revision_b,
        pairing_id,
        &redeem_a,
    );
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: bad_transcript.to_bytes(),
            owner_proof: proof_wire(&bad_record.owner_proof),
        })
        .await;
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Pending),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    let _ = (code_a, revision_a);
    server.stop().await;
}

#[tokio::test]
async fn pair_proof_with_a_foreign_transcript_digest_is_dropped() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;

    let (request_id, pairing_id, request) =
        redeem(&mut redeemer, &server.registry_id(), &invite).await;
    consume_offer(&mut owner, pairing_id, request_id, code_id).await;
    let (transcript, record) = build_material(
        &server.registry_id(),
        &owner.key,
        &redeemer.key,
        &invite,
        code_id,
        code_revision,
        pairing_id,
        &request,
    );
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: transcript.to_bytes(),
            owner_proof: proof_wire(&record.owner_proof),
        })
        .await;
    assert!(matches!(
        redeemer.recv().await,
        CtrlMessage::PairChallenge { .. }
    ));

    // Digest of a *different* transcript: dropped by digest consistency.
    let mut tampered = transcript;
    tampered.deadline_ms += 1;
    let wrong_digest = tampered.transcript_digest();
    redeemer
        .send(&CtrlMessage::PairProof {
            pairing_id,
            transcript_digest: wrong_digest,
            redeemer_proof: proof_wire(&record.redeemer_proof),
        })
        .await;
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Pending),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }

    // The correct digest is then accepted and the flow completes.
    let digest = transcript.transcript_digest();
    redeemer
        .send(&CtrlMessage::PairProof {
            pairing_id,
            transcript_digest: digest,
            redeemer_proof: proof_wire(&record.redeemer_proof),
        })
        .await;
    assert!(matches!(owner.recv().await, CtrlMessage::PairProof { .. }));
    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;
    server.stop().await;
}

#[tokio::test]
async fn prepared_with_a_bad_signature_is_rejected_then_retried() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;
    let digest = transcript.transcript_digest();

    // A Prepared signed by the wrong key: rejected with the unified
    // unavailable error; the transaction is untouched.
    let impostor = DeviceKeyPair::generate();
    owner
        .send(&CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest: digest,
            signature: impostor.sign(&hello2::prepared_message(&pairing_id, &digest)),
        })
        .await;
    assert_eq!(owner.recv_pair_error().await, PAIR_UNAVAILABLE);

    // The genuine retry proceeds to commit.
    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;
    server.stop().await;
}

// ---- recovery ------------------------------------------------------------------------

#[tokio::test]
async fn server_restart_recovers_active_codes_and_pending_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("reg.sqlite");
    let registry = DeviceKeyPair::generate();
    let server = TestServer::spawn_at(db.clone(), registry.clone()).await;
    let owner_key = DeviceKeyPair::generate();

    let invite = InviteCode::generate();
    let code_id;
    let _pairing_id;
    {
        let mut owner = Client::connect(&server, owner_key.clone(), "o").await;
        let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
        let (id, _revision) = claim(&mut owner, &invite, 0).await;
        code_id = id;
        // Owner offline during the redeem: the transaction stays pending.
        drop(owner);
        _pairing_id = redeem(&mut redeemer, &server.registry_id(), &invite)
            .await
            .1;
    }
    server.stop().await;

    // Restart with the same registry key and database (new ephemeral port
    // — recovery is about durable state, not the socket).
    let server = TestServer::spawn_at(db, registry.clone()).await;

    // The claimed code is still not re-issuable…
    let mut stranger = Client::connect(&server, DeviceKeyPair::generate(), "s").await;
    stranger
        .send(&CtrlMessage::InviteClaim {
            request_id: random_id16(),
            lookup_digest: invite.lookup_digest(),
            ttl_secs: 0,
        })
        .await;
    assert_eq!(stranger.recv_pair_error().await, CODE_CONFLICT);

    // …and the owner's reconnect delivers the pending offer catch-up.
    let mut owner = Client::connect(&server, owner_key, "o").await;
    match owner.recv().await {
        CtrlMessage::InviteRedeemOffered {
            code_id: caught_up, ..
        } => assert_eq!(caught_up, code_id),
        other => panic!("expected recovered offer, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test]
async fn status_query_resends_commit_and_ready_for_reconnected_clients() {
    let dir = tempfile::tempdir().unwrap();
    let server =
        TestServer::spawn_at(dir.path().join("reg.sqlite"), DeviceKeyPair::generate()).await;
    let owner_key = DeviceKeyPair::generate();
    let redeemer_key = DeviceKeyPair::generate();
    let (pairing_id, digest, commit_revision) = {
        let mut owner = Client::connect(&server, owner_key.clone(), "o").await;
        let mut redeemer = Client::connect(&server, redeemer_key.clone(), "r").await;
        let invite = InviteCode::generate();
        let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
        let (pairing_id, transcript, _record) = flow_to_proofs(
            &server,
            &mut owner,
            &mut redeemer,
            &invite,
            code_id,
            code_revision,
        )
        .await;
        let d = transcript.transcript_digest();
        let revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &d).await;
        // Both clients drop before sending Applied.
        (pairing_id, d, revision)
    };

    // Reconnect (fresh TLS, fresh Hello) and query status: the reply is
    // followed by the fixed PairCommit resend (contract §6).
    let mut owner = Client::connect(&server, owner_key, "o").await;
    let mut redeemer = Client::connect(&server, redeemer_key, "r").await;
    owner
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Committed),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    assert!(matches!(owner.recv().await, CtrlMessage::PairCommit { .. }));

    // Complete from the recovered state: Applied both → Ready both.
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;

    // A ready transaction answers Ready plus the fixed PairReady resend.
    redeemer
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match redeemer.recv().await {
        CtrlMessage::PairStatusReply { state, .. } => assert_eq!(state, PairingState::Ready),
        other => panic!("expected PairStatusReply, got {other:?}"),
    }
    assert!(matches!(
        redeemer.recv().await,
        CtrlMessage::PairReady { .. }
    ));
    server.stop().await;
}

#[tokio::test]
async fn status_query_by_non_participant_is_unified_unavailable() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;
    let mut stranger = Client::connect(&server, DeviceKeyPair::generate(), "s").await;
    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, _t, _r) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;

    stranger
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    assert_eq!(stranger.recv_pair_error().await, PAIR_UNAVAILABLE);

    // Unknown pairing ids look exactly the same (no oracle).
    stranger
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id: [0x99; 16],
        })
        .await;
    assert_eq!(stranger.recv_pair_error().await, PAIR_UNAVAILABLE);
    server.stop().await;
}

// ---- v2 sessions ---------------------------------------------------------------------

#[tokio::test]
async fn connect_request_and_signal_relay_are_gated_by_the_verified_triple() {
    let server = TestServer::spawn_memory().await;
    let mut owner = Client::connect(&server, DeviceKeyPair::generate(), "o").await;
    let mut redeemer = Client::connect(&server, DeviceKeyPair::generate(), "r").await;

    let invite = InviteCode::generate();
    let (code_id, code_revision) = claim(&mut owner, &invite, 0).await;
    let (pairing_id, transcript, _record) = flow_to_proofs(
        &server,
        &mut owner,
        &mut redeemer,
        &invite,
        code_id,
        code_revision,
    )
    .await;
    let digest = transcript.transcript_digest();
    let commit_revision = drive_commit(&mut owner, &mut redeemer, &pairing_id, &digest).await;
    drive_ready(
        &mut owner,
        &mut redeemer,
        &pairing_id,
        &digest,
        commit_revision,
    )
    .await;

    // Before a relation exists, an arbitrary triple is refused.
    let connection_id = random_id16();
    redeemer
        .send(&CtrlMessage::ConnectRequest {
            request_id: random_id16(),
            target: owner.device_id(),
            pairing_id: [0x77; 16],
            code_id,
            connection_id,
        })
        .await;
    assert_eq!(redeemer.recv_pair_error().await, PAIR_UNAVAILABLE);

    // The verified triple opens a session for both endpoints.
    redeemer
        .send(&CtrlMessage::ConnectRequest {
            request_id: random_id16(),
            target: owner.device_id(),
            pairing_id,
            code_id,
            connection_id,
        })
        .await;
    let session = match redeemer.recv().await {
        CtrlMessage::SessionOpened {
            session: s,
            peer,
            pairing_id: p,
            code_id: c,
            connection_id: cn,
            ..
        } => {
            assert_eq!(peer, owner.device_id());
            assert_eq!((p, c, cn), (pairing_id, code_id, connection_id));
            s
        }
        other => panic!("expected SessionOpened, got {other:?}"),
    };
    match owner.recv().await {
        CtrlMessage::SessionOpened {
            session: s, peer, ..
        } => {
            assert_eq!(peer, redeemer.device_id());
            assert_eq!(s, session);
        }
        other => panic!("expected SessionOpened, got {other:?}"),
    }

    // Both endpoints sit on the same loopback egress: the server sends
    // its SameEgress ordering hint right after SessionOpened.
    match owner.recv().await {
        CtrlMessage::Signal {
            signal: PeerSignal::SameEgress,
            ..
        } => {}
        other => panic!("expected SameEgress hint, got {other:?}"),
    }

    // Signals relay verbatim within the session.
    let candidates = vec!["127.0.0.1:9000".parse().unwrap()];
    redeemer
        .send(&CtrlMessage::Signal {
            session,
            signal: PeerSignal::Candidates(candidates.clone()),
        })
        .await;
    match owner.recv().await {
        CtrlMessage::Signal { signal, .. } => {
            assert_eq!(signal, PeerSignal::Candidates(candidates));
        }
        other => panic!("expected relayed Signal, got {other:?}"),
    }

    // Cancel is idempotent and silent.
    redeemer.send(&CtrlMessage::Cancel { session }).await;
    redeemer.send(&CtrlMessage::Cancel { session }).await;
    server.stop().await;
}
