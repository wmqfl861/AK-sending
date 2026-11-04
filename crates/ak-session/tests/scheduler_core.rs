//! End-to-end core scenarios: admission through the queue, policy switches,
//! draining, cancellation, pause/yield/requeue, permit recycling.
//!
//! Concurrency semantics are proven with activity time intervals (contract
//! §12: serial execution must be shown by non-overlapping intervals, not
//! just by final success).
use ak_core::error::ErrorCode;
use ak_session::scheduler::{
    CancelOutcome, CodeGroupKey, CodeId, CrossCodeMode, FinishOutcome, FinishResult, RegistryId,
    SameCodeMode, SchedulerCore, SchedulerEvent, SchedulerPolicy, TaskHandle, TaskId, TaskPhase,
    TaskState,
};

fn group(n: u8) -> CodeGroupKey {
    CodeGroupKey::new(RegistryId::from_bytes([n; 32]), CodeId::from_bytes([n; 16]))
}

fn task(n: u64) -> TaskHandle {
    TaskHandle::new(TaskId::from_counter(n))
}

fn policy(
    same: SameCodeMode,
    cross: CrossCodeMode,
    per_code: u32,
    connections: u32,
    queued: u32,
) -> SchedulerPolicy {
    SchedulerPolicy::new(same, cross, per_code, connections, queued).unwrap()
}

/// Drive one admitted task through the contract's happy-path phases.
fn run_to_transferring(core: &mut SchedulerCore, task: &TaskHandle) {
    assert!(core.report_phase(task, TaskPhase::WaitingDirect));
    assert!(core.report_phase(task, TaskPhase::AwaitingAdmission));
    assert!(core.report_phase(task, TaskPhase::Scanning));
    assert!(core.report_phase(task, TaskPhase::Transferring));
}

#[test]
fn enqueue_admits_first_task_with_ordered_events() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default());
    let (t1, a) = (task(1), group(1));
    core.enqueue(t1, a).unwrap();
    assert_eq!(core.active_total(), 1);
    assert_eq!(core.active_in_group(&a), 1);
    assert_eq!(core.live_permits(), 1);
    assert_eq!(core.task_state(&t1), Some(TaskState::WaitingPairing));
    let events = core.take_events();
    assert_eq!(events[0], SchedulerEvent::TaskQueued { task: t1, group: a });
    match &events[1] {
        SchedulerEvent::TaskAdmitted {
            task,
            group,
            permit,
        } => {
            assert_eq!(*task, t1);
            assert_eq!(*group, a);
            assert_eq!(core.permit_of(&t1), Some(*permit));
        }
        other => panic!("expected TaskAdmitted, got {other:?}"),
    }
    assert_eq!(events.len(), 2);
    assert_eq!(core.policy_revision(), 1);
    // Progress reports walk the contract edges and emit TaskStarted once.
    assert!(core.report_phase(&t1, TaskPhase::WaitingDirect));
    assert!(core.report_phase(&t1, TaskPhase::AwaitingAdmission));
    assert!(core.report_phase(&t1, TaskPhase::Scanning));
    let events = core.take_events();
    assert_eq!(events, vec![SchedulerEvent::TaskStarted { task: t1 }]);
    assert!(core.report_phase(&t1, TaskPhase::Transferring));
    assert!(core.take_events().is_empty(), "no duplicate TaskStarted");
}

#[test]
fn serial_same_code_waits_and_admits_after_finish_without_permit_leak() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default()); // serial/parallel
    let (t1, t2, a) = (task(1), task(2), group(1));
    core.enqueue(t1, a).unwrap();
    core.enqueue(t2, a).unwrap();
    // One active task per code; the second stays queued.
    assert_eq!(core.active_total(), 1);
    assert_eq!(core.task_state(&t2), Some(TaskState::Queued));
    assert!(core.task_in_queue(&t2));
    // Finishing recycles the permit to the queued task.
    core.take_events(); // drop the enqueue-time events
    assert_eq!(
        core.finish(&t1, FinishOutcome::Completed),
        Some(FinishResult::Completed)
    );
    assert_eq!(core.task_state(&t1), Some(TaskState::Completed));
    assert_eq!(core.task_state(&t2), Some(TaskState::WaitingPairing));
    assert_eq!(core.active_total(), 1);
    assert_eq!(
        core.live_permits(),
        1,
        "permit must be recycled, not leaked"
    );
    let permit1 = core.permit_of(&t1);
    let permit2 = core.permit_of(&t2);
    assert_ne!(permit1, permit2);
    // Events: finish of t1 then admission of t2.
    let events = core.take_events();
    assert!(matches!(events[0], SchedulerEvent::TaskFinished { .. }));
    assert!(matches!(events[1], SchedulerEvent::TaskAdmitted { .. }));
    // Completing the last task drains the active set to zero.
    core.finish(&t2, FinishOutcome::Completed).unwrap();
    assert_eq!(core.active_total(), 0);
    assert_eq!(core.live_permits(), 0);
    assert_eq!(core.active_group_count(), 0);
}

#[test]
fn parallel_same_code_fills_the_cap_and_refills_on_finish() {
    let p = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel, 2, 4, 16);
    let mut core = SchedulerCore::new(p);
    let a = group(1);
    for n in 1..=3 {
        core.enqueue(task(n), a).unwrap();
    }
    assert_eq!(core.active_in_group(&a), 2, "cap of two same-code tasks");
    assert_eq!(core.task_state(&task(3)), Some(TaskState::Queued));
    // Finishing one admits the third.
    core.finish(&task(1), FinishOutcome::Completed).unwrap();
    assert_eq!(core.active_in_group(&a), 2);
    assert_eq!(core.task_state(&task(3)), Some(TaskState::WaitingPairing));
    // Finishing both leaves the group empty but admits nothing more.
    core.finish(&task(2), FinishOutcome::Completed).unwrap();
    core.finish(&task(3), FinishOutcome::Completed).unwrap();
    assert_eq!(core.active_in_group(&a), 0);
    assert_eq!(core.live_permits(), 0);
}

#[test]
fn queue_full_rejects_without_disturbing_existing_tasks() {
    let p = policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 2);
    let mut core = SchedulerCore::new(p);
    let a = group(1);
    core.enqueue(task(1), a).unwrap(); // admitted
    core.enqueue(task(2), a).unwrap(); // queued (serial)
    core.enqueue(task(3), a).unwrap(); // queued: queue now full (2)
    let err = core.enqueue(task(4), a).unwrap_err();
    assert_eq!(err.to_string(), "QUEUE_FULL");
    assert_eq!(core.queued_len(), 2);
    assert_eq!(core.task_state(&task(4)), None, "no record left behind");
    assert_eq!(core.task_state(&task(2)), Some(TaskState::Queued));
    assert_eq!(core.task_state(&task(3)), Some(TaskState::Queued));
    // Freeing a queue slot admits exactly one more enqueue.
    core.cancel(&task(2));
    core.enqueue(task(4), a).unwrap();
    assert_eq!(core.queued_len(), 2);
}

#[test]
fn cancel_propagation_differs_between_queued_and_running() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default()); // serial/parallel
    let (t1, t2, t3, a) = (task(1), task(2), task(3), group(1));
    core.enqueue(t1, a).unwrap(); // running
    core.enqueue(t2, a).unwrap(); // queued
    core.enqueue(t3, a).unwrap(); // queued

    // Cancel a queued task: no permit involved, nothing else moves.
    core.take_events(); // drop the enqueue-time events
    assert_eq!(core.cancel(&t2), CancelOutcome::RemovedQueued);
    assert_eq!(core.task_state(&t2), Some(TaskState::Cancelled));
    assert!(!core.task_in_queue(&t2));
    assert_eq!(core.active_total(), 1);
    assert_eq!(core.queued_len(), 1);
    let events = core.take_events();
    assert_eq!(
        events,
        vec![SchedulerEvent::TaskCancelled {
            task: t2,
            group: a,
            was_running: false
        }]
    );

    // Cancel the running task: its permit is released and the queued task
    // backfills immediately (propagation).
    assert_eq!(core.cancel(&t1), CancelOutcome::CancelledRunning);
    assert_eq!(core.task_state(&t1), Some(TaskState::Cancelled));
    assert_eq!(
        core.permit_of(&t1),
        None,
        "running cancel must release the permit"
    );
    assert_eq!(core.task_state(&t3), Some(TaskState::WaitingPairing));
    assert_eq!(core.active_total(), 1);
    assert_eq!(
        core.live_permits(),
        1,
        "the backfilled task now holds the recycled permit"
    );
    let events = core.take_events();
    assert_eq!(
        events[0],
        SchedulerEvent::TaskCancelled {
            task: t1,
            group: a,
            was_running: true
        }
    );
    assert!(matches!(events[1], SchedulerEvent::TaskAdmitted { .. }));

    // Terminal tasks reject further operations.
    assert_eq!(core.cancel(&t1), CancelOutcome::NotActive);
    assert_eq!(core.finish(&t1, FinishOutcome::Completed), None);
}

#[test]
fn parallel_to_serial_switch_drains_without_killing_running_tasks() {
    // Start parallel/parallel (per-code cap 2) with two same-code tasks
    // active and a third queued.
    let start = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel, 2, 4, 16);
    let mut core = SchedulerCore::new(start);
    let a = group(1);
    for n in 1..=3 {
        core.enqueue(task(n), a).unwrap();
    }
    run_to_transferring(&mut core, &task(1));
    run_to_transferring(&mut core, &task(2));
    assert_eq!(core.active_in_group(&a), 2);
    assert_eq!(core.queued_len(), 1);
    core.take_events();

    // Switch to serial/parallel: draining, in-flight tasks untouched.
    let switch = core.set_policy(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        16,
    ));
    assert_eq!(switch.revision, 2);
    assert!(switch.draining);
    assert_eq!(switch.admitted, 0);
    assert!(core.is_draining());
    assert_eq!(core.task_state(&task(1)), Some(TaskState::Transferring));
    assert_eq!(core.task_state(&task(2)), Some(TaskState::Transferring));
    assert_eq!(
        core.live_permits(),
        2,
        "draining must not revoke in-flight permits"
    );
    let events = core.take_events();
    assert_eq!(
        events[0],
        SchedulerEvent::PolicyChanged {
            revision: 2,
            draining: true
        }
    );
    assert_eq!(events[1], SchedulerEvent::Draining { active_tasks: 2 });
    assert_eq!(events.len(), 2);

    // New work is accepted into the queue but not admitted.
    core.enqueue(task(4), a).unwrap();
    assert_eq!(core.active_in_group(&a), 2);
    assert_eq!(core.task_state(&task(4)), Some(TaskState::Queued));

    // In-flight tasks finish naturally; the serial envelope converges.
    core.finish(&task(1), FinishOutcome::Completed).unwrap();
    assert_eq!(core.active_in_group(&a), 1);
    assert_eq!(
        core.task_state(&task(3)),
        Some(TaskState::Queued),
        "serial keeps denying while one task is active"
    );
    assert!(
        !core.is_draining(),
        "envelope is satisfied once at most one same-code task remains"
    );
    core.finish(&task(2), FinishOutcome::Completed).unwrap();
    assert!(!core.is_draining());
    assert_eq!(core.task_state(&task(3)), Some(TaskState::WaitingPairing));
    assert_eq!(core.active_in_group(&a), 1);
    assert_eq!(core.live_permits(), 1);
}

#[test]
fn serial_to_parallel_switch_backfills_immediately() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default()); // serial/parallel, per-code 4
    let a = group(1);
    for n in 1..=3 {
        core.enqueue(task(n), a).unwrap();
    }
    assert_eq!(core.active_in_group(&a), 1);
    assert_eq!(core.queued_len(), 2);
    core.take_events();
    let switch = core.set_policy(policy(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        4,
        4,
        16,
    ));
    assert!(!switch.draining);
    assert_eq!(
        switch.admitted, 2,
        "serial -> parallel backfills to the cap"
    );
    assert_eq!(core.active_in_group(&a), 3);
    assert_eq!(core.queued_len(), 0);
    let events = core.take_events();
    assert_eq!(
        events[0],
        SchedulerEvent::PolicyChanged {
            revision: 2,
            draining: false
        }
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, SchedulerEvent::TaskAdmitted { .. }))
            .count(),
        2
    );
}

#[test]
fn pause_releases_the_serial_slot_and_resume_requeues() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default()); // serial/parallel
    let (t1, t2, a) = (task(1), task(2), group(1));
    core.enqueue(t1, a).unwrap();
    core.enqueue(t2, a).unwrap();
    run_to_transferring(&mut core, &t1);

    // Pausing must not let a long pause monopolize the serial group.
    core.take_events(); // drop the enqueue-time events
    assert!(core.pause(&t1));
    assert_eq!(core.task_state(&t1), Some(TaskState::Paused));
    assert_eq!(core.permit_of(&t1), None, "paused task returns its permit");
    assert_eq!(
        core.task_state(&t2),
        Some(TaskState::WaitingPairing),
        "queued task backfills the freed slot"
    );
    assert_eq!(core.active_in_group(&a), 1);
    assert_eq!(core.live_permits(), 1);

    // A paused task cannot report progress.
    assert!(!core.report_phase(&t1, TaskPhase::Transferring));

    // Resume re-queues; with the other task active it waits again.
    assert_eq!(core.requeue(&t1), Ok(true));
    assert_eq!(core.task_state(&t1), Some(TaskState::Queued));
    core.finish(&t2, FinishOutcome::Completed).unwrap();
    assert_eq!(core.task_state(&t1), Some(TaskState::WaitingPairing));
    assert_eq!(core.live_permits(), 1);
}

#[test]
fn yield_slot_frees_capacity_while_waiting_for_a_direct_path() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default()); // serial/parallel
    let (t1, t2, a) = (task(1), task(2), group(1));
    core.enqueue(t1, a).unwrap();
    let first_permit = core.permit_of(&t1).unwrap();
    core.enqueue(t2, a).unwrap();
    // t1 admitted but stuck before the direct path exists.
    assert!(core.report_phase(&t1, TaskPhase::WaitingDirect));

    assert!(core.yield_slot(&t1));
    assert_eq!(
        core.task_state(&t1),
        Some(TaskState::WaitingDirect),
        "state kept, slot returned"
    );
    assert_eq!(core.permit_of(&t1), None, "yielded task returns its permit");
    assert_eq!(core.live_permits(), 1, "backfill task holds the slot now");
    assert_eq!(core.task_state(&t2), Some(TaskState::WaitingPairing));

    // A yielded task cannot progress without re-admission.
    assert!(!core.report_phase(&t1, TaskPhase::AwaitingAdmission));

    // Path returns: requeue brings t1 back through admission, and the
    // fresh admission carries a fresh permit id.
    assert_eq!(core.requeue(&t1), Ok(true));
    core.finish(&t2, FinishOutcome::Completed).unwrap();
    assert_eq!(core.task_state(&t1), Some(TaskState::WaitingPairing));
    assert_ne!(core.permit_of(&t1), Some(first_permit));
    // Yielding is only valid in waiting phases, not mid-transfer.
    run_to_transferring(&mut core, &t1);
    assert!(
        !core.yield_slot(&t1),
        "Transferring must keep its slot unless paused/cancelled"
    );
}

#[test]
fn report_phase_enforces_the_contract_edges() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default());
    let (t1, a) = (task(1), group(1));
    core.enqueue(t1, a).unwrap();

    // Illegal jumps out of order.
    assert!(!core.report_phase(&t1, TaskPhase::Transferring));
    assert!(!core.report_phase(&t1, TaskPhase::AwaitingAdmission));
    assert!(!core.report_phase(&t1, TaskPhase::Committing));
    // Legal chain up to the admission handshake.
    assert!(core.report_phase(&t1, TaskPhase::WaitingDirect));
    assert!(
        !core.report_phase(&t1, TaskPhase::Scanning),
        "must pass AwaitingAdmission first"
    );
    assert!(core.report_phase(&t1, TaskPhase::AwaitingAdmission));
    assert!(core.report_phase(&t1, TaskPhase::Scanning));
    // Source authorization lost: waiting phase, no progress from here.
    assert!(core.report_phase(&t1, TaskPhase::WaitingPermission));
    assert!(
        !core.report_phase(&t1, TaskPhase::Transferring),
        "permission wait needs a requeue"
    );
    // Re-authorization path: yield the held slot, then requeue (contract
    // edge WaitingPermission -> Queued).
    assert!(core.yield_slot(&t1));
    assert_eq!(core.active_total(), 0);
    assert_eq!(core.requeue(&t1), Ok(true));
    assert_eq!(
        core.task_state(&t1),
        Some(TaskState::WaitingPairing),
        "requeued task is admitted again"
    );
    core.finish(&t1, FinishOutcome::Completed).unwrap();
    assert_eq!(core.task_state(&t1), Some(TaskState::Completed));
    // Terminal states accept nothing.
    assert!(!core.report_phase(&t1, TaskPhase::CancelRequested));
}

#[test]
fn finish_failed_requeues_behind_and_keeps_the_stable_error() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default());
    let (t1, t2, a) = (task(1), task(2), group(1));
    core.enqueue(t1, a).unwrap();
    core.enqueue(t2, a).unwrap();
    run_to_transferring(&mut core, &t1);

    let result = core
        .finish(
            &t1,
            FinishOutcome::Failed {
                error: ErrorCode::HashMismatch,
            },
        )
        .unwrap();
    assert_eq!(result, FinishResult::FailedRequeued);
    assert_eq!(core.last_error(&t1), Some(ErrorCode::HashMismatch));
    assert_eq!(
        core.task_state(&t1),
        Some(TaskState::Queued),
        "failed task re-enters the queue"
    );
    // Fair FIFO: t2 was queued before the failure, so t2 is admitted next.
    assert_eq!(core.task_state(&t2), Some(TaskState::WaitingPairing));
    assert_eq!(core.task_state(&t1), Some(TaskState::Queued));
    assert_eq!(core.live_permits(), 1);

    // Giving up: cancel keeps the stable error for persistence.
    assert_eq!(core.cancel(&t1), CancelOutcome::RemovedQueued);
    assert_eq!(core.task_state(&t1), Some(TaskState::Cancelled));
    assert_eq!(core.last_error(&t1), Some(ErrorCode::HashMismatch));
}

#[test]
fn finish_completed_wins_the_race_against_cancel_request() {
    let mut core = SchedulerCore::new(SchedulerPolicy::default());
    let (t1, a) = (task(1), group(1));
    core.enqueue(t1, a).unwrap();
    run_to_transferring(&mut core, &t1);
    assert!(core.report_phase(&t1, TaskPhase::CancelRequested));
    assert_eq!(core.task_state(&t1), Some(TaskState::CancelRequested));
    // The final commit had already happened (contract §9 edge
    // CancelRequested -> Completed).
    assert_eq!(
        core.finish(&t1, FinishOutcome::Completed),
        Some(FinishResult::Completed)
    );
    assert_eq!(core.task_state(&t1), Some(TaskState::Completed));
    assert_eq!(
        core.cancel(&t1),
        CancelOutcome::NotActive,
        "no zombie cancellation after completion"
    );
}

#[test]
fn cross_code_serial_runs_groups_exclusively_but_tasks_parallel_inside() {
    let p = policy(SameCodeMode::Parallel, CrossCodeMode::Serial, 2, 4, 16);
    let mut core = SchedulerCore::new(p);
    let a = group(1);
    let b = group(2);
    for n in 1..=3 {
        core.enqueue(task(n), a).unwrap();
    }
    core.enqueue(task(4), b).unwrap();
    // Group a fills its internal parallelism; group b waits (one group).
    assert_eq!(core.active_in_group(&a), 2);
    assert_eq!(core.task_state(&task(3)), Some(TaskState::Queued));
    assert_eq!(core.task_state(&task(4)), Some(TaskState::Queued));
    core.finish(&task(1), FinishOutcome::Completed).unwrap();
    // a still active: b keeps waiting, a's third task admits.
    assert_eq!(core.task_state(&task(4)), Some(TaskState::Queued));
    assert_eq!(core.task_state(&task(3)), Some(TaskState::WaitingPairing));
    core.finish(&task(2), FinishOutcome::Completed).unwrap();
    core.finish(&task(3), FinishOutcome::Completed).unwrap();
    assert_eq!(core.task_state(&task(4)), Some(TaskState::WaitingPairing));
}

#[test]
fn churn_proves_serial_per_code_and_overlap_across_codes_with_intervals() {
    // serial/parallel: per-code intervals must never overlap; different
    // codes must genuinely overlap. (Contract §12: prove concurrency with
    // activity intervals, not final success.)
    let mut core = SchedulerCore::new(SchedulerPolicy::default());
    let a = group(1);
    let b = group(2);
    for n in 0..4u64 {
        core.enqueue(task(1 + 2 * n), a).unwrap();
        core.enqueue(task(2 + 2 * n), b).unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    struct Span {
        start: u64,
        end: u64,
    }
    // Counter of a task id built by `task(n)` (big-endian low bytes).
    fn counter_of(handle: &TaskHandle) -> u64 {
        let id = handle.task_id();
        let bytes = id.as_bytes();
        u64::from_be_bytes(bytes[8..16].try_into().unwrap())
    }

    let mut spans: Vec<(u64, CodeGroupKey, Span)> = Vec::new();
    let mut open: Vec<(TaskHandle, u64)> = Vec::new();
    let mut finished = 0usize;

    // Deterministic simulation: every admitted task runs for one tick;
    // the loop variable is the simulated clock.
    for clock in 0..64u64 {
        for event in core.take_events() {
            match event {
                SchedulerEvent::TaskAdmitted { task, .. } => {
                    open.push((task, clock));
                }
                SchedulerEvent::TaskFinished { task, group, .. } => {
                    let index = open.iter().position(|(t, _)| *t == task).unwrap();
                    let (_, start) = open.remove(index);
                    spans.push((counter_of(&task), group, Span { start, end: clock }));
                    finished += 1;
                }
                _ => {}
            }
        }
        if finished == 8 {
            break;
        }
        let admitted: Vec<TaskHandle> = open.iter().map(|(t, _)| *t).collect();
        for t in admitted {
            core.finish(&t, FinishOutcome::Completed).unwrap();
        }
    }
    assert_eq!(finished, 8);
    assert_eq!(core.active_total(), 0);
    assert_eq!(core.live_permits(), 0);
    assert_eq!(core.queued_len(), 0);

    let overlaps = |g: CodeGroupKey| {
        let in_group: Vec<&Span> = spans
            .iter()
            .filter(|(_, grp, _)| *grp == g)
            .map(|(_, _, span)| span)
            .collect();
        in_group.iter().enumerate().any(|(i, s1)| {
            in_group
                .iter()
                .enumerate()
                .any(|(j, s2)| i != j && s1.start < s2.end && s2.start < s1.end)
        })
    };
    assert!(
        !overlaps(a),
        "same-code tasks must be strictly serial: {spans:?}"
    );
    assert!(
        !overlaps(b),
        "same-code tasks must be strictly serial: {spans:?}"
    );
    // Cross-code overlap must exist at least once.
    let cross = spans.iter().any(|(_, g1, s1)| {
        spans
            .iter()
            .any(|(_, g2, s2)| g1 != g2 && s1.start < s2.end && s2.start < s1.end)
    });
    assert!(cross, "different codes must run in parallel: {spans:?}");
    // FIFO completion order within each group.
    for g in [a, b] {
        let order: Vec<u64> = spans
            .iter()
            .filter(|(_, grp, _)| *grp == g)
            .map(|(counter, _, _)| *counter)
            .collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "per-group completion must follow FIFO");
    }
}
