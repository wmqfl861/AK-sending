/// Wave-28 connection detail: live status, the send action (sender-side
/// connections only — one-way pairing), the file/folder picker with a
/// review list, and 断开 with a confirm dialog.
library;

import 'dart:async' show StreamSubscription;

import 'package:flutter/material.dart';

import '../service/file_pick_gateway.dart';
import '../service/simple_transfer_service.dart';
import '../widgets/task_connection_views.dart';

enum _SendStage { idle, sending, done, failed }

class ConnectionDetailPage extends StatefulWidget {
  const ConnectionDetailPage({
    super.key,
    required this.service,
    required this.connectionId,
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;
  final String connectionId;
  final FilePickGateway picker;

  @override
  State<ConnectionDetailPage> createState() => _ConnectionDetailPageState();
}

class _ConnectionDetailPageState extends State<ConnectionDetailPage> {
  StreamSubscription<List<TaskConnection>>? _sub;
  TaskConnection? _connection;
  final List<PickedEntry> _picked = [];
  _SendStage _sendStage = _SendStage.idle;
  String? _sendSummary;

  @override
  void initState() {
    super.initState();
    _sub = widget.service.connections.listen(
      (snapshot) {
        if (!mounted) return;
        setState(() {
          _connection = snapshot
              .where((c) => c.id == widget.connectionId)
              .firstOrNullSafe;
        });
      },
      onError: (Object _) {},
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  Future<void> _pickSource() async {
    final mode = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text('选择要发送的内容',
                  style: Theme.of(context).textTheme.titleMedium),
              const SizedBox(height: 16),
              FilledButton.tonalIcon(
                key: const ValueKey('pick_files_option'),
                onPressed: () => Navigator.of(context).pop('files'),
                icon: const Icon(Icons.description),
                label: const Text('选择文件（可多选）'),
              ),
              const SizedBox(height: 8),
              FilledButton.tonalIcon(
                key: const ValueKey('pick_dir_option'),
                onPressed: () => Navigator.of(context).pop('dir'),
                icon: const Icon(Icons.folder),
                label: const Text('选择文件夹'),
              ),
            ],
          ),
        ),
      ),
    );
    if (mode == null || !mounted) {
      return;
    }
    List<PickedEntry> entries;
    if (mode == 'dir') {
      final dir = await widget.picker.pickDirectory();
      entries = dir == null ? const <PickedEntry>[] : <PickedEntry>[dir];
    } else {
      entries = await widget.picker.pickFiles();
    }
    if (!mounted) return;
    setState(() {
      _picked
        ..clear()
        ..addAll(entries);
      _sendStage = _SendStage.idle;
      _sendSummary = null;
    });
  }

  Future<void> _send() async {
    if (_picked.isEmpty) return;
    setState(() {
      _sendStage = _SendStage.sending;
      _sendSummary = null;
    });
    try {
      final summary = await widget.service
          .sendFiles(widget.connectionId, _picked.map((e) => e.path).toList());
      if (!mounted) return;
      setState(() {
        _sendStage = _SendStage.done;
        _sendSummary = summary;
      });
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        _sendStage = _SendStage.failed;
        _sendSummary = '$e';
      });
    }
  }

  Future<void> _confirmDisconnect() async {
    final connection = _connection;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('断开连接？'),
        content: Text(
          '确定断开与「${connection?.peerName ?? '对端'}」的连接吗？'
          '断开后此配对码立即失效，需要重新配对。',
        ),
        actions: [
          TextButton(
            key: const ValueKey('conn_disconnect_cancel'),
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const ValueKey('conn_disconnect_confirm'),
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(context).colorScheme.error,
            ),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('断开'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    await widget.service.disconnect(widget.connectionId);
    if (mounted) {
      Navigator.of(context).maybePop();
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final connection = _connection;
    final live = connection != null &&
        connection.state != TaskConnectionState.disconnected &&
        connection.state != TaskConnectionState.unreachable;
    final peerName = connection?.peerName ?? '对端';

    return Scaffold(
      appBar: AppBar(title: Text(peerName)),
      body: ListView(
        key: const ValueKey('conn_detail_page'),
        padding: const EdgeInsets.all(16),
        children: [
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          peerName,
                          key: const ValueKey('conn_detail_peer'),
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.titleLarge,
                        ),
                      ),
                      if (connection != null)
                        ConnectionStateChip(state: connection.state)
                      else
                        const ConnectionStateChip(
                            state: TaskConnectionState.disconnected),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Text(
                    connection == null
                        ? '此连接已断开（对端离开或网络变化）。'
                        : '方向：${taskRoleLabel(connection.role)}',
                    key: const ValueKey('conn_detail_role'),
                    style: theme.textTheme.bodySmall,
                  ),
                  if (connection != null &&
                      connection.state != TaskConnectionState.connected)
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: Text(
                        switch (connection.state) {
                          TaskConnectionState.testing => '正在测试连通性…',
                          TaskConnectionState.unreachable =>
                            '连通测试未通过：无法与对端建立数据直连，请重新配对。',
                          _ => '正在建立连接…',
                        },
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: connection.state ==
                                  TaskConnectionState.unreachable
                              ? theme.colorScheme.error
                              : theme.colorScheme.tertiary,
                        ),
                      ),
                    ),
                  if (connection?.state == TaskConnectionState.unreachable)
                    const Padding(
                      padding: EdgeInsets.only(top: 12),
                      child: DirectUnavailableGuidance(),
                    ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 8),
          if (connection?.role == TaskRole.send)
            _SendCard(
              live: live,
              picked: _picked,
              stage: _sendStage,
              summary: _sendSummary,
              onPick: _pickSource,
              onSend: _send,
              onClear: () => setState(() {
                _picked.clear();
                _sendStage = _SendStage.idle;
                _sendSummary = null;
              }),
            )
          else
            _ReceiveCard(saveDir: widget.service.saveDir),
          const SizedBox(height: 24),
          OutlinedButton.icon(
            key: const ValueKey('conn_disconnect_button'),
            style: OutlinedButton.styleFrom(
              foregroundColor: theme.colorScheme.error,
              padding: const EdgeInsets.symmetric(vertical: 14),
            ),
            onPressed: _confirmDisconnect,
            icon: const Icon(Icons.link_off),
            label: const Text('断开连接'),
          ),
        ],
      ),
    );
  }
}

/// Sender-side card: pick → review → send → progress → result.
class _SendCard extends StatelessWidget {
  const _SendCard({
    required this.live,
    required this.picked,
    required this.stage,
    required this.summary,
    required this.onPick,
    required this.onSend,
    required this.onClear,
  });

  final bool live;
  final List<PickedEntry> picked;
  final _SendStage stage;
  final String? summary;
  final VoidCallback onPick;
  final VoidCallback onSend;
  final VoidCallback onClear;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('发送文件', style: theme.textTheme.titleMedium),
            const SizedBox(height: 4),
            Text('可多选同目录文件，或选择整个文件夹。',
                style: theme.textTheme.bodySmall),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const ValueKey('conn_pick_files_button'),
              onPressed: live ? onPick : null,
              icon: const Icon(Icons.library_add_check),
              label: const Text('选择文件发送'),
            ),
            if (picked.isNotEmpty) ...[
              const SizedBox(height: 12),
              Row(
                children: [
                  Expanded(
                    child: Text('已选 ${picked.length} 项',
                        key: const ValueKey('picked_count'),
                        style: theme.textTheme.labelLarge),
                  ),
                  TextButton(
                    key: const ValueKey('picked_clear_button'),
                    onPressed: onClear,
                    child: const Text('清空'),
                  ),
                ],
              ),
              ...picked.asMap().entries.map(
                    (entry) => Padding(
                      padding: const EdgeInsets.symmetric(vertical: 2),
                      child: Row(
                        children: [
                          Icon(
                            entry.value.isDirectory
                                ? Icons.folder_outlined
                                : Icons.insert_drive_file_outlined,
                            size: 18,
                            color: scheme.outline,
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: Text(
                              entry.value.name,
                              key: ValueKey('picked_item_${entry.key}'),
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.bodyMedium,
                            ),
                          ),
                          const SizedBox(width: 8),
                          Text(
                            entry.value.isDirectory
                                ? '文件夹'
                                : formatBytes(entry.value.sizeBytes ?? 0),
                            style: theme.textTheme.bodySmall,
                          ),
                        ],
                      ),
                    ),
                  ),
              const SizedBox(height: 12),
              if (stage == _SendStage.sending) ...[
                const LinearProgressIndicator(
                  key: ValueKey('conn_send_progress'),
                ),
                const SizedBox(height: 8),
                const Text('发送中…',
                    style: TextStyle(fontWeight: FontWeight.w500)),
              ] else
                FilledButton.icon(
                  key: const ValueKey('conn_send_button'),
                  onPressed: live ? onSend : null,
                  icon: const Icon(Icons.send),
                  label: Text('发送（${picked.length} 项）'),
                ),
            ],
            if (stage == _SendStage.done)
              Container(
                key: const ValueKey('conn_send_result'),
                margin: const EdgeInsets.only(top: 12),
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(
                  color: Colors.green.withValues(alpha: 0.08),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Row(
                  children: [
                    const Icon(Icons.check_circle_outline, color: Colors.green),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        '发送完成${summary == null ? '' : '：$summary'}',
                        style: TextStyle(color: Colors.green.shade800),
                      ),
                    ),
                  ],
                ),
              ),
            if (stage == _SendStage.failed)
              Container(
                key: const ValueKey('conn_send_error'),
                margin: const EdgeInsets.only(top: 12),
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(
                  color: scheme.errorContainer,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Row(
                  children: [
                    Icon(Icons.error_outline, color: scheme.onErrorContainer),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        '发送失败：$summary',
                        style: TextStyle(color: scheme.onErrorContainer),
                      ),
                    ),
                  ],
                ),
              ),
            if (!live)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(
                  '连接未就绪，暂时不能发送。',
                  style:
                      theme.textTheme.bodySmall?.copyWith(color: scheme.error),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// Receiver-side card: no send button (one-way), save location shown.
class _ReceiveCard extends StatelessWidget {
  const _ReceiveCard({required this.saveDir});

  final String saveDir;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(Icons.download, color: theme.colorScheme.primary),
                const SizedBox(width: 8),
                Text('此连接用于接收', style: theme.textTheme.titleMedium),
              ],
            ),
            const SizedBox(height: 8),
            const Text('对方发送文件时会弹出提示；点「接受」后开始接收。'),
            const SizedBox(height: 4),
            Text(
              '接收的文件将保存到：$saveDir',
              key: const ValueKey('conn_detail_save_dir'),
              style: theme.textTheme.bodySmall,
            ),
          ],
        ),
      ),
    );
  }
}

/// `firstOrNull` without package:collection.
extension<T> on Iterable<T> {
  T? get firstOrNullSafe => isEmpty ? null : first;
}
