//! Bounded, ordered, connection-bound in-memory batches. No filesystem API.
//! An Ack means receipt of a protocol frame, NOT durable storage or file integrity.
//! Data already returned is a snapshot, not an ongoing authority to write files.
//! A future file engine must add its own revocation/commit fence.

mod codec;
#[cfg(test)]
mod tests;

use crate::{
    AuthenticatedDirectConnection, Error, ReceiveGrant, ReceiveSession, Result, OPERATION_TIMEOUT,
};
use ak_core::job::State;
use ak_core::receive::ConsentRequest;
pub use codec::MAX_BATCH_BYTES;
use codec::{Context, Header, Kind, HEADER_SIZE};
use std::time::Duration;
use tokio::time::timeout;

/// Can only abort the connection that created it, including a pending receive.
/// It cannot produce a grant, reconnect, or change the trusted peer.
#[derive(Clone)]
pub struct SessionAbortHandle {
    connection: quinn::Connection,
}
impl SessionAbortHandle {
    pub fn abort(&self) {
        self.connection.close(1_u32.into(), b"session aborted");
    }
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
}

// Owns a private clone so it survives all mutable borrows and every await.
// Dropping an in-flight future never leaves a partially consumed frame reusable.
struct Operation {
    abort: SessionAbortHandle,
    complete: bool,
}
impl Operation {
    fn new(abort: SessionAbortHandle) -> Self {
        Self {
            abort,
            complete: false,
        }
    }
    fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        self.complete = result.is_ok();
        result
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        if !self.complete {
            self.abort.abort();
        }
    }
}

struct Wire {
    send: quinn::SendStream,
    receive: quinn::RecvStream,
    context: Context,
}
impl Wire {
    async fn write(&mut self, kind: Kind, sequence: u64, bytes: &[u8]) -> Result<()> {
        let header = Header {
            kind,
            sequence,
            length: bytes.len(),
        }
        .encode(self.context)?;
        self.send
            .write_all(&header)
            .await
            .map_err(|_| Error::Closed)?;
        self.send.write_all(bytes).await.map_err(|_| Error::Closed)
    }

    async fn read(&mut self, allowed: &[Kind], sequence: u64) -> Result<(Kind, Vec<u8>)> {
        let mut bytes = [0; HEADER_SIZE];
        self.receive
            .read_exact(&mut bytes)
            .await
            .map_err(|_| Error::Protocol)?;
        let header = Header::decode(&bytes, self.context)?;
        if !allowed.contains(&header.kind) || header.sequence != sequence {
            return Err(Error::Protocol);
        }
        let mut body = vec![0; header.length];
        self.receive
            .read_exact(&mut body)
            .await
            .map_err(|_| Error::Protocol)?;
        Ok((header.kind, body))
    }
}

fn context(connection: &AuthenticatedDirectConnection, task: [u8; 16]) -> Result<Context> {
    Ok(Context {
        task,
        binding: connection.session_binding(&task)?,
    })
}

fn next_sequence(sequence: u64) -> Result<u64> {
    sequence.checked_add(1).ok_or(Error::Protocol)
}

/// Sender of bounded memory batches. One in-flight batch; no raw stream escape.
/// ```compile_fail
/// use ak_transport::SessionSender;
/// let forged = SessionSender {};
/// ```
pub struct SessionSender {
    connection: AuthenticatedDirectConnection,
    abort: SessionAbortHandle,
    wire: Wire,
    next: u64,
    ready: bool,
    ended: bool,
}

impl SessionSender {
    /// Sends only Hello; this does not wait for or imply receiving-side consent.
    pub async fn open(connection: AuthenticatedDirectConnection, task: [u8; 16]) -> Result<Self> {
        let context = context(&connection, task)?;
        let raw = connection.session_transport();
        let abort = SessionAbortHandle {
            connection: raw.clone(),
        };
        let guard = Operation::new(abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            let (send, receive) = raw.open_bi().await.map_err(|_| Error::Closed)?;
            let mut wire = Wire {
                send,
                receive,
                context,
            };
            wire.write(Kind::Hello, 0, &[]).await?;
            connection.ensure_live()?;
            Ok(wire)
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        let wire = guard.finish(result)?;
        Ok(Self {
            connection,
            abort,
            wire,
            next: 1,
            ready: false,
            ended: false,
        })
    }

    pub fn abort_handle(&self) -> SessionAbortHandle {
        self.abort.clone()
    }

    async fn await_ready(&mut self) -> Result<()> {
        self.connection.ensure_live()?;
        if self.ended {
            return Err(Error::Protocol);
        }
        if !self.ready {
            self.wire.read(&[Kind::Ready], 0).await?;
            self.connection.ensure_live()?;
            self.ready = true;
        }
        Ok(())
    }

    /// Waits for local approval at the other endpoint, then sends one batch.
    /// Receipt acknowledgement is not a durable-save acknowledgement.
    pub async fn send_batch(&mut self, bytes: &[u8]) -> Result<()> {
        let guard = Operation::new(self.abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            if bytes.is_empty() || bytes.len() > MAX_BATCH_BYTES {
                return Err(Error::Protocol);
            }
            let next = next_sequence(self.next)?;
            self.await_ready().await?;
            self.wire.write(Kind::Data, self.next, bytes).await?;
            self.wire.read(&[Kind::Ack], self.next).await?;
            self.connection.ensure_live()?;
            self.next = next;
            Ok(())
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        guard.finish(result)
    }

    /// Ends only this framed body. No file checksum/save claim is made.
    pub async fn finish(&mut self) -> Result<()> {
        let guard = Operation::new(self.abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            self.await_ready().await?;
            self.wire.write(Kind::End, self.next, &[]).await?;
            self.wire.send.finish().map_err(|_| Error::Closed)?;
            self.wire.read(&[Kind::EndAck], self.next).await?;
            self.wire
                .receive
                .read_to_end(0)
                .await
                .map_err(|_| Error::Protocol)?;
            self.connection.ensure_live()?;
            self.ended = true;
            Ok(())
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        guard.finish(result)
    }
}

/// Memory returned under a valid local grant; not a filesystem capability.
#[derive(Debug, PartialEq, Eq)]
pub enum ReceivedBatch {
    Data(Vec<u8>),
    Ended,
}

/// A framed stream plus a private, receiver-local authorization controller.
/// There is deliberately no reconnect method: reconnect requires a fresh Hello,
/// TLS binding, consent request and grant. This prevents stale stream reuse.
/// ```compile_fail
/// fn raw(receiver: &ak_transport::SessionReceiver) {
///     let stream = &receiver.wire.receive;
/// }
/// ```
pub struct SessionReceiver {
    authorization: ReceiveSession,
    abort: SessionAbortHandle,
    wire: Wire,
    next: u64,
    ready: bool,
    ended: bool,
}

impl SessionReceiver {
    /// The task is supplied by trusted local coordination, not inferred from a
    /// sender's claim of approval. Hello consumes only a bounded fixed header.
    pub async fn accept(connection: AuthenticatedDirectConnection, task: [u8; 16]) -> Result<Self> {
        let context = context(&connection, task)?;
        let raw = connection.session_transport();
        let abort = SessionAbortHandle {
            connection: raw.clone(),
        };
        let guard = Operation::new(abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            let (send, receive) = raw.accept_bi().await.map_err(|_| Error::Closed)?;
            let mut wire = Wire {
                send,
                receive,
                context,
            };
            wire.read(&[Kind::Hello], 0).await?;
            connection.ensure_live()?;
            Ok(wire)
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        let wire = guard.finish(result)?;
        let authorization = connection.into_receive_session(task)?;
        Ok(Self {
            authorization,
            abort,
            wire,
            next: 1,
            ready: false,
            ended: false,
        })
    }

    pub fn abort_handle(&self) -> SessionAbortHandle {
        self.abort.clone()
    }
    pub fn state(&self) -> State {
        self.authorization.state()
    }
    pub fn consent_request(&mut self) -> Result<Option<ConsentRequest>> {
        self.authorization.consent_request()
    }
    /// Trusted receiver UI/policy only. No wire type dispatches to this method.
    pub fn approve(
        &mut self,
        request: &ConsentRequest,
        lifetime: Duration,
    ) -> Result<ReceiveGrant> {
        self.authorization.approve(request, lifetime)
    }
    pub fn reject(&mut self, request: &ConsentRequest) -> Result<()> {
        self.authorization.reject(request)
    }
    pub fn pause(&mut self) -> Result<()> {
        self.authorization.pause()
    }
    pub fn revoke(&mut self) -> Result<()> {
        self.authorization.revoke()
    }
    pub fn cancel(&mut self) -> Result<()> {
        self.authorization.cancel()
    }

    /// Advertises readiness only after checking an actual local grant.
    pub async fn ready(&mut self, grant: &ReceiveGrant) -> Result<()> {
        let guard = Operation::new(self.abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            self.authorization.check_receive(grant)?;
            if self.ready || self.ended {
                return Err(Error::Protocol);
            }
            self.wire.write(Kind::Ready, 0, &[]).await?;
            self.authorization.check_receive(grant)?;
            self.ready = true;
            Ok(())
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        guard.finish(result)
    }

    /// No data is returned without checking the grant before and after awaits.
    /// An asynchronously aborted/failed operation closes the connection. The
    /// result is only memory: callers must not treat it as a future disk grant.
    pub async fn receive_batch(&mut self, grant: &ReceiveGrant) -> Result<ReceivedBatch> {
        let guard = Operation::new(self.abort.clone());
        let result = timeout(OPERATION_TIMEOUT, async {
            self.authorization.check_receive(grant)?;
            if !self.ready || self.ended {
                return Err(Error::Protocol);
            }
            let (kind, bytes) = self.wire.read(&[Kind::Data, Kind::End], self.next).await?;
            self.authorization.check_receive(grant)?;
            if kind == Kind::End {
                // Reject trailing bytes rather than silently accepting extra frames.
                self.wire
                    .receive
                    .read_to_end(0)
                    .await
                    .map_err(|_| Error::Protocol)?;
                self.authorization.check_receive(grant)?;
                self.wire.write(Kind::EndAck, self.next, &[]).await?;
                self.wire.send.finish().map_err(|_| Error::Closed)?;
                self.authorization.check_receive(grant)?;
                self.ended = true;
                Ok(ReceivedBatch::Ended)
            } else {
                let next = next_sequence(self.next)?;
                self.wire.write(Kind::Ack, self.next, &[]).await?;
                self.authorization.check_receive(grant)?;
                self.next = next;
                Ok(ReceivedBatch::Data(bytes))
            }
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        guard.finish(result)
    }
}
