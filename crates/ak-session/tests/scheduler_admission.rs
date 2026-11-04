//! Exhaustive four-mode admission matrix (contract `scheduler.md` §2).
//!
//! Each mode is probed with at least three scenarios: the first task of a
//! fresh group, a concurrent request against an active set, and rejection
//! at the configured cap. A final data-driven test re-checks the whole
//! matrix in one table.
use ak_session::scheduler::{
    admission, ActiveGroups, ActiveTasks, AdmissionDecision, CodeGroupKey, CodeId, CrossCodeMode,
    DenyReason, RegistryId, SameCodeMode, SchedulerPolicy,
};

fn group(n: u8) -> CodeGroupKey {
    CodeGroupKey::new(RegistryId::from_bytes([n; 32]), CodeId::from_bytes([n; 16]))
}

fn policy(
    same: SameCodeMode,
    cross: CrossCodeMode,
    per_code: u32,
    connections: u32,
) -> SchedulerPolicy {
    SchedulerPolicy::new(same, cross, per_code, connections, 256).unwrap()
}

fn snapshot(
    groups: &[CodeGroupKey],
    counts: &[(CodeGroupKey, u32)],
) -> (ActiveGroups, ActiveTasks) {
    (
        ActiveGroups::from_keys(groups.iter().copied()),
        ActiveTasks::from_counts(counts.iter().copied()),
    )
}

// ---- Mode 1: serial / parallel (the default) -------------------------

#[test]
fn m1_first_task_of_a_fresh_group_admits() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4);
    let (groups, tasks) = snapshot(&[], &[]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(1)),
        AdmissionDecision::Admit
    );
}

#[test]
fn m1_concurrent_second_task_of_one_code_is_denied_serial() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

#[test]
fn m1_fifth_active_code_connection_is_denied_capacity() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    // Different codes still transfer concurrently up to the connection cap.
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Admit
    );
    let (groups, tasks) = snapshot(
        &[group(1), group(2), group(3), group(4)],
        &[(group(1), 1), (group(2), 1), (group(3), 1), (group(4), 1)],
    );
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(5)),
        AdmissionDecision::Deny(DenyReason::WaitingCapacity)
    );
    // A group that is already active bypasses the connection cap (it does
    // not open a new code connection).
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(3)),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

// ---- Mode 2: parallel / parallel --------------------------------------

#[test]
fn m2_first_task_of_a_fresh_group_admits() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel, 2, 3);
    let (groups, tasks) = snapshot(&[], &[]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(1)),
        AdmissionDecision::Admit
    );
}

#[test]
fn m2_concurrent_tasks_of_one_code_admit_up_to_the_cap() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel, 2, 3);
    let a = group(1);
    // One active: a second concurrent task still admits.
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Admit
    );
}

#[test]
fn m2_per_code_cap_rejects_with_capacity() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Parallel, 2, 3);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 2)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Deny(DenyReason::WaitingCapacity)
    );
    // Distinct groups are independent: a new group admits while a is full.
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Admit
    );
    // ... but the connection cap eventually rejects a fresh group.
    let (groups, tasks) = snapshot(
        &[group(1), group(2), group(3)],
        &[(group(1), 2), (group(2), 1), (group(3), 1)],
    );
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(4)),
        AdmissionDecision::Deny(DenyReason::WaitingCapacity)
    );
}

// ---- Mode 3: serial / serial -------------------------------------------

#[test]
fn m3_first_task_of_an_idle_system_admits() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Serial, 4, 4);
    let (groups, tasks) = snapshot(&[], &[]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(1)),
        AdmissionDecision::Admit
    );
}

#[test]
fn m3_second_task_of_the_active_group_is_denied_serial() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Serial, 4, 4);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

#[test]
fn m3_any_other_group_is_denied_while_one_group_is_active() {
    let policy = policy(SameCodeMode::Serial, CrossCodeMode::Serial, 4, 4);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
    // Once everything finishes, the next group admits.
    let (groups, tasks) = snapshot(&[], &[]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Admit
    );
}

// ---- Mode 4: parallel / serial -----------------------------------------

#[test]
fn m4_first_task_of_an_idle_system_admits() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Serial, 2, 4);
    let (groups, tasks) = snapshot(&[], &[]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(1)),
        AdmissionDecision::Admit
    );
}

#[test]
fn m4_tasks_inside_the_single_active_group_run_parallel() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Serial, 2, 4);
    let a = group(1);
    // Same group (the one active group): second task admits.
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Admit
    );
    // A different group must wait: only one active code group overall.
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

#[test]
fn m4_per_code_cap_still_rejects_inside_the_group() {
    let policy = policy(SameCodeMode::Parallel, CrossCodeMode::Serial, 2, 4);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 2)]);
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &a),
        AdmissionDecision::Deny(DenyReason::WaitingCapacity)
    );
    assert_eq!(
        admission::decide(&policy, &groups, &tasks, &group(2)),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

// ---- Overrides and the full matrix --------------------------------------

#[test]
fn serial_mode_overrides_the_parallel_caps() {
    // same-code serial wins even with a per-code cap of 16.
    let same_serial = policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 16, 16);
    let a = group(1);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&same_serial, &groups, &tasks, &a),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
    // cross-code serial wins even with a connection cap of 16.
    let cross_serial = policy(SameCodeMode::Parallel, CrossCodeMode::Serial, 16, 16);
    let (groups, tasks) = snapshot(&[a], &[(a, 1)]);
    assert_eq!(
        admission::decide(&cross_serial, &groups, &tasks, &group(2)),
        AdmissionDecision::Deny(DenyReason::WaitingSerialMode)
    );
}

#[test]
fn full_four_mode_matrix_table() {
    struct Case {
        same: SameCodeMode,
        cross: CrossCodeMode,
        groups: &'static [u8],
        counts: &'static [(u8, u32)],
        probe: u8,
        expect: Result<bool, DenyReason>,
    }
    let cases = [
        // (mode, active set, probe group, expected)
        // M1 serial/parallel: one task per code, codes parallel.
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Parallel,
            groups: &[],
            counts: &[],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Parallel,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 1,
            expect: Err(DenyReason::WaitingSerialMode),
        },
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Parallel,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 2,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Parallel,
            groups: &[1, 2, 3, 4],
            counts: &[(1, 1), (2, 1), (3, 1), (4, 1)],
            probe: 5,
            expect: Err(DenyReason::WaitingCapacity),
        },
        // M2 parallel/parallel: everything bounded by caps (per-code 2,
        // connections 3 below).
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Parallel,
            groups: &[],
            counts: &[],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Parallel,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Parallel,
            groups: &[1],
            counts: &[(1, 2)],
            probe: 1,
            expect: Err(DenyReason::WaitingCapacity),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Parallel,
            groups: &[1, 2, 3],
            counts: &[(1, 2), (2, 1), (3, 1)],
            probe: 4,
            expect: Err(DenyReason::WaitingCapacity),
        },
        // M3 serial/serial: one group, one task.
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Serial,
            groups: &[],
            counts: &[],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Serial,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 1,
            expect: Err(DenyReason::WaitingSerialMode),
        },
        Case {
            same: SameCodeMode::Serial,
            cross: CrossCodeMode::Serial,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 2,
            expect: Err(DenyReason::WaitingSerialMode),
        },
        // M4 parallel/serial: one group, parallel inside (per-code 2).
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Serial,
            groups: &[],
            counts: &[],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Serial,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 1,
            expect: Ok(true),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Serial,
            groups: &[1],
            counts: &[(1, 2)],
            probe: 1,
            expect: Err(DenyReason::WaitingCapacity),
        },
        Case {
            same: SameCodeMode::Parallel,
            cross: CrossCodeMode::Serial,
            groups: &[1],
            counts: &[(1, 1)],
            probe: 2,
            expect: Err(DenyReason::WaitingSerialMode),
        },
    ];
    for (index, case) in cases.iter().enumerate() {
        let per_code = if case.same == SameCodeMode::Serial {
            4
        } else {
            2
        };
        let connections = if case.cross == CrossCodeMode::Serial {
            4
        } else {
            3
        };
        let policy = policy(case.same, case.cross, per_code, connections);
        let group_keys: Vec<CodeGroupKey> = case.groups.iter().map(|n| group(*n)).collect();
        let counts: Vec<(CodeGroupKey, u32)> =
            case.counts.iter().map(|(n, c)| (group(*n), *c)).collect();
        let (groups, tasks) = snapshot(&group_keys, &counts);
        let decision = admission::decide(&policy, &groups, &tasks, &group(case.probe));
        let actual = match decision {
            AdmissionDecision::Admit => Ok(true),
            AdmissionDecision::Deny(reason) => Err(reason),
        };
        assert_eq!(actual, case.expect, "matrix case {index} failed");
    }
}
