//! Direction legality, encoded at the type layer: every [`CtrlMessage`]
//! variant declares which way it may travel, and the frame layer refuses to
//! encode or decode a message moving the wrong way
//! ([`super::frame::CtrlFrame::try_encode_as`] /
//! [`super::frame::CtrlFrame::decode_as`]).
//!
//! The mapping is the frozen direction table of the contract (section 8,
//! roles taken from the plan A.5.3 sequence: A = owner, B = redeemer,
//! S = server; owner and redeemer are both `Role::Client` endpoints).
//! Notable freezes: `Cancel` is **client-to-server only** in v2 (unlike
//! v1, where the server also pushed Cancel notifications), and `Ping`/
//! `Pong` are strictly client→server / server→client. Adding a variant
//! without a direction is a compile error, and tests lock the whole table.

use super::message::CtrlMessage;
use super::peer_signal::PeerSignal;

/// Which endpoint of one control connection a frame is authored by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// A paired device (owner or redeemer).
    Client,
    /// The coordination service.
    Server,
}

impl Role {
    pub fn opposite(self) -> Self {
        match self {
            Role::Client => Role::Server,
            Role::Server => Role::Client,
        }
    }
}

/// Legal travel direction of one message type on the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Client sends, server receives.
    ClientToServer,
    /// Server sends, client receives.
    ServerToClient,
    /// Either endpoint may send it (e.g. verbatim relays, keepalives).
    Both,
}

impl Direction {
    /// True when `sender` is allowed to emit a message of this direction.
    pub fn allows_sender(self, sender: Role) -> bool {
        matches!(
            (self, sender),
            (Direction::ClientToServer, Role::Client)
                | (Direction::ServerToClient, Role::Server)
                | (Direction::Both, _)
        )
    }
}

impl PeerSignal {
    /// Legal travel direction of one signal payload. Candidates/PunchAt/Bye
    /// are authored by a client and relayed verbatim by the server, so both
    /// endpoints transmit them. `SameEgress` is **server-generated only**
    /// (contract A.4.5): a client must never emit it, so a server
    /// receiving one — or being asked to encode one — is refused.
    pub fn direction(&self) -> Direction {
        match self {
            PeerSignal::SameEgress => Direction::ServerToClient,
            PeerSignal::Candidates(_) | PeerSignal::PunchAt { .. } | PeerSignal::Bye => {
                Direction::Both
            }
        }
    }
}

impl CtrlMessage {
    /// Declared travel direction of this message type (envelope level).
    pub fn direction(&self) -> Direction {
        match self {
            CtrlMessage::Hello { .. } => Direction::ClientToServer,
            CtrlMessage::Welcome { .. } => Direction::ServerToClient,
            CtrlMessage::Register { .. } => Direction::ClientToServer,
            CtrlMessage::ConnectRequest { .. } => Direction::ClientToServer,
            CtrlMessage::SessionOpened { .. } => Direction::ServerToClient,
            CtrlMessage::Signal { .. } => Direction::Both,
            CtrlMessage::Cancel { .. } => Direction::ClientToServer,
            CtrlMessage::ServerError { .. } => Direction::ServerToClient,
            CtrlMessage::AuthChallenge { .. } => Direction::ServerToClient,
            CtrlMessage::InviteClaim { .. } => Direction::ClientToServer,
            CtrlMessage::InviteClaimed { .. } => Direction::ServerToClient,
            CtrlMessage::InviteList { .. } => Direction::ClientToServer,
            CtrlMessage::InviteListPage { .. } => Direction::ServerToClient,
            CtrlMessage::InviteRevoke { .. } => Direction::ClientToServer,
            CtrlMessage::InviteRevoked { .. } => Direction::ServerToClient,
            CtrlMessage::InviteRedeem(_) => Direction::ClientToServer,
            CtrlMessage::InviteRedeemPending { .. } => Direction::ServerToClient,
            CtrlMessage::InviteRedeemOffered { .. } => Direction::ServerToClient,
            CtrlMessage::PairChallenge { .. } => Direction::Both,
            CtrlMessage::PairProof { .. } => Direction::Both,
            CtrlMessage::PairPrepared { .. } => Direction::ClientToServer,
            CtrlMessage::PairCommit { .. } => Direction::ServerToClient,
            CtrlMessage::PairApplied { .. } => Direction::ClientToServer,
            CtrlMessage::PairReady { .. } => Direction::ServerToClient,
            CtrlMessage::PairStatusQuery { .. } => Direction::ClientToServer,
            CtrlMessage::PairStatusReply { .. } => Direction::ServerToClient,
            CtrlMessage::PairAbort { .. } => Direction::ClientToServer,
            CtrlMessage::PairError { .. } => Direction::ServerToClient,
            CtrlMessage::Ping { .. } => Direction::ClientToServer,
            CtrlMessage::Pong { .. } => Direction::ServerToClient,
        }
    }

    /// Refined sender check used by the direction-enforcing frame paths:
    /// the envelope direction, plus the `Signal(SameEgress)`
    /// server-origin refinement.
    pub fn allows_sender(&self, sender: Role) -> bool {
        match self {
            CtrlMessage::Signal { signal, .. } => signal.direction().allows_sender(sender),
            _ => self.direction().allows_sender(sender),
        }
    }
}
