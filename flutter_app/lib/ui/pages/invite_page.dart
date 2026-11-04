import 'dart:async' show StreamSubscription;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        InvitationRevokedEvent,
        InviteCreated,
        InviteState,
        InviteSummary,
        PairingCompleteEvent,
        PairingEvent,
        PeerPreparedEvent,
        RedeemFailedEvent,
        RedeemPendingEvent,
        ServiceOnlineEvent,
        SchedCodeMode,
        SchedulerPolicy,
        normalizeInviteCode;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard, ClipboardData;

import '../../services/invite_service.dart'
    show InviteService, InviteTtlKind, SchedulerPreference;
import '../service/transfer_service.dart';
import '../widgets/empty_state.dart';

/// Invite-code pairing (wave-22): the seventh rail tab "邀请码".
///
/// UI key contract frozen by `integration_test/invite_e2e_test.dart`
/// (INTEGRATION SWITCH there flips to this page — mirror its keys
/// exactly): `invite_page`, `same_code_toggle`, `cross_code_toggle`,
/// `invite_create_btn`, `invite_ttl_long/short`, `invite_create_confirm`,
/// `invite_code_once_text/close`, `invite_code_hint_<id>`,
/// `invite_time_<id>`, `invite_state_<id>`, `invite_revoke_<id>`,
/// `invite_revoke_confirm`, `invite_empty`, `invite_code_input`,
/// `invite_redeem_btn`, `invite_redeem_error`, `invite_events`,
/// `invite_event_<n>`.
///
/// Behavioral freezes (same contract):
/// * the list is `listInvites(includeTerminal: true)` — revoked/expired
///   rows STAY listed as tombstones (pairing-flow.md §8);
/// * the plaintext code is shown exactly ONCE after create; rows carry
///   only the masked hint (`pair list --invites` rule);
/// * a malformed redeem is rejected LOCALLY (§11) — inline error, no
///   registry round-trip, no event;
/// * the scheduler toggles persist via
///   [InviteService.saveSchedulerPreference] (scheduler_prefs.json under
///   the identity dir); applying to the runtime through
///   [TransferService.setSchedulerPolicy] is best-effort this wave (an
///   old DLL shows the reason inline but the preference stays saved).
///
/// Construction: pass either the [InviteService] directly (`service` —
/// the e2e journey shell's one-line switch) and/or the
/// [TransferService] (`transfer` — the app-shell wiring, which resolves
/// the service through the `inviteService` seam and enables the
/// runtime-apply path). Neither → the honest unsupported card
/// (ffi-v2.md §2: an old runtime is never silently downgraded).
class InvitePage extends StatefulWidget {
  const InvitePage({super.key, this.service, this.transfer});

  /// The invite pairing service; when null, resolved from
  /// [transfer.inviteService].
  final InviteService? service;

  /// The transfer service: enables scheduler-policy application via
  /// [TransferService.setSchedulerPolicy] and the inviteService seam.
  final TransferService? transfer;

  @override
  State<InvitePage> createState() => _InvitePageState();
}

class _InvitePageState extends State<InvitePage> {
  final TextEditingController _redeemInput = TextEditingController();

  InviteService? _invite;

  /// Null while the first listInvites() is in flight.
  List<InviteSummary>? _invites;

  /// Inline reason when listInvites() threw (engine down / old DLL).
  String? _listError;

  String? _redeemError;
  String? _createError;
  String? _actionError;
  String? _schedError;

  bool _busy = false;

  /// Masked code hints captured from create results (`codeId → hint`);
  /// rows without one (created in an earlier session) fall back to the
  /// registry-id prefix.
  final Map<String, String> _hints = <String, String>{};

  /// Live pairing events in ARRIVAL order (feed line n = event n).
  final List<PairingEvent> _feed = <PairingEvent>[];
  static const int _maxFeed = 200;
  StreamSubscription<PairingEvent>? _eventsSub;

  SchedulerPreference _preference = SchedulerPreference.defaults;
  bool _preferenceLoaded = false;

  @override
  void initState() {
    super.initState();
    _invite = widget.service ?? widget.transfer?.inviteService;
    final invite = _invite;
    if (invite != null) {
      _eventsSub = invite.events.listen(_onEvent, onError: (Object _) {});
      _bootstrap();
    }
  }

  @override
  void dispose() {
    _eventsSub?.cancel();
    _redeemInput.dispose();
    super.dispose();
  }

  // -------------------------------------------------------------------------
  // Bootstrap / event feed.
  // -------------------------------------------------------------------------

  Future<void> _bootstrap() async {
    await _reloadRows();
    final invite = _invite;
    if (invite == null || !mounted) {
      return;
    }
    final preference = await invite.loadSchedulerPreference();
    if (!mounted) {
      return;
    }
    setState(() {
      _preference = preference;
      _preferenceLoaded = true;
    });
    // Keep the runtime in sync with the persisted preference (best
    // effort: an old DLL surfaces its reason on the next explicit toggle,
    // not on page open).
    final transfer = widget.transfer;
    if (transfer != null) {
      try {
        await transfer.setSchedulerPolicy(_policyOf(_preference));
      } on Object {
        // ignored on purpose (see above)
      }
    }
  }

  void _onEvent(PairingEvent event) {
    if (!mounted) {
      return;
    }
    setState(() {
      _feed.add(event);
      if (_feed.length > _maxFeed) {
        _feed.removeRange(0, _feed.length - _maxFeed);
      }
    });
  }

  Future<void> _reloadRows() async {
    final invite = _invite;
    if (invite == null) {
      return;
    }
    try {
      final rows = await invite.listInvites(includeTerminal: true);
      if (mounted) {
        setState(() {
          _invites = rows;
          _listError = null;
        });
      }
    } on Object catch (error) {
      if (mounted) {
        setState(() {
          _listError = '邀请码列表读取失败：$error';
          _invites = null;
        });
      }
    }
  }

  // -------------------------------------------------------------------------
  // Create (TTL dialog → one-time code dialog).
  // -------------------------------------------------------------------------

  Future<void> _createCode() async {
    if (_busy) {
      return;
    }
    final ttl = await _showTtlDialog();
    if (ttl == null || !mounted) {
      return; // cancelled
    }
    setState(() {
      _busy = true;
      _createError = null;
    });
    try {
      final created = await _invite!.createInvite(ttl: ttl);
      // Bearer credential: keep only the masked hint for the list.
      if (created.invitationCode.isNotEmpty) {
        _hints[created.summary.codeId] = _hintOf(created.invitationCode);
      }
      await _reloadRows();
      if (mounted) {
        await _showOneTimeCode(created);
      }
    } on Object catch (error) {
      if (mounted) {
        setState(() => _createError = '生成失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  Future<InviteTtlKind?> _showTtlDialog() {
    return showDialog<InviteTtlKind>(
      context: context,
      builder: (dialogContext) => const _TtlDialog(),
    );
  }

  Future<void> _showOneTimeCode(InviteCreated created) {
    return showDialog<void>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('邀请码已生成'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('完整邀请码仅展示这一次，请立即保存并分享：'),
            const SizedBox(height: 8),
            SelectableText(
              created.invitationCode.isEmpty
                  ? '（该码此前已生成，明文不再显示）'
                  : created.invitationCode,
              key: const ValueKey('invite_code_once_text'),
              style: const TextStyle(fontWeight: FontWeight.w700),
            ),
          ],
        ),
        actions: [
          TextButton.icon(
            icon: const Icon(Icons.copy),
            label: const Text('复制'),
            onPressed: created.invitationCode.isEmpty
                ? null
                : () => _copyCode(created.invitationCode),
          ),
          TextButton(
            key: const ValueKey('invite_code_once_close'),
            onPressed: () => Navigator.of(dialogContext).pop(),
            child: const Text('我已保存'),
          ),
        ],
      ),
    );
  }

  Future<void> _copyCode(String code) async {
    await Clipboard.setData(ClipboardData(text: code));
    if (mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        const SnackBar(
            content: Text('已复制邀请码'), duration: Duration(seconds: 2)),
      );
    }
  }

  // -------------------------------------------------------------------------
  // Redeem (local Crockford reject, §11).
  // -------------------------------------------------------------------------

  /// Live Crockford validation hint for the input: null while empty, the
  /// specific reason (or 格式正确) otherwise.
  String? get _redeemHint {
    final text = _redeemInput.text.trim();
    if (text.isEmpty) {
      return null;
    }
    try {
      normalizeInviteCode(text);
      return '格式正确';
    } on FormatException catch (error) {
      return error.message;
    }
  }

  Future<void> _redeem() async {
    if (_busy) {
      return;
    }
    setState(() => _redeemError = null);
    final text = _redeemInput.text.trim();
    try {
      // Malformed codes are rejected LOCALLY before anything is sent
      // (pairing-flow.md §11); the NORMALIZED canonical form leaves the
      // page.
      final normalized = normalizeInviteCode(text);
      setState(() => _busy = true);
      await _invite!.redeemInvite(normalized);
      _redeemInput.clear();
      // Accepted/pending: the outcome arrives on the event feed.
    } on FormatException catch (error) {
      if (mounted) {
        setState(() => _redeemError = error.message);
      }
    } on Object catch (error) {
      if (mounted) {
        setState(() => _redeemError = '兑换失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  // -------------------------------------------------------------------------
  // Revoke (confirmation dialog; tombstone stays listed, §8).
  // -------------------------------------------------------------------------

  Future<void> _revokeCode(InviteSummary row) async {
    if (_busy) {
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('撤销邀请码'),
        content: Text(
          '撤销后该码不能再被兑换，已完成的配对不受影响。'
          '（码 ${row.codeId.substring(0, 8)}）',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('取消'),
          ),
          TextButton(
            key: const ValueKey('invite_revoke_confirm'),
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('确认撤销'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) {
      return;
    }
    setState(() => _busy = true);
    try {
      await _invite!.revokeInvite(row.codeId);
      await _reloadRows();
    } on Object catch (error) {
      if (mounted) {
        setState(() => _actionError = '撤销失败：$error');
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  // -------------------------------------------------------------------------
  // Scheduler toggles (persist first; runtime apply best-effort).
  // -------------------------------------------------------------------------

  Future<void> _onToggleSameCode(bool parallel) => _savePreference(
        SchedulerPreference(
          sameCodeMode: parallel ? SchedCodeMode.parallel : SchedCodeMode.serial,
          crossCodeMode: _preference.crossCodeMode,
        ),
      );

  Future<void> _onToggleCrossCode(bool parallel) => _savePreference(
        SchedulerPreference(
          sameCodeMode: _preference.sameCodeMode,
          crossCodeMode:
              parallel ? SchedCodeMode.parallel : SchedCodeMode.serial,
        ),
      );

  Future<void> _savePreference(SchedulerPreference preference) async {
    if (_busy) {
      return;
    }
    setState(() {
      _preference = preference; // optimistic; persistence is the truth below
      _schedError = null;
      _busy = true;
    });
    try {
      // Persist first — the preference is what the user asked for; the
      // scheduler_prefs.json file is the persistence proof (frozen
      // journey contract).
      await _invite!.saveSchedulerPreference(preference);
    } on Object catch (error) {
      if (mounted) {
        setState(() => _schedError = '偏好保存失败：$error');
      }
    }
    // Runtime apply is best-effort this wave: the sched family needs the
    // wave-22 DLL; an old runtime shows its reason inline but the saved
    // preference and the toggle stay as the user set them.
    final transfer = widget.transfer;
    if (transfer != null) {
      try {
        await transfer.setSchedulerPolicy(_policyOf(preference));
      } on Object catch (error) {
        if (mounted) {
          setState(() => _schedError = '调度策略未应用到运行时：$error');
        }
      }
    }
    if (mounted) {
      setState(() => _busy = false);
    }
  }

  // -------------------------------------------------------------------------
  // Rendering helpers.
  // -------------------------------------------------------------------------

  static SchedulerPolicy _policyOf(SchedulerPreference preference) =>
      SchedulerPolicy(
          sameCodeMode: preference.sameCodeMode,
          crossCodeMode: preference.crossCodeMode);

  /// Masked hint of a full code: first two groups + ellipsis.
  static String _hintOf(String codeText) => '${codeText.substring(0, 14)}…';

  String _two(int v) => v.toString().padLeft(2, '0');

  String _clockOf(int? atMs) {
    final local = atMs == null
        ? DateTime.now()
        : DateTime.fromMillisecondsSinceEpoch(atMs);
    return '${_two(local.hour)}:${_two(local.minute)}:${_two(local.second)}';
  }

  String _feedLine(PairingEvent event) {
    final clock = _clockOf(event.atMs);
    if (event is ServiceOnlineEvent) return '$clock · 配对服务已连接';
    if (event is PeerPreparedEvent) return '$clock · 配对事务已持久化';
    if (event is PairingCompleteEvent) {
      final peer = event.peer;
      final short = peer.length >= 8 ? peer.substring(0, 8) : peer;
      return '$clock · 配对完成 · 对端指纹 $short';
    }
    if (event is InvitationRevokedEvent) return '$clock · 邀请码已撤销';
    if (event is RedeemFailedEvent) return '$clock · 兑换失败 · ${event.error}';
    if (event is RedeemPendingEvent) {
      return '$clock · 收到兑换请求 · 码 ${event.codeId.substring(0, 8)}';
    }
    return '$clock · ${event.name}';
  }

  String _rowTimeLine(InviteSummary row) {
    final ttlText = row.expiresMs == 0
        ? '长期有效'
        : row.expiresMs - row.createdMs >= 3600 * 1000
            ? '${(row.expiresMs - row.createdMs) ~/ (3600 * 1000)} 小时有效'
            : '${(row.expiresMs - row.createdMs) ~/ (60 * 1000)} 分钟有效';
    return '创建于 ${_clockOf(row.createdMs)} · $ttlText';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final invite = _invite;
    if (invite == null) {
      return SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: const [
            Text('配对邀请码'),
            SizedBox(height: 16),
            EmptyState(
              icon: Icons.link_off,
              title: '邀请码配对不可用',
              message: '当前运行时不支持邀请码配对（需要带有 ak_pairing_* / '
                  'ak_sched_* 导出的 ak-ffi 运行时）。请更新 ak_ffi.dll 后重试；'
                  '旧运行时不会被静默降级使用。',
            ),
          ],
        ),
      );
    }

    final rows = _invites;
    final listError = _listError;

    // SingleChildScrollView + Column (NOT ListView): every child stays
    // BUILT regardless of scroll offset (the e2e journey taps by key
    // below the fold).
    return SingleChildScrollView(
      key: const ValueKey('invite_page'),
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('配对邀请码', style: theme.textTheme.headlineSmall),
          const SizedBox(height: 4),
          Text(
            '码本身即授权：持有者可与本设备配对（owner 在线自动处理，撤销需另行操作）',
            style: theme.textTheme.bodySmall,
          ),
          const SizedBox(height: 16),

          // -- 调度开关（same_code / cross_code） ---------------------------
          Text('任务调度', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          Text(
            '两维并发偏好（关闭 = 串行）；两台设备取更严格的一端生效，偏好保存在本机。',
            style: theme.textTheme.bodySmall,
          ),
          SwitchListTile(
            key: const ValueKey('same_code_toggle'),
            title: const Text('同码并行（same_code）'),
            subtitle: const Text('关闭 = 同一邀请码同时只跑一个任务'),
            value: _preference.sameCodeMode == SchedCodeMode.parallel,
            onChanged: _busy ? null : _onToggleSameCode,
          ),
          SwitchListTile(
            key: const ValueKey('cross_code_toggle'),
            title: const Text('跨码并行（cross_code）'),
            subtitle: const Text('关闭 = 不同邀请码之间也串行'),
            value: _preference.crossCodeMode == SchedCodeMode.parallel,
            onChanged: _busy ? null : _onToggleCrossCode,
          ),
          if (!_preferenceLoaded)
            const Padding(
              padding: EdgeInsets.only(left: 4),
              child: Text('正在读取本机偏好…'),
            ),
          if (_schedError != null)
            Padding(
              key: const ValueKey('invite_sched_error'),
              padding: const EdgeInsets.only(left: 4, top: 4),
              child: Text(
                _schedError!,
                style: TextStyle(color: scheme.error),
              ),
            ),
          const SizedBox(height: 8),

          // -- 生成 ----------------------------------------------------------
          FilledButton.icon(
            key: const ValueKey('invite_create_btn'),
            icon: const Icon(Icons.add_link),
            label: const Text('生成邀请码'),
            onPressed: _busy ? null : _createCode,
          ),
          if (_createError != null)
            Padding(
              key: const ValueKey('invite_create_error'),
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _createError!,
                style: TextStyle(color: scheme.error),
              ),
            ),
          const SizedBox(height: 16),

          // -- 本机邀请码列表 -------------------------------------------------
          Row(
            children: [
              Expanded(
                child: Text('我的邀请码', style: theme.textTheme.titleMedium),
              ),
              IconButton(
                tooltip: '刷新',
                onPressed: _reloadRows,
                icon: const Icon(Icons.refresh),
              ),
            ],
          ),
          const SizedBox(height: 4),
          if (_actionError != null)
            Padding(
              key: const ValueKey('invite_action_error'),
              padding: const EdgeInsets.only(bottom: 4),
              child: Text(
                _actionError!,
                style: TextStyle(color: scheme.error),
              ),
            ),
          if (listError != null)
            Padding(
              key: const ValueKey('invite_list_error'),
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Text(
                listError,
                style: TextStyle(color: scheme.error),
              ),
            )
          else if (rows == null)
            const Padding(
              key: ValueKey('invite_loading'),
              padding: EdgeInsets.symmetric(vertical: 16),
              child: Center(child: CircularProgressIndicator()),
            )
          else
            SizedBox(
              key: const ValueKey('invite_list'),
              width: double.infinity,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (rows.isEmpty)
                    const Padding(
                      padding: EdgeInsets.symmetric(vertical: 16),
                      child: Text('暂无邀请码', key: ValueKey('invite_empty')),
                    )
                  else
                    ...rows.map(_codeTile),
                ],
              ),
            ),
          const SizedBox(height: 16),

          // -- 兑换 ----------------------------------------------------------
          Text('兑换邀请码', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          Row(
            children: [
              Expanded(
                child: TextField(
                  key: const ValueKey('invite_code_input'),
                  controller: _redeemInput,
                  enabled: !_busy,
                  onChanged: (text) {
                    setState(() {
                      // A now-valid edit clears the stale inline error
                      // (the live helperText already shows the state).
                      if (_redeemError != null) {
                        try {
                          normalizeInviteCode(text.trim());
                          _redeemError = null;
                        } on FormatException {
                          // still invalid: keep the error visible
                        }
                      }
                    });
                  },
                  decoration: InputDecoration(
                    hintText: 'AKI1-XXXX-XXXX-…（收到对方分享的邀请码）',
                    helperText: _redeemHint,
                    border: const OutlineInputBorder(),
                  ),
                ),
              ),
              const SizedBox(width: 12),
              FilledButton(
                key: const ValueKey('invite_redeem_btn'),
                onPressed: _busy ? null : _redeem,
                child: const Text('兑换'),
              ),
            ],
          ),
          if (_redeemError != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _redeemError!,
                key: const ValueKey('invite_redeem_error'),
                style: TextStyle(color: scheme.error),
              ),
            ),
          const SizedBox(height: 16),

          // -- 配对事件实时列表 ------------------------------------------------
          Text('配对事件', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          Container(
            key: const ValueKey('invite_events'),
            width: double.infinity,
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
            ),
            child: _feed.isEmpty
                ? const Text(
                    '暂无事件；生成或兑换邀请码后，这里会实时显示配对进展。')
                : Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (var i = 0; i < _feed.length; i++)
                        Text(
                          _feedLine(_feed[i]),
                          key: ValueKey('invite_event_$i'),
                        ),
                    ],
                  ),
          ),
        ],
      ),
    );
  }

  Widget _codeTile(InviteSummary row) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final stateText = switch (row.state) {
      InviteState.active => '有效',
      InviteState.revoked => '已撤销',
      InviteState.expired => '已过期',
    };
    final hint =
        _hints[row.codeId] ?? '${row.codeId.substring(0, 8)}…';
    return Card(
      child: ListTile(
        title: Text(
          hint,
          key: ValueKey('invite_code_hint_${row.codeId}'),
        ),
        subtitle: Text(
          _rowTimeLine(row),
          key: ValueKey('invite_time_${row.codeId}'),
        ),
        trailing: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              stateText,
              key: ValueKey('invite_state_${row.codeId}'),
              style: TextStyle(
                color: row.state == InviteState.active
                    ? scheme.primary
                    : scheme.outline,
              ),
            ),
            IconButton(
              key: ValueKey('invite_revoke_${row.codeId}'),
              icon: const Icon(Icons.link_off),
              tooltip: '撤销',
              onPressed: row.state == InviteState.active
                  ? () => _revokeCode(row)
                  : null,
            ),
          ],
        ),
      ),
    );
  }
}

/// The TTL choice dialog of the create flow (the frozen two-kind enum).
class _TtlDialog extends StatefulWidget {
  const _TtlDialog();

  @override
  State<_TtlDialog> createState() => _TtlDialogState();
}

class _TtlDialogState extends State<_TtlDialog> {
  InviteTtlKind _selected = InviteTtlKind.longLived;

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('生成邀请码'),
      content: RadioGroup<InviteTtlKind>(
        groupValue: _selected,
        onChanged: (value) {
          if (value != null) {
            setState(() => _selected = value);
          }
        },
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            RadioListTile<InviteTtlKind>(
              key: const ValueKey('invite_ttl_long'),
              title: const Text('长期有效'),
              value: InviteTtlKind.longLived,
            ),
            RadioListTile<InviteTtlKind>(
              key: const ValueKey('invite_ttl_short'),
              title: const Text('短期 1 小时'),
              value: InviteTtlKind.shortLived,
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        TextButton(
          key: const ValueKey('invite_create_confirm'),
          onPressed: () => Navigator.of(context).pop(_selected),
          child: const Text('确认'),
        ),
      ],
    );
  }
}
