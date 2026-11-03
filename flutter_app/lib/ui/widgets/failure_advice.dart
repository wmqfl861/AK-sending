/// Actionable failure advice aligned with the product copy requirements:
///
/// * docs/UX.zh-CN.md line 10 — the connectivity-failure sentence
///   "当前网络无法建立设备直连，文件没有上传到服务器";
/// * docs/REVIEW_2026-11-02.zh-CN.md §4.1 "连不上时的出口" — the concrete
///   exits: switch network, phone hotspot, IPv6, router UPnP, or use an
///   always-on PC/NAS as the receiver (ADR-001 allows it);
/// * docs/UX.zh-CN.md "CLI 原型已实现的交互" — conflict advice
///   (send separately), cancel keeps .akpart resumable state.
library;

/// The mandated no-relay sentence shown on connectivity failures.
const String kNoRelayNotice = '当前网络无法建立设备直连，文件没有上传到服务器。';

/// True when the error text looks like a connectivity/dial failure.
bool _looksLikeConnectivityFailure(String text) {
  const keywords = [
    'connect', 'dial', 'timeout', 'timed out', 'unreachable', 'refused',
    'network', 'reset', 'broken pipe', 'no route', 'address family',
    'wsa', 'socket',
  ];
  return keywords.any(text.contains);
}

/// Advice lines for a failed transfer, shown under the red error bar of
/// the task card. The first line of the connectivity branch is exactly
/// [kNoRelayNotice] (UX doc copy).
List<String> transferFailureAdvice(String? error) {
  final text = (error ?? '').toLowerCase();
  if (_looksLikeConnectivityFailure(text)) {
    return const [
      kNoRelayNotice,
      '可以把两台设备换到同一个 Wi-Fi，或用手机热点组建局域网后重试。',
      '公网直连可尝试：启用 IPv6、开启路由器 UPnP/NAT-PMP 端口映射，或更换网络。',
      '也可以用一台常开的电脑或 NAS 作为接收端（仅设备直连，无中继）。',
      '首次监听时 Windows 可能弹出防火墙提示，请选择"允许"；公用网络下入站常被拦截。',
    ];
  }
  if (text.contains('conflict') || text.contains('duplicate') ||
      text.contains('exists') || text.contains('重名')) {
    return const [
      '多个来源路径中存在同名文件：可分开发送，或在设置中选择"自动重命名"。',
    ];
  }
  if (text.contains('cancel')) {
    return const [
      '任务已被取消；未完成的分片（.akpart）与检查点保留，下次可续传。',
    ];
  }
  if (text.contains('fingerprint') || text.contains('pairing') ||
      text.contains('accept')) {
    return const [
      '对端未通过准入检查：请核对指纹或配对码是否与本机"主页"页显示的一致。',
    ];
  }
  return const [
    '请重试；若持续失败，可在"关于"页核对版本与指纹后反馈日志。',
  ];
}
