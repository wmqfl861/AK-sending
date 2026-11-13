/// Wave-28 connection detail: live status, the send action (sender-side
/// connections only — one-way pairing), the file/folder picker with a
/// review list, and 断开 with a confirm dialog.
///
/// Client UI U1 (2026-11-13):
///
/// * the send card shows the transfer's LIVE progress (bytes, percent,
///   throughput, time left, the file in flight) and, before any byte
///   moves, says what the send waits for (connecting / the receiver's
///   接受) instead of one generic spinner;
/// * the finished result names the elapsed time and average speed (the
///   service's summary carries the direct-path label);
/// * v1 retires a connection once its ONE transfer finished (the row
///   leaves the live list): the page now keeps the last known connection,
///   says the pairing ENDED after a completed transfer (instead of the
///   misleading "dropped by the peer or a network change"), and keeps the
///   send card with its result rather than flipping to the receive card;
/// * every finished or failed send is recorded in the session's
///   「最近传输」 list.
library;

import 'dart:async' show StreamSubscription;

import 'package:flutter/material.dart';

import '../service/file_pick_gateway.dart';
import '../service/simple_transfer_service.dart';
import '../service/transfer_format.dart';
import '../service/transfer_history.dart';
import '../theme.dart';
import '../widgets/task_connection_views.dart';
import '../widgets/transfer_progress_view.dart';

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
  StreamSubscription<TransferProgress>? _progressSub;
  StreamSubscription<String>? _idleExpirySub;

  /// J2: this pairing ended by idle expiry (latched from the service's
  /// `idleExpiries` stream — terminal rows leave the live snapshot, so
  /// the page learns the CAUSE of the disappearance here).
  bool _idleExpired = false;
  TaskConnection? _connection;

  /// The last row the live list carried for this id: the page's identity
  /// (peer name, direction) after the row left the list.
  TaskConnection? _lastKnown;

  final List<PickedEntry> _picked = [];
  _SendStage _sendStage = _SendStage.idle;
  String? _sendSummary;

  /// U1: the latest progress snapshot of THIS connection's transfer.
  TransferProgress? _progress;

  /// U1: how long the last finished send took (start → peer has all).
  Duration? _sendElapsed;

  /// U1: the byte total the last finished send moved (progress total, or
  /// the picked files' sizes when the engine reported none).
  int? _sentBytes;

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
          _lastKnown = _connection ?? _lastKnown;
        });
      },
      onError: (Object _) {},
    );
    _progressSub = widget.service.transferProgress.listen(
      (progress) {
        if (!mounted || progress.connId != widget.connectionId) return;
        setState(() => _progress = progress);
      },
      onError: (Object _) {},
    );
    _idleExpirySub = widget.service.idleExpiries.listen(
      (connId) {
        if (!mounted || connId != widget.connectionId) return;
        setState(() => _idleExpired = true);
      },
      onError: (Object _) {},
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    _progressSub?.cancel();
    _idleExpirySub?.cancel();
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
      _progress = null;
    });
  }

  Future<void> _send() async {
    if (_picked.isEmpty) return;
    // Read everything the record needs BEFORE the await (no context use
    // after an async gap).
    final history = TransferHistoryScope.read(context);
    final peerName = (_connection ?? _lastKnown)?.peerName ?? '对端';
    final pickedBytes = _knownPickedBytes();
    final fileCount = _picked.every((entry) => !entry.isDirectory)
        ? _picked.length
        : null;
    final started = DateTime.now();
    setState(() {
      _sendStage = _SendStage.sending;
      _sendSummary = null;
      _progress = null;
    });
    try {
      final summary = await widget.service
          .sendFiles(widget.connectionId, _picked.map((e) => e.path).toList());
      final elapsed = DateTime.now().difference(started);
      final bytes = _progress?.totalBytes ?? pickedBytes;
      history?.add(TransferRecord(
        role: TaskRole.send,
        peerName: peerName,
        succeeded: true,
        finishedAt: DateTime.now(),
        fileCount: _progress?.totalFiles ?? fileCount,
        totalBytes: bytes,
        elapsed: elapsed,
        pathLabel: _pathLabelIn(summary),
      ));
      if (!mounted) return;
      setState(() {
        _sendStage = _SendStage.done;
        _sendSummary = summary;
        _sendElapsed = elapsed;
        _sentBytes = bytes;
      });
    } on Object catch (e) {
      history?.add(TransferRecord(
        role: TaskRole.send,
        peerName: peerName,
        succeeded: false,
        finishedAt: DateTime.now(),
        fileCount: fileCount,
        totalBytes: pickedBytes,
        detail: '$e',
      ));
      if (!mounted) return;
      setState(() {
        _sendStage = _SendStage.failed;
        _sendSummary = '$e';
      });
    }
  }

  /// The picked files' total when every entry is a plain file with a
  /// known size (a folder's size is unknown until the engine scans it).
  int? _knownPickedBytes() {
    var total = 0;
    for (final entry in _picked) {
      final size = entry.sizeBytes;
      if (entry.isDirectory || size == null) {
        return null;
      }
      total += size;
    }
    return total;
  }

  /// The direct-path label the service appended to its summary, if any.
  static String? _pathLabelIn(String summary) {
    for (final label in const ['局域网直连', '互联网直连', 'IPv6 直连', '本机直连']) {
      if (summary.contains(label)) {
        return label;
      }
    }
    return null;
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
    final known = connection ?? _lastKnown;
    // U1: v1 retires the row once its one transfer finished — a gone row
    // after a COMPLETED transfer (this page's send, or a receive whose
    // progress reached `completed`) is the pairing's normal end, not a
    // drop.
    final ended = connection == null &&
        (_sendStage == _SendStage.done ||
            _progress?.phase == TransferPhase.completed);
    // CR-P1-07 (review 2026-11-13 §4.4 改法 2): only `disconnected` ends
    // the page's liveness — a failed pairing-time connectivity test
    // (`unreachable`) is informational. The test rides the adopted QUIC
    // socket since H1, but its window can still skew against the peer's
    // punch window (an honest negative), and the send phase carries its
    // own probe round + dial, so the buttons must stay usable and the
    // copy says the send can still be tried.
    final live = connection != null &&
        connection.state != TaskConnectionState.disconnected;
    final peerName = known?.peerName ?? '对端';

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
                      if (ended)
                        _EndedPill(color: successColor(theme.colorScheme))
                      else if (connection != null)
                        ConnectionStateChip(state: connection.state)
                      else
                        const ConnectionStateChip(
                            state: TaskConnectionState.disconnected),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Text(
                    ended
                        ? '本次传输已完成，配对随之结束。如需再次传输，请重新配对。'
                        : _idleExpired ||
                                (connection?.idleExpired ?? false)
                            ? '配对已闲置过久，请重新配对。'
                            : connection == null
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
                          // CR-P1-07: a failed pairing-time test is NOT a
                          // dead end — the send phase retries on its own
                          // probe round and dial (same for the receiver:
                          // nothing gates the incoming transfer).
                          TaskConnectionState.unreachable =>
                            connection.role == TaskRole.send
                                ? '连通测试未通过，仍可尝试发送。'
                                : '连通测试未通过，仍可等待对方发送。',
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
          // U1: the direction comes from the LAST KNOWN row — a retired
          // sender row keeps its send card (and result).
          if (known?.role == TaskRole.send)
            _SendCard(
              live: live,
              ended: ended,
              picked: _picked,
              stage: _sendStage,
              summary: _sendSummary,
              progress: _progress,
              elapsed: _sendElapsed,
              sentBytes: _sentBytes,
              onPick: _pickSource,
              onSend: _send,
              onClear: () => setState(() {
                _picked.clear();
                _sendStage = _SendStage.idle;
                _sendSummary = null;
                _progress = null;
              }),
            )
          else
            _ReceiveCard(saveDir: widget.service.saveDir),
          const SizedBox(height: 24),
          if (ended)
            FilledButton.tonalIcon(
              key: const ValueKey('conn_back_home_button'),
              style: FilledButton.styleFrom(
                padding: const EdgeInsets.symmetric(vertical: 14),
              ),
              onPressed: () => Navigator.of(context).maybePop(),
              icon: const Icon(Icons.arrow_back),
              label: const Text('返回连接列表'),
            )
          else
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

/// U1: the 「已完成」 pill of a pairing whose one transfer finished.
class _EndedPill extends StatelessWidget {
  const _EndedPill({required this.color});

  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.15),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        '已完成',
        key: const ValueKey('conn_state_ended'),
        style: TextStyle(
          color: color,
          fontWeight: FontWeight.w600,
          fontSize: 12,
        ),
      ),
    );
  }
}

/// Sender-side card: pick → review → send → progress → result.
class _SendCard extends StatelessWidget {
  const _SendCard({
    required this.live,
    required this.ended,
    required this.picked,
    required this.stage,
    required this.summary,
    required this.progress,
    required this.elapsed,
    required this.sentBytes,
    required this.onPick,
    required this.onSend,
    required this.onClear,
  });

  final bool live;

  /// U1: the pairing ended after its completed transfer — no new pick.
  final bool ended;

  final List<PickedEntry> picked;
  final _SendStage stage;
  final String? summary;
  final TransferProgress? progress;
  final Duration? elapsed;
  final int? sentBytes;
  final VoidCallback onPick;
  final VoidCallback onSend;
  final VoidCallback onClear;

  /// `用时 12 秒 · 平均 3.2 MB/s` (null when the duration is unknown).
  String? get _resultLine {
    final took = elapsed;
    if (took == null) {
      return null;
    }
    final seconds = took.inMilliseconds / 1000;
    final bytes = sentBytes;
    final average = bytes != null && bytes > 0 && seconds >= 1
        ? ' · 平均 ${formatSpeed(bytes / seconds)}'
        : '';
    return '用时 ${formatElapsed(took)}$average';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final success = successColor(scheme);
    final resultLine = _resultLine;
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
            if (!ended)
              FilledButton.icon(
                key: const ValueKey('conn_pick_files_button'),
                onPressed: live && stage != _SendStage.sending ? onPick : null,
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
                  if (!ended && stage != _SendStage.sending)
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
              if (stage == _SendStage.sending)
                // U1: live progress (the bar keeps the pre-U1
                // `conn_send_progress` key).
                TransferProgressView(
                  progress: progress,
                  role: TaskRole.send,
                  barKey: const ValueKey('conn_send_progress'),
                  keyPrefix: 'conn_send_progress',
                )
              else if (!ended)
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
                  color: successContainerColor(scheme),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(Icons.check_circle_outline, color: success),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            '发送完成${summary == null ? '' : '：$summary'}',
                            style: TextStyle(color: success),
                          ),
                          if (resultLine != null) ...[
                            const SizedBox(height: 2),
                            Text(
                              resultLine,
                              key: const ValueKey('conn_send_result_stats'),
                              style: theme.textTheme.bodySmall
                                  ?.copyWith(color: scheme.onSurfaceVariant),
                            ),
                          ],
                        ],
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
            if (!live && !ended)
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
