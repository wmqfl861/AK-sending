//! Tunables for the v2 control-plane client, plus the frozen reconnect
//! backoff sequence.

use std::time::Duration;

/// Default per-waiter / per-subscriber inbox capacity.
///
/// Frozen by the client-dispatch contract (`docs/contracts/pairing-flow.md`
/// §11: "客户端请求 map / 收件箱有界"): every waiter and event subscription
/// gets its own bounded channel of this size; the single reader never blocks
/// on a slow consumer — a full inbox counts and drops the frame instead
/// (the v1 shared unbounded inbox defect is what this replaces).
pub const DEFAULT_INBOX_CAPACITY: usize = 64;

/// Default outbound frame queue feeding the TLS writer task.
pub const DEFAULT_OUTBOX_CAPACITY: usize = 128;

/// Reconnect backoff: 1s, 2s, 4s, 8s, 16s, then capped at 30s.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Exponential reconnect backoff: 1s / 2s / 4s doubling, capped at 30s.
///
/// ```
/// use ak_signal::client_v2::Backoff;
/// use std::time::Duration;
/// let mut b = Backoff::default();
/// assert_eq!(b.next_delay(), Duration::from_secs(1));
/// assert_eq!(b.next_delay(), Duration::from_secs(2));
/// assert_eq!(b.next_delay(), Duration::from_secs(4));
/// ```
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
    cap: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            next: BACKOFF_FIRST,
            cap: BACKOFF_CAP,
        }
    }
}

impl Backoff {
    /// Backoff with a custom first delay and cap (tests, tight budgets).
    pub fn new(first: Duration, cap: Duration) -> Self {
        Backoff {
            next: first.max(Duration::ZERO),
            cap: cap.max(first),
        }
    }

    /// The delay to wait before the next reconnect attempt; doubles each
    /// call up to the cap.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.cap);
        delay
    }

    /// Restart the sequence (after a connection has been healthy).
    pub fn reset(&mut self) {
        self.next = BACKOFF_FIRST.min(self.cap);
    }
}

/// Client tunables. All timeouts are per logical operation; request retries
/// reuse the same `request_id` (idempotency contract,
/// `docs/contracts/pairing-flow.md` §5).
#[derive(Debug, Clone)]
pub struct ClientV2Config {
    /// TCP + TLS + AuthChallenge/Hello/Welcome budget for one connect
    /// attempt. Default 10 s.
    pub connect_timeout: Duration,
    /// How long one request attempt waits for its keyed response before the
    /// client re-sends the frame (same request id). Default 10 s.
    pub response_timeout: Duration,
    /// Re-sends with the same request id after a response timeout before
    /// giving up (total attempts = 1 + retries). Default 2.
    pub request_retries: u32,
    /// Connection attempts inside one [`super::ClientV2::reconnect`] call,
    /// separated by the [`Backoff`] sequence. Default 3.
    pub reconnect_attempts: u32,
    /// Bounded capacity of every waiter / subscriber inbox. Default 64
    /// (contract-frozen).
    pub inbox_capacity: usize,
    /// Bounded capacity of the outbound frame queue. Default 128.
    pub outbox_capacity: usize,
}

impl Default for ClientV2Config {
    fn default() -> Self {
        ClientV2Config {
            connect_timeout: Duration::from_secs(10),
            response_timeout: Duration::from_secs(10),
            request_retries: 2,
            reconnect_attempts: 3,
            inbox_capacity: DEFAULT_INBOX_CAPACITY,
            outbox_capacity: DEFAULT_OUTBOX_CAPACITY,
        }
    }
}
