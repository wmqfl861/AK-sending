import 'package:flutter/material.dart';

import '../service/transfer_service.dart';

/// About: the ADR-001 one-liner, the explicit no-relay statement, and the
/// engine version from `service.version()`.
class AboutPage extends StatelessWidget {
  const AboutPage({super.key, required this.service});

  final TransferService service;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        Text('关于', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(Icons.security_outlined,
                        color: theme.colorScheme.primary),
                    const SizedBox(width: 8),
                    Text('仅设备直连承载文件数据（ADR-001）',
                        style: theme.textTheme.titleMedium),
                  ],
                ),
                const SizedBox(height: 8),
                const Text(
                  '允许服务器参与设备发现、信令和 NAT 穿透协调，'
                  '但绝不允许任何服务器参与文件数据传输，包括加密中继。',
                ),
              ],
            ),
          ),
        ),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(Icons.block_outlined, color: theme.colorScheme.error),
                    const SizedBox(width: 8),
                    Text('无中继声明', style: theme.textTheme.titleMedium),
                  ],
                ),
                const SizedBox(height: 8),
                const Text(
                  '本应用不提供、不使用任何中继或"加速服务器"，也不经云端暂存：'
                  '连接失败时不会把文件上传到服务器，只会按你的选择等待重试或更换网络。',
                ),
              ],
            ),
          ),
        ),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('版本', style: theme.textTheme.titleMedium),
                const SizedBox(height: 8),
                SelectableText(
                  service.version(),
                  key: const ValueKey('about_version'),
                  style: theme.textTheme.bodyLarge
                      ?.copyWith(fontFamily: 'monospace'),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
