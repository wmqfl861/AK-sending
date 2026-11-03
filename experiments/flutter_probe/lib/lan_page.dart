import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'lan_api.dart';
import 'lan_controller.dart';

class LanPage extends StatefulWidget {
  final LanApi? api;
  final Future<String> Function()? permission;
  const LanPage({super.key, this.api, this.permission});
  @override
  State<LanPage> createState() => _LanPageState();
}

class _LanPageState extends State<LanPage> with WidgetsBindingObserver {
  final ip = TextEditingController();
  final port = TextEditingController(text: '42424');
  final offer = TextEditingController();
  LanController? controller;
  Timer? timer;
  bool sending = false;
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    try {
      controller = LanController(
        widget.api ?? NativeLan(),
        widget.permission ?? permission,
      );
      final state = WidgetsBinding.instance.lifecycleState;
      if (state != null)
        controller!.setForeground(state == AppLifecycleState.resumed);
    } catch (_) {
      /* Explicit native-unavailable screen; no fallback transport. */
    }
  }

  Future<String> permission() async {
    if (!Platform.isAndroid && !Platform.isIOS) return 'not-applicable';
    return await const MethodChannel('ak.m0/risk')
            .invokeMethod<String>('lanNetwork') ??
        'failed';
  }

  void refresh() {
    controller?.tick();
    if (mounted) setState(() {});
    if (!(controller?.active ?? false)) {
      timer?.cancel();
      timer = null;
    }
  }

  void watch() {
    if (controller?.active ?? false) {
      timer ??= Timer.periodic(
        const Duration(milliseconds: 200),
        (_) => refresh(),
      );
    }
    refresh();
  }

  Future<void> prepare() async {
    final future = controller?.prepare(ip.text.trim(), port.text.trim());
    setState(() {});
    await future;
    if (mounted) watch();
  }

  Future<void> start() async {
    final future = controller?.start();
    setState(() {});
    await future;
    if (mounted) watch();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    controller?.setForeground(state == AppLifecycleState.resumed);
    watch();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    timer?.cancel();
    ip.dispose();
    port.dispose();
    offer.dispose();
    final owned = controller;
    owned?.dispose();
    if (owned?.active ?? false) {
      var ticks = 0;
      Timer.periodic(const Duration(milliseconds: 200), (reaper) {
        owned!.tick();
        if (!owned.active || ++ticks >= 75) reaper.cancel();
      });
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = controller;
    return Scaffold(
      appBar: AppBar(title: const Text('AK · 双设备 LAN 实验')),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: [
          const Text('只传两块固定合成数据，不读取或传输用户文件。接收端先开始监听，发送端再开始。'),
          const Text(
            '输入本机 Wi-Fi／有线接口 IPv4，不是对端地址。本阶段仅支持私网 IPv4；127.0.0.1 仅供同机测试。',
          ),
          TextField(
            key: const Key('lan-ip'),
            controller: ip,
            enabled: c?.canPrepare ?? false,
            decoration: const InputDecoration(labelText: '本机 IPv4（手动输入）'),
          ),
          TextField(
            key: const Key('lan-port'),
            controller: port,
            enabled: c?.canPrepare ?? false,
            decoration: const InputDecoration(
              labelText: '本机 UDP 端口：1024–65535',
            ),
          ),
          FilledButton(
            key: const Key('lan-prepare'),
            onPressed: c?.canPrepare ?? false ? prepare : null,
            child: const Text('准备临时身份（不开始连接）'),
          ),
          Text(c?.status ?? 'native-unavailable', key: const Key('lan-status')),
          Text('权限状态：${c?.permissionStatus ?? "not-tested"}'),
          const Text(
            'system-mediated 只表示由系统在实际访问时询问，绝不表示已获授权。系统弹窗或后台切换会取消当前实验，返回后须重新准备。',
          ),
          if (c != null && c.localOffer.isNotEmpty) ...[
            const Divider(),
            const Text('本机公开配对信息（含地址和公开证书，请仅向另一台自己的设备交换）：'),
            SelectableText(c.localOffer),
            TextButton(
              onPressed: () async {
                try {
                  await Clipboard.setData(ClipboardData(text: c.localOffer));
                } catch (_) {
                  /* No logs. */
                }
              },
              child: const Text('明确复制本机公开配对信息'),
            ),
            const Text('本机完整证书 SHA-256：'),
            SelectableText(c.localFingerprint),
          ],
          const Divider(),
          TextField(
            key: const Key('lan-offer'),
            controller: offer,
            enabled: c?.canPair ?? false,
            maxLength: lanOfferLimit,
            maxLines: 4,
            decoration: const InputDecoration(labelText: '粘贴另一端 AKL1 配对信息'),
          ),
          SwitchListTile(
            value: sending,
            onChanged: c?.canPair ?? false
                ? (v) => setState(() => sending = v)
                : null,
            title: Text(sending ? '本机角色：发送固定合成数据' : '本机角色：接收固定合成数据'),
          ),
          OutlinedButton(
            key: const Key('lan-pair'),
            onPressed: c?.canPair ?? false
                ? () {
                    c!.pair(offer.text, sending: sending);
                    watch();
                  }
                : null,
            child: const Text('固定对端与角色（尚未开始）'),
          ),
          if (c != null && c.peerFingerprint.isNotEmpty) ...[
            const Text('对端完整证书 SHA-256（必须与另一台设备显示的本机摘要一致）：'),
            SelectableText(c.peerFingerprint),
          ],
          CheckboxListTile(
            key: const Key('lan-confirm'),
            value: c?.verified ?? false,
            onChanged:
                c != null &&
                    c.paired &&
                    c.nativeState == 12 &&
                    !c.pending &&
                    c.foreground
                ? (v) {
                    c.setVerified(v ?? false);
                    setState(() {});
                  }
                : null,
            title: const Text('我已在另一台设备核对完整摘要，并允许仅本次合成数据实验'),
          ),
          FilledButton(
            key: const Key('lan-start'),
            onPressed: c?.canStart ?? false ? start : null,
            child: Text(sending ? '确认并发送固定合成数据' : '确认并监听接收固定合成数据'),
          ),
          OutlinedButton(
            onPressed: c != null && (c.active || c.pending)
                ? () {
                    c.cancel();
                    watch();
                  }
                : null,
            child: const Text('取消并释放临时身份'),
          ),
          const Text(
            '临时身份与实验有时限；取消、后台或过期后需要全新配对。失败不会通过服务器继续。实验成功不代表文件落盘或完成全部真机验收。',
          ),
        ],
      ),
    );
  }
}
