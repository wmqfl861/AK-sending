/// Invite-code models of the wave-22 `ak_pairing_*` family: the invite
/// registry rows ([InviteSummary]/[InviteCreated]) and the client-side
/// Crockford Base32 invite-code validator.
///
/// Source contracts:
/// * `docs/contracts/pairing-flow.md` §1 — code format: 20 random bytes
///   (160 bit entropy) encoded in Crockford Base32 (alphabet
///   `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, no I/L/O/U), displayed as
///   `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` (`AKI1` is the format
///   version and NOT part of the entropy). Normalization accepts lowercase
///   ASCII and either the canonical dashes or no grouping; Unicode
///   homoglyphs, control characters, any whitespace, wrong lengths and
///   undefined characters are rejected.
/// * `docs/contracts/pairing-flow.md` §8 — lifecycle: `ttl_secs=0` means
///   long-lived; finite TTLs are 60 s..30 d; `expires_ms == 0` renders as
///   "never expires"; states `active`/`revoked`/`expired` are terminal-safe
///   (an expired/revoked code never becomes active again).
///
/// The op-result JSON envelopes decoded here mirror the wave-22 Rust side
/// (`crates/ak-ffi/src/pairing.rs`, `run_op`): list rows are
/// `{"code_id","state","created_ms","expires_ms","revision"}`; the create
/// result is `{"op":"create","code":"AKI1-...","hint":"AKI1-XXXX…",
/// "code_id":"<32hex>","expires_ms":N,"revision":N}` (the plaintext `code`
/// is shown once; `hint` is the engine's masked form); the list envelope
/// is `{"op":"list","next_cursor":"<32hex>","entries":[...]}`; revoke is
/// `{"op":"revoke","code_id":"...","revision":N,"revoked_ms":N}`; a failed
/// op is `{"op":"...","error":"..."}`.
///
/// FFI-free plain data: unit-testable without the DLL.
library;

/// Lifecycle state of one invite code, as listed by `ak_pairing_list_start`
/// (the owner sees the real states; redeemers only ever see
/// `PAIR_UNAVAILABLE`, see pairing-flow.md §11).
enum InviteState {
  /// Claimed on the registry and redeemable.
  active,

  /// Owner-revoked (possibly still pending server sync).
  revoked,

  /// Finite TTL elapsed.
  expired;

  /// The wire string of this state.
  String get wireName => switch (this) {
        InviteState.active => 'active',
        InviteState.revoked => 'revoked',
        InviteState.expired => 'expired',
      };

  /// Decodes the `"state"` field; unknown values throw [FormatException]
  /// (a newer engine introducing a state must not decode silently into a
  /// wrong bucket).
  static InviteState fromWire(String wire) => switch (wire) {
        'active' => InviteState.active,
        'revoked' => InviteState.revoked,
        'expired' => InviteState.expired,
        _ => throw FormatException('未知的邀请码状态 "$wire"'),
      };
}

/// One invite-code registry row (metadata only — the plaintext code is NOT
/// part of rows re-listed later; only the create result carries it).
///
/// JSON shape: `{"code_id":"<32 hex>","state":"active",
/// "created_ms":1730000000000,"expires_ms":0,"revision":1}`.
final class InviteSummary {
  const InviteSummary({
    required this.codeId,
    required this.state,
    required this.createdMs,
    required this.expiresMs,
    required this.revision,
  });

  /// Registry id of the code, 32 hex characters (the 16-byte `code_id`).
  final String codeId;

  /// Lifecycle state of the code.
  final InviteState state;

  /// When the persistent claim succeeded, Unix epoch milliseconds.
  final int createdMs;

  /// When the code stops being redeemable, epoch milliseconds; `0` means
  /// long-lived (`ttl_secs == 0`).
  final int expiresMs;

  /// Registry revision of the code (bumped by revoke/expiry transitions);
  /// redeem transcripts bind it (pairing-flow.md §7 "换码检测").
  final int revision;

  /// Decode one `ak_pairing_list_start` row.
  factory InviteSummary.fromJson(Map<String, dynamic> json) => InviteSummary(
        codeId: _requiredString(json, 'code_id'),
        state: InviteState.fromWire(_requiredString(json, 'state')),
        createdMs: _requiredInt(json, 'created_ms'),
        expiresMs: _requiredInt(json, 'expires_ms'),
        revision: _requiredInt(json, 'revision'),
      );

  /// Decodes the `"entries"` array of a list op result (the engine's list
  /// envelope is `{"op":"list","next_cursor":...,"entries":[...]}`).
  static List<InviteSummary> listFromJson(Map<String, dynamic> json) {
    final raw = json['entries'];
    if (raw is! List) {
      throw const FormatException('邀请码列表结果缺少 "entries" 数组');
    }
    return [
      for (final element in raw)
        InviteSummary.fromJson(element as Map<String, dynamic>),
    ];
  }

  /// The `"next_cursor"` of a list envelope ('' at the end of pagination).
  static String listNextCursor(Map<String, dynamic> json) =>
      json['next_cursor'] is String ? json['next_cursor'] as String : '';

  /// True when [expiresMs] is non-zero and before [nowMs] — a pure helper
  /// for UI freshness chips; the registry stays the authority.
  bool isExpiredAt(int nowMs) =>
      expiresMs != 0 && expiresMs < nowMs;

  @override
  bool operator ==(Object other) =>
      other is InviteSummary &&
      other.codeId == codeId &&
      other.state == state &&
      other.createdMs == createdMs &&
      other.expiresMs == expiresMs &&
      other.revision == revision;

  @override
  int get hashCode =>
      Object.hash(codeId, state, createdMs, expiresMs, revision);

  @override
  String toString() =>
      'InviteSummary($codeId ${state.wireName} rev=$revision '
      'expires=${expiresMs == 0 ? "<never>" : expiresMs})';
}

/// The successful `ak_pairing_create_start` op result: the registry row of
/// the new code PLUS the plaintext invitation code, shown to the user
/// exactly once (it is a bearer credential — pairing-flow.md §7).
///
/// Engine envelope (crates/ak-ffi/src/pairing.rs): `{"op":"create",
/// "code":"AKI1-...","hint":"AKI1-XXXX…","code_id":"<32hex>",
/// "expires_ms":N,"revision":N}` — no `state`/`created_ms` (a freshly
/// claimed code is active by definition; the authoritative row with the
/// real `created_ms` comes from the following list read).
final class InviteCreated {
  const InviteCreated({
    required this.summary,
    required this.invitationCode,
    this.hint = '',
  });

  final InviteSummary summary;

  /// The canonical display form `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-
  /// XXXX`; empty when the engine returned the row without a code.
  final String invitationCode;

  /// The engine's masked hint of the code (first groups + ellipsis) for
  /// list rows — the plaintext never re-renders.
  final String hint;

  /// Decodes a create op-result envelope (throws [FormatException] on a
  /// missing/invalid code shape).
  factory InviteCreated.fromJson(Map<String, dynamic> json) {
    final codeId = json['code_id'];
    if (codeId is! String || codeId.isEmpty) {
      throw const FormatException('邀请码创建结果缺少 "code_id" 字段');
    }
    final expiresMs = json['expires_ms'];
    final revision = json['revision'];
    final code = json['code'];
    final hint = json['hint'];
    final invitationCode =
        code is String && code.isNotEmpty ? normalizeInviteCode(code) : '';
    return InviteCreated(
      summary: InviteSummary(
        codeId: codeId,
        state: InviteState.active,
        createdMs: 0,
        expiresMs: expiresMs is int && expiresMs >= 0 ? expiresMs : 0,
        revision: revision is int && revision >= 0 ? revision : 1,
      ),
      invitationCode: invitationCode,
      hint: hint is String ? hint : '',
    );
  }

  @override
  bool operator ==(Object other) =>
      other is InviteCreated &&
      other.summary == summary &&
      other.invitationCode == invitationCode &&
      other.hint == hint;

  @override
  int get hashCode => Object.hash(summary, invitationCode, hint);

  @override
  String toString() =>
      'InviteCreated(${summary.codeId} ${summary.state.wireName} '
      'code=${invitationCode.isEmpty ? "<none>" : "<redacted>"})';
}

// ---------------------------------------------------------------------------
// Crockford Base32 invite-code validation (pairing-flow.md §1).
// ---------------------------------------------------------------------------

/// The Crockford Base32 alphabet (32 symbols; I, L, O and U excluded).
const String crockfordAlphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/// Format-version prefix of the display form; not part of the entropy.
const String inviteCodePrefix = 'AKI1';

/// Number of Crockford payload groups after the prefix (8 groups of 4 =
/// 32 chars = 160 bits).
const int inviteCodeGroups = 8;

/// Total payload characters (32) and the total ungrouped length (`AKI1` +
/// 32 = 36).
const int inviteCodePayloadLength = inviteCodeGroups * 4;
const int inviteCodeUngroupedLength =
    inviteCodePrefix.length + inviteCodePayloadLength;

/// True when [rune] is an ASCII character of the Crockford alphabet
/// (`0-9 A-Z` minus the excluded `I L O U`).
bool _isCrockfordRune(int rune) =>
    rune < 0x80 && crockfordAlphabet.contains(String.fromCharCode(rune));

/// Validates and normalizes one invite code (pairing-flow.md §1 rules).
///
/// Accepted inputs: the canonical grouped display form (dashes exactly
/// between every 4 characters, `AKI1-XXXX-...`), the ungrouped form
/// (`AKI1XXXX...`, 36 chars), and ASCII-lowercase variants of both.
/// Rejected: any whitespace (including interior spaces), Unicode
/// homoglyphs and every non-ASCII rune, control characters, Crockford's
/// excluded letters (`I L O U`) and other undefined characters, dashes in
/// wrong positions, and wrong lengths. Crockford's optional `*` checksum
/// symbol is NOT accepted (the format has no checksum slot).
///
/// Returns the canonical grouped display form; throws [FormatException]
/// with a user-presentable reason otherwise.
String normalizeInviteCode(String input) {
  if (input.isEmpty) {
    throw const FormatException('邀请码不能为空。');
  }
  // Whole-input character gate first: no non-ASCII, no whitespace, no
  // control characters.
  for (final rune in input.runes) {
    if (rune >= 0x80) {
      throw const FormatException('邀请码包含非 ASCII 字符（可能是同形字符），请重新复制。');
    }
    if (rune < 0x20) {
      throw const FormatException('邀请码含有控制字符。');
    }
    if (rune == 0x20) {
      // 0x20 is the only ASCII whitespace above the control range.
      throw const FormatException('邀请码不能包含空白字符。');
    }
  }
  // Version prefix gate (case-insensitive): everything before the payload.
  // The prefix legitimately contains `I` — the Crockford exclusion below
  // applies to the payload only.
  if (input.length < inviteCodePrefix.length ||
      input.substring(0, inviteCodePrefix.length).toUpperCase() !=
          inviteCodePrefix) {
    throw const FormatException('邀请码必须以 "AKI1" 版本前缀开头。');
  }
  // Payload character gate: only Crockford symbols and dashes.
  for (final rune in input.substring(inviteCodePrefix.length).runes) {
    final char = String.fromCharCode(rune);
    if (char == '-') {
      continue;
    }
    final upper = char.toUpperCase();
    if (!_isCrockfordRune(upper.codeUnitAt(0))) {
      if (RegExp(r'[ilouILU]').hasMatch(char)) {
        throw const FormatException(
            '邀请码含有 Crockford 字母表排除的字符（I/L/O/U），请核对后重新输入。');
      }
      throw FormatException('邀请码含有未定义字符 "$char"。');
    }
  }
  final upper = input.toUpperCase();
  final compact = upper.replaceAll('-', '');
  if (compact.length != inviteCodeUngroupedLength) {
    throw FormatException(
        '邀请码长度不正确：应为 $inviteCodeUngroupedLength 个字符（不含连字符），'
        '实际为 ${compact.length} 个。');
  }
  // Dash-position check on the ORIGINAL input: when grouped, dashes must sit
  // exactly at 4, 9, 14, ... (i.e. every 4th character of the compact run).
  if (upper.contains('-')) {
    final compactRun = StringBuffer();
    var group = 0;
    var i = 0;
    while (i < upper.length) {
      if (upper[i] == '-') {
        if (group != 4) {
          throw const FormatException('邀请码连字符位置不正确（应每 4 个字符一组）。');
        }
        group = 0;
        i++;
        continue;
      }
      compactRun.write(upper[i]);
      group++;
      i++;
    }
    if (group != 4) {
      throw const FormatException('邀请码连字符位置不正确（应每 4 个字符一组）。');
    }
    if (compactRun.toString() != compact) {
      throw const FormatException('邀请码连字符位置不正确（应每 4 个字符一组）。');
    }
  }
  final payload = compact.substring(inviteCodePrefix.length);
  for (final rune in payload.runes) {
    if (!crockfordAlphabet.contains(String.fromCharCode(rune))) {
      // Unreachable after the whole-input gate; kept as a defensive assert.
      throw const FormatException('邀请码含有未定义字符。');
    }
  }
  final groups = <String>[
    for (var g = 0; g < inviteCodeGroups; g++)
      payload.substring(g * 4, g * 4 + 4),
  ];
  return '$inviteCodePrefix-${groups.join('-')}';
}

/// True when [input] normalizes cleanly (the non-throwing probe for live
/// form validation).
bool isValidInviteCode(String input) {
  try {
    normalizeInviteCode(input);
    return true;
  } on FormatException {
    return false;
  }
}

String _requiredString(Map<String, dynamic> json, String key) {
  final value = json[key];
  if (value is! String || value.isEmpty) {
    throw FormatException('邀请码结果缺少字符串字段 "$key"');
  }
  return value;
}

int _requiredInt(Map<String, dynamic> json, String key) {
  final value = json[key];
  if (value is! int || value < 0) {
    throw FormatException('邀请码结果缺少非负整数字段 "$key"');
  }
  return value;
}
