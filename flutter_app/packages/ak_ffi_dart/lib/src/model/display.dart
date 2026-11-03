/// Pure display helpers: the two-line fingerprint block, pairing-code
/// normalization, and progress-bar text. Behavior-aligned Dart
/// reimplementations of the ak-cli display conventions
/// (crates/ak-cli/src/main.rs: `fingerprint_lines`, `human_bytes`) and the
/// ak-identity pairing-code shape (crates/ak-identity/src/lib.rs,
/// "###-###"). No hashing happens here — see model/accept_policy.dart for
/// the fingerprint-to-code boundary.
library;

import 'report.dart' show humanBytes;

/// True when [text] is exactly 64 hex characters (either case), the shape
/// `Fingerprint::from_hex` accepts.
bool isFingerprintHex(String text) {
  if (text.length != 64) return false;
  for (var i = 0; i < text.length; i++) {
    final c = text.codeUnitAt(i);
    final isDigit = c >= 0x30 && c <= 0x39;
    final isLowerHex = c >= 0x61 && c <= 0x66;
    final isUpperHex = c >= 0x41 && c <= 0x46;
    if (!isDigit && !isLowerHex && !isUpperHex) return false;
  }
  return true;
}

/// First 4 bytes as 8 lowercase hex characters (`Fingerprint::short_id`).
///
/// Throws [FormatException] when [fingerprintHex] is not 64 hex chars.
String fingerprintShortId(String fingerprintHex) {
  return _normalizedFingerprint(fingerprintHex).substring(0, 8);
}

/// The two-line fingerprint display used everywhere in ak-cli:
///
/// ```text
/// peer: 1a2b3c4d
/// peer (full): 1a2b3c4d...<64 hex chars, lowercase>
/// ```
///
/// Input is case-insensitive (display is canonical lowercase); throws
/// [FormatException] for anything that is not 64 hex characters.
String fingerprintLines(String label, String fingerprintHex) {
  final full = _normalizedFingerprint(fingerprintHex);
  return '$label: ${full.substring(0, 8)}\n$label (full): $full';
}

String _normalizedFingerprint(String fingerprintHex) {
  if (!isFingerprintHex(fingerprintHex)) {
    throw const FormatException('fingerprint must be exactly 64 hex characters');
  }
  return fingerprintHex.toLowerCase();
}

/// Canonical pairing-code display `###-###` (e.g. `123-456`).
///
/// Accepts the same input shapes `ak_identity::verify_pairing_code` does:
/// exactly 6 ASCII digits with any number of `-`/space separators
/// (`123456`, `123-456`, `123 456`, `1-2-3-4-5-6`). Anything else — wrong
/// digit count, non-ASCII digits, other characters — throws
/// [FormatException].
///
/// Dart never *derives* a code from a fingerprint (that needs blake3 in
/// Rust); this only normalizes user input for display and storage.
String formatPairingCode(String input) {
  final digits = StringBuffer();
  for (var i = 0; i < input.length; i++) {
    final c = input.codeUnitAt(i);
    if (c == 0x2D || c == 0x20) continue; // '-' or ' '
    if (c < 0x30 || c > 0x39) {
      throw const FormatException(
        'pairing code must be exactly 6 digits (optionally "123-456")',
      );
    }
    digits.writeCharCode(c);
    if (digits.length > 6) {
      throw const FormatException(
        'pairing code must be exactly 6 digits (optionally "123-456")',
      );
    }
  }
  final text = digits.toString();
  if (text.length != 6) {
    throw const FormatException(
      'pairing code must be exactly 6 digits (optionally "123-456")',
    );
  }
  return '${text.substring(0, 3)}-${text.substring(3)}';
}

/// Whole-number percent (0..100) of [transferred] within [total].
///
/// `total <= 0` reports 0; [transferred] is clamped into `[0, total]`
/// before rounding to nearest.
int progressPercent(int transferred, int total) {
  if (total <= 0) return 0;
  final clamped = transferred < 0 ? 0 : (transferred > total ? total : transferred);
  return ((clamped * 100) / total).round();
}

/// One-line progress bar, e.g. `[#####-----] 50% 4.00 GiB/8.00 GiB`.
///
/// * [width] characters inside the brackets, `#` for the filled share
///   (rounded to nearest), `-` for the rest;
/// * an integer percent (see [progressPercent]);
/// * transferred/total via [humanBytes] (binary units, two decimals —
///   byte counts are printed as passed, so a buggy upstream that reports
///   transferred > total shows honestly in the text).
///
/// `total <= 0` renders an empty bar at 0%; negative counts render as 0.
String progressBarText({required int transferred, required int total, int width = 10}) {
  if (width <= 0) {
    throw ArgumentError.value(width, 'width', 'must be positive');
  }
  var filled = 0;
  var percent = 0;
  if (total > 0) {
    final clamped = transferred < 0 ? 0 : (transferred > total ? total : transferred);
    percent = ((clamped * 100) / total).round();
    filled = ((clamped * width) / total).round();
    if (filled > width) filled = width;
  }
  final shown = StringBuffer('[')
    ..write('#' * filled)
    ..write('-' * (width - filled))
    ..write('] $percent% ')
    ..write(humanBytes(transferred < 0 ? 0 : transferred))
    ..write('/')
    ..write(humanBytes(total < 0 ? 0 : total));
  return shown.toString();
}
