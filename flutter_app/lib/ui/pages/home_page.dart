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
  List<TaskConnection> _connections = const [];

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
  }

  @override
  void dispose() {
    _flow.removeListener(_onFlowChanged);
    _flow.dispose();
    _connectionsSub?.cancel();
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
            onSubmit: (code) => _flow.startJoin(code),
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
/// hyphen-tolerant, live normalized-length counter.
class _JoinInputPanel extends StatefulWidget {
  const _JoinInputPanel({required this.onSubmit, required this.onCancel});

  final ValueChanged<String> onSubmit;
  final VoidCallback onCancel;

  @override
  State<_JoinInputPanel> createState() => _JoinInputPanelState();
}

class _JoinInputPanelState extends State<_JoinInputPanel> {
  final TextEditingController _controller = TextEditingController();

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
    final normalizedLength = normalizeJoinCode(_controller.text).length;
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
                FilteringTextInputFormatter.allow(RegExp('[0-9A-Za-z-]')),
                _upperCase,
              ],
              textCapitalization: TextCapitalization.characters,
              onChanged: (_) => setState(() {}),
              onSubmitted: normalizedLength == 8
                  ? (_) => widget.onSubmit(_controller.text)
                  : null,
              decoration: const InputDecoration(
                border: OutlineInputBorder(),
                hintText: '例如 K3P7-QM2X',
                helperText: '大小写、连字符均可，自动识别',
                counterText: '',
              ),
              maxLength: 9,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 22,
                letterSpacing: 2,
              ),
            ),
            const SizedBox(height: 4),
            Text('$normalizedLength/8 位',
                key: const ValueKey('join_code_counter'),
                style: theme.textTheme.bodySmall),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const ValueKey('join_submit_button'),
              onPressed: normalizedLength == 8
                  ? () => widget.onSubmit(_controller.text)
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
          children: [
            // FittedBox keeps the 8-char code on screen at 390dp and
            // under large text scale without ellipsizing.
            Expanded(
              child: FittedBox(
                fit: BoxFit.scaleDown,
                child: SelectableText(
                  code,
                  key: const ValueKey('flow_pairing_code'),
                  style: theme.textTheme.displayMedium?.copyWith(
                    fontWeight: FontWeight.w700,
                    fontFamily: 'monospace',
                    letterSpacing: 4,
                  ),
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
            onPressed: controller.reset,
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
