import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AcceptAnyPolicy, AcceptPolicy, JobController;
import 'package:flutter/material.dart';

import '../service/transfer_service.dart';
import '../widgets/task_card.dart';

/// Receive: output directory (text input, remembered in page state),
/// port, one of the three admission forms (pairing book default /
/// any with warning-confirmation dialog / pinned fingerprint or pairing
/// code), start/stop; while listening the banner shows address +
/// fingerprint + pairing code for the sending side, and the live task
/// card folds the listener's JobController.
class ReceivePage extends StatefulWidget {
  const ReceivePage({super.key, required this.service});

  final TransferService service;

  @override
  State<ReceivePage> createState() => _ReceivePageState();
}

class _ReceivePageState extends State<ReceivePage> {
  final TextEditingController _outDir = TextEditingController();
  final TextEditingController _port = TextEditingController(text: '0');
  final TextEditingController _expect = TextEditingController();
  final List<JobController> _history = [];

  AdmissionMode _admission = AdmissionMode.book;
  String _conflict = 'fail';
  JobController? _session;
  String? _formError;

  @override
  void dispose() {
    _session?.removeListener(_onSessionEvent);
    _outDir.dispose();
    _port.dispose();
    _expect.dispose();
    super.dispose();
  }

  /// Re-render on every folded session event so the listening banner
  /// (bound address) stays live; the task card re-renders itself.
  void _onSessionEvent() {
    if (mounted) setState(() {});
  }

  /// Swaps the watched session, moving the listener from the old one.
  void _watch(JobController? session) {
    _session?.removeListener(_onSessionEvent);
    session?.addListener(_onSessionEvent);
    _session = session;
  }

  bool get _isListening => _session != null && widget.service.isListening;

  /// Resolves the admission form into the policy passed to the service.
  /// Returns a null policy with `error` set on invalid input;
  /// `needsWarning` marks the risky `any` form.
  ({AcceptPolicy? policy, bool needsWarning, String? error}) _resolvePolicy() {
    switch (_admission) {
      case AdmissionMode.book:
        return (policy: null, needsWarning: false, error: null);
      case AdmissionMode.any:
        return (
          policy: const AcceptAnyPolicy(),
          needsWarning: true,
          error: null,
        );
      case AdmissionMode.pinned:
        final text = _expect.text.trim();
        if (text.isEmpty) {
          return (
            policy: null,
            needsWarning: false,
            error: '请输入对端指纹或 6 位配对码。',
          );
        }
        final policy = AcceptPolicy.tryParse(text);
        if (policy == null) {
          return (
            policy: null,
            needsWarning: false,
            error: '需为 64 位十六进制指纹，或 6 位配对码（如 123-456）。',
          );
        }
        return (
          policy: policy,
          needsWarning: policy.warnsUser,
          error: null,
        );
    }
  }

  /// The expect string handed to the service (null = pairing book).
  String? _expectTextFor(AcceptPolicy? policy) {
    if (policy == null) return null;
    if (policy is AcceptAnyPolicy) return 'any';
    return _expect.text.trim();
  }

  Future<void> _start() async {
    setState(() => _formError = null);
    final outDir = _outDir.text.trim();
    if (outDir.isEmpty) {
      setState(() => _formError = '请先填写输出目录。');
      return;
    }
    final port = int.tryParse(_port.text.trim());
    if (port == null || port < 0 || port > 65535) {
      setState(() => _formError = '端口须为 0–65535（0 表示自动分配）。');
      return;
    }
    final (:policy, :needsWarning, :error) = _resolvePolicy();
    if (error != null) {
      setState(() => _formError = error);
      return;
    }
    if (needsWarning) {
      final confirmed = await _confirmAnyDialog();
      if (confirmed != true) return;
    }
    try {
      final session = widget.service.startReceive(ReceiveOptions(
        outDir: outDir,
        port: port,
        admission: _admission,
        expectText: _expectTextFor(policy),
        conflict: _conflict,
      ));
      setState(() {
        if (_session != null) _history.insert(0, _session!);
        _watch(session);
      });
    } catch (error) {
      setState(() => _formError = '启动失败：$error');
    }
  }

  /// The `any` admission warning dialog ("接受每一个对端" confirmation).
  Future<bool?> _confirmAnyDialog() {
    return showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('确认放开准入限制？'),
        content: const Text(
          '选择"接受每一个对端"后，任何连到本机地址的设备都可以向你发送文件。'
          '请仅在与信任设备同处内网时使用，并勿把本机地址与配对码告知不信任的人。',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('仍要开始'),
          ),
        ],
      ),
    );
  }

  Future<void> _stop() async {
    try {
      await widget.service.stopReceive();
    } finally {
      if (mounted) {
        setState(() {
          if (_session != null) _history.insert(0, _session!);
          _watch(null);
        });
      }
    }
  }

  /// First bound address among the session's connections, from the
  /// `listening` progress event folded by the controller.
  String? _listenAddr(JobController? session) {
    if (session == null) return null;
    for (final connection in session.connections.values) {
      final address = connection.addr;
      if (address != null) return address;
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final info = widget.service.deviceInfo();
    final session = _session;
    final addr = _listenAddr(session);

    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('接收', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 16),
        if (session != null && _isListening) ...[
          _ListeningBanner(info: info, addr: addr),
          const SizedBox(height: 16),
        ],
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('监听设置', style: theme.textTheme.titleMedium),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('receive_out_dir'),
                  controller: _outDir,
                  decoration: const InputDecoration(
                    labelText: '输出目录（完整路径）',
                    hintText: r'例如 C:\Users\me\Downloads\ak-in',
                    helperText: '粘贴目录完整路径；目录不存在时会自动创建。'
                        '当前版本未集成系统目录选择窗口。',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 12),
                TextField(
                  key: const ValueKey('receive_port'),
                  controller: _port,
                  keyboardType: TextInputType.number,
                  decoration: const InputDecoration(
                    labelText: '端口',
                    helperText: '0–65535；0 表示由系统自动分配临时端口。',
                    border: OutlineInputBorder(),
                    suffix: SizedBox(width: 160),
                  ),
                ),
                const SizedBox(height: 16),
                Text('准入策略（对端如何被接受）',
                    style: theme.textTheme.labelLarge),
                _AdmissionTile(
                  key: const ValueKey('receive_admission_book'),
                  selected: _admission == AdmissionMode.book,
                  onTap: () =>
                      setState(() => _admission = AdmissionMode.book),
                  title: '配对簿（默认）',
                  subtitle:
                      '只接受本机配对簿（peers.book）内的指纹；簿为空时引擎会拒绝启动，'
                      '错误原因会显示在下方。配对簿外的连接会被拒绝并继续等待。',
                ),
                _AdmissionTile(
                  key: const ValueKey('receive_admission_pinned'),
                  selected: _admission == AdmissionMode.pinned,
                  onTap: () =>
                      setState(() => _admission = AdmissionMode.pinned),
                  title: '指定指纹或配对码',
                  subtitle: '64 位十六进制指纹，或 6 位配对码（如 123-456）。',
                ),
                if (_admission == AdmissionMode.pinned)
                  Padding(
                    padding: const EdgeInsets.only(left: 12, right: 12),
                    child: TextField(
                      key: const ValueKey('receive_expect'),
                      controller: _expect,
                      decoration: const InputDecoration(
                        labelText: '对端指纹或配对码',
                        border: OutlineInputBorder(),
                      ),
                    ),
                  ),
                _AdmissionTile(
                  key: const ValueKey('receive_admission_any'),
                  selected: _admission == AdmissionMode.any,
                  onTap: () =>
                      setState(() => _admission = AdmissionMode.any),
                  title: '接受每一个对端（any）',
                  subtitle: '有安全风险：开始前需确认警告弹窗。',
                ),
                const SizedBox(height: 8),
                Text('重名处理', style: theme.textTheme.labelLarge),
                const SizedBox(height: 4),
                SegmentedButton<String>(
                  segments: const [
                    ButtonSegment(
                        value: 'fail', label: Text('重名报错')),
                    ButtonSegment(
                        value: 'rename', label: Text('自动重命名')),
                  ],
                  selected: {_conflict},
                  onSelectionChanged: (s) =>
                      setState(() => _conflict = s.first),
                ),
                if (_formError != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 12),
                    child: Text(
                      _formError!,
                      style:
                          TextStyle(color: theme.colorScheme.error),
                    ),
                  ),
                const SizedBox(height: 16),
                Row(
                  children: [
                    FilledButton.icon(
                      onPressed: _isListening ? null : _start,
                      icon: const Icon(Icons.play_arrow),
                      label: const Text('开始监听'),
                    ),
                    const SizedBox(width: 12),
                    OutlinedButton.icon(
                      onPressed: _isListening ? _stop : null,
                      icon: const Icon(Icons.stop),
                      label: const Text('停止'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
        if (session != null)
          TaskCard(
            key: const ValueKey('receive_task_card'),
            controller: session,
            title: '接收任务',
            subtitle: '输出：${_outDir.text}',
          ),
        for (final controller in _history.take(3))
          TaskCard(
            controller: controller,
            title: '接收任务（历史）',
            initiallyExpanded: false,
          ),
      ],
    );
  }
}

/// The "sender-facing" banner shown while listening: bound address, this
/// device's fingerprint and the large pairing code the sender should
/// enter (or scan from a QR of the same data).
class _ListeningBanner extends StatelessWidget {
  const _ListeningBanner({required this.info, required this.addr});

  final DeviceInfo info;
  final String? addr;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Card(
      color: scheme.primaryContainer,
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(Icons.wifi_tethering, color: scheme.onPrimaryContainer),
                const SizedBox(width: 8),
                Text('监听中 — 把下面的信息告诉发送方',
                    style: theme.textTheme.titleMedium
                        ?.copyWith(color: scheme.onPrimaryContainer)),
              ],
            ),
            const SizedBox(height: 12),
            Text('地址', style: theme.textTheme.labelLarge),
            SelectableText(
              addr ?? '等待端口绑定…',
              key: const ValueKey('receive_listen_addr'),
              style: theme.textTheme.headlineSmall
                  ?.copyWith(fontFamily: 'monospace'),
            ),
            const SizedBox(height: 8),
            Text('本机指纹', style: theme.textTheme.labelLarge),
            SelectableText(
              info.fingerprintHex.toLowerCase(),
              key: const ValueKey('receive_listen_fp'),
              style: theme.textTheme.bodyMedium
                  ?.copyWith(fontFamily: 'monospace'),
            ),
            const SizedBox(height: 12),
            Text('配对码（发送方扫码或输码配对）',
                style: theme.textTheme.labelLarge),
            SelectableText(
              info.pairingCode,
              key: const ValueKey('receive_listen_code'),
              style: theme.textTheme.displaySmall
                  ?.copyWith(fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 12),
            Text(
              'Windows 防火墙可能弹出提示，请选择"允许"；'
              '未完成校验与落盘前不会显示接收成功。',
              style: theme.textTheme.bodySmall,
            ),
          ],
        ),
      ),
    );
  }
}

/// One selectable admission-policy row (custom radio tile; avoids the
/// deprecated `RadioListTile.groupValue` API).
class _AdmissionTile extends StatelessWidget {
  const _AdmissionTile({
    super.key,
    required this.selected,
    required this.onTap,
    required this.title,
    required this.subtitle,
  });

  final bool selected;
  final VoidCallback onTap;
  final String title;
  final String subtitle;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return ListTile(
      onTap: onTap,
      selected: selected,
      leading: Icon(
        selected ? Icons.radio_button_checked : Icons.radio_button_off,
        color: selected ? scheme.primary : scheme.outline,
      ),
      title: Text(title),
      subtitle: Text(subtitle),
    );
  }
}
