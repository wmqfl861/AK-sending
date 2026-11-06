/// Wave-28 settings page: exactly ONE setting — the default save folder
/// (system directory picker, persisted across restarts via [SaveDirStore]).
library;

import 'package:flutter/material.dart';

import '../service/file_pick_gateway.dart';
import '../service/save_dir_store.dart';
import '../service/simple_transfer_service.dart';

class SettingsPage extends StatefulWidget {
  const SettingsPage({
    super.key,
    required this.service,
    required this.store,
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;
  final SaveDirStore store;
  final FilePickGateway picker;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  bool _busy = false;

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
      ],
    );
  }
}
