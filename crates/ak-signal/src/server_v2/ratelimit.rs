//! Lazy rate limiters for the v2 plane: a token bucket for the per-frame
//! budget (same semantics as the v1 server) and a sliding-window counter
//! for the per-minute redeem budgets.
//!
//! Both take an `Instant` parameter on their decision methods so tests can
//! exercise window expiry without sleeping; production callers pass
//! `Instant::now()`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Lazy token bucket: no background task, refilled on demand.
#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    burst: f64,
    rate_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate_per_sec: u32, burst: u32) -> Self {
        TokenBucket {
            tokens: burst as f64,
            burst: burst as f64,
            rate_per_sec: rate_per_sec as f64,
            last: Instant::now(),
        }
    }

    /// Decision at `now`; false when the bucket is empty.
    pub fn allow_at(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.last = now;
            self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.burst);
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn allow(&mut self) -> bool {
        self.allow_at(Instant::now())
    }
}

/// Fixed-capacity sliding window over event timestamps: `allow_at` admits
/// an event when fewer than `limit` events fall inside `window` before
/// `now`, and records it. Memory is bounded by `limit` entries.
#[derive(Debug)]
pub struct SlidingWindow {
    events: VecDeque<Instant>,
    limit: u32,
    window: Duration,
}

impl SlidingWindow {
    pub fn new(limit: u32, window: Duration) -> Self {
        SlidingWindow {
            events: VecDeque::with_capacity(limit as usize),
            limit,
            window,
        }
    }

    /// Drop events that left the window as of `now`.
    fn retain_fresh(&mut self, now: Instant) {
        while let Some(&oldest) = self.events.front() {
            if now.duration_since(oldest) > self.window {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }

    /// Try to admit one event at `now`.
    pub fn allow_at(&mut self, now: Instant) -> bool {
        self.retain_fresh(now);
        if self.events.len() >= self.limit as usize {
            return false;
        }
        self.events.push_back(now);
        true
    }

    /// Events still inside the window as of `now`, without mutating (the
    /// events deque may still carry stale entries until the next
    /// `allow_at`; they are skipped, not dropped). CR-P2-10: the
    /// fair-share layers read a source's in-window tally to decide who is
    /// over their share without recording anything.
    pub fn count_in_window(&self, now: Instant) -> usize {
        self.events
            .iter()
            .rev()
            .take_while(|&&event| now.duration_since(event) <= self.window)
            .count()
    }

    /// Milliseconds until the oldest recorded event leaves the window
    /// (a retry-after hint for rejected callers). Zero when empty.
    pub fn retry_after_ms(&self, now: Instant) -> u32 {
        match self.events.front() {
            Some(&oldest) => {
                let elapsed = now.duration_since(oldest);
                let remain = self.window.saturating_sub(elapsed);
                remain.as_millis() as u32
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_the_burst_then_throttles_to_the_rate() {
        let mut bucket = TokenBucket::new(32, 64);
        let t0 = Instant::now();
        for _ in 0..64 {
            assert!(bucket.allow_at(t0), "burst of 64 admitted");
        }
        assert!(!bucket.allow_at(t0), "burst exhausted");
        // One second later exactly 32 tokens refilled (rate 32/s).
        assert!(bucket.allow_at(t0 + Duration::from_secs(1)));
        for _ in 0..31 {
            assert!(bucket.allow_at(t0 + Duration::from_secs(1)));
        }
        assert!(
            !bucket.allow_at(t0 + Duration::from_secs(1)),
            "refill is capped at the rate"
        );
    }

    #[test]
    fn bucket_never_exceeds_its_burst_after_long_idle() {
        let mut bucket = TokenBucket::new(1, 5);
        let t0 = Instant::now();
        assert!(bucket.allow_at(t0 + Duration::from_secs(1000)));
        for _ in 0..4 {
            assert!(bucket.allow_at(t0 + Duration::from_secs(1000)));
        }
        assert!(!bucket.allow_at(t0 + Duration::from_secs(1000)));
    }

    #[test]
    fn window_admits_the_limit_then_slides() {
        let mut window = SlidingWindow::new(10, Duration::from_secs(60));
        let t0 = Instant::now();
        for _ in 0..10 {
            assert!(window.allow_at(t0));
        }
        assert!(!window.allow_at(t0), "11th inside one minute rejected");
        assert!(window.retry_after_ms(t0) > 0);

        // All events age out after the window; admission resumes.
        let later = t0 + Duration::from_secs(61);
        assert!(window.allow_at(later));
        // The only recorded event is `later` itself: full window remains.
        assert_eq!(window.retry_after_ms(later), 60_000);
    }

    #[test]
    fn window_retry_hint_counts_down_to_window_edge() {
        let mut window = SlidingWindow::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        window.allow_at(t0);
        window.allow_at(t0);
        assert!(!window.allow_at(t0 + Duration::from_secs(10)));
        let hint = window.retry_after_ms(t0 + Duration::from_secs(10));
        // The event at t0 leaves at t0+60s: ~50s remain.
        assert!((49_000..=50_000).contains(&hint), "hint was {hint}");
    }

    #[test]
    fn window_counts_only_fresh_events_without_mutating() {
        let mut window = SlidingWindow::new(10, Duration::from_secs(60));
        let t0 = Instant::now();
        for _ in 0..3 {
            window.allow_at(t0);
        }
        assert_eq!(window.count_in_window(t0), 3);
        // Forty seconds later the same three are still fresh...
        let mid = t0 + Duration::from_secs(40);
        assert_eq!(window.count_in_window(mid), 3);
        // ...and after they expire the tally drops to zero even though
        // no `allow_at` has run to physically drop them.
        let later = t0 + Duration::from_secs(61);
        assert_eq!(window.count_in_window(later), 0);
        // A rejected attempt records nothing, so the tally is unchanged.
        let mut full = SlidingWindow::new(1, Duration::from_secs(60));
        assert!(full.allow_at(t0));
        assert!(!full.allow_at(t0));
        assert_eq!(full.count_in_window(t0), 1);
    }
}
