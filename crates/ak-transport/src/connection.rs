use crate::identity::quic_configs;
use crate::{Error, LocalIdentity, PairedPeer, ReceiveSession, Result, ALPN, OPERATION_TIMEOUT};
use rustls::pki_types::CertificateDer;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::time::timeout;

const PING: &[u8; 8] = b"AKPING01";
const PONG: &[u8; 8] = b"AKPONG01";

/// Provenance is supplied by trusted local coordination, not a network claim.
/// Only manually specified native-UDP targets are implemented in M0-02.
#[derive(Debug, Clone, Copy)]
pub enum Candidate {
    ManualDirect(SocketAddr),
    Relay(SocketAddr),
    Unknown,
}

impl Candidate {
    fn address(self) -> Result<SocketAddr> {
        match self {
            Self::ManualDirect(address) => {
                validate_address(address, false)?;
                Ok(address)
            }
            Self::Relay(_) | Self::Unknown => Err(Error::UntrustedPath),
        }
    }
}

fn validate_address(address: SocketAddr, bind: bool) -> Result<()> {
    if !bind && address.port() == 0 {
        return Err(Error::InvalidAddress);
    }
    let valid = match address.ip() {
        IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && ip.octets()[0] != 0
                && ip.octets()[0] < 240
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && ip.to_ipv4_mapped().is_none()
                && (!ip.is_unicast_link_local()
                    || matches!(address, SocketAddr::V6(a) if a.scope_id() != 0))
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidAddress)
    }
}

/// Owns a concrete native UDP endpoint, with one locally configured paired peer.
/// No user-provided sockets, resolver, proxy, endpoint rebinding or relay API.
/// Mutably borrowing connect/accept serializes handshakes on this endpoint.
pub struct DirectEndpoint {
    endpoint: quinn::Endpoint,
    peer: PairedPeer,
}

impl DirectEndpoint {
    pub fn bind(address: SocketAddr, identity: LocalIdentity, peer: PairedPeer) -> Result<Self> {
        validate_address(address, true)?;
        let (server, client) = quic_configs(&identity, &peer)?;
        let mut endpoint =
            quinn::Endpoint::server(server, address).map_err(|_| Error::Configuration)?;
        endpoint.set_default_client_config(client);
        Ok(Self { endpoint, peer })
    }

    /// Actual bound socket address, not a discovery assertion.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr().map_err(|_| Error::Closed)
    }

    /// Close every connection on this endpoint and allow QUIC closing packets
    /// to drain before the caller drops its runtime. Bounded, not a file ACK.
    /// Consumes the endpoint; cancelling the wait cannot reopen its connections.
    pub async fn shutdown(self) -> Result<()> {
        self.endpoint.close(0_u32.into(), b"endpoint shutdown");
        timeout(OPERATION_TIMEOUT, self.endpoint.wait_idle())
            .await
            .map_err(|_| Error::Timeout)
    }

    pub async fn connect(&mut self, candidate: Candidate) -> Result<AuthenticatedDirectConnection> {
        let address = candidate.address()?;
        let connecting = self
            .endpoint
            .connect(address, crate::SERVER_NAME)
            .map_err(|_| Error::Handshake)?;
        let connection = timeout(OPERATION_TIMEOUT, connecting)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Handshake)?;
        self.authenticate(connection, address)
    }

    /// The trusted caller must supply the expected remote UDP source address.
    /// An unexpected incoming connection is refused before TLS processing.
    pub async fn accept(&mut self, candidate: Candidate) -> Result<AuthenticatedDirectConnection> {
        let expected = candidate.address()?;
        let operation = async {
            let incoming = self.endpoint.accept().await.ok_or(Error::Closed)?;
            if incoming.remote_address() != expected {
                incoming.refuse();
                return Err(Error::UnexpectedAddress);
            }
            let connection = incoming.await.map_err(|_| Error::Handshake)?;
            self.authenticate(connection, expected)
        };
        timeout(OPERATION_TIMEOUT, operation)
            .await
            .map_err(|_| Error::Timeout)?
    }

    fn authenticate(
        &self,
        connection: quinn::Connection,
        expected: SocketAddr,
    ) -> Result<AuthenticatedDirectConnection> {
        let result = (|| {
            if connection.remote_address() != expected {
                return Err(Error::UnexpectedAddress);
            }
            let identity = connection.peer_identity().ok_or(Error::PeerMismatch)?;
            let certificates = identity
                .downcast::<Vec<CertificateDer<'static>>>()
                .map_err(|_| Error::PeerMismatch)?;
            if !self.peer.matches(&certificates) {
                return Err(Error::PeerMismatch);
            }
            let handshake = connection.handshake_data().ok_or(Error::Protocol)?;
            let handshake = handshake
                .downcast::<quinn::crypto::rustls::HandshakeData>()
                .map_err(|_| Error::Protocol)?;
            if handshake.protocol.as_deref() != Some(ALPN) {
                return Err(Error::Protocol);
            }
            if connection.close_reason().is_some() {
                return Err(Error::Closed);
            }
            self.local_addr()
        })();
        match result {
            Ok(local) => Ok(AuthenticatedDirectConnection {
                connection,
                _endpoint: self.endpoint.clone(),
                peer_id: self.peer.id(),
                remote: expected,
                local,
                epoch: Arc::new(()),
            }),
            Err(error) => {
                connection.close(1_u32.into(), b"authentication rejected");
                Err(error)
            }
        }
    }
}

/// Constructed only after full mutual TLS, exact leaf pin, ALPN and socket checks.
/// Not Clone, serializable or constructible from a domain Observation.
pub struct AuthenticatedDirectConnection {
    connection: quinn::Connection,
    // Keeps the native endpoint alive without exposing its rebind/configuration API.
    _endpoint: quinn::Endpoint,
    peer_id: [u8; 32],
    remote: SocketAddr,
    local: SocketAddr,
    epoch: Arc<()>,
}

impl AuthenticatedDirectConnection {
    pub fn peer_id(&self) -> [u8; 32] {
        self.peer_id
    }
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
    pub(crate) fn epoch(&self) -> &Arc<()> {
        &self.epoch
    }

    // Private protocol bridge; never exported to callers or UI.
    pub(crate) fn session_transport(&self) -> quinn::Connection {
        self.connection.clone()
    }

    pub(crate) fn session_binding(&self, task: &[u8; 16]) -> Result<[u8; 32]> {
        self.ensure_live()?;
        let mut binding = [0; 32];
        self.connection
            .export_keying_material(&mut binding, b"EXPORTER-AK-session-v1", task)
            .map_err(|_| Error::Protocol)?;
        Ok(binding)
    }

    /// A point-in-time check, NOT a grant to write files or a future I/O fence.
    pub fn ensure_live(&self) -> Result<()> {
        if self.connection.close_reason().is_some() {
            return Err(Error::Closed);
        }
        if self.connection.remote_address() != self.remote {
            self.close();
            return Err(Error::PathChanged);
        }
        Ok(())
    }

    pub fn close(&self) {
        self.connection
            .close(0_u32.into(), b"direct connection closed");
    }

    /// Bounded observation of peer/transport closure; does not send keep-alives.
    pub async fn wait_closed(&self) -> Result<()> {
        timeout(OPERATION_TIMEOUT, self.connection.closed())
            .await
            .map(|_| ())
            .map_err(|_| Error::Timeout)
    }

    pub fn into_receive_session(self, task: [u8; 16]) -> Result<ReceiveSession> {
        ReceiveSession::new(self, task)
    }

    /// Fixed protocol probe only: no caller-supplied bytes, metadata or file data.
    pub async fn probe(&mut self) -> Result<()> {
        self.run_probe(false).await
    }

    /// Responds to exactly one fixed protocol probe. Unknown/oversize data closes.
    pub async fn respond_to_probe(&mut self) -> Result<()> {
        self.run_probe(true).await
    }

    async fn run_probe(&mut self, responder: bool) -> Result<()> {
        self.ensure_live()?;
        let mut guard = ProbeGuard {
            connection: self,
            complete: false,
        };
        let operation = async {
            if responder {
                let (mut send, mut receive) = self
                    .connection
                    .accept_bi()
                    .await
                    .map_err(|_| Error::Closed)?;
                let request = receive
                    .read_to_end(PING.len())
                    .await
                    .map_err(|_| Error::Protocol)?;
                if request != PING {
                    return Err(Error::Protocol);
                }
                self.ensure_live()?;
                send.write_all(PONG).await.map_err(|_| Error::Closed)?;
                send.finish().map_err(|_| Error::Closed)?;
            } else {
                let (mut send, mut receive) =
                    self.connection.open_bi().await.map_err(|_| Error::Closed)?;
                send.write_all(PING).await.map_err(|_| Error::Closed)?;
                send.finish().map_err(|_| Error::Closed)?;
                let reply = receive
                    .read_to_end(PONG.len())
                    .await
                    .map_err(|_| Error::Protocol)?;
                if reply != PONG {
                    return Err(Error::Protocol);
                }
            }
            self.ensure_live()
        };
        let result = timeout(OPERATION_TIMEOUT, operation)
            .await
            .unwrap_or(Err(Error::Timeout));
        guard.complete = result.is_ok();
        result
    }
}

// The guard lives across every probe await. Dropping a polled future closes the
// borrowed connection too; dropping an unpolled future has no network effects.
struct ProbeGuard<'a> {
    connection: &'a AuthenticatedDirectConnection,
    complete: bool,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.connection.close();
        }
    }
}

impl Drop for AuthenticatedDirectConnection {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;
