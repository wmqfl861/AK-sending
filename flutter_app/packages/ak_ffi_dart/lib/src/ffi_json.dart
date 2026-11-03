/// Spec-JSON builders for `ak_send_start` / `ak_receive_start` plus the
/// hand-rolled UTF-8 <-> C-string conversions used across the boundary.
///
/// The builders mirror the field validation of `crates/ak-ffi/src/core.rs`
/// (`build_send_spec` / `build_receive_spec`) so a malformed spec fails in
/// Dart with a friendly [AkSpecException] before it ever reaches the DLL;
/// the Rust side re-validates everything and stays the authority. Output is
/// compact JSON (`jsonEncode`, no added whitespace) in a fixed field order.
///
/// No third-party packages: `dart:convert` supplies utf8/json, and the
/// native memory comes from [processAllocator] in `bindings.dart`.
library;

import 'dart:convert' show jsonDecode, jsonEncode, utf8;

import 'dart:ffi';

import 'bindings.dart' show processAllocator;

/// A spec rejected by the Dart-side mirror of the Rust validation.
class AkSpecException implements Exception {
  const AkSpecException(this.message);

  final String message;

  @override
  String toString() => 'AkSpecException: $message';
}

// ---------------------------------------------------------------------------
// UTF-8 C-string conversions (replacement for package:ffi's Utf8 helpers).
// ---------------------------------------------------------------------------

/// Encode [text] as NUL-terminated UTF-8 in freshly allocated native memory.
///
/// The caller owns the returned pointer and must release it with
/// `allocator.free(pointer)` (the default allocator exposes [processAllocator]
/// whose `free` can be called directly).
Pointer<Uint8> toNativeUtf8(String text, {Allocator? allocator}) {
  final heap = allocator ?? processAllocator;
  final bytes = utf8.encode(text);
  final pointer = heap.allocate<Uint8>(bytes.length + 1);
  for (var i = 0; i < bytes.length; i++) {
    pointer[i] = bytes[i];
  }
  pointer[bytes.length] = 0;
  return pointer;
}

/// Decode the NUL-terminated UTF-8 C string at [pointer].
///
/// Reads up to the first NUL byte; invalid UTF-8 throws (the ak-ffi shim
/// only ever writes valid UTF-8).
String fromNativeUtf8(Pointer<Uint8> pointer) {
  var length = 0;
  while (pointer[length] != 0) {
    length++;
  }
  return utf8.decode(pointer.asTypedList(length));
}

// ---------------------------------------------------------------------------
// Shared validation mirrors (see crates/ak-ffi/src/core.rs).
// ---------------------------------------------------------------------------

/// Rust parser policy: control characters (anything < U+0020) never appear
/// in string values, raw or escaped; strings are capped at 4096 bytes.
const _maxStringBytes = 4096;
const _maxInputBytes = 1 << 20;

final _hex64 = RegExp(r'^[0-9a-fA-F]{64}$');
final _ipv4Octet = RegExp(r'^(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])$');
final _digits = RegExp(r'^[0-9]+$');
final _v6Body = RegExp(r'^[0-9A-Fa-f:.%]+$');

void _checkString(String field, String value) {
  for (final rune in value.runes) {
    if (rune < 0x20) {
      throw AkSpecException(
        'field "$field" must not contain control characters '
        '(found U+${rune.toRadixString(16).padLeft(4, '0').toUpperCase()}); '
        'the wire codec refuses them in either raw or escaped form',
      );
    }
  }
  if (utf8.encode(value).length > _maxStringBytes) {
    throw AkSpecException(
      'field "$field" is longer than $_maxStringBytes bytes',
    );
  }
}

bool _portOk(String text) {
  if (text.isEmpty || text.length > 5 || !_digits.hasMatch(text)) {
    return false;
  }
  return int.parse(text) <= 65535;
}

/// Mirror of Rust `SocketAddr::from_str`: an `ip:port` (IPv4 dotted quad
/// without leading zeros, or a bracketed IPv6 literal); hostnames rejected.
bool _isIpPort(String text) {
  if (text.startsWith('[')) {
    final close = text.indexOf(']');
    if (close < 2 || close + 1 >= text.length || text[close + 1] != ':') {
      return false;
    }
    final host = text.substring(1, close);
    return _v6Body.hasMatch(host) && _portOk(text.substring(close + 2));
  }
  final colon = text.lastIndexOf(':');
  if (colon <= 0) {
    return false;
  }
  final octets = text.substring(0, colon).split('.');
  if (octets.length != 4) {
    return false;
  }
  if (!octets.every(_ipv4Octet.hasMatch)) {
    return false;
  }
  return _portOk(text.substring(colon + 1));
}

void _checkPeer(String peer) {
  _checkString('peer', peer);
  if (!_isIpPort(peer)) {
    throw AkSpecException('field "peer" is not an ip:port address: "$peer"');
  }
}

void _checkFingerprint(String field, String fpHex) {
  _checkString(field, fpHex);
  if (!_hex64.hasMatch(fpHex)) {
    throw AkSpecException('field "$field" must be 64 hex characters');
  }
}

void _checkConflict(String conflict) {
  _checkString('conflict', conflict);
  if (conflict != 'fail' && conflict != 'rename') {
    throw AkSpecException(
      'field "conflict" must be "fail" or "rename", got "$conflict"',
    );
  }
}

void _checkTimeoutSecs(int secs) {
  if (secs < 1 || secs > 86400) {
    throw AkSpecException(
      'field "timeout_secs" must be between 1 and 86400',
    );
  }
}

void _checkDeviceName(String deviceName) {
  _checkString('device_name', deviceName);
  if (deviceName.runes.length > 64) {
    throw AkSpecException(
      'field "device_name" must be at most 64 characters',
    );
  }
}

void _checkIdDir(String idDir) {
  _checkString('id_dir', idDir);
}

void _checkRange(String field, int value, int min, int max) {
  if (value < min || value > max) {
    throw AkSpecException(
      'field "$field" must be between $min and $max',
    );
  }
}

/// Mirror of `Expect::parse`: `any` or `book` (case-insensitive), a 64-hex
/// fingerprint, or a 6-digit pairing code (digits plus `-`/space separators).
void _checkExpect(String expect) {
  _checkString('expect', expect);
  final trimmed = expect.trim();
  final lowered = trimmed.toLowerCase();
  if (lowered == 'any' || lowered == 'book') {
    return;
  }
  if (_hex64.hasMatch(trimmed)) {
    return;
  }
  final digits = trimmed.replaceAll(RegExp(r'[^0-9]'), '');
  final separatorsOk = trimmed.runes.every(
    (r) => (r >= 0x30 && r <= 0x39) || r == 0x2D || r == 0x20,
  );
  if (separatorsOk && digits.length == 6) {
    return;
  }
  throw AkSpecException(
    'field "expect" must be "any", "book", a 64-hex fingerprint, or a 6-digit '
    'pairing code',
  );
}

/// Encode [json] compactly and enforce the 1 MiB input cap of the parser.
String _encode(Object? json) {
  final text = jsonEncode(json);
  if (utf8.encode(text).length > _maxInputBytes) {
    throw AkSpecException(
      'spec JSON exceeds the $_maxInputBytes byte input cap',
    );
  }
  return text;
}

// ---------------------------------------------------------------------------
// Send spec.
// ---------------------------------------------------------------------------

/// Builder for the `ak_send_start` spec JSON (schema frozen in
/// `crates/ak-ffi/src/lib.rs`; unknown fields are ignored by Rust, so this
/// emits exactly the fields it knows).
class AkSendSpec {
  AkSendSpec({
    required List<String> sourceRoots,
    required this.peer,
    required this.fpHex,
    this.chunkKb,
    this.multistream,
    this.noInternet,
    this.timeoutSecs,
    this.conflict,
    this.deviceName,
    this.idDir,
  }) : sourceRoots = List.unmodifiable(sourceRoots);

  /// Required, >= 1 entry, every entry a non-empty path.
  final List<String> sourceRoots;

  /// Required `ip:port` of the receiver (hostname literals rejected).
  final String peer;

  /// Required receiver fingerprint, 64 hex characters.
  final String fpHex;

  /// Optional, 1..=8192 (default 1024 on the Rust side).
  final int? chunkKb;

  /// Optional (needs peer capability).
  final bool? multistream;

  /// Optional policy gate (loopback/LAN unaffected).
  final bool? noInternet;

  /// Optional idle budget, 1..=86400 seconds.
  final int? timeoutSecs;

  /// Optional `"fail"` (default) or `"rename"`.
  final String? conflict;

  /// Optional, at most 64 characters.
  final String? deviceName;

  /// Optional; `<id_dir>/device.key` holds the identity, else ephemeral.
  final String? idDir;

  /// Validate (throwing [AkSpecException]) and return the JSON object.
  Map<String, dynamic> toJson() {
    if (sourceRoots.isEmpty) {
      throw AkSpecException(
        'missing required field "source_roots" (at least one path)',
      );
    }
    for (final root in sourceRoots) {
      _checkString('source_roots', root);
      if (root.isEmpty) {
        throw AkSpecException(
          'entries of "source_roots" must be non-empty paths',
        );
      }
    }
    _checkPeer(peer);
    _checkFingerprint('fp_hex', fpHex);
    final chunk = chunkKb;
    final timeout = timeoutSecs;
    final name = deviceName;
    final identityDir = idDir;
    if (chunk != null) {
      _checkRange('chunk_kb', chunk, 1, 8192);
    }
    if (timeout != null) {
      _checkTimeoutSecs(timeout);
    }
    final policy = conflict;
    if (policy != null) {
      _checkConflict(policy);
    }
    if (name != null) {
      _checkDeviceName(name);
    }
    if (identityDir != null && identityDir.isNotEmpty) {
      _checkIdDir(identityDir);
    }
    return <String, dynamic>{
      'source_roots': sourceRoots,
      'peer': peer,
      'fp_hex': fpHex,
      if (chunk != null) 'chunk_kb': chunk,
      if (multistream != null) 'multistream': multistream,
      if (noInternet != null) 'no_internet': noInternet,
      if (timeout != null) 'timeout_secs': timeout,
      if (policy != null) 'conflict': policy,
      if (name != null) 'device_name': name,
      if (identityDir != null && identityDir.isNotEmpty) 'id_dir': identityDir,
    };
  }

  /// Compact spec JSON for `ak_send_start`; throws [AkSpecException] when a
  /// field violates the mirrored validation.
  String toJsonString() => _encode(toJson());
}

// ---------------------------------------------------------------------------
// Receive spec.
// ---------------------------------------------------------------------------

/// Builder for the `ak_receive_start` spec JSON.
class AkReceiveSpec {
  const AkReceiveSpec({
    required this.outDir,
    this.port,
    this.expect,
    this.conflict,
    this.timeoutSecs,
    this.idDir,
    this.deviceName,
    this.onceJobs,
    this.noInternet,
  });

  /// Required output directory (created by the Rust side when missing).
  final String outDir;

  /// Optional UDP port 0..=65535; null emits nothing (Rust default 7100);
  /// 0 asks the OS for an ephemeral port, learned from the `listening`
  /// progress event (only meaningful with the default `onceJobs == 1`).
  final int? port;

  /// Optional admission policy: `"any"` (default), `"book"`, a 64-hex
  /// fingerprint, or a 6-digit pairing code.
  ///
  /// `"book"` admits ONLY fingerprints listed in `<id_dir>/peers.book`
  /// (the book is re-read on every connection, so pairing edits apply
  /// without restarting a concurrent receive) and requires a non-empty
  /// [idDir]; the engine validates both at start — a missing, empty, or
  /// corrupt book makes `receiveStart` fail with an `AkFfiException`
  /// ("pair first") before any listener binds.
  final String? expect;

  /// Optional `"fail"` (default) or `"rename"`.
  final String? conflict;

  /// Optional idle budget, 1..=86400 seconds.
  final int? timeoutSecs;

  /// Optional stable identity directory (`<id_dir>/device.key`).
  final String? idDir;

  /// Optional, at most 64 characters.
  final String? deviceName;

  /// Optional 1..=64; > 1 serves concurrently and requires an explicit
  /// non-zero [port] (that path never reports its bound address).
  final int? onceJobs;

  /// Optional policy gate (loopback/LAN unaffected).
  final bool? noInternet;

  /// Validate (throwing [AkSpecException]) and return the JSON object.
  Map<String, dynamic> toJson() {
    _checkString('out_dir', outDir);
    if (outDir.isEmpty) {
      throw AkSpecException('field "out_dir" must be non-empty');
    }
    final bindPort = port;
    final jobs = onceJobs;
    if (bindPort != null) {
      _checkRange('port', bindPort, 0, 65535);
    }
    if (jobs != null) {
      _checkRange('once_jobs', jobs, 1, 64);
    }
    final effectivePort = bindPort ?? 7100;
    if (jobs != null && jobs > 1 && effectivePort == 0) {
      throw AkSpecException(
        'once_jobs > 1 requires an explicit non-zero "port" '
        '(concurrent receive does not report its bound address)',
      );
    }
    final admission = expect;
    if (admission != null) {
      _checkExpect(admission);
    }
    final timeout = timeoutSecs;
    if (timeout != null) {
      _checkTimeoutSecs(timeout);
    }
    final policy = conflict;
    if (policy != null) {
      _checkConflict(policy);
    }
    final name = deviceName;
    if (name != null) {
      _checkDeviceName(name);
    }
    final identityDir = idDir;
    if (identityDir != null && identityDir.isNotEmpty) {
      _checkIdDir(identityDir);
    }
    return <String, dynamic>{
      'out_dir': outDir,
      if (bindPort != null) 'port': bindPort,
      if (admission != null) 'expect': admission,
      if (policy != null) 'conflict': policy,
      if (timeout != null) 'timeout_secs': timeout,
      if (identityDir != null && identityDir.isNotEmpty) 'id_dir': identityDir,
      if (name != null) 'device_name': name,
      if (jobs != null) 'once_jobs': jobs,
      if (noInternet != null) 'no_internet': noInternet,
    };
  }

  /// Compact spec JSON for `ak_receive_start`; throws [AkSpecException]
  /// when a field violates the mirrored validation.
  String toJsonString() => _encode(toJson());
}

/// Decode one progress/result JSON line back to a map (used by tests and by
/// callers that keep raw strings).
Map<String, dynamic> decodeFfiJson(String line) =>
    jsonDecode(line) as Map<String, dynamic>;
