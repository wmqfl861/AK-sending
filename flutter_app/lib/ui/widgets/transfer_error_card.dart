/// W3 (V-N4): the shared failure card — title/reason/suggestion from the
/// [transferErrorView] mapping plus an expandable 「详情」 that carries the
/// error's ORIGINAL text (error code included) and a copy button. The
/// original never renders while the details are collapsed (built only
/// when expanded), so `AkFfiException`/`AK_FAILED` vocabulary can never
/// leak onto a default screen.
///
/// Callers keep their frozen outer keys (`conn_send_error`,
/// `incoming_error_<job>`, …) via [cardKey]; inner lines are keyed
/// `<prefix>_title` / `_reason` / `_suggestion` / `_toggle` / `_detail` /
/// `_copy`.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard, ClipboardData;

import '../service/transfer_errors.dart';

class TransferErrorCard extends StatefulWidget {
  const TransferErrorCard({
    super.key,
    required this.error,
    required this.scene,
    required this.keyPrefix,
    this.titleKey,
  });

  /// The error object as caught (ANY shape the mapping understands). The
  /// card classifies and words it; it never prints the object directly
  /// outside the details section.
  final Object error;

  final TransferErrorScene scene;

  /// Prefix of the inner line keys (see the library doc).
  final String keyPrefix;

  /// Optional override for the TITLE text's key (e.g. the home flow
  /// keeps its frozen `flow_error` key on the title line).
  final Key? titleKey;

  @override
  State<TransferErrorCard> createState() => _TransferErrorCardState();
}

class _TransferErrorCardState extends State<TransferErrorCard> {
  bool _expanded = false;

  Future<void> _copyDetail(String detail) async {
    await Clipboard.setData(ClipboardData(text: detail));
    if (!mounted) return;
    ScaffoldMessenger.maybeOf(context)?.showSnackBar(
      const SnackBar(
        content: Text('已复制错误详情'),
        duration: Duration(seconds: 2),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final view = transferErrorView(widget.error, widget.scene);
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
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
                  view.title,
                  key: widget.titleKey ?? ValueKey('${widget.keyPrefix}_title'),
                  style: theme.textTheme.titleSmall
                      ?.copyWith(color: scheme.onErrorContainer),
                ),
                const SizedBox(height: 2),
                Text(
                  view.reason,
                  key: ValueKey('${widget.keyPrefix}_reason'),
                  style: theme.textTheme.bodySmall
                      ?.copyWith(color: scheme.onErrorContainer),
                ),
                const SizedBox(height: 2),
                Text(
                  view.suggestion,
                  key: ValueKey('${widget.keyPrefix}_suggestion'),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onErrorContainer,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const SizedBox(height: 4),
                Row(
                  children: [
                    TextButton(
                      key: ValueKey('${widget.keyPrefix}_toggle'),
                      onPressed: () =>
                          setState(() => _expanded = !_expanded),
                      child: Text(_expanded ? '收起详情' : '详情'),
                    ),
                    // The copy button only exists WITH the details —
                    // copying a text that is not on screen would be a lie.
                    if (_expanded) ...[
                      IconButton(
                        key: ValueKey('${widget.keyPrefix}_copy'),
                        icon: const Icon(Icons.copy, size: 18),
                        tooltip: '复制错误详情',
                        onPressed: () => _copyDetail(view.detail),
                      ),
                    ],
                  ],
                ),
                // Built ONLY while expanded: the original text (error
                // code included) never exists on a collapsed card.
                if (_expanded)
                  SelectableText(
                    view.detail,
                    key: ValueKey('${widget.keyPrefix}_detail'),
                    style: theme.textTheme.bodySmall?.copyWith(
                      fontFamily: 'monospace',
                      color: scheme.onErrorContainer,
                    ),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
