//! Synthetic loopback experiment. No pointers, user paths, credentials or target
//! addresses cross this ABI. Load is inert; starting is an explicit caller action.
//! One task per loaded library instance. This is not the product task manager.
#![deny(unsafe_op_in_unsafe_fn)]

mod probe;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const RUNNING: i32 = 0;
const PASSED: i32 = 1;
const FAILED: i32 = -1;
const CANCELLED: i32 = -2;
const INVALID: i32 = -3;
const BUSY: i32 = -4;
const EXHAUSTED: i32 = -5;
type Worker = fn(Arc<AtomicBool>) -> i32;

struct Job {
    id: u64,
    status: Arc<AtomicI32>,
    cancel: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}
struct Registry {
    next: u64,
    job: Option<Job>,
}
impl Registry {
    const fn new() -> Self {
        Self { next: 1, job: None }
    }
    fn start(&mut self, work: Worker) -> i64 {
        if self.job.is_some() {
            return i64::from(BUSY);
        }
        // Never wrap/reuse a handle, including after a failed spawn.
        if self.next > i64::MAX as u64 {
            return i64::from(EXHAUSTED);
        }
        let id = self.next;
        self.next = match self.next.checked_add(1) {
            Some(next) => next,
            None => return i64::from(EXHAUSTED),
        };
        let status = Arc::new(AtomicI32::new(RUNNING));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_status = status.clone();
        let worker_cancel = cancel.clone();
        let worker = thread::Builder::new()
            .name("ak-m0-probe".into())
            .spawn(move || {
                let result =
                    catch_unwind(AssertUnwindSafe(|| work(worker_cancel))).unwrap_or(FAILED);
                // Cancel and completion race at this CAS. Once terminal, immutable.
                let _ = worker_status.compare_exchange(
                    RUNNING,
                    result,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            });
        match worker {
            Ok(worker) => {
                self.job = Some(Job {
                    id,
                    status,
                    cancel,
                    worker,
                });
                id as i64
            }
            Err(_) => i64::from(FAILED),
        }
    }
    fn find(&self, id: u64) -> Option<&Job> {
        self.job.as_ref().filter(|job| job.id == id)
    }
    fn poll(&self, id: u64) -> i32 {
        self.find(id)
            .map_or(INVALID, |job| job.status.load(Ordering::SeqCst))
    }
    fn cancel(&self, id: u64) -> i32 {
        let Some(job) = self.find(id) else {
            return INVALID;
        };
        match job
            .status
            .compare_exchange(RUNNING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {
                job.cancel.store(true, Ordering::SeqCst);
                CANCELLED
            }
            Err(terminal) => terminal,
        }
    }
    fn release(&mut self, id: u64) -> i32 {
        let Some(job) = self.find(id) else {
            return INVALID;
        };
        if !job.worker.is_finished() {
            return BUSY;
        }
        let Some(job) = self.job.take() else {
            return INVALID;
        };
        let result = job.status.load(Ordering::SeqCst);
        if job.worker.join().is_err() {
            FAILED
        } else {
            result
        }
    }
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

// No unsafe blocks or pointer arguments. These unique, fixed-width C exports
// require an unsafe symbol attribute; never link two copies into one process.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_probe_version() -> u32 {
    0x0001_0000
}

/// Positive handle on success; negative error otherwise. No networking on caller.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_probe_start() -> i64 {
    catch_unwind(|| {
        REGISTRY
            .lock()
            .map_or(i64::from(FAILED), |mut registry| registry.start(probe::run))
    })
    .unwrap_or(i64::from(FAILED))
}

#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_probe_poll(id: u64) -> i32 {
    catch_unwind(|| REGISTRY.lock().map_or(FAILED, |r| r.poll(id))).unwrap_or(FAILED)
}

/// Returns CANCELLED only if cancellation won; already-terminal status otherwise.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_probe_cancel(id: u64) -> i32 {
    catch_unwind(|| REGISTRY.lock().map_or(FAILED, |r| r.cancel(id))).unwrap_or(FAILED)
}

/// Never waits for network or a live worker. BUSY means poll/release again later.
/// The caller must not unload the library until its task has been released.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_probe_release(id: u64) -> i32 {
    catch_unwind(|| REGISTRY.lock().map_or(FAILED, |mut r| r.release(id))).unwrap_or(FAILED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn done(_: Arc<AtomicBool>) -> i32 {
        PASSED
    }
    fn until_cancel(cancel: Arc<AtomicBool>) -> i32 {
        let limit = Instant::now() + Duration::from_secs(3);
        while !cancel.load(Ordering::SeqCst) && Instant::now() < limit {
            thread::sleep(Duration::from_millis(1));
        }
        PASSED // Intentionally races: cannot overwrite an accepted cancel.
    }
    fn drain(registry: &mut Registry, id: u64) -> i32 {
        let limit = Instant::now() + Duration::from_secs(4);
        loop {
            let value = registry.release(id);
            if value != BUSY {
                return value;
            }
            assert!(Instant::now() < limit);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn one_slot_busy_cancel_release_and_stale_handles() {
        let mut r = Registry::new();
        let id = r.start(until_cancel) as u64;
        assert_eq!(r.poll(id), RUNNING);
        assert_eq!(r.start(done), i64::from(BUSY));
        assert_eq!(r.release(id), BUSY);
        assert_eq!(r.cancel(id), CANCELLED);
        assert_eq!(r.cancel(id), CANCELLED);
        assert_eq!(drain(&mut r, id), CANCELLED);
        assert_eq!(r.poll(id), INVALID);
        assert_eq!(r.cancel(id), INVALID);
        assert_eq!(r.release(id), INVALID);
        let next = r.start(done) as u64;
        assert!(next > id);
        assert_eq!(r.cancel(id), INVALID);
        assert_eq!(drain(&mut r, next), PASSED);
    }
    #[test]
    fn terminal_success_cannot_be_changed_to_cancelled() {
        let mut r = Registry::new();
        let id = r.start(done) as u64;
        let limit = Instant::now() + Duration::from_secs(3);
        while r.poll(id) == RUNNING {
            assert!(Instant::now() < limit);
            thread::yield_now();
        }
        assert_eq!(r.cancel(id), PASSED);
        assert_eq!(drain(&mut r, id), PASSED);
    }
    #[test]
    fn handles_never_wrap_and_unknown_handles_are_rejected() {
        let mut r = Registry::new();
        assert_eq!(r.poll(0), INVALID);
        assert_eq!(r.release(u64::MAX), INVALID);
        r.next = i64::MAX as u64;
        let id = r.start(done) as u64;
        assert_eq!(id, i64::MAX as u64);
        assert_eq!(drain(&mut r, id), PASSED);
        assert_eq!(r.start(done), i64::from(EXHAUSTED));
        r.next = u64::MAX;
        assert_eq!(r.start(done), i64::from(EXHAUSTED));
    }
    #[test]
    fn worker_panic_is_failed_not_unwind_across_abi() {
        let mut r = Registry::new();
        let id = r.start(|_| panic!("synthetic failure")) as u64;
        assert_eq!(drain(&mut r, id), FAILED);
    }
    #[test]
    fn repeated_jobs_release_every_worker() {
        let mut r = Registry::new();
        for _ in 0..20 {
            let id = r.start(done) as u64;
            assert_eq!(drain(&mut r, id), PASSED);
            assert!(r.job.is_none());
        }
    }
    #[test]
    fn actual_loopback_and_pre_cancelled_worker() {
        assert_eq!(probe::run(Arc::new(AtomicBool::new(true))), CANCELLED);
        assert_eq!(probe::run(Arc::new(AtomicBool::new(false))), PASSED);
    }
}
