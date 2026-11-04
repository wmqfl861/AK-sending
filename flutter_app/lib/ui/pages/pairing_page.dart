import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show fingerprintShortId, formatPairingCode;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../service/transfer_service.dart';
import '../widgets/empty_state.dart';
import '../widgets/fingerprint_chip.dart';

/// Pairing management (wave-14): the sixth rail tab.
///
/// * Top card — this device's identity from the service's cached
///   [DeviceInfo] (refreshed via [TransferService.refreshIdentity] on
///   entry): device name, the expandable two-line fingerprint and the
///   large pairing code, mirroring the HomePage identity card.
/// * Pairing book (`peers.book`) — the paired-peer list (short id with
///   tap-to-expand full fingerprint, stored name, pairing time; an
///   empty state with pairing guidance), an add form (full 64-hex
///   fingerprint REQUIRED — an 8–63 prefix is rejected with a
///   "paste the full fingerprint" hint; name optional, blank adds
///   "(unnamed)") and removal behind a confirmation dialog.
/// * A corrupt book ([TransferService.listPeers] throwing) renders as an
///   inline error row — never as a silently empty list; after add/remove
///   the list reloads AND the identity refreshes so the HomePage
///   pairing-book count follows.
class PairingPage extends StatefulWidget {
  const PairingPage({super.key, required this.service});

  final TransferService service;

  @override
  State<PairingPage> createState() => _PairingPageState();
}

class _PairingPageState extends State<PairingPage> {
  final TextEditingController _fingerprintInput = TextEditingController();
  final TextEditingController _nameInput = TextEditingController();

  /// Null while the first listPeers() is in flight.
  List<PeerEntryUi>? _peers;

  /// Inline reason when listPeers() threw (corrupt book / engine down).
  String? _listError;

  /// Inline error of the add form.
  String? _formError;

  /// Inline error of the latest remove action (e.g. the not-found race).
  String? _actionError;

  bool _adding = false;

  @override
  void initState() {
    super.initState();
    widget.service.addStatusListener(_onStatus);
    // Identity card renders the cached snapshot first, then re-renders
    // once the refresh lands (same pattern as HomePage).
    _refreshIdentity();
    _loadPeers();
  }

  @override
  void dispose() {
    widget.service.removeStatusListener(_onStatus);
    _fingerprintInput.dispose();
    _nameInput.dispose();
    super.dispose();
  }

  void _onStatus() {
    if (mounted) setState(() {});
  }

  Future<void> _refreshIdentity() async {
    await widget.service.refreshIdentity();
    if (mounted) setState(() {});
  }

  Future<void> _loadPeers() async {
    try {
      final peers = await widget.service.listPeers();
      if (mounted) {
        setState(() {
          _peers = peers;
          _listError = null;
        });
      }
    } on Object catch (error) {
      // Corrupt peers.book or an unusable engine: show the reason inline
      // instead of pretending the book were empty.
      if (mounted) {
        setState(() {
          _listError = '配对簿读取失败：$error';
          _peers = null;
        });
      }
    }
  }

  /// Reloads the list and refreshes the identity — the HomePage
  /// "已配对设备" count follows every book mutation.
  Future<void> _reload() async {
    await _loadPeers();
    await _refreshIdentity();
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

  /// True for a non-empty run of ASCII hex characters (any length).
  static bool _isHexRun(String text) =>
      text.isNotEmpty && RegExp(r'^[0-9a-fA-F]+$').hasMatch(text);

  Future<void> _add() async {
    setState(() {
      _formError = null;
      _actionError = null;
    });
    final text = _fingerprintInput.text.trim();
    if (text.isEmpty) {
      setState(() => _formError = '请输入对端指纹（64 位十六进制）。');
      return;
    }
    final hexRun = _isHexRun(text);
    if (!hexRun || text.length != 64) {
      if (hexRun && text.length >= 8 && text.length < 64) {
        setState(
            () => _formError = '指纹前缀不能用于添加：请粘贴完整 64 位指纹。');
      } else {
        setState(() => _formError = '指纹须为 64 位十六进制字符。');
      }
      return;
    }
    setState(() => _adding = true);
    try {
      await widget.service.addPeer(text, _nameInput.text);
      _fingerprintInput.clear();
      _nameInput.clear();
      if (mounted) setState(() => _formError = null);
      await _reload();
      if (mounted) {
        ScaffoldMessenger.maybeOf(context)?.showSnackBar(
          const SnackBar(
              content: Text('已添加配对设备'),
              duration: Duration(seconds: 2)),
        );
      }
    } on Object catch (error) {
      if (mounted) setState(() => _formError = '添加失败：$error');
    } finally {
      if (mounted) setState(() => _adding = false);
    }
  }

  Future<void> _confirmRemove(PeerEntryUi peer) async {
    setState(() => _actionError = null);
    final displayName =
        peer.name.isEmpty || peer.name == '(unnamed)' ? '未命名设备' : peer.name;
    final shortId = _shortIdOf(peer.fingerprint);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('移除配对设备'),
        content: Text(
          '确定移除 $displayName'
          '${shortId == null ? '' : '（指纹 $shortId）'}吗？'
          '移除后，对方将无法再通过配对簿准入连接本机。',
        ),
        actions: [
          TextButton(
            key: const ValueKey('pairing_remove_cancel'),
            onPressed: () => Navigator.pop(dialogContext, false),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const ValueKey('pairing_remove_confirm'),
            onPressed: () => Navigator.pop(dialogContext, true),
            child: const Text('移除'),
          ),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    try {
      final removed = await widget.service.removePeer(peer.fingerprint);
      if (!removed && mounted) {
        setState(() => _actionError = '该设备已不在配对簿中（可能已被其他窗口移除）。');
      }
      await _reload();
    } on Object catch (error) {
      if (mounted) setState(() => _actionError = '移除失败：$error');
    }
  }

  static String? _shortIdOf(String fingerprintHex) {
    try {
      return fingerprintShortId(fingerprintHex);
    } on FormatException {
      return null;
    }
  }

  String _two(int n) => n.toString().padLeft(2, '0');

  String _formatTime(DateTime t) =>
      '${t.year}-${_two(t.month)}-${_two(t.day)} '
      '${_two(t.hour)}:${_two(t.minute)}';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final info = widget.service.deviceInfo();
    final pairingCode = _normalizePairingCode(info.pairingCode);
    final peers = _peers;
    final listError = _listError;

    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('配对', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        Text('管理配对簿（peers.book）：本机身份、已配对设备、添加与移除。',
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
                      child: Icon(Icons.fingerprint,
                          color: scheme.onPrimaryContainer),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(info.deviceName,
                              style: theme.textTheme.titleLarge),
                          Text('配对码与指纹告知对端，完成一次互信配对',
                              style: theme.textTheme.bodySmall),
                        ],
                      ),
                    ),
                  ],
                ),
                const Divider(height: 32),
                Text('本机指纹', style: theme.textTheme.labelLarge),
                FingerprintChip(fingerprintHex: info.fingerprintHex),
                const SizedBox(height: 16),
                Text('配对码（告知发送方，或让其扫码）',
                    style: theme.textTheme.labelLarge),
                const SizedBox(height: 4),
                Row(
                  children: [
                    SelectableText(
                      pairingCode,
                      key: const ValueKey('pairing_pairing_code'),
                      style: theme.textTheme.displaySmall
                          ?.copyWith(fontWeight: FontWeight.w700),
                    ),
                    const SizedBox(width: 12),
                    IconButton.outlined(
                      icon: const Icon(Icons.copy),
                      tooltip: '复制配对码',
                      onPressed: pairingCode.isEmpty
                          ? null
                          : () => _copyPairingCode(pairingCode),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                Text(
                  '对端在"发送"页输入此配对码或完整指纹即可配对；'
                  '发送成功后会自动记入本机配对簿（TOFU）。',
                  style: theme.textTheme.bodySmall,
                ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('添加配对设备', style: theme.textTheme.titleMedium),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('pairing_add_fp'),
                  controller: _fingerprintInput,
                  enabled: !_adding,
                  decoration: const InputDecoration(
                    labelText: '对端指纹（完整 64 位十六进制）',
                    helperText: '前缀不能用于添加；8–63 位前缀请补全为完整指纹。',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('pairing_add_name'),
                  controller: _nameInput,
                  enabled: !_adding,
                  decoration: const InputDecoration(
                    labelText: '名称（可选）',
                    helperText: '留空时按未命名设备记录。',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 12),
                FilledButton.icon(
                  key: const ValueKey('pairing_add_button'),
                  icon: const Icon(Icons.person_add_alt),
                  label: const Text('添加配对'),
                  onPressed: _adding ? null : _add,
                ),
                if (_formError != null)
                  Padding(
                    key: const ValueKey('pairing_form_error'),
                    padding: const EdgeInsets.only(top: 12),
                    child: Text(
                      _formError!,
                      style: TextStyle(color: scheme.error),
                    ),
                  ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 16),
        Row(
          children: [
            Expanded(
              child: Text(
                '已配对设备'
                '${peers == null ? '' : '（${peers.length} 台）'}',
                key: const ValueKey('pairing_peers_header'),
                style: theme.textTheme.titleMedium,
              ),
            ),
            IconButton(
              key: const ValueKey('pairing_refresh'),
              tooltip: '刷新',
              onPressed: _reload,
              icon: const Icon(Icons.refresh),
            ),
          ],
        ),
        if (_actionError != null)
          Padding(
            key: const ValueKey('pairing_action_error'),
            padding: const EdgeInsets.only(top: 4, bottom: 4),
            child: Text(
              _actionError!,
              style: TextStyle(color: scheme.error),
            ),
          ),
        if (listError != null)
          Card(
            key: const ValueKey('pairing_list_error'),
            color: scheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Text(
                listError,
                style: TextStyle(color: scheme.onErrorContainer),
              ),
            ),
          )
        else if (peers == null)
          const Padding(
            key: ValueKey('pairing_loading'),
            padding: EdgeInsets.all(32),
            child: Center(child: CircularProgressIndicator()),
          )
        else if (peers.isEmpty)
          const EmptyState(
            icon: Icons.devices_other,
            title: '还没有配对设备',
            message: '对端在"发送"页输入本机指纹或配对码完成一次传输后，'
                '会自动记入配对簿（TOFU）；也可以在上方手动粘贴对端的'
                '完整 64 位指纹添加。',
          )
        else
          ...peers.map(
            (peer) => _PeerTile(
              peer: peer,
              formatTime: _formatTime,
              onRemove: () => _confirmRemove(peer),
            ),
          ),
      ],
    );
  }
}

/// One paired-peer row: stored name (未命名设备 fallback), pairing time,
/// the expandable fingerprint and the remove button.
class _PeerTile extends StatelessWidget {
  const _PeerTile({
    required this.peer,
    required this.formatTime,
    required this.onRemove,
  });

  final PeerEntryUi peer;
  final String Function(DateTime) formatTime;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final hasName = peer.name.isNotEmpty && peer.name != '(unnamed)';
    return Card(
      margin: const EdgeInsets.symmetric(vertical: 6),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(Icons.devices_other, size: 20, color: scheme.primary),
                const SizedBox(width: 10),
                Expanded(
                  child: Text(
                    hasName ? peer.name : '未命名设备',
                    key: ValueKey('pairing_peer_name_${peer.fingerprint}'),
                    style: theme.textTheme.titleSmall,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
                Text('配对于 ${formatTime(peer.pairedAt)}',
                    key: ValueKey('pairing_peer_time_${peer.fingerprint}'),
                    style: theme.textTheme.bodySmall),
                const SizedBox(width: 4),
                IconButton(
                  key: ValueKey('pairing_remove_${peer.fingerprint}'),
                  icon: const Icon(Icons.delete_outline),
                  tooltip: '移除配对',
                  onPressed: onRemove,
                ),
              ],
            ),
            FingerprintChip(fingerprintHex: peer.fingerprint),
          ],
        ),
      ),
    );
  }
}

/// Canonical `123-456` display; falls back to the raw text (empty on the
/// degraded identity) when the service-provided code does not match.
String _normalizePairingCode(String code) {
  try {
    return formatPairingCode(code);
  } on FormatException {
    return code;
  }
}
