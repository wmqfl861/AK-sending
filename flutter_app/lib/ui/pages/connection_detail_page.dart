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
///
/// X4 (batch X, audit §4.3) — the primary send path's automatic send:
/// the home page hands the PRESELECTED entries in [initialPicked] and
/// [autoSendOnReady]; once the connection row is live (connected, or the
/// paired-but-unreachable probe verdict — the send phase runs its own
/// probe round and dial, so that verdict does not block), the page sends
/// them automatically EXACTLY ONCE (审核 §4.3 发送方主路径: 对方加入且
/// 连通后自动开始发送，不再需要点任何按钮).
///
/// Batch Z: a folder picked on Android is a STAGED copy in app-private
/// storage (the engine only takes filesystem paths). Whoever holds the
/// [PickedEntry] owns that copy — this page hands the staged entries back
/// through [FilePickGateway.releaseStaged] wherever its ownership of them
/// ends: a SUCCESSFUL send (they also leave the review list, so nothing
/// can send a released path again), 清空, a new pick replacing them, and
/// `dispose` while no send is in flight. A FAILED send releases nothing
/// on its own (see `_send`).
library;

import 'dart:async' show StreamSubscription, TimeoutException, unawaited;

import 'package:flutter/material.dart';

import '../service/file_pick_gateway.dart';
import '../service/simple_transfer_service.dart';
import '../service/transfer_errors.dart';
import '../service/transfer_format.dart';
import '../service/transfer_history.dart';
import '../theme.dart';
import '../widgets/keep_foreground_hint.dart';
import '../widgets/task_connection_views.dart';
import '../widgets/transfer_error_card.dart';
import '../widgets/transfer_progress_view.dart';

enum _SendStage { idle, sending, done, failed }

class ConnectionDetailPage extends StatefulWidget {
  const ConnectionDetailPage({
    super.key,
    required this.service,
    required this.connectionId,
    this.picker = const PluginFilePickGateway(),
    this.autoPickOnOpen = false,
    this.initialPicked = const <PickedEntry>[],
    this.autoSendOnReady = false,
  });

  final SimpleTransferService service;
  final String connectionId;
  final FilePickGateway picker;

  /// W7.2: open the file/folder picker ONCE on entry (the pairing-flow
  /// success shortcut — the sender lands here ready to choose files). A
  /// cancelled pick simply leaves the page as-is.
  final bool autoPickOnOpen;

  /// X4: the primary send path's preselected entries (chosen BEFORE the
  /// code was minted) — seeds the review list so [autoSendOnReady] has
  /// its payload.
  final List<PickedEntry> initialPicked;

  /// X4: send [initialPicked] automatically, exactly once, as soon as the
  /// connection row is live (connected or paired-unreachable). The
  /// automatic send goes through the SAME [_send] path as the manual
  /// button (progress, W3 error card, 「最近传输」 recording).
  final bool autoSendOnReady;

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

  /// X4: the exactly-once latch of the automatic send.
  bool _autoSendDone = false;

  /// W3: the raw error object of the last failed send — the failure card
  /// maps it (title/reason/suggestion) and keeps the original text ONLY
  /// inside the expandable 「详情」.
  Object? _sendError;

  /// U1: the latest progress snapshot of THIS connection's transfer.
  TransferProgress? _progress;

  /// U1: how long the last finished send took (start → peer has all).
  Duration? _sendElapsed;

  /// U1: the byte total the last finished send moved (progress total, or
  /// the picked files' sizes when the engine reported none).
  int? _sentBytes;

  /// Batch Z: the staged folders of the send in flight (empty when none) —
  /// the engine's to read until that send settles. Nothing else hands them
  /// back meanwhile: a pick that replaces the review list while the send
  /// still runs leaves them to the send's own completion.
  Set<String> _readingStagedIds = const <String>{};

  /// Batch Z: a send is with the engine right now. Set with the sending
  /// STAGE and cleared when the send settles — but unlike `_sendStage`,
  /// which a pick landing during the send resets with the rest of the
  /// card, it stays true until the engine has answered.
  bool _sendInFlight = false;

  @override
  void initState() {
    super.initState();
    // X4: the primary send path's preselected payload.
    _picked.addAll(widget.initialPicked);
    _sub = widget.service.connections.listen(
      (snapshot) {
        if (!mounted) return;
        TaskConnection? current;
        for (final connection in snapshot) {
          if (connection.id == widget.connectionId) {
            current = connection;
            break;
          }
        }
        setState(() {
          _connection = current;
          _lastKnown = _connection ?? _lastKnown;
        });
        _maybeAutoSend(current);
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
    if (widget.autoPickOnOpen) {
      // W7.2: one post-frame auto pick — the sheet needs a frame of the
      // page beneath it. Cancelling keeps the page (nothing selected).
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) {
          _pickSource(roleHint: true);
        }
      });
    }
  }

  @override
  void dispose() {
    _sub?.cancel();
    _progressSub?.cancel();
    _idleExpirySub?.cancel();
    // Batch Z: the page is gone and hands back the staged folders still in
    // its review list — unless a send is in flight: that send's own
    // completion decides (a SUCCESS releases, a failure never does), else
    // the stale cleanup of a later process owns them. (A pick that landed
    // during a send has replaced the list and reset the stage: what is
    // listed then is NOT what the engine reads, so handing it back is
    // safe — the entries being read are that send's, not the list's.)
    if (_sendStage != _SendStage.sending) {
      _releaseStaged(widget.picker, _picked);
    }
    super.dispose();
  }

  /// X4: the primary send path's automatic send — fires exactly once, on
  /// the first connections snapshot whose row for THIS page is live.
  /// "Live" includes the paired-but-unreachable probe verdict: the send
  /// phase runs its own probe round and dial on the real socket
  /// (CR-P1-07), so the pairing-time verdict never blocks the send
  /// (审核 §4.3: 对方加入且连通后自动开始发送).
  ///
  /// Exactly once is latched by `_autoSendDone` BEFORE the async send
  /// starts (a second snapshot can never start a parallel send), and the
  /// payload is seeded from [ConnectionDetailPage.initialPicked] exactly
  /// once — in `initState`, which a rebuild of the pushed route never
  /// re-runs (the route's builder may build again, the State is kept).
  void _maybeAutoSend(TaskConnection? row) {
    if (!widget.autoSendOnReady || _autoSendDone || _picked.isEmpty) {
      return;
    }
    if (row == null || row.state == TaskConnectionState.disconnected) {
      return;
    }
    // X4 P1-1(c): only a SEND-direction row may carry the automatic send —
    // `sendFiles` is invalid on every other connection. The home layer
    // hands over for send connections only; this keeps that true for any
    // future caller instead of failing inside the send.
    if (row.role != TaskRole.send) {
      return;
    }
    _autoSendDone = true;
    _send();
  }

  /// Batch Z: the staged folders among [entries] — the only ones anyone
  /// owns (a plain file or folder pick is the user's own path).
  static List<PickedEntry> _stagedOf(Iterable<PickedEntry> entries) => [
        for (final entry in entries)
          if (entry.stagingId != null) entry,
      ];

  /// Batch Z: hands the staged folders behind [entries] back to [picker].
  /// Fire-and-forget — it never delays or reorders the flow — and does
  /// nothing when [entries] holds no staged folder.
  static void _releaseStaged(
      FilePickGateway picker, Iterable<PickedEntry> entries) {
    final staged = _stagedOf(entries);
    if (staged.isNotEmpty) {
      unawaited(picker.releaseStaged(staged));
    }
  }

  /// Batch Z: a failed send that signals an UNCONFIRMED native cancel —
  /// the `TimeoutException` `SimpleTransferService.sendFiles` rethrows when
  /// cancelling the native task fails. The service has no typed error for
  /// it, so the text of its message is the only signal.
  static bool _isUnconfirmedNativeCancel(Object error) =>
      error is TimeoutException &&
      (error.message ?? '').contains('取消原生任务失败');

  Future<void> _pickSource({bool roleHint = false}) async {
    // Batch Z: read before the awaits — the unmounted branch below must
    // still reach the gateway.
    final picker = widget.picker;
    final mode = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // W7.1/W7.2: the auto-opened picker (the sender handover
              // from a pairing success) leads with the role statement.
              if (roleHint) ...[
                Text(
                  '现在可以选择文件发给对方',
                  key: const ValueKey('pick_sheet_role_hint'),
                  style: Theme.of(context).textTheme.bodyMedium,
                ),
                const SizedBox(height: 8),
              ],
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
      // Batch Z: the SEND-side pick — on Android it goes through the SAF
      // tree picker and a staged copy. The plain `pickDirectory()` stays
      // with the receive save-dir pick of the settings page.
      final dir = await picker.pickSendDirectory();
      entries = dir == null ? const <PickedEntry>[] : <PickedEntry>[dir];
    } else {
      entries = await picker.pickFiles();
    }
    if (!mounted) {
      // Batch Z: the page left while the picker was up — the entries it
      // just obtained were never shown, so nothing else owns their staged
      // copies.
      _releaseStaged(picker, entries);
      return;
    }
    // Batch Z: the new selection replaces the old one — the staged folders
    // of the replaced selection go back with it. EXCEPT the ones a send in
    // flight is still reading (a pick opened before that send started can
    // return while it runs): those are the send's own completion to hand
    // back, never a path that runs while the engine may still be reading.
    final replaced = [
      for (final entry in _picked)
        if (!_readingStagedIds.contains(entry.stagingId)) entry,
    ];
    setState(() {
      _picked
        ..clear()
        ..addAll(entries);
      _sendStage = _SendStage.idle;
      _sendSummary = null;
      _sendError = null;
      _progress = null;
    });
    _releaseStaged(picker, replaced);
  }

  Future<void> _send() async {
    // Batch Z: one send at a time — while a send is in flight its entries
    // are the engine's to read; a second start must not hand them over
    // again (a double tap, a duplicate auto-send snapshot). `_sendInFlight`
    // also covers a pick that landed during the send and reset the STAGE.
    if (_sendStage == _SendStage.sending || _sendInFlight) return;
    if (_picked.isEmpty) return;
    // Read everything the record needs BEFORE the await (no context use
    // after an async gap).
    final history = TransferHistoryScope.read(context);
    final service = widget.service;
    final picker = widget.picker;
    final connectionId = widget.connectionId;
    final peerName = (_connection ?? _lastKnown)?.peerName ?? '对端';
    final pickedBytes = _knownPickedBytes();
    final fileCount = _picked.every((entry) => !entry.isDirectory)
        ? _picked.length
        : null;
    // Batch Z: the entries THIS send hands the engine — the ownership
    // decisions when it settles are made on this snapshot.
    final sending = List<PickedEntry>.of(_picked);
    final stagedIds = {
      for (final entry in _stagedOf(sending)) entry.stagingId!,
    };
    final started = DateTime.now();
    setState(() {
      _sendStage = _SendStage.sending;
      _sendInFlight = true;
      _readingStagedIds = stagedIds;
      _sendSummary = null;
      _sendError = null;
      _progress = null;
    });
    try {
      final summary = await service.sendFiles(
          connectionId, [for (final entry in sending) entry.path]);
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
      // Batch Z: the engine is done reading — the staged folders of THIS
      // send go back, also when the page is gone by now (its dispose
      // released nothing: the send was in flight, so nothing else owns
      // them).
      _releaseStaged(picker, sending);
      _readingStagedIds = const <String>{}; // the engine is done reading
      _sendInFlight = false;
      if (!mounted) return;
      setState(() {
        // …and they leave the review list: nothing can send a released
        // path again. The result display stays.
        _picked.removeWhere((entry) => stagedIds.contains(entry.stagingId));
        _sendStage = _SendStage.done;
        _sendSummary = summary;
        _sendElapsed = elapsed;
        _sentBytes = bytes;
      });
    } on Object catch (e) {
      // Batch Z: the engine has answered — this send is no longer in
      // flight. (An UNCONFIRMED native cancel may STILL be reading in the
      // background; its copies are QUARANTINED below: they leave the review
      // list, so no path that reads it can ever hand them back.)
      _readingStagedIds = const <String>{};
      _sendInFlight = false;
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
        // W3: keep the OBJECT — the card maps it; the original text only
        // renders inside the expandable 「详情」.
        _sendError = e;
        // Batch Z: a failure releases NOTHING on its own — a returned
        // Future does not prove the engine stopped reading; a mounted page
        // keeps the entries for a retry, an unmounted one leaves them to
        // the stale cleanup. One failure shape still changes the list: an
        // UNCONFIRMED native cancel (the engine may STILL be reading these
        // files in the background) QUARANTINES the staged folders of this
        // send — they leave the review list (nothing can resend them) and
        // are never handed back by any later path: 清空, a new pick and
        // dispose all read this list. The stale cleanup of a later process
        // removes the copies.
        if (_isUnconfirmedNativeCancel(e)) {
          _picked
              .removeWhere((entry) => stagedIds.contains(entry.stagingId));
        }
      });
    }
  }

  /// 清空: the review list goes. Batch Z: with it the page's ownership of
  /// the staged folders that were in it.
  void _clearPicked() {
    final removed = List<PickedEntry>.of(_picked);
    setState(() {
      _picked.clear();
      _sendStage = _SendStage.idle;
      _sendSummary = null;
      _sendError = null;
      _progress = null;
    });
    _releaseStaged(widget.picker, removed);
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
                        : _idleExpired || (connection?.idleExpired ?? false)
                            // W3.3 (V-N4): the idle copy ONLY for the
                            // engine's idle-expiry reason (the dedicated
                            // idleExpiries stream / row flag) — a plain
                            // break is the interruption copy below.
                            ? '配对已闲置过久，请重新配对。'
                            : connection == null
                                // W3.3: a dropped row without the idle
                                // reason is an INTERRUPTION — W1's copy
                                // (title + resume suggestion), never the
                                // misleading idle wording.
                                ? interruptedStatusLine
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
              error: _sendError,
              progress: _progress,
              elapsed: _sendElapsed,
              sentBytes: _sentBytes,
              onPick: _pickSource,
              onSend: _send,
              onClear: _clearPicked,
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
    required this.error,
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

  /// W3: the raw error of the failed send (null in every other stage) —
  /// rendered through the mapping, never verbatim outside 「详情」.
  final Object? error;

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
            // P2-B (third review): paired and WAITING for the file
            // selection is a keep-foreground stage too (the pairing dies
            // with a backgrounded process — the W-N11 reproduction).
            // Android-only, the W7.3 copy adapted per stage; a picked
            // review list or a running send shows its own state instead.
            if (live && picked.isEmpty)
              const Padding(
                padding: EdgeInsets.only(top: 8),
                child: KeepForegroundHint(
                  keyName: 'conn_pick_keep_foreground',
                  text: '等待选择文件期间请保持本应用在前台',
                ),
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
              if (stage == _SendStage.sending) ...[
                // U1: live progress (the bar keeps the pre-U1
                // `conn_send_progress` key).
                TransferProgressView(
                  progress: progress,
                  role: TaskRole.send,
                  barKey: const ValueKey('conn_send_progress'),
                  keyPrefix: 'conn_send_progress',
                ),
                const SizedBox(height: 6),
                // W7.3: Android foreground hint under a running send.
                const KeepForegroundHint(
                  keyName: 'conn_send_keep_foreground',
                ),
              ]
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
              // W3 (V-N4): the mapped failure card — title/reason/
              // suggestion from the error code; the original text only
              // inside the expandable 「详情」 (with copy). Keeps the
              // frozen `conn_send_error` key on the card's wrapper.
              Padding(
                key: const ValueKey('conn_send_error'),
                padding: const EdgeInsets.only(top: 12),
                child: TransferErrorCard(
                  error: error ?? StateError('发送失败'),
                  scene: TransferErrorScene.sending,
                  keyPrefix: 'conn_send_error',
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
