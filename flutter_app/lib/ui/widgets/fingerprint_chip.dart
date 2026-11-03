import 'package:ak_ffi_dart/ak_ffi_dart.dart' show fingerprintShortId;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Compact fingerprint chip: short id by default, tap to expand the full
/// 64-hex form with a copy button (the "two-line fingerprint" convention
/// of ak-cli, restructured for a card layout).
///
/// Invalid input (not 64 hex chars) is displayed as-is and marked 无效
/// instead of throwing.
class FingerprintChip extends StatefulWidget {
  const FingerprintChip({
    super.key,
    required this.fingerprintHex,
    this.label,
  });

  /// The fingerprint to display (64 hex chars, either case).
  final String fingerprintHex;

  /// Optional leading label, e.g. `本机` or `对端`.
  final String? label;

  @override
  State<FingerprintChip> createState() => _FingerprintChipState();
}

class _FingerprintChipState extends State<FingerprintChip> {
  bool _expanded = false;

  String? get _shortId {
    try {
      return fingerprintShortId(widget.fingerprintHex);
    } on FormatException {
      return null;
    }
  }

  Future<void> _copy() async {
    await Clipboard.setData(
        ClipboardData(text: widget.fingerprintHex.toLowerCase()));
    if (mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        const SnackBar(content: Text('已复制完整指纹'), duration: Duration(seconds: 2)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final shortId = _shortId;
    final label = widget.label == null ? '' : '${widget.label} ';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          borderRadius: BorderRadius.circular(8),
          onTap: () => setState(() => _expanded = !_expanded),
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(Icons.fingerprint, size: 18, color: scheme.primary),
                const SizedBox(width: 6),
                Text(
                  shortId == null
                      ? '$label指纹无效'
                      : '$label指纹 $shortId',
                  style: const TextStyle(fontFamily: 'monospace'),
                ),
                Icon(_expanded ? Icons.expand_less : Icons.expand_more,
                    size: 18),
              ],
            ),
          ),
        ),
        if (_expanded)
          Padding(
            padding: const EdgeInsets.only(left: 24, bottom: 4),
            child: Row(
              children: [
                Expanded(
                  child: SelectableText(
                    widget.fingerprintHex.toLowerCase(),
                    style: Theme.of(context).textTheme.bodySmall?.copyWith(
                          fontFamily: 'monospace',
                        ),
                  ),
                ),
                IconButton(
                  icon: const Icon(Icons.copy, size: 16),
                  tooltip: '复制完整指纹',
                  onPressed: _copy,
                ),
              ],
            ),
          ),
      ],
    );
  }
}
