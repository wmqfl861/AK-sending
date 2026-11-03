use super::*;
use ak_transport::{
    Candidate, DirectEndpoint, LocalIdentity, ReceivedBatch, SessionReceiver, SessionSender,
    MAX_BATCH_BYTES,
};
use tokio::time::{sleep, timeout, Instant};

pub(super) fn run(local: SocketAddr, state: Shared) -> i32 {
    run_with_limit(local, state, TOTAL_LIMIT)
}

pub(super) fn run_with_limit(local: SocketAddr, state: Shared, limit: Duration) -> i32 {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return FAILED;
    };
    runtime.block_on(async {
        tokio::select! {
            result = timeout(limit, session(local, &state)) => {
                match result { Ok(Ok(())) => PASSED, Ok(Err(code)) => code, Err(_) => EXPIRED }
            },
            () = async {
                loop {
                    if state.lock().map_or(true, |s| s.status < PREPARING) { return; }
                    sleep(Duration::from_millis(10)).await;
                }
            } => CANCELLED,
        }
    })
}

fn phase(state: &Shared, next: i32) -> Result<(), i32> {
    state.lock().map_err(|_| FAILED)?.phase(next)
}
async fn session(local: SocketAddr, state: &Shared) -> Result<(), i32> {
    let generated =
        rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).map_err(|_| FAILED)?;
    let certificate = generated.cert.der().to_vec();
    if certificate.len() > CERT_LIMIT {
        return Err(FAILED);
    }
    let identity =
        LocalIdentity::from_der(certificate.clone(), generated.signing_key.serialize_der())
            .map_err(|_| FAILED)?;
    let own_id = PairedPeer::from_certificate_der(certificate.clone())
        .map_err(|_| FAILED)?
        .id();
    {
        let mut s = state.lock().map_err(|_| FAILED)?;
        s.phase(READY)?;
        s.certificate = certificate;
        s.own_id = own_id;
    }
    let config = loop {
        {
            let mut s = state.lock().map_err(|_| FAILED)?;
            if s.status < PREPARING {
                return Err(s.status);
            }
            if s.status == START {
                // Copy public pairing only; the configuration remains frozen for UI.
                let c = s.config.as_ref().ok_or(FAILED)?;
                let result = (c.peer.clone(), c.address, c.sending);
                s.phase(CONNECTING)?;
                break result;
            }
        }
        sleep(Duration::from_millis(10)).await;
    };
    let (peer, remote, sending) = config;
    let receiver_id = if sending { peer.id() } else { own_id };
    let task: [u8; 16] = receiver_id[..16].try_into().map_err(|_| FAILED)?;
    let mut endpoint =
        DirectEndpoint::bind(local, identity, peer).map_err(|_| CONNECTION_FAILED)?;
    let connection = if sending {
        endpoint
            .connect(Candidate::ManualDirect(remote))
            .await
            .map_err(|_| CONNECTION_FAILED)?
    } else {
        phase(state, LISTENING)?;
        let limit = Instant::now() + Duration::from_secs(60);
        loop {
            match endpoint.accept(Candidate::ManualDirect(remote)).await {
                Ok(c) => break c,
                Err(ak_transport::Error::Timeout) if Instant::now() < limit => continue,
                Err(_) => return Err(CONNECTION_FAILED),
            }
        }
    };
    phase(state, ACTIVE)?;
    if sending {
        let mut sender = SessionSender::open(connection, task)
            .await
            .map_err(|_| CONNECTION_FAILED)?;
        for data in [vec![0x42; 32], vec![0xA5; MAX_BATCH_BYTES]] {
            phase(state, ACTIVE)?;
            sender
                .send_batch(&data)
                .await
                .map_err(|_| CONNECTION_FAILED)?;
        }
        sender.finish().await.map_err(|_| CONNECTION_FAILED)?;
    } else {
        let mut receiver = SessionReceiver::accept(connection, task)
            .await
            .map_err(|_| CONNECTION_FAILED)?;
        phase(state, ACTIVE)?;
        let request = receiver
            .consent_request()
            .map_err(|_| FAILED)?
            .ok_or(FAILED)?;
        // Captured one-shot LOCAL start intent, immutable peer and synthetic task.
        // No certificate from the wire and no network "approve" message is trusted.
        let grant = receiver
            .approve(&request, Duration::from_secs(30))
            .map_err(|_| FAILED)?;
        receiver
            .ready(&grant)
            .await
            .map_err(|_| CONNECTION_FAILED)?;
        for expected in [vec![0x42; 32], vec![0xA5; MAX_BATCH_BYTES]] {
            phase(state, ACTIVE)?;
            if receiver
                .receive_batch(&grant)
                .await
                .map_err(|_| CONNECTION_FAILED)?
                != ReceivedBatch::Data(expected)
            {
                return Err(CONTENT_FAILED);
            }
        }
        if receiver
            .receive_batch(&grant)
            .await
            .map_err(|_| CONNECTION_FAILED)?
            != ReceivedBatch::Ended
        {
            return Err(CONTENT_FAILED);
        }
    }
    phase(state, ACTIVE)?; // A concurrent cancellation must not report success.
                           // Do not substitute a fixed sleep for QUIC closing/draining.
    endpoint.shutdown().await.map_err(|_| CONNECTION_FAILED)?;
    Ok(())
}
