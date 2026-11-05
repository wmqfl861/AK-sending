import 'package:ak_ffi_dart/ak_ffi_dart.dart' show formatPairingCode;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../service/transfer_service.dart';
import '../widgets/fingerprint_chip.dart';

/// Status dot color for each [TransferServiceStatus].
Color statusDotColor(TransferServiceStatus status, ColorScheme scheme) {
  switch (status) {
    case TransferServiceStatus.idle:
      return scheme.outline;
    case TransferServiceStatus.listening:
      return Colors.green;
    case TransferServiceStatus.busy:
      return scheme.primary;
    case TransferServiceStatus.error:
      return scheme.error;
  }
}

/// Status dot label for each [TransferServiceStatus].
String statusLabel(TransferServiceStatus status) {
  switch (status) {
    case TransferServiceStatus.idle:
      return '就绪';
    case TransferServiceStatus.listening:
      return '监听中';
    case TransferServiceStatus.busy:
      return '传输中';
    case TransferServiceStatus.error:
      return '服务异常';
  }
}

/// Home: this device's identity card (device id, two-line fingerprint,
/// large pairing code with copy), the live service status dot, and quick
/// entries into Receive / Send.
class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.service, this.onGoTab});

  final TransferService service;

  /// Jump to a shell tab index (0..5; 4 = the Pairing tab); wired by the
  /// NavigationRail shell.
  final void Function(int index)? onGoTab;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  @override
  void initState() {
    super.initState();
    widget.service.addStatusListener(_onStatus);
    // Real identity read at startup (engine.identityInfo through the
    // service); the card first renders the cached/degraded snapshot and
    // re-renders once this lands.
    _refreshIdentity();
  }

  @override
  void dispose() {
    widget.service.removeStatusListener(_onStatus);
    super.dispose();
  }

  void _onStatus() {
    if (mounted) setState(() {});
  }

  Future<void> _refreshIdentity() async {
    await widget.service.refreshIdentity();
    if (mounted) setState(() {});
  }

  Future<void> _copyPairingCode(String code) async {
    await Clipboard.setData(ClipboardData(text: code));
    if (mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(
            content: Text('已复制配对码 $code'),
            duration: const Duration(seconds: 2)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final info = widget.service.deviceInfo();
    final status = widget.service.status;
    final pairingCode = _normalizePairingCode(info.pairingCode);

    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('主页', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        Text('设备直连传输 · 无中继、无云端暂存',
            style: theme.textTheme.bodySmall),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    CircleAvatar(
                      backgroundColor: scheme.primaryContainer,
                      child: Icon(Icons.computer,
                          color: scheme.onPrimaryContainer),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(info.deviceName,
                              style: theme.textTheme.titleLarge),
                          Text('设备 ID：${_deviceId(info)}',
                              style: theme.textTheme.bodySmall?.copyWith(
                                  fontFamily: 'monospace')),
                        ],
                      ),
                    ),
                    Tooltip(
                      message: statusLabel(status),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Container(
                            width: 10,
                            height: 10,
                            decoration: BoxDecoration(
                              color: statusDotColor(status, scheme),
                              shape: BoxShape.circle,
                            ),
                          ),
                          const SizedBox(width: 6),
                          Text(statusLabel(status)),
                        ],
                      ),
                    ),
                  ],
                ),
                const Divider(height: 32),
                if (info.identityCreated)
                  Padding(
                    key: const ValueKey('home_identity_created'),
                    padding: const EdgeInsets.only(bottom: 12),
                    child: Row(
                      children: [
                        Icon(Icons.auto_awesome,
                            size: 18, color: scheme.primary),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Text(
                            '首次已创建身份：本机已生成新的设备身份并写入身份目录。',
                            style: theme.textTheme.bodySmall
                                ?.copyWith(color: scheme.primary),
                          ),
                        ),
                      ],
                    ),
                  ),
                Text('本机指纹', style: theme.textTheme.labelLarge),
                FingerprintChip(fingerprintHex: info.fingerprintHex),
                const SizedBox(height: 16),
                Text('配对码（告知发送方，或让其扫码）',
                    style: theme.textTheme.labelLarge),
                const SizedBox(height: 4),
                Row(
                  children: [
                    // Flexible: at phone widths (~294dp inside the card)
                    // displaySmall + the copy button measured 21px past
                    // the viewport; the code keeps its size whenever it
                    // fits and only shrinks (with FittedBox scaling, so
                    // the digits stay legible rather than ellipsizing a
                    // 7-char code) on narrow surfaces / large text scale.
                    Flexible(
                      child: FittedBox(
                        fit: BoxFit.scaleDown,
                        child: SelectableText(
                          pairingCode,
                          key: const ValueKey('home_pairing_code'),
                          style: theme.textTheme.displaySmall
                              ?.copyWith(fontWeight: FontWeight.w700),
                        ),
                      ),
                    ),
                    const SizedBox(width: 12),
                    IconButton.outlined(
                      icon: const Icon(Icons.copy),
                      tooltip: '复制配对码',
                      onPressed: () => _copyPairingCode(pairingCode),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                // Wave-14 linkage: tapping the pairing-book row jumps to
                // the Pairing tab (index 4) where the book is managed.
                InkWell(
                  borderRadius: BorderRadius.circular(8),
                  onTap: () => widget.onGoTab?.call(4),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 4),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Icon(Icons.how_to_reg,
                            size: 16, color: scheme.outline),
                        const SizedBox(width: 6),
                        // Flexible + ellipsis: on phone widths this row
                        // measured 34px past the viewport (real-device
                        // wave-25 finding); the label shrinks and the
                        // trailing "（配对簿 peers.book）" ellipsizes
                        // instead of overflowing. Desktop widths render
                        // the full label unchanged.
                        Flexible(
                          child: Text(
                            '已配对设备：${info.peersCount} 台（配对簿 peers.book）',
                            key: const ValueKey('home_peers_count'),
                            style: theme.textTheme.bodySmall,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                        const SizedBox(width: 4),
                        Icon(Icons.chevron_right,
                            size: 16, color: scheme.outline),
                      ],
                    ),
                  ),
                ),
                const SizedBox(height: 8),
                Text(
                  '对端可在发送页输入此配对码完成配对；'
                  '本机接收时的准入策略在"接收"页设置。',
                  style: theme.textTheme.bodySmall,
                ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 16),
        Row(
          children: [
            Expanded(
              child: FilledButton.icon(
                icon: const Icon(Icons.download),
                label: const Text('接收文件'),
                onPressed: () => widget.onGoTab?.call(1),
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: FilledButton.tonalIcon(
                icon: const Icon(Icons.upload),
                label: const Text('发送文件'),
                onPressed: () => widget.onGoTab?.call(2),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// Short display id: first 8 chars of the (lowercased) fingerprint.
String _deviceId(DeviceInfo info) {
  final hex = info.fingerprintHex.toLowerCase();
  return hex.length >= 8 ? hex.substring(0, 8) : hex;
}

/// Canonical `123-456` display; falls back to the raw text when the
/// service-provided code does not match the expected shape.
String _normalizePairingCode(String code) {
  try {
    return formatPairingCode(code);
  } on FormatException {
    return code;
  }
}
