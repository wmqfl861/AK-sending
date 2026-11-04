//! Injectable runtime clock.
//!
//! The scheduler actor and the task drivers never read a wall clock
//! directly: they receive an [`RuntimeClock`] through
//! [`RuntimeConfig`](crate::runtime::RuntimeConfig). Tests inject
//! [`FakeClock`] so backoff deadlines, phase timing and shutdown timeouts
//! are driven by explicit `advance_ms` calls (contract §12: concurrency
//! semantics are proven with activity intervals measured on a controlled
//! clock, not by sleeping real time). [`RealClock`] is the production
//! implementation used from wave 21 on.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// A clock sleep future returned by [`RuntimeClock::sleep_until`].
pub type ClockSleep<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Time source for the runtime (millisecond domain).
///
/// `now_ms` must be monotonic within one runtime instance and
/// `sleep_until(deadline_ms)` must resolve once `now_ms() >= deadline_ms`.
pub trait RuntimeClock: Send + Sync {
    /// Current runtime time in milliseconds.
    fn now_ms(&self) -> u64;

    /// Resolve once the clock reaches `deadline_ms` (immediately when it
    /// already has).
    fn sleep_until<'a>(&'a self, deadline_ms: u64) -> ClockSleep<'a>;
}

/// Deterministic manually-advanced clock.
///
/// `advance_ms` publishes the new time on a `watch` channel, so every
/// sleeper re-checks the shared value before parking — there is no
/// lost-wakeup window between the deadline check and the wait.
#[derive(Debug)]
pub struct FakeClock {
    time_tx: watch::Sender<u64>,
    own_rx: watch::Receiver<u64>,
}

impl FakeClock {
    /// Create a clock reading `start_ms`.
    pub fn new(start_ms: u64) -> Self {
        let (time_tx, own_rx) = watch::channel(start_ms);
        FakeClock { time_tx, own_rx }
    }

    /// Current fake time.
    pub fn now_ms(&self) -> u64 {
        *self.own_rx.borrow()
    }

    /// Move fake time forward by `by` milliseconds, waking all sleepers
    /// whose deadline has been reached.
    pub fn advance_ms(&self, by: u64) {
        let next = self.now_ms().saturating_add(by);
        let _ = self.time_tx.send(next);
    }
}

impl RuntimeClock for FakeClock {
    fn now_ms(&self) -> u64 {
        FakeClock::now_ms(self)
    }

    fn sleep_until<'a>(&'a self, deadline_ms: u64) -> ClockSleep<'a> {
        Box::pin(async move {
            let mut rx = self.time_tx.subscribe();
            loop {
                if *rx.borrow_and_update() >= deadline_ms {
                    return;
                }
                if rx.changed().await.is_err() {
                    // The clock is only dropped together with the runtime.
                    return;
                }
            }
        })
    }
}

/// Wall-clock implementation backed by `SystemTime` + `tokio::time`.
#[derive(Debug)]
pub struct RealClock {
    epoch: Instant,
    base_ms: u64,
}

impl RealClock {
    /// Capture the current wall time as the clock origin.
    pub fn new() -> Self {
        RealClock {
            epoch: Instant::now(),
            base_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// Wrap in an `Arc<dyn RuntimeClock>` ready for
    /// [`RuntimeConfig`](crate::runtime::RuntimeConfig).
    pub fn shared() -> Arc<dyn RuntimeClock> {
        Arc::new(RealClock::new())
    }
}

impl Default for RealClock {
    fn default() -> Self {
        RealClock::new()
    }
}

impl RuntimeClock for RealClock {
    fn now_ms(&self) -> u64 {
        self.base_ms + u64::from(self.epoch.elapsed().as_millis() as u32)
    }

    fn sleep_until<'a>(&'a self, deadline_ms: u64) -> ClockSleep<'a> {
        Box::pin(async move {
            let now = self.now_ms();
            if deadline_ms > now {
                tokio::time::sleep(Duration::from_millis(deadline_ms - now)).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_clock_sleep_until_past_deadline_returns_immediately() {
        let clock = FakeClock::new(500);
        let started = std::time::Instant::now();
        clock.sleep_until(100).await;
        clock.sleep_until(500).await;
        assert!(started.elapsed() < Duration::from_millis(50));
        assert_eq!(clock.now_ms(), 500);
    }

    #[tokio::test]
    async fn fake_clock_advance_wakes_sleeper_and_missed_wakeups_resolve() {
        let clock = Arc::new(FakeClock::new(0));
        // Sleeper registered after the advance must still observe the value.
        clock.advance_ms(300);
        let sleeper = {
            let clock = Arc::clone(&clock);
            tokio::spawn(async move {
                clock.sleep_until(250).await;
                250
            })
        };
        let done = tokio::time::timeout(Duration::from_millis(200), sleeper)
            .await
            .expect("sleep resolves from the shared value")
            .expect("task join");
        assert_eq!(done, 250);
        assert_eq!(clock.now_ms(), 300);
    }

    #[tokio::test]
    async fn fake_clock_concurrent_sleepers_wake_on_shared_deadline() {
        let clock = Arc::new(FakeClock::new(0));
        let a = {
            let clock = Arc::clone(&clock);
            tokio::spawn(async move {
                clock.sleep_until(100).await;
                100
            })
        };
        let b = {
            let clock = Arc::clone(&clock);
            tokio::spawn(async move {
                clock.sleep_until(90).await;
                90
            })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        clock.advance_ms(100);
        let (ra, rb) = tokio::join!(a, b);
        assert_eq!((ra.unwrap(), rb.unwrap()), (100, 90));
    }
}
