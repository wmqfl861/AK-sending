/// Wave-28 receive prompts: a top banner stack driven by
/// `SimpleTransferService.incomingRequests` — 「对方请求发送 N 个文件」
/// with 接受/拒绝, an indeterminate progress row while receiving, and a
/// success card carrying the save path. Visible on both tabs (mounted by
/// the shell above the page content).
///
/// Wave-29 P2-11: banners whose 30 s consent window closed undecided are
/// REMOVED via `expiredIncomingRequests` (the engine auto-declined them),
/// and every consent answer — accept AND decline — is error-contained: a
/// click that races the window resolves without an unhandled async
/// exception.
library;

import 'dart:async' show StreamSubscription;

import 'package:flutter/material.dart';

import '../service/simple_transfer_service.dart';

enum _Stage { pending, receiving, done, failed }

class _BannerState {
  _BannerState(this.request, this.stage);

  final IncomingRequest request;
  _Stage stage;
  String? error;
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
  final List<_BannerState> _banners = [];

  @override
  void initState() {
    super.initState();
    _sub = widget.service.incomingRequests.listen(
      (request) {
        if (!mounted) return;
        setState(() => _banners.add(_BannerState(request, _Stage.pending)));
      },
      onError: (Object _) {},
    );
    _expiredSub = widget.service.expiredIncomingRequests.listen(
      (jobId) {
        if (!mounted) return;
        // The engine auto-declined the offer: close its banner instead of
        // leaving a dead accept/reject pair on screen (P2-11).
        setState(() => _banners.removeWhere(
            (banner) => banner.request.jobId == jobId));
      },
      onError: (Object _) {},
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    _expiredSub?.cancel();
    super.dispose();
  }

  Future<void> _respond(_BannerState banner, bool accept) async {
    if (banner.stage != _Stage.pending) return;
    if (!accept) {
      setState(() => _banners.remove(banner));
      try {
        await widget.service.consent(banner.request.jobId, false);
      } on Object {
        // P2-11: a decline racing the 30 s window (or a disposed engine)
        // must not surface as an unhandled async exception — nothing was
        // received either way.
      }
      return;
    }
    setState(() => banner.stage = _Stage.receiving);
    try {
      await widget.service.consent(banner.request.jobId, true);
      if (!mounted) return;
      setState(() => banner.stage = _Stage.done);
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        banner.stage = _Stage.failed;
        banner.error = '$e';
      });
    }
  }

  void _dismiss(_BannerState banner) {
    setState(() => _banners.remove(banner));
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
              onAccept: () => _respond(banner, true),
              onReject: () => _respond(banner, false),
              onDismiss: () => _dismiss(banner),
            ),
        ],
      ),
    );
  }
}

class _IncomingBannerCard extends StatelessWidget {
  const _IncomingBannerCard({
    required this.banner,
    required this.saveDir,
    required this.onAccept,
    required this.onReject,
    required this.onDismiss,
  });

  final _BannerState banner;
  final String saveDir;
  final VoidCallback onAccept;
  final VoidCallback onReject;
  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final request = banner.request;
    final jobId = request.jobId;

    return Card(
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Icon(Icons.download_for_offline_outlined,
                    color: scheme.primary),
                const SizedBox(width: 8),
                Expanded(
                  // Phone-width guard: the whole sentence ellipsizes after
                  // the size instead of overflowing.
                  child: Text(
                    '对方请求发送 ${request.fileCount} 个文件'
                    '（共 ${formatBytes(request.totalBytes)}）',
                    key: ValueKey('incoming_title_$jobId'),
                    style: theme.textTheme.titleSmall,
                  ),
                ),
              ],
            ),
            if (banner.stage == _Stage.pending) ...[
              const SizedBox(height: 8),
              // Wave-30 P2-06: the request's REAL target — the engine
              // snapshotted the folder at offer time and the receive
              // lands there even if the settings change before the user
              // answers, so the banner follows the event's field (the
              // live setting only stands in when the engine predates it).
              Row(
                children: [
                  Icon(Icons.folder_outlined,
                      size: 16, color: scheme.outline),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      '将保存到：${request.saveDir ?? saveDir}',
                      key: ValueKey('incoming_target_$jobId'),
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodySmall,
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    child: OutlinedButton(
                      key: ValueKey('incoming_reject_$jobId'),
                      onPressed: onReject,
                      child: const Text('拒绝'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: FilledButton(
                      key: ValueKey('incoming_accept_$jobId'),
                      onPressed: onAccept,
                      child: const Text('接受'),
                    ),
                  ),
                ],
              ),
            ],
            if (banner.stage == _Stage.receiving) ...[
              const SizedBox(height: 8),
              LinearProgressIndicator(
                  key: ValueKey('incoming_progress_$jobId')),
              const SizedBox(height: 6),
              Text('正在接收…', style: theme.textTheme.bodySmall),
            ],
            if (banner.stage == _Stage.done) ...[
              const SizedBox(height: 8),
              Row(
                children: [
                  Icon(Icons.check_circle, color: Colors.green.shade700,
                      size: 20),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      '已接收 ${request.fileCount} 个文件，'
                      '保存在：${request.saveDir ?? saveDir}',
                      key: ValueKey('incoming_success_$jobId'),
                      style: theme.textTheme.bodyMedium
                          ?.copyWith(color: Colors.green.shade800),
                    ),
                  ),
                ],
              ),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  key: ValueKey('incoming_dismiss_$jobId'),
                  onPressed: onDismiss,
                  child: const Text('知道了'),
                ),
              ),
            ],
            if (banner.stage == _Stage.failed) ...[
              const SizedBox(height: 8),
              Row(
                children: [
                  Icon(Icons.error_outline, color: scheme.error, size: 20),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      '接收失败：${banner.error ?? '未知错误'}',
                      key: ValueKey('incoming_error_$jobId'),
                      style: theme.textTheme.bodyMedium
                          ?.copyWith(color: scheme.error),
                    ),
                  ),
                ],
              ),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  key: ValueKey('incoming_error_dismiss_$jobId'),
                  onPressed: onDismiss,
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
