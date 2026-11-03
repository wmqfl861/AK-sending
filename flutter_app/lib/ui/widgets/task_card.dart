import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:flutter/material.dart';

import 'failure_advice.dart';

/// Chinese label for a [JobPhase] (task card chip).
String jobPhaseLabel(JobPhase phase) {
  switch (phase) {
    case JobPhase.idle:
      return '等待';
    case JobPhase.connecting:
      return '建立连接';
    case JobPhase.transferring:
      return '传输中';
    case JobPhase.verifying:
      return '校验中';
    case JobPhase.done:
      return '已完成';
    case JobPhase.failed:
      return '失败';
  }
}

Color _phaseColor(JobPhase phase, ColorScheme scheme) {
  switch (phase) {
    case JobPhase.idle:
      return scheme.outline;
    case JobPhase.connecting:
      return scheme.tertiary;
    case JobPhase.transferring:
      return scheme.primary;
    case JobPhase.verifying:
      return scheme.secondary;
    case JobPhase.done:
      return Colors.green;
    case JobPhase.failed:
      return scheme.error;
  }
}

/// Live task card driven by one [JobController].
///
/// Subscribes via [JobController.addListener] and re-renders on every
/// folded event: phase chip, the CLI-aligned `progressBarText` progress
/// line, per-file table, red error banner with actionable advice (UX
/// copy), and the final [TransferReport] summary (files / bytes /
/// duration / speed / roots).
class TaskCard extends StatefulWidget {
  const TaskCard({
    super.key,
    required this.controller,
    required this.title,
    this.subtitle,
    this.initiallyExpanded = true,
  });

  /// The controller folding this job's progress events.
  final JobController controller;

  /// Card header, e.g. `接收任务` / `发送任务`.
  final String title;

  /// Optional secondary line (peer, policy, ...).
  final String? subtitle;

  /// Whether the file table starts expanded.
  final bool initiallyExpanded;

  @override
  State<TaskCard> createState() => _TaskCardState();
}

class _TaskCardState extends State<TaskCard> {
  late bool _expanded;

  @override
  void initState() {
    super.initState();
    _expanded = widget.initiallyExpanded;
    widget.controller.addListener(_onChange);
  }

  @override
  void didUpdateWidget(TaskCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    // State objects survive widget swaps of the same runtimeType; follow
    // controller changes so the card always watches the live job.
    if (widget.controller != oldWidget.controller) {
      oldWidget.controller.removeListener(_onChange);
      widget.controller.addListener(_onChange);
      setState(() {});
    }
  }

  @override
  void dispose() {
    widget.controller.removeListener(_onChange);
    super.dispose();
  }

  void _onChange() {
    if (mounted) setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    final scheme = Theme.of(context).colorScheme;
    final files = controller.fileTable.values.toList()
      ..sort((a, b) {
        final byConn = a.conn.compareTo(b.conn);
        return byConn != 0 ? byConn : a.index.compareTo(b.index);
      });
    final error = controller.error;
    final report = controller.report;
    final phase = controller.phase;

    return Card(
      margin: const EdgeInsets.symmetric(vertical: 8),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(widget.title,
                          style: Theme.of(context).textTheme.titleMedium),
                      if (widget.subtitle != null)
                        Text(widget.subtitle!,
                            style: Theme.of(context).textTheme.bodySmall),
                    ],
                  ),
                ),
                if (controller.paused)
                  const Padding(
                    padding: EdgeInsets.only(right: 8),
                    child: Chip(
                      label: Text('已暂停'),
                      visualDensity: VisualDensity.compact,
                    ),
                  ),
                Container(
                  padding:
                      const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
                  decoration: BoxDecoration(
                    color: _phaseColor(phase, scheme).withValues(alpha: 0.15),
                    borderRadius: BorderRadius.circular(999),
                  ),
                  child: Text(
                    jobPhaseLabel(phase),
                    style: TextStyle(
                      color: _phaseColor(phase, scheme),
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            SelectableText(
              progressBarText(
                transferred: controller.ackedBytes,
                total: controller.expectedBytes ?? 0,
                width: 24,
              ),
              style: const TextStyle(fontFamily: 'monospace', fontSize: 13),
            ),
            const SizedBox(height: 4),
            Text(
              '文件 ${controller.committedFileCount}/'
              '${controller.expectedFiles ?? '?'} 已提交 · '
              '${humanBytes(controller.ackedBytes)}/'
              '${humanBytes(controller.expectedBytes ?? 0)}',
              style: Theme.of(context).textTheme.bodySmall,
            ),
            if (error != null) ...[
              const SizedBox(height: 12),
              _ErrorBanner(error: error),
            ],
            if (report != null) ...[
              const SizedBox(height: 12),
              _ReportPanel(report: report),
            ],
            if (files.isNotEmpty) ...[
              const SizedBox(height: 12),
              Row(
                children: [
                  Expanded(
                    child: Text('文件（${files.length}）',
                        style: Theme.of(context).textTheme.labelLarge),
                  ),
                  IconButton(
                    icon: Icon(_expanded
                        ? Icons.expand_less
                        : Icons.expand_more),
                    tooltip: _expanded ? '收起文件列表' : '展开文件列表',
                    onPressed: () => setState(() => _expanded = !_expanded),
                  ),
                ],
              ),
              if (_expanded)
                ...files.map((file) => _FileRow(file: file)),
            ],
          ],
        ),
      ),
    );
  }
}

/// Red banner: the raw error plus the UX-aligned action advice lines.
class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.error});

  final String error;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.error_outline, color: scheme.onErrorContainer),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  error,
                  style: TextStyle(
                      color: scheme.onErrorContainer,
                      fontWeight: FontWeight.w600),
                ),
              ),
            ],
          ),
          const SizedBox(height: 8),
          ...transferFailureAdvice(error).map(
            (line) => Padding(
              padding: const EdgeInsets.only(top: 2),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('· ', style: TextStyle(color: scheme.onErrorContainer)),
                  Expanded(
                    child: Text(line,
                        style:
                            TextStyle(color: scheme.onErrorContainer)),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Green summary of the terminal [TransferReport].
class _ReportPanel extends StatelessWidget {
  const _ReportPanel({required this.report});

  final TransferReport report;

  @override
  Widget build(BuildContext context) {
    final speed = report.speedText;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: Colors.green.withValues(alpha: 0.08),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              const Icon(Icons.check_circle_outline, color: Colors.green),
              const SizedBox(width: 8),
              Text('传输完成',
                  style: TextStyle(
                      color: Colors.green.shade800,
                      fontWeight: FontWeight.w600)),
            ],
          ),
          const SizedBox(height: 8),
          Text(
            '${report.files} 个文件 · ${report.bytesText} · '
            '用时 ${report.durationText}'
            '${speed == null ? '' : ' · $speed'}',
          ),
          if (report.roots.isNotEmpty)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text('目录：${report.roots.join('、')}',
                  style: Theme.of(context).textTheme.bodySmall),
            ),
        ],
      ),
    );
  }
}

/// One row of the per-file table: status icon, path, size/progress.
class _FileRow extends StatelessWidget {
  const _FileRow({required this.file});

  final FileProgress file;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final IconData icon;
    final Color color;
    final String state;
    if (file.committed) {
      icon = Icons.check_circle;
      color = Colors.green;
      state = '已提交';
    } else if (file.verified) {
      icon = Icons.verified_outlined;
      color = Colors.green;
      state = '已校验';
    } else {
      icon = Icons.circle_outlined;
      color = scheme.outline;
      state = '进行中';
    }
    final size = file.size;
    final acked = humanBytes(file.ackedBytes);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        children: [
          Icon(icon, size: 16, color: color),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              file.path ?? '文件 ${file.index}',
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
            ),
          ),
          const SizedBox(width: 8),
          Text(
            size == null ? acked : '$acked/${humanBytes(size)}',
            style: Theme.of(context).textTheme.bodySmall,
          ),
          const SizedBox(width: 8),
          Text(state, style: Theme.of(context).textTheme.bodySmall),
        ],
      ),
    );
  }
}
