use super::*;
use crate::server_v2::tls;
use std::net::Ipv4Addr;

#[test]
fn hello_replay_cache_admits_once_per_window() {
    let mut cache = HelloReplayCache::default();
    let t0 = Instant::now();
    let window = Duration::from_secs(10);
    let digest = [0xAB; 32];
    assert!(cache.admit(digest, t0, window));
    assert!(!cache.admit(digest, t0 + Duration::from_secs(1), window));
    assert!(cache.admit(digest, t0 + Duration::from_secs(11), window));
}

/// R36-P2-04 remainder (run 43321601964 diagnosis): when a newer
/// same-device Hello replaces an older connection, the OLD connection
/// must receive the ERR_DEVICE_REPLACED reason FRAME before the close
/// — a bare close reads as plain EOF on the client, `replace_closed`
/// never sets, and the daemon keeper reconnects immediately (backoff
/// 0), re-killing the one-shot that replaced it (the invite-revoke
/// connection war). This pins the SERVER side of that contract; the
/// client pacing side is pinned in ak-session's keeper tests.
#[tokio::test]
async fn replaced_connection_receives_the_reason_frame_before_eof() {
    let server = spawn_server().await;
    let key = DeviceKeyPair::generate();
    let mut first = TestClient::connect(server.addr, &server.registry_id(), key.clone(), "a").await;
    // Same device identity connects again: the server replaces the
    // first connection.
    let _second = TestClient::connect(server.addr, &server.registry_id(), key, "a").await;
    // The FIRST connection must now see the reason frame — not a
    // bare EOF — and only then the stream end.
    let frame = first.recv().await;
    match frame {
        CtrlMessage::ServerError { code, message } => {
            assert_eq!(code, 0x1001, "the reserved ERR_DEVICE_REPLACED code");
            assert!(
                !message.is_empty(),
                "the reason carries a human-readable message"
            );
        }
        other => panic!("expected ServerError(0x1001), got {other:?}"),
    }
    // After the frame, the stream closes: raw read returns None
    // (EOF) or an error — recv() would panic on that, so read
    // directly.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        crate::server_v2::framing::read_frame(&mut first.stream).await
    })
    .await;
    assert!(
        matches!(closed, Ok(Ok(None)) | Ok(Err(_)) | Err(_)),
        "stream must end (EOF or error) after the reason frame: {closed:?}"
    );
    server.stop().await;
}

/// G6 §6a: the Welcome advertises the server-info capability bit, and
/// a `ServerInfoRequest` is answered with the configured build
/// revision — never pushed unasked (an idle connection sees nothing).
#[tokio::test]
async fn server_info_is_advertised_and_answered_on_request() {
    let server = spawn_server_with_revision("0.1.0+test1234").await;
    let key = DeviceKeyPair::generate();
    let mut client = TestClient::connect(server.addr, &server.registry_id(), key, "info").await;
    assert!(
        client.welcome_caps & crate::server_v2::CAP_SERVER_INFO != 0,
        "the Welcome must advertise the server-info bit (got 0x{:08x})",
        client.welcome_caps
    );
    client.send(&CtrlMessage::ServerInfoRequest).await;
    match client.recv().await {
        CtrlMessage::ServerInfo { revision } => {
            assert_eq!(revision, "0.1.0+test1234");
        }
        other => panic!("expected ServerInfo, got {other:?}"),
    }
    // Nothing is pushed without a request: an idle read window stays
    // silent (the recv-deadline surfaces as a timeout error, not a
    // frame).
    let unsolicited = tokio::time::timeout(Duration::from_millis(300), client.recv()).await;
    assert!(unsolicited.is_err(), "the server never pushes ServerInfo");
    server.stop().await;
}

/// G6 §6a: a server started without a configured revision (every
/// pre-G6 embedder of `serve_v2`) still advertises the bit and
/// answers `unknown` — the answer is always well-formed.
#[tokio::test]
async fn server_info_without_a_configured_revision_answers_unknown() {
    let server = spawn_server().await;
    let key = DeviceKeyPair::generate();
    let mut client = TestClient::connect(server.addr, &server.registry_id(), key, "anon").await;
    assert!(client.welcome_caps & crate::server_v2::CAP_SERVER_INFO != 0);
    client.send(&CtrlMessage::ServerInfoRequest).await;
    match client.recv().await {
        CtrlMessage::ServerInfo { revision } => assert_eq!(revision, "unknown"),
        other => panic!("expected ServerInfo(unknown), got {other:?}"),
    }
    server.stop().await;
}

#[test]
fn sanitize_revision_degrades_illegal_build_stamps() {
    use super::sanitize_revision;
    assert_eq!(sanitize_revision(None).as_ref(), "unknown");
    assert_eq!(sanitize_revision(Some("")).as_ref(), "unknown");
    assert_eq!(
        sanitize_revision(Some(&"x".repeat(33))).as_ref(),
        "unknown",
        "over the 32-byte wire budget"
    );
    assert_eq!(
        sanitize_revision(Some("0.1.0+\u{1}evil")).as_ref(),
        "unknown",
        "control characters are not a fingerprint"
    );
    assert_eq!(
        sanitize_revision(Some("0.1.0+4739f4d")).as_ref(),
        "0.1.0+4739f4d"
    );
}

#[test]
fn transcript_bindings_reject_key_and_code_substitution() {
    let registry = [0x11u8; 32];
    let snapshot = PairingSnapshot {
        pairing_id: [0x22; 16],
        request_id: [0x33; 16],
        code_id: [0x44; 16],
        lookup_digest: [0x55; 32],
        owner: [0x66; 32],
        redeemer: [0x77; 32],
        state: PairingState::Pending,
        deadline_ms: 9_000,
        code_revision: 1,
        commit_revision: None,
        transcript: None,
        transcript_digest: None,
        redeemer_nonce: [0x88; 32],
        owner_proof: None,
        redeemer_proof: None,
        owner_prepared_signature: None,
        redeemer_prepared_signature: None,
        owner_applied_signature: None,
        redeemer_applied_signature: None,
        owner_endpoint: None,
        redeemer_endpoint: None,
    };
    let typed = PairTranscript {
        registry_id: registry,
        code_id: snapshot.code_id,
        code_revision: 1,
        pairing_id: snapshot.pairing_id,
        request_id: snapshot.request_id,
        lookup_digest: snapshot.lookup_digest,
        owner_device_id: snapshot.owner,
        redeemer_device_id: snapshot.redeemer,
        owner_nonce: [0; 32],
        redeemer_nonce: snapshot.redeemer_nonce,
        deadline_ms: 9_000,
    };
    assert!(transcript_bindings_ok(&typed, &snapshot, &registry));
    // Key substitution: swapped redeemer key.
    let mut bad = typed;
    bad.redeemer_device_id = [0x99; 32];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    // Code substitution: foreign code id / lookup digest / revision.
    let mut bad = typed;
    bad.code_id = [0xAA; 16];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    let mut bad = typed;
    bad.lookup_digest = [0xBB; 32];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    let mut bad = typed;
    bad.code_revision = 2;
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    // Wrong registry (malicious server substitution).
    let mut bad = typed;
    bad.registry_id = [0x00; 32];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    // Wrong nonce / request / deadline.
    let mut bad = typed;
    bad.redeemer_nonce = [1; 32];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    let mut bad = typed;
    bad.request_id = [2; 16];
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
    let mut bad = typed;
    bad.deadline_ms = 8_999;
    assert!(!transcript_bindings_ok(&bad, &snapshot, &registry));
}

// ---- integration harness (P1-5 / P2 regression tests) -------------------

struct TestServer {
    addr: SocketAddr,
    registry: DeviceKeyPair,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<io::Result<()>>,
}

async fn spawn_server() -> TestServer {
    spawn_server_with(Limits::default()).await
}

/// A server with explicitly-injected budgets: the global-cap
/// regression tests isolate the GLOBAL admission semantics and must
/// not trip the (much smaller) per-source caps first — see
/// [`global_cap_isolation_limits`].
async fn spawn_server_with(limits: Limits) -> TestServer {
    let store = RegistryStore::open_in_memory().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let registry = DeviceKeyPair::generate();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let key = registry.clone();
    let task =
        tokio::spawn(async move { serve_v2(listener, &key, store, limits, shutdown_rx).await });
    TestServer {
        addr,
        registry,
        shutdown,
        task,
    }
}

/// A server answering `ServerInfoRequest` with an explicit build
/// revision (G6 §6a; the plain `spawn_server_with` answers
/// `"unknown"` — the unconfigured default).
async fn spawn_server_with_revision(revision: &str) -> TestServer {
    let store = RegistryStore::open_in_memory().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let registry = DeviceKeyPair::generate();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let key = registry.clone();
    let revision = revision.to_string();
    let task = tokio::spawn(async move {
        serve_v2_with_revision(
            listener,
            &key,
            store,
            Limits::default(),
            Some(revision.as_str()),
            shutdown_rx,
        )
        .await
    });
    TestServer {
        addr,
        registry,
        shutdown,
        task,
    }
}

/// Default budgets EXCEPT the per-source caps raised past the global
/// serving cap AND the reject budget (CR-P1-03): lets the global-cap
/// tests below open MAX_LIVE_CONNECTIONS held sockets plus the
/// reject-budget pinning sockets from the single loopback source the
/// harness has. The per-source behavior itself is pinned by the
/// CR-P1-03 tests, which use the default limits and TWO sources
/// (127.0.0.1 and the 127.0.0.2 loopback alias — the whole 127/8 is
/// loopback, so no second interface or real address is involved).
fn global_cap_isolation_limits() -> Limits {
    Limits {
        max_conns_per_source: MAX_LIVE_CONNECTIONS + MAX_REJECT_TASKS + 1,
        max_handshaking_per_source: MAX_LIVE_CONNECTIONS + MAX_REJECT_TASKS + 1,
        ..Limits::default()
    }
}

impl TestServer {
    fn registry_id(&self) -> RegistryId {
        self.registry.public_key()
    }

    async fn stop(self) {
        let _ = self.shutdown.send(true);
        let joined = tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("serve_v2 returns after shutdown (bounded abort+join)");
        joined.expect("task join").expect("serve_v2 exits Ok");
    }
}

struct TestClient {
    stream: tokio_rustls::client::TlsStream<TcpStream>,
    key: DeviceKeyPair,
    /// The `Welcome.capabilities` this connection's Welcome carried
    /// (G6 tests assert the advertised bits).
    welcome_caps: u32,
    /// `name@local-address`: which of possibly hundreds of test
    /// connections a missed frame deadline belongs to (L-N3, review
    /// 2026-11-17 — a deadline panic used to say only `Elapsed`).
    label: String,
}

impl TestClient {
    async fn connect(
        addr: SocketAddr,
        registry_id: &RegistryId,
        key: DeviceKeyPair,
        name: &str,
    ) -> TestClient {
        TestClient::connect_at(None, addr, registry_id, key, name).await
    }

    /// [`TestClient::connect`], but the client socket is first bound
    /// to `local` when given — CR-P1-03 tests exercise per-source
    /// budgets from two distinct loopback sources on one host
    /// (127.0.0.1 and the 127.0.0.2 alias). `TcpSocket` is the one
    /// tokio API that binds a source address before connecting; no
    /// extra dependency needed.
    async fn connect_at(
        local: Option<IpAddr>,
        addr: SocketAddr,
        registry_id: &RegistryId,
        key: DeviceKeyPair,
        name: &str,
    ) -> TestClient {
        let tcp = match local {
            None => TcpStream::connect(addr).await.expect("tcp connect"),
            Some(source) => {
                let socket = tokio::net::TcpSocket::new_v4().expect("v4 client socket");
                socket
                    .bind(SocketAddr::new(source, 0))
                    .expect("bind loopback source alias");
                socket.connect(addr).await.expect("tcp connect from source")
            }
        };
        tcp.set_nodelay(true).expect("nodelay");
        let label = format!("{name}@{}", tcp.local_addr().expect("client local address"));
        let connector = tls::client_config(registry_id).expect("pinned client config");
        let mut stream = connector
            .connect(
                rustls::pki_types::ServerName::IpAddress(addr.ip().into()),
                tcp,
            )
            .await
            .expect("pinned tls handshake");
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
        assert!(expires_ms > now_ms());
        let exporter = hello2::exporter_secret_client(stream.get_ref().1).unwrap();
        let timestamp = now_ms();
        let signed = hello2::hello2_message(
            &registry_id,
            &exporter,
            u16::from(crate::protocol_v2::VERSION),
            &key.public_key(),
            name,
            timestamp,
            &nonce,
        );
        write_message(
            &mut stream,
            &CtrlMessage::Hello {
                version: u16::from(crate::protocol_v2::VERSION),
                device_id: key.public_key(),
                name: name.to_owned(),
                timestamp_ms: timestamp,
                challenge: nonce,
                signature: key.sign(&signed),
            },
            Role::Client,
        )
        .await
        .unwrap();
        let mut client = TestClient {
            stream,
            key,
            welcome_caps: 0,
            label,
        };
        match client.recv().await {
            CtrlMessage::Welcome { capabilities, .. } => {
                client.welcome_caps = capabilities;
                client
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    async fn send(&mut self, message: &CtrlMessage) {
        write_message(&mut self.stream, message, Role::Client)
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> CtrlMessage {
        let deadline = Duration::from_secs(10);
        match tokio::time::timeout(deadline, async {
            let bytes = read_frame(&mut self.stream).await.unwrap().expect("frame");
            CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message
        })
        .await
        {
            Ok(message) => message,
            Err(_) => self.missed_deadline(deadline),
        }
    }

    /// A frame deadline passed with the connection still open (an EOF
    /// panics inside the read instead): name the connection.
    fn missed_deadline(&self, deadline: Duration) -> ! {
        panic!(
            "{}: no complete frame within {deadline:?} and no EOF — a frame \
             stuck in flight, not a closed connection (L-N3)",
            self.label
        )
    }
}

/// A signed redeem request over an arbitrary lookup digest (the server
/// verifies the signature, never the MAC).
fn signed_redeem(
    key: &DeviceKeyPair,
    registry_id: &RegistryId,
    lookup: Digest32,
    request_id: RequestId,
    deadline: u64,
) -> RedeemRequest {
    let redeemer_nonce = random_nonce32();
    let request_mac = [0x5A; 32];
    let signature = key.sign(&hello2::redeem_message(
        registry_id,
        &lookup,
        &key.public_key(),
        &request_id,
        &redeemer_nonce,
        deadline,
        &request_mac,
    ));
    RedeemRequest {
        request_id,
        lookup_digest: lookup,
        redeemer_nonce,
        deadline_ms: deadline,
        request_mac,
        signature,
    }
}

// ---- P2 反枚举 -----------------------------------------------------------

/// A status query for one's own LIVE ghost answers with the exact
/// `PairStatusReply(Pending)` shape a real pending transaction gets.
/// Before the fix the ghost fell through to the SQLite lookup and
/// answered `PAIR_UNAVAILABLE` immediately — one status query right
/// after the redeem pending told a redeemer whether the code existed.
#[tokio::test]
async fn status_query_of_a_live_ghost_replies_like_a_real_pending_transaction() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut redeemer =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

    let deadline = now_ms() + 8_000;
    let request = signed_redeem(
        &redeemer.key,
        &registry_id,
        [0xC0; 32],
        random_id16(),
        deadline,
    );
    redeemer.send(&CtrlMessage::InviteRedeem(request)).await;
    let pairing_id = match redeemer.recv().await {
        CtrlMessage::InviteRedeemPending {
            pairing_id,
            deadline_ms,
            ..
        } => {
            assert_eq!(deadline_ms, deadline);
            pairing_id
        }
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    };

    redeemer
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match redeemer.recv().await {
        CtrlMessage::PairStatusReply {
            pairing_id: answered,
            state,
            commit_revision,
            deadline_ms,
            ..
        } => {
            assert_eq!(answered, pairing_id);
            assert_eq!(state, PairingState::Pending);
            assert_eq!(commit_revision, 0);
            assert_eq!(deadline_ms, deadline);
        }
        other => panic!("expected PairStatusReply, got {other:?}"),
    }

    // Another device probing the same id: still the unified
    // unavailable (no oracle in either direction).
    let mut stranger =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "s").await;
    stranger
        .send(&CtrlMessage::PairStatusQuery {
            request_id: random_id16(),
            pairing_id,
        })
        .await;
    match stranger.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, PAIR_UNAVAILABLE),
        other => panic!("expected PairError, got {other:?}"),
    }
    server.stop().await;
}

// ---- P2 redeem 幂等 --------------------------------------------------------

/// A retry of the same (redeemer, request_id) replays the ORIGINAL
/// pairing id instead of minting a second transaction; a different
/// attempt reusing the id is a REQUEST_CONFLICT (same discipline as
/// the invite claim journal).
#[tokio::test]
async fn redeem_retry_with_the_same_request_id_replays_the_original_pairing() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut redeemer =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

    let deadline = now_ms() + 8_000;
    let request_id = random_id16();
    let request = signed_redeem(
        &redeemer.key,
        &registry_id,
        [0xEE; 32],
        request_id,
        deadline,
    );

    redeemer
        .send(&CtrlMessage::InviteRedeem(request.clone()))
        .await;
    let first = match redeemer.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    };

    // Identical retry: the same pairing id, the same deadline — no
    // second ghost, no second pending transaction.
    redeemer
        .send(&CtrlMessage::InviteRedeem(request.clone()))
        .await;
    match redeemer.recv().await {
        CtrlMessage::InviteRedeemPending {
            pairing_id: replayed,
            deadline_ms,
            ..
        } => {
            assert_eq!(replayed, first, "retry must replay the original pairing id");
            assert_eq!(deadline_ms, deadline);
        }
        other => panic!("expected InviteRedeemPending replay, got {other:?}"),
    }

    // Same request id, different attempt: REQUEST_CONFLICT.
    let conflicting = signed_redeem(
        &redeemer.key,
        &registry_id,
        [0xEF; 32],
        request_id,
        deadline,
    );
    redeemer.send(&CtrlMessage::InviteRedeem(conflicting)).await;
    match redeemer.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, REQUEST_CONFLICT),
        other => panic!("expected REQUEST_CONFLICT, got {other:?}"),
    }
    server.stop().await;
}

// ---- P1-5 resource bounds ---------------------------------------------------

/// One connection beyond the global cap completes the TLS handshake
/// and receives exactly one ServerError before the disconnect.
/// CR-P1-03: the server runs with the per-source caps raised to the
/// serving cap (the harness has one loopback source) — this test
/// isolates the GLOBAL path; the per-source caps have their own
/// tests below.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn connections_beyond_the_global_cap_are_rejected_with_server_error() {
    let server = spawn_server_with(global_cap_isolation_limits()).await;
    let registry_id = server.registry_id();

    // Hold the cap's worth of live connections (TLS up, challenge
    // read, never Hello — they stay counted until closed).
    let mut held = Vec::new();
    for _ in 0..MAX_LIVE_CONNECTIONS {
        let mut stream = tls::connect_pinned(server.addr, &registry_id)
            .await
            .expect("tls handshake");
        let bytes = read_frame(&mut stream).await.unwrap().expect("challenge");
        match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
            CtrlMessage::AuthChallenge { .. } => {}
            other => panic!("expected AuthChallenge, got {other:?}"),
        }
        held.push(stream);
    }

    let mut extra = tls::connect_pinned(server.addr, &registry_id)
        .await
        .expect("over-cap tls still completes");
    let bytes = read_frame(&mut extra).await.unwrap().expect("error frame");
    match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
        CtrlMessage::ServerError { code, .. } => assert_eq!(code, ERR_SESSION_LIMIT),
        other => panic!("expected ServerError, got {other:?}"),
    }
    assert_eq!(
        read_frame(&mut extra).await.unwrap(),
        None,
        "the rejected connection is closed after the error"
    );
    drop(held);
    drop(extra);
    server.stop().await;
}

/// R36-P1-03: a failed handshake (peer dies before the TLS handshake
/// completes, or sends garbage into it) must return its admission
/// slot. A full cap's worth of failures may not lock legal clients
/// out until process restart — before the RAII guard the counter was
/// only released on the successful connection-loop path. CR-P1-03:
/// per-source caps raised to the serving cap for the same isolation
/// reason as the test above (the flood and the probe share the one
/// loopback source); the per-source release discipline is pinned by
/// `source_counters_admit_release_and_prune_by_ttl` below.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn failed_handshakes_release_their_admission_quota() {
    let server = spawn_server_with(global_cap_isolation_limits()).await;
    let registry_id = server.registry_id();

    // A full cap's worth of connections that close before the TLS
    // handshake even starts, then a batch that connects, pushes
    // garbage into the handshake, and closes. The tiny periodic
    // yield keeps the OS accept backlog from saturating on hosts
    // with a small listen queue.
    for index in 0..MAX_LIVE_CONNECTIONS {
        let stream = TcpStream::connect(server.addr).await.unwrap();
        drop(stream);
        if index.is_multiple_of(64) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    for index in 0..MAX_LIVE_CONNECTIONS {
        let mut stream = TcpStream::connect(server.addr).await.unwrap();
        let _ = stream.write_all(&[0xB0_u8; 32]).await;
        drop(stream);
        if index.is_multiple_of(64) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // Every failed handshake returned its slot: a legal client gets
    // the AuthChallenge (an over-cap server would answer one
    // ServerError instead). Poll with retries so the assertion does
    // not race the failure cleanup; the deadline is the verdict.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut admitted = false;
    while !admitted {
        assert!(
            Instant::now() < deadline,
            "legal client never re-admitted after failed handshakes \
             (admission quota leaked)"
        );
        let mut probe = tls::connect_pinned(server.addr, &registry_id)
            .await
            .expect("tls handshake");
        match tokio::time::timeout(Duration::from_secs(5), read_frame(&mut probe)).await {
            Ok(Ok(Some(bytes))) => {
                match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
                    CtrlMessage::AuthChallenge { .. } => admitted = true,
                    CtrlMessage::ServerError { code, .. } => {
                        assert_eq!(
                            code, ERR_SESSION_LIMIT,
                            "only the connection-cap error may intervene here"
                        );
                    }
                    other => panic!("unexpected first frame: {other:?}"),
                }
            }
            other => panic!("unexpected probe read: {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Full end-to-end proof: Hello/Welcome still served.
    let client =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "c").await;
    drop(client);
    server.stop().await;
}

/// R36-P1-03: past the serving cap, polite rejections run under their
/// own small budget. Once that budget is pinned by silent sockets,
/// further over-cap connections are dropped at accept time — no
/// handshake task is spawned for them (the socket sees an immediate
/// close instead of parking until the TLS timeout), and shutdown
/// still tracks and reaps everything. CR-P1-03: per-source caps
/// raised to the serving cap (single loopback source) — the
/// per-source equivalent of this drop-at-accept behavior is pinned
/// by `over_source_cap_sockets_are_dropped_at_accept_without_a_task`.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn over_cap_flood_beyond_the_reject_budget_is_dropped_immediately() {
    use tokio::io::AsyncReadExt as _;

    let server = spawn_server_with(global_cap_isolation_limits()).await;
    let registry_id = server.registry_id();

    // Hold the cap's worth of live connections (TLS up, challenge
    // read, never Hello — they stay counted until closed).
    let mut held = Vec::new();
    for _ in 0..MAX_LIVE_CONNECTIONS {
        let mut stream = tls::connect_pinned(server.addr, &registry_id)
            .await
            .expect("tls handshake");
        let bytes = read_frame(&mut stream).await.unwrap().expect("challenge");
        match CtrlFrame::decode_as(&bytes, Role::Client).unwrap().message {
            CtrlMessage::AuthChallenge { .. } => {}
            other => panic!("expected AuthChallenge, got {other:?}"),
        }
        held.push(stream);
    }

    // Pin the whole reject budget with plain TCP sockets that never
    // start the TLS handshake (each holds one reject task parked in
    // acceptor.accept until the TLS budget expires).
    let mut silent = Vec::new();
    for _ in 0..MAX_REJECT_TASKS {
        silent.push(TcpStream::connect(server.addr).await.unwrap());
    }

    // The next over-cap connection is not even handshaked: the server
    // closes it at accept time (immediate EOF/reset), it does not
    // park until the TLS timeout.
    let mut extra = TcpStream::connect(server.addr).await.unwrap();
    let mut byte = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(2), extra.read(&mut byte)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("no bytes may be sent to a dropped over-cap connection"),
        Err(_) => panic!("over-cap connection must be dropped immediately, not parked"),
    }

    drop(held);
    drop(silent);
    drop(extra);
    server.stop().await;
}

// ---- CR-P1-03 per-source budgets ---------------------------------------

/// The accounting key collapses what it must: a full IPv4 address
/// (one host), an IPv6 address to its /64 delegation, and
/// IPv4-mapped IPv6 to the IPv4 source (a dual-stack bind must not
/// double a host's budgets). Documentation-range addresses only.
#[test]
fn source_key_collapses_hosts_prefixes_and_mapped_v4() {
    let v4: IpAddr = "192.0.2.10".parse().unwrap();
    assert_eq!(source_key(v4), v4, "IPv4 keys by the full address (/32)");
    // Same /64, different host bits: one source.
    let a: IpAddr = "2001:db8:1:2:3:4:5:6".parse().unwrap();
    let b: IpAddr = "2001:db8:1:2:ffff::1".parse().unwrap();
    assert_eq!(source_key(a), source_key(b), "IPv6 keys by the /64 prefix");
    assert_eq!(
        source_key(a),
        "2001:db8:1:2::".parse::<IpAddr>().unwrap(),
        "the /64 key carries no host bits"
    );
    // A different /64 is a different source.
    let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
    assert_ne!(source_key(a), source_key(c));
    // IPv4-mapped IPv6 collapses to the IPv4 source.
    let mapped: IpAddr = "::ffff:192.0.2.10".parse().unwrap();
    assert_eq!(source_key(mapped), v4);
}

/// CR-P1-03 acceptance (1): one source opens a pile of TCP-only
/// connections (never starting the TLS handshake) — the flood pins
/// at most its OWN per-source handshaking budget, and a client from
/// a different source still completes the full TLS + Hello/Welcome
/// exchange within ~1 s. Before the per-source caps the same flood
/// occupied the whole global serving cap for the TLS timeout and
/// locked every source out.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn tcp_only_flood_from_one_source_leaves_another_source_unharmed() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();

    // Three times the per-source concurrent cap, connect-only.
    let mut flood = Vec::new();
    for _ in 0..(3 * crate::server_v2::config::MAX_CONNS_PER_SOURCE) {
        flood.push(TcpStream::connect(server.addr).await.unwrap());
    }
    // Let the accept loop process the backlog before timing the
    // victim, so the flood's effect is in force for the measurement.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    let victim = TestClient::connect_at(
        Some("127.0.0.2".parse().unwrap()),
        server.addr,
        &registry_id,
        DeviceKeyPair::generate(),
        "victim",
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "another source must complete Hello within ~1 s despite a \
         connect-only flood (took {:?})",
        started.elapsed()
    );
    drop(victim);
    drop(flood);
    server.stop().await;
}

/// CR-P1-03 acceptance (2): one source redeem-floods through six
/// self-generated identities — the exact shape that used to exhaust
/// the flat global 60/min window and RATE_LIMIT every other
/// source's join attempts. The flood saturates only its own
/// per-source window; a different source's legit redeem goes
/// through untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn identity_redeem_flood_from_one_source_leaves_other_sources_alone() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();

    // 6 identities x 6 attempts = 36/min from 127.0.0.1: past the
    // 30/min per-source budget while every identity stays under its
    // own 30/min and every connection under its 10/min window (the
    // old attack worked precisely because each bucket was per-something
    // the attacker could multiply).
    let mut rate_limited = 0usize;
    let mut flood = Vec::new();
    for index in 0..6u8 {
        let mut client = TestClient::connect(
            server.addr,
            &registry_id,
            DeviceKeyPair::generate(),
            &format!("flood-{index}"),
        )
        .await;
        for _ in 0..6 {
            let redeem = signed_redeem(
                &client.key,
                &registry_id,
                [index; 32],
                random_id16(),
                now_ms() + 8_000,
            );
            client.send(&CtrlMessage::InviteRedeem(redeem)).await;
            match client.recv().await {
                CtrlMessage::InviteRedeemPending { .. } => {}
                CtrlMessage::PairError { code, .. } => {
                    // The ONLY legal rejection here is the flood
                    // source's own budget — never a foreign code.
                    assert_eq!(code, RATE_LIMITED);
                    rate_limited += 1;
                }
                other => panic!("unexpected redeem answer: {other:?}"),
            }
        }
        flood.push(client);
    }
    assert!(
        rate_limited > 0,
        "36 attempts/min from one source must exhaust that source's \
         own 30/min window (got no RATE_LIMITED at all)"
    );

    // The other source's legit redeem (fresh source window, fresh
    // identity, breaker far away): pending as usual, NOT the global
    // RATE_LIMITED this flood used to cause for everyone.
    let mut victim = TestClient::connect_at(
        Some("127.0.0.2".parse().unwrap()),
        server.addr,
        &registry_id,
        DeviceKeyPair::generate(),
        "victim",
    )
    .await;
    let redeem = signed_redeem(
        &victim.key,
        &registry_id,
        [0x9E; 32],
        random_id16(),
        now_ms() + 8_000,
    );
    victim.send(&CtrlMessage::InviteRedeem(redeem)).await;
    match victim.recv().await {
        CtrlMessage::InviteRedeemPending { .. } => {}
        CtrlMessage::PairError { code, .. } => {
            panic!("another source's legit redeem must not be rate limited, got {code}")
        }
        other => panic!("unexpected victim redeem answer: {other:?}"),
    }
    drop(flood);
    drop(victim);
    server.stop().await;
}

/// CR-P1-03 acceptance (3): past the per-source handshaking cap the
/// socket is dropped AT ACCEPT — no task is spawned for it: the
/// client sees an immediate close instead of parking until the TLS
/// deadline, and a different source keeps being served (the drop is
/// the source's own cap, not the global one).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn over_source_cap_sockets_are_dropped_at_accept_without_a_task() {
    use tokio::io::AsyncReadExt as _;

    let server = spawn_server().await;
    let registry_id = server.registry_id();

    // Pin the source's handshaking budget with silent TCP sockets
    // (each parks in acceptor.accept until the 10 s TLS deadline).
    let mut silent = Vec::new();
    for _ in 0..crate::server_v2::config::MAX_HANDSHAKING_PER_SOURCE {
        silent.push(TcpStream::connect(server.addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    // The next socket from the same source is dropped at accept: an
    // immediate close. A spawned handshake task would park it until
    // the TLS deadline instead.
    let mut extra = TcpStream::connect(server.addr).await.unwrap();
    let mut byte = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(2), extra.read(&mut byte)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("no bytes may be sent to a dropped over-cap socket"),
        Err(_) => panic!("over-source-cap socket must be dropped immediately, not parked"),
    }

    // The per-source drop is not a global outage: another source
    // completes the full exchange right away.
    let victim = TestClient::connect_at(
        Some("127.0.0.2".parse().unwrap()),
        server.addr,
        &registry_id,
        DeviceKeyPair::generate(),
        "victim",
    )
    .await;
    drop(victim);
    drop(silent);
    drop(extra);
    server.stop().await;
}

/// CR-P1-03: the shared redeem windows survive reconnects — cycling
/// TCP connections (a fresh per-connection bucket each time) must
/// not reset them. One identity from one source gets exactly the
/// shared budget no matter how many connections it spreads the
/// attempts over; the first attempt past it is RATE_LIMITED on a
/// connection whose own per-conn bucket is still empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn shared_redeem_windows_survive_reconnects() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let key = DeviceKeyPair::generate();

    // 6 attempts per connection (under the 10/min per-conn window),
    // reconnect between rounds. Both shared windows (source and
    // identity) sit at 30/min for this single source and identity,
    // so round 6 (attempt 31 overall) must be rejected although the
    // 6th connection's per-conn bucket is fresh.
    let mut admitted = 0usize;
    'rounds: for round in 0..6u8 {
        let mut client =
            TestClient::connect(server.addr, &registry_id, key.clone(), "flooder").await;
        for attempt in 0..6u8 {
            let lookup = [round * 16 + attempt; 32];
            let redeem = signed_redeem(
                &client.key,
                &registry_id,
                lookup,
                random_id16(),
                now_ms() + 8_000,
            );
            client.send(&CtrlMessage::InviteRedeem(redeem)).await;
            match client.recv().await {
                CtrlMessage::InviteRedeemPending { .. } => admitted += 1,
                CtrlMessage::PairError {
                    code,
                    retry_after_ms,
                    ..
                } => {
                    assert_eq!(code, RATE_LIMITED);
                    assert!(retry_after_ms > 0, "the limiter suggests a retry delay");
                    assert_eq!(
                        round, 5,
                        "the shared budget holds exactly 30, not {admitted}"
                    );
                    break 'rounds;
                }
                other => panic!("unexpected redeem answer: {other:?}"),
            }
        }
    }
    assert_eq!(
        admitted,
        crate::server_v2::config::REDEEM_PER_SOURCE_PER_MIN as usize,
        "exactly the shared per-source budget is admitted across reconnects"
    );
    server.stop().await;
}

/// CR-P1-03 unit level: the per-source counters admit up to both
/// caps, release on every close-out path, hand the handshaking slot
/// back as soon as the handshake resolves, and idle entries are
/// TTL-pruned while busy ones never are. CR-P2-10: source_admit now
/// also takes the pre-reservation global count (0 throughout this
/// test — the global side has its own tests) and returns a
/// SourceAdmission.
#[test]
fn source_counters_admit_release_and_prune_by_ttl() {
    let limits = Limits::default();
    let mut inner = Inner::default();
    let t0 = Instant::now();
    let src = "127.0.0.1".parse::<IpAddr>().unwrap();
    let admit = |inner: &mut Inner, limits: &Limits, src: IpAddr, t0: Instant| {
        matches!(
            inner.source_admit(limits, src, 0, t0),
            SourceAdmission::Admit
        )
    };

    // The handshaking cap fires before the concurrent cap (4 < 8).
    for _ in 0..limits.max_handshaking_per_source {
        assert!(admit(&mut inner, &limits, src, t0));
    }
    assert!(
        !admit(&mut inner, &limits, src, t0),
        "handshaking cap must reject while all four handshakes are open"
    );

    // Handshakes resolve: the handshaking budget reopens while the
    // live budget keeps counting.
    for _ in 0..limits.max_handshaking_per_source {
        inner.source_handshake_done(src, t0);
    }
    for extra in 0..(limits.max_conns_per_source - limits.max_handshaking_per_source) {
        assert!(
            admit(&mut inner, &limits, src, t0),
            "live cap allows the {extra}-th additional served connection"
        );
    }
    assert!(
        !admit(&mut inner, &limits, src, t0),
        "concurrent-connection cap must reject the 9th connection"
    );

    // Release everything (half via handshake_done + release, half via
    // the guard-Drop shape that releases a still-open handshake).
    for _ in 0..limits.max_conns_per_source {
        inner.source_handshake_done(src, t0);
        inner.source_release(src, false, t0);
    }
    assert!(
        admit(&mut inner, &limits, src, t0),
        "counters returned to zero"
    );
    inner.source_release(src, true, t0);

    // A zero-counter entry survives inside the TTL and is pruned
    // after it; a busy entry survives even past the TTL.
    inner.prune_source_tables(t0 + SOURCE_TABLE_IDLE_TTL);
    assert!(
        inner.sources.get(&src).is_some(),
        "inside the TTL nothing is pruned"
    );
    inner.prune_source_tables(t0 + SOURCE_TABLE_IDLE_TTL + Duration::from_secs(1));
    assert!(
        !inner.sources.get(&src).is_some(),
        "an idle zero-counter entry is TTL-pruned"
    );
    assert!(admit(&mut inner, &limits, src, t0));
    inner.prune_source_tables(t0 + SOURCE_TABLE_IDLE_TTL + Duration::from_secs(60));
    assert!(
        inner.sources.get(&src).is_some(),
        "an entry holding counters is never pruned"
    );
    // CR-P2-10: the live entry keeps the fair-share divisor exact.
    assert_eq!(inner.active_conn_sources, 1);
}

// ---- CR-P2-10 fair share, prefix aggregates, O(log n) eviction ---------

/// CR-P2-10: the /56 and /48 parents of a normalized source, and
/// their collapse rules.
#[test]
fn v6_parents_carry_the_delegation_hierarchy() {
    let v4: IpAddr = "192.0.2.9".parse().unwrap();
    assert_eq!(v6_parents(v4), None, "a /32 is its own source");
    let v6: IpAddr = "2001:db8:aaaa:bb07::1".parse().unwrap();
    let (p56, p48) = v6_parents(source_key(v6)).unwrap();
    assert_eq!(
        p56,
        "2001:db8:aaaa:bb00::".parse::<IpAddr>().unwrap(),
        "the /56 keeps the first 56 bits"
    );
    assert_eq!(
        p48,
        "2001:db8:aaaa::".parse::<IpAddr>().unwrap(),
        "the /48 keeps the first 48 bits"
    );
    // Any /64 inside one /56 shares its parents.
    let other64: IpAddr = "2001:db8:aaaa:bbff::1".parse().unwrap();
    assert_eq!(v6_parents(source_key(other64)).unwrap(), (p56, p48));
    // A different /56 (byte 6 differs) shares only the /48.
    let other56: IpAddr = "2001:db8:aaaa:bc00::1".parse().unwrap();
    let (q56, q48) = v6_parents(source_key(other56)).unwrap();
    assert_ne!(q56, p56);
    assert_eq!(q48, p48);
}

/// CR-P2-10: the connection fair-share formula —
/// `ceil(SOFT_CAP / active)` floored at [`FAIR_CONN_FLOOR`].
#[test]
fn conn_fair_share_splits_the_cap_with_a_floor() {
    assert_eq!(conn_fair_share(1), MAX_LIVE_CONNECTIONS);
    assert_eq!(conn_fair_share(2), MAX_LIVE_CONNECTIONS / 2);
    assert_eq!(conn_fair_share(3), 86, "ceil, not floor: 256/3 = 85.33");
    assert_eq!(conn_fair_share(64), 4);
    assert_eq!(conn_fair_share(128), 2);
    assert_eq!(conn_fair_share(300), FAIR_CONN_FLOOR, "the floor binds");
    assert_eq!(conn_fair_share(0), MAX_LIVE_CONNECTIONS, "defensive");
}

/// CR-P2-10 unit level, connections: at the soft cap a source at its
/// fair share is refused on the GLOBAL side (the polite rejection
/// path) while its own caps still have room; a below-share source —
/// every newcomer — is admitted PAST the soft cap; the hard cap
/// refuses everyone; releases keep the divisor exact.
#[test]
fn connection_fair_share_sheds_fat_sources_and_admits_newcomers() {
    let limits = Limits::default();
    let mut inner = Inner::default();
    let t0 = Instant::now();
    let src = |i: u16| IpAddr::V4(Ipv4Addr::new(10, 0, (i >> 8) as u8, i as u8));
    let admit = |inner: &mut Inner, i: u16, conn_count: &mut usize| {
        let decision = inner.source_admit(&limits, src(i), *conn_count, t0);
        if matches!(decision, SourceAdmission::Admit) {
            inner.source_handshake_done(src(i), t0);
            *conn_count += 1;
        }
        decision
    };

    // 64 sources × 4 live connections = the soft cap exactly.
    let mut conn_count = 0usize;
    for i in 0..64u16 {
        for _ in 0..4 {
            assert!(matches!(
                admit(&mut inner, i, &mut conn_count),
                SourceAdmission::Admit
            ));
        }
    }
    assert_eq!(conn_count, MAX_LIVE_CONNECTIONS);
    assert_eq!(inner.active_conn_sources, 64);

    // A 4-holder at the cap is AT its fair share (ceil(256/64) = 4)
    // even though its own cap allows 8: the global side refuses.
    assert!(matches!(
        admit(&mut inner, 0, &mut conn_count),
        SourceAdmission::OverGlobal
    ));
    assert_eq!(conn_count, MAX_LIVE_CONNECTIONS, "nothing was reserved");

    // A newcomer (live 0 < share) is admitted past the soft cap —
    // one connection at a time up to the hard cap.
    let mut newcomers = 0usize;
    let mut next = 100u16;
    while conn_count < MAX_LIVE_CONNECTIONS_HARD {
        assert!(
            matches!(
                admit(&mut inner, next, &mut conn_count),
                SourceAdmission::Admit
            ),
            "below-share source {next} must be admitted at conn_count {conn_count}"
        );
        newcomers += 1;
        next += 1;
    }
    assert_eq!(conn_count, MAX_LIVE_CONNECTIONS_HARD);
    assert_eq!(
        newcomers,
        MAX_LIVE_CONNECTIONS_HARD - MAX_LIVE_CONNECTIONS,
        "the overshoot is exactly the hard-minus-soft margin"
    );

    // At the hard cap even a brand-new source is refused.
    assert!(matches!(
        admit(&mut inner, next, &mut conn_count),
        SourceAdmission::OverGlobal
    ));

    // Releases feed the divisor: after one newcomer fully
    // disconnects, active_conn_sources drops with it.
    let retired = 100u16;
    inner.source_release(src(retired), false, t0);
    assert_eq!(inner.active_conn_sources, 64 + newcomers - 1);
}

/// CR-P2-10 unit level, connections: the IPv6 /56 and /48
/// aggregates. 256 /64s share one /56 — their CONCURRENT aggregate
/// is capped at 4× the per-/64 cap (32), not 256×; the /48
/// aggregate at 16× (128); other prefixes are unaffected; IPv4 has
/// no parent hierarchy at all.
#[test]
fn prefix_aggregate_connection_caps_hold_at_56_and_48() {
    let limits = Limits::default();
    let t0 = Instant::now();
    // One /64 per low byte of the 4th group: all inside 2001:db8:aaaa:bb00::/56.
    let src56 = |sub: u16| {
        IpAddr::V6(Ipv6Addr::new(
            0x2001,
            0x0db8,
            0xaaaa,
            0xbb00 + sub,
            0,
            0,
            0,
            1,
        ))
    };
    // One /56 per high byte of the 4th group (bb..be), all inside 2001:db8:aaaa::/48.
    let src48 = |block: u16, sub: u16| {
        IpAddr::V6(Ipv6Addr::new(
            0x2001,
            0x0db8,
            0xaaaa,
            0xbb00 + block * 0x100 + sub,
            0,
            0,
            0,
            1,
        ))
    };
    let admit = |inner: &mut Inner, src: IpAddr| {
        let decision = inner.source_admit(&limits, src, 0, t0);
        if matches!(decision, SourceAdmission::Admit) {
            inner.source_handshake_done(src, t0);
        }
        decision
    };

    // /56: 32 held connections inside the delegation are fine, the
    // 33rd /64 (each holding only 1 of its own 8) is dropped at
    // accept by the AGGREGATE; a different /56 is unaffected.
    let mut inner56 = Inner::default();
    for sub in 0..limits.max_conns_per_56 as u16 {
        assert!(
            matches!(admit(&mut inner56, src56(sub)), SourceAdmission::Admit),
            "sub {sub} inside the /56 aggregate"
        );
    }
    assert!(
        matches!(
            admit(&mut inner56, src56(limits.max_conns_per_56 as u16)),
            SourceAdmission::OverSourceCaps
        ),
        "the /56 aggregate (4x per-source) fires while every /64 holds 1"
    );
    assert!(matches!(
        admit(&mut inner56, src48(1, 0)),
        SourceAdmission::Admit
    ));

    // /48 (fresh accounting): four /56s x 32 = the 128 aggregate;
    // the 129th — in a FIFTH /56 with room everywhere below — is
    // the /48's to refuse; a different /48 starts fresh.
    let mut inner48 = Inner::default();
    for block in 0..4u16 {
        for sub in 0..limits.max_conns_per_56 as u16 {
            assert!(matches!(
                admit(&mut inner48, src48(block, sub)),
                SourceAdmission::Admit
            ));
        }
    }
    assert!(matches!(
        admit(&mut inner48, src48(4, 0)),
        SourceAdmission::OverSourceCaps
    ));
    let other48 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0xaaab, 1, 0, 0, 0, 1));
    assert!(matches!(
        admit(&mut inner48, other48),
        SourceAdmission::Admit
    ));

    // IPv4: no parents — a plain /32 crowd never touches the tables.
    let mut v4_inner = Inner::default();
    for i in 0..64u16 {
        assert!(matches!(
            admit(&mut v4_inner, IpAddr::V4(Ipv4Addr::new(192, 0, 2, i as u8))),
            SourceAdmission::Admit
        ));
    }
    assert_eq!(v4_inner.prefixes56.len(), 0);
    assert_eq!(v4_inner.prefixes48.len(), 0);
}

/// CR-P2-10 unit level, redeems: the /56 aggregate admits exactly
/// 4× the per-/64 budget spread over the delegation's /64s, the
/// /48 aggregate 16×, and neither touches foreign prefixes.
#[test]
fn prefix_aggregate_redeem_caps_hold_at_56_and_48() {
    let limits = Limits::default();
    let t0 = Instant::now();
    // Four /64s x the full 30/min per-/64 budget = the /56 share.
    let mut inner = Inner::default();
    let identity = |i: u8| DeviceId::from([i; 32]);
    let mut attempt = 0u8;
    for sub in 0..4u16 {
        let src = IpAddr::V6(Ipv6Addr::new(
            0x2001,
            0x0db8,
            0xaaaa,
            0xbb00 + sub,
            0,
            0,
            0,
            1,
        ));
        for _ in 0..limits.redeem_per_source_per_min {
            let (ok, _) = inner.redeem_allows(&limits, src, &identity(attempt), t0);
            assert!(ok, "attempt {attempt} inside the /56 share");
            attempt += 1;
        }
    }
    assert_eq!(attempt, limits.redeem_per_56_per_min as u8);
    // The 121st attempt in the same /56 — from a FRESH /64 with a
    // fresh identity — is the aggregate's to refuse.
    let fresh64 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0xaaaa, 0xbb10, 0, 0, 0, 1));
    let (ok, retry) = inner.redeem_allows(&limits, fresh64, &identity(200), t0);
    assert!(!ok, "the /56 aggregate (4x per-source) must fire");
    assert!(retry > 0, "the limiter suggests a retry delay");
    // A different /56 redeems as if nothing happened.
    let other56 = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0xaaab, 1, 0, 0, 0, 1));
    let (ok, _) = inner.redeem_allows(&limits, other56, &identity(201), t0);
    assert!(ok, "a foreign /56 is untouched");

    // /48 (fresh accounting): sixteen /64s x 30 = the 480 share,
    // spread over four /56s (each also capped at its own 120).
    let mut inner = Inner::default();
    let next_identity = |attempt: &mut u16| {
        let mut identity = [0u8; 32];
        identity[..2].copy_from_slice(&attempt.to_be_bytes());
        *attempt += 1;
        identity
    };
    let mut attempt = 0u16;
    'outer: for block in 0..4u16 {
        for sub in 0..4u16 {
            let src = IpAddr::V6(Ipv6Addr::new(
                0x2001,
                0x0db8,
                0xaaaa,
                0xbb00 + block * 0x100 + sub,
                0,
                0,
                0,
                1,
            ));
            for _ in 0..limits.redeem_per_source_per_min {
                let identity = next_identity(&mut attempt);
                let (ok, _) = inner.redeem_allows(&limits, src, &identity, t0);
                assert!(ok, "attempt {} inside the /48 share", attempt - 1);
                if attempt >= limits.redeem_per_48_per_min as u16 {
                    break 'outer;
                }
            }
        }
    }
    assert_eq!(attempt, limits.redeem_per_48_per_min as u16);
    let fresh = IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0xaaaa, 0xbe10, 0, 0, 0, 1));
    let (ok, _) = inner.redeem_allows(&limits, fresh, &[0xFF; 32], t0);
    assert!(!ok, "the /48 aggregate (16x per-source) must fire");
}

/// CR-P2-10 unit level: at [`SOURCE_TABLE_MAX_ENTRIES`] the table
/// evicts over the recency index in O(log n) — the victim is the
/// least-recently-ACTIVE idle entry, never a live one, never a
/// fresher one; idle-expired entries are dropped in preference; and
/// the all-live table refuses instead of overrunning memory.
#[test]
fn source_table_evicts_the_least_recently_active_entry_at_the_cap() {
    let mut table: IdleTable<IpAddr, SourceEntry> = IdleTable::default();
    let t0 = Instant::now();
    let key = |i: usize| IpAddr::V4(Ipv4Addr::new(10, (i >> 16) as u8, (i >> 8) as u8, i as u8));
    // Fill to the cap with idle entries stamped in insertion order
    // (i is also the recency rank).
    for i in 0..SOURCE_TABLE_MAX_ENTRIES {
        let stamp = t0 + Duration::from_nanos(i as u64 * 1_000);
        table
            .touch_entry(key(i), stamp, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(1, REDEEM_WINDOW),
                last_seen: stamp,
            })
            .last_seen = stamp;
    }
    assert_eq!(table.len(), SOURCE_TABLE_MAX_ENTRIES);
    // The stalest entry holds a LIVE counter: never the victim.
    table.get_mut(&key(0)).unwrap().live = 1;

    let now = t0 + Duration::from_secs(1);
    assert!(table.make_room(now), "an idle victim exists");
    assert_eq!(table.len(), SOURCE_TABLE_MAX_ENTRIES - 1);
    assert!(
        table.get(&key(0)).is_some(),
        "the live entry survives despite being the stalest"
    );
    assert!(
        table.get(&key(1)).is_none(),
        "the least-recently-active IDLE entry is the victim"
    );
    assert!(table.get(&key(2)).is_some(), "fresher entries survive");
    assert!(
        table.get(&key(SOURCE_TABLE_MAX_ENTRIES - 1)).is_some(),
        "the newest entry survives"
    );

    // Refill to the cap, then the next eviction walks further down
    // the same order (key 2 — the now-stalest idle entry).
    table.touch_entry(key(700_000), now, || SourceEntry {
        live: 0,
        handshaking: 0,
        redeem: SlidingWindow::new(1, REDEEM_WINDOW),
        last_seen: now,
    });
    assert_eq!(table.len(), SOURCE_TABLE_MAX_ENTRIES);
    assert!(table.make_room(now));
    assert_eq!(table.len(), SOURCE_TABLE_MAX_ENTRIES - 1);
    assert!(table.get(&key(2)).is_none());
    assert!(table.get(&key(0)).is_some(), "still never the live entry");
    assert!(
        table.get(&key(700_000)).is_some(),
        "the filler, stamped now, is newer than every original entry"
    );

    // Idle-EXPIRED entries are dropped in preference to evicting a
    // fresh one: re-stamp every remaining idle entry fresh except
    // key 3, which is left a full TTL (+ margin) in the past, and
    // refill back to the cap so make_room actually walks.
    let later = now + SOURCE_TABLE_IDLE_TTL + Duration::from_secs(2);
    for i in 4..SOURCE_TABLE_MAX_ENTRIES {
        table.note_activity(&key(i), later);
    }
    table.note_activity(&key(700_000), later);
    table.touch_entry(key(800_000), later, || SourceEntry {
        live: 0,
        handshaking: 0,
        redeem: SlidingWindow::new(1, REDEEM_WINDOW),
        last_seen: later,
    });
    assert_eq!(table.len(), SOURCE_TABLE_MAX_ENTRIES);
    assert!(table.make_room(later));
    assert!(
        table.get(&key(3)).is_none(),
        "the idle-expired entry is dropped, not the freshest idle one"
    );
    assert!(
        table.get(&key(4)).is_some(),
        "fresh idle entries are not evicted while expired ones exist"
    );

    // All-live table at the cap: refuse rather than overrun.
    let mut all_live: IdleTable<IpAddr, SourceEntry> = IdleTable::default();
    for i in 0..SOURCE_TABLE_MAX_ENTRIES {
        let stamp = t0 + Duration::from_nanos(i as u64 * 1_000);
        all_live
            .touch_entry(key(i), stamp, || SourceEntry {
                live: 1,
                handshaking: 0,
                redeem: SlidingWindow::new(1, REDEEM_WINDOW),
                last_seen: stamp,
            })
            .last_seen = stamp;
    }
    assert!(!all_live.make_room(now), "no evictable entry: over budget");

    // And through Inner: a redeem from one more source finds room
    // via the same path (the evicted entry is the oldest idle one,
    // never the live one).
    let limits = Limits::default();
    let mut inner = Inner::default();
    for i in 0..SOURCE_TABLE_MAX_ENTRIES {
        let stamp = t0 + Duration::from_nanos(i as u64 * 1_000);
        inner
            .sources
            .touch_entry(key(i), stamp, || SourceEntry {
                live: 0,
                handshaking: 0,
                redeem: SlidingWindow::new(limits.redeem_per_source_per_min, REDEEM_WINDOW),
                last_seen: stamp,
            })
            .last_seen = stamp;
    }
    inner.sources.get_mut(&key(0)).unwrap().live = 1;
    let (ok, _) = inner.redeem_allows(&limits, key(900_000), &[9; 32], now);
    assert!(ok, "the table made room for one more source");
    assert!(inner.sources.get(&key(0)).is_some());
    assert!(inner.sources.get(&key(1)).is_none());
    assert!(inner.sources.get(&key(900_000)).is_some());
}

/// CR-P2-10 acceptance (a): 20 IPv4 sources spend their full
/// 30/min budgets — exactly the 600/min global breaker — and the
/// 21st source's legitimate redeem STILL succeeds. Before the
/// fair-share breaker the full window meant RATE_LIMITED for every
/// later source (the CR-P2-10 finding).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn fair_share_breaker_admits_the_twenty_first_source() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let per_source = crate::server_v2::config::REDEEM_PER_SOURCE_PER_MIN as usize;
    let sources = crate::server_v2::config::REDEEM_GLOBAL_BREAKER_PER_MIN as usize / per_source;
    assert_eq!(sources, 20, "the scenario: 20 x 30 = exactly 600/min");

    // Every source spends its exact per-source budget: three
    // connections x the 10/min per-connection window, each
    // connection its own identity (every CR-P1-03 layer stays under
    // its cap, so the only near-exhaustion is the global breaker).
    let mut flood = Vec::new();
    let mut admitted = 0usize;
    let mut lookup_counter = 0u32;
    for host in 1..=sources as u8 {
        let source = format!("127.0.0.{host}").parse::<IpAddr>().unwrap();
        for round in 0..3u8 {
            let mut client = TestClient::connect_at(
                Some(source),
                server.addr,
                &registry_id,
                DeviceKeyPair::generate(),
                &format!("flood-{host}-{round}"),
            )
            .await;
            for _ in 0..(per_source / 3) {
                let mut lookup = [0u8; 32];
                lookup[..4].copy_from_slice(&lookup_counter.to_be_bytes());
                lookup_counter += 1;
                let redeem = signed_redeem(
                    &client.key,
                    &registry_id,
                    lookup,
                    random_id16(),
                    now_ms() + 8_000,
                );
                client.send(&CtrlMessage::InviteRedeem(redeem)).await;
                match client.recv().await {
                    CtrlMessage::InviteRedeemPending { .. } => admitted += 1,
                    other => panic!("flood redeem must be pending, got {other:?}"),
                }
            }
            flood.push(client);
        }
    }
    assert_eq!(
        admitted,
        crate::server_v2::config::REDEEM_GLOBAL_BREAKER_PER_MIN as usize,
        "the breaker window is exactly full (600 admitted)"
    );

    // The 21st source's legitimate redeem: pending, NOT the global
    // RATE_LIMITED this saturation used to mean for everyone.
    let mut newcomer = TestClient::connect_at(
        Some("127.0.0.21".parse().unwrap()),
        server.addr,
        &registry_id,
        DeviceKeyPair::generate(),
        "newcomer",
    )
    .await;
    let redeem = signed_redeem(
        &newcomer.key,
        &registry_id,
        [0x21; 32],
        random_id16(),
        now_ms() + 8_000,
    );
    newcomer.send(&CtrlMessage::InviteRedeem(redeem)).await;
    match newcomer.recv().await {
        CtrlMessage::InviteRedeemPending { .. } => {}
        CtrlMessage::PairError { code, .. } => {
            panic!("the 21st source's legit redeem must pass the saturated breaker, got {code}")
        }
        other => panic!("unexpected newcomer redeem answer: {other:?}"),
    }
    drop(flood);
    drop(newcomer);
    server.stop().await;
}

/// CR-P2-10 acceptance, end to end: 32 sources x 8 held connections
/// fill the 256 soft cap — the connection-side twin of the redeem
/// scenario — and a 33rd source still completes the full TLS +
/// Hello/Welcome exchange (admitted past the soft cap under its
/// fair share; the fat sources are the ones refused).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn fair_share_serving_cap_admits_a_newcomer_past_the_soft_cap() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let per_source = crate::server_v2::config::MAX_CONNS_PER_SOURCE;
    let sources = MAX_LIVE_CONNECTIONS / per_source;
    assert_eq!(sources, 32, "the scenario: 32 x 8 = exactly 256");

    // Hold the soft cap's worth of live, authenticated connections.
    let mut held = Vec::new();
    for host in 1..=sources as u8 {
        let source = format!("127.0.0.{host}").parse::<IpAddr>().unwrap();
        for _ in 0..per_source {
            held.push(
                TestClient::connect_at(
                    Some(source),
                    server.addr,
                    &registry_id,
                    DeviceKeyPair::generate(),
                    &format!("holder-{host}"),
                )
                .await,
            );
        }
    }

    // The 33rd source's full exchange completes within the normal
    // envelope — before CR-P2-10 it was refused with ServerError.
    let started = Instant::now();
    let newcomer = TestClient::connect_at(
        Some("127.0.0.33".parse().unwrap()),
        server.addr,
        &registry_id,
        DeviceKeyPair::generate(),
        "newcomer",
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the newcomer completes despite the saturated soft cap (took {:?})",
        started.elapsed()
    );
    drop(newcomer);
    drop(held);
    server.stop().await;
}

/// R36-P2-06: the per-owner hourly claim budget is exactly the
/// contract's 30 — the first 30 claims succeed, the 31st NEW claim
/// is `RATE_LIMITED`, and neither a reconnect (fresh control
/// connection, same identity) nor revoking codes resets the hour
/// window: the counter is persisted per owner, not held per
/// connection, and revoked rows still count.
#[tokio::test]
async fn hourly_claim_budget_boundary_is_thirty_and_survives_reconnect_and_revoke() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let key = DeviceKeyPair::generate();
    let mut owner = TestClient::connect(server.addr, &registry_id, key.clone(), "o").await;

    // The first 30 claims of the hour succeed (distinct requests and
    // lookups under one owner).
    let mut first_code = None;
    for index in 0..crate::server_v2::config::INVITE_CLAIMS_PER_HOUR {
        let mut lookup = [0u8; 32];
        lookup[..8].copy_from_slice(&u64::from(index).to_be_bytes());
        owner
            .send(&CtrlMessage::InviteClaim {
                request_id: random_id16(),
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .await;
        match owner.recv().await {
            CtrlMessage::InviteClaimed { code_id, .. } => {
                if index == 0 {
                    first_code = Some(code_id);
                }
            }
            other => {
                panic!("claim {index} inside the 30/hour budget must succeed, got {other:?}")
            }
        }
    }
    // The 31st NEW claim of the hour: RATE_LIMITED.
    owner
        .send(&CtrlMessage::InviteClaim {
            request_id: random_id16(),
            lookup_digest: [0xE1; 32],
            ttl_secs: 0,
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, RATE_LIMITED),
        other => panic!("expected RATE_LIMITED on the 31st claim, got {other:?}"),
    }

    // A reconnect (fresh control connection, same owner identity)
    // does not reset the hour window.
    drop(owner);
    let mut owner2 = TestClient::connect(server.addr, &registry_id, key.clone(), "o").await;
    owner2
        .send(&CtrlMessage::InviteClaim {
            request_id: random_id16(),
            lookup_digest: [0xE2; 32],
            ttl_secs: 0,
        })
        .await;
    match owner2.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, RATE_LIMITED),
        other => panic!("reconnect must not reset the hourly window, got {other:?}"),
    }

    // Revoking a code does not reset the window either: the count
    // includes revoked rows.
    owner2
        .send(&CtrlMessage::InviteRevoke {
            request_id: random_id16(),
            code_id: first_code.expect("the first claim answered"),
            expected_revision: 1,
        })
        .await;
    match owner2.recv().await {
        CtrlMessage::InviteRevoked { .. } => {}
        other => panic!("expected InviteRevoked, got {other:?}"),
    }
    owner2
        .send(&CtrlMessage::InviteClaim {
            request_id: random_id16(),
            lookup_digest: [0xE3; 32],
            ttl_secs: 0,
        })
        .await;
    match owner2.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, RATE_LIMITED),
        other => panic!("revoke must not reset the hourly window, got {other:?}"),
    }
    server.stop().await;
}

/// Shutdown aborts every live connection task and returns promptly.
#[tokio::test]
async fn shutdown_aborts_live_connection_tasks_and_returns() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut client =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "c").await;
    let _silent = tls::connect_pinned(server.addr, &registry_id)
        .await
        .unwrap();

    let _ = server.shutdown.send(true);
    let joined = tokio::time::timeout(Duration::from_secs(5), server.task)
        .await
        .expect("serve_v2 returns after shutdown");
    assert!(joined.unwrap().is_ok());

    // The aborted handler closed the client's socket: EOF or an I/O
    // error promptly, and never another frame.
    let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut client.stream))
        .await
        .expect("client socket closes promptly");
    assert!(
        frame.as_ref().map(|maybe| maybe.is_none()).unwrap_or(true),
        "no frame may arrive after shutdown (got {frame:?})"
    );
}

/// The reader's first-frame budget: a client that completed the
/// connection but never speaks loses its task at the budget (real
/// budgets are [`FIRST_FRAME_DEADLINE`]; the test injects a tiny one).
#[tokio::test]
async fn reader_enforces_the_first_frame_deadline_on_silence() {
    let (client, server_end) = tokio::io::duplex(64);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
    let (_close_tx, close) = watch::channel(false);
    let reader = tokio::spawn(reader_loop(
        server_end,
        tx,
        close,
        Duration::from_millis(75),
        Duration::from_secs(30),
    ));
    // The client half stays open but never speaks: no EOF, no bytes —
    // only the first-frame budget can end this reader.
    let _silent_peer = client;
    let started = Instant::now();
    assert!(
        rx.recv().await.is_none(),
        "reader exits at the first-frame deadline"
    );
    reader.await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the budget ended the reader, not the peer"
    );
}

/// The reader forwards frames while they arrive inside the budget and
/// exits once silence outlives the steady budget.
#[tokio::test]
async fn reader_forwards_frames_then_dies_on_silence() {
    let (mut client, server_end) = tokio::io::duplex(64);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
    let (_close_tx, close) = watch::channel(false);
    let reader = tokio::spawn(reader_loop(
        server_end,
        tx,
        close,
        Duration::from_secs(30),
        Duration::from_millis(75),
    ));

    // One legal client frame passes through the reader untouched.
    write_message(
        &mut client,
        &CtrlMessage::Ping { nonce: [7; 16] },
        Role::Client,
    )
    .await
    .unwrap();
    let bytes = rx.recv().await.expect("frame forwarded");
    assert_eq!(
        CtrlFrame::decode_as(&bytes, Role::Server).unwrap().message,
        CtrlMessage::Ping { nonce: [7; 16] }
    );

    // Then silence: the steady read budget ends the reader.
    assert!(
        rx.recv().await.is_none(),
        "reader exits after the steady deadline"
    );
    reader.await.unwrap();
}

/// The writer's write budget: a write that can never drain (1-byte
/// duplex buffer, nobody reading) reports the dead connection to the
/// handler instead of parking forever.
#[tokio::test]
async fn writer_stall_tears_the_connection_down() {
    let (unread, server_end) = tokio::io::duplex(1);
    let (tx, rx) = mpsc::channel::<CtrlFrame>(4);
    let (_close_tx, close) = watch::channel(false);
    let (dead_tx, mut dead_rx) = mpsc::channel::<()>(1);
    let writer = tokio::spawn(writer_loop(
        server_end,
        rx,
        close,
        dead_tx,
        Duration::from_millis(75),
    ));

    tx.send(CtrlFrame::new(CtrlMessage::Pong { nonce: [9; 16] }))
        .await
        .unwrap();
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), dead_rx.recv())
        .await
        .expect("a stalled write reports the dead connection")
        .expect("the dead signal carries no payload");
    writer.await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the write budget fired, not the test timeout"
    );
    drop(unread);
}

// ---- wave-28 endpoint exchange (frame extension e2e) --------------------

impl TestClient {
    async fn send_frame(&mut self, frame: &CtrlFrame) {
        crate::server_v2::framing::write_frame(&mut self.stream, frame, Role::Client)
            .await
            .unwrap();
    }

    async fn recv_frame(&mut self) -> CtrlFrame {
        let deadline = Duration::from_secs(10);
        match tokio::time::timeout(deadline, async {
            let bytes = read_frame(&mut self.stream).await.unwrap().expect("frame");
            CtrlFrame::decode_as(&bytes, Role::Client).unwrap()
        })
        .await
        {
            Ok(frame) => frame,
            Err(_) => self.missed_deadline(deadline),
        }
    }
}

/// Drive both participants from a live pending redeem to the
/// authoritative commit: owner PairChallenge, redeemer PairProof,
/// both PairPrepared. Returns the transcript bytes and digest.
#[allow(clippy::too_many_arguments)]
async fn drive_to_commit(
    owner: &mut TestClient,
    redeemer: &mut TestClient,
    registry_id: &RegistryId,
    code_id: CodeId,
    lookup: Digest32,
    pairing_id: PairingId,
    request_id: RequestId,
    redeemer_nonce: [u8; 32],
    deadline: u64,
) -> ([u8; 256], Digest32) {
    let transcript = PairTranscript {
        registry_id: *registry_id,
        code_id,
        code_revision: 1,
        pairing_id,
        request_id,
        lookup_digest: lookup,
        owner_device_id: owner.key.public_key(),
        redeemer_device_id: redeemer.key.public_key(),
        owner_nonce: [0x0D; 32],
        redeemer_nonce,
        deadline_ms: deadline,
    };
    let bytes = transcript.to_bytes();
    let owner_proof = PairProof {
        mac: [0x11; 32],
        signature: owner.key.sign(&hello2::proof_signed_message(
            OWNER_PROOF_DOMAIN,
            &bytes,
            &[0x11; 32],
        )),
    };
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: bytes,
            owner_proof: owner_proof.clone(),
        })
        .await;
    match redeemer.recv().await {
        CtrlMessage::PairChallenge {
            transcript,
            owner_proof: relayed,
        } => {
            assert_eq!(transcript, bytes);
            assert_eq!(relayed, owner_proof);
        }
        other => panic!("expected relayed PairChallenge, got {other:?}"),
    }
    let digest = hello2::transcript_digest(&bytes);
    let redeemer_proof = PairProof {
        mac: [0x22; 32],
        signature: redeemer.key.sign(&hello2::proof_signed_message(
            REDEEMER_PROOF_DOMAIN,
            &bytes,
            &[0x22; 32],
        )),
    };
    redeemer
        .send(&CtrlMessage::PairProof {
            pairing_id,
            transcript_digest: digest,
            redeemer_proof: redeemer_proof.clone(),
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairProof {
            transcript_digest,
            redeemer_proof: relayed,
            ..
        } => {
            assert_eq!(transcript_digest, digest);
            assert_eq!(relayed, redeemer_proof);
        }
        other => panic!("expected relayed PairProof, got {other:?}"),
    }
    owner
        .send(&CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest: digest,
            signature: owner
                .key
                .sign(&hello2::prepared_message(&pairing_id, &digest)),
        })
        .await;
    redeemer
        .send(&CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest: digest,
            signature: redeemer
                .key
                .sign(&hello2::prepared_message(&pairing_id, &digest)),
        })
        .await;
    (bytes, digest)
}

/// Wave-28 end to end: the owner's connection settings claimed with
/// the code and the redeemer's settings sent with the redeem both
/// reach the opposite side, byte-identical, riding the existing
/// offer/commit frames (no extra round trips).
#[tokio::test]
async fn endpoint_blobs_reach_both_sides_byte_identical() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut owner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
    let mut redeemer =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

    let owner_blob = EndpointBlob(vec![0xA1; 217]);
    let redeemer_blob = EndpointBlob(vec![0xB2; 333]);
    let lookup: Digest32 = [0x7C; 32];

    // Claim with the owner endpoint (no cap: legacy multi-pairing).
    let claim_request = random_id16();
    owner
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: claim_request,
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .with_endpoint(owner_blob.clone()),
        )
        .await;
    let code_id = match owner.recv_frame().await.message {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };

    // Redeem with the redeemer endpoint.
    let deadline = now_ms() + 8_000;
    let redeem = signed_redeem(&redeemer.key, &registry_id, lookup, random_id16(), deadline);
    let request_id = redeem.request_id;
    let redeemer_nonce = redeem.redeemer_nonce;
    redeemer
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteRedeem(redeem)).with_endpoint(redeemer_blob.clone()),
        )
        .await;
    let pairing_id = match redeemer.recv_frame().await.message {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    };

    // The owner's offer carries the redeemer endpoint verbatim.
    let offered = owner.recv_frame().await;
    match &offered.message {
        CtrlMessage::InviteRedeemOffered {
            code_id: offered_code,
            pairing_id: offered_pairing,
            ..
        } => {
            assert_eq!(*offered_code, code_id);
            assert_eq!(*offered_pairing, pairing_id);
        }
        other => panic!("expected InviteRedeemOffered, got {other:?}"),
    }
    assert_eq!(
        offered.endpoint,
        Some(redeemer_blob.clone()),
        "redeemer endpoint must ride the offer byte-identical"
    );

    // Complete the pairing; the redeemer's PairCommit carries the
    // owner endpoint verbatim.
    drive_to_commit(
        &mut owner,
        &mut redeemer,
        &registry_id,
        code_id,
        lookup,
        pairing_id,
        request_id,
        redeemer_nonce,
        deadline,
    )
    .await;
    let commit = redeemer.recv_frame().await;
    match &commit.message {
        CtrlMessage::PairCommit {
            commit_revision, ..
        } => assert_eq!(*commit_revision, 1),
        other => panic!("expected PairCommit, got {other:?}"),
    }
    assert_eq!(
        commit.endpoint,
        Some(owner_blob.clone()),
        "owner endpoint must ride the commit byte-identical"
    );
    // The owner's own commit copy also carries its blob (same fixed
    // frame to both participants; no third party is involved).
    let owner_commit = owner.recv_frame().await;
    assert!(matches!(
        owner_commit.message,
        CtrlMessage::PairCommit { .. }
    ));
    assert_eq!(owner_commit.endpoint, Some(owner_blob.clone()));
    server.stop().await;
}

/// `max_redeems=1`: after one committed pairing the code is dead for
/// every other redeemer — the second, different redeemer is rejected
/// immediately with the dedicated `REDEEM_EXHAUSTED` code (no
/// deferred-deadline ghost semantics).
#[tokio::test]
async fn single_redeem_cap_rejects_the_second_redeemer() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut owner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
    let mut first =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "a").await;

    let lookup: Digest32 = [0x5E; 32];
    let claim_request = random_id16();
    owner
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: claim_request,
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .with_endpoint(EndpointBlob(vec![0xC3; 64]))
            .with_max_redeems(1),
        )
        .await;
    let code_id = match owner.recv_frame().await.message {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };

    // First redeemer completes through the commit.
    let deadline = now_ms() + 8_000;
    let redeem = signed_redeem(&first.key, &registry_id, lookup, random_id16(), deadline);
    let request_id = redeem.request_id;
    let redeemer_nonce = redeem.redeemer_nonce;
    first.send(&CtrlMessage::InviteRedeem(redeem)).await;
    let pairing_id = match first.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    };
    let _offer = owner.recv_frame().await; // offer push
    drive_to_commit(
        &mut owner,
        &mut first,
        &registry_id,
        code_id,
        lookup,
        pairing_id,
        request_id,
        redeemer_nonce,
        deadline,
    )
    .await;
    let _ = first.recv_frame().await; // PairCommit to the redeemer
    let _ = owner.recv_frame().await; // PairCommit to the owner

    // A second, different redeemer: immediate, explicit rejection.
    let mut second =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "b").await;
    let second_deadline = now_ms() + 8_000;
    let second_redeem = signed_redeem(
        &second.key,
        &registry_id,
        lookup,
        random_id16(),
        second_deadline,
    );
    let second_request = second_redeem.request_id;
    second.send(&CtrlMessage::InviteRedeem(second_redeem)).await;
    match second.recv().await {
        CtrlMessage::PairError {
            request_id,
            scope,
            object_id,
            code,
            ..
        } => {
            assert_eq!(request_id, second_request);
            assert_eq!(code, REDEEM_EXHAUSTED);
            assert_eq!(scope, ErrorScope::Code);
            assert_eq!(object_id, code_id);
        }
        other => panic!("expected REDEEM_EXHAUSTED PairError, got {other:?}"),
    }
    server.stop().await;
}

/// The same request id replaying a completed capped redeem still gets
/// the original pending answer (idempotency unchanged by the cap).
#[tokio::test]
async fn redeem_replay_semantics_survive_the_cap() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut owner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
    let mut redeemer =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "r").await;

    let lookup: Digest32 = [0x6F; 32];
    owner
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: random_id16(),
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .with_max_redeems(1),
        )
        .await;
    let code_id = match owner.recv_frame().await.message {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };
    let deadline = now_ms() + 8_000;
    let redeem = signed_redeem(&redeemer.key, &registry_id, lookup, random_id16(), deadline);
    let request_id = redeem.request_id;
    let redeemer_nonce = redeem.redeemer_nonce;
    redeemer
        .send(&CtrlMessage::InviteRedeem(redeem.clone()))
        .await;
    let pairing_id = match redeemer.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("expected InviteRedeemPending, got {other:?}"),
    };
    let _offer = owner.recv_frame().await;
    drive_to_commit(
        &mut owner,
        &mut redeemer,
        &registry_id,
        code_id,
        lookup,
        pairing_id,
        request_id,
        redeemer_nonce,
        deadline,
    )
    .await;
    let _ = redeemer.recv_frame().await; // PairCommit

    // Same request id retry after the cap is in force: the original
    // result replays, not a fresh REDEEM_EXHAUSTED.
    redeemer.send(&CtrlMessage::InviteRedeem(redeem)).await;
    match redeemer.recv().await {
        CtrlMessage::InviteRedeemPending {
            pairing_id: replayed,
            ..
        } => assert_eq!(replayed, pairing_id),
        other => panic!("expected replayed InviteRedeemPending, got {other:?}"),
    }
    server.stop().await;
}

/// P1-03: `max_redeems=1` binds at the commit linearization point,
/// not only at redeem begin. Two redeemers both reach pending (pending
/// consumes nothing), then race to Commit — exactly one pairing
/// commits; the loser's Commit fails with the explicit
/// `REDEEM_EXHAUSTED` error to both participants, the code refuses
/// every new redeemer, and the loser's same-`request_id` retry still
/// replays its original pending answer (idempotency unchanged).
#[tokio::test]
async fn two_pending_redeems_racing_commit_yield_exactly_one() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut owner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
    let mut first =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "a").await;
    let mut second =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "b").await;

    let lookup: Digest32 = [0x7E; 32];
    let claim_request = random_id16();
    owner
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: claim_request,
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .with_max_redeems(1),
        )
        .await;
    let code_id = match owner.recv_frame().await.message {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };

    // Both redeemers are admitted: pending never consumes the cap.
    let deadline = now_ms() + 8_000;
    let redeem_a = signed_redeem(&first.key, &registry_id, lookup, random_id16(), deadline);
    let request_a = redeem_a.request_id;
    let nonce_a = redeem_a.redeemer_nonce;
    first
        .send(&CtrlMessage::InviteRedeem(redeem_a.clone()))
        .await;
    let pairing_a = match first.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("first redeem must begin, got {other:?}"),
    };
    let _offer_a = owner.recv_frame().await; // push to the owner

    let redeem_b = signed_redeem(&second.key, &registry_id, lookup, random_id16(), deadline);
    second
        .send(&CtrlMessage::InviteRedeem(redeem_b.clone()))
        .await;
    let pairing_b = match second.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("second redeem must begin, got {other:?}"),
    };
    assert_ne!(pairing_a, pairing_b);
    let _offer_b = owner.recv_frame().await;

    // Racer A completes through the authoritative commit.
    drive_to_commit(
        &mut owner,
        &mut first,
        &registry_id,
        code_id,
        lookup,
        pairing_a,
        request_a,
        nonce_a,
        deadline,
    )
    .await;
    let _ = first.recv_frame().await; // PairCommit to the redeemer
    let _ = owner.recv_frame().await; // PairCommit to the owner

    // Racer B's Commit is refused inside the same write transaction:
    // both B and the owner get the explicit, code-scoped failure.
    drive_to_commit(
        &mut owner,
        &mut second,
        &registry_id,
        code_id,
        lookup,
        pairing_b,
        redeem_b.request_id,
        redeem_b.redeemer_nonce,
        deadline,
    )
    .await;
    for (who, frame) in [
        ("redeemer", second.recv().await),
        ("owner", owner.recv().await),
    ] {
        match frame {
            CtrlMessage::PairError {
                scope,
                object_id,
                code,
                ..
            } => {
                assert_eq!(code, REDEEM_EXHAUSTED, "{who}");
                // Wave-30 (R29-P2-01): pairing-scoped and addressed to
                // THIS pairing so the production client dispatch and
                // orchestrator wait_frame actually deliver the loss.
                assert_eq!(scope, ErrorScope::Pairing, "{who}");
                assert_eq!(object_id, pairing_b, "{who}");
            }
            other => panic!("{who}: expected REDEEM_EXHAUSTED, got {other:?}"),
        }
    }

    // The exhausted code refuses every new redeemer immediately.
    let mut third =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "c").await;
    let third_redeem = signed_redeem(
        &third.key,
        &registry_id,
        lookup,
        random_id16(),
        now_ms() + 8_000,
    );
    third.send(&CtrlMessage::InviteRedeem(third_redeem)).await;
    match third.recv().await {
        CtrlMessage::PairError { code, .. } => assert_eq!(code, REDEEM_EXHAUSTED),
        other => panic!("expected REDEEM_EXHAUSTED, got {other:?}"),
    }

    // The losing redeemer retrying its own request id still replays
    // the original pending answer — the cap never breaks redeem
    // idempotency.
    second.send(&CtrlMessage::InviteRedeem(redeem_b)).await;
    match second.recv().await {
        CtrlMessage::InviteRedeemPending {
            pairing_id: replayed,
            ..
        } => assert_eq!(replayed, pairing_b),
        other => panic!("expected replayed InviteRedeemPending, got {other:?}"),
    }
    server.stop().await;
}

// ---- Q-N3 recurrence (2026-11-27): the diagnostics name each exit --------
//
// A Windows CI run saw task-code joins fail with the joiner's
// `pairing target unavailable (0x2101)` and no line saying why. The
// registry now logs one decision line per exit (DEBUG, target
// `ak_signal::server_v2`); in these tests the same lines land in a sink, so
// each test drives one exit over the real engine and checks that the line
// for its own pairing names that exit and no other. Every assertion
// filters by the test's own pairing or device prefix: the sink is
// process-wide and the tests run in parallel.

/// Every decision line the handlers produced, in order.
pub(super) static DECISIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn sink_len() -> usize {
    DECISIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

/// The decision lines written since `since` that mention `about`.
fn decisions_since(since: usize, about: &str) -> Vec<String> {
    DECISIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .skip(since)
        .filter(|line| line.contains(about))
        .cloned()
        .collect()
}

fn prefix(bytes: &[u8]) -> String {
    util::hex_prefix(bytes, 4)
}

/// The pairing-scope `unavailable` for `object_id` (the pairing id, or the
/// all-zero id of a ghost's answer), and nothing else before it.
async fn expect_unavailable(client: &mut TestClient, object_id: [u8; 16]) {
    match client.recv().await {
        CtrlMessage::PairError {
            scope: crate::protocol_v2::ErrorScope::Pairing,
            object_id: seen,
            code: crate::protocol_v2::PAIR_UNAVAILABLE,
            ..
        } if seen == object_id => {}
        other => panic!("expected the pairing's `unavailable`, got {other:?}"),
    }
}

/// What `pending_redeem` hands back.
struct PendingRedeem {
    owner: TestClient,
    joiner: TestClient,
    pairing_id: PairingId,
    code_id: CodeId,
    /// The redeem's request id and nonce: the transcript must bind both.
    request_id: RequestId,
    redeemer_nonce: [u8; 32],
    /// The deadline the redeem was signed with.
    deadline_ms: u64,
}

/// A live code of an owner and a pending redeem of a joiner under it: the
/// registry has begun the pairing and pushed the offer to the owner.
/// `deadline` is evaluated when the redeem is about to go out — after the
/// two connections and the claim — so that a slow machine does not use up
/// the window before the request exists (the registry answers a redeem
/// whose deadline has passed with `PROTOCOL_UNSUPPORTED`).
async fn pending_redeem(server: &TestServer, deadline: impl FnOnce() -> u64) -> PendingRedeem {
    let registry_id = server.registry_id();
    let mut owner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "o").await;
    let mut joiner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "j").await;
    let lookup: Digest32 = [0x5A; 32];
    owner
        .send_frame(
            &CtrlFrame::new(CtrlMessage::InviteClaim {
                request_id: random_id16(),
                lookup_digest: lookup,
                ttl_secs: 0,
            })
            .with_max_redeems(1),
        )
        .await;
    let code_id = match owner.recv_frame().await.message {
        CtrlMessage::InviteClaimed { code_id, .. } => code_id,
        other => panic!("expected InviteClaimed, got {other:?}"),
    };
    let deadline = deadline();
    let redeem = signed_redeem(&joiner.key, &registry_id, lookup, random_id16(), deadline);
    joiner
        .send(&CtrlMessage::InviteRedeem(redeem.clone()))
        .await;
    let pairing_id = match joiner.recv().await {
        CtrlMessage::InviteRedeemPending { pairing_id, .. } => pairing_id,
        other => panic!("expected the redeem to begin, got {other:?}"),
    };
    let _offer = owner.recv_frame().await;
    PendingRedeem {
        owner,
        joiner,
        pairing_id,
        code_id,
        request_id: redeem.request_id,
        redeemer_nonce: redeem.redeemer_nonce,
        deadline_ms: deadline,
    }
}

/// The owner's challenge and the joiner's proof, both in time: after this
/// the registry holds the transcript and both proofs, and the pairing
/// waits for the two Prepared.
#[allow(clippy::too_many_arguments)] // the transcript's own fields, as in `drive_to_commit`
async fn challenge_and_proof(
    owner: &mut TestClient,
    joiner: &mut TestClient,
    registry_id: &RegistryId,
    code_id: CodeId,
    lookup: Digest32,
    pairing_id: PairingId,
    request_id: RequestId,
    redeemer_nonce: [u8; 32],
    deadline: u64,
) -> Digest32 {
    let transcript = PairTranscript {
        registry_id: *registry_id,
        code_id,
        code_revision: 1,
        pairing_id,
        request_id,
        lookup_digest: lookup,
        owner_device_id: owner.key.public_key(),
        redeemer_device_id: joiner.key.public_key(),
        owner_nonce: [0x0D; 32],
        redeemer_nonce,
        deadline_ms: deadline,
    };
    let bytes = transcript.to_bytes();
    owner
        .send(&CtrlMessage::PairChallenge {
            transcript: bytes,
            owner_proof: PairProof {
                mac: [0x11; 32],
                signature: owner.key.sign(&hello2::proof_signed_message(
                    OWNER_PROOF_DOMAIN,
                    &bytes,
                    &[0x11; 32],
                )),
            },
        })
        .await;
    match joiner.recv().await {
        CtrlMessage::PairChallenge { transcript, .. } => assert_eq!(transcript, bytes),
        other => panic!("expected the relayed PairChallenge, got {other:?}"),
    }
    let digest = hello2::transcript_digest(&bytes);
    joiner
        .send(&CtrlMessage::PairProof {
            pairing_id,
            transcript_digest: digest,
            redeemer_proof: PairProof {
                mac: [0x22; 32],
                signature: joiner.key.sign(&hello2::proof_signed_message(
                    REDEEMER_PROOF_DOMAIN,
                    &bytes,
                    &[0x22; 32],
                )),
            },
        })
        .await;
    match owner.recv().await {
        CtrlMessage::PairProof {
            transcript_digest, ..
        } => assert_eq!(transcript_digest, digest),
        other => panic!("expected the relayed PairProof, got {other:?}"),
    }
    digest
}

async fn sleep_until_after(deadline: u64) {
    while now_ms() <= deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A redeem for a code the registry does not have is a ghost: the joiner's
/// `unavailable` comes at the deadline and is named as a ghost expiry —
/// not as a sweep, and not as an abort.
#[tokio::test]
async fn diagnostics_name_a_ghost_redeem_expiring_at_its_deadline() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let mut joiner =
        TestClient::connect(server.addr, &registry_id, DeviceKeyPair::generate(), "j").await;
    let since = sink_len();
    let deadline = now_ms() + 300;
    let redeem = signed_redeem(
        &joiner.key,
        &registry_id,
        [0x9E; 32],
        random_id16(),
        deadline,
    );
    joiner
        .send(&CtrlMessage::InviteRedeem(redeem.clone()))
        .await;
    match joiner.recv().await {
        CtrlMessage::InviteRedeemPending { .. } => {}
        other => panic!("expected the ghost's pending shape, got {other:?}"),
    }
    // The unified answer carries the all-zero object id and the request id.
    match joiner.recv().await {
        CtrlMessage::PairError {
            request_id,
            scope: crate::protocol_v2::ErrorScope::Pairing,
            object_id,
            code: crate::protocol_v2::PAIR_UNAVAILABLE,
            ..
        } => {
            assert_eq!(request_id, redeem.request_id);
            assert_eq!(object_id, [0u8; 16]);
        }
        other => panic!("expected the ghost's `unavailable`, got {other:?}"),
    }
    let about_joiner = decisions_since(since, &prefix(&joiner.key.public_key()));
    assert!(
        about_joiner
            .iter()
            .any(|line| line.contains("recorded as a ghost")
                && line.contains("self-redeem: false")),
        "{about_joiner:?}"
    );
    assert!(
        about_joiner
            .iter()
            .all(|line| !line.contains("begun under code revision")),
        "{about_joiner:?}"
    );
    let expired = decisions_since(since, "ghost redeem expired");
    assert!(
        expired
            .iter()
            .any(|line| line.contains(&format!("at its {deadline} ms deadline"))),
        "{expired:?}"
    );
    server.stop().await;
}

/// A begun pairing nobody completes is swept at its deadline, and both
/// participants' `unavailable` is named as that sweep — not as an abort.
#[tokio::test]
async fn diagnostics_name_a_pending_pairing_swept_past_its_deadline() {
    let server = spawn_server().await;
    let since = sink_len();
    let pending = pending_redeem(&server, || now_ms() + 300).await;
    let PendingRedeem {
        mut owner,
        mut joiner,
        pairing_id,
        code_id: _,
        request_id: _,
        redeemer_nonce: _,
        deadline_ms: _,
    } = pending;
    expect_unavailable(&mut joiner, pairing_id).await;
    expect_unavailable(&mut owner, pairing_id).await;
    let about = decisions_since(since, &prefix(&pairing_id));
    assert!(
        about
            .iter()
            .any(|line| line.contains("is past its deadline and is aborted")),
        "{about:?}"
    );
    assert!(
        about
            .iter()
            .all(|line| !line.contains("aborted by an explicit request")),
        "{about:?}"
    );
    server.stop().await;
}

/// An owner that gives up before the deadline: the joiner's `unavailable`
/// is named as that explicit abort — and no sweep line exists for the
/// pairing.
#[tokio::test]
async fn diagnostics_name_an_owner_aborting_before_the_deadline() {
    let server = spawn_server().await;
    let since = sink_len();
    let pending = pending_redeem(&server, || now_ms() + 8_000).await;
    let PendingRedeem {
        mut owner,
        mut joiner,
        pairing_id,
        code_id: _,
        request_id: _,
        redeemer_nonce: _,
        deadline_ms: _,
    } = pending;
    owner
        .send(&CtrlMessage::PairAbort {
            request_id: random_id16(),
            pairing_id,
            reason: crate::protocol_v2::AbortReason::User,
        })
        .await;
    expect_unavailable(&mut joiner, pairing_id).await;
    let about = decisions_since(since, &prefix(&pairing_id));
    assert!(
        about
            .iter()
            .any(|line| line.contains("aborted by an explicit request from")
                && line.contains(&prefix(&owner.key.public_key()))),
        "{about:?}"
    );
    assert!(
        about
            .iter()
            .all(|line| !line.contains("is past its deadline and is aborted")),
        "{about:?}"
    );
    server.stop().await;
}

/// A Prepared that reaches the registry after the deadline is answered
/// `unavailable`, and the line says which of the two exits took it: the
/// late-Prepared check itself, or the sweep that got there first (the
/// sweeper runs every 200 ms, so both orders happen).
#[tokio::test]
async fn diagnostics_name_a_prepared_arriving_after_the_deadline() {
    let server = spawn_server().await;
    let registry_id = server.registry_id();
    let since = sink_len();
    let pending = pending_redeem(&server, || now_ms() + 700).await;
    let PendingRedeem {
        mut owner,
        mut joiner,
        pairing_id,
        code_id,
        request_id,
        redeemer_nonce,
        deadline_ms: deadline,
    } = pending;
    let digest = challenge_and_proof(
        &mut owner,
        &mut joiner,
        &registry_id,
        code_id,
        [0x5A; 32],
        pairing_id,
        request_id,
        redeemer_nonce,
        deadline,
    )
    .await;
    sleep_until_after(deadline).await;
    owner
        .send(&CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest: digest,
            signature: owner
                .key
                .sign(&hello2::prepared_message(&pairing_id, &digest)),
        })
        .await;
    // The owner's first error is its own answer (the all-zero object id)
    // or, when the sweep got there first, the sweep's notification.
    match owner.recv().await {
        CtrlMessage::PairError {
            code: crate::protocol_v2::PAIR_UNAVAILABLE,
            ..
        } => {}
        other => panic!("expected the registry's `unavailable`, got {other:?}"),
    }
    let about = decisions_since(since, &prefix(&pairing_id));
    let late_check = about.iter().any(|line| {
        line.contains(
            "answered unavailable: not a participant: false, transcript digest differs: false, ",
        )
    });
    let swept_first = about
        .iter()
        .any(|line| line.contains("is past its deadline and is aborted"));
    assert!(
        late_check || swept_first,
        "neither exit is named for the late Prepared: {about:?}"
    );
    // The joiner's `unavailable` comes from the sweep in either order.
    expect_unavailable(&mut joiner, pairing_id).await;
    // Review 20: both notices are on record for the owner — the sweep's
    // and the late Prepared's answer — numbered in the order its
    // connection took them; that order is the one the frames arrived in.
    let to_owner = notices(since, &pairing_id, "owner", &owner.key.public_key());
    assert_eq!(to_owner.len(), 2, "{to_owner:?}");
    let late_first = to_owner[0].contains("prepared past the deadline");
    let sweep_first = to_owner[0].contains("sweep:");
    assert!(late_first || sweep_first, "{to_owner:?}");
    assert!(
        to_owner[0].contains("notice 1 ") && to_owner[1].contains("notice 2 "),
        "{to_owner:?}"
    );
    assert!(
        to_owner[1].contains(if late_first {
            "sweep:"
        } else {
            "prepared past the deadline"
        }),
        "{to_owner:?}"
    );
    server.stop().await;
}

// ---- review 20: the order of two identical errors -----------------------------

/// Every engine this process serves, by registry id, weakly (a stopped
/// server is not kept alive). `serve_v2_with_revision` registers its engine
/// here in test builds.
static SHARED: Mutex<Vec<(RegistryId, std::sync::Weak<Shared>)>> = Mutex::new(Vec::new());

pub(super) fn register_shared(shared: &Arc<Shared>) {
    SHARED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((shared.registry_id, Arc::downgrade(shared)));
}

/// Stop the serve loop from sweeping `server`'s engine and hand back the
/// state to run `sweep` from: the test then decides whether the sweep or a
/// handler comes first, instead of racing the 200 ms tick. The engine
/// registers itself when its serve task starts, which can be after the
/// test gets here: that is waited for.
async fn hold_the_sweeper(server: &TestServer) -> Arc<Shared> {
    let registry_id = server.registry_id();
    let shared = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = SHARED
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .find(|(registered, _)| *registered == registry_id)
                .and_then(|(_, shared)| shared.upgrade());
            match found {
                Some(shared) => break shared,
                None => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("the engine registers its shared state when it starts serving");
    shared
        .sweep_held
        .store(true, std::sync::atomic::Ordering::Relaxed);
    shared
}

fn sweep_now(shared: &Shared) {
    super::sweep(shared);
}

/// A pairing with both proofs that is now past its deadline, on an engine
/// whose sweeper is held.
struct HeldPairing {
    shared: Arc<Shared>,
    owner: TestClient,
    joiner: TestClient,
    pairing_id: PairingId,
    digest: Digest32,
}

async fn held_pairing_past_its_deadline(server: &TestServer) -> HeldPairing {
    let shared = hold_the_sweeper(server).await;
    let registry_id = server.registry_id();
    let PendingRedeem {
        mut owner,
        mut joiner,
        pairing_id,
        code_id,
        request_id,
        redeemer_nonce,
        deadline_ms: deadline,
    } = pending_redeem(server, || now_ms() + 700).await;
    let digest = challenge_and_proof(
        &mut owner,
        &mut joiner,
        &registry_id,
        code_id,
        [0x5A; 32],
        pairing_id,
        request_id,
        redeemer_nonce,
        deadline,
    )
    .await;
    sleep_until_after(deadline).await;
    HeldPairing {
        shared,
        owner,
        joiner,
        pairing_id,
        digest,
    }
}

async fn send_prepared(client: &mut TestClient, pairing_id: PairingId, digest: Digest32) {
    client
        .send(&CtrlMessage::PairPrepared {
            pairing_id,
            transcript_digest: digest,
            signature: client
                .key
                .sign(&hello2::prepared_message(&pairing_id, &digest)),
        })
        .await;
}

/// The registry's notice lines to one participant of a pairing, in the
/// sink's order.
fn notices(since: usize, pairing_id: &PairingId, role: &str, device: &DeviceId) -> Vec<String> {
    let to = format!("to the {role} {}", prefix(device));
    DECISIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .skip(since)
        .filter(|line| line.contains(&prefix(pairing_id)) && line.contains(&to))
        .cloned()
        .collect()
}

/// The sweep and a late Prepared answer the redeemer with the same frame.
/// Sweep first: the redeemer's first `unavailable` is the sweep's, notice 1
/// on its connection; the late Prepared's answer is notice 2.
#[tokio::test]
async fn diagnostics_order_a_sweep_before_a_late_prepared() {
    let server = spawn_server().await;
    let since = sink_len();
    let HeldPairing {
        shared,
        mut owner,
        mut joiner,
        pairing_id,
        digest,
    } = held_pairing_past_its_deadline(&server).await;
    sweep_now(&shared);
    expect_unavailable(&mut joiner, pairing_id).await;
    expect_unavailable(&mut owner, pairing_id).await;
    send_prepared(&mut joiner, pairing_id, digest).await;
    expect_unavailable(&mut joiner, pairing_id).await;

    let to_redeemer = notices(since, &pairing_id, "redeemer", &joiner.key.public_key());
    assert_eq!(to_redeemer.len(), 2, "{to_redeemer:?}");
    assert!(
        to_redeemer[0].contains("notice 1 to the redeemer")
            && to_redeemer[0].contains("sweep:")
            && to_redeemer[0].ends_with("enqueued"),
        "{to_redeemer:?}"
    );
    assert!(
        to_redeemer[1].contains("notice 2 to the redeemer")
            && to_redeemer[1].contains("prepared past the deadline")
            && to_redeemer[1].ends_with("enqueued"),
        "{to_redeemer:?}"
    );
    let to_owner = notices(since, &pairing_id, "owner", &owner.key.public_key());
    assert_eq!(to_owner.len(), 1, "{to_owner:?}");
    assert!(
        to_owner[0].contains("notice 1 to the owner")
            && to_owner[0].contains("sweep:")
            && to_owner[0].ends_with("enqueued"),
        "{to_owner:?}"
    );
    // Nothing follows the two.
    match tokio::time::timeout(Duration::from_millis(300), joiner.recv()).await {
        Err(_elapsed) => {}
        Ok(other) => panic!("a third frame for the pairing: {other:?}"),
    }
    server.stop().await;
}

/// The other order: the late Prepared's answer is notice 1 and the
/// redeemer's first `unavailable`; the sweep's is notice 2.
#[tokio::test]
async fn diagnostics_order_a_late_prepared_before_the_sweep() {
    let server = spawn_server().await;
    let since = sink_len();
    let HeldPairing {
        shared,
        mut owner,
        mut joiner,
        pairing_id,
        digest,
    } = held_pairing_past_its_deadline(&server).await;
    send_prepared(&mut joiner, pairing_id, digest).await;
    expect_unavailable(&mut joiner, pairing_id).await;
    sweep_now(&shared);
    expect_unavailable(&mut joiner, pairing_id).await;
    expect_unavailable(&mut owner, pairing_id).await;

    let to_redeemer = notices(since, &pairing_id, "redeemer", &joiner.key.public_key());
    assert_eq!(to_redeemer.len(), 2, "{to_redeemer:?}");
    assert!(
        to_redeemer[0].contains("notice 1 to the redeemer")
            && to_redeemer[0].contains("prepared past the deadline")
            && to_redeemer[0].ends_with("enqueued"),
        "{to_redeemer:?}"
    );
    assert!(
        to_redeemer[1].contains("notice 2 to the redeemer")
            && to_redeemer[1].contains("sweep:")
            && to_redeemer[1].ends_with("enqueued"),
        "{to_redeemer:?}"
    );
    let to_owner = notices(since, &pairing_id, "owner", &owner.key.public_key());
    assert_eq!(to_owner.len(), 1, "{to_owner:?}");
    assert!(
        to_owner[0].contains("notice 1 to the owner")
            && to_owner[0].contains("sweep:")
            && to_owner[0].ends_with("enqueued"),
        "{to_owner:?}"
    );
    match tokio::time::timeout(Duration::from_millis(300), joiner.recv()).await {
        Err(_elapsed) => {}
        Ok(other) => panic!("a third frame for the pairing: {other:?}"),
    }
    server.stop().await;
}

/// A participant that went offline before the sweep: nothing is queued for
/// it and its line says so; the owner's notice is number 1 as usual.
#[tokio::test]
async fn diagnostics_name_a_notice_dropped_for_an_offline_participant() {
    let server = spawn_server().await;
    let since = sink_len();
    let HeldPairing {
        shared,
        mut owner,
        joiner,
        pairing_id,
        ..
    } = held_pairing_past_its_deadline(&server).await;
    let redeemer_id = joiner.key.public_key();
    drop(joiner);
    // The registry forgets the connection once its reader saw the EOF.
    let forgotten = tokio::time::timeout(Duration::from_secs(5), async {
        while shared.lock().conns.contains_key(&redeemer_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(forgotten.is_ok(), "the registry still lists the redeemer");
    sweep_now(&shared);
    expect_unavailable(&mut owner, pairing_id).await;

    let to_redeemer = notices(since, &pairing_id, "redeemer", &redeemer_id);
    assert_eq!(to_redeemer.len(), 1, "{to_redeemer:?}");
    assert!(
        to_redeemer[0].contains("sweep:")
            && to_redeemer[0].ends_with("dropped: the device is offline"),
        "{to_redeemer:?}"
    );
    let to_owner = notices(since, &pairing_id, "owner", &owner.key.public_key());
    assert_eq!(to_owner.len(), 1, "{to_owner:?}");
    assert!(
        to_owner[0].contains("notice 1 to the owner")
            && to_owner[0].contains("sweep:")
            && to_owner[0].ends_with("enqueued"),
        "{to_owner:?}"
    );
    server.stop().await;
}

/// The owner aborts right after the deadline, where the sweep may already
/// have run: the joiner gets exactly one `unavailable`, and the lines say
/// which exit produced it.
#[tokio::test]
async fn diagnostics_say_which_of_a_sweep_and_a_late_abort_came_first() {
    let server = spawn_server().await;
    let since = sink_len();
    let pending = pending_redeem(&server, || now_ms() + 300).await;
    let PendingRedeem {
        mut owner,
        mut joiner,
        pairing_id,
        code_id: _,
        request_id: _,
        redeemer_nonce: _,
        deadline_ms: deadline,
    } = pending;
    sleep_until_after(deadline).await;
    owner
        .send(&CtrlMessage::PairAbort {
            request_id: random_id16(),
            pairing_id,
            reason: crate::protocol_v2::AbortReason::User,
        })
        .await;
    expect_unavailable(&mut joiner, pairing_id).await;
    // A second sweep tick (200 ms) has had time to record its part.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let about = decisions_since(since, &prefix(&pairing_id));
    let aborted = about
        .iter()
        .position(|line| line.contains("aborted by an explicit request from"));
    let swept = about
        .iter()
        .position(|line| line.contains("is past its deadline and is aborted"));
    let first = match (swept, aborted) {
        (None, None) => panic!("neither exit is named for the joiner's `unavailable`: {about:?}"),
        (Some(s), Some(a)) if s < a => "sweep",
        (Some(_), _) => "sweep",
        (None, Some(_)) => "abort",
    };
    // No second `unavailable` for the pairing: whichever exit won ended it.
    match tokio::time::timeout(Duration::from_millis(400), joiner.recv()).await {
        Err(_elapsed) => {}
        Ok(CtrlMessage::PairError { object_id, .. }) if object_id == pairing_id => {
            panic!("a second `unavailable` for the pairing after the {first}")
        }
        Ok(other) => panic!("unexpected frame after the {first}: {other:?}"),
    }
    server.stop().await;
}
