/// W7.3 (AUD-09): the Android foreground hint — v1 only promises
/// foreground transfers, so every card that shows a RUNNING transfer
/// adds this one line on Android ("传输期间请保持本应用在前台").
/// P2-B (third review): the WAITING stages hold the pairing alive in
/// exactly the same way (a backgrounded process kills the registry
/// keeper — the W-N11 reproduction), so the same hint family with
/// stage-adapted wording covers them via [KeepForegroundHint.text].
/// Platform-gated through the injectable [UiPlatform] seam; nothing on
/// Windows/Linux. No method channel involved.
library;

import 'package:flutter/material.dart';

import '../service/ui_platform.dart';

class KeepForegroundHint extends StatelessWidget {
  const KeepForegroundHint({
    super.key,
    this.keyName = 'keep_foreground_hint',
    this.text = '传输期间请保持本应用在前台',
  });

  /// Key suffix — callers on compound cards pass their own (e.g.
  /// `conn_card_keep_foreground`), tests address the line through it.
  final String keyName;

  /// P2-B: the stage's line — the running-transfer default (W7.3) or a
  /// same-family variant for a waiting stage (「…期间请保持本应用在前台」,
  /// the AUDIT §4.5 copy style).
  final String text;

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
            text,
            key: ValueKey(keyName),
            style: theme.textTheme.labelSmall
                ?.copyWith(color: theme.colorScheme.outline),
          ),
        ),
      ],
    );
  }
}
