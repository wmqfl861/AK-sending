import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show JobController, isFingerprintHex;
import 'package:flutter/material.dart';

import '../service/transfer_service.dart';
import '../widgets/task_card.dart';

final RegExp _hexPrefix = RegExp(r'^[0-9a-fA-F]{8,63}$');
final RegExp _ipv4Port =
    RegExp(r'^(\d{1,3}\.){3}\d{1,3}:\d{1,5}$');
final RegExp _v6Port = RegExp(r'^\[[0-9A-Fa-f:.%]+\]:\d{1,5}$');

/// Send: multi-path list (add/remove), peer address, fingerprint (full
/// 64-hex or a unique prefix resolved via the pairing book by the
/// service), multistream / no-internet switches, conflict policy; the
/// live task card folds the send JobController and shows the final
/// TransferReport (speed / duration / roots).
class SendPage extends StatefulWidget {
  const SendPage({super.key, required this.service});

  final TransferService service;

  @override
  State<SendPage> createState() => _SendPageState();
}

class _SendPageState extends State<SendPage> {
  final TextEditingController _pathInput = TextEditingController();
  final TextEditingController _peer = TextEditingController();
  final TextEditingController _fingerprint = TextEditingController();

  final List<String> _roots = [];
  bool _multistream = false;
  bool _noInternet = false;
  String _conflict = 'fail';
  JobController? _session;
  String? _formError;

  @override
  void dispose() {
    _pathInput.dispose();
    _peer.dispose();
    _fingerprint.dispose();
    super.dispose();
  }

  void _addRoot() {
    final path = _pathInput.text.trim();
    if (path.isEmpty) {
      setState(() => _formError = '路径不能为空。');
      return;
    }
    setState(() {
      _roots.add(path);
      _pathInput.clear();
      _formError = null;
    });
  }

  void _removeRoot(int index) {
    setState(() => _roots.removeAt(index));
  }

  /// null = invalid; true = full fingerprint; false = prefix.
  bool? _fingerprintStatus(String text) {
    if (isFingerprintHex(text)) return true;
    if (_hexPrefix.hasMatch(text)) return false;
    return null;
  }

  Future<void> _start() async {
    setState(() => _formError = null);
    if (_roots.isEmpty) {
      setState(() => _formError = '请至少添加一个要发送的文件或目录路径。');
      return;
    }
    final peer = _peer.text.trim();
    if (!_ipv4Port.hasMatch(peer) && !_v6Port.hasMatch(peer)) {
      setState(() => _formError = '对端地址须为 ip:端口（如 192.168.1.5:52341 或 [::1]:7100）。');
      return;
    }
    final fp = _fingerprint.text.trim();
    if (_fingerprintStatus(fp) == null) {
      setState(() => _formError = '指纹须为 64 位十六进制，或 8–63 位的唯一前缀。');
      return;
    }
    try {
      final session = widget.service.startSend(SendOptions(
        sourceRoots: List.of(_roots),
        peer: peer,
        fingerprint: fp,
        multistream: _multistream,
        noInternet: _noInternet,
        conflict: _conflict,
      ));
      setState(() => _session = session);
    } catch (error) {
      setState(() => _formError = '启动失败：$error');
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final fpText = _fingerprint.text.trim();
    final fpStatus = fpText.isEmpty ? null : _fingerprintStatus(fpText);

    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('发送', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('发送内容', style: theme.textTheme.titleMedium),
                const SizedBox(height: 12),
                Row(
                  children: [
                    Expanded(
                      child: TextField(
                        key: const ValueKey('send_path_input'),
                        controller: _pathInput,
                        onSubmitted: (_) => _addRoot(),
                        decoration: const InputDecoration(
                          labelText: '文件或目录的完整路径',
                          hintText: r'例如 C:\Users\me\Photos',
                          border: OutlineInputBorder(),
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    OutlinedButton(
                      key: const ValueKey('send_path_add'),
                      onPressed: _addRoot,
                      child: const Text('添加'),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                for (final (index, root) in _roots.indexed)
                  ListTile(
                    key: ValueKey('send_root_$index'),
                    dense: true,
                    leading: const Icon(Icons.folder_open),
                    title: Text(root,
                        style: const TextStyle(fontFamily: 'monospace')),
                    trailing: IconButton(
                      icon: const Icon(Icons.delete_outline),
                      tooltip: '移除此路径',
                      onPressed: () => _removeRoot(index),
                    ),
                  ),
                if (_roots.isEmpty)
                  const Text('尚未添加路径；多个路径同时发送时，若存在同名文件需在下方选择处理方式。'),
                const Divider(height: 32),
                Text('对端', style: theme.textTheme.titleMedium),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('send_peer'),
                  controller: _peer,
                  decoration: const InputDecoration(
                    labelText: '对端地址（ip:端口）',
                    hintText: '例如 192.168.1.5:52341',
                    helperText: '对方在"接收"页开始监听后会显示此地址。',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('send_fingerprint'),
                  controller: _fingerprint,
                  // The helper text (full vs prefix) derives from the text;
                  // rebuild on each keystroke so the hint follows input.
                  onChanged: (_) => setState(() {}),
                  decoration: InputDecoration(
                    labelText: '对端指纹（完整 64 位或唯一前缀）',
                    helperText: switch (fpStatus) {
                      null => fpText.isEmpty ? '粘贴完整指纹，或 8–63 位十六进制前缀。' : null,
                      true => '已输入完整指纹。',
                      false => '已输入前缀：将用本机配对簿查补全（须唯一匹配，否则请补齐或改用完整指纹）。',
                    },
                    helperStyle: const TextStyle(fontFamily: 'monospace', fontSize: 12),
                    border: const OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 8),
                SwitchListTile(
                  key: const ValueKey('send_multistream'),
                  value: _multistream,
                  onChanged: (v) => setState(() => _multistream = v),
                  title: const Text('多流并发'),
                  subtitle: const Text('对端支持时并行传输多个文件（multistream）。'),
                ),
                SwitchListTile(
                  key: const ValueKey('send_no_internet'),
                  value: _noInternet,
                  onChanged: (v) => setState(() => _noInternet = v),
                  title: const Text('仅限局域网'),
                  subtitle: const Text('禁止经公网直连发送；回环与局域网不受影响。'),
                ),
                const SizedBox(height: 8),
                Text('重名处理', style: theme.textTheme.labelLarge),
                const SizedBox(height: 4),
                SegmentedButton<String>(
                  segments: const [
                    ButtonSegment(value: 'fail', label: Text('重名报错')),
                    ButtonSegment(value: 'rename', label: Text('自动重命名')),
                  ],
                  selected: {_conflict},
                  onSelectionChanged: (s) => setState(() => _conflict = s.first),
                ),
                if (_formError != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 12),
                    child: Text(
                      _formError!,
                      style: TextStyle(color: theme.colorScheme.error),
                    ),
                  ),
                const SizedBox(height: 16),
                FilledButton.icon(
                  key: const ValueKey('send_start'),
                  onPressed: _start,
                  icon: const Icon(Icons.send),
                  label: const Text('开始发送'),
                ),
              ],
            ),
          ),
        ),
        if (_session != null)
          TaskCard(
            key: const ValueKey('send_task_card'),
            controller: _session!,
            title: '发送任务',
            subtitle: '对端：${_peer.text}',
          ),
      ],
    );
  }
}
