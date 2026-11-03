import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show fingerprintShortId, humanBytes, isFingerprintHex;
import 'package:flutter/material.dart';

import '../service/transfer_service.dart';
import '../widgets/empty_state.dart';

/// History: lists past jobs from `service.listJobs()` (the checkpoint
/// store read). File bodies are managed separately from history — this
/// page only shows records.
class JobsPage extends StatefulWidget {
  const JobsPage({super.key, required this.service});

  final TransferService service;

  @override
  State<JobsPage> createState() => _JobsPageState();
}

class _JobsPageState extends State<JobsPage> {
  List<JobRecord>? _jobs;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    // No synchronous setState here: called from initState, i.e. during
    // the first build.
    try {
      final jobs = await widget.service.listJobs();
      if (mounted) setState(() => _jobs = jobs);
    } catch (error) {
      if (mounted) setState(() => _error = '读取历史失败：$error');
    }
  }

  String _two(int n) => n.toString().padLeft(2, '0');

  String _formatTime(DateTime t) =>
      '${t.year}-${_two(t.month)}-${_two(t.day)} '
      '${_two(t.hour)}:${_two(t.minute)}';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final jobs = _jobs;

    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Row(
          children: [
            Expanded(
              child: Text('历史任务', style: theme.textTheme.headlineSmall),
            ),
            IconButton(
              key: const ValueKey('jobs_refresh'),
              tooltip: '刷新',
              onPressed: _load,
              icon: const Icon(Icons.refresh),
            ),
          ],
        ),
        Text('历史记录与文件本体分开管理；取消任务的临时分片按检查点策略清理。',
            style: theme.textTheme.bodySmall),
        const SizedBox(height: 16),
        if (_error != null)
          Card(
            color: theme.colorScheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Text(_error!,
                  style:
                      TextStyle(color: theme.colorScheme.onErrorContainer)),
            ),
          )
        else if (jobs == null)
          const Padding(
            key: ValueKey('jobs_loading'),
            padding: EdgeInsets.all(32),
            child: Center(child: CircularProgressIndicator()),
          )
        else if (jobs.isEmpty)
          const EmptyState(
            icon: Icons.history_toggle_off,
            title: '还没有历史任务',
            message: '完成一次发送或接收后，任务会出现在这里（来自任务检查点记录）。',
          )
        else
          ...jobs.map((job) => _JobTile(
                job: job,
                formatTime: _formatTime,
              )),
      ],
    );
  }
}

class _JobTile extends StatelessWidget {
  const _JobTile({required this.job, required this.formatTime});

  final JobRecord job;
  final String Function(DateTime) formatTime;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final (stateLabel, stateColor) = switch (job.state) {
      JobRecordState.running => ('运行中', scheme.primary),
      JobRecordState.done => ('已完成', Colors.green),
      JobRecordState.failed => ('失败', scheme.error),
    };
    final isSend = job.direction == JobDirection.send;
    return Card(
      margin: const EdgeInsets.symmetric(vertical: 6),
      child: ListTile(
        leading: Icon(
          isSend ? Icons.upload_outlined : Icons.download_outlined,
          color: scheme.primary,
        ),
        title: Row(
          children: [
            Text(isSend ? '发送 · ${job.id}' : '接收 · ${job.id}',
                style: const TextStyle(fontFamily: 'monospace', fontSize: 14)),
            const SizedBox(width: 8),
            Container(
              padding:
                  const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
              decoration: BoxDecoration(
                color: stateColor.withValues(alpha: 0.15),
                borderRadius: BorderRadius.circular(999),
              ),
              child: Text(stateLabel,
                  style: TextStyle(
                      color: stateColor,
                      fontSize: 12,
                      fontWeight: FontWeight.w600)),
            ),
          ],
        ),
        subtitle: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '${formatTime(job.startedAt)} · '
              '${job.files} 个文件 · ${humanBytes(job.bytes)}',
            ),
            if (_peerLine(job).isNotEmpty)
              Text(
                _peerLine(job),
                key: const ValueKey('job_peer_line'),
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
              ),
            if (job.summary.isNotEmpty) Text(job.summary),
            if (job.error != null)
              Text(
                job.error!,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(color: scheme.error),
              ),
          ],
        ),
      ),
    );
  }

  /// `对端 <短式指纹> · <root>` from the record's peer/root fields
  /// (engine jobsList rows); empty when the record carries neither.
  static String _peerLine(JobRecord job) {
    final peer = job.peer;
    final root = job.root;
    final buffer = StringBuffer();
    if (peer != null && peer.isNotEmpty) {
      buffer.write('对端 ');
      buffer.write(
          isFingerprintHex(peer) ? fingerprintShortId(peer) : peer);
    }
    if (root != null && root.isNotEmpty) {
      if (buffer.isNotEmpty) buffer.write(' · ');
      buffer.write(root);
    }
    return buffer.toString();
  }
}
