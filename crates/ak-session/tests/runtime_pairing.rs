//! The injected `PairingTransport` surface: the redeem→confirm handshake
//! runs on the send path before the executor, offline failures feed the
//! backoff machinery, and the claim/redeem/pair method families behave as
//! the A3 client will (recorded calls, scripted online/offline).
mod common;

use ak_core::error::ErrorCode;
use ak_session::runtime::{
    InviteClaimRequest, PairingPlan, PairingTransport, RedeemPayload, RetryPolicy, TransferSpec,
};
use ak_session::scheduler::{CrossCodeMode, SameCodeMode, TaskState};
use common::{env_with, env_with_config, group, policy, run_until, send_spec, state_of};

fn redeem_request(deadline_ms: u64) -> RedeemPayload {
    RedeemPayload {
        request_id: [1; 16],
        lookup_digest: [2; 32],
        redeemer_nonce: [3; 32],
        deadline_ms,
        request_mac: [4; 32],
        signature: [5; 64],
    }
}

/// A spec that must redeem a code and finish the pairing transcript before
/// opening the data session.
fn redeem_spec(
    task: u64,
    code: u8,
    executor: &std::sync::Arc<ak_session::runtime::FakeTransferExecutor>,
) -> TransferSpec {
    let mut spec = send_spec(task, group(code), executor);
    spec.pairing = PairingPlan::Redeem {
        request: redeem_request(9_000),
    };
    spec
}

/// The send path drives `redeem` + `confirm_pairing` (A3 transcript)
/// before `open_session` and the executor.
#[tokio::test]
async fn redeem_path_runs_handshake_then_transfer() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;
    let task = env
        .handle
        .enqueue(redeem_spec(1, 3, &env.executor))
        .await
        .unwrap();

    let snap = run_until(&env, |s| state_of(s, task) == TaskState::Completed, 2_000).await;
    assert_eq!(state_of(&snap, task), TaskState::Completed);
    assert_eq!(env.transport.redeem_calls().len(), 1, "redeem called once");
    assert_eq!(
        env.transport.confirm_calls().len(),
        1,
        "confirm called once"
    );
    let opens = env.transport.open_calls();
    assert_eq!(opens.len(), 1, "one data session");
    assert_eq!(
        opens[0].1,
        group(3).code_id,
        "session opened for the task's code"
    );
    assert_eq!(env.executor.start_count(), 1);
}

/// An offline transport fails the attempt with `DIRECT_UNAVAILABLE`; the
/// task backs off, and once the transport is back the retry completes.
#[tokio::test]
async fn offline_transport_fails_then_recovers_after_reconnect() {
    let retry = RetryPolicy {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 1_000,
        multiplier: 2,
    };
    let env = env_with_config(
        policy(SameCodeMode::Serial, CrossCodeMode::Parallel, 4, 4, 64),
        |config| config.retry = retry,
    )
    .await;
    env.transport.set_online(false);
    let task = env
        .handle
        .enqueue(send_spec(1, group(1), &env.executor))
        .await
        .unwrap();

    // Attempt 1 dies at open_session with DIRECT_UNAVAILABLE; the retry
    // waits out its backoff on the fake clock.
    let snap = run_until(
        &env,
        |s| {
            let view = common::view_of(s, task);
            view.attempts == 1 && !view.state.is_terminal()
        },
        1_000,
    )
    .await;
    assert_eq!(
        common::view_of(&snap, task).last_error,
        Some(ErrorCode::DirectUnavailable)
    );
    assert_eq!(
        env.executor.start_count(),
        0,
        "engine never saw the offline attempt"
    );
    assert!(!env.transport.is_connected());

    // Reconnect; the backoff retry succeeds end to end.
    env.transport.set_online(true);
    let snap = run_until(&env, |s| state_of(s, task) == TaskState::Completed, 3_000).await;
    assert_eq!(state_of(&snap, task), TaskState::Completed);
    assert_eq!(
        env.executor.start_count(),
        1,
        "exactly one engine attempt after recovery"
    );
    assert_eq!(env.transport.open_calls().len(), 1);
}

/// The claim/redeem/pair method families behave as the A3 client will:
/// calls are recorded, and offline requests fail without blocking.
#[tokio::test]
async fn claim_redeem_surface_records_calls_and_fails_offline() {
    let env = env_with(policy(
        SameCodeMode::Serial,
        CrossCodeMode::Parallel,
        4,
        4,
        64,
    ))
    .await;

    let claim = InviteClaimRequest {
        lookup_digest: [9; 32],
        ttl_secs: 600,
    };
    let claimed = env
        .transport
        .claim_invite(claim.clone())
        .await
        .expect("claim succeeds online");
    assert_ne!(
        claimed.code_id,
        group(0).code_id,
        "server assigned a code id"
    );
    assert_eq!(claimed.revision, 1);
    assert_eq!(env.transport.claim_calls(), vec![claim]);
    assert!(env.transport.is_connected());

    let accepted = env
        .transport
        .redeem(redeem_request(1_000))
        .await
        .expect("redeem succeeds online");
    assert_eq!(env.transport.redeem_calls().len(), 1);
    assert_eq!(
        env.transport.redeem_calls()[0].deadline_ms,
        accepted.deadline_ms,
        "deadline mirrors the request"
    );

    // Offline: bounded refusal instead of blocking.
    env.transport.set_online(false);
    let err = env.transport.claim_invite(InviteClaimRequest {
        lookup_digest: [1; 32],
        ttl_secs: 60,
    });
    assert_eq!(
        err.await.unwrap_err().error_code(),
        ErrorCode::DirectUnavailable
    );
    assert_eq!(
        env.transport.claim_calls().len(),
        1,
        "offline claim not recorded as a call"
    );
}
