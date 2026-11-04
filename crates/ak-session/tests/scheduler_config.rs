//! Policy parsing, validation, JSON round-trip and serial-override tests
//! (contract `scheduler.md` §2/§3).
use ak_session::scheduler::{
    CrossCodeMode, PolicyError, SameCodeMode, SchedulerPolicy, MAX_ACTIVE_CODE_CONNECTIONS,
    MAX_ACTIVE_TASKS_PER_CODE, MAX_QUEUED_TASKS,
};

/// The exact JSON document of contract §3 (all seven frozen fields).
const CONTRACT_SAMPLE: &str = "{\n  \"same_code_mode\": \"serial\",\n  \"cross_code_mode\": \"parallel\",\n  \"max_active_tasks\": 8,\n  \"max_active_tasks_per_code\": 4,\n  \"max_active_code_groups\": 4,\n  \"max_queued_tasks\": 256,\n  \"max_queued_tasks_per_code\": 64,\n  \"max_pending_admissions\": 32\n}";

#[test]
fn defaults_follow_the_contract() {
    let policy = SchedulerPolicy::default();
    assert_eq!(policy.same_code_mode, SameCodeMode::Serial);
    assert_eq!(policy.cross_code_mode, CrossCodeMode::Parallel);
    assert_eq!(policy.max_active_tasks_per_code, 4);
    assert_eq!(policy.max_active_code_connections, 4);
    assert_eq!(policy.max_queued_tasks, 256);
    assert!(policy.validate().is_ok());
}

#[test]
fn constructor_validates_frozen_ranges() {
    // Lower bounds accepted.
    assert!(SchedulerPolicy::new(SameCodeMode::Serial, CrossCodeMode::Parallel, 1, 1, 1).is_ok());
    // Upper bounds accepted.
    assert!(SchedulerPolicy::new(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        MAX_ACTIVE_TASKS_PER_CODE,
        MAX_ACTIVE_CODE_CONNECTIONS,
        MAX_QUEUED_TASKS
    )
    .is_ok());
    // One step beyond each bound is rejected with the offending field/value.
    for (per_code, connections, queued, field, value) in [
        (0u32, 4u32, 256u32, "max_active_tasks_per_code", 0u32),
        (17, 4, 256, "max_active_tasks_per_code", 17),
        (4, 0, 256, "max_active_code_connections", 0),
        (4, 17, 256, "max_active_code_connections", 17),
        (4, 4, 0, "max_queued_tasks", 0),
        (4, 4, 4097, "max_queued_tasks", 4097),
    ] {
        let err = SchedulerPolicy::new(
            SameCodeMode::Parallel,
            CrossCodeMode::Parallel,
            per_code,
            connections,
            queued,
        )
        .unwrap_err();
        assert_eq!(
            err,
            PolicyError::OutOfRange { field, value },
            "case per_code={per_code} connections={connections} queued={queued}"
        );
    }
}

#[test]
fn json_round_trip_uses_frozen_field_names() {
    let policy =
        SchedulerPolicy::new(SameCodeMode::Parallel, CrossCodeMode::Serial, 7, 3, 99).unwrap();
    let json = policy.to_json();
    assert_eq!(
        json,
        "{\"same_code_mode\":\"parallel\",\"cross_code_mode\":\"serial\",\
         \"max_active_tasks_per_code\":7,\"max_active_code_groups\":3,\
         \"max_queued_tasks\":99}"
    );
    assert_eq!(SchedulerPolicy::from_json(&json).unwrap(), policy);
    // Default document is byte-stable.
    assert_eq!(
        SchedulerPolicy::default().to_json(),
        "{\"same_code_mode\":\"serial\",\"cross_code_mode\":\"parallel\",\
         \"max_active_tasks_per_code\":4,\"max_active_code_groups\":4,\
         \"max_queued_tasks\":256}"
    );
}

#[test]
fn json_parses_the_full_contract_sample() {
    let policy = SchedulerPolicy::from_json(CONTRACT_SAMPLE).unwrap();
    assert_eq!(policy, SchedulerPolicy::default());
    // Whitespace-free variant parses identically.
    let compact = CONTRACT_SAMPLE.replace([' ', '\n'], "");
    assert_eq!(SchedulerPolicy::from_json(&compact).unwrap(), policy);
}

#[test]
fn json_missing_fields_fall_back_to_defaults() {
    let policy = SchedulerPolicy::from_json("{}").unwrap();
    assert_eq!(policy, SchedulerPolicy::default());
    let partial =
        SchedulerPolicy::from_json("{\"same_code_mode\":\"parallel\",\"max_queued_tasks\":8}")
            .unwrap();
    assert_eq!(partial.same_code_mode, SameCodeMode::Parallel);
    assert_eq!(partial.cross_code_mode, CrossCodeMode::Parallel);
    assert_eq!(partial.max_active_tasks_per_code, 4);
    assert_eq!(partial.max_active_code_connections, 4);
    assert_eq!(partial.max_queued_tasks, 8);
}

#[test]
fn json_rejects_unknown_fields() {
    let err = SchedulerPolicy::from_json("{\"max_relay\": 1}").unwrap_err();
    assert_eq!(
        err,
        PolicyError::UnknownField {
            field: "max_relay".to_owned()
        }
    );
    // Long hostile names are bounded in the error, control characters fold.
    let hostile = format!("{{\"{}\": 1}}", "x".repeat(200));
    match SchedulerPolicy::from_json(&hostile).unwrap_err() {
        PolicyError::UnknownField { field } => assert_eq!(field.len(), 40),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn json_rejects_duplicate_fields() {
    let err = SchedulerPolicy::from_json("{\"max_queued_tasks\": 1, \"max_queued_tasks\": 2}")
        .unwrap_err();
    assert_eq!(
        err,
        PolicyError::DuplicateField {
            field: "max_queued_tasks".to_owned()
        }
    );
}

#[test]
fn json_rejects_structural_garbage() {
    let documents = [
        "",
        "null",
        "[]",
        "\"serial\"",
        "{\"same_code_mode\": \"serial\",}",
        "{\"same_code_mode\": \"serial\" \"cross_code_mode\": \"parallel\"}",
        "{same_code_mode: serial}",
        "{\"same_code_mode\": 'serial'}",
        "{\"same_code_mode\": \"serial\"} trailing",
        "{\"max_queued_tasks\": 04}",
        "{\"max_queued_tasks\": 4.5}",
        "{\"max_queued_tasks\": 4e2}",
        "{\"max_queued_tasks\": -1}",
        "{\"max_queued_tasks\": \"4\"}",
        "{\"max_queued_tasks\": true}",
        "{\"max_queued_tasks\": null}",
        "{\"max_queued_tasks\": [4]}",
        "{\"same_code_mode\": \"seri\u{1}al\"}",
        "{\"same\\mode\": \"serial\"}",
        "{\"same_code_mode\": \"serial\"",
        "{\"max_queued_tasks\": 99999999999999999999}",
    ];
    for doc in documents {
        match SchedulerPolicy::from_json(doc) {
            Err(
                PolicyError::MalformedJson { .. }
                | PolicyError::InvalidMode { .. }
                | PolicyError::InvalidLimit { .. },
            ) => {}
            other => panic!("document {doc:?} unexpectedly parsed: {other:?}"),
        }
    }
}

#[test]
fn json_rejects_bad_values_with_specific_errors() {
    // Wrong mode word.
    assert_eq!(
        SchedulerPolicy::from_json("{\"same_code_mode\": \"SERIAL\"}").unwrap_err(),
        PolicyError::InvalidMode {
            field: "same_code_mode",
            value: "SERIAL".to_owned(),
        }
    );
    // Beyond u32.
    assert!(matches!(
        SchedulerPolicy::from_json("{\"max_queued_tasks\": 4294967296}").unwrap_err(),
        PolicyError::InvalidLimit {
            field: "max_queued_tasks",
            ..
        }
    ));
    // Inside u32 but outside the frozen range.
    assert_eq!(
        SchedulerPolicy::from_json("{\"max_queued_tasks\": 5000}").unwrap_err(),
        PolicyError::OutOfRange {
            field: "max_queued_tasks",
            value: 5000,
        }
    );
    // Forward-compatible fields are range-checked too.
    assert!(matches!(
        SchedulerPolicy::from_json("{\"max_active_tasks\": 65}").unwrap_err(),
        PolicyError::OutOfRange {
            field: "max_active_tasks",
            ..
        }
    ));
    assert!(matches!(
        SchedulerPolicy::from_json("{\"max_queued_tasks_per_code\": 513}").unwrap_err(),
        PolicyError::OutOfRange {
            field: "max_queued_tasks_per_code",
            ..
        }
    ));
    assert!(matches!(
        SchedulerPolicy::from_json("{\"max_queued_tasks\": 8, \"max_queued_tasks_per_code\": 9}")
            .unwrap_err(),
        PolicyError::OutOfRange {
            field: "max_queued_tasks_per_code",
            ..
        }
    ));
    assert!(matches!(
        SchedulerPolicy::from_json("{\"max_pending_admissions\": 0}").unwrap_err(),
        PolicyError::OutOfRange {
            field: "max_pending_admissions",
            ..
        }
    ));
}

#[test]
fn serial_mode_overrides_parallel_caps() {
    let both_parallel =
        SchedulerPolicy::new(SameCodeMode::Parallel, CrossCodeMode::Parallel, 16, 16, 256).unwrap();
    assert_eq!(both_parallel.effective_limits().max_tasks_per_group, 16);
    assert_eq!(both_parallel.effective_limits().max_active_groups, 16);

    let same_serial =
        SchedulerPolicy::new(SameCodeMode::Serial, CrossCodeMode::Parallel, 16, 16, 256).unwrap();
    assert_eq!(same_serial.effective_limits().max_tasks_per_group, 1);
    assert_eq!(same_serial.effective_limits().max_active_groups, 16);

    let cross_serial =
        SchedulerPolicy::new(SameCodeMode::Parallel, CrossCodeMode::Serial, 16, 16, 256).unwrap();
    assert_eq!(cross_serial.effective_limits().max_tasks_per_group, 16);
    assert_eq!(cross_serial.effective_limits().max_active_groups, 1);

    let both_serial =
        SchedulerPolicy::new(SameCodeMode::Serial, CrossCodeMode::Serial, 16, 16, 256).unwrap();
    assert_eq!(both_serial.effective_limits().max_tasks_per_group, 1);
    assert_eq!(both_serial.effective_limits().max_active_groups, 1);
}

#[test]
fn intersection_takes_the_stricter_side() {
    let loose = SchedulerPolicy::new(
        SameCodeMode::Parallel,
        CrossCodeMode::Parallel,
        16,
        16,
        4096,
    )
    .unwrap();
    let strict =
        SchedulerPolicy::new(SameCodeMode::Serial, CrossCodeMode::Serial, 2, 3, 64).unwrap();
    let mixed =
        SchedulerPolicy::new(SameCodeMode::Parallel, CrossCodeMode::Serial, 4, 4, 256).unwrap();

    let a = loose.intersect(&strict);
    assert_eq!(a.same_code_mode, SameCodeMode::Serial);
    assert_eq!(a.cross_code_mode, CrossCodeMode::Serial);
    assert_eq!(a.max_active_tasks_per_code, 2);
    assert_eq!(a.max_active_code_connections, 3);
    assert_eq!(a.max_queued_tasks, 64);

    let b = loose.intersect(&mixed);
    assert_eq!(b.same_code_mode, SameCodeMode::Parallel);
    assert_eq!(b.cross_code_mode, CrossCodeMode::Serial);
    assert_eq!(b.max_queued_tasks, 256);

    // Symmetric and idempotent.
    assert_eq!(strict.intersect(&loose), a);
    assert_eq!(a.intersect(&a), a);
}
