/// Wave-28 receive prompts: a top banner stack driven by
/// `SimpleTransferService.incomingRequests` — 「谁要发什么」 with
/// 接受/拒绝, an indeterminate progress row while receiving, and a
/// success card carrying the save path. Visible on both tabs (mounted by
/// the shell above the page content).
///
/// X5 (batch X, 2026-11-22): the pending banner names the SENDER — the
/// offer's self-reported device name (`IncomingRequest.peerName`),
/// falling back to the connection row's label, falling back to
/// 「对方的设备」. The name is SELF-REPORTED, never an identity proof, so
/// it is annotated with the verified fingerprint's first 8 hex
/// (`TaskConnection.peerShortId`, from the connections stream) rather
/// than any 已验证 wording. The payload summary shows the top-level
/// label for multi-item offers (「photos」等 3 项内容) and — PRIVACY: a
/// single-file offer's root IS the file name — a bare 「1 个文件」 for
/// single files (never the name itself). Multi-item counts say 「项」
/// because the engine's file_count includes directory entries; a merged-
/// roots offer reports the internal token 'multi' as its root, which
/// must never reach the screen (codex P2, 2026-11-22).
///
/// Wave-29 P2-11: banners whose consent window closed undecided are
/// REMOVED via `expiredIncomingRequests` (the engine auto-declined them),
/// and every consent answer — accept AND decline — is error-contained: a
/// click that races the window resolves without an unhandled async
/// exception.
///
/// E2/G4 (review 2026-11-12 §6): the pending banner shows a countdown of
/// the remaining seconds (`剩余 N 秒后自动拒绝`), driven by the window the
/// event itself carries (`IncomingRequest.windowSecs`, 120 s since E2 —
/// the frozen v1 model said 30 s). The countdown is pure local state —
/// a one-second ticker, NOT a mirror of the deadline: the NATIVE
/// `consent_expired` event (via `expiredIncomingRequests`) removes the
/// banner at expiry; the ticker only shows the number. An offer with no
/// `windowSecs` (an engine predating the field) shows no countdown.
///
/// Client UI U1 (2026-11-13): while receiving, the banner renders the
/// live [TransferProgress] of the request's connection (bytes, percent,
/// throughput, time left, the file in flight); the finished card names
/// the elapsed time and average speed; and every finished or failed
/// receive is recorded in the session's 「最近传输」 list.
library;

import 'dart:async' show StreamSubscription, Timer;

import 'package:flutter/material.dart';

import '../service/simple_transfer_service.dart';
import '../service/transfer_errors.dart';
import '../service/transfer_format.dart';
import '../service/transfer_history.dart';
import '../theme.dart';
import 'keep_foreground_hint.dart';
import 'transfer_error_card.dart';
import 'transfer_progress_view.dart';

enum _Stage { pending, receiving, done, failed }

/// X5 + codex P2: the payload summary — the top-level label for
/// multi-item offers (「photos」等 3 项内容), a bare count otherwise.
/// PRIVACY: a single-file offer's root IS the file name, so it is NEVER
/// shown. Multi-item counts say 「项」 because the engine's file_count
/// includes directory entries (crates/ak-core/src/manifest.rs:347) —
/// claiming N 个文件 would be wrong when folders ride along. A root of
/// 'multi' (the engine's display_name for merged roots,
/// crates/ak-engine/src/sender.rs:182) or an empty one is an internal
/// token, not a name: the count label alone.
String _payloadLabelOf(IncomingRequest request) {
  final count = request.fileCount;
  if (count == 1) {
    // The frozen single-file rule: the root IS the filename — bare count.
    return '1 个文件';
  }
  final root = request.rootName;
  final namedRoot =
      (root != null && root.isNotEmpty && root != 'multi') ? root : null;
  if (namedRoot == null) {
    return '$count 项内容';
  }
  return '「$namedRoot」等 $count 项内容';
}

class _BannerState {
  _BannerState(this.request, this.stage);

  final IncomingRequest request;
  _Stage stage;

  /// W3: the raw failure OBJECT of a failed receive (mapped to
  /// title/reason/suggestion; the original only in 「详情」).
  Object? error;

  /// How long the accepted receive took, from the 接受 click to the end
  /// (set when it finished).
  Duration? elapsed;
}

class IncomingRequestsHost extends StatefulWidget {
  const IncomingRequestsHost({super.key, required this.service});

  final SimpleTransferService service;

  @override
  State<IncomingRequestsHost> createState() => _IncomingRequestsHostState();
}

class _IncomingRequestsHostState extends State<IncomingRequestsHost> {
  StreamSubscription<IncomingRequest>? _sub;
  StreamSubscription<String>? _expiredSub;
  StreamSubscription<TransferProgress>? _progressSub;
  StreamSubscription<List<TaskConnection>>? _connectionsSub;
  final List<_BannerState> _banners = [];

  /// U1: the latest progress snapshot per connection id.
  final Map<String, TransferProgress> _progress = {};

  /// U1: peer labels by connection id (the 「最近传输」 record names the
  /// sender; the offer itself carries no peer name).
  final Map<String, String> _peerNames = {};

  /// X5: the verified-fingerprint short codes by connection id — the
  /// identity anchor annotated beside the self-reported peer names.
  final Map<String, String> _peerShortIds = {};

  @override
  void initState() {
    super.initState();
    _sub = widget.service.incomingRequests.listen((request) {
      if (!mounted) return;
      setState(() => _banners.add(_BannerState(request, _Stage.pending)));
    }, onError: (Object _) {});
    _expiredSub = widget.service.expiredIncomingRequests.listen((jobId) {
      if (!mounted) return;
      // The engine auto-declined the offer: close its banner instead of
      // leaving a dead accept/reject pair on screen (P2-11).
      setState(
        () => _banners.removeWhere((banner) => banner.request.jobId == jobId),
      );
    }, onError: (Object _) {});
    _progressSub = widget.service.transferProgress.listen((progress) {
      if (!mounted) return;
      // Only receives this host shows a banner for are worth a rebuild.
      final shown = _banners.any((banner) =>
          banner.request.connId == progress.connId &&
          banner.stage == _Stage.receiving);
      if (shown) {
        setState(() => _progress[progress.connId] = progress);
      } else {
        _progress[progress.connId] = progress;
      }
    }, onError: (Object _) {});
    _connectionsSub = widget.service.connections.listen((rows) {
      // X5: a LATE identity fact (the connection row landing after the
      // banner rendered) must refresh the pending banners' peer labels.
      var identityChanged = false;
      for (final row in rows) {
        identityChanged |= _peerNames[row.id] != row.peerName;
        _peerNames[row.id] = row.peerName;
        final short = row.peerShortId;
        identityChanged |= short != null && _peerShortIds[row.id] != short;
        if (short != null) {
          _peerShortIds[row.id] = short;
        }
      }
      // Keep progress only for live rows and rows a banner still shows.
      final live = {for (final row in rows) row.id};
      final bannered = {for (final banner in _banners) banner.request.connId};
      _progress.removeWhere(
          (connId, _) => !live.contains(connId) && !bannered.contains(connId));
      if (identityChanged && _banners.isNotEmpty) {
        setState(() {});
      }
    }, onError: (Object _) {});
  }

  @override
  void dispose() {
    _sub?.cancel();
    _expiredSub?.cancel();
    _progressSub?.cancel();
    _connectionsSub?.cancel();
    super.dispose();
  }

  /// The sender's label for [banner]'s record (「对方的设备」 when no
  /// name is known — 审核 §4.5: 对方, never 对端).
  String _peerOf(_BannerState banner) {
    final connId = banner.request.connId;
    return (connId == null ? null : _peerNames[connId]) ?? '对方的设备';
  }

  /// X5: the sender identity the pending banner names — the offer's
  /// self-reported device name (first choice), the connection row's
  /// label, or 「对方的设备」 when neither is known. The name is
  /// SELF-REPORTED, so the verified fingerprint's short code rides along
  /// when the connection row carries it (never any 已验证 wording).
  String _peerLabelOf(_BannerState banner) {
    final connId = banner.request.connId;
    final name = banner.request.peerName ??
        (connId == null ? null : _peerNames[connId]);
    final short = connId == null ? null : _peerShortIds[connId];
    if (name == null || name.isEmpty) {
      return '对方的设备';
    }
    if (short == null || short.isEmpty) {
      return name;
    }
    return '$name · $short';
  }

  Future<void> _respond(_BannerState banner, bool accept) async {
    if (banner.stage != _Stage.pending) return;
    if (!accept) {
      setState(() => _banners.remove(banner));
      try {
        await widget.service.consent(banner.request.jobId, false);
      } on Object {
        // P2-11: a decline racing the consent window (or a disposed
        // engine) must not surface as an unhandled async exception —
        // nothing was received either way.
      }
      return;
    }
    // U1: read the session history BEFORE the await (no context use
    // after an async gap).
    final history = TransferHistoryScope.read(context);
    final started = DateTime.now();
    setState(() => banner.stage = _Stage.receiving);
    try {
      await widget.service.consent(banner.request.jobId, true);
      final elapsed = DateTime.now().difference(started);
      history?.add(TransferRecord(
        role: TaskRole.receive,
        peerName: _peerOf(banner),
        succeeded: true,
        finishedAt: DateTime.now(),
        fileCount: banner.request.fileCount,
        totalBytes: banner.request.totalBytes,
        elapsed: elapsed,
      ));
      if (!mounted) return;
      setState(() {
        banner.stage = _Stage.done;
        banner.elapsed = elapsed;
      });
    } on Object catch (e) {
      history?.add(TransferRecord(
        role: TaskRole.receive,
        peerName: _peerOf(banner),
        succeeded: false,
        finishedAt: DateTime.now(),
        fileCount: banner.request.fileCount,
        totalBytes: banner.request.totalBytes,
        detail: '$e',
      ));
      if (!mounted) return;
      setState(() {
        banner.stage = _Stage.failed;
        banner.error = e;
      });
    }
  }

  void _dismiss(_BannerState banner) {
    setState(() => _banners.remove(banner));
  }

  /// U1: the latest progress of [banner]'s receive (null before the first
  /// snapshot, or when the offer did not name its connection).
  TransferProgress? _progressOf(_BannerState banner) {
    final connId = banner.request.connId;
    return connId == null ? null : _progress[connId];
  }

  @override
  Widget build(BuildContext context) {
    if (_banners.isEmpty) {
      return const SizedBox.shrink(key: ValueKey('incoming_host_empty'));
    }
    return SafeArea(
      bottom: false,
      child: Column(
        key: const ValueKey('incoming_host'),
        children: [
          for (final banner in List.of(_banners))
            _IncomingBannerCard(
              banner: banner,
              saveDir: widget.service.saveDir,
              progress: _progressOf(banner),
              peerLabel: _peerLabelOf(banner),
              onAccept: () => _respond(banner, true),
              onReject: () => _respond(banner, false),
              onDismiss: () => _dismiss(banner),
            ),
        ],
      ),
    );
  }
}

/// E2/G4: the pending card is stateful for one reason — the countdown
/// ticker. It counts SECONDS SINCE OFFER (a plain tick counter, so
/// widget tests drive it deterministically with `pump(1s)`), and the
/// remaining number is `windowSecs - ticks` clamped at 0. The native
/// expiry (`consent_expired` → `expiredIncomingRequests`) still owns the
/// banner's removal; the ticker never decides anything.
class _IncomingBannerCard extends StatefulWidget {
  const _IncomingBannerCard({
    required this.banner,
    required this.saveDir,
    required this.progress,
    required this.peerLabel,
    required this.onAccept,
    required this.onReject,
    required this.onDismiss,
  });

  final _BannerState banner;
  final String saveDir;

  /// U1: the receive's latest progress snapshot (null = none yet).
  final TransferProgress? progress;

  /// X5: the sender identity line — the self-reported device name with
  /// the verified fingerprint's short code annotated (or the fallbacks
  /// the host resolves; see `_peerLabelOf`).
  final String peerLabel;

  final VoidCallback onAccept;
  final VoidCallback onReject;
  final VoidCallback onDismiss;

  @override
  State<_IncomingBannerCard> createState() => _IncomingBannerCardState();
}

class _IncomingBannerCardState extends State<_IncomingBannerCard> {
  Timer? _ticker;
  int _ticks = 0;

  @override
  void initState() {
    super.initState();
    _armTicker();
  }

  @override
  void didUpdateWidget(covariant _IncomingBannerCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    // A stage change (accept clicked → receiving) stops the countdown;
    // a banner that somehow returns to pending (not a real flow) re-arms.
    if (widget.banner.stage != _Stage.pending) {
      _ticker?.cancel();
    } else if (_ticker == null || !_ticker!.isActive) {
      _ticks = 0;
      _armTicker();
    }
  }

  void _armTicker() {
    final window = widget.banner.request.windowSecs;
    // No window on the offer (an engine predating E2/G4) → no countdown:
    // a guessed number would be a lie.
    if (window == null ||
        window <= 0 ||
        widget.banner.stage != _Stage.pending) {
      return;
    }
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (!mounted) return;
      if (widget.banner.stage != _Stage.pending) {
        _ticker?.cancel();
        return;
      }
      if (_ticks < window) {
        setState(() => _ticks += 1);
      }
      // At 0 the ticker goes quiet: the native consent_expired event
      // removes the banner (or the user's click raced it and the stage
      // already moved on).
    });
  }

  @override
  void dispose() {
    _ticker?.cancel();
    super.dispose();
  }

  /// codex P2: the success card's count label — 「项」 for multi-item
  /// offers (file_count includes directory entries, so 个文件 would be a
  /// false claim), the frozen 「1 个文件」 for single files.
  String _receivedLabelOf(IncomingRequest request) =>
      request.fileCount == 1 ? '1 个文件' : '${request.fileCount} 项内容';

  /// U1: `用时 12 秒 · 平均 3.2 MB/s` for a finished receive (null when
  /// the duration was not measured).
  String? _summaryLine(IncomingRequest request) {
    final elapsed = widget.banner.elapsed;
    if (elapsed == null) {
      return null;
    }
    final seconds = elapsed.inMilliseconds / 1000;
    final average = request.totalBytes > 0 && seconds >= 1
        ? ' · 平均 ${formatSpeed(request.totalBytes / seconds)}'
        : '';
    return '用时 ${formatElapsed(elapsed)}$average';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final request = widget.banner.request;
    final jobId = request.jobId;
    final window = request.windowSecs;

    return Card(
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Icon(
                  Icons.download_for_offline_outlined,
                  color: scheme.primary,
                ),
                const SizedBox(width: 8),
                Expanded(
                  // Phone-width guard: the whole sentence ellipsizes after
                  // the size instead of overflowing.
                  child: Text(
                    // X5: WHO (self-reported name · verified-fp short
                    // code, or the fallbacks) sends WHAT (top-level
                    // summary; a single file's name is never shown).
                    '${widget.peerLabel} 请求发送'
                    ' ${_payloadLabelOf(request)}'
                    '（共 ${formatBytes(request.totalBytes)}）',
                    key: ValueKey('incoming_title_$jobId'),
                    style: theme.textTheme.titleSmall,
                  ),
                ),
              ],
            ),
            if (widget.banner.stage == _Stage.pending) ...[
              const SizedBox(height: 8),
              // Wave-30 P2-06: the request's REAL target — the engine
              // snapshotted the folder at offer time and the receive
              // lands there even if the settings change before the user
              // answers, so the banner follows the event's field (the
              // live setting only stands in when the engine predates it).
              Row(
                children: [
                  Icon(Icons.folder_outlined, size: 16, color: scheme.outline),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      '将保存到：${request.saveDir ?? widget.saveDir}',
                      key: ValueKey('incoming_target_$jobId'),
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodySmall,
                    ),
                  ),
                ],
              ),
              if (window != null && window > 0) ...[
                const SizedBox(height: 4),
                // E2/G4 countdown: the number the native window will act
                // on. Clamped at 0 — the removal is the native event's.
                Text(
                  window - _ticks > 0
                      ? '剩余 ${window - _ticks} 秒后自动拒绝'
                      : '即将自动拒绝…',
                  key: ValueKey('incoming_countdown_$jobId'),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.outline,
                  ),
                ),
              ],
              const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    child: OutlinedButton(
                      key: ValueKey('incoming_reject_$jobId'),
                      onPressed: widget.onReject,
                      child: const Text('拒绝'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: FilledButton(
                      key: ValueKey('incoming_accept_$jobId'),
                      onPressed: widget.onAccept,
                      child: const Text('接受'),
                    ),
                  ),
                ],
              ),
            ],
            if (widget.banner.stage == _Stage.receiving) ...[
              const SizedBox(height: 10),
              // U1: the live progress of this receive (an indeterminate
              // bar + 「正在接收…」 until the first snapshot lands).
              TransferProgressView(
                progress: widget.progress,
                role: TaskRole.receive,
                barKey: ValueKey('incoming_progress_$jobId'),
                keyPrefix: 'incoming_progress_$jobId',
              ),
              const SizedBox(height: 6),
              // W7.3: the Android foreground hint while bytes flow.
              KeepForegroundHint(keyName: 'incoming_keep_foreground_$jobId'),
            ],
            if (widget.banner.stage == _Stage.done) ...[
              const SizedBox(height: 8),
              Row(
                children: [
                  Icon(
                    Icons.check_circle,
                    color: successColor(scheme),
                    size: 20,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      '已接收 ${_receivedLabelOf(request)}，'
                      '保存在：${request.saveDir ?? widget.saveDir}',
                      key: ValueKey('incoming_success_$jobId'),
                      style: theme.textTheme.bodyMedium?.copyWith(
                        color: successColor(scheme),
                      ),
                    ),
                  ),
                ],
              ),
              if (_summaryLine(request) case final summary?) ...[
                const SizedBox(height: 2),
                Padding(
                  padding: const EdgeInsets.only(left: 26),
                  child: Text(
                    summary,
                    key: ValueKey('incoming_summary_$jobId'),
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: scheme.onSurfaceVariant),
                  ),
                ),
              ],
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  key: ValueKey('incoming_dismiss_$jobId'),
                  onPressed: widget.onDismiss,
                  child: const Text('知道了'),
                ),
              ),
            ],
            if (widget.banner.stage == _Stage.failed) ...[
              const SizedBox(height: 8),
              // W3 (V-N4): the mapped failure card — title/reason/
              // suggestion by error code; the original text only in the
              // expandable 「详情」. Keeps the frozen `incoming_error_<job>`
              // key on the wrapper.
              TransferErrorCard(
                key: ValueKey('incoming_error_$jobId'),
                error: widget.banner.error ?? StateError('接收失败'),
                scene: TransferErrorScene.receiving,
                keyPrefix: 'incoming_error_$jobId',
              ),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  key: ValueKey('incoming_error_dismiss_$jobId'),
                  onPressed: widget.onDismiss,
                  child: const Text('知道了'),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
