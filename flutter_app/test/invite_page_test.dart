import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        InviteState,
        InviteSummary,
        PairingCompleteEvent,
        PeerPreparedEvent,
        RedeemFailedEvent,
        SchedulerPolicy,
        SchedCodeMode,
        ServiceOnlineEvent;

import 'package:ak_sending_app/services/invite_service.dart'
    show SchedulerPreference;
import 'package:ak_sending_app/ui/pages/invite_page.dart';
import 'package:flutter/material.dart'
    show IconButton, SelectableText, SwitchListTile, TextField, ValueKey;
import 'package:flutter_test/flutter_test.dart';

import 'fake_invite_service.dart';
import 'fake_transfer_service.dart';
import 'test_helpers.dart';

/// Wave-22 invite-page tests over [FakeInviteService] (surfaced through
/// the [FakeTransferService.inviteService] seam), asserting the UI key
/// contract frozen by `integration_test/invite_e2e_test.dart`: list rows
/// (masked hint / state / TTL line / revoke), the create journey (TTL
/// dialog → one-time code dialog), local Crockford redeem rejection,
/// revoke confirmation, the event feed, scheduler toggles with
/// persistence, and the unsupported/empty/error states.
void main() {
  InviteSummary row(
    String codeId, {
    InviteState state = InviteState.active,
    int expiresMs = 0,
    int revision = 1,
  }) =>
      InviteSummary(
        codeId: codeId,
        state: state,
        createdMs: 1730000000000,
        expiresMs: expiresMs,
        revision: revision,
      );

  /// Pumps the page resolving the invite service through the seam.
  Future<FakeTransferService> pumpInvitePage(
    WidgetTester tester,
    FakeInviteService invite,
  ) async {
    final fake = FakeTransferService()..inviteService = invite;
    await pumpPage(tester, InvitePage(transfer: fake));
    await tester.pump();
    return fake;
  }

  testWidgets('没有任何服务时渲染诚实的不可用卡片，不渲染表单', (tester) async {
    await pumpPage(tester, const InvitePage());
    await tester.pump();

    expect(find.text('邀请码配对不可用'), findsOneWidget);
    expect(find.textContaining('ak_pairing_*'), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_create_btn')), findsNothing);
    expect(find.byKey(const ValueKey('invite_code_input')), findsNothing);
    expect(find.byKey(const ValueKey('same_code_toggle')), findsNothing);
  });

  testWidgets('transfer.seam 为 null 时同样渲染不可用卡片', (tester) async {
    await pumpPage(tester, InvitePage(transfer: FakeTransferService()));
    await tester.pump();
    expect(find.text('邀请码配对不可用'), findsOneWidget);
  });

  testWidgets('列表渲染：掩码提示、状态文本、TTL 时间行与撤销按钮可用性', (tester) async {
    final activeCodeId = 'ab' * 16;
    final revokedCodeId = 'cd' * 16;
    final expiredCodeId = 'ef' * 16;
    final invite = FakeInviteService(invites: [
      row(activeCodeId),
      row(revokedCodeId, state: InviteState.revoked, revision: 3),
      row(expiredCodeId,
          state: InviteState.expired, expiresMs: 1730003600000),
    ]);
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('invite_page')), findsOneWidget);
    expect(find.byKey(ValueKey('invite_code_hint_$activeCodeId')),
        findsOneWidget);
    // 未捕获明文的行回退到 code_id 前缀提示。
    expect(
        find.text('${activeCodeId.substring(0, 8)}…'), findsOneWidget);
    expect(find.text('有效'), findsOneWidget);
    expect(find.text('已撤销'), findsOneWidget);
    expect(find.text('已过期'), findsOneWidget);
    expect(find.textContaining('长期有效'), findsNWidgets(2),
        reason: '两个 expires_ms=0 的行（活跃 + 已撤销）都渲染长期有效');
    expect(find.textContaining('1 小时有效'), findsOneWidget,
        reason: 'expires-created = 1 小时的行渲染有限 TTL');
    expect(find.byKey(const ValueKey('invite_loading')), findsNothing);
    // 终态行的撤销按钮禁用；活跃行可用。
    expect(
        tester
            .widget<IconButton>(
                find.byKey(ValueKey('invite_revoke_$revokedCodeId')))
            .onPressed,
        isNull);
    expect(
        tester
            .widget<IconButton>(
                find.byKey(ValueKey('invite_revoke_$activeCodeId')))
            .onPressed,
        isNotNull);
  });

  testWidgets('空列表渲染 invite_empty 空态；默认偏好 serial × parallel', (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('invite_empty')), findsOneWidget);
    expect(find.text('暂无邀请码'), findsOneWidget);
    expect(
        tester
            .widget<SwitchListTile>(
                find.byKey(const ValueKey('same_code_toggle')))
            .value,
        isFalse,
        reason: '默认同码串行');
    expect(
        tester
            .widget<SwitchListTile>(
                find.byKey(const ValueKey('cross_code_toggle')))
            .value,
        isTrue,
        reason: '默认跨码并行');
  });

  testWidgets('列表读取失败渲染内联错误（不是静默空列表）', (tester) async {
    final invite = FakeInviteService()
      ..listError = StateError('未找到 ak_pairing_* 导出（旧 DLL）');
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('invite_list_error')), findsOneWidget);
    expect(find.textContaining('邀请码列表读取失败'), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_empty')), findsNothing);
  });

  testWidgets('生成旅程：TTL 对话框 → 默认长期 → 明文码仅展示一次 → 行内只留掩码',
      (tester) async {
    final invite = FakeInviteService();
    final fake = await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('invite_create_btn')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('invite_ttl_long')), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_ttl_short')), findsOneWidget);

    // 默认选中长期，直接确认。
    await tester.tap(find.byKey(const ValueKey('invite_create_confirm')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(invite.createInviteCalls, 1);
    expect(invite.lastCreateShortTerm, isFalse, reason: '默认长期有效');
    final onceText = tester.widget<SelectableText>(
        find.byKey(const ValueKey('invite_code_once_text')));
    expect(onceText.data, invite.issuedCodes.single);
    expect(find.byKey(const ValueKey('invite_code_once_close')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('invite_code_once_close')));
    await tester.pumpAndSettle();

    // 明文不再出现；行内只显示掩码提示（前两组 + 省略号）。
    expect(find.text(invite.issuedCodes.single), findsNothing);
    final newCodeId = '1'.padLeft(32, '0');
    final hint = invite.issuedCodes.single.substring(0, 14);
    expect(find.byKey(ValueKey('invite_code_hint_$newCodeId')), findsOneWidget);
    expect(find.text('$hint…'), findsOneWidget);
    expect(find.text('有效'), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_empty')), findsNothing);
    expect(invite.listInvitesCalls, greaterThanOrEqualTo(2));
    expect(fake.policyRequests, isNotEmpty,
        reason: '打开页面时会把持久化偏好应用到运行时');
  });

  testWidgets('生成旅程：选择短期 1 小时后按 shortLived 生成；生成失败内联报错',
      (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('invite_create_btn')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('invite_ttl_short')));
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('invite_create_confirm')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(invite.lastCreateShortTerm, isTrue);
    expect(invite.invites.single.expiresMs, invite.clockMs + 3600 * 1000);
    // 关闭一次性明文对话框，让第二次生成可以点击按钮。
    await tester.tap(find.byKey(const ValueKey('invite_code_once_close')));
    await tester.pumpAndSettle();

    // 注入错误后的下一次生成。
    invite.createError = StateError('RATE_LIMITED');
    await tester.tap(find.byKey(const ValueKey('invite_create_btn')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('invite_create_confirm')));
    await tester.pump();
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('invite_create_error')), findsOneWidget);
    expect(find.textContaining('生成失败'), findsOneWidget);
  });

  testWidgets('兑换：畸形输入被本地拒绝且不触达服务、不产生事件', (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    for (final bad in [
      'AKI1-0123-4567-89oB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
      'АKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
      'AKI1-0123',
      'AKI1 0123 4567 89AB CDEF GHJK MNPQ RSTV WXYZ',
    ]) {
      await enterField(tester, const ValueKey('invite_code_input'), bad);
      await tester.tap(find.byKey(const ValueKey('invite_redeem_btn')));
      await tester.pump();
      expect(find.byKey(const ValueKey('invite_redeem_error')), findsOneWidget,
          reason: '输入 $bad 被本地拒绝');
      expect(invite.redeemInviteCalls, 0, reason: '不发任何请求');
    }
    // 事件 feed 不因本地拒绝而增加。
    expect(find.byKey(const ValueKey('invite_event_0')), findsNothing);
    // 有效但非规范的形式给出正面提示，且没有错误。
    await enterField(tester, const ValueKey('invite_code_input'),
        'aki10123456789abcdefghjkmnpqrstvwxyz');
    await tester.pump();
    expect(find.text('格式正确'), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_redeem_error')), findsNothing);
  });

  testWidgets('兑换成功：规范化分组形式触达服务，输入清空', (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    await enterField(tester, const ValueKey('invite_code_input'),
        'aki10123456789abcdefghjkmnpqrstvwxyz');
    await tester.tap(find.byKey(const ValueKey('invite_redeem_btn')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(invite.redeemInviteCalls, 1);
    expect(invite.lastRedeemCode,
        'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ',
        reason: '服务收到的是规范化分组形式');
    final field = tester
        .widget<TextField>(find.byKey(const ValueKey('invite_code_input')));
    expect(field.controller?.text, '', reason: '成功后清空输入框');
  });

  testWidgets('兑换失败渲染内联错误', (tester) async {
    final invite = FakeInviteService()
      ..redeemError = StateError('PAIR_UNAVAILABLE（码不可用或对方离线）');
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    await enterField(tester, const ValueKey('invite_code_input'),
        'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ');
    await tester.tap(find.byKey(const ValueKey('invite_redeem_btn')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(find.byKey(const ValueKey('invite_redeem_error')), findsOneWidget);
    expect(find.textContaining('兑换失败'), findsOneWidget);
    expect(find.textContaining('PAIR_UNAVAILABLE'), findsOneWidget);
  });

  testWidgets('撤销：确认弹层取消保留行，确认后 tombstone 保留并翻转状态', (tester) async {
    final codeId = 'ab' * 16;
    final revokeKey = ValueKey('invite_revoke_$codeId');
    final invite = FakeInviteService(invites: [row(codeId)]);
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(revokeKey));
    await tester.pumpAndSettle();
    expect(find.text('撤销邀请码'), findsOneWidget);

    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(invite.revokeInviteCalls, 0);
    expect(find.text('有效'), findsOneWidget);

    await tester.tap(find.byKey(revokeKey));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('invite_revoke_confirm')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(invite.lastRevokeCodeId, codeId);
    expect(find.text('已撤销'), findsOneWidget);
    expect(find.text('有效'), findsNothing);
    // tombstone 保留在列表（§8）。
    expect(find.byKey(ValueKey('invite_code_hint_$codeId')), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_action_error')), findsNothing);
  });

  testWidgets('事件流按到达顺序驱动实时 feed（event_0 最先到达）', (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    invite.emit(ServiceOnlineEvent(atMs: 1730000000000));
    await tester.pump();
    await tester.pump();
    expect(find.byKey(const ValueKey('invite_event_0')), findsOneWidget);
    expect(find.textContaining('配对服务已连接'), findsOneWidget);

    invite.emit(PeerPreparedEvent(pairingId: 'ef' * 16, atMs: 1730000001000));
    await tester.pump();
    invite.emit(PairingCompleteEvent(
      pairingId: 'ef' * 16,
      peer: 'cd' * 32,
      atMs: 1730000002000,
    ));
    await tester.pump();
    await tester.pump();
    expect(find.byKey(const ValueKey('invite_event_1')), findsOneWidget);
    expect(find.byKey(const ValueKey('invite_event_2')), findsOneWidget);
    expect(find.textContaining('配对事务已持久化'), findsOneWidget);
    expect(find.textContaining('配对完成 · 对端指纹 cdcdcdcd'), findsOneWidget);

    invite.emit(RedeemFailedEvent(error: 'RATE_LIMITED'));
    await tester.pump();
    await tester.pump();
    expect(find.textContaining('兑换失败 · RATE_LIMITED'), findsOneWidget);
  });

  testWidgets('调度开关：持久化偏好 + 尽力应用；应用失败保留用户选择并显示原因',
      (tester) async {
    final invite = FakeInviteService();
    final fake = await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();
    expect(fake.policyRequests.first, SchedulerPolicy(),
        reason: '页面打开时应用持久化的默认偏好（serial × parallel）');

    await tester.tap(find.byKey(const ValueKey('same_code_toggle')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(invite.lastSavedPreference,
        const SchedulerPreference(
            sameCodeMode: SchedCodeMode.parallel,
            crossCodeMode: SchedCodeMode.parallel),
        reason: '偏好按用户选择持久化（默认跨码并行保持不变）');
    expect(fake.policyRequests.last.sameCodeMode, SchedCodeMode.parallel);

    // 应用失败（旧 DLL）：偏好已保存、开关保持用户选择、内联显示原因。
    fake.setSchedulerPolicyError = StateError('旧 DLL 缺少 ak_sched_* 导出');
    await tester.tap(find.byKey(const ValueKey('cross_code_toggle')));
    await tester.pump();
    await tester.pumpAndSettle();
    expect(invite.savePreferenceCalls, 2, reason: '保存先于应用，不受应用失败影响');
    expect(invite.lastSavedPreference!.crossCodeMode, SchedCodeMode.serial);
    expect(
        tester
            .widget<SwitchListTile>(
                find.byKey(const ValueKey('cross_code_toggle')))
            .value,
        isFalse,
        reason: '开关保持用户选择（不回滚）');
    expect(find.byKey(const ValueKey('invite_sched_error')), findsOneWidget);
    expect(find.textContaining('调度策略未应用到运行时'), findsOneWidget);
  });

  testWidgets('直接注入 InviteService（无 TransferService）时仅持久化偏好',
      (tester) async {
    final invite = FakeInviteService();
    await pumpPage(tester, InvitePage(service: invite));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('same_code_toggle')));
    await tester.pump();
    await tester.pumpAndSettle();
    expect(invite.savePreferenceCalls, 1);
    expect(invite.lastSavedPreference!.sameCodeMode, SchedCodeMode.parallel);
    expect(find.byKey(const ValueKey('invite_sched_error')), findsNothing,
        reason: '没有 transfer 时没有可应用的运行时，不报错');
  });

  testWidgets('持久化偏好在下次打开时恢复开关状态', (tester) async {
    final invite = FakeInviteService(
      preference: const SchedulerPreference(
          sameCodeMode: SchedCodeMode.parallel,
          crossCodeMode: SchedCodeMode.serial),
    );
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();

    expect(
        tester
            .widget<SwitchListTile>(
                find.byKey(const ValueKey('same_code_toggle')))
            .value,
        isTrue,
        reason: '恢复同码并行偏好');
    expect(
        tester
            .widget<SwitchListTile>(
                find.byKey(const ValueKey('cross_code_toggle')))
            .value,
        isFalse,
        reason: '恢复跨码串行偏好');
  });

  testWidgets('刷新按钮重新读取列表', (tester) async {
    final invite = FakeInviteService();
    await pumpInvitePage(tester, invite);
    await tester.pumpAndSettle();
    final before = invite.listInvitesCalls;

    await tester.tap(find.byTooltip('刷新'));
    await tester.pump();
    expect(invite.listInvitesCalls, greaterThan(before));
  });
}
