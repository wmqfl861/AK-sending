//! Control-protocol v2 client tests (A3).
//!
//! Contract under test: `docs/contracts/protocol-v2.md` (message set,
//! directions) and `docs/contracts/pairing-flow.md` (idempotent retries,
//! bounded inboxes, replacement). The peer is a scripted **fake registry**:
//! a real `tokio` TCP listener behind real rustls TLS with a self-signed
//! Ed25519 certificate, hand-writing v2 frames — A2 (the real server) lands
//! in parallel; wave 21 wires the two together.
//!
//! Gates covered here:
//!
//! * pinned TLS + AuthChallenge/Hello/Welcome handshake (the Hello
//!   signature is verified against the exact signing-bytes construction,
//!   including the TLS exporter);
//! * single-reader dispatch: 10 parallel claims with zero crosstalk, per-key
//!   routing for sessions/pairings, unmatched frames counted and dropped
//!   without killing the connection;
//! * idempotency: timeouts re-send the same `request_id`; cancellation only
//!   removes the waiter;
//! * reconnect: waiters failed, request-id space reset, pairing
//!   subscriptions surviving;
//! * replacement notice (`ServerError{ERR_DEVICE_REPLACED}` → `is_replaced`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ak_identity::{DeviceKeyPair, Fingerprint};
use ak_signal::client_v2::{
    hello_v2_signing_bytes, Backoff, ClientV2, ClientV2Config, ClientV2Error, ConnState,
    ERR_DEVICE_REPLACED, HELLO_EXPORTER_LABEL,
};
use ak_signal::protocol_v2::{
    AbortReason, CtrlFrame, CtrlMessage, DeviceId, Digest32, ErrorScope, InviteState,
    InviteSummary, Nonce32, PairProof, PairingId, PairingState, PeerSignal, RedeemRequest,
    RequestId, Role, SessionId, StateFilter, Transcript256, CURSOR_START,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn rng32(seed: u8) -> Digest32 {
    [seed; 32]
}

fn pid(seed: u8) -> PairingId {
    [seed; 16]
}

fn server_tls_config(key: &DeviceKeyPair) -> (rustls::ServerConfig, Fingerprint) {
    let bundle = ak_identity::tls::make_self_signed(key);
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(
            bundle.cert_der.clone(),
        )],
        rustls::pki_types::PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(
            bundle.key_pkcs8_der.clone(),
        )),
    )
    .unwrap();
    config.alpn_protocols = vec![ak_signal::client_v2::CTRL_ALPN.to_vec()];
    (config, bundle.fingerprint)
}

// ---- fake registry --------------------------------------------------------

struct HelloRecord {
    device_id: DeviceId,
    name: String,
    signature_ok: bool,
}

struct ConnHandle {
    out: tokio::sync::mpsc::UnboundedSender<CtrlMessage>,
    kill: Arc<tokio::sync::Notify>,
}

/// Scripted v2 registry: real TLS (self-signed Ed25519, no client auth),
/// the frozen opening exchange, then pure test puppeteering via
/// [`FakeRegistry::send`] / [`FakeRegistry::kill`].
struct FakeRegistry {
    addr: SocketAddr,
    fp: Fingerprint,
    conns: Arc<StdMutex<HashMap<u64, ConnHandle>>>,
    hellos: Arc<StdMutex<HashMap<u64, HelloRecord>>>,
    received: tokio::sync::mpsc::Receiver<(u64, CtrlMessage)>,
    next_conn: Arc<AtomicU64>,
}

impl FakeRegistry {
    async fn start() -> FakeRegistry {
        let registry_key = DeviceKeyPair::generate();
        let (config, fp) = server_tls_config(&registry_key);
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let conns: Arc<StdMutex<HashMap<u64, ConnHandle>>> = Arc::default();
        let hellos: Arc<StdMutex<HashMap<u64, HelloRecord>>> = Arc::default();
        let (tx_received, rx_received) = tokio::sync::mpsc::channel(256);
        let next_conn = Arc::new(AtomicU64::new(0));

        let accept = {
            let conns = Arc::clone(&conns);
            let hellos = Arc::clone(&hellos);
            let next_conn = Arc::clone(&next_conn);
            tokio::spawn(async move {
                loop {
                    let (tcp, _) = match listener.accept().await {
                        Ok(pair) => pair,
                        Err(_) => return,
                    };
                    let _ = tcp.set_nodelay(true);
                    let tls = match acceptor.accept(tcp).await {
                        Ok(tls) => tls,
                        Err(_) => continue,
                    };
                    let conn = next_conn.fetch_add(1, Ordering::AcqRel) + 1;
                    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
                    let kill = Arc::new(tokio::sync::Notify::new());
                    conns.lock().unwrap().insert(
                        conn,
                        ConnHandle {
                            out: out_tx,
                            kill: Arc::clone(&kill),
                        },
                    );
                    if opening_exchange(tls, conn, &hellos, &fp, &tx_received, out_rx, &kill)
                        .await
                        .is_err()
                    {
                        conns.lock().unwrap().remove(&conn);
                    }
                }
            })
        };
        // Only used to keep the accept task alive for the test duration.
        std::mem::forget(accept);

        FakeRegistry {
            addr,
            fp,
            conns,
            hellos,
            received: rx_received,
            next_conn,
        }
    }

    /// Next inbound frame from any connection (arrival order).
    async fn next_received(&mut self) -> (u64, CtrlMessage) {
        tokio::time::timeout(Duration::from_secs(10), self.received.recv())
            .await
            .expect("fake registry frame within 10s")
            .expect("accept loop alive")
    }

    /// Deliver one server→client frame on `conn`.
    fn send(&self, conn: u64, message: CtrlMessage) {
        self.conns
            .lock()
            .unwrap()
            .get(&conn)
            .expect("connection alive")
            .out
            .send(message)
            .expect("conn writer alive");
    }

    /// Abruptly drop one connection (no close_notify).
    fn kill(&self, conn: u64) {
        if let Some(handle) = self.conns.lock().unwrap().remove(&conn) {
            handle.kill.notify_waiters();
        }
    }

    fn hello_record(&self, conn: u64) -> Option<(DeviceId, String, bool)> {
        self.hellos
            .lock()
            .unwrap()
            .get(&conn)
            .map(|r| (r.device_id, r.name.clone(), r.signature_ok))
    }

    fn live_conns(&self) -> Vec<u64> {
        self.conns.lock().unwrap().keys().copied().collect()
    }

    fn connection_count(&self) -> u64 {
        self.next_conn.load(Ordering::Acquire)
    }
}

/// Server-side frame write (direction-checked: authored by the server).
async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: CtrlMessage,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(96);
    CtrlFrame::new(message)
        .try_encode_as(Role::Server, &mut buf)
        .expect("fake server encodes legal frames");
    writer.write_all(&buf).await
}

/// Server-side frame read.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<CtrlMessage>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    if header[0] != ak_signal::protocol_v2::VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad version",
        ));
    }
    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if 4 + declared > ak_signal::protocol_v2::MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "oversized",
        ));
    }
    let mut frame = header.to_vec();
    frame.resize(4 + declared, 0);
    reader.read_exact(&mut frame[4..]).await?;
    Ok(Some(
        CtrlFrame::decode_as(&frame, Role::Server)
            .expect("fake server decodes client frames")
            .message,
    ))
}

/// The frozen opening exchange, then the connection's reader/writer tasks.
/// A bad Hello gets `ServerError` and the connection is dropped; the record
/// stays visible to the test through `hellos`.
#[allow(clippy::too_many_arguments)]
async fn opening_exchange(
    mut tls: tokio_rustls::server::TlsStream<TcpStream>,
    conn: u64,
    hellos: &Arc<StdMutex<HashMap<u64, HelloRecord>>>,
    fp: &Fingerprint,
    tx_received: &tokio::sync::mpsc::Sender<(u64, CtrlMessage)>,
    out_rx: tokio::sync::mpsc::UnboundedReceiver<CtrlMessage>,
    kill: &Arc<tokio::sync::Notify>,
) -> Result<(), ()> {
    let nonce: Nonce32 = [conn as u8 | 0x80; 32];
    write_frame(
        &mut tls,
        CtrlMessage::AuthChallenge {
            registry_id: *fp.as_bytes(),
            nonce,
            expires_ms: now_ms() + 10_000,
        },
    )
    .await
    .map_err(|_| ())?;

    let hello = match read_frame(&mut tls).await {
        Ok(Some(CtrlMessage::Hello {
            version,
            device_id,
            name,
            timestamp_ms,
            challenge,
            signature,
        })) => {
            let mut exporter = [0u8; 32];
            let (_io, server_conn) = tls.get_ref();
            server_conn
                .export_keying_material(&mut exporter, HELLO_EXPORTER_LABEL, None)
                .unwrap();
            let signing = hello_v2_signing_bytes(
                fp.as_bytes(),
                &exporter,
                version,
                &device_id,
                &name,
                timestamp_ms,
                &challenge,
            );
            let signature_ok = VerifyingKey::from_bytes(&device_id)
                .ok()
                .map(|key| {
                    key.verify(&signing, &Signature::from_bytes(&signature))
                        .is_ok()
                })
                .unwrap_or(false);
            HelloRecord {
                device_id,
                name,
                signature_ok: signature_ok && version == 2 && challenge == nonce,
            }
        }
        _ => return Err(()),
    };
    let ok = hello.signature_ok;
    hellos.lock().unwrap().insert(conn, hello);
    if !ok {
        write_frame(
            &mut tls,
            CtrlMessage::ServerError {
                code: 4,
                message: "bad hello".into(),
            },
        )
        .await
        .map_err(|_| ())?;
        return Err(());
    }
    write_frame(
        &mut tls,
        CtrlMessage::Welcome {
            session_ttl_secs: 120,
            capabilities: 0,
        },
    )
    .await
    .map_err(|_| ())?;

    let (read_half, write_half) = tokio::io::split(tls);
    spawn_conn_io(
        conn,
        read_half,
        write_half,
        tx_received.clone(),
        out_rx,
        kill,
    );
    Ok(())
}

/// Reader/writer pump for one live fake-registry connection. Both halves
/// abort when the test kills the connection.
fn spawn_conn_io(
    conn: u64,
    read_half: ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>,
    write_half: WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>,
    tx_received: tokio::sync::mpsc::Sender<(u64, CtrlMessage)>,
    mut out_rx: tokio::sync::mpsc::UnboundedReceiver<CtrlMessage>,
    kill: &Arc<tokio::sync::Notify>,
) {
    let kill_reader = Arc::clone(kill);
    tokio::spawn(async move {
        let mut read_half = read_half;
        loop {
            tokio::select! {
                _ = kill_reader.notified() => break,
                frame = read_frame(&mut read_half) => match frame {
                    Ok(Some(message)) => {
                        if tx_received.send((conn, message)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => break,
                },
            }
        }
    });
    let kill_writer = Arc::clone(kill);
    tokio::spawn(async move {
        let mut write_half = write_half;
        loop {
            tokio::select! {
                biased;
                // Queued frames drain before a kill notice takes effect.
                message = out_rx.recv() => match message {
                    Some(message) => {
                        if write_frame(&mut write_half, message).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = kill_writer.notified() => break,
            }
        }
    });
}

/// A listener with valid TLS that sends one (attacker-controlled)
/// AuthChallenge and then stays silent. Returns the address and the
/// certificate fingerprint (so tests can pin the cert correctly while
/// breaking the protocol layer above it).
struct BadChallenge {
    addr: SocketAddr,
    cert_fp: Fingerprint,
}

impl BadChallenge {
    /// `None` names the registry after the served certificate (so the pin
    /// matches and only the expiry is wrong).
    async fn start(registry_id: Option<[u8; 32]>, expires_ms: u64) -> BadChallenge {
        let key = DeviceKeyPair::generate();
        let (config, cert_fp) = server_tls_config(&key);
        let registry_id = registry_id.unwrap_or(*cert_fp.as_bytes());
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let _ = write_frame(
                        &mut tls,
                        CtrlMessage::AuthChallenge {
                            registry_id,
                            nonce: [1; 32],
                            expires_ms,
                        },
                    )
                    .await;
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
        });
        BadChallenge { addr, cert_fp }
    }

    /// TLS completes, but no frame ever arrives.
    async fn start_silent() -> BadChallenge {
        let key = DeviceKeyPair::generate();
        let (config, cert_fp) = server_tls_config(&key);
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                if let Ok(_tls) = acceptor.accept(tcp).await {
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
        });
        BadChallenge { addr, cert_fp }
    }
}

// ---- helpers --------------------------------------------------------------

fn fast_config(response_timeout: Duration, retries: u32) -> ClientV2Config {
    ClientV2Config {
        connect_timeout: Duration::from_secs(3),
        response_timeout,
        request_retries: retries,
        reconnect_attempts: 2,
        ..ClientV2Config::default()
    }
}

async fn connect(fake: &FakeRegistry, key: &DeviceKeyPair) -> ClientV2 {
    ClientV2::connect_with_config(
        fake.addr,
        fake.fp,
        key,
        "test-device",
        ClientV2Config {
            connect_timeout: Duration::from_secs(3),
            ..ClientV2Config::default()
        },
    )
    .await
    .expect("connect to fake registry")
}

/// Claim on a fresh connection, answering from the fake: returns the
/// request id the client used.
async fn claim_once(fake: &mut FakeRegistry, client: &ClientV2, seed: u8) -> RequestId {
    let c = client.clone();
    let task = tokio::spawn(async move { c.invite_claim(rng32(seed), 0).await });
    let (conn, message) = fake.next_received().await;
    let request_id = match message {
        CtrlMessage::InviteClaim { request_id, .. } => request_id,
        other => panic!("expected InviteClaim, got {other:?}"),
    };
    fake.send(
        conn,
        CtrlMessage::InviteClaimed {
            request_id,
            code_id: [seed; 16],
            created_ms: 1,
            expires_ms: 0,
            revision: 1,
        },
    );
    task.await
        .unwrap()
        .unwrap_or_else(|e| panic!("claim failed: {e:?}"))
        .request_id
}

// ---- handshake ------------------------------------------------------------

#[tokio::test]
async fn connect_handshake_verifies_hello_signature() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    assert!(client.is_connected());
    assert_eq!(client.device_id(), key.public_key());
    let (device_id, name, signature_ok) = fake.hello_record(1).expect("hello recorded");
    assert!(signature_ok, "fake registry verified the Hello signature");
    assert_eq!(device_id, key.public_key());
    assert_eq!(name, "test-device");
}

#[tokio::test]
async fn connect_rejects_authchallenge_naming_another_registry() {
    // TLS pin matches the served certificate, but the AuthChallenge
    // registry_id does not match the pin: refused before signing anything.
    let bad = BadChallenge::start(Some([0x99; 32]), now_ms() + 10_000).await;
    let key = DeviceKeyPair::generate();
    let result = ClientV2::connect_with_config(
        bad.addr,
        bad.cert_fp,
        &key,
        "x",
        fast_config(Duration::from_secs(3), 0),
    )
    .await;
    assert!(matches!(result, Err(ClientV2Error::RegistryMismatch)));
}

#[tokio::test]
async fn connect_rejects_expired_challenge() {
    let bad = BadChallenge::start(None, now_ms() - 1_000).await;
    let key = DeviceKeyPair::generate();
    let result = ClientV2::connect_with_config(
        bad.addr,
        bad.cert_fp,
        &key,
        "x",
        fast_config(Duration::from_secs(3), 0),
    )
    .await;
    assert!(matches!(
        result,
        Err(ClientV2Error::ChallengeExpired { .. })
    ));
}

#[tokio::test]
async fn connect_with_wrong_tls_pin_fails_closed() {
    let fake = FakeRegistry::start().await;
    let other = DeviceKeyPair::generate();
    let (_config, other_fp) = server_tls_config(&other);
    let key = DeviceKeyPair::generate();
    let result = ClientV2::connect_with_config(
        fake.addr,
        other_fp, // pin ≠ served certificate
        &key,
        "x",
        fast_config(Duration::from_secs(3), 0),
    )
    .await;
    assert!(matches!(
        result,
        Err(ClientV2Error::Io(_)) | Err(ClientV2Error::HandshakeTimeout(_))
    ));
}

#[tokio::test]
async fn connect_times_out_when_challenge_never_arrives() {
    let silent = BadChallenge::start_silent().await;
    let key = DeviceKeyPair::generate();
    let result = ClientV2::connect_with_config(
        silent.addr,
        silent.cert_fp,
        &key,
        "x",
        ClientV2Config {
            connect_timeout: Duration::from_millis(400),
            ..ClientV2Config::default()
        },
    )
    .await;
    assert!(matches!(result, Err(ClientV2Error::HandshakeTimeout(_))));
}

// ---- requests -------------------------------------------------------------

#[tokio::test]
async fn invite_claim_roundtrip() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let lookup = rng32(0xA5);
    let c = client.clone();
    let task = tokio::spawn(async move { c.invite_claim(lookup, 0).await });
    let (conn, message) = fake.next_received().await;
    assert_eq!(conn, 1);
    match message {
        CtrlMessage::InviteClaim {
            request_id,
            lookup_digest,
            ttl_secs,
        } => {
            assert_eq!(lookup_digest, lookup);
            assert_eq!(ttl_secs, 0);
            fake.send(
                conn,
                CtrlMessage::InviteClaimed {
                    request_id,
                    code_id: [0xC1; 16],
                    created_ms: 11,
                    expires_ms: 0,
                    revision: 3,
                },
            );
        }
        other => panic!("expected InviteClaim, got {other:?}"),
    }
    let claimed = task.await.unwrap().expect("claim ok");
    assert_eq!(claimed.code_id, [0xC1; 16]);
    assert_eq!(claimed.revision, 3);
    assert_eq!(claimed.expires_ms, 0);
}

#[tokio::test]
async fn ten_parallel_claims_never_cross() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    // Fake policy: code_id = request_id, so any crosstalk misattributes
    // visibly. The driver answers every claim it sees.
    let driver = {
        let mut fake = fake;
        tokio::spawn(async move {
            for _ in 0..10 {
                let (conn, message) = fake.next_received().await;
                if let CtrlMessage::InviteClaim { request_id, .. } = message {
                    fake.send(
                        conn,
                        CtrlMessage::InviteClaimed {
                            request_id,
                            code_id: request_id,
                            created_ms: 1,
                            expires_ms: 0,
                            revision: 1,
                        },
                    );
                }
            }
        })
    };

    let mut tasks = Vec::new();
    for i in 0..10u8 {
        let c = client.clone();
        tasks.push(tokio::spawn(async move {
            c.invite_claim([i; 32], 60).await.expect("claim ok")
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        let claimed = task.await.unwrap();
        // code_id == request_id proves each caller saw its own answer.
        assert_eq!(claimed.code_id, claimed.request_id);
        ids.push(claimed.request_id);
    }
    let unique: std::collections::HashSet<_> = ids.into_iter().collect();
    assert_eq!(unique.len(), 10, "all request ids distinct");
    driver.await.unwrap();
}

#[tokio::test]
async fn response_timeout_is_reported_with_attempt_count() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = ClientV2::connect_with_config(
        fake.addr,
        fake.fp,
        &key,
        "slow",
        fast_config(Duration::from_millis(250), 0),
    )
    .await
    .unwrap();

    let result = client.invite_claim(rng32(1), 0).await;
    assert!(matches!(
        result,
        Err(ClientV2Error::Timeout { attempts: 1 })
    ));
    // The frame was still delivered to the (silent) server.
    let (_conn, message) = fake.next_received().await;
    assert!(matches!(message, CtrlMessage::InviteClaim { .. }));
}

#[tokio::test]
async fn timeout_retry_reuses_the_same_request_id() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = ClientV2::connect_with_config(
        fake.addr,
        fake.fp,
        &key,
        "retry",
        fast_config(Duration::from_millis(250), 1),
    )
    .await
    .unwrap();

    // Drop the first InviteClaim (no response); answer the second — the
    // client must have re-sent with the SAME request id. The "only answer
    // once per id" semantics is what the real server guarantees through its
    // idempotency table; here the fake asserts the client-side half.
    let driver = {
        let mut fake = fake;
        tokio::spawn(async move {
            let mut first: Option<RequestId> = None;
            let mut second: Option<RequestId> = None;
            for _ in 0..2 {
                let (conn, message) = fake.next_received().await;
                if let CtrlMessage::InviteClaim { request_id, .. } = message {
                    if first.is_none() {
                        first = Some(request_id);
                    } else if second.is_none() {
                        second = Some(request_id);
                        fake.send(
                            conn,
                            CtrlMessage::InviteClaimed {
                                request_id,
                                code_id: [0x42; 16],
                                created_ms: 1,
                                expires_ms: 0,
                                revision: 1,
                            },
                        );
                    }
                }
            }
            (first.unwrap(), second.unwrap())
        })
    };

    let claimed = client.invite_claim(rng32(2), 0).await.expect("claim ok");
    let (first, second) = driver.await.unwrap();
    assert_eq!(first, second, "retry reuses the same request id");
    assert_eq!(claimed.request_id, first);
    assert_eq!(claimed.code_id, [0x42; 16]);
}

#[tokio::test]
async fn cancelling_a_request_keeps_the_connection_alive() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let doomed = client.clone();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        tokio::select! {
            _ = drop_rx => {} // drop the request future mid-wait
            r = doomed.invite_claim(rng32(3), 0) => {
                panic!("should have been cancelled, got {r:?}")
            }
        }
    });
    let (conn, message) = fake.next_received().await;
    assert!(matches!(message, CtrlMessage::InviteClaim { .. }));
    drop_tx.send(()).unwrap();
    task.await.unwrap();

    // Late response for the cancelled request: counted as unmatched, not
    // delivered, and the connection must still work.
    if let CtrlMessage::InviteClaim { request_id, .. } = message {
        fake.send(
            conn,
            CtrlMessage::InviteClaimed {
                request_id,
                code_id: [9; 16],
                created_ms: 1,
                expires_ms: 0,
                revision: 1,
            },
        );
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(client.dropped_unmatched_frames(), 1);
    assert!(client.is_connected());

    // The same logical request again works on the same connection.
    let again_id = claim_once(&mut fake, &client, 4).await;
    assert_ne!(again_id, [0u8; 16]);
}

#[tokio::test]
async fn pair_error_resolves_the_pending_request() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let c = client.clone();
    let task = tokio::spawn(async move { c.invite_claim(rng32(5), 0).await });
    let (conn, message) = fake.next_received().await;
    let request_id = match message {
        CtrlMessage::InviteClaim { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    fake.send(
        conn,
        CtrlMessage::PairError {
            request_id,
            scope: ErrorScope::Request,
            object_id: request_id,
            code: ak_signal::protocol_v2::RATE_LIMITED,
            retry_after_ms: 1_000,
        },
    );
    match task.await.unwrap() {
        Err(ClientV2Error::Pair(info)) => {
            assert_eq!(info.code, ak_signal::protocol_v2::RATE_LIMITED);
            assert_eq!(info.retry_after_ms, 1_000);
        }
        other => panic!("expected Pair error, got {other:?}"),
    }
}

#[tokio::test]
async fn unsolicited_pair_error_for_unknown_pairing_is_dropped_safely() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    fake.send(
        1,
        CtrlMessage::PairError {
            request_id: [0xF0; 16],
            scope: ErrorScope::Pairing,
            object_id: pid(0xF1),
            code: ak_signal::protocol_v2::PAIR_UNAVAILABLE,
            retry_after_ms: 0,
        },
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(client.dropped_unmatched_frames(), 1);
    assert!(
        client.is_connected(),
        "dispatch error must not kill the connection"
    );

    // The client is still fully usable.
    let pong = {
        let c = client.clone();
        tokio::spawn(async move { c.ping().await })
    };
    let (conn, message) = fake.next_received().await;
    assert!(matches!(message, CtrlMessage::Ping { .. }));
    if let CtrlMessage::Ping { nonce } = message {
        fake.send(conn, CtrlMessage::Pong { nonce });
    }
    pong.await.unwrap().expect("ping after unmatched frame");
}

// ---- push streams ---------------------------------------------------------

#[tokio::test]
async fn pairing_inbox_receives_offer_and_commit() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let pairing = pid(0x0A);
    let mut inbox = client.subscribe_pairing(pairing).unwrap();

    fake.send(
        1,
        CtrlMessage::InviteRedeemOffered {
            code_id: [0x0B; 16],
            code_revision: 2,
            pairing_id: pairing,
            redeemer_device_id: [0x0C; 32],
            redeem: RedeemRequest {
                request_id: [0x0D; 16],
                lookup_digest: rng32(0x0E),
                redeemer_nonce: rng32(0x0F),
                deadline_ms: u64::MAX,
                request_mac: rng32(0x10),
                signature: [0x11; 64],
            },
        },
    );
    // PairCommit carries only the transcript; the pairing id sits at byte
    // 56 of the frozen layout and the dispatcher routes by it.
    let mut transcript: Transcript256 = [0; 256];
    transcript[56..72].copy_from_slice(&pairing);
    fake.send(
        1,
        CtrlMessage::PairCommit {
            transcript,
            owner_proof: PairProof {
                mac: rng32(1),
                signature: [1; 64],
            },
            redeemer_proof: PairProof {
                mac: rng32(2),
                signature: [2; 64],
            },
            owner_prepared_signature: [3; 64],
            redeemer_prepared_signature: [4; 64],
            commit_revision: 9,
        },
    );

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap(),
        CtrlMessage::InviteRedeemOffered { .. }
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap(),
        CtrlMessage::PairCommit { .. }
    ));
}

#[tokio::test]
async fn session_routing_matches_only_the_opened_session() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let connection_id = [0x5A; 16];
    let session: SessionId = [0x5B; 16];
    let task = {
        let c = client.clone();
        tokio::spawn(async move {
            c.connect_peer([0x5C; 32], pid(0x5D), [0x5E; 16], connection_id)
                .await
        })
    };
    let (conn, message) = fake.next_received().await;
    match message {
        CtrlMessage::ConnectRequest {
            connection_id: got, ..
        } => {
            assert_eq!(got, connection_id);
            fake.send(
                conn,
                CtrlMessage::SessionOpened {
                    session,
                    peer: [0x5C; 32],
                    pairing_id: pid(0x5D),
                    code_id: [0x5E; 16],
                    connection_id,
                    expires_ms: now_ms() + 120_000,
                },
            );
        }
        other => panic!("{other:?}"),
    }
    let opened = task.await.unwrap().expect("session opened");
    assert_eq!(opened.session, session);
    assert_eq!(opened.connection_id, connection_id);

    let mut inbox = client.subscribe_session(session).unwrap();
    fake.send(
        conn,
        CtrlMessage::Signal {
            session,
            signal: PeerSignal::Bye,
        },
    );
    // A signal for an unknown session: unmatched, never delivered above.
    fake.send(
        conn,
        CtrlMessage::Signal {
            session: [0x77; 16],
            signal: PeerSignal::Bye,
        },
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap(),
        CtrlMessage::Signal { .. }
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(client.dropped_unmatched_frames(), 1);
}

#[tokio::test]
async fn events_inbox_receives_unkeyed_server_error() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;
    let mut events = client.subscribe_events();

    fake.send(
        1,
        CtrlMessage::ServerError {
            code: 3,
            message: "frame rate exceeded".into(),
        },
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap(),
        CtrlMessage::ServerError { .. }
    ));
}

// ---- remaining request surface -------------------------------------------

#[tokio::test]
async fn invite_list_revoke_and_status_roundtrip() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    // list
    let list_task = {
        let c = client.clone();
        tokio::spawn(async move { c.invite_list(CURSOR_START, 2, StateFilter::ACTIVE).await })
    };
    let (conn, message) = fake.next_received().await;
    match message {
        CtrlMessage::InviteList {
            request_id,
            cursor,
            limit,
            ..
        } => {
            assert_eq!(cursor, CURSOR_START);
            assert_eq!(limit, 2);
            fake.send(
                conn,
                CtrlMessage::InviteListPage {
                    request_id,
                    next_cursor: [0x21; 16],
                    entries: vec![
                        InviteSummary {
                            code_id: [1; 16],
                            state: InviteState::Active,
                            created_ms: 1,
                            expires_ms: 0,
                            revision: 1,
                        },
                        InviteSummary {
                            code_id: [2; 16],
                            state: InviteState::Active,
                            created_ms: 2,
                            expires_ms: 0,
                            revision: 1,
                        },
                    ],
                },
            );
        }
        other => panic!("{other:?}"),
    }
    let page = list_task.await.unwrap().expect("list ok");
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.next_cursor, [0x21; 16]);

    // revoke
    let revoke_task = {
        let c = client.clone();
        tokio::spawn(async move { c.invite_revoke([1; 16], 1).await })
    };
    let (conn, message) = fake.next_received().await;
    match message {
        CtrlMessage::InviteRevoke {
            request_id,
            code_id,
            expected_revision,
        } => {
            assert_eq!(code_id, [1; 16]);
            assert_eq!(expected_revision, 1);
            fake.send(
                conn,
                CtrlMessage::InviteRevoked {
                    request_id,
                    code_id,
                    revision: 2,
                    revoked_ms: 99,
                },
            );
        }
        other => panic!("{other:?}"),
    }
    let revoked = revoke_task.await.unwrap().expect("revoke ok");
    assert_eq!(revoked.revision, 2);

    // pair status
    let status_task = {
        let c = client.clone();
        tokio::spawn(async move { c.pair_status_query(pid(0x31)).await })
    };
    let (conn, message) = fake.next_received().await;
    match message {
        CtrlMessage::PairStatusQuery {
            request_id,
            pairing_id,
        } => {
            assert_eq!(pairing_id, pid(0x31));
            fake.send(
                conn,
                CtrlMessage::PairStatusReply {
                    request_id,
                    pairing_id,
                    state: PairingState::Committed,
                    commit_revision: 5,
                    deadline_ms: 1_234,
                },
            );
        }
        other => panic!("{other:?}"),
    }
    let status = status_task.await.unwrap().expect("status ok");
    assert_eq!(status.state, PairingState::Committed);
    assert_eq!(status.commit_revision, 5);
}

#[tokio::test]
async fn invite_redeem_returns_pending_with_pairing_id() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let redeem = RedeemRequest {
        request_id: [0x61; 16],
        lookup_digest: rng32(0x62),
        redeemer_nonce: rng32(0x63),
        deadline_ms: now_ms() + 120_000,
        request_mac: rng32(0x64),
        signature: [0x65; 64],
    };
    let task = {
        let c = client.clone();
        let redeem = redeem.clone();
        tokio::spawn(async move { c.invite_redeem(redeem).await })
    };
    let (conn, message) = fake.next_received().await;
    match message {
        CtrlMessage::InviteRedeem(got) => {
            assert_eq!(got, redeem);
            fake.send(
                conn,
                CtrlMessage::InviteRedeemPending {
                    request_id: redeem.request_id,
                    pairing_id: pid(0x66),
                    deadline_ms: now_ms() + 120_000,
                },
            );
        }
        other => panic!("{other:?}"),
    }
    let ack = task.await.unwrap().expect("redeem pending ok");
    assert_eq!(ack.pairing_id, pid(0x66));
    assert_eq!(ack.request_id, redeem.request_id);
}

#[tokio::test]
async fn pair_abort_and_transcript_sends_are_fire_and_forget() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    client
        .pair_abort(pid(0x71), AbortReason::User)
        .await
        .expect("abort send ok");
    client
        .send_pair_prepared(pid(0x71), rng32(0x72), [0x73; 64])
        .await
        .expect("prepared send ok");
    client
        .send_pair_applied(pid(0x71), rng32(0x72), 4, [0x74; 64])
        .await
        .expect("applied send ok");
    client
        .send_pair_challenge(
            [0x75; 256],
            PairProof {
                mac: rng32(1),
                signature: [2; 64],
            },
        )
        .await
        .expect("challenge send ok");
    client
        .send_pair_proof(
            pid(0x71),
            rng32(0x72),
            PairProof {
                mac: rng32(3),
                signature: [4; 64],
            },
        )
        .await
        .expect("proof send ok");

    for expected in 0..5 {
        let (_conn, message) = fake.next_received().await;
        assert!(
            matches!(
                message,
                CtrlMessage::PairAbort { .. }
                    | CtrlMessage::PairPrepared { .. }
                    | CtrlMessage::PairApplied { .. }
                    | CtrlMessage::PairChallenge { .. }
                    | CtrlMessage::PairProof { .. }
            ),
            "message {expected}: {message:?}"
        );
    }
}

#[tokio::test]
async fn ping_pong_roundtrip() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let task = {
        let c = client.clone();
        tokio::spawn(async move { c.ping().await })
    };
    let (conn, message) = fake.next_received().await;
    if let CtrlMessage::Ping { nonce } = message {
        fake.send(conn, CtrlMessage::Pong { nonce });
    } else {
        panic!("{message:?}");
    }
    task.await.unwrap().expect("pong ok");
}

#[tokio::test]
async fn register_and_cancel_session_sends() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    let addr: SocketAddr = "192.0.2.1:5000".parse().unwrap();
    client.register(&[addr]).await.expect("register send ok");
    client
        .cancel_session([0x81; 16])
        .await
        .expect("cancel send ok");
    let (_conn, m1) = fake.next_received().await;
    assert!(matches!(m1, CtrlMessage::Register { .. }));
    let (_conn, m2) = fake.next_received().await;
    assert!(matches!(m2, CtrlMessage::Cancel { .. }));
}

// ---- lifecycle: reconnect, replacement, backoff ---------------------------

#[tokio::test]
async fn reconnect_fails_waiters_and_resets_request_ids() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;

    // One successful claim to observe a request id.
    let id1 = claim_once(&mut fake, &client, 0x91).await;

    // Kill the connection while a second claim is in flight.
    let in_flight = {
        let c = client.clone();
        tokio::spawn(async move { c.invite_claim(rng32(0x92), 0).await })
    };
    let (conn, _m) = fake.next_received().await;
    assert_eq!(conn, 1);
    fake.kill(conn);
    match in_flight.await.unwrap() {
        Err(ClientV2Error::ConnectionLost) | Err(ClientV2Error::Replaced) => {}
        other => panic!("in-flight claim should fail on disconnect, got {other:?}"),
    }
    assert_eq!(client.epoch(), 1);

    // Reconnect (the fake listener still accepts) and claim again.
    client.reconnect().await.expect("reconnect ok");
    assert!(client.is_connected());
    assert!(client.epoch() >= 2);
    assert!(matches!(
        *client.state_watcher().borrow(),
        ConnState::Connected(_)
    ));

    let id2 = claim_once(&mut fake, &client, 0x93).await;
    assert_ne!(id1, id2, "request-id space resets across reconnects");

    // A stale response for the OLD request id can never match anything.
    let conn_now = *fake.live_conns().iter().max().unwrap();
    let before_unmatched = client.dropped_unmatched_frames();
    fake.send(
        conn_now,
        CtrlMessage::InviteClaimed {
            request_id: id1,
            code_id: [9; 16],
            created_ms: 1,
            expires_ms: 0,
            revision: 1,
        },
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(client.dropped_unmatched_frames() > before_unmatched);
}

#[tokio::test]
async fn pairing_subscription_survives_reconnect() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;
    let pairing = pid(0x95);
    let mut inbox = client.subscribe_pairing(pairing).unwrap();

    fake.kill(1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client.is_connected());
    client.reconnect().await.expect("reconnect ok");

    let conn = *fake.live_conns().iter().max().unwrap();
    fake.send(
        conn,
        CtrlMessage::PairReady {
            pairing_id: pairing,
            transcript_digest: rng32(0x96),
            commit_revision: 1,
            owner_applied_signature: [1; 64],
            redeemer_applied_signature: [2; 64],
        },
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap(),
        CtrlMessage::PairReady { .. }
    ));
}

#[tokio::test]
async fn replaced_connection_is_surfaced_not_silent() {
    let mut fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let old = connect(&fake, &key).await;

    // A second connection of the same device id: the server tells the old
    // one it was replaced, then drops it (v1 replacement semantics, v2
    // notice). This is exactly what NOT sharing one instance causes.
    let new = connect(&fake, &key).await;
    assert_eq!(fake.connection_count(), 2);

    let mut replaced = old.replaced_watcher();
    fake.send(
        1,
        CtrlMessage::ServerError {
            code: ERR_DEVICE_REPLACED,
            message: "superseded by a newer connection".into(),
        },
    );
    // Let the replaced notice drain before the abrupt drop.
    tokio::time::sleep(Duration::from_millis(200)).await;
    fake.kill(1);
    tokio::time::timeout(Duration::from_secs(5), replaced.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*replaced.borrow());
    assert!(old.is_replaced());

    // Let the reader notice the EOF before new work on the old handle.
    tokio::time::sleep(Duration::from_millis(200)).await;
    match old.invite_claim(rng32(0x97), 0).await {
        Err(ClientV2Error::Replaced)
        | Err(ClientV2Error::ConnectionLost)
        | Err(ClientV2Error::Closed) => {}
        other => panic!("expected Replaced/ConnectionLost, got {other:?}"),
    }
    // The flag is sticky and the new connection is unaffected.
    assert!(old.is_replaced());
    let pong = {
        let c = new.clone();
        tokio::spawn(async move { c.ping().await })
    };
    let (conn, message) = fake.next_received().await;
    if let CtrlMessage::Ping { nonce } = message {
        fake.send(conn, CtrlMessage::Pong { nonce });
    }
    pong.await.unwrap().expect("new connection still healthy");
}

#[tokio::test]
async fn close_is_idempotent_and_stops_sending() {
    let fake = FakeRegistry::start().await;
    let key = DeviceKeyPair::generate();
    let client = connect(&fake, &key).await;
    client.close().await;
    client.close().await; // second close is a no-op
    assert!(!client.is_connected());
    assert!(matches!(
        *client.state_watcher().borrow(),
        ConnState::Closed
    ));
    assert!(matches!(
        client.ping().await,
        Err(ClientV2Error::Closed | ClientV2Error::ConnectionLost)
    ));
}

#[test]
fn backoff_sequence_is_one_two_four_capped_at_thirty_seconds() {
    let mut backoff = Backoff::default();
    let mut delays = Vec::new();
    for _ in 0..8 {
        delays.push(backoff.next_delay());
    }
    let expected = [
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(16),
        Duration::from_secs(30),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ];
    assert_eq!(delays, expected);
    backoff.reset();
    assert_eq!(backoff.next_delay(), Duration::from_secs(1));
}
