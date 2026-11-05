/// dart:ffi layer over the nine `ak_pairing_*` exports of the wave-22
/// C ABI (signatures frozen in `docs/contracts/ffi-v2.md` §1):
///
/// ```text
/// ak_pairing_start(engine, spec_json, runtime_id_out[16])
/// ak_pairing_create_start(engine, runtime_id, spec_json, op_id_out[16])
/// ak_pairing_redeem_start(engine, runtime_id, spec_json, op_id_out[16])
/// ak_pairing_list_start(engine, runtime_id, query_json, op_id_out[16])
/// ak_pairing_revoke_start(engine, runtime_id, spec_json, op_id_out[16])
/// ak_pairing_op_result(engine, runtime_id, op_id, buf, cap)
/// ak_pairing_poll_event(engine, runtime_id, buf, cap)
/// ak_pairing_cancel_op(engine, runtime_id, op_id)
/// ak_pairing_stop_start(engine, runtime_id, op_id_out[16])
/// ```
///
/// Layering (testable without the DLL):
///
/// ```text
/// spec builders (pure, mirrored validation, AkSpecException)
///        │ toJsonString()
/// ▼
/// PairingFfi / PairingRuntimeFfi / PairingOpHandle   (friendly layer:
///        │                      id marshaling, -3 auto-grow, typed events)
///        ▼
/// AkPairingCalls (pointer-free per-call seam; fakes drive every loop)
///        ▲ implemented by
/// AkPairingFfiBindings (dart:ffi: pointers + the frozen signatures)
/// ```
///
/// Contract notes baked in (ffi-v2.md §2/§3/§5):
/// * `start` returning `AK_OK` means "accepted", NOT "online" — online is
///   the `service_online` pairing event.
/// * `-3 AK_ERR_BUFFER` consumes nothing: the buffer grow-retry loops here
///   are lossless by construction.
/// * Ops are idempotent per `request_id`; polling a result repeatedly only
///   reads the same stored result.
/// * Pairing events are polled separately from transfer progress — never
///   feed these lines to `ak_poll_progress` parsers.
/// * One native engine handle is owned by ONE Dart isolate; calls on it
///   are serialized (the Dart event loop does this for free).
library;

import 'dart:async' show TimeoutException;
import 'dart:convert' show jsonEncode, utf8;
import 'dart:ffi';
import 'dart:math' show Random;
import 'dart:typed_data' show Uint8List;

import 'bindings.dart'
    show AkEngine, AkFfiException, AkStatus, processAllocator;
import 'ffi_json.dart'
    show AkSpecException, decodeFfiJson, toNativeUtf8;
import 'model/invite.dart';
import 'model/pairing_events.dart';

/// Byte length of every wave-22 id (`runtime_id` / `op_id` / `task_id`).
const int akPairingIdLength = 16;

/// Defensive ceiling for the automatic `-3` grow-retry loops.
const int _maxBufferBytes = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Id helpers (16-byte ids ⇄ 32 hex chars; request ids are the hex form).
// ---------------------------------------------------------------------------

/// Lowercase hex of a 16-byte id (`runtime_id`/`op_id`/`task_id`).
String akIdToHex(Uint8List id) {
  if (id.length != akPairingIdLength) {
    throw ArgumentError.value(id.length, 'id.length',
        'wave-22 ids are exactly $akPairingIdLength bytes');
  }
  final sb = StringBuffer();
  for (final byte in id) {
    sb.write(byte.toRadixString(16).padLeft(2, '0'));
  }
  return sb.toString();
}

/// Decodes 32 hex characters into a 16-byte id; throws [FormatException]
/// otherwise (never silently truncates).
Uint8List akIdFromHex(String hex) {
  if (!RegExp(r'^[0-9a-fA-F]{32}$').hasMatch(hex)) {
    throw const FormatException('id 必须是 32 个十六进制字符');
  }
  final bytes = Uint8List(akPairingIdLength);
  for (var i = 0; i < akPairingIdLength; i++) {
    bytes[i] = int.parse(hex.substring(i * 2, i * 2 + 2), radix: 16);
  }
  return bytes;
}

/// A fresh cryptographically random `request_id` (32 hex characters from
/// 16 CSPRNG bytes) — the idempotency key of one logical operation.
String newRequestId([Random? random]) {
  final rng = random ?? Random.secure();
  final bytes = List<int>.generate(akPairingIdLength, (_) => rng.nextInt(256));
  return bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
}

// ---------------------------------------------------------------------------
// Spec builders (pure Dart; frozen JSON of ffi-v2.md §4 where given).
// ---------------------------------------------------------------------------

final RegExp _hex32 = RegExp(r'^[0-9a-fA-F]{32}$');

void _checkRequestId(String requestId) {
  if (!_hex32.hasMatch(requestId)) {
    throw const AkSpecException('字段 "request_id" 必须是 32 个十六进制字符');
  }
}

/// Builder for the `ak_pairing_start` spec — the runtime configuration.
///
/// Wire fields as parsed by the wave-22 engine (crates/ak-ffi/src/
/// pairing.rs, `pairing_start`): REQUIRED `registry` (an `ip:port`
/// coordination endpoint), `registry_id` (64 hex — the registry's raw
/// 32-byte Ed25519 public key) and `id_dir`; OPTIONAL
/// `device_name` (≤64 chars), `owner_loop` (default true — the resident
/// auto-pairing loop, pairing-flow.md §4) and `redeem_window_ms`
/// (1000..119000, default 90000). Unknown fields are ignored, so the
/// `spec_version` marker rides along harmlessly.
class AkPairingStartSpec {
  const AkPairingStartSpec({
    required this.registry,
    required this.registryIdHex,
    required this.idDir,
    this.deviceName,
    this.ownerLoop = true,
    this.redeemWindowMs = 90000,
  });

  /// Coordination registry endpoint as a literal `ip:port` (hostnames
  /// rejected, like the receive `peer` field).
  final String registry;

  /// Pinned registry identity: 64 hex characters encoding the registry's
  /// raw 32-byte Ed25519 public key (pairing-flow.md §3: unknown pins are
  /// never auto-accepted).
  final String registryIdHex;

  /// Identity directory of this runtime owner (created when missing);
  /// non-empty, one runtime owner per directory (ffi-v2.md §2).
  final String idDir;

  /// Device name advertised to peers (≤64 chars; default engine-side
  /// `"ak-ffi"`).
  final String? deviceName;

  /// Run the resident owner loop (auto-answer redeems, pairing-flow.md §4).
  final bool ownerLoop;

  /// How long the owner waits for a redeem attempt window, milliseconds.
  final int redeemWindowMs;

  /// Validate and return the JSON object; throws [AkSpecException].
  Map<String, dynamic> toJson() {
    if (registry.isEmpty || registry == 'ip:port') {
      throw const AkSpecException('字段 "registry" 必须是 ip:port 地址');
    }
    if (!_isIpPort(registry)) {
      throw AkSpecException('字段 "registry" 不是 ip:port 地址："$registry"');
    }
    // Registry pin is the raw 32-byte Ed25519 public key = 64 hex chars
    // (matching the Rust engine's decode_hex_into in ak-ffi/src/pairing.rs).
    if (RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(registryIdHex) == false) {
      throw const AkSpecException('字段 "registry_id" 必须是 64 个十六进制字符（32 字节公钥）');
    }
    if (idDir.isEmpty) {
      throw const AkSpecException('字段 "id_dir" 不能为空');
    }
    final name = deviceName;
    if (name != null && name.runes.length > 64) {
      throw const AkSpecException('字段 "device_name" 至多 64 个字符');
    }
    if (redeemWindowMs < 1000 || redeemWindowMs > 119000) {
      throw const AkSpecException('字段 "redeem_window_ms" 必须在 1000..=119000 之间');
    }
    return <String, dynamic>{
      'spec_version': 1,
      'registry': registry,
      'registry_id': registryIdHex.toLowerCase(),
      'id_dir': idDir,
      if (name != null && name.isNotEmpty) 'device_name': name,
      'owner_loop': ownerLoop,
      'redeem_window_ms': redeemWindowMs,
    };
  }

  /// Compact spec JSON for `ak_pairing_start`.
  String toJsonString() => jsonEncode(toJson());

  static final RegExp _ipv4Octet =
      RegExp(r'^(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])$');
  static final RegExp _digits = RegExp(r'^[0-9]+$');
  static final RegExp _v6Body = RegExp(r'^[0-9A-Fa-f:.%]+$');

  /// Mirror of the engine's `SocketAddr` parse: IPv4 dotted quad or a
  /// bracketed IPv6 literal, hostname-free (mirrors `ffi_json.dart`).
  static bool _isIpPort(String text) {
    bool portOk(String port) {
      if (port.isEmpty || port.length > 5 || !_digits.hasMatch(port)) {
        return false;
      }
      return int.parse(port) <= 65535;
    }

    if (text.startsWith('[')) {
      final close = text.indexOf(']');
      if (close < 2 || close + 1 >= text.length || text[close + 1] != ':') {
        return false;
      }
      final host = text.substring(1, close);
      return _v6Body.hasMatch(host) && portOk(text.substring(close + 2));
    }
    final colon = text.lastIndexOf(':');
    if (colon <= 0) {
      return false;
    }
    final octets = text.substring(0, colon).split('.');
    if (octets.length != 4 || !octets.every(_ipv4Octet.hasMatch)) {
      return false;
    }
    return portOk(text.substring(colon + 1));
  }
}

/// Builder for the `ak_pairing_create_start` spec (engine shape):
///
/// ```json
/// {"spec_version":1,"request_id":"<32hex>","ttl_secs":0}
/// ```
///
/// `request_id` is OPTIONAL on the wire (the engine mints one when
/// absent); sending one keeps the Dart side the idempotency source.
class AkInviteCreateSpec {
  const AkInviteCreateSpec({this.requestId, this.ttlSecs = 0});

  final String? requestId;

  /// `0` = long-lived; finite TTLs are 60..=2_592_000 seconds
  /// (60 s..30 d, pairing-flow.md §8).
  final int ttlSecs;

  Map<String, dynamic> toJson() {
    final rid = requestId;
    if (rid != null) {
      _checkRequestId(rid);
    }
    if (ttlSecs != 0 && (ttlSecs < 60 || ttlSecs > 30 * 24 * 3600)) {
      throw const AkSpecException(
          '字段 "ttl_secs" 为 0（长期）或 60..=2592000（60 秒至 30 天）');
    }
    return <String, dynamic>{
      'spec_version': 1,
      if (rid != null) 'request_id': rid.toLowerCase(),
      'ttl_secs': ttlSecs,
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_pairing_redeem_start` spec (`invite_code` carries
/// the AKI1 code; `request_id` optional engine-side).
class AkInviteRedeemSpec {
  const AkInviteRedeemSpec({this.requestId, required this.inviteCode});

  final String? requestId;

  /// The invitation code; validated and normalized to the canonical
  /// grouped `AKI1-XXXX-...` form before the call (pairing-flow.md §1).
  final String inviteCode;

  Map<String, dynamic> toJson() {
    final rid = requestId;
    if (rid != null) {
      _checkRequestId(rid);
    }
    return <String, dynamic>{
      'spec_version': 1,
      if (rid != null) 'request_id': rid.toLowerCase(),
      'invite_code': normalizeInviteCode(inviteCode),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_pairing_list_start` query — the engine's paged
/// form (`cursor` hex16, `limit` 1..=32 default 32, `state_filter`
/// `active|revoked|expired|all`; metadata rows only, never secrets).
class AkInviteListQuery {
  const AkInviteListQuery({
    this.requestId,
    this.stateFilter = InviteListStateFilter.active,
    this.limit = 32,
    this.cursor,
  });

  final String? requestId;

  /// Which rows to list — `all` keeps revoked/expired tombstones visible
  /// (the owner sees the real states, pairing-flow.md §11).
  final InviteListStateFilter stateFilter;

  final int limit;

  /// Pagination cursor from a previous page's `next_cursor` (hex32).
  final String? cursor;

  Map<String, dynamic> toJson() {
    final rid = requestId;
    if (rid != null) {
      _checkRequestId(rid);
    }
    if (limit < 1 || limit > 32) {
      throw const AkSpecException('字段 "limit" 必须在 1..=32 之间');
    }
    final pageCursor = cursor;
    if (pageCursor != null &&
        RegExp(r'^[0-9a-fA-F]{32}$').hasMatch(pageCursor) == false) {
      throw const AkSpecException('字段 "cursor" 必须是 32 个十六进制字符');
    }
    return <String, dynamic>{
      'spec_version': 1,
      if (rid != null) 'request_id': rid.toLowerCase(),
      'state_filter': stateFilter.wireName,
      'limit': limit,
      if (pageCursor != null && pageCursor.isNotEmpty)
        'cursor': pageCursor.toLowerCase(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// The `state_filter` values of a list query.
enum InviteListStateFilter {
  active,
  revoked,
  expired,
  all;

  String get wireName => name;
}

/// Builder for the `ak_pairing_revoke_start` spec (CLI `pair revoke
/// CODE-ID`: waits for server confirm or reports pending-sync).
class AkInviteRevokeSpec {
  const AkInviteRevokeSpec({this.requestId, required this.codeId});

  final String? requestId;

  /// Registry id of the code, 32 hex characters.
  final String codeId;

  Map<String, dynamic> toJson() {
    final rid = requestId;
    if (rid != null) {
      _checkRequestId(rid);
    }
    if (!_hex32.hasMatch(codeId)) {
      throw const AkSpecException('字段 "code_id" 必须是 32 个十六进制字符');
    }
    return <String, dynamic>{
      'spec_version': 1,
      if (rid != null) 'request_id': rid.toLowerCase(),
      'code_id': codeId.toLowerCase(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

// ---------------------------------------------------------------------------
// The pointer-free call seam (fake-drivable; see the library doc).
// ---------------------------------------------------------------------------

/// One `ak_pairing_*` call site, expressed without pointers: string specs
/// in, [Uint8List] id-out buffers / caller buffers, and the raw `int32`
/// status back ([AkStatus]). The buffer forms model `buf`/`cap` so fakes
/// can exercise the `-3` grow-retry loops faithfully: a `-3` must leave
/// the buffer untouched and consume nothing.
abstract interface class AkPairingCalls {
  /// `ak_pairing_start`: 0 accepted (16-byte runtime id written), negative
  /// on error.
  int start(String specJson, Uint8List runtimeIdOut);

  /// `ak_pairing_create_start`: 0 accepted (op id written).
  int createStart(Uint8List runtimeId, String specJson, Uint8List opIdOut);

  /// `ak_pairing_redeem_start`: 0 accepted (op id written).
  int redeemStart(Uint8List runtimeId, String specJson, Uint8List opIdOut);

  /// `ak_pairing_list_start`: 0 accepted (op id written).
  int listStart(Uint8List runtimeId, String queryJson, Uint8List opIdOut);

  /// `ak_pairing_revoke_start`: 0 accepted (op id written).
  int revokeStart(Uint8List runtimeId, String specJson, Uint8List opIdOut);

  /// `ak_pairing_op_result`: 0 pending, 1 done (JSON written), 2 failed
  /// (error JSON written), -3 too small (nothing consumed), -4 unknown op.
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap);

  /// `ak_pairing_poll_event`: 1 event JSON written, 0 queue empty, -3 too
  /// small (event stays queued).
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap);

  /// `ak_pairing_cancel_op`: 0 ok, -4 unknown op.
  int cancelOp(Uint8List runtimeId, Uint8List opId);

  /// `ak_pairing_stop_start`: 0 accepted (op id written).
  int stopStart(Uint8List runtimeId, Uint8List opIdOut);
}

// ---------------------------------------------------------------------------
// dart:ffi bindings (the only layer touching pointers).
// ---------------------------------------------------------------------------

typedef _IdInOutC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _IdInOutDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _SpecStartC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);
typedef _SpecStartDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);

typedef _IdBufC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _IdBufDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _EventBufC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _EventBufDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _TwoIdsC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _TwoIdsDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

/// The `ak_pairing_*` exports the CURRENT wave-22 engine ships
/// (crates/ak-ffi/src/shim.rs) — the probe set of
/// [AkPairingFfiBindings.missingPairingExports].
const List<String> akPairingExportNames = <String>[
  'ak_pairing_start',
  'ak_pairing_create_start',
  'ak_pairing_redeem_start',
  'ak_pairing_list_start',
  'ak_pairing_revoke_start',
  'ak_pairing_op_result',
  'ak_pairing_poll_event',
  'ak_pairing_cancel_op',
];

/// Contract exports not yet shipped by the engine (`ak_pairing_stop_start`
/// of ffi-v2.md §1): resolved lazily; calling [PairingRuntimeFfi.stop]
/// throws the explicit unsupported error until they land.
const List<String> akPairingPendingExportNames = <String>[
  'ak_pairing_stop_start',
];

/// [AkPairingCalls] over one `DynamicLibrary` and a BORROWED engine handle
/// (ffi-v2.md §2: the shared facade borrows the handle and never
/// double-disposes — this class owns no native resource).
///
/// The constructor resolves all nine exports; a library predating wave 22
/// makes `DynamicLibrary.lookup` throw `ArgumentError` — callers surface
/// that as the explicit "old DLL unsupported" case (ffi-v2.md §2), e.g.
/// via [missingPairingExports] first.
final class AkPairingFfiBindings implements AkPairingCalls {
  AkPairingFfiBindings(
      {required Pointer<AkEngine> engine, required DynamicLibrary library})
      : _engine = engine,
        _library = library {
    // ak_pairing_start(engine, spec_json, runtime_id_out) — the same
    // three-pointer shape as the *_stop_start family.
    _start =
        _library.lookupFunction<_IdInOutC, _IdInOutDart>('ak_pairing_start');
    _createStart = _library.lookupFunction<_SpecStartC, _SpecStartDart>(
        'ak_pairing_create_start');
    _redeemStart = _library.lookupFunction<_SpecStartC, _SpecStartDart>(
        'ak_pairing_redeem_start');
    _listStart = _library.lookupFunction<_SpecStartC, _SpecStartDart>(
        'ak_pairing_list_start');
    _revokeStart = _library.lookupFunction<_SpecStartC, _SpecStartDart>(
        'ak_pairing_revoke_start');
    _opResult = _library
        .lookupFunction<_IdBufC, _IdBufDart>('ak_pairing_op_result');
    _pollEvent = _library
        .lookupFunction<_EventBufC, _EventBufDart>('ak_pairing_poll_event');
    _cancelOp = _library
        .lookupFunction<_TwoIdsC, _TwoIdsDart>('ak_pairing_cancel_op');
    // Not yet shipped by the engine (akPairingPendingExportNames);
    // resolved opportunistically so the class stays usable against the
    // current DLL.
    _stopStart = _tryStopStart();
  }

  final Pointer<AkEngine> _engine;
  final DynamicLibrary _library;

  late final _IdInOutDart _start;
  late final _SpecStartDart _createStart;
  late final _SpecStartDart _redeemStart;
  late final _SpecStartDart _listStart;
  late final _SpecStartDart _revokeStart;
  late final _IdBufDart _opResult;
  late final _EventBufDart _pollEvent;
  late final _TwoIdsDart _cancelOp;
  _IdInOutDart? _stopStart;

  _IdInOutDart? _tryStopStart() {
    try {
      return _library
          .lookupFunction<_IdInOutC, _IdInOutDart>('ak_pairing_stop_start');
    } on ArgumentError {
      return null;
    }
  }

  /// Probes [library] for every `ak_pairing_*` export and returns the
  /// MISSING symbol names (empty list = fully supported). Use this to
  /// implement the "old DLL explicitly unsupported" rule without catching
  /// loader exceptions.
  static List<String> missingPairingExports(DynamicLibrary library) => [
        for (final name in akPairingExportNames)
          if (!_hasSymbol(library, name)) name,
      ];

  static bool _hasSymbol(DynamicLibrary library, String name) {
    try {
      library.lookup(name);
      return true;
    } on ArgumentError {
      return false;
    }
  }

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    final spec = toNativeUtf8(specJson);
    final idOut = processAllocator.allocate<Uint8>(akPairingIdLength);
    try {
      final code = _start(_engine, spec, idOut);
      if (code == AkStatus.ok) {
        runtimeIdOut.setAll(0, idOut.asTypedList(akPairingIdLength));
      }
      return code;
    } finally {
      processAllocator.free(idOut);
      processAllocator.free(spec);
    }
  }

  @override
  int createStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _specStart(_createStart, runtimeId, specJson, opIdOut);

  @override
  int redeemStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _specStart(_redeemStart, runtimeId, specJson, opIdOut);

  @override
  int listStart(
          Uint8List runtimeId, String queryJson, Uint8List opIdOut) =>
      _specStart(_listStart, runtimeId, queryJson, opIdOut);

  @override
  int revokeStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _specStart(_revokeStart, runtimeId, specJson, opIdOut);

  int _specStart(_SpecStartDart invoke, Uint8List runtimeId, String specJson,
      Uint8List opIdOut) {
    final id = _idIn(runtimeId);
    final spec = toNativeUtf8(specJson);
    final opOut = processAllocator.allocate<Uint8>(akPairingIdLength);
    try {
      final code = invoke(_engine, id, spec, opOut);
      if (code == AkStatus.ok) {
        opIdOut.setAll(0, opOut.asTypedList(akPairingIdLength));
      }
      return code;
    } finally {
      processAllocator.free(opOut);
      processAllocator.free(spec);
      processAllocator.free(id);
    }
  }

  @override
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    final runtime = _idIn(runtimeId);
    final op = _idIn(opId);
    try {
      return _bufCall(
          (buf, capacity) => _opResult(_engine, runtime, op, buf, capacity),
          buffer,
          cap);
    } finally {
      processAllocator.free(op);
      processAllocator.free(runtime);
    }
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    final runtime = _idIn(runtimeId);
    try {
      return _bufCall(
          (buf, capacity) => _pollEvent(_engine, runtime, buf, capacity),
          buffer,
          cap);
    } finally {
      processAllocator.free(runtime);
    }
  }

  @override
  int cancelOp(Uint8List runtimeId, Uint8List opId) {
    final runtime = _idIn(runtimeId);
    final op = _idIn(opId);
    try {
      return _cancelOp(_engine, runtime, op);
    } finally {
      processAllocator.free(op);
      processAllocator.free(runtime);
    }
  }

  @override
  int stopStart(Uint8List runtimeId, Uint8List opIdOut) {
    final invoke = _stopStart;
    if (invoke == null) {
      throw UnsupportedError(
          '当前 ak_ffi 运行时尚未导出 ak_pairing_stop_start'
          '（见 akPairingPendingExportNames）');
    }
    final runtime = _idIn(runtimeId);
    final opOut = processAllocator.allocate<Uint8>(akPairingIdLength);
    try {
      final code = invoke(_engine, runtime, opOut);
      if (code == AkStatus.ok) {
        opIdOut.setAll(0, opOut.asTypedList(akPairingIdLength));
      }
      return code;
    } finally {
      processAllocator.free(opOut);
      processAllocator.free(runtime);
    }
  }

  Pointer<Uint8> _idIn(Uint8List id) {
    if (id.length != akPairingIdLength) {
      throw ArgumentError.value(id.length, 'id.length',
          'wave-22 ids are exactly $akPairingIdLength bytes');
    }
    final pointer = processAllocator.allocate<Uint8>(akPairingIdLength);
    for (var i = 0; i < akPairingIdLength; i++) {
      pointer[i] = id[i];
    }
    return pointer;
  }

  /// Runs one buffer call: on a positive code copies the NUL-terminated
  /// payload into [buffer] (bounded by [cap]); `-3` and the other negative
  /// codes leave it untouched (nothing consumed).
  int _bufCall(int Function(Pointer<Uint8>, int) invoke, Uint8List buffer,
      int cap) {
    final buf = processAllocator.allocate<Uint8>(cap);
    try {
      final code = invoke(buf, cap);
      if (code > 0) {
        var length = 0;
        while (length < cap && buf[length] != 0) {
          length++;
        }
        buffer.setAll(0, buf.asTypedList(length));
      }
      return code;
    } finally {
      processAllocator.free(buf);
    }
  }
}

// ---------------------------------------------------------------------------
// Friendly layer: op handles, runtime, typed events.
// ---------------------------------------------------------------------------

/// Which `*_start` produced an op.
enum PairingOpKind { create, redeem, list, revoke, stop }

/// One snapshot of an `ak_pairing_op_result` poll (status codes 0/1/2 of
/// ffi-v2.md §3; the buffer grow happens inside [PairingOpHandle.poll]).
final class PairingOpResult {
  const PairingOpResult._(this.state, this.map);

  /// `'pending'`, `'done'` or `'failed'`.
  final String state;

  /// Decoded payload JSON for terminal states; null while pending.
  final Map<String, dynamic>? map;

  const PairingOpResult.pending() : this._('pending', null);

  factory PairingOpResult.done(Map<String, dynamic> map) =>
      PairingOpResult._('done', map);

  factory PairingOpResult.failed(Map<String, dynamic> map) =>
      PairingOpResult._('failed', map);

  bool get isPending => state == 'pending';
  bool get isDone => state == 'done';
  bool get isFailed => state == 'failed';

  /// The `error` string of a failed result (null otherwise).
  String? get error => isFailed ? map!['error'] as String? : null;
}

/// A handle to one accepted asynchronous pairing operation: the op id plus
/// the polling loop over `ak_pairing_op_result` with automatic `-3`
/// buffer growth (lossless: `-3` consumes nothing and re-reads the SAME
/// stored result).
final class PairingOpHandle {
  PairingOpHandle._(
      this._calls, this.runtimeId, this.opId, this.kind, this.symbol);

  final AkPairingCalls _calls;

  /// The runtime this op belongs to.
  final Uint8List runtimeId;

  /// The 16-byte op id.
  final Uint8List opId;

  /// Hex form of [opId] (32 chars) — the correlation key for logs/UI.
  late final String opIdHex = akIdToHex(opId);

  /// Which start call produced this op.
  final PairingOpKind kind;

  /// The C symbol that accepted the op (diagnostics on errors).
  final String symbol;

  /// One poll: `AK_PENDING` → pending, `AK_DONE/AK_EVENT` → decoded JSON,
  /// `AK_FAILED` → failed JSON, `-3` grows the buffer and retries
  /// (nothing was consumed), other negatives throw [AkFfiException]
  /// carrying the last error.
  PairingOpResult poll() {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = _calls.opResult(runtimeId, opId, buffer, capacity);
      switch (code) {
        case AkStatus.pending:
          return const PairingOpResult.pending();
        case AkStatus.done:
          return PairingOpResult.done(
              decodeFfiJson(fromNativeUtf8Text(buffer)));
        case AkStatus.failed:
          return PairingOpResult.failed(
              decodeFfiJson(fromNativeUtf8Text(buffer)));
        case AkStatus.errBuffer:
          capacity *= 2; // nothing consumed; retry losslessly
          continue;
        default:
          throw AkFfiException(
              code, '$symbol 结果轮询失败（${AkStatus.name(code)}）');
      }
    }
    throw StateError('$symbol 结果超过 $_maxBufferBytes 字节');
  }

  /// Polls until the op settles and returns the done JSON map; a failed op
  /// throws [AkFfiException] carrying the engine error. [delay] is
  /// injectable so tests drive the loop without real timers.
  Future<Map<String, dynamic>> wait({
    Duration interval = const Duration(milliseconds: 200),
    Duration timeout = const Duration(seconds: 30),
    Future<void> Function(Duration)? delay,
  }) async {
    final wait = delay ?? Future<void>.delayed;
    final deadline = DateTime.now().add(timeout);
    while (true) {
      final result = poll();
      if (result.isDone) {
        return result.map!;
      }
      if (result.isFailed) {
        throw AkFfiException(
            AkStatus.failed, result.error ?? '$symbol 操作失败');
      }
      if (DateTime.now().isAfter(deadline)) {
        throw TimeoutException('$symbol 等待结果超时', timeout);
      }
      await wait(interval);
    }
  }
}

/// Entry point: starts (or re-attaches to) the pairing runtime.
final class PairingFfi {
  /// Creates the pairing service over [calls].
  PairingFfi({required AkPairingCalls calls}) : _calls = calls;

  final AkPairingCalls _calls;

  /// `ak_pairing_start`: accepts the runtime (NOT online yet — that is the
  /// `service_online` event) and returns the bound runtime facade.
  ///
  /// Throws [AkFfiException] on a negative status (the last-error message
  /// rides along when the bindings layer captured one).
  PairingRuntimeFfi start(AkPairingStartSpec spec) {
    final runtimeId = Uint8List(akPairingIdLength);
    final code = _calls.start(spec.toJsonString(), runtimeId);
    if (code != AkStatus.ok) {
      throw AkFfiException(
          code, 'ak_pairing_start 未受理（${AkStatus.name(code)}）');
    }
    return PairingRuntimeFfi._(_calls, runtimeId);
  }
}

/// One live pairing runtime (`runtime_id`-bound view of the call seam).
final class PairingRuntimeFfi {
  PairingRuntimeFfi._(this._calls, this.runtimeId);

  final AkPairingCalls _calls;

  /// The 16-byte runtime id issued by `ak_pairing_start`.
  final Uint8List runtimeId;

  /// Hex form of [runtimeId].
  late final String runtimeIdHex = akIdToHex(runtimeId);

  /// Queues code creation (idempotent per [requestId]; a null id is
  /// omitted and the engine mints one).
  PairingOpHandle createInvite({int ttlSecs = 0, String? requestId}) {
    final spec = AkInviteCreateSpec(requestId: requestId, ttlSecs: ttlSecs);
    return _startOp(PairingOpKind.create, 'ak_pairing_create_start',
        (opOut) => _calls.createStart(runtimeId, spec.toJsonString(), opOut));
  }

  /// Queues redeeming [inviteCode] (validated/normalized first).
  PairingOpHandle redeemInvite(String inviteCode, {String? requestId}) {
    final spec =
        AkInviteRedeemSpec(requestId: requestId, inviteCode: inviteCode);
    return _startOp(PairingOpKind.redeem, 'ak_pairing_redeem_start',
        (opOut) => _calls.redeemStart(runtimeId, spec.toJsonString(), opOut));
  }

  /// Queues listing the invite registry rows (paged; tombstones when
  /// [stateFilter] is [InviteListStateFilter.all]).
  PairingOpHandle listInvites(
      {InviteListStateFilter stateFilter = InviteListStateFilter.active,
      int limit = 32,
      String? cursor,
      String? requestId}) {
    final query = AkInviteListQuery(
        requestId: requestId,
        stateFilter: stateFilter,
        limit: limit,
        cursor: cursor);
    return _startOp(PairingOpKind.list, 'ak_pairing_list_start',
        (opOut) => _calls.listStart(runtimeId, query.toJsonString(), opOut));
  }

  /// Queues revoking [codeId].
  PairingOpHandle revokeInvite(String codeId, {String? requestId}) {
    final spec = AkInviteRevokeSpec(requestId: requestId, codeId: codeId);
    return _startOp(PairingOpKind.revoke, 'ak_pairing_revoke_start',
        (opOut) => _calls.revokeStart(runtimeId, spec.toJsonString(), opOut));
  }

  /// Pops the oldest pending pairing event, typed (null when the queue is
  /// empty). Never feed these lines to the transfer progress parser.
  PairingEvent? pollEvent() {
    final map = _pollJson('ak_pairing_poll_event',
        (buffer, cap) => _calls.pollEvent(runtimeId, buffer, cap));
    return map == null ? null : PairingEvent.fromJson(map);
  }

  /// Cancels a still-pending op (no-op semantics owned by the engine).
  void cancelOp(PairingOpHandle op) {
    final code = _calls.cancelOp(runtimeId, op.opId);
    if (code != AkStatus.ok) {
      throw AkFfiException(
          code, 'ak_pairing_cancel_op 失败（${AkStatus.name(code)}）');
    }
  }

  /// `ak_pairing_stop_start`: accepts shutdown; completion via the
  /// returned op's polling.
  PairingOpHandle stop() => _startOp(PairingOpKind.stop,
      'ak_pairing_stop_start', (opOut) => _calls.stopStart(runtimeId, opOut));

  PairingOpHandle _startOp(PairingOpKind kind, String symbol,
      int Function(Uint8List opOut) invoke) {
    final opOut = Uint8List(akPairingIdLength);
    final code = invoke(opOut);
    if (code != AkStatus.ok) {
      throw AkFfiException(
          code, '$symbol 未受理（${AkStatus.name(code)}）');
    }
    return PairingOpHandle._(_calls, runtimeId, opOut, kind, symbol);
  }

  /// One buffered poll turned into a decoded map: `1` decodes, `0` is
  /// null, `-3` grows (nothing consumed), other negatives throw.
  Map<String, dynamic>? _pollJson(
      String symbol, int Function(Uint8List buffer, int cap) invoke) {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = invoke(buffer, capacity);
      if (code == AkStatus.pending) {
        return null;
      }
      if (code == AkStatus.event) {
        return decodeFfiJson(fromNativeUtf8Text(buffer));
      }
      if (code == AkStatus.errBuffer) {
        capacity *= 2; // event stays queued; retry losslessly
        continue;
      }
      throw AkFfiException(code, '$symbol 失败（${AkStatus.name(code)}）');
    }
    throw StateError('$symbol 事件超过 $_maxBufferBytes 字节');
  }
}

/// Decodes the NUL-terminated prefix of [bytes] as UTF-8 text (the buffer
/// forms of the seam hand back raw byte arrays, unlike the pointer-based
/// `fromNativeUtf8`).
String fromNativeUtf8Text(Uint8List bytes) {
  var length = 0;
  while (length < bytes.length && bytes[length] != 0) {
    length++;
  }
  return utf8.decode(bytes.take(length).toList(growable: false));
}
