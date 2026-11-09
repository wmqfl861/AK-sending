/// Wave-28 home (「连接」 tab): the two pairing actions (生成配对码 /
/// 输入配对码) plus the live 「当前连接」 list. No IP/port/fingerprint
/// forms anywhere — the whole page runs on one-time pairing codes.
library;

import 'dart:async' show StreamSubscription;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart'
    show
        Clipboard,
        ClipboardData,
        FilteringTextInputFormatter,
        TextInputFormatter;

import '../service/file_pick_gateway.dart';
import '../service/pairing_flow_controller.dart';
import '../service/simple_transfer_service.dart';
import '../widgets/engine_error_card.dart';
import '../widgets/task_connection_views.dart';
import 'connection_detail_page.dart';

/// Home page: pairing flows + current connections.
class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    required this.service,
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;

  /// File/folder picker seam, passed down to connection detail pages so
  /// tests inject a fake (the shell injects its own).
  final FilePickGateway picker;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late final PairingFlowController _flow;
  StreamSubscription<List<TaskConnection>>? _connectionsSub;
  StreamSubscription<EngineHealth>? _engineHealthSub;
  List<TaskConnection> _connections = const [];

  /// The engine health as the health stream last reported it (R36-P2-03)
  /// — a latched failure renders the recoverable engine-failed card.
  EngineHealth _engineHealth = const EngineHealth.ok();

  /// True while the join-code input panel is open (the 输入配对码 action).
  bool _joinInputOpen = false;

  @override
  void initState() {
    super.initState();
    _flow = PairingFlowController(widget.service)
      ..addListener(_onFlowChanged);
    _connectionsSub = widget.service.connections.listen(
      (snapshot) {
        if (mounted) {
          setState(() => _connections = snapshot);
        }
      },
      onError: (Object _) {},
    );
    // R36-P2-03: an engine failure must be VISIBLE on the page the user
    // is looking at — the replayable health stream delivers the latch
    // (and replays it to a page rebuilt mid-failure) instead of leaving
    // stale waiting/connecting UI on screen.
    _engineHealthSub = widget.service.engineHealth.listen(
      (health) {
        if (mounted) {
          setState(() => _engineHealth = health);
        }
      },
      onError: (Object _) {},
    );
  }

  @override
  void dispose() {
    _flow.removeListener(_onFlowChanged);
    _flow.dispose();
    _connectionsSub?.cancel();
    _engineHealthSub?.cancel();
    super.dispose();
  }

  void _onFlowChanged() {
    if (mounted) setState(() {});
  }

  Future<void> _chooseRoleAndCreate() async {
    final role = await showModalBottomSheet<TaskRole>(
      context: context,
      builder: (context) => const _RoleSheet(),
    );
    if (role != null && mounted) {
      await _flow.startCreate(role);
    }
  }

  void _openConnection(TaskConnection connection) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ConnectionDetailPage(
          service: widget.service,
          connectionId: connection.id,
          picker: widget.picker,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      key: const ValueKey('home_page'),
      padding: const EdgeInsets.all(16),
      children: [
        if (_engineHealth.isEngineFailed)
          // R36-P2-03: the terminal engine-failure latch made VISIBLE —
          // the shared engine-error style, with the honest recoverable
          // affordance (restart the app; the engine is presumed broken
          // native-side, retrying actions on this service cannot heal it).
          EngineErrorCard(
            key: const ValueKey('engine_failed_card'),
            title: '传输引擎故障，请重启应用',
            detail: _engineHealth.failureReason ?? '连续原生轮询失败。',
            detailKey: const ValueKey('engine_failed_card_detail'),
          ),
        Text('连接', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 4),
        Text('无需设置任何信息——一个配对码即可互传文件',
            style: theme.textTheme.bodySmall),
        const SizedBox(height: 16),
        if (_flow.active)
          _PairingFlowView(
            controller: _flow,
            onJoinRetry: () => setState(() => _joinInputOpen = true),
          )
        else if (_joinInputOpen)
          _JoinInputPanel(
            onSubmit: (code, role) => _flow.startJoin(code, role: role),
            onCancel: () => setState(() => _joinInputOpen = false),
          )
        else
          _HomeActions(
            onCreateCode: _chooseRoleAndCreate,
            onJoinCode: () => setState(() => _joinInputOpen = true),
          ),
        const SizedBox(height: 24),
        _ConnectionsSection(
          connections: _connections,
          onOpen: _openConnection,
        ),
      ],
    );
  }
}

/// The empty state's two big actions.
class _HomeActions extends StatelessWidget {
  const _HomeActions({required this.onCreateCode, required this.onJoinCode});

  final VoidCallback onCreateCode;
  final VoidCallback onJoinCode;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        FilledButton.icon(
          key: const ValueKey('home_create_code_button'),
          style: FilledButton.styleFrom(
            padding: const EdgeInsets.symmetric(vertical: 20),
          ),
          onPressed: onCreateCode,
          icon: const Icon(Icons.qr_code_2, size: 28),
          label: const Text('生成配对码', style: TextStyle(fontSize: 18)),
        ),
        const SizedBox(height: 12),
        FilledButton.tonalIcon(
          key: const ValueKey('home_join_code_button'),
          style: FilledButton.styleFrom(
            padding: const EdgeInsets.symmetric(vertical: 20),
          ),
          onPressed: onJoinCode,
          icon: const Icon(Icons.keyboard, size: 28),
          label: const Text('输入配对码', style: TextStyle(fontSize: 18)),
        ),
        const SizedBox(height: 8),
        Text(
          '「生成配对码」把码告诉对方；「输入配对码」输入对方给的码。',
          textAlign: TextAlign.center,
          style: theme.textTheme.bodySmall,
        ),
      ],
    );
  }
}

/// Bottom sheet: pick the direction the new pairing declares.
class _RoleSheet extends StatelessWidget {
  const _RoleSheet();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('这次配对用来做什么？', style: theme.textTheme.titleMedium),
            const SizedBox(height: 4),
            Text('方向在配对后固定；反向传输需要重新配对。',
                style: theme.textTheme.bodySmall),
            const SizedBox(height: 16),
            FilledButton.icon(
              key: const ValueKey('role_send_button'),
              style: FilledButton.styleFrom(
                  padding: const EdgeInsets.symmetric(vertical: 16)),
              onPressed: () => Navigator.of(context).pop(TaskRole.send),
              icon: const Icon(Icons.upload),
              label: const Text('我要发送（把文件发给对方）'),
            ),
            const SizedBox(height: 8),
            FilledButton.tonalIcon(
              key: const ValueKey('role_receive_button'),
              style: FilledButton.styleFrom(
                  padding: const EdgeInsets.symmetric(vertical: 16)),
              onPressed: () => Navigator.of(context).pop(TaskRole.receive),
              icon: const Icon(Icons.download),
              label: const Text('我要接收（接收对方发来的文件）'),
            ),
          ],
        ),
      ),
    );
  }
}

/// The join-code input form (加入流 first step): auto-uppercase,
/// hyphen/space-tolerant paste of the FULL `AKI1-…` code (wave-29 P1-02),
/// live normalized-length counter, and the joiner's direction pick — the
/// user's explicit intent forwarded with the join (P1-05).
class _JoinInputPanel extends StatefulWidget {
  const _JoinInputPanel({
    required this.onSubmit,
    required this.onCancel,
  });

  final void Function(String code, TaskRole role) onSubmit;
  final VoidCallback onCancel;

  @override
  State<_JoinInputPanel> createState() => _JoinInputPanelState();
}

class _JoinInputPanelState extends State<_JoinInputPanel> {
  final TextEditingController _controller = TextEditingController();
  TaskRole? _role;

  static final TextInputFormatter _upperCase = TextInputFormatter.withFunction(
    (oldValue, newValue) => newValue.copyWith(text: newValue.text.toUpperCase()),
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final normalized = normalizeJoinCode(_controller.text);
    final codeValid = isValidTaskCode(normalized);
    final role = _role;
    final ready = codeValid && role != null;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('输入对方的配对码', style: theme.textTheme.titleMedium),
            const SizedBox(height: 12),
            TextField(
              key: const ValueKey('join_code_field'),
              controller: _controller,
              autofocus: true,
              inputFormatters: [
                FilteringTextInputFormatter.allow(RegExp('[0-9A-Za-z- ]')),
                _upperCase,
              ],
              textCapitalization: TextCapitalization.characters,
              onChanged: (_) => setState(() {}),
              onSubmitted: ready
                  ? (_) => widget.onSubmit(_controller.text, role)
                  : null,
              decoration: const InputDecoration(
                border: OutlineInputBorder(),
                hintText: 'AKI1-XXXX-XXXX-XXXX-…（完整复制粘贴即可）',
                helperText: '大小写、连字符、空格均可，自动识别',
                counterText: '',
              ),
              // The grouped display form is 44 characters; the slack also
              // tolerates pasted leading/trailing spaces.
              maxLength: 64,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 16,
                letterSpacing: 1,
              ),
            ),
            const SizedBox(height: 4),
            Text(
              '${normalized.length}/36 位'
              '${normalized.isNotEmpty && !codeValid ? '（需 AKI1 开头的 36 位字符）' : ''}',
              key: const ValueKey('join_code_counter'),
              style: theme.textTheme.bodySmall,
            ),
            const SizedBox(height: 12),
            Text('这次配对我要：', style: theme.textTheme.bodyMedium),
            const SizedBox(height: 4),
            SegmentedButton<TaskRole>(
              key: const ValueKey('join_role_selector'),
              segments: const [
                ButtonSegment(
                  value: TaskRole.send,
                  icon: Icon(Icons.upload),
                  label: Text('发送文件'),
                ),
                ButtonSegment(
                  value: TaskRole.receive,
                  icon: Icon(Icons.download),
                  label: Text('接收文件'),
                ),
              ],
              selected: role == null ? const <TaskRole>{} : <TaskRole>{role},
              // `emptySelectionAllowed` is required for the INITIAL
              // nothing-picked state (SegmentedButton asserts non-empty
              // otherwise). R29-P2-04: an empty selection still can never
              // reach the join — the callback guards the empty set (a
              // null role keeps the submit DISABLED), so a re-tap that
              // deselects is honest, not a StateError crash.
              emptySelectionAllowed: true,
              onSelectionChanged: (selection) => setState(
                  () => _role = selection.isEmpty ? null : selection.first),
            ),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const ValueKey('join_submit_button'),
              onPressed: ready
                  ? () => widget.onSubmit(_controller.text, role)
                  : null,
              icon: const Icon(Icons.link),
              label: const Text('配对'),
            ),
            TextButton(
              key: const ValueKey('join_cancel_button'),
              onPressed: widget.onCancel,
              child: const Text('取消'),
            ),
          ],
        ),
      ),
    );
  }
}

/// Renders the active flow phase (code display / waiting / probe /
/// success / failure) driven by [PairingFlowController].
class _PairingFlowView extends StatelessWidget {
  const _PairingFlowView({required this.controller, required this.onJoinRetry});

  final PairingFlowController controller;
  final VoidCallback onJoinRetry;

  @override
  Widget build(BuildContext context) {
    final phase = controller.phase;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: AnimatedSwitcher(
          duration: const Duration(milliseconds: 200),
          child: switch (phase) {
            PairingFlowPhase.creating => const _FlowProgress(
                key: ValueKey('flow_creating'), label: '正在生成配对码…'),
            PairingFlowPhase.awaitingPeer => _CodeShownView(controller: controller),
            PairingFlowPhase.joining => const _FlowProgress(
                key: ValueKey('flow_joining'), label: '正在配对…'),
            PairingFlowPhase.paired => const _FlowProgress(
                key: ValueKey('flow_paired'), label: '配对成功，正在建立连接…'),
            PairingFlowPhase.probing => const _FlowProgress(
                key: ValueKey('flow_probing'), label: '正在测试连通性…'),
            PairingFlowPhase.success => _FlowSuccessView(controller: controller),
            PairingFlowPhase.failed || null => _FlowFailedView(
                controller: controller, onJoinRetry: onJoinRetry),
          },
        ),
      ),
    );
  }
}

/// Spinner + label row for the indeterminate flow phases.
class _FlowProgress extends StatelessWidget {
  const _FlowProgress({super.key, required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        const SizedBox(
          width: 24,
          height: 24,
          child: CircularProgressIndicator(strokeWidth: 2.5),
        ),
        const SizedBox(width: 16),
        Expanded(child: Text(label, style: Theme.of(context).textTheme.bodyLarge)),
      ],
    );
  }
}

/// 大号配对码（等宽）+ 复制 + 等待动画.
class _CodeShownView extends StatelessWidget {
  const _CodeShownView({required this.controller});

  final PairingFlowController controller;

  Future<void> _cancel(PairingFlowController flow) async {
    // Wave-30 P2-04: cancelling the wait also REVOKES the unconsumed code
    // engine-side (disconnect(code_id)) — the code dies with the flow
    // instead of lingering redeemable until its TTL. A failed revoke
    // surfaces in the flow's failed state (honest, never silent).
    await flow.cancelWaiting();
  }

  Future<void> _copy(BuildContext context, String code) async {
    await Clipboard.setData(ClipboardData(text: code));
    if (context.mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(content: Text('已复制配对码 $code'),
            duration: const Duration(seconds: 2)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final code = controller.code?.code ?? '';
    final role = controller.code?.role;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text('我的配对码', style: theme.textTheme.titleMedium),
            const SizedBox(width: 8),
            if (role != null)
              Chip(
                label: Text(taskRoleShortLabel(role)),
                visualDensity: VisualDensity.compact,
              ),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            // The full AKI1 display form is 44 characters: wrap over (at
            // most) two monospace lines instead of shrinking to dust at
            // 390dp.
            Expanded(
              child: SelectableText(
                code,
                key: const ValueKey('flow_pairing_code'),
                maxLines: 2,
                style: theme.textTheme.headlineMedium?.copyWith(
                  fontWeight: FontWeight.w700,
                  fontFamily: 'monospace',
                  letterSpacing: 1,
                  height: 1.25,
                ),
              ),
            ),
            const SizedBox(width: 8),
            IconButton.outlined(
              key: const ValueKey('flow_copy_code'),
              icon: const Icon(Icons.copy),
              tooltip: '复制配对码',
              onPressed: () => _copy(context, code),
            ),
          ],
        ),
        const SizedBox(height: 16),
        Row(
          key: const ValueKey('flow_waiting'),
          children: [
            const SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Text(
                '等待对方输入配对码…配对成功后自动测试连通。',
                style: theme.textTheme.bodyMedium,
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const ValueKey('flow_cancel_button'),
            onPressed: () => _cancel(controller),
            child: const Text('取消配对'),
          ),
        ),
      ],
    );
  }
}

/// 连接成功（连通测试通过）.
class _FlowSuccessView extends StatelessWidget {
  const _FlowSuccessView({required this.controller});

  final PairingFlowController controller;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final connection = controller.connection;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Icon(Icons.check_circle, color: Colors.green.shade600, size: 40),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('连接成功',
                      key: const ValueKey('flow_success'),
                      style: theme.textTheme.titleLarge
                          ?.copyWith(color: Colors.green.shade700)),
                  if (connection != null)
                    Text(
                      '连通测试通过：${connection.peerName}'
                      '（${taskRoleShortLabel(connection.role)}）',
                      style: theme.textTheme.bodySmall,
                    ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 16),
        FilledButton.icon(
          key: const ValueKey('flow_done_button'),
          onPressed: controller.reset,
          icon: const Icon(Icons.check),
          label: const Text('完成'),
        ),
      ],
    );
  }
}

/// 失败态：可读原因 + 重试/重新输入.
class _FlowFailedView extends StatelessWidget {
  const _FlowFailedView({required this.controller, required this.onJoinRetry});

  final PairingFlowController controller;
  final VoidCallback onJoinRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final isJoin = controller.kind == PairingFlowKind.join;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Icon(Icons.error_outline, color: theme.colorScheme.error, size: 32),
            const SizedBox(width: 12),
            Expanded(
              child: Text(
                controller.error ?? '配对失败，请重试。',
                key: const ValueKey('flow_error'),
                style: theme.textTheme.bodyLarge?.copyWith(
                  color: theme.colorScheme.error,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 16),
        FilledButton.icon(
          key: const ValueKey('flow_retry_button'),
          onPressed: isJoin
              ? () {
                  controller.reset();
                  onJoinRetry();
                }
              : () => controller.startCreate(controller.code?.role ??
                  controller.connection?.role ??
                  TaskRole.send),
          icon: const Icon(Icons.refresh),
          label: Text(isJoin ? '重新输入配对码' : '重试'),
        ),
        TextButton(
          key: const ValueKey('flow_back_button'),
          onPressed: controller.reset,
          child: const Text('返回'),
        ),
      ],
    );
  }
}

/// 「当前连接」 list section.
class _ConnectionsSection extends StatelessWidget {
  const _ConnectionsSection({
    required this.connections,
    required this.onOpen,
  });

  final List<TaskConnection> connections;
  final ValueChanged<TaskConnection> onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text('当前连接',
                key: const ValueKey('connections_header'),
                style: theme.textTheme.titleMedium),
            const SizedBox(width: 8),
            Text('（${connections.length}）', style: theme.textTheme.bodySmall),
          ],
        ),
        const SizedBox(height: 4),
        if (connections.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Text(
              '暂无连接。生成或输入配对码后，连接会显示在这里。',
              key: const ValueKey('connections_empty'),
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.outline),
            ),
          )
        else
          ...connections.map(
            (connection) => ConnectionCard(
              connection: connection,
              onOpen: () => onOpen(connection),
            ),
          ),
      ],
    );
  }
}
