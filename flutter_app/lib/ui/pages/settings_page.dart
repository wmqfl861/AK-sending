/// Wave-28 settings page: exactly ONE setting — the default save folder
/// (system directory picker, persisted across restarts via [SaveDirStore])
/// — plus the G2 (batchF 2026-11-13)「导出连接诊断」entry and the G6b
/// (batch G6, 2026-11-13)「关于」card: the registry server's build
/// revision (`RegistryClient::server_info`, shown as-is; null answers
/// render as 未知 — an old server never reports a version).
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
    this.isActive = true,
  });

  final SimpleTransferService service;
  final SaveDirStore store;
  final FilePickGateway picker;

  /// Where the diagnostics export lands (the temp-file gateway by
  /// default; tests inject a capturing fake).
  final DiagnosticsExportGateway exporter;

  /// Whether this page is the shell's SELECTED tab. The shell hosts it
  /// inside an [IndexedStack] (built at startup, visible or not), so the
  /// G6b revision query deliberately waits for the FIRST activation — a
  /// query nobody is looking at must not run (and must not put its
  /// `op_done` line into the shared task event queue) while another tab
  /// is up. Default true: a directly constructed page (tests, other
  /// hosts) behaves as a plain always-active page.
  final bool isActive;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  bool _busy = false;
  bool _exporting = false;

  /// G6b: the registry server's build revision (null = unknown — an old
  /// server without the revision-query capability, a failed query, or
  /// the runtime not connected; the service's contract is "never an
  /// error").
  String? _serverRevision;
  bool _revisionLoading = false;
  bool _revisionLoadedOnce = false;

  @override
  void initState() {
    super.initState();
    if (widget.isActive) {
      _loadRevision();
    }
  }

  @override
  void didUpdateWidget(covariant SettingsPage oldWidget) {
    super.didUpdateWidget(oldWidget);
    // G6b: first activation of a background-built tab — run the query
    // exactly once, when the user actually opens 设置.
    if (widget.isActive && !oldWidget.isActive && !_revisionLoadedOnce) {
      _loadRevision();
    }
  }

  /// G6b: fetch the registry revision once on entry (and on 刷新). The
  /// call never throws per the service contract; the finally still keeps
  /// the loading flag honest for a disposed page.
  Future<void> _loadRevision() async {
    if (_revisionLoading) return;
    _revisionLoadedOnce = true;
    setState(() => _revisionLoading = true);
    try {
      final revision = await widget.service.serverRevision();
      if (!mounted) return;
      setState(() => _serverRevision = revision);
    } finally {
      if (mounted) {
        setState(() => _revisionLoading = false);
      }
    }
  }

  /// What the「关于」card shows for the revision: the build string, the
  /// loading placeholder, or the honest unknown copy.
  String get _revisionDisplay {
    final revision = _serverRevision;
    if (revision != null && revision.isNotEmpty) {
      return revision;
    }
    return _revisionLoading ? '查询中…' : '未知（服务器未报告版本）';
  }

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
        const SizedBox(height: 16),
        // G6b (batch G6, 2026-11-13): the「关于」card — the registry
        // server's build revision, the ServerInfo fact G6 landed on the
        // wire (protocol-v2.md §6a). Read-only diagnostic: 未知 is the
        // honest answer of an old server (no capability bit — never
        // asked), a failed query, or an unconnected runtime; the query
        // itself never surfaces engine errors to this page.
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    Icon(Icons.info_outline, color: theme.colorScheme.primary),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text('关于', style: theme.textTheme.titleMedium),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                Text(
                  '注册服务器版本：连接协调服务器报告的构建版本，用于排查连接问题。',
                  style: theme.textTheme.bodySmall,
                ),
                const SizedBox(height: 12),
                Row(
                  children: [
                    Expanded(
                      child: Container(
                        width: double.infinity,
                        padding: const EdgeInsets.all(12),
                        decoration: BoxDecoration(
                          color: theme.colorScheme.surfaceContainerHighest,
                          borderRadius: BorderRadius.circular(8),
                        ),
                        child: Text(
                          _revisionDisplay,
                          key: const ValueKey('settings_server_revision'),
                          style: theme.textTheme.bodyMedium?.copyWith(
                            fontFamily: 'monospace',
                          ),
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    IconButton(
                      key: const ValueKey('settings_refresh_revision_button'),
                      onPressed: _revisionLoading ? null : _loadRevision,
                      icon: _revisionLoading
                          ? const SizedBox(
                              width: 16,
                              height: 16,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : const Icon(Icons.refresh),
                      tooltip: '重新查询服务器版本',
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
