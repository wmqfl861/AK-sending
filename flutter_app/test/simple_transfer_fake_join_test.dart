/// Batch X1 step 3 (W-N7): the UI fake's `joinCode` models the
/// CROSS-DEVICE join — a missing role resolves to the engine's v1
/// receiver default, and any same-direction join (explicit or
/// defaulted) is refused BEFORE the code is consumed with the engine's
/// unified unavailable answer verbatim. Grounded in the Rust tests
/// `production_owner_loop_refuses_same_direction_joins_before_commit`
/// (crates/ak-ffi/src/task/task_tests.rs:2913) and
/// `task_codes_relay_across_engines_via_the_registry` (:1405), and the
/// engine sources `run_join` (crates/ak-ffi/src/task/join_send.rs:23)
/// / `veto_task_offer`
/// (crates/ak-session/src/pairing/task.rs:980; answer text from
/// crates/ak-session/src/pairing/task_net.rs:651).
library;

import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;

void main() {
  test('X1/W-N7: a same-direction join is refused pre-commit with the '
      'engine answer, books nothing, and the code still redeems', () async {
    final fake = LocalSimpleTransferService();
    addTearDown(fake.dispose);
    final code = await fake.createCode(TaskRole.receive);

    // The no-hint default is the engine's v1 receiver default — on a
    // 「我要接收」 code that is a same-direction pair and must be
    // refused (phase 2 of the pinned Rust test: the cross-engine
    // default-receiver join of a receiver code never commits).
    final defaulted = await fake.joinCode(code.code);
    expect(defaulted.ok, isFalse);
    expect(defaulted.error,
        'unknown task code (or expired, revoked, owner offline)');

    // An explicit same-direction declaration is equally refused
    // (phase 1 of the same Rust test).
    final explicit =
        await fake.joinCode(code.code, role: TaskRole.receive);
    expect(explicit.ok, isFalse);
    expect(explicit.error,
        'unknown task code (or expired, revoked, owner offline)');

    // The refusal booked NOTHING: no connection row exists, the code is
    // still pending, and its single redeem survives.
    expect(fake.connectionsSnapshot, isEmpty);
    expect(fake.pendingCode?.code, code.code);

    // A complementary join of the SAME code pairs (a consumed code
    // would answer the unknown-code failure instead).
    final joined = await fake.joinCode(code.code, role: TaskRole.send);
    expect(joined.ok, isTrue, reason: '被拒的兑换不消耗码：互补方向仍能配对');
    expect(joined.connectionId, isNotNull);
    expect(fake.connectionsSnapshot.single.role, TaskRole.send);
    expect(fake.pendingCode, isNull); // consumed by the successful join
  });

  test('X1/W-N7: the cross-device no-hint default joins a sender code as '
      'receiver (the registry-relay positive path)', () async {
    final fake = LocalSimpleTransferService();
    addTearDown(fake.dispose);
    // Mirrors task_codes_relay_across_engines_via_the_registry
    // (crates/ak-ffi/src/task/task_tests.rs:1405): a sender-role code
    // joined with no role hint pairs with the joiner as RECEIVER.
    final code = await fake.createCode(TaskRole.send);
    final joined = await fake.joinCode(code.code);
    expect(joined.ok, isTrue);
    expect(fake.connectionsSnapshot.single.role, TaskRole.receive);
  });
}
