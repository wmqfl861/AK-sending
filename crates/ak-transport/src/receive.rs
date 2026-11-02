use crate::{AuthenticatedDirectConnection, Error, Result};
use ak_core::job::State;
use ak_core::receive::{ConsentRequest, ReceiveError, ReceiveJob};
use std::sync::Arc;
use std::time::Duration;

/// Opaque local grant bound to one session, connection and approval generation.
/// Not serializable or cloneable. Every use must be checked against its session.
pub struct ReceiveGrant {
    scope: Arc<()>,
    connection_epoch: Arc<()>,
}

/// Transport-bound receiver authorization. The inner domain job is never exposed.
/// Future file I/O must additionally fence in-flight operations against revocation;
/// this module performs no disk writes and does not implement that future fence.
pub struct ReceiveSession {
    connection: Option<AuthenticatedDirectConnection>,
    peer_id: [u8; 32],
    job: ReceiveJob,
    grant_scope: Option<Arc<()>>,
}

impl ReceiveSession {
    pub(crate) fn new(connection: AuthenticatedDirectConnection, task: [u8; 16]) -> Result<Self> {
        connection.ensure_live()?;
        let peer_id = connection.peer_id();
        let mut job = ReceiveJob::new(task, peer_id);
        job.begin()?;
        job.direct_ready(peer_id)?;
        Ok(Self { connection: Some(connection), peer_id, job, grant_scope: None })
    }

    /// Domain state; network loss is observed on checked operations/disconnect wait.
    pub fn state(&self) -> State {
        self.job.state()
    }

    pub fn consent_request(&mut self) -> Result<Option<ConsentRequest>> {
        self.ensure_live()?;
        Ok(self.job.consent_request())
    }

    /// Trusted receiver UI/policy only. Never dispatch a network message here.
    pub fn approve(&mut self, request: &ConsentRequest, lifetime: Duration) -> Result<ReceiveGrant> {
        self.ensure_live()?;
        if let Err(error) = self.job.approve(request, lifetime) {
            if error == ReceiveError::RequestExpired {
                self.clear_connection();
            }
            return Err(error.into());
        }
        let scope = Arc::new(());
        let epoch = self.connection.as_ref().ok_or(Error::Closed)?.epoch().clone();
        self.grant_scope = Some(scope.clone());
        Ok(ReceiveGrant { scope, connection_epoch: epoch })
    }

    pub fn check_receive(&mut self, grant: &ReceiveGrant) -> Result<()> {
        self.authorized(grant, ReceiveJob::check_transfer)
    }

    /// Trusted engine event, not evidence that this adapter has received any file.
    pub fn all_bytes_received(&mut self, grant: &ReceiveGrant) -> Result<()> {
        self.authorized(grant, ReceiveJob::all_bytes_received)
    }

    /// Trusted engine event; this adapter does not compute file digests.
    pub fn verification_passed(&mut self, grant: &ReceiveGrant) -> Result<()> {
        self.authorized(grant, ReceiveJob::verification_passed)
    }

    /// Call only after actual reliable save in a future file engine. No disk I/O here.
    pub fn save_committed(&mut self, grant: &ReceiveGrant) -> Result<()> {
        self.authorized(grant, ReceiveJob::save_committed)
    }

    pub fn reject(&mut self, request: &ConsentRequest) -> Result<()> {
        self.job.reject(request)?;
        self.clear_connection();
        Ok(())
    }
    pub fn pause(&mut self) -> Result<()> {
        self.job.pause()?;
        self.clear_connection();
        Ok(())
    }
    pub fn revoke(&mut self) -> Result<()> {
        self.job.revoke()?;
        self.clear_connection();
        Ok(())
    }
    pub fn cancel(&mut self) -> Result<()> {
        self.job.cancel()?;
        self.clear_connection();
        Ok(())
    }

    /// Only Paused/WaitingDirect tasks may resume; a fresh TLS connection is required.
    /// Identity replacement is refused before any domain state mutation.
    pub fn reconnect(&mut self, connection: AuthenticatedDirectConnection) -> Result<()> {
        connection.ensure_live()?;
        if connection.peer_id() != self.peer_id {
            return Err(Error::PeerMismatch);
        }
        self.job.resume()?;
        self.grant_scope = None;
        self.job.direct_ready(self.peer_id)?;
        self.connection = Some(connection);
        Ok(())
    }

    /// Observe actual closure and invalidate the domain authorization.
    pub async fn wait_for_disconnect(&mut self) -> Result<()> {
        self.connection.as_ref().ok_or(Error::Closed)?.wait_closed().await?;
        self.disconnected();
        Ok(())
    }

    fn clear_connection(&mut self) {
        self.grant_scope = None;
        // Dropping the owned wrapper closes the underlying QUIC connection.
        self.connection = None;
    }

    fn disconnected(&mut self) {
        self.clear_connection();
        // A terminal/paused state must not be reopened by a late disconnect event.
        let _ = self.job.connection_lost();
    }

    fn ensure_live(&mut self) -> Result<()> {
        let result = self.connection.as_ref().ok_or(Error::Closed)
            .and_then(AuthenticatedDirectConnection::ensure_live);
        if result.is_err() {
            self.disconnected();
        }
        result
    }

    fn authorized(
        &mut self,
        grant: &ReceiveGrant,
        operation: fn(&mut ReceiveJob) -> std::result::Result<(), ReceiveError>,
    ) -> Result<()> {
        self.ensure_live()?;
        let matches = self.grant_scope.as_ref()
            .is_some_and(|scope| Arc::ptr_eq(scope, &grant.scope))
            && self.connection.as_ref()
                .is_some_and(|connection| Arc::ptr_eq(connection.epoch(), &grant.connection_epoch));
        if !matches {
            return Err(Error::InvalidGrant);
        }
        let result = operation(&mut self.job);
        if result == Err(ReceiveError::PermissionExpired) || self.job.state().is_terminal() {
            self.clear_connection();
        }
        result.map_err(Error::from)
    }
}
