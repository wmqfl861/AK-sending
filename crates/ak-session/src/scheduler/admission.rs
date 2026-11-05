//! Pure admission decision over the two-dimensional concurrency policy.
//!
//! [`decide`] answers one question: given the current active set, may the
//! next task of `group_key` enter the transfer service? It is a pure
//! function — no state, no clocks, no I/O — so every cell of the four-mode
//! matrix (contract §2) is exhaustively testable, and the runtime (B6)
//! simply feeds it snapshots of its actor state.
//!
//! Deny reasons:
//!
//! * [`DenyReason::WaitingSerialMode`] — a serial-mode constraint says this
//!   group (or every other group) must finish its current task first.
//!   Serial mode overrides the numeric caps, so this fires before any
//!   capacity check.
//! * [`DenyReason::WaitingCapacity`] — a numeric cap (per-code active tasks
//!   or active code connections) is exhausted.
//!
//! Mode-switch dynamics live in
//! [`SchedulerCore::set_policy`](crate::scheduler::core::SchedulerCore::set_policy):
//! `parallel → serial` drains because `decide` keeps denying over-limit
//! work while in-flight tasks finish naturally; `serial → parallel`
//! backfills because pending queue items suddenly admit.
use crate::scheduler::config::{CrossCodeMode, SameCodeMode, SchedulerPolicy};
use crate::scheduler::key::CodeGroupKey;

/// Distinct code groups currently holding at least one active (admitted)
/// task, in first-admission order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActiveGroups {
    keys: Vec<CodeGroupKey>,
}

impl ActiveGroups {
    /// Build from an iterator of active group keys (duplicates collapse,
    /// first occurrence defines the order).
    pub fn from_keys<I: IntoIterator<Item = CodeGroupKey>>(keys: I) -> Self {
        let mut groups = ActiveGroups::default();
        for key in keys {
            groups.push(key);
        }
        groups
    }

    /// Record a group as active (no-op if already listed).
    pub fn push(&mut self, key: CodeGroupKey) {
        if !self.contains(&key) {
            self.keys.push(key);
        }
    }

    /// Whether the group currently holds at least one active task.
    pub fn contains(&self, key: &CodeGroupKey) -> bool {
        self.keys.contains(key)
    }

    /// Number of distinct active groups.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether no group is active.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Active groups in first-admission order.
    pub fn iter(&self) -> impl Iterator<Item = &CodeGroupKey> + '_ {
        self.keys.iter()
    }
}

impl FromIterator<CodeGroupKey> for ActiveGroups {
    fn from_iter<I: IntoIterator<Item = CodeGroupKey>>(keys: I) -> Self {
        ActiveGroups::from_keys(keys)
    }
}

/// Active-task counts backing an admission decision.
///
/// Must describe the same active set as the paired [`ActiveGroups`]: a
/// group listed with a positive count is listed there, and vice versa.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActiveTasks {
    per_group: Vec<(CodeGroupKey, u32)>,
    total: u32,
}

impl ActiveTasks {
    /// Build from `(group, count)` pairs. Zero counts are dropped.
    pub fn from_counts<I: IntoIterator<Item = (CodeGroupKey, u32)>>(counts: I) -> Self {
        let mut tasks = ActiveTasks::default();
        for (group, count) in counts {
            if count == 0 {
                continue;
            }
            match tasks.per_group.iter_mut().find(|(g, _)| *g == group) {
                Some(slot) => slot.1 += count,
                None => tasks.per_group.push((group, count)),
            }
            tasks.total += count;
        }
        tasks
    }

    /// Active tasks of one group (0 if none).
    pub fn count_in_group(&self, key: &CodeGroupKey) -> u32 {
        self.per_group
            .iter()
            .find(|(g, _)| g == key)
            .map_or(0, |(_, c)| *c)
    }

    /// Total active tasks across all groups.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// Whether no task is active.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Group/count pairs (positive counts only).
    pub fn iter(&self) -> impl Iterator<Item = &(CodeGroupKey, u32)> + '_ {
        self.per_group.iter()
    }
}

/// Why a task may not enter the transfer service yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DenyReason {
    /// A serial-mode constraint holds: same-code serial with a task active
    /// in this group, or cross-code serial with another group active.
    WaitingSerialMode,
    /// A numeric cap is exhausted: per-code active tasks, active code
    /// connections for a group that is not active yet, or the global
    /// `max_active_tasks` ceiling (wave 24).
    WaitingCapacity,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyReason::WaitingSerialMode => f.write_str("WAITING_SERIAL_MODE"),
            DenyReason::WaitingCapacity => f.write_str("WAITING_CAPACITY"),
        }
    }
}

/// Outcome of one admission probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// The task may enter the transfer service.
    Admit,
    /// The task must wait; it stays queued.
    Deny(DenyReason),
}

impl AdmissionDecision {
    /// Whether the decision is [`AdmissionDecision::Admit`].
    pub fn is_admitted(&self) -> bool {
        matches!(self, AdmissionDecision::Admit)
    }

    /// The deny reason, if this is a denial.
    pub fn deny_reason(&self) -> Option<DenyReason> {
        match self {
            AdmissionDecision::Admit => None,
            AdmissionDecision::Deny(reason) => Some(*reason),
        }
    }
}

/// Decide whether the next task of `group_key` may become active.
///
/// Checks run in a fixed order so serial semantics always override numeric
/// caps (contract §2):
///
/// 1. same-code serial and the group has an active task →
///    [`DenyReason::WaitingSerialMode`];
/// 2. cross-code serial, the group is not active, and some other group is →
///    [`DenyReason::WaitingSerialMode`];
/// 3. the group's active count reached the effective per-group cap
///    (1 under same-code serial) → [`DenyReason::WaitingCapacity`];
/// 4. the group is not active and the active-code-connection cap is
///    exhausted (1 under cross-code serial) → [`DenyReason::WaitingCapacity`];
/// 5. the total active-task count across all groups reached the global
///    `max_active_tasks` cap (wave 24) → [`DenyReason::WaitingCapacity`];
/// 6. otherwise [`AdmissionDecision::Admit`].
pub fn decide(
    policy: &SchedulerPolicy,
    active_groups: &ActiveGroups,
    active_tasks: &ActiveTasks,
    group_key: &CodeGroupKey,
) -> AdmissionDecision {
    let limits = policy.effective_limits();
    let in_group = active_tasks.count_in_group(group_key);
    let group_is_active = active_groups.contains(group_key);

    if policy.same_code_mode == SameCodeMode::Serial && in_group >= 1 {
        return AdmissionDecision::Deny(DenyReason::WaitingSerialMode);
    }
    if policy.cross_code_mode == CrossCodeMode::Serial
        && !group_is_active
        && !active_groups.is_empty()
    {
        return AdmissionDecision::Deny(DenyReason::WaitingSerialMode);
    }
    if in_group >= limits.max_tasks_per_group {
        return AdmissionDecision::Deny(DenyReason::WaitingCapacity);
    }
    if !group_is_active && active_groups.len() >= limits.max_active_groups as usize {
        return AdmissionDecision::Deny(DenyReason::WaitingCapacity);
    }
    // Wave-24 global cap: no further admission once this many tasks hold
    // permits, regardless of how they distribute over groups. The cap is
    // 1..=64, so the `usize` bound always fits the `u32` counter.
    if active_tasks.total() >= policy.max_active_tasks as u32 {
        return AdmissionDecision::Deny(DenyReason::WaitingCapacity);
    }
    AdmissionDecision::Admit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::key::{CodeId, RegistryId};

    fn group(n: u8) -> CodeGroupKey {
        CodeGroupKey::new(RegistryId::from_bytes([n; 32]), CodeId::from_bytes([n; 16]))
    }

    /// The wave-24 global cap: parallel/parallel with
    /// `max_active_tasks = 8`, eight active tasks spread over eight groups
    /// — the ninth task (of a fresh group, so no per-group or group-count
    /// cap is in the way) is denied with `WaitingCapacity`.
    #[test]
    fn global_active_tasks_cap_denies_the_ninth_task() {
        let policy = SchedulerPolicy {
            same_code_mode: SameCodeMode::Parallel,
            cross_code_mode: CrossCodeMode::Parallel,
            max_active_tasks_per_code: 4,
            max_active_code_connections: 16,
            max_queued_tasks: 256,
            max_active_tasks: 8,
            ..SchedulerPolicy::default()
        };
        let groups: Vec<CodeGroupKey> = (1..=9).map(group).collect();
        let active_groups = ActiveGroups::from_keys(groups.iter().take(8).copied());
        let active_tasks = ActiveTasks::from_counts(groups.iter().take(8).map(|key| (*key, 1)));
        // Sanity: with a cap of 8 the ninth (fresh group, per-group and
        // group-count caps nowhere near) is denied by the global cap alone.
        assert_eq!(
            decide(&policy, &active_groups, &active_tasks, &groups[8]),
            AdmissionDecision::Deny(DenyReason::WaitingCapacity)
        );
        // The same probe with only 7 active admits: the global cap is the
        // deciding check.
        let active_groups_7 = ActiveGroups::from_keys(groups.iter().take(7).copied());
        let active_tasks_7 = ActiveTasks::from_counts(groups.iter().take(7).map(|key| (*key, 1)));
        assert_eq!(
            decide(&policy, &active_groups_7, &active_tasks_7, &groups[8]),
            AdmissionDecision::Admit
        );
        // A larger global cap lifts the denial (the cap, not another check,
        // was the cause).
        let lifted = SchedulerPolicy {
            max_active_tasks: 9,
            ..policy
        };
        assert_eq!(
            decide(&lifted, &active_groups, &active_tasks, &groups[8]),
            AdmissionDecision::Admit
        );
    }
}
