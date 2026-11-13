/// Shared views/labels for [TaskConnection] rows (wave-28): the home-page
/// card and the detail page render direction and state through these.
library;

import 'package:flutter/material.dart';

import '../service/simple_transfer_service.dart';

/// Direction label from MY point of view (frozen one-way connection).
String taskRoleLabel(TaskRole role) =>
    role == TaskRole.send ? '我发送 → 对方' : '对方 → 我接收';

/// Short direction chip text.
String taskRoleShortLabel(TaskRole role) =>
    role == TaskRole.send ? '发送' : '接收';

/// Live-state label (list card + detail page).
String connectionStateLabel(TaskConnectionState state) => switch (state) {
      TaskConnectionState.connecting => '建立中',
      TaskConnectionState.testing => '测试中',
      TaskConnectionState.connected => '已连接',
      TaskConnectionState.unreachable => '不可达',
      TaskConnectionState.disconnected => '已断开',
    };

Color _connectionStateColor(
        TaskConnectionState state, ColorScheme scheme) => switch (state) {
      TaskConnectionState.connecting => scheme.tertiary,
      TaskConnectionState.testing => scheme.secondary,
      TaskConnectionState.connected => Colors.green,
      TaskConnectionState.unreachable => Colors.orange,
      TaskConnectionState.disconnected => scheme.error,
    };

/// Rounded state chip (label + tinted background).
class ConnectionStateChip extends StatelessWidget {
  const ConnectionStateChip({super.key, required this.state});

  final TaskConnectionState state;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = _connectionStateColor(state, scheme);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.15),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Text(
        connectionStateLabel(state),
        key: ValueKey('conn_state_${state.name}'),
        style: TextStyle(
          color: color,
          fontWeight: FontWeight.w600,
          fontSize: 12,
        ),
      ),
    );
  }
}

/// Connectivity help for a failed probe, not a confirmed NAT diagnosis.
class DirectUnavailableGuidance extends StatelessWidget {
  const DirectUnavailableGuidance({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          '连接帮助（尚未确认是否为以下网络限制）：',
          style: theme.textTheme.bodySmall,
        ),
        const SizedBox(height: 4),
        Text(
          '无法建立直连：两台设备所在的网络都不允许外部直接连入（例如运营商级 NAT）。'
          '可以让两台设备连接同一个 Wi-Fi，或让其中一台换用支持 IPv6 的网络。'
          '本应用不会通过服务器中转文件。',
          style: theme.textTheme.bodyMedium,
        ),
      ],
    );
  }
}

/// U1: the one-line progress caption of a connection card
/// (`发送中 27%`, `等待对方接受…`, `正在校验…`); null when there is
/// nothing running to report.
String? connectionProgressCaption(TaskRole role, TransferProgress? progress) {
  if (progress == null) {
    return null;
  }
  final verb = role == TaskRole.send ? '发送' : '接收';
  switch (progress.phase) {
    case TransferPhase.connecting:
      return '正在建立传输连接…';
    case TransferPhase.awaitingConsent:
      return '等待对方接受…';
    case TransferPhase.transferring:
      final fraction = progress.fraction;
      return fraction == null
          ? '$verb中…'
          : '$verb中 ${(fraction * 100).floor()}%';
    case TransferPhase.verifying:
      return '正在校验…';
    case TransferPhase.completed:
      return null;
  }
}

/// One live connection row of the home page's 「当前连接」 section:
/// peer name + direction + state. Tapping opens the detail page.
///
/// U1: a running transfer adds a thin progress bar and a caption.
class ConnectionCard extends StatelessWidget {
  const ConnectionCard({
    super.key,
    required this.connection,
    required this.onOpen,
    this.progress,
  });

  final TaskConnection connection;
  final VoidCallback onOpen;

  /// The connection's latest transfer progress (null = none running).
  final TransferProgress? progress;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final caption = connectionProgressCaption(connection.role, progress);
    final snapshot = progress;
    final barValue = snapshot != null &&
            (snapshot.phase == TransferPhase.transferring ||
                snapshot.phase == TransferPhase.verifying)
        ? snapshot.fraction
        : null;
    return Card(
      margin: const EdgeInsets.symmetric(vertical: 4),
      child: InkWell(
        borderRadius: BorderRadius.circular(12),
        onTap: onOpen,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
          child: Row(
            children: [
              Icon(
                connection.role == TaskRole.send
                    ? Icons.upload_outlined
                    : Icons.download_outlined,
                color: theme.colorScheme.primary,
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    // Phone-width guard: long peer names ellipsize instead
                    // of overflowing next to the state chip.
                    Text(
                      connection.peerName,
                      key: ValueKey('conn_card_name_${connection.id}'),
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleMedium,
                    ),
                    Text(
                      taskRoleLabel(connection.role),
                      key: ValueKey('conn_card_role_${connection.id}'),
                      style: theme.textTheme.bodySmall,
                    ),
                    if (caption != null) ...[
                      const SizedBox(height: 6),
                      ClipRRect(
                        borderRadius: BorderRadius.circular(2),
                        child: LinearProgressIndicator(
                          key: ValueKey('conn_card_progress_${connection.id}'),
                          minHeight: 3,
                          value: barValue,
                        ),
                      ),
                      const SizedBox(height: 4),
                      Text(
                        caption,
                        key: ValueKey('conn_card_caption_${connection.id}'),
                        style: theme.textTheme.labelSmall
                            ?.copyWith(color: theme.colorScheme.primary),
                      ),
                    ],
                  ],
                ),
              ),
              const SizedBox(width: 8),
              ConnectionStateChip(state: connection.state),
              const SizedBox(width: 4),
              Icon(Icons.chevron_right, color: theme.colorScheme.outline),
            ],
          ),
        ),
      ),
    );
  }
}
