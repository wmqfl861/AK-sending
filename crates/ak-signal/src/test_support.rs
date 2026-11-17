//! Test-only I/O helpers shared by the v2 framing and client tests.
//!
//! L-N3 (review 2026-11-17): a TLS 1.3 pair built from the production
//! configs ([`crate::server_v2::tls`]) over an in-memory duplex whose
//! transports can each be made unwritable on demand. That is exactly the
//! condition under which tokio-rustls reports a `write_all` complete while
//! the encrypted record still waits inside the TLS session — the condition
//! the frame writers must survive by flushing.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use tokio::io::{duplex, AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

/// The write side of one transport: while closed, `poll_write` and
/// `poll_flush` are `Pending` with the waker kept — what a TCP socket with
/// a momentarily full send buffer reports. Reads are never gated.
#[derive(Default)]
pub(crate) struct WriteGate {
    closed: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl WriteGate {
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn open(&self) {
        self.closed.store(false, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }

    fn poll_open(&self, cx: &mut Context<'_>) -> Poll<()> {
        if !self.closed.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *self.waker.lock().unwrap() = Some(cx.waker().clone());
        // Re-check after registering: an `open` racing the store above
        // must not leave this poll parked forever.
        if self.closed.load(Ordering::SeqCst) {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

/// One end of the in-memory duplex with its write side behind a
/// [`WriteGate`].
pub(crate) struct GatedStream {
    inner: DuplexStream,
    gate: Arc<WriteGate>,
}

impl AsyncRead for GatedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for GatedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        std::task::ready!(self.gate.poll_open(cx));
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        std::task::ready!(self.gate.poll_open(cx));
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A completed, pinned TLS 1.3 handshake (ALPN `ak-signal/2`) over the
/// gated duplex. Both gates start open.
pub(crate) struct GatedTlsPair {
    pub(crate) client: tokio_rustls::client::TlsStream<GatedStream>,
    pub(crate) client_gate: Arc<WriteGate>,
    pub(crate) server: tokio_rustls::server::TlsStream<GatedStream>,
    pub(crate) server_gate: Arc<WriteGate>,
}

pub(crate) async fn gated_tls_pair() -> GatedTlsPair {
    let registry = ak_identity::DeviceKeyPair::generate();
    let bundle = ak_identity::tls::make_self_signed(&registry);
    let acceptor = crate::server_v2::tls::server_config(&bundle).expect("server config");
    let connector =
        crate::server_v2::tls::client_config(&registry.public_key()).expect("client config");
    let (client_io, server_io) = duplex(64 * 1024);
    let client_gate = Arc::new(WriteGate::default());
    let server_gate = Arc::new(WriteGate::default());
    let client_io = GatedStream {
        inner: client_io,
        gate: Arc::clone(&client_gate),
    };
    let server_io = GatedStream {
        inner: server_io,
        gate: Arc::clone(&server_gate),
    };
    let accept = tokio::spawn(async move { acceptor.accept(server_io).await });
    let client = connector
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").expect("server name"),
            client_io,
        )
        .await
        .expect("tls handshake");
    let server = accept.await.expect("accept task").expect("tls accept");
    GatedTlsPair {
        client,
        client_gate,
        server,
        server_gate,
    }
}
