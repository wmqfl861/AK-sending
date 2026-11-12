/// Wave-28 settings page: exactly ONE setting — the default save folder
/// (system directory picker, persisted across restarts via [SaveDirStore])
/// — plus the G2 (batchF 2026-11-13)「导出连接诊断」entry.
library;

import 'package:flutter/material.dart';

import '../../services/connection_diagnostics.dart';
import '../service/file_pick_gateway.dart';
import '../service/save_dir_store.dart';
import '../service/simple_transfer_service.dart';

class SettingsPage extends StatefulWidget {
  const SettingsPage({
    super.key,
    required this.service,
    required this.store,
    this.picker = const PluginFilePickGateway(),
    this.exporter = const TempFileDiagnosticsExportGateway(),
  });

  final SimpleTransferService service;
  final SaveDirStore store;
  final FilePickGateway picker;

  /// Where the diagnostics export lands (the temp-file gateway by
  /// default; tests inject a capturing fake).
  final DiagnosticsExportGateway exporter;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  bool _busy = false;
  bool _exporting = false;

  Future<void> _change() async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      final picked = await widget.picker.pickDirectory();
      if (picked == null || !mounted) {
        return;
      }
      await widget.service.setSaveDir(picked.path);
      await widget.store.save(picked.path);
      if (!mounted) return;
      setState(() {});
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(
          content: Text('默认保存文件夹已更新：${picked.path}'),
          duration: const Duration(seconds: 2),
        ),
      );
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// G2 (batchF 2026-11-13): collect the diagnostics ring buffer's recent
  /// lines + the platform summary into one JSON-Lines body and hand it to
  /// the export gateway. The exported file is the user's OWN diagnostic
  /// data and stays on their device (see connection_diagnostics.dart's
  /// red-line note); `share_plus` is not a dependency, so the flow is
  /// save-then-show-the-path.
  Future<void> _exportDiagnostics() async {
    if (_exporting) return;
    setState(() => _exporting = true);
    try {
      final body =
          buildDiagnosticsExport(ConnectionDiagnosticsLog.shared.lines);
      final path = await widget.exporter.exportDiagnostics(body);
      if (!mounted) return;
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(
          key: const ValueKey('settings_export_diag_done'),
          content: Text('连接诊断已导出：$path'),
          duration: const Duration(seconds: 4),
        ),
      );
    } on Object catch (error) {
      if (!mounted) return;
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(
          content: Text('连接诊断导出失败：$error'),
          duration: const Duration(seconds: 4),
        ),
      );
    } finally {
      if (mounted) {
        setState(() => _exporting = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      key: const ValueKey('settings_page'),
      padding: const EdgeInsets.all(16),
      children: [
        Text('设置', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    Icon(Icons.folder, color: theme.colorScheme.primary),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text('默认保存文件夹',
                          style: theme.textTheme.titleMedium),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                Text('接收的文件全部保存在这里。', style: theme.textTheme.bodySmall),
                const SizedBox(height: 12),
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(12),
                  decoration: BoxDecoration(
                    color: theme.colorScheme.surfaceContainerHighest,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Text(
                    widget.service.saveDir,
                    key: const ValueKey('settings_save_dir'),
                    style: theme.textTheme.bodyMedium?.copyWith(
                      fontFamily: 'monospace',
                    ),
                  ),
                ),
                const SizedBox(height: 12),
                FilledButton.tonalIcon(
                  key: const ValueKey('settings_change_dir_button'),
                  onPressed: _busy ? null : _change,
                  icon: _busy
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.drive_file_move_outline),
                  label: const Text('更改文件夹'),
                ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 16),
        // G2 (batchF 2026-11-13): one-tap diagnostics export — the recent
        // bind/probe/connect facts plus platform info land in ONE local
        // file (temp directory; the snackbar shows the full path). The
        // file is the user's own diagnostic data and never leaves the
        // device — the C4 "no real IPs in repo records" red line governs
        // repository documents, not this user-local export.
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    Icon(Icons.troubleshoot, color: theme.colorScheme.primary),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text('连接诊断',
                          style: theme.textTheme.titleMedium),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                Text(
                  '导出最近的连接建立过程（绑定、探测、连接地址）与平台信息，'
                  '保存为本地文件用于排查连接问题。文件仅保存在本机。',
                  style: theme.textTheme.bodySmall,
                ),
                const SizedBox(height: 12),
                FilledButton.tonalIcon(
                  key: const ValueKey('settings_export_diag_button'),
                  onPressed: _exporting ? null : _exportDiagnostics,
                  icon: _exporting
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.file_download_outlined),
                  label: const Text('导出连接诊断'),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
