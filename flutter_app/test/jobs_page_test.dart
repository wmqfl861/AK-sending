import 'package:ak_sending_app/ui/pages/jobs_page.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

void main() {
  testWidgets('lists job history records from the service fixture',
      (tester) async {
    final fake = FakeTransferService(jobs: [
      JobRecord(
        id: '6c310412',
        direction: JobDirection.send,
        startedAt: DateTime(2026, 11, 3, 10, 30),
        state: JobRecordState.done,
        files: 12,
        bytes: 3 * 1024 * 1024 * 1024,
        summary: '对端 192.168.1.5:52341',
      ),
      JobRecord(
        id: 'abababab',
        direction: JobDirection.receive,
        startedAt: DateTime(2026, 11, 2, 18, 5),
        state: JobRecordState.failed,
        files: 0,
        bytes: 0,
        error: 'connect timeout after 30s',
      ),
    ]);
    await pumpPage(tester, JobsPage(service: fake));
    await tester.pump(const Duration(milliseconds: 1));

    expect(find.textContaining('发送 · 6c310412'), findsOneWidget);
    expect(find.textContaining('接收 · abababab'), findsOneWidget);
    expect(find.text('已完成'), findsOneWidget);
    expect(find.text('失败'), findsOneWidget);
    expect(find.textContaining('3.00 GiB'), findsOneWidget);
    expect(find.text('connect timeout after 30s'), findsOneWidget);
    expect(find.textContaining('2026-11-03 10:30'), findsOneWidget);
    expect(find.textContaining('2026-11-02 18:05'), findsOneWidget);
  });

  testWidgets('shows the empty state when there is no history',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, JobsPage(service: fake));
    await tester.pump(const Duration(milliseconds: 1));

    expect(find.text('还没有历史任务'), findsOneWidget);
    expect(find.textContaining('来自任务检查点'), findsOneWidget);
  });
}
