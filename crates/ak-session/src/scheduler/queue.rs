//! Bounded local task queue with per-group fairness.
//!
//! Contract §5/§7/§8 semantics implemented here:
//!
//! * **Bounded**: `enqueue` rejects with [`QueueError::QueueFull`] the moment
//!   the queue holds `capacity` items. The check is O(1) against a live
//!   length counter — accepting or rejecting an enqueue never scans the
//!   queued items (the plan calls this out explicitly: no scan, no
//!   connection, no silent drop of older tasks).
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
    /// The queue is at capacity; the enqueue was rejected without scanning
    /// and without touching any queued item (wire-facing name `QUEUE_FULL`,
    /// contract §7).
    #[error("QUEUE_FULL")]
    QueueFull,
    /// The task id is already queued; ids are unique per submission.
    #[error("duplicate task already queued")]
    DuplicateTask,
}

/// Bounded FIFO queue with group-level round-robin fairness.
///
/// Invariants (relied on by [`SchedulerCore`](crate::scheduler::core::SchedulerCore)):
///
/// * `ready` contains exactly the groups holding at least one queued task,
///   in rotation order;
/// * every queued task id maps to its group in `index` and appears exactly
///   once in that group's deque.
#[derive(Debug, Clone)]
pub struct FairQueue {
    capacity: usize,
    groups: HashMap<CodeGroupKey, VecDeque<TaskId>>,
    ready: VecDeque<CodeGroupKey>,
    index: HashMap<TaskId, CodeGroupKey>,
}

impl FairQueue {
    /// Create a queue accepting at most `capacity` items.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero; the frozen policy range guarantees
    /// `max_queued_tasks >= 1`.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "queue capacity must be at least 1");
        FairQueue {
            capacity,
            groups: HashMap::new(),
            ready: VecDeque::new(),
            index: HashMap::new(),
        }
    }

    /// Maximum number of queued items.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Adjust the capacity. Never below the current length: shrinking below
    /// the live backlog keeps the bound for *new* enqueues without dropping
    /// or reordering anything already queued (contract §7: no automatic
    /// dropping of old tasks).
    pub fn set_capacity(&mut self, capacity: usize) {
        assert!(capacity >= 1, "queue capacity must be at least 1");
        self.capacity = capacity.max(self.index.len());
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
    /// O(1): the capacity check compares the live length counter and never
    /// scans the queued items. A group appearing for the first time joins
    /// the back of the ready ring.
    pub fn enqueue(&mut self, group: &CodeGroupKey, task: TaskId) -> Result<(), QueueError> {
        if self.index.contains_key(&task) {
            return Err(QueueError::DuplicateTask);
        }
        if self.index.len() >= self.capacity {
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
        q.set_capacity(2); // below the live backlog: clamps to 3, keeps items
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
}
