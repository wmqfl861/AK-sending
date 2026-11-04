//! Grouping keys, task handles and permits for the scheduler domain model.
//!
//! These types are pure identifiers: no I/O, no clocks, no randomness.
//! Wire-level permit negotiation (`TaskOpen`/`TaskAdmission`/`TaskAdmissionAck`
//! in `docs/contracts/scheduler.md` §6) is layered on top by the runtime and
//! `ak-wire`; the scheduler only tracks local slot ownership.
use std::fmt;

/// Registry identity byte length (`registry_id[32]` in the admission wire
/// messages).
pub const REGISTRY_ID_LEN: usize = 32;
/// Pairing-code identity byte length (`code_id[16]` in the admission wire
/// messages).
pub const CODE_ID_LEN: usize = 16;
/// Task (transfer) identity byte length (`transfer_id[16]`).
pub const TASK_ID_LEN: usize = 16;
/// Permit identity byte length (`permit_id[16]`).
pub const PERMIT_ID_LEN: usize = 16;

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

macro_rules! byte_id {
    ($name:ident, $len:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; $len]);

        impl $name {
            /// Wrap raw bytes (no re-hashing).
            pub fn from_bytes(bytes: [u8; $len]) -> Self {
                $name(bytes)
            }

            /// Raw bytes.
            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            /// 64/32 lowercase hex characters, stable display form.
            pub fn to_hex(&self) -> String {
                hex_lower(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }
    };
}

byte_id!(
    RegistryId,
    REGISTRY_ID_LEN,
    "Registry identity owning a pairing code (`registry_id[32]`)."
);
byte_id!(
    CodeId,
    CODE_ID_LEN,
    "Pairing-code identity (`code_id[16]`), one code one owner."
);
byte_id!(
    TaskId,
    TASK_ID_LEN,
    "Stable task instance identity (`transfer_id[16]`)."
);
byte_id!(
    PermitId,
    PERMIT_ID_LEN,
    "Local admission permit identity (`permit_id[16]`)."
);

impl TaskId {
    /// Deterministic test/domain id from a counter (big-endian in the low
    /// bytes). Real transfer ids are random and persistent; this constructor
    /// exists for the pure domain model and tests.
    pub fn from_counter(counter: u64) -> Self {
        let mut bytes = [0u8; TASK_ID_LEN];
        bytes[TASK_ID_LEN - 8..].copy_from_slice(&counter.to_be_bytes());
        TaskId(bytes)
    }
}

impl PermitId {
    /// Deterministic domain permit id from a counter (big-endian in the low
    /// bytes). Wire-visible permit ids are assigned by the runtime when a
    /// `TaskAdmission` message is sent.
    pub fn from_counter(counter: u64) -> Self {
        let mut bytes = [0u8; PERMIT_ID_LEN];
        bytes[PERMIT_ID_LEN - 8..].copy_from_slice(&counter.to_be_bytes());
        PermitId(bytes)
    }
}

/// Scheduling group key: `(registry_id, code_id)`.
///
/// One code has one owner; every relation the owner's code produces (A→B,
/// B→A, A→C, C→A) shares this key, so the same-code policy cannot be split
/// per peer (contract §2).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodeGroupKey {
    /// Owning registry.
    pub registry_id: RegistryId,
    /// Pairing code inside that registry.
    pub code_id: CodeId,
}

impl CodeGroupKey {
    /// Assemble a group key from its parts.
    pub fn new(registry_id: RegistryId, code_id: CodeId) -> Self {
        CodeGroupKey {
            registry_id,
            code_id,
        }
    }
}

impl fmt::Debug for CodeGroupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CodeGroupKey({}/{})",
            self.registry_id.to_hex(),
            self.code_id.to_hex()
        )
    }
}

/// Public handle for one submitted task, wrapping its stable [`TaskId`].
///
/// Copyable and comparable; the runtime, FFI and UI pass this handle around
/// instead of raw byte arrays.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskHandle(TaskId);

impl TaskHandle {
    /// Wrap a task id.
    pub fn new(task_id: TaskId) -> Self {
        TaskHandle(task_id)
    }

    /// The wrapped task id.
    pub fn task_id(&self) -> TaskId {
        self.0
    }
}

impl From<TaskId> for TaskHandle {
    fn from(task_id: TaskId) -> Self {
        TaskHandle(task_id)
    }
}

impl fmt::Debug for TaskHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TaskHandle({})", self.0.to_hex())
    }
}

/// Lifecycle state of a local admission permit.
///
/// A permit is `Granted` while the task occupies one active slot of its
/// group (and, for the group's first task, one active-code-connection
/// slot) and becomes `Released` when the task finishes, is cancelled,
/// pauses or yields its slot (contract §6/§7). Two-end tentative permits
/// with bounded leases are negotiated by the runtime over the direct
/// channel; the local model only needs granted/released bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitState {
    /// The task holds one active slot.
    Granted,
    /// The slot has been returned; the permit is kept only for audit.
    Released,
}

/// One local admission permit: binds a task to the group whose capacity it
/// occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permit {
    id: PermitId,
    task: TaskId,
    group: CodeGroupKey,
    state: PermitState,
}

impl Permit {
    /// Create a permit in the [`PermitState::Granted`] state.
    pub fn new_granted(id: PermitId, task: TaskId, group: CodeGroupKey) -> Self {
        Permit {
            id,
            task,
            group,
            state: PermitState::Granted,
        }
    }

    /// Permit identity.
    pub fn id(&self) -> PermitId {
        self.id
    }

    /// The task holding (or having held) this permit.
    pub fn task(&self) -> TaskId {
        self.task
    }

    /// The group whose capacity this permit occupies.
    pub fn group(&self) -> CodeGroupKey {
        self.group
    }

    /// Current lifecycle state.
    pub fn state(&self) -> PermitState {
        self.state
    }

    /// Mark released; returns whether this call performed the transition
    /// (releasing an already-released permit is a no-op).
    pub fn release(&mut self) -> bool {
        if self.state == PermitState::Released {
            return false;
        }
        self.state = PermitState::Released;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ids_round_trip_and_debug_compact() {
        let registry = RegistryId::from_bytes([0x11; REGISTRY_ID_LEN]);
        let code = CodeId::from_bytes([0x22; CODE_ID_LEN]);
        assert_eq!(registry.as_bytes(), &[0x11; REGISTRY_ID_LEN]);
        assert_eq!(registry.to_hex().len(), REGISTRY_ID_LEN * 2);
        assert!(registry
            .to_hex()
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(
            format!("{registry:?}"),
            format!("RegistryId({})", registry.to_hex())
        );
        assert_ne!(registry, RegistryId::from_bytes([0x00; REGISTRY_ID_LEN]));
        let _ = code;
    }

    #[test]
    fn counter_ids_are_ordered_and_distinct() {
        let a = TaskId::from_counter(1);
        let b = TaskId::from_counter(2);
        assert_ne!(a, b);
        assert!(a < b);
        assert_eq!(TaskId::from_counter(7), TaskId::from_counter(7));
        // Permit ids and task ids never compare across types; same shape.
        assert!(PermitId::from_counter(1) < PermitId::from_counter(2));
    }

    #[test]
    fn permit_release_is_idempotent() {
        let task = TaskId::from_counter(9);
        let group = CodeGroupKey::new(
            RegistryId::from_bytes([1; REGISTRY_ID_LEN]),
            CodeId::from_bytes([2; CODE_ID_LEN]),
        );
        let mut permit = Permit::new_granted(PermitId::from_counter(3), task, group);
        assert_eq!(permit.state(), PermitState::Granted);
        assert_eq!(permit.task(), task);
        assert_eq!(permit.group(), group);
        assert!(permit.release());
        assert_eq!(permit.state(), PermitState::Released);
        assert!(!permit.release());
    }
}
