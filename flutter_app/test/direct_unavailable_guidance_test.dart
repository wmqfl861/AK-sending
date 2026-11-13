import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'w28_test_helpers.dart';

// Literal acceptance copy, independent of the production widget's strings.
const _approvedGuidance =
    '无法建立直连：两台设备所在的网络都不允许外部直接连入（例如运营商级 NAT）。'
    '可以让两台设备连接同一个 Wi-Fi，或让其中一台换用支持 IPv6 的网络。'
    '本应用不会通过服务器中转文件。';
const _helpContext = '连接帮助（尚未确认是否为以下网络限制）：';

Future<void> _tap(WidgetTester tester, String key) async {
  final target = find.byKey(ValueKey(key));
  await tester.ensureVisible(target);
  await tester.tap(target);
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
}

Future<void> _create(WidgetTester tester) async {
  await _tap(tester, 'home_create_code_button');
  await _tap(tester, 'role_send_button');
}

Future<void> _join(WidgetTester tester, String code) async {
  await _tap(tester, 'home_join_code_button');
  await tester.enterText(find.byKey(const ValueKey('join_code_field')), code);
  await tester.pump();
  await tester.tap(
    find.descendant(
      of: find.byKey(const ValueKey('join_role_selector')),
      matching: find.text('发送文件'),
    ),
  );
  await tester.pump();
  await _tap(tester, 'join_submit_button');
}

Future<void> _renderEvents(WidgetTester tester) async {
  await pumpEvents(tester);
  await tester.pump(const Duration(milliseconds: 400));
}

void _expectGuidance() {
  expect(find.text(_approvedGuidance), findsOneWidget);
  expect(
    find.text(_helpContext),
    findsOneWidget,
    reason: 'A failed connectivity probe does not prove the NAT cause.',
  );
  expect(find.textContaining('同一个 Wi-Fi'), findsOneWidget);
  expect(find.textContaining('支持 IPv6'), findsOneWidget);
  expect(find.textContaining('本应用不会通过服务器中转文件。'), findsOneWidget);
  final relay = RegExp('中转|中继|relay|TURN', caseSensitive: false);
  expect(
    find.descendant(
      of: find.byWidgetPredicate(
        (widget) => widget is ButtonStyleButton || widget is InkWell,
      ),
      matching: find.textContaining(relay),
    ),
    findsNothing,
    reason: 'Guidance must not introduce a relay action.',
  );
  expect(find.byTooltip(relay), findsNothing);
}

void _expectNoGuidance() {
  expect(find.text(_approvedGuidance), findsNothing);
  expect(find.text(_helpContext), findsNothing);
  expect(find.textContaining('运营商级 NAT'), findsNothing);
}

class _CreateFailureService extends LocalSimpleTransferService {
  _CreateFailureService(this.reason);

  final String reason;

  @override
  Future<TaskCode> createCode(TaskRole role) async => throw StateError(reason);
}

void main() {
  testWidgets('real adapter probe failure reaches home guidance; create retry '
      'and cancel still recover', (tester) async {
    await tester.binding.setSurfaceSize(kPhoneSize);
    addTearDown(() => tester.binding.setSurfaceSize(null));
    final calls = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: calls,
      registry: '127.0.0.1:7200',
      registryIdHex:
          '1111111111111111111111111111111111111111111111111111111111111111',
      idDir: 'test-fixtures/g3-identity',
      isAndroid: false,
      environment: () => const {},
    );
    try {
      await tester.pumpWidget(
        AkSendingApp(service: service, store: InMemorySaveDirStore()),
      );
      await tester.pump();
      await _create(tester);
      expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
      _expectNoGuidance();

      // Only the native call boundary is fake: production event parsing,
      // state mapping, flow controller and the app shell all run normally.
      final runtime = calls.runtimes.values.single;
      const connId = '22222222222222222222222222222222';
      runtime.events.add({
        'event': 'connected',
        'conn': connId,
        'role': 'sender',
        'peer_endpoint': '127.0.0.1:45678',
        'code': runtime.codes.single,
        'code_id': runtime.codeIdOfCode.values.single,
      });
      service.engine.pumpEvents();
      await _renderEvents(tester);
      expect(find.byKey(const ValueKey('flow_probing')), findsOneWidget);
      _expectNoGuidance();

      runtime.events.add({
        'event': 'probe_result',
        'conn': connId,
        'endpoint': null,
        'reachable': false,
        'failures': [
          {'candidate': '127.0.0.1:45678', 'reason': 'probe timed out'},
        ],
      });
      service.engine.pumpEvents();
      await _renderEvents(tester);
      expect(find.textContaining('连通测试未通过'), findsOneWidget);
      expect(find.byKey(const ValueKey('flow_success')), findsNothing);
      _expectGuidance();
      expect(tester.takeException(), isNull);

      await _tap(tester, 'flow_retry_button');
      expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
      _expectNoGuidance();
      await _tap(tester, 'flow_cancel_button');
      expect(
        find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget,
      );
      _expectNoGuidance();
    } finally {
      service.dispose();
    }
  });

  testWidgets('join probe failure offers guidance; retry reopens input and '
      'cancel dismisses it', (tester) async {
    final service = await pumpApp(tester, size: kPhoneSize);
    addTearDown(service.dispose);
    final code = await service.createCode(TaskRole.receive);
    await _join(tester, code.code);
    _expectNoGuidance();
    service.failProbe('conn-1');
    await _renderEvents(tester);
    _expectGuidance();
    expect(find.text('重新输入配对码'), findsOneWidget);
    expect(tester.takeException(), isNull);

    await _tap(tester, 'flow_retry_button');
    expect(find.byKey(const ValueKey('join_code_field')), findsOneWidget);
    _expectNoGuidance();
    await _tap(tester, 'join_cancel_button');
    expect(find.byKey(const ValueKey('home_join_code_button')), findsOneWidget);
  });

  for (final role in TaskRole.values) {
    testWidgets('unreachable ${role.name} detail has guidance and retains '
        'disconnect confirmation', (tester) async {
      final service = await pumpApp(tester, size: kPhoneSize);
      addTearDown(service.dispose);
      await service.createCode(role);
      final id = service.simulatePeerJoin();
      service.failProbe(id);
      await _renderEvents(tester);
      await _tap(tester, 'conn_card_name_$id');
      _expectGuidance();
      expect(find.textContaining('连通测试未通过'), findsOneWidget);
      expect(find.byKey(const ValueKey('conn_send_button')), findsNothing);
      if (role == TaskRole.send) {
        // CR-P1-07 (review 2026-11-13 §4.4 改法 2): a failed pairing-time
        // test no longer gates the send — the pick (and send) buttons stay
        // ENABLED; the copy says the send can still be tried.
        final pick = tester.widget<FilledButton>(
          find.byKey(const ValueKey('conn_pick_files_button')),
        );
        expect(pick.onPressed, isNotNull);
        expect(find.text('连通测试未通过，仍可尝试发送。'), findsOneWidget);
      } else {
        expect(find.text('连通测试未通过，仍可等待对方发送。'), findsOneWidget);
      }
      expect(tester.takeException(), isNull);

      await _tap(tester, 'conn_disconnect_button');
      await _tap(tester, 'conn_disconnect_cancel');
      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
      _expectGuidance();
      await _tap(tester, 'conn_disconnect_button');
      await _tap(tester, 'conn_disconnect_confirm');
      await tester.pumpAndSettle();
      expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
      expect(find.byKey(ValueKey('conn_card_name_$id')), findsNothing);
      _expectNoGuidance();
    });
  }

  testWidgets('a dropped formerly-unreachable connection does not leave NAT '
      'help on the home failure', (tester) async {
    final service = await pumpApp(tester);
    addTearDown(service.dispose);
    await _create(tester);
    final id = service.simulatePeerJoin();
    await _renderEvents(tester);
    service.failProbe(id);
    await _renderEvents(tester);
    _expectGuidance();

    service.simulateNetworkChange();
    await _renderEvents(tester);
    expect(find.textContaining('连接已断开'), findsOneWidget);
    _expectNoGuidance();
    await _tap(tester, 'flow_back_button');
    expect(
      find.byKey(const ValueKey('home_create_code_button')),
      findsOneWidget,
    );
  });

  testWidgets('detail guidance follows only the live unreachable state', (
    tester,
  ) async {
    final service = await pumpApp(tester);
    addTearDown(service.dispose);
    await service.createCode(TaskRole.send);
    final id = service.simulatePeerJoin();
    await _renderEvents(tester);
    await _tap(tester, 'conn_card_name_$id');
    _expectNoGuidance();
    service.beginProbe(id);
    await _renderEvents(tester);
    _expectNoGuidance();
    service.completeProbe(id);
    await _renderEvents(tester);
    _expectNoGuidance();
    service.failProbe(id);
    await _renderEvents(tester);
    _expectGuidance();
    service.simulateNetworkChange();
    await _renderEvents(tester);
    expect(find.textContaining('此连接已断开'), findsOneWidget);
    _expectNoGuidance();
  });

  for (final reason in ['身份认证失败', '读取本地身份失败：permission denied']) {
    testWidgets('create error retains its cause without NAT help: $reason', (
      tester,
    ) async {
      final service = _CreateFailureService(reason);
      addTearDown(service.dispose);
      await tester.binding.setSurfaceSize(kWideSize);
      addTearDown(() => tester.binding.setSurfaceSize(null));
      await tester.pumpWidget(
        AkSendingApp(service: service, store: InMemorySaveDirStore()),
      );
      await tester.pump();
      await _create(tester);
      expect(find.textContaining(reason), findsOneWidget);
      _expectNoGuidance();
      await _tap(tester, 'flow_back_button');
      expect(
        find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget,
      );
    });
  }

  for (final reason in [
    '任务码无效或已被使用，请确认后重试。',
    '身份认证失败',
    '对端身份已被替换，拒绝连接',
    '加入超时：请确认任务码后重试。',
  ]) {
    testWidgets('join error is not relabeled by an unrelated unreachable '
        'connection: $reason', (tester) async {
      final service = await pumpApp(tester);
      addTearDown(service.dispose);
      await service.createCode(TaskRole.send);
      final unrelated = service.simulatePeerJoin();
      service.failProbe(unrelated);
      await _renderEvents(tester);
      final code = await service.createCode(TaskRole.receive);
      service.nextJoinError = reason;
      await _join(tester, code.code);
      expect(find.text(reason), findsOneWidget);
      _expectNoGuidance();
      await _tap(tester, 'flow_retry_button');
      expect(find.byKey(const ValueKey('join_code_field')), findsOneWidget);
    });
  }

  for (final reason in [
    '对方拒绝接收文件（DECLINED）',
    '磁盘空间不足（STORAGE_FULL）',
    '没有文件读取权限（permission denied）',
    '对端身份已被替换，拒绝发送',
    '无法建立直连：连接超时',
  ]) {
    testWidgets('send error remains readable and clearable without NAT help: '
        '$reason', (tester) async {
      final picker = FakeFilePickGateway(filesResult: kTwoFiles);
      final service = await pumpApp(tester, picker: picker);
      addTearDown(service.dispose);
      await service.createCode(TaskRole.send);
      final id = service.simulatePeerJoin();
      service.completeProbe(id);
      await _renderEvents(tester);
      await _tap(tester, 'conn_card_name_$id');
      await _tap(tester, 'conn_pick_files_button');
      await _tap(tester, 'pick_files_option');
      await _tap(tester, 'conn_send_button');
      service.failSend(reason);
      await _renderEvents(tester);
      expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
      expect(find.textContaining(reason), findsOneWidget);
      _expectNoGuidance();
      final retry = tester.widget<FilledButton>(
        find.byKey(const ValueKey('conn_send_button')),
      );
      expect(retry.onPressed, isNotNull);
      await _tap(tester, 'picked_clear_button');
      expect(find.byKey(const ValueKey('conn_send_error')), findsNothing);
      _expectNoGuidance();
    });
  }
}
