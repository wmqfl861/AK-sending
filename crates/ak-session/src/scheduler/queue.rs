//! Bounded local task queue with per-group fairness.
//!
//! Contract §5/§7/§8 semantics implemented here:
//!
//! * **Bounded**: `enqueue` rejects with [`QueueError::QueueFull`] the moment
//!   the queue holds `capacity` items. The check is O(1) against a live
//!   length counter — accepting or rejecting an enqueue never scans the
//!   queued items (the plan calls this out explicitly: no scan, no
//!   connection, no silent drop of older tasks).
//! * **Per-code bound (wave 24)**: `enqueue` also rejects once the task's
//!   own group holds `per_group_capacity` items, so one hot code cannot
//!   monopolize the whole queue. O(1) against the group's deque length.
//! * **Same-group FIFO**: tasks of one group leave in arrival order.
//! * **Group round-robin**: after a group yields one task it rotates to the
//!   back of the ready ring, so a hot code cannot starve other codes.
//! * **Cancellable**: any queued item can be removed without disturbing the
//!   relative order of the rest.
use std::collections::{HashMap, VecDeque};

use crate::scheduler::key::{CodeGroupKey, TaskId};

/// Why an enqueue was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    /// The queue is at capacity (globally, or for the task's own code
    /// group); the enqueue was rejected without scanning and without
    /// touching any queued item (wire-facing name `QUEUE_FULL`, contract
    /// §7).
    #[error("QUEUE_FULL")]
    QueueFull,
    /// The task id is already queued; ids are unique per submission.
    #[error("duplicate task already queued")]
    DuplicateTask,
}

/// Bounded FIFO queue with group-level round-robin fairness.
///
/// Invariants (relied on by [`SchedulerCore`](crate::scheduler::SchedulerCore)):
///
/// * `ready` contains exactly the groups holding at least one queued task,
///   in rotation order;
/// * every queued task id maps to its group in `index` and appears exactly
///   once in that group's deque.
#[derive(Debug, Clone)]
pub struct FairQueue {
    capacity: usize,
    per_group_capacity: usize,
    groups: HashMap<CodeGroupKey, VecDeque<TaskId>>,
    ready: VecDeque<CodeGroupKey>,
    index: HashMap<TaskId, CodeGroupKey>,
}

impl FairQueue {
    /// Create a queue accepting at most `capacity` items with no separate
    /// per-group bound (each group may fill the whole queue).
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero; the frozen policy range guarantees
    /// `max_queued_tasks >= 1`.
    pub fn new(capacity: usize) -> Self {
        Self::with_caps(capacity, usize::MAX)
    }

    /// Create a queue accepting at most `capacity` items in total and
    /// `per_group_capacity` items per code group (the wave-24
    /// `max_queued_tasks_per_code` knob; the global bound is checked
    /// first, so a per-group bound above the total simply never fires).
    ///
    /// # Panics
    ///
    /// Panics if either bound is zero.
    pub fn with_caps(capacity: usize, per_group_capacity: usize) -> Self {
        assert!(capacity >= 1, "queue capacity must be at least 1");
        assert!(
            per_group_capacity >= 1,
            "per-group queue capacity must be at least 1"
        );
        FairQueue {
            capacity,
            per_group_capacity,
            groups: HashMap::new(),
            ready: VecDeque::new(),
            index: HashMap::new(),
        }
    }

    /// Maximum number of queued items.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Maximum number of queued items for one code group.
    pub fn per_group_capacity(&self) -> usize {
        self.per_group_capacity
    }

    /// Adjust the capacity, storing the requested target as-is.
    ///
    /// Shrinking below the live backlog neither drops nor reorders anything
    /// already queued (contract §7: no automatic dropping of old tasks):
    /// `enqueue` refuses while the current length is at or above the target,
    /// and the bound converges to the new value naturally as tasks dequeue.
    /// The target is *not* silently raised back to the old backlog size, so
    /// the configured limit and the actual backpressure agree once the
    /// backlog drains.
    pub fn set_capacity(&mut self, capacity: usize) {
        assert!(capacity >= 1, "queue capacity must be at least 1");
        self.capacity = capacity;
    }

    /// Adjust the per-group capacity, storing the requested target as-is.
    ///
    /// Shrinking below a group's live backlog keeps those tasks queued
    /// (contract §7) while *new* enqueues of that group are refused until
    /// its backlog drains below the target — the same natural-convergence
    /// discipline as [`FairQueue::set_capacity`]. Other groups are never
    /// blocked by one group's oversized backlog.
    pub fn set_per_group_capacity(&mut self, per_group_capacity: usize) {
        assert!(
            per_group_capacity >= 1,
            "per-group queue capacity must be at least 1"
        );
        self.per_group_capacity = per_group_capacity;
    }

    /// Number of queued items (O(1)).
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether no task is queued.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Whether the task is currently queued.
    pub fn contains(&self, task: &TaskId) -> bool {
        self.index.contains_key(task)
    }

    /// Number of queued items for one group.
    pub fn group_len(&self, group: &CodeGroupKey) -> usize {
        self.groups.get(group).map_or(0, VecDeque::len)
    }

    /// Ready-ring order of groups holding queued items (snapshot).
    pub fn ready_order(&self) -> Vec<CodeGroupKey> {
        self.ready.iter().copied().collect()
    }

    /// Append a task to its group's FIFO.
    ///
    /// O(1): the capacity checks compare the live length counters and never
    /// scan the queued items. The global bound fires first, then the
    /// per-group bound (`max_queued_tasks_per_code`, wave 24). A group
    /// appearing for the first time joins the back of the ready ring.
    pub fn enqueue(&mut self, group: &CodeGroupKey, task: TaskId) -> Result<(), QueueError> {
        if self.index.contains_key(&task) {
            return Err(QueueError::DuplicateTask);
        }
        if self.index.len() >= self.capacity {
            return Err(QueueError::QueueFull);
        }
        if self.group_len(group) >= self.per_group_capacity {
            return Err(QueueError::QueueFull);
        }
        self.index.insert(task, *group);
        let deque = self.groups.entry(*group).or_default();
        if deque.is_empty() {
            self.ready.push_back(*group);
        }
        deque.push_back(task);
        Ok(())
    }

    /// Pop the front task of the front ready group and rotate that group to
    /// the back of the ring (per-task round-robin, contract §8).
    pub fn pop_next(&mut self) -> Option<(CodeGroupKey, TaskId)> {
        let group = self.ready.front().copied()?;
        let task = self.pop_for(&group)?;
        Some((group, task))
    }

    /// Pop the front task of one specific group, rotating that group to the
    /// back of the ring. Returns `None` if the group has nothing queued.
    pub fn pop_for(&mut self, group: &CodeGroupKey) -> Option<TaskId> {
        let task = self.groups.get_mut(group)?.pop_front()?;
        self.index.remove(&task);
        if let Some(pos) = self.ready.iter().position(|g| g == group) {
            self.ready.remove(pos);
        }
        let empty = self.groups.get(group).is_some_and(VecDeque::is_empty);
        if empty {
            self.groups.remove(group);
        } else {
            self.ready.push_back(*group);
        }
        Some(task)
    }

    /// Put a just-popped task back at the **front** of its group, restoring
    /// the exact pre-pop queue order.
    ///
    /// Contract: call this immediately after the [`FairQueue::pop_for`] (or
    /// [`FairQueue::pop_next`]) that removed `task`, with no intervening
    /// queue operation. It exists so the admission loop can examine a task,
    /// get a deny decision, and leave the queue bit-for-bit as it was.
    pub fn requeue_front(&mut self, group: &CodeGroupKey, task: TaskId) {
        let deque = self.groups.entry(*group).or_default();
        if deque.is_empty() {
            self.ready.push_back(*group);
        }
        deque.push_front(task);
        self.index.insert(task, *group);
        if let Some(pos) = self.ready.iter().position(|g| g == group) {
            self.ready.remove(pos);
        }
        self.ready.push_front(*group);
    }

    /// Put a just-popped task back at the **front** of its group while
    /// rotating the group to the **back** of the ready ring.
    ///
    /// Same calling contract as [`FairQueue::requeue_front`] (immediately
    /// after the pop, task back at its group's head), but the group yields
    /// its place in the rotation instead of regaining the front. The
    /// cross-code serial batch gate uses this for tasks deferred to their
    /// group's next batch (contract §8): the group just held its turn, so
    /// the next group in line must be served first when the batch ends.
    pub fn requeue_defer(&mut self, group: &CodeGroupKey, task: TaskId) {
        let deque = self.groups.entry(*group).or_default();
        if deque.is_empty() {
            self.ready.push_back(*group);
        }
        deque.push_front(task);
        self.index.insert(task, *group);
        if let Some(pos) = self.ready.iter().position(|g| g == group) {
            self.ready.remove(pos);
        }
        self.ready.push_back(*group);
    }

    /// Remove one queued task; returns whether it was found. The order of
    /// all other tasks (and groups) is unchanged.
    pub fn cancel(&mut self, task: &TaskId) -> bool {
        let Some(group) = self.index.remove(task) else {
            return false;
        };
        let empty = {
            let deque = self
                .groups
                .get_mut(&group)
                .expect("index entry must have a group deque");
            let pos = deque
                .iter()
                .position(|t| t == task)
                .expect("index entry must be in the group deque");
            deque.remove(pos);
            deque.is_empty()
        };
        if empty {
            self.groups.remove(&group);
            if let Some(pos) = self.ready.iter().position(|g| g == &group) {
                self.ready.remove(pos);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::key::{CodeId, RegistryId};

    fn group(n: u8) -> CodeGroupKey {
        CodeGroupKey::new(RegistryId::from_bytes([n; 32]), CodeId::from_bytes([n; 16]))
    }

    fn task(n: u64) -> TaskId {
        TaskId::from_counter(n)
    }

    #[test]
    fn single_group_keeps_strict_fifo() {
        let g = group(1);
        let mut q = FairQueue::new(8);
        for n in 1..=4 {
            q.enqueue(&g, task(n)).unwrap();
        }
        let mut popped = Vec::new();
        while let Some(t) = q.pop_for(&g) {
            popped.push(t);
        }
        assert_eq!(popped, vec![task(1), task(2), task(3), task(4)]);
        assert!(q.is_empty());
    }

    #[test]
    fn capacity_rejects_exactly_at_the_boundary() {
        let g = group(1);
        let mut q = FairQueue::new(2);
        assert!(q.enqueue(&g, task(1)).is_ok());
        assert!(q.enqueue(&g, task(2)).is_ok());
        assert_eq!(q.enqueue(&g, task(3)), Err(QueueError::QueueFull));
        assert_eq!(q.len(), 2);
        // Freeing one slot admits exactly one more.
        assert!(q.cancel(&task(1)));
        assert!(q.enqueue(&g, task(4)).is_ok());
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn duplicate_task_id_is_rejected() {
        let g = group(1);
        let mut q = FairQueue::new(4);
        q.enqueue(&g, task(1)).unwrap();
        assert_eq!(
            q.enqueue(&group(2), task(1)),
            Err(QueueError::DuplicateTask)
        );
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn two_groups_alternate_under_round_robin() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::new(8);
        // Interleaved arrival: A1 B1 A2 B2 ...
        for n in 0..3 {
            q.enqueue(&a, task(1 + 2 * n)).unwrap();
            q.enqueue(&b, task(2 + 2 * n)).unwrap();
        }
        let mut order = Vec::new();
        while let Some((g, t)) = q.pop_next() {
            order.push((g, t));
        }
        let expect = vec![
            (a, task(1)),
            (b, task(2)),
            (a, task(3)),
            (b, task(4)),
            (a, task(5)),
            (b, task(6)),
        ];
        assert_eq!(order, expect);
    }

    #[test]
    fn hot_group_cannot_starve_a_cold_group() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::new(16);
        for n in 1..=5 {
            q.enqueue(&a, task(n)).unwrap();
        }
        q.enqueue(&b, task(99)).unwrap();
        let first = q.pop_next().unwrap();
        let second = q.pop_next().unwrap();
        assert_eq!(first.0, a);
        assert_eq!(second.0, b, "cold group must be served within two pops");
        assert_eq!(q.group_len(&a), 4);
    }

    #[test]
    fn cancel_from_middle_keeps_relative_order() {
        let g = group(1);
        let mut q = FairQueue::new(8);
        for n in 1..=4 {
            q.enqueue(&g, task(n)).unwrap();
        }
        assert!(q.cancel(&task(2)));
        assert!(!q.cancel(&task(2)));
        let mut popped = Vec::new();
        while let Some(t) = q.pop_for(&g) {
            popped.push(t);
        }
        assert_eq!(popped, vec![task(1), task(3), task(4)]);
    }

    #[test]
    fn canceling_last_task_of_a_group_retires_the_group() {
        let a = group(1);
        let mut q = FairQueue::new(8);
        q.enqueue(&a, task(1)).unwrap();
        assert!(q.cancel(&task(1)));
        assert_eq!(q.ready_order(), Vec::<CodeGroupKey>::new());
        assert_eq!(q.group_len(&a), 0);
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn requeue_front_restores_exact_rotation() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::new(8);
        for n in 1..=2 {
            q.enqueue(&a, task(n)).unwrap();
            q.enqueue(&b, task(10 + n)).unwrap();
        }
        let before_groups = q.group_len(&a);
        // Pop A's head, put it back; the ring and FIFO must be as before.
        let t = q.pop_for(&a).unwrap();
        q.requeue_front(&a, t);
        assert_eq!(q.group_len(&a), before_groups);
        assert_eq!(
            q.pop_next().unwrap(),
            (a, task(1)),
            "requeued task must be the next one served"
        );
        assert_eq!(q.pop_next().unwrap(), (b, task(11)));
    }

    #[test]
    fn set_capacity_never_drops_or_reorders() {
        let g = group(1);
        let mut q = FairQueue::new(4);
        for n in 1..=3 {
            q.enqueue(&g, task(n)).unwrap();
        }
        q.set_capacity(2); // below the live backlog: keeps items, binds new enqueues
        assert_eq!(q.len(), 3);
        assert_eq!(q.enqueue(&g, task(4)), Err(QueueError::QueueFull));
        q.set_capacity(10);
        assert!(q.enqueue(&g, task(4)).is_ok());
        let mut popped = Vec::new();
        while let Some(t) = q.pop_for(&g) {
            popped.push(t);
        }
        assert_eq!(popped, vec![task(1), task(2), task(3), task(4)]);
    }

    /// P2-03: a shrunk global capacity must not stay inflated to the old
    /// backlog forever. Shrink below the backlog, drain it, and the *new*
    /// (smaller) target is what admits and refuses further enqueues.
    #[test]
    fn shrunk_global_capacity_converges_after_the_backlog_drains() {
        let g = group(1);
        let mut q = FairQueue::new(4);
        for n in 1..=3 {
            q.enqueue(&g, task(n)).unwrap();
        }
        q.set_capacity(2);
        assert_eq!(
            q.capacity(),
            2,
            "the requested target is stored, not max(requested, backlog)"
        );
        assert_eq!(q.len(), 3, "the live backlog is kept, not dropped");
        // At/above the target: refused even though the old capacity was 4.
        assert_eq!(q.enqueue(&g, task(4)), Err(QueueError::QueueFull));
        // Drain the oversized backlog completely.
        for n in 1..=3 {
            assert_eq!(q.pop_for(&g), Some(task(n)));
        }
        // The new capacity now binds: two admissions, the third refused.
        assert!(q.enqueue(&g, task(5)).is_ok());
        assert!(q.enqueue(&g, task(6)).is_ok());
        assert_eq!(q.enqueue(&g, task(7)), Err(QueueError::QueueFull));
        assert_eq!(q.len(), 2);
    }

    // ---- wave 24: the per-group queue bound ------------------------------

    #[test]
    fn per_group_bound_rejects_at_the_boundary_without_touching_others() {
        let a = group(1);
        let mut q = FairQueue::with_caps(16, 2);
        assert_eq!(q.per_group_capacity(), 2);
        assert!(q.enqueue(&a, task(1)).is_ok());
        assert!(q.enqueue(&a, task(2)).is_ok());
        // Third item of the SAME group is refused even though the global
        // queue is nearly empty.
        assert_eq!(q.enqueue(&a, task(3)), Err(QueueError::QueueFull));
        assert_eq!(q.len(), 2, "the refused enqueue changed nothing");
        // A different group is unaffected by group 1's saturation.
        assert!(q.enqueue(&group(2), task(3)).is_ok());
        // Freeing one slot of the hot group admits exactly one more.
        assert!(q.cancel(&task(1)));
        assert!(q.enqueue(&a, task(4)).is_ok());
        assert_eq!(q.enqueue(&a, task(5)), Err(QueueError::QueueFull));
    }

    #[test]
    fn global_bound_still_fires_first_when_tighter() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::with_caps(3, 512); // per-group bound never fires
        for n in 1..=3 {
            q.enqueue(&a, task(n)).unwrap();
        }
        assert_eq!(q.enqueue(&b, task(9)), Err(QueueError::QueueFull));
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn set_per_group_capacity_never_drops_or_reorders() {
        let a = group(1);
        let mut q = FairQueue::with_caps(16, 4);
        for n in 1..=3 {
            q.enqueue(&a, task(n)).unwrap();
        }
        // Shrinking below the live group backlog keeps the backlog queued
        // and binds new enqueues of that group at the requested target.
        q.set_per_group_capacity(1);
        assert_eq!(
            q.per_group_capacity(),
            1,
            "the requested target is stored, not max(requested, backlog)"
        );
        assert_eq!(q.len(), 3);
        assert_eq!(q.enqueue(&a, task(4)), Err(QueueError::QueueFull));
        // Raising the bound admits more of the same group.
        q.set_per_group_capacity(5);
        assert!(q.enqueue(&a, task(4)).is_ok());
        let mut popped = Vec::new();
        while let Some(t) = q.pop_for(&a) {
            popped.push(t);
        }
        assert_eq!(popped, vec![task(1), task(2), task(3), task(4)]);
    }

    /// P2-03: a shrunk per-group capacity converges to the new value once
    /// the oversized group backlog drains, and never blocks other groups.
    #[test]
    fn shrunk_per_group_capacity_converges_after_the_backlog_drains() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::with_caps(16, 4);
        for n in 1..=3 {
            q.enqueue(&a, task(n)).unwrap();
        }
        q.set_per_group_capacity(1);
        assert_eq!(q.len(), 3, "the live backlog is kept, not dropped");
        assert_eq!(q.enqueue(&a, task(4)), Err(QueueError::QueueFull));
        // Other groups queue at the new bound while group a is oversized.
        assert!(q.enqueue(&b, task(11)).is_ok());
        // Drain group a completely.
        for n in 1..=3 {
            assert_eq!(q.pop_for(&a), Some(task(n)));
        }
        // The new per-group bound now binds: one admission, the second
        // refused even though the old bound was 4.
        assert!(q.enqueue(&a, task(5)).is_ok());
        assert_eq!(q.enqueue(&a, task(6)), Err(QueueError::QueueFull));
        assert_eq!(q.group_len(&a), 1);
        assert_eq!(q.group_len(&b), 1);
    }

    #[test]
    fn requeue_defer_keeps_head_but_rotates_the_group_to_the_back() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::new(8);
        for n in 1..=2 {
            q.enqueue(&a, task(n)).unwrap();
        }
        q.enqueue(&b, task(11)).unwrap();
        // Ring before: [a, b]. Pop a's head (a rotates to the back), then
        // defer it: the task returns to a's head, a stays at the back.
        let t = q.pop_for(&a).unwrap();
        q.requeue_defer(&a, t);
        assert_eq!(q.ready_order(), vec![b, a]);
        assert_eq!(q.pop_next().unwrap(), (b, task(11)));
        assert_eq!(
            q.pop_next().unwrap(),
            (a, task(1)),
            "the deferred task still heads its group's FIFO"
        );
        assert_eq!(q.pop_next().unwrap(), (a, task(2)));
    }

    #[test]
    fn requeue_defer_re_adds_a_retired_group_at_the_back() {
        let a = group(1);
        let b = group(2);
        let mut q = FairQueue::new(8);
        q.enqueue(&a, task(1)).unwrap();
        q.enqueue(&b, task(11)).unwrap();
        // Popping a's only task retires a from the ring; deferring must
        // re-add it behind b, not in front of it.
        let t = q.pop_for(&a).unwrap();
        q.requeue_defer(&a, t);
        assert_eq!(q.ready_order(), vec![b, a]);
        assert_eq!(q.pop_next().unwrap(), (b, task(11)));
        assert_eq!(q.pop_next().unwrap(), (a, task(1)));
    }
}
