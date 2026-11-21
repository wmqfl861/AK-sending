/// W7.3 (AUD-09): the Android foreground hint — v1 only promises
/// foreground transfers, so every card that shows a RUNNING transfer
/// adds this one line on Android ("传输期间请保持本应用在前台").
/// Platform-gated through the injectable [UiPlatform] seam; nothing on
/// Windows/Linux. No method channel involved.
library;

import 'package:flutter/material.dart';

import '../service/ui_platform.dart';

class KeepForegroundHint extends StatelessWidget {
  const KeepForegroundHint({super.key, this.keyName = 'keep_foreground_hint'});

  /// Key suffix — callers on compound cards pass their own (e.g.
  /// `conn_card_keep_foreground`), tests address the line through it.
  final String keyName;

  @override
  Widget build(BuildContext context) {
    if (!UiPlatform.isAndroid) {
      return const SizedBox.shrink();
    }
    final theme = Theme.of(context);
    return Row(
      children: [
        Icon(Icons.smartphone, size: 14, color: theme.colorScheme.outline),
        const SizedBox(width: 4),
        Expanded(
          child: Text(
            '传输期间请保持本应用在前台',
            key: ValueKey(keyName),
            style: theme.textTheme.labelSmall
                ?.copyWith(color: theme.colorScheme.outline),
          ),
        ),
      ],
    );
  }
}
