use ak_transport::{
    Candidate, DirectEndpoint, LocalIdentity, PairedPeer, ReceivedBatch, SessionReceiver,
    SessionSender, MAX_BATCH_BYTES,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn run(cancel: Arc<AtomicBool>) -> i32 {
    if cancel.load(Ordering::SeqCst) {
        return super::CANCELLED;
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return super::FAILED;
    };
    runtime.block_on(async {
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(10), loopback()) => {
                if matches!(result, Ok(Ok(()))) { super::PASSED } else { super::FAILED }
            }
            () = async {
                while !cancel.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => super::CANCELLED,
        }
    })
}

async fn loopback() -> Result<(), ()> {
    // All inputs are synthetic and local. No discovery, arbitrary dial or relay.
    let a = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).map_err(|_| ())?;
    let b = rcgen::generate_simple_self_signed(vec!["ak-peer.invalid".into()]).map_err(|_| ())?;
    let aid = LocalIdentity::from_der(a.cert.der().to_vec(), a.signing_key.serialize_der())
        .map_err(|_| ())?;
    let bid = LocalIdentity::from_der(b.cert.der().to_vec(), b.signing_key.serialize_der())
        .map_err(|_| ())?;
    let ap = PairedPeer::from_certificate_der(a.cert.der().to_vec()).map_err(|_| ())?;
    let bp = PairedPeer::from_certificate_der(b.cert.der().to_vec()).map_err(|_| ())?;
    let local = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let mut a = DirectEndpoint::bind(local, aid, bp).map_err(|_| ())?;
    let mut b = DirectEndpoint::bind(local, bid, ap).map_err(|_| ())?;
    let aa = a.local_addr().map_err(|_| ())?;
    let ba = b.local_addr().map_err(|_| ())?;
    let (a, b) = tokio::join!(
        a.connect(Candidate::ManualDirect(ba)),
        b.accept(Candidate::ManualDirect(aa))
    );
    let (sender, receiver) = tokio::join!(
        SessionSender::open(a.map_err(|_| ())?, [42; 16]),
        SessionReceiver::accept(b.map_err(|_| ())?, [42; 16])
    );
    let mut sender = sender.map_err(|_| ())?;
    let mut receiver = receiver.map_err(|_| ())?;
    let request = receiver.consent_request().map_err(|_| ())?.ok_or(())?;
    // Only this synthetic self-test approves automatically, never production UI.
    let grant = receiver
        .approve(&request, Duration::from_secs(10))
        .map_err(|_| ())?;
    receiver.ready(&grant).await.map_err(|_| ())?;
    for data in [vec![0x42; 32], vec![0xA5; MAX_BATCH_BYTES]] {
        let (sent, received) =
            tokio::join!(sender.send_batch(&data), receiver.receive_batch(&grant));
        sent.map_err(|_| ())?;
        if received.map_err(|_| ())? != ReceivedBatch::Data(data) {
            return Err(());
        }
    }
    let (sent, received) = tokio::join!(sender.finish(), receiver.receive_batch(&grant));
    sent.map_err(|_| ())?;
    if received.map_err(|_| ())? != ReceivedBatch::Ended {
        return Err(());
    }
    Ok(())
}
