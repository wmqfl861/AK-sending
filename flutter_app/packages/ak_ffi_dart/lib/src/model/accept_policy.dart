/// Receiver admission policy parsed from the receive spec's `"expect"`
/// field: `"any"` (warn), a 64-hex pinned fingerprint, or a 6-digit
/// pairing code.
///
/// Parsing mirrors `Expect::parse` in crates/ak-ffi/src/core.rs branch for
/// branch and in the same order:
///
/// 1. `any`, ASCII-case-insensitive, after trimming;
/// 2. exactly 64 hex characters (case-insensitive) — a pinned fingerprint;
/// 3. otherwise only ASCII digits plus `-`/space separators with exactly
///    6 digits — a pairing code (kept in its typed form, like Rust keeps
///    the trimmed input);
/// 4. anything else is a [FormatException] carrying the engine's message.
///
/// Note step 2 wins over step 3: a 64-character all-digit string is a
/// fingerprint, not a (malformed) code — same as Rust.
///
/// ## Pairing-code matching boundary
///
/// `ak_identity::pairing_code` derives the code from the fingerprint with
/// blake3 (`"ak-pairing-code"` domain tag over the 32 fingerprint bytes).
/// This package is FFI-free and deliberately does **not** reimplement
/// blake3, so [AcceptPairingCodePolicy.matchFingerprint] throws
/// [UnsupportedError]: the code-to-fingerprint check must run in Rust.
/// Pass the raw typed code as the receive spec's `"expect"` field; the
/// engine's authorize closure calls `verify_pairing_code` for you. Dart
/// only validates the *shape* of what the user typed.
library;

import 'display.dart' show isFingerprintHex;

/// Error text matching the engine's `Expect::parse` failure verbatim.
const String expectPolicyErrorMessage =
    'field "expect" must be "any", a 64-hex fingerprint, or a 6-digit pairing code';

/// Parsed `"expect"` value.
sealed class AcceptPolicy {
  const AcceptPolicy();

  /// Parses [text]; throws [FormatException] with
  /// [expectPolicyErrorMessage] for anything unrecognized.
  factory AcceptPolicy.parse(String text) {
    final trimmed = text.trim();
    if (_isAny(trimmed)) {
      return const AcceptAnyPolicy();
    }
    if (isFingerprintHex(trimmed)) {
      return AcceptPinnedPolicy(trimmed.toLowerCase());
    }
    var digits = 0;
    for (var i = 0; i < trimmed.length; i++) {
      final c = trimmed.codeUnitAt(i);
      final isDigit = c >= 0x30 && c <= 0x39;
      final isSeparator = c == 0x2D || c == 0x20; // '-' or ' '
      if (isDigit) {
        digits += 1;
      } else if (!isSeparator) {
        throw const FormatException(expectPolicyErrorMessage);
      }
    }
    if (digits == 6) {
      return AcceptPairingCodePolicy(trimmed);
    }
    throw const FormatException(expectPolicyErrorMessage);
  }

  /// Non-throwing variant for form validation.
  static AcceptPolicy? tryParse(String text) {
    try {
      return AcceptPolicy.parse(text);
    } on FormatException {
      return null;
    }
  }

  /// True when this policy is risky enough that the UI should show a
  /// warning before starting the receive (only "any" accepts unknown
  /// peers silently).
  bool get warnsUser;

  /// True when [matchFingerprint] can decide locally: "any" and a pinned
  /// fingerprint need shape checks only; a pairing code needs blake3 in
  /// Rust (see the library docs).
  bool get canMatchLocally;

  /// Whether a connecting peer with fingerprint [fingerprintHex]
  /// (64 hex chars, either case) passes this policy.
  ///
  /// Throws [FormatException] when [fingerprintHex] is not 64 hex chars
  /// (a broken contract, not a mismatch) and [UnsupportedError] for the
  /// pairing-code policy, whose check must run in Rust.
  bool matchFingerprint(String fingerprintHex);
}

/// `"any"`: accept whoever connects. [warnsUser] is true — surface a
/// confirmation before starting.
final class AcceptAnyPolicy implements AcceptPolicy {
  const AcceptAnyPolicy();

  @override
  bool get warnsUser => true;

  @override
  bool get canMatchLocally => true;

  @override
  bool matchFingerprint(String fingerprintHex) {
    _validateFingerprintShape(fingerprintHex);
    return true;
  }

  @override
  String toString() => 'AcceptAnyPolicy()';
}

/// A pinned 64-hex fingerprint: only that exact peer connects.
final class AcceptPinnedPolicy implements AcceptPolicy {
  const AcceptPinnedPolicy(this.fingerprintHex);

  /// The pinned fingerprint, normalized to 64 lowercase hex chars.
  final String fingerprintHex;

  @override
  bool get warnsUser => false;

  @override
  bool get canMatchLocally => true;

  @override
  bool matchFingerprint(String fingerprintHex) {
    _validateFingerprintShape(fingerprintHex);
    return fingerprintHex.toLowerCase() == this.fingerprintHex;
  }

  @override
  String toString() => 'AcceptPinnedPolicy($fingerprintHex)';
}

/// A 6-digit pairing code (with or without the `123-456` dash). The
/// fingerprint-to-code comparison itself runs in Rust (blake3); Dart only
/// validated the shape.
final class AcceptPairingCodePolicy implements AcceptPolicy {
  const AcceptPairingCodePolicy(this.code);

  /// The typed code exactly as accepted (trimmed input, separators kept —
  /// `verify_pairing_code` ignores `-`/space).
  final String code;

  /// The 6 digits, separator-free.
  String get digits {
    final buffer = StringBuffer();
    for (var i = 0; i < code.length; i++) {
      final c = code.codeUnitAt(i);
      if (c >= 0x30 && c <= 0x39) buffer.writeCharCode(c);
    }
    return buffer.toString();
  }

  @override
  bool get warnsUser => false;

  @override
  bool get canMatchLocally => false;

  @override
  bool matchFingerprint(String fingerprintHex) {
    _validateFingerprintShape(fingerprintHex);
    throw UnsupportedError(
      'pairing-code matching needs blake3 over the fingerprint; pass the code '
      'as the receive spec "expect" field and let the engine verify it '
      '(ak_identity::verify_pairing_code)',
    );
  }

  @override
  String toString() => 'AcceptPairingCodePolicy($code)';
}

bool _isAny(String trimmed) {
  if (trimmed.length != 3) return false;
  for (var i = 0; i < 3; i++) {
    var c = trimmed.codeUnitAt(i);
    if (c >= 0x41 && c <= 0x5A) c += 0x20; // ASCII fold, like eq_ignore_ascii_case
    if (c != 'any'.codeUnitAt(i)) return false;
  }
  return true;
}

void _validateFingerprintShape(String fingerprintHex) {
  if (!isFingerprintHex(fingerprintHex)) {
    throw const FormatException('fingerprint must be exactly 64 hex characters');
  }
}
