import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:ak_sending_app/ui/widgets/empty_state.dart';
import 'package:ak_sending_app/ui/widgets/failure_advice.dart';
import 'package:ak_sending_app/ui/widgets/fingerprint_chip.dart';
import 'package:ak_sending_app/ui/widgets/task_card.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'test_helpers.dart';

void main() {
  group('TaskCard', () {
    testWidgets('shows the paused flag and the progress line while '
        'transferring', (tester) async {
      final controller = JobController();
      await pumpPage(
        tester,
        TaskCard(controller: controller, title: '发送任务'),
      );

      controller.feed(const Offered(job: 'j', files: 1, bytes: 4096));
      controller.feed(
          const FileStarted(job: 'j', file: 0, path: 'a.bin', bytes: 4096));
      controller.feed(const BytesAcked(job: 'j', file: 0, bytes: 1024));
      controller.feed(const Paused(job: 'j'));
      await tester.pump();

      expect(find.text('传输中'), findsOneWidget);
      expect(find.text('已暂停'), findsOneWidget);
      expect(find.textContaining('25%'), findsOneWidget);
      expect(find.text('a.bin'), findsOneWidget);
    });

    testWidgets('failed result renders the error banner with advice lines',
        (tester) async {
      final controller = JobController();
      await pumpPage(
        tester,
        TaskCard(controller: controller, title: '接收任务'),
      );

      controller.setResult(const JobFailed(
          job: 'j', error: 'dial unreachable: no route to host'));
      await tester.pump();

      expect(find.text('失败'), findsOneWidget);
      expect(find.text('dial unreachable: no route to host'), findsOneWidget);
      expect(find.text(kNoRelayNotice), findsOneWidget);
      expect(find.textContaining('IPv6'), findsOneWidget);
    });
  });

  group('FingerprintChip', () {
    testWidgets('shows the short id and expands to the full fingerprint',
        (tester) async {
      await pumpPage(
        tester,
        Center(child: FingerprintChip(fingerprintHex: 'AB' * 32)),
      );

      expect(find.text('指纹 abababab'), findsOneWidget);
      // Tap to expand: full fingerprint lowercased.
      await tester.tap(find.text('指纹 abababab'));
      await tester.pump();
      expect(find.text('ab' * 32), findsOneWidget);
    });

    testWidgets('marks an invalid fingerprint instead of throwing',
        (tester) async {
      await pumpPage(
        tester,
        const Center(child: FingerprintChip(fingerprintHex: 'xyz')),
      );
      expect(find.text('指纹无效'), findsOneWidget);
    });
  });

  group('EmptyState', () {
    testWidgets('renders icon, title, message and action', (tester) async {
      var pressed = 0;
      await pumpPage(
        tester,
        EmptyState(
          icon: Icons.inbox_outlined,
          title: '暂无任务',
          message: '开始一次传输后会显示在这里。',
          actionLabel: '去发送',
          onAction: () => pressed++,
        ),
      );
      expect(find.text('暂无任务'), findsOneWidget);
      expect(find.textContaining('开始一次传输'), findsOneWidget);
      await tester.tap(find.text('去发送'));
      expect(pressed, 1);
    });
  });

  testWidgets('failure advice: conflict and cancel branches',
      (tester) async {
    final controller = JobController();
    await pumpPage(
      tester,
      TaskCard(controller: controller, title: '发送任务'),
    );
    controller.setResult(const JobFailed(
        job: 'j', error: 'duplicate name conflict between roots'));
    await tester.pump();
    expect(find.textContaining('分开发送'), findsOneWidget);

    final cancelled = JobController();
    await pumpPage(
      tester,
      TaskCard(controller: cancelled, title: '发送任务'),
    );
    cancelled
        .setResult(const JobFailed(job: 'j2', error: 'cancelled by caller'));
    await tester.pump();
    expect(find.textContaining('.akpart'), findsOneWidget);
  });
}
