//! One independent endpoint. Only synthetic bytes, no server or file operations.
//! Local start is a one-shot intent for the immutable peer, role and test task.
mod network;
#[cfg(test)]
mod tests;

use ak_transport::PairedPeer;
use std::net::{Ipv4Addr, SocketAddr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const PREPARING: i32 = 10;
const READY: i32 = 11;
const PAIRED: i32 = 12;
const START: i32 = 13;
const CONNECTING: i32 = 14;
const ACTIVE: i32 = 15;
const LISTENING: i32 = 16;
const PASSED: i32 = 1;
const FAILED: i32 = -1;
const CANCELLED: i32 = -2;
const INVALID: i32 = -3;
const BUSY: i32 = -4;
const EXHAUSTED: i32 = -5;
const BAD_INPUT: i32 = -6;
const CONNECTION_FAILED: i32 = -7;
const EXPIRED: i32 = -8;
const CONTENT_FAILED: i32 = -9;
const CERT_LIMIT: usize = 4096;
const TOTAL_LIMIT: Duration = Duration::from_secs(180);

fn address(ip: u32, port: u32) -> Result<SocketAddr, i32> {
    let ip = Ipv4Addr::from(ip);
    if !(ip.is_private() || ip == Ipv4Addr::LOCALHOST) || !(1024..=65535).contains(&port) {
        return Err(BAD_INPUT);
    }
    // A private address is an input restriction, not proof of LAN routing.
    Ok(SocketAddr::from((ip, port as u16)))
}

struct Config {
    peer: PairedPeer,
    address: SocketAddr,
    sending: bool,
}
struct Control {
    status: i32,
    certificate: Vec<u8>,
    own_id: [u8; 32],
    staged_peer: Vec<u8>,
    config: Option<Config>,
}
impl Control {
    fn new() -> Self {
        Self {
            status: PREPARING,
            certificate: Vec::new(),
            own_id: [0; 32],
            staged_peer: Vec::new(),
            config: None,
        }
    }
    fn push(&mut self, index: u32, byte: u32) -> i32 {
        if self.status != READY {
            return BAD_INPUT;
        }
        if index as usize != self.staged_peer.len()
            || self.staged_peer.len() >= CERT_LIMIT
            || byte > 255
        {
            return BAD_INPUT;
        }
        self.staged_peer.push(byte as u8);
        0
    }
    fn read(&self, kind: u32, index: u32) -> i32 {
        if self.certificate.is_empty() {
            return BUSY;
        }
        let peer_id;
        let bytes: &[u8] = match kind {
            0 => &self.certificate,
            1 => &self.own_id,
            2 => {
                let Some(config) = &self.config else {
                    return BAD_INPUT;
                };
                peer_id = config.peer.id();
                &peer_id
            }
            _ => return BAD_INPUT,
        };
        if index == u32::MAX {
            bytes.len() as i32
        } else {
            bytes
                .get(index as usize)
                .map_or(BAD_INPUT, |b| i32::from(*b))
        }
    }
    fn configure(&mut self, local: SocketAddr, ip: u32, port: u32, role: u32) -> i32 {
        if self.status != READY || role > 1 {
            return BAD_INPUT;
        }
        let Ok(remote) = address(ip, port) else {
            return BAD_INPUT;
        };
        if local == remote || local.ip().is_loopback() != remote.ip().is_loopback() {
            return BAD_INPUT;
        }
        let Ok(peer) = PairedPeer::from_certificate_der(self.staged_peer.clone()) else {
            return BAD_INPUT;
        };
        if peer.id() == self.own_id {
            return BAD_INPUT;
        }
        self.staged_peer.clear();
        self.config = Some(Config {
            peer,
            address: remote,
            sending: role == 1,
        });
        self.status = PAIRED;
        PAIRED
    }
    fn start(&mut self) -> i32 {
        if self.status != PAIRED {
            return BAD_INPUT;
        }
        self.status = START;
        START
    }
    fn cancel(&mut self) -> i32 {
        if self.status >= PREPARING {
            self.status = CANCELLED;
        }
        self.status
    }
    fn phase(&mut self, phase: i32) -> Result<(), i32> {
        if self.status < PREPARING {
            return Err(self.status);
        }
        self.status = phase;
        Ok(())
    }
}

type Shared = Arc<Mutex<Control>>;
struct Job {
    id: u64,
    local: SocketAddr,
    control: Shared,
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
    fn prepare(&mut self, ip: u32, port: u32) -> i64 {
        let Ok(local) = address(ip, port) else {
            return i64::from(BAD_INPUT);
        };
        if self.job.is_some() {
            return i64::from(BUSY);
        }
        if self.next > i64::MAX as u64 {
            return i64::from(EXHAUSTED);
        }
        let id = self.next;
        self.next += 1; // Bounded by i64::MAX above; no wrapping or reuse.
        let shared = Arc::new(Mutex::new(Control::new()));
        let control = shared.clone();
        let worker = thread::Builder::new()
            .name("ak-lan-probe".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| network::run(local, shared.clone())))
                    .unwrap_or(FAILED);
                if let Ok(mut state) = shared.lock() {
                    // Cancellation and completion linearize under the same lock.
                    if state.status >= PREPARING {
                        state.status = result;
                    }
                }
            });
        match worker {
            Ok(worker) => {
                self.job = Some(Job {
                    id,
                    local,
                    control,
                    worker,
                });
                id as i64
            }
            Err(_) => i64::from(FAILED),
        }
    }
    fn with(&self, id: u64, action: impl FnOnce(&mut Control, SocketAddr) -> i32) -> i32 {
        let Some(job) = self.job.as_ref().filter(|job| job.id == id) else {
            return INVALID;
        };
        match job.control.lock() {
            Ok(mut state) => action(&mut state, job.local),
            Err(_) => FAILED,
        }
    }
    fn release(&mut self, id: u64) -> i32 {
        let Some(job) = self.job.as_ref().filter(|job| job.id == id) else {
            return INVALID;
        };
        if !job.worker.is_finished() {
            return BUSY;
        }
        let Some(job) = self.job.take() else {
            return INVALID;
        };
        if job.worker.join().is_err() {
            return FAILED;
        }
        let result = job.control.lock().map_or(FAILED, |state| state.status);
        result
    }
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());
fn ffi(action: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(action)).unwrap_or(FAILED)
}
fn with(id: u64, action: impl FnOnce(&mut Control, SocketAddr) -> i32) -> i32 {
    ffi(|| REGISTRY.lock().map_or(FAILED, |r| r.with(id, action)))
}

#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_version() -> u32 {
    1
}
/// Preparation generates an ephemeral identity but does not open a UDP socket.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_prepare(ip: u32, port: u32) -> i64 {
    catch_unwind(|| {
        REGISTRY
            .lock()
            .map_or(i64::from(FAILED), |mut r| r.prepare(ip, port))
    })
    .unwrap_or(i64::from(FAILED))
}
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_poll(id: u64) -> i32 {
    with(id, |s, _| s.status)
}
/// kind: local DER=0, local SHA256=1, paired peer SHA256=2. MAX index returns length.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_read(id: u64, kind: u32, index: u32) -> i32 {
    with(id, |s, _| s.read(kind, index))
}
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_push(id: u64, index: u32, byte: u32) -> i32 {
    with(id, |s, _| s.push(index, byte))
}
/// Immutable peer configuration. role=0 receiver, role=1 sender. No network yet.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_configure(id: u64, ip: u32, port: u32, role: u32) -> i32 {
    with(id, |s, local| s.configure(local, ip, port, role))
}
/// Call only after the local operator verifies the peer fingerprint and explicitly
/// authorizes this one synthetic experiment. The network cannot invoke this API.
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_start(id: u64) -> i32 {
    with(id, |s, _| s.start())
}
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_cancel(id: u64) -> i32 {
    with(id, |s, _| s.cancel())
}
#[unsafe(no_mangle)]
pub extern "C" fn ak_m0_lan_release(id: u64) -> i32 {
    ffi(|| REGISTRY.lock().map_or(FAILED, |mut r| r.release(id)))
}
