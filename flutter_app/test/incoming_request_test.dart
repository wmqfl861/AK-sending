/// Wave-28 receive prompts: the incoming-request banner (accept/reject),
/// receive progress, the success card with the save path, and the
/// failure card. Driven by the fake's `emitIncomingRequest` +
/// `finishIncoming`/`failIncoming` knobs.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'w28_test_helpers.dart';

void main() {
  testWidgets('accept: prompt → progress → success with the save path',
      (tester) async {
    final fake = await pumpApp(tester);

    fake.emitIncomingRequest(fileCount: 3, totalBytes: 2 * 1024 * 1024);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('incoming_host')), findsOneWidget);
    expect(find.text('对方请求发送 3 个文件（共 2.0 MB）'), findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_accept_job-1')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_reject_job-1')),
        findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('incoming_accept_job-1')));
    await tester.pump();
    expect(
        find.byKey(const ValueKey('incoming_progress_job-1')), findsOneWidget);
    expect(find.text('正在接收…'), findsOneWidget);

    fake.finishIncoming('job-1');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_success_job-1')),
        findsOneWidget);
    expect(
        find.textContaining('已接收 3 个文件，保存在：'), findsOneWidget);
    // The save path is the service's current save dir.
    expect(find.textContaining('Download/AK-接收'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('incoming_dismiss_job-1')));
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsNothing);
  });

  testWidgets('reject: banner disappears and the consent refusal reaches '
      'the service (nothing is received)', (tester) async {
    final fake = await pumpApp(tester);

    fake.emitIncomingRequest(fileCount: 1, totalBytes: 42);
    await pumpEvents(tester);
    await tester.pump();

    await tester.tap(find.byKey(const ValueKey('incoming_reject_job-1')));
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsNothing);
    expect(fake.consentRequests.single, ('job-1', false));
    expect(fake.consentRequests.single.$2, isFalse);
  });

  testWidgets('receive failure surfaces a readable reason',
      (tester) async {
    final fake = await pumpApp(tester);

    fake.emitIncomingRequest(fileCount: 5, totalBytes: 10);
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('incoming_accept_job-1')));
    await tester.pump();

    fake.failIncoming('job-1', '对端中断了连接');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_error_job-1')),
        findsOneWidget);
    expect(find.textContaining('对端中断了连接'), findsOneWidget);

    await tester.tap(
        find.byKey(const ValueKey('incoming_error_dismiss_job-1')));
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsNothing);
  });

  testWidgets('P2-11: the 30 s window closing removes the banner; a '
      'reject racing the expiry never throws', (tester) async {
    final fake = await pumpApp(tester);

    fake.emitIncomingRequest(fileCount: 2, totalBytes: 2048);
    await pumpEvents(tester);
    await tester.pump();
    expect(
        find.byKey(const ValueKey('incoming_reject_job-1')), findsOneWidget);

    // The engine auto-declined (window closed): the banner closes.
    fake.expireIncoming('job-1');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsNothing);

    // Race shape: the user taps reject BEFORE the expiry event reaches
    // the host — the decline must be contained, no unhandled exception.
    fake.emitIncomingRequest(jobId: 'late', fileCount: 1, totalBytes: 1);
    await pumpEvents(tester);
    await tester.pump();
    fake.expireIncoming('late');
    await tester.tap(find.byKey(const ValueKey('incoming_reject_late')));
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsNothing);
    expect(tester.takeException(), isNull);
    expect(fake.consentRequests.last, ('late', false));
  });

  testWidgets('two concurrent offers render stacked banners',
      (tester) async {
    final fake = await pumpApp(tester);

    fake.emitIncomingRequest(jobId: 'a', fileCount: 1, totalBytes: 1);
    fake.emitIncomingRequest(jobId: 'b', fileCount: 2, totalBytes: 2);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('incoming_accept_a')), findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_accept_b')), findsOneWidget);

    // Answer the first; the second stays pending.
    await tester.tap(find.byKey(const ValueKey('incoming_accept_a')));
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_progress_a')), findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_accept_b')), findsOneWidget);
  });
}
