/// Client UI U1 (2026-11-13): one transfer's live progress — a
/// determinate bar with transferred/total bytes and percent, the
/// throughput and time left, and the file in flight. Before any byte
/// count is known (the offer/consent phase, or an engine predating the
/// task-plane payload fields) the bar is indeterminate and the text says
/// what the transfer is waiting for, never a guessed percentage.
library;

import 'package:flutter/material.dart';

import '../service/simple_transfer_service.dart';
import '../service/transfer_format.dart';

class TransferProgressView extends StatelessWidget {
  const TransferProgressView({
    super.key,
    required this.progress,
    required this.role,
    required this.barKey,
    this.keyPrefix = 'transfer',
  });

  /// The latest snapshot of this transfer; null before the first one.
  final TransferProgress? progress;

  /// MY direction — picks the 发送/接收 wording.
  final TaskRole role;

  /// Key of the progress bar itself (the pre-U1 widget tests address the
  /// bar by `conn_send_progress` / `incoming_progress_<job>`).
  final Key barKey;

  /// Prefix of the text keys: `<prefix>_status`, `<prefix>_rate`,
  /// `<prefix>_file`.
  final String keyPrefix;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final snapshot = progress;
    final verb = role == TaskRole.send ? '发送' : '接收';
    final status = _statusText(snapshot, verb);
    final rate = _rateText(snapshot);
    final file = _fileText(snapshot, verb);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ClipRRect(
          borderRadius: BorderRadius.circular(4),
          child: LinearProgressIndicator(
            key: barKey,
            minHeight: 6,
            value: _barValue(snapshot),
          ),
        ),
        const SizedBox(height: 8),
        Text(
          status,
          key: ValueKey('${keyPrefix}_status'),
          style: theme.textTheme.bodyMedium
              ?.copyWith(fontWeight: FontWeight.w500),
        ),
        if (rate != null) ...[
          const SizedBox(height: 2),
          Text(
            rate,
            key: ValueKey('${keyPrefix}_rate'),
            style: theme.textTheme.bodySmall
                ?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ],
        if (file != null) ...[
          const SizedBox(height: 2),
          Text(
            file,
            key: ValueKey('${keyPrefix}_file'),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodySmall
                ?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ],
      ],
    );
  }

  /// Determinate only once bytes can move AND the total is known.
  static double? _barValue(TransferProgress? snapshot) {
    if (snapshot == null) {
      return null;
    }
    return switch (snapshot.phase) {
      TransferPhase.connecting || TransferPhase.awaitingConsent => null,
      TransferPhase.transferring ||
      TransferPhase.verifying ||
      TransferPhase.completed =>
        snapshot.fraction,
    };
  }

  static String _statusText(TransferProgress? snapshot, String verb) {
    if (snapshot == null) {
      // No snapshot yet (or an engine without progress facts): the
      // pre-U1 wording, so nothing claims a phase it cannot know.
      return '正在$verb…';
    }
    final sending = verb == '发送';
    return switch (snapshot.phase) {
      TransferPhase.connecting => sending ? '正在建立连接…' : '正在准备接收…',
      TransferPhase.awaitingConsent => '已发出请求，等待对方点「接受」…',
      TransferPhase.transferring => _bytesText(snapshot, verb),
      TransferPhase.verifying =>
        sending ? '数据已全部发出，等待对方校验…' : '正在校验文件完整性…',
      TransferPhase.completed => '$verb完成',
    };
  }

  static String _bytesText(TransferProgress snapshot, String verb) {
    final total = snapshot.totalBytes;
    final fraction = snapshot.fraction;
    if (total == null || fraction == null) {
      return '正在$verb…';
    }
    final percent = (fraction * 100).floor();
    return '已$verb ${formatBytes(snapshot.transferredBytes)} / '
        '${formatBytes(total)}（$percent%）';
  }

  static String? _rateText(TransferProgress? snapshot) {
    if (snapshot == null || snapshot.phase != TransferPhase.transferring) {
      return null;
    }
    final speed = snapshot.bytesPerSecond;
    if (speed == null || speed <= 0) {
      return null;
    }
    final remaining = snapshot.remaining;
    return remaining == null
        ? formatSpeed(speed)
        : '${formatSpeed(speed)} · 剩余${formatRemaining(remaining)}';
  }

  static String? _fileText(TransferProgress? snapshot, String verb) {
    if (snapshot == null || snapshot.phase != TransferPhase.transferring) {
      return null;
    }
    final name = snapshot.currentFileName;
    if (name == null || name.isEmpty) {
      return null;
    }
    final index = snapshot.currentFileIndex;
    final total = snapshot.totalFiles;
    final position =
        index != null && total != null && total > 1 ? '（${index + 1}/$total）' : '';
    return '正在$verb：$name$position';
  }
}
