/// The shared engine-error card visual (R36-P2-03): the error-container
/// card the app shell renders for startup failures, extracted so the
/// home page's engine-failed banner REUSES the exact same style instead
/// of forking it. Keys stay caller-owned — the shell keeps its frozen
/// `app_startup_error` keys, the home page passes `engine_failed_card`.
library;

import 'package:flutter/material.dart';

class EngineErrorCard extends StatelessWidget {
  const EngineErrorCard({
    super.key,
    required this.title,
    required this.detail,
    this.titleKey,
    this.detailKey,
  });

  /// The headline line (already user-readable Chinese).
  final String title;

  /// The detail line under the title (the failure reason etc.).
  final String detail;

  final Key? titleKey;
  final Key? detailKey;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Card(
      margin: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      color: scheme.errorContainer,
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(Icons.error_outline, color: scheme.onErrorContainer),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    key: titleKey,
                    style: theme.textTheme.titleSmall
                        ?.copyWith(color: scheme.onErrorContainer),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    detail,
                    key: detailKey,
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: scheme.onErrorContainer),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
