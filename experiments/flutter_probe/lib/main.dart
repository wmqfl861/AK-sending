import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'controller.dart';
import 'native_api.dart';

const riskChannel = MethodChannel('ak.m0/risk');
void main() => runApp(const MaterialApp(home: ProbePage()));

class ProbePage extends StatefulWidget {
  final ProbeApi? api;
  final Future<String> Function()? permission;
  const ProbePage({super.key, this.api, this.permission});
  @override
  State<ProbePage> createState() => _ProbePageState();
}

class _ProbePageState extends State<ProbePage> with WidgetsBindingObserver {
  ProbeController? controller;
  Timer? timer;
  bool operation = false;
  bool foreground = true;
  String platformStatus = 'not-tested';
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    try {
      controller = ProbeController(widget.api ?? NativeProbe());
    } catch (_) {
      platformStatus = 'native-unavailable';
    }
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

  Future<String> invoke(String method) async {
    if (!Platform.isAndroid && !Platform.isIOS) return 'not-applicable';
    try {
      return await riskChannel
              .invokeMethod<String>(method)
              .timeout(const Duration(seconds: 45)) ??
          'failed';
    } catch (_) {
      return 'platform-error';
    }
  }

  Future<void> start() async {
    if (operation || !(controller?.canStart ?? false)) return;
    setState(() {
      operation = true;
    });
    final result = await (widget.permission?.call() ?? invoke('network'));
    if (!mounted) return;
    operation = false;
    platformStatus = result;
    if (foreground &&
        [
          'granted',
          'not-required',
          'not-applicable',
          'loopback-only',
        ].contains(result)) {
      controller?.start();
    }
    watch();
  }

  Future<void> directory(String method) async {
    if (operation || (controller?.active ?? false) || !foreground) return;
    setState(() {
      operation = true;
    });
    final result = await invoke(method);
    if (!mounted) return;
    setState(() {
      operation = false;
      platformStatus = result;
    });
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    foreground = state == AppLifecycleState.resumed;
    controller?.setForeground(foreground);
    watch();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    timer?.cancel();
    final owned = controller;
    owned?.dispose();
    // Short, bounded reaper; never drop the library or fake that a worker exited.
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
    final idle = !operation && !(controller?.active ?? false) && foreground;
    return Scaffold(
      appBar: AppBar(title: const Text('AK · M0 风险实验')),
      body: ListView(
        padding: const EdgeInsets.all(24),
        children: [
          const Text('同机实验，不是文件传输或局域网／真机验收。'),
          const SizedBox(height: 16),
          Text(
            controller?.status ?? 'native-unavailable',
            key: const Key('status'),
          ),
          Text('平台：$platformStatus'),
          const SizedBox(height: 16),
          FilledButton(
            onPressed: idle && (controller?.canStart ?? false) ? start : null,
            child: const Text('运行合成 TLS 实验'),
          ),
          OutlinedButton(
            onPressed: controller?.active ?? false
                ? () {
                    controller?.cancel();
                    watch();
                  }
                : null,
            child: const Text('取消并释放'),
          ),
          const Divider(height: 32),
          const Text('目录实验仅在明确操作后创建一个随机命名的 64 字节新文件，验证后删除；不会读取现有文件。请选择本机测试目录。'),
          OutlinedButton(
            onPressed: idle ? () => directory('choose') : null,
            child: const Text('选择测试目录（不写入）'),
          ),
          OutlinedButton(
            onPressed: idle ? () => directory('test') : null,
            child: const Text('创建、校验并删除测试文件'),
          ),
          OutlinedButton(
            onPressed: idle ? () => directory('forget') : null,
            child: const Text('忘记本机目录授权'),
          ),
          const Text('后台切换会取消实验；前台恢复不会自动重启。权限拒绝、杀进程、无公网 Wi-Fi 和资源预算仍须物理设备验证。'),
        ],
      ),
    );
  }
}
