/// dart:ffi layer over the eight `ak_task_*` exports of the wave-28
/// minimal-pairing task plane (signatures in the task-plane table of
/// `docs/contracts/ffi-v2.md`):
///
/// ```text
/// ak_task_start(engine, spec_json, runtime_id_out[16])
/// ak_task_create_code(engine, runtime_id, role_and_cands_json, op_id_out[16])
/// ak_task_join(engine, runtime_id, code_and_cands_json, op_id_out[16])
/// ak_task_send(engine, runtime_id, conn_id[16], paths_json, op_id_out[16])
/// ak_task_list(engine, runtime_id, buf, cap)
/// ak_task_disconnect(engine, runtime_id, task_id[16])
/// ak_task_set_save_dir(engine, runtime_id, path)
/// ak_task_consent(engine, runtime_id, job_id[16], accept)
/// ```
///
/// Op RESULTS and EVENTS add no symbols: they flow through the existing
/// `ak_pairing_op_result` / `ak_pairing_poll_event` exports with the task
/// runtime id in place of a pairing runtime id (the frozen wave-28
/// channel-sharing ruling), so this seam carries those two calls too.
///
/// Layering mirrors `pairing_ffi.dart` (testable without the DLL):
///
/// ```text
/// spec builders (pure, mirrored validation, AkSpecException)
///        │ toJsonString()
/// ▼
/// TaskFfi / TaskRuntimeFfi / TaskOpHandle   (friendly layer: id
///        │                marshaling, -3 auto-grow, typed events)
///        ▼
/// AkTaskCalls (pointer-free per-call seam; fakes drive every loop)
///        ▲ implemented by
/// AkTaskFfiBindings (dart:ffi: pointers + the frozen signatures)
/// ```
///
/// Contract notes baked in (ffi-v2.md §2/§3, task-plane table):
/// * `start` returning `AK_OK` means accepted; the `runtime_ready` event
///   follows in the task event queue.
/// * `-3 AK_ERR_BUFFER` consumes nothing: every buffer grow-retry loop
///   here is lossless by construction.
/// * Ops are idempotent per `request_id` (same id + same spec → the SAME
///   op id and answer; same id + different spec → `-2`).
/// * One native engine handle is owned by ONE Dart isolate; calls on it
///   are serialized (the Dart event loop does this for free).
/// * C07 (crash-analysis 2026-11-07): the borrowing layer carries the
///   OWNER's liveness probe. [AkTaskFfiBindings] takes an optional
///   [AkTaskLeaseProbe] and checks it at the TOP of every exported call,
///   throwing a clear [StateError] instead of touching an engine handle
///   the owner already freed. [FakeAkTaskCalls] has no owner and stays
///   probe-free; [AkTaskLeaseGate] is the same check as a composable
///   wrapper over any [AkTaskCalls] (the lease tests drive it with the
///   fake — no DLL needed).
library;

import 'dart:async' show TimeoutException;
import 'dart:convert' show jsonEncode;
import 'dart:ffi';
import 'dart:typed_data' show Uint8List;

import 'package:ffi/ffi.dart' show Arena, using;

import 'bindings.dart' show AkEngine, AkFfiException, AkStatus, processAllocator;
import 'ffi_json.dart' show AkSpecException, decodeFfiJson, toNativeUtf8;
import 'model/task_events.dart';
import 'pairing_ffi.dart'
    show
        EngineLastErrorC,
        EngineLastErrorDart,
        akBufferCall,
        akCallFailure,
        akIdFromHex,
        akIdToHex,
        akPairingIdLength,
        fromNativeUtf8Text,
        isIpPortAddress,
        readEngineLastError;

/// Defensive ceiling for the automatic `-3` grow-retry loops.
const int _maxBufferBytes = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Spec builders (pure Dart; mirrored validation).
// ---------------------------------------------------------------------------

final RegExp _hex32 = RegExp(r'^[0-9a-fA-F]{32}$');
final RegExp _hex64 = RegExp(r'^[0-9a-fA-F]{64}$');

/// The role picked when GENERATING a task code (the joiner receives the
/// opposite one).
enum AkTaskRole {
  sender,
  receiver;

  String get wireName => name;
}

void _checkRequestId(String requestId) {
  if (!_hex32.hasMatch(requestId)) {
    throw const AkSpecException('字段 "request_id" 必须是 32 个十六进制字符');
  }
}

void _checkRequestIdOptional(String? requestId) {
  if (requestId != null) {
    _checkRequestId(requestId);
  }
}

/// Builder for the `ak_task_start` spec — the task runtime's
/// configuration. Registry fields are the invite plane's (the caller
/// resolves `AK_REGISTRY_ADDR` / `AK_REGISTRY_ID` from the environment or
/// the compiled `--dart-define` defaults, exactly like
/// `FfiTransferService.inviteConfigFromEnvironment`); `save_dir` defaults
/// engine-side to `<id_dir>/downloads`.
class AkTaskStartSpec {
  const AkTaskStartSpec({
    required this.registry,
    required this.registryIdHex,
    required this.idDir,
    this.deviceName,
    this.saveDir,
    this.stunAddr,
  });

  /// Coordination registry endpoint as a literal `ip:port`.
  final String registry;

  /// Pinned registry identity: 64 hex (the raw 32-byte Ed25519 key).
  final String registryIdHex;

  /// Identity directory (`device.key`, `checkpoints/`).
  final String idDir;

  /// Device name advertised to peers (≤64 chars).
  final String? deviceName;

  /// The one default save folder; null = engine default `<id_dir>/downloads`.
  final String? saveDir;

  /// Optional STUN server (`ip:port`) for the receiver-side reflexive
  /// candidate (CR-P1-02 C2): the Rust side queries it ON the QUIC socket
  /// before binding (same-socket NAT mapping). null/empty = no query,
  /// fail-open; must be `ip:port` when present (ffi-v2.md §8.1).
  final String? stunAddr;

  Map<String, dynamic> toJson() {
    if (!isIpPortAddress(registry)) {
      throw AkSpecException('字段 "registry" 不是 ip:port 地址："$registry"');
    }
    if (!_hex64.hasMatch(registryIdHex)) {
      throw const AkSpecException('字段 "registry_id" 必须是 64 个十六进制字符（32 字节公钥）');
    }
    if (idDir.isEmpty) {
      throw const AkSpecException('字段 "id_dir" 不能为空');
    }
    final name = deviceName;
    if (name != null && name.runes.length > 64) {
      throw const AkSpecException('字段 "device_name" 至多 64 个字符');
    }
    final folder = saveDir;
    if (folder != null && folder.isEmpty) {
      throw const AkSpecException('字段 "save_dir" 为空或省略');
    }
    final stun = stunAddr;
    if (stun != null && stun.isNotEmpty && !isIpPortAddress(stun)) {
      throw AkSpecException('字段 "stun_addr" 不是 ip:port 地址："$stun"');
    }
    return <String, dynamic>{
      'spec_version': 1,
      'registry': registry,
      'registry_id': registryIdHex.toLowerCase(),
      'id_dir': idDir,
      if (name != null && name.isNotEmpty) 'device_name': name,
      if (folder != null && folder.isNotEmpty) 'save_dir': folder,
      if (stun != null && stun.isNotEmpty) 'stun_addr': stun,
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_task_create_code` spec:
///
/// ```json
/// {"spec_version":1,"role":"sender|receiver",
///  "candidates":["ip:port",...],"ttl_secs":0,"request_id":"<32hex>"}
/// ```
class AkTaskCreateCodeSpec {
  const AkTaskCreateCodeSpec({
    required this.role,
    this.candidates = const <String>[],
    this.ttlSecs = 0,
    this.requestId,
  });

  /// The role THIS device plays for the code.
  final AkTaskRole role;

  /// Extra dial candidates the caller knows (LAN addresses first is the
  /// engine's ordering rule; a receiver-role creation derives its real
  /// bound endpoints itself).
  final List<String> candidates;

  /// `0` = no expiry; finite TTLs span 60 s..30 d.
  final int ttlSecs;

  /// Optional 32-hex idempotency key (the engine mints one when absent).
  final String? requestId;

  Map<String, dynamic> toJson() {
    _checkRequestIdOptional(requestId);
    if (ttlSecs != 0 && (ttlSecs < 60 || ttlSecs > 30 * 24 * 3600)) {
      throw const AkSpecException('字段 "ttl_secs" 为 0（长期）或 60..=2592000');
    }
    for (final candidate in candidates) {
      if (!isIpPortAddress(candidate)) {
        throw AkSpecException('字段 "candidates" 含非 ip:port 项："$candidate"');
      }
    }
    return <String, dynamic>{
      'spec_version': 1,
      'role': role.wireName,
      if (candidates.isNotEmpty) 'candidates': candidates,
      'ttl_secs': ttlSecs,
      if (requestId != null) 'request_id': requestId!.toLowerCase(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_task_join` spec:
///
/// ```json
/// {"spec_version":1,"code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX",
///  "role":"sender|receiver"(optional),"candidates":["ip:port",...],
///  "request_id":"<32hex>"}
/// ```
///
/// The code is the REAL display form the Rust side mints
/// (`InviteCode::to_display`: `AKI1-` + eight groups of four Crockford
/// Base32 symbols — the engine's `normalize_code_key` tolerates case,
/// hyphens and spaces, so any user-typed grouping passes through here).
class AkTaskJoinSpec {
  const AkTaskJoinSpec({
    required this.code,
    this.role,
    this.candidates = const <String>[],
    this.requestId,
  });

  /// The task code as displayed to the user (`AKI1-…`, case-insensitive).
  final String code;

  /// The joiner's EXPLICIT role (`"sender"`/`"receiver"`) — the UI user's
  /// declared intent for this pairing (wave-29 P1-05 alignment: the hint
  /// rides the join spec so cross-device joins never guess the direction).
  /// Null keeps the engine's resolution: same-engine mirror-complement,
  /// else the v1 receiver default.
  final AkTaskRole? role;

  final List<String> candidates;

  final String? requestId;

  Map<String, dynamic> toJson() {
    _checkRequestIdOptional(requestId);
    final trimmed = code.trim();
    if (trimmed.isEmpty) {
      throw const AkSpecException('字段 "code" 不能为空');
    }
    for (final candidate in candidates) {
      if (!isIpPortAddress(candidate)) {
        throw AkSpecException('字段 "candidates" 含非 ip:port 项："$candidate"');
      }
    }
    return <String, dynamic>{
      'spec_version': 1,
      'code': trimmed.toUpperCase(),
      if (role != null) 'role': role!.wireName,
      if (candidates.isNotEmpty) 'candidates': candidates,
      if (requestId != null) 'request_id': requestId!.toLowerCase(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_task_send` paths JSON:
///
/// ```json
/// {"spec_version":1,"paths":["<本地路径>",...],"request_id":"<32hex>"}
/// ```
class AkTaskSendSpec {
  AkTaskSendSpec({required List<String> paths, this.requestId})
      : paths = List.unmodifiable(paths);

  final List<String> paths;

  final String? requestId;

  Map<String, dynamic> toJson() {
    _checkRequestIdOptional(requestId);
    if (paths.isEmpty) {
      throw const AkSpecException('字段 "paths" 不能为空');
    }
    if (paths.any((path) => path.isEmpty)) {
      throw const AkSpecException('字段 "paths" 的每一项都必须是非空路径');
    }
    return <String, dynamic>{
      'spec_version': 1,
      'paths': paths,
      if (requestId != null) 'request_id': requestId!.toLowerCase(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

// ---------------------------------------------------------------------------
// The pointer-free call seam (fake-drivable).
// ---------------------------------------------------------------------------

/// One `ak_task_*` call site plus the two SHARED channel calls, expressed
/// without pointers: string specs in, [Uint8List] id-out buffers / caller
/// buffers, and the raw `int32` status back ([AkStatus]). The buffer forms
/// model `buf`/`cap` so fakes can exercise the `-3` grow-retry loops
/// faithfully: a `-3` must leave the buffer untouched and consume nothing.
abstract interface class AkTaskCalls {
  /// `ak_task_start`: 0 accepted (16-byte runtime id written).
  int start(String specJson, Uint8List runtimeIdOut);

  /// `ak_task_create_code`: 0 accepted (op id written).
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut);

  /// `ak_task_join`: 0 accepted (op id written).
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut);

  /// `ak_task_send`: 0 accepted (op id written).
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson, Uint8List opIdOut);

  /// `ak_task_list`: 1 snapshot JSON written, 0 never, -3 too small
  /// (nothing consumed).
  int list(Uint8List runtimeId, Uint8List buffer, int cap);

  /// `ak_task_disconnect`: 0 ok (idempotent), -4 unknown connection.
  int disconnect(Uint8List runtimeId, Uint8List connId);

  /// `ak_task_set_save_dir`: 0 ok, -2 unusable path.
  int setSaveDir(Uint8List runtimeId, String path);

  /// `ak_task_consent`: 0 decided, -4 unknown/already-decided request.
  /// `accept` uses the C-truthy form (0 = decline).
  int consent(Uint8List runtimeId, Uint8List jobId, int accept);

  /// `ak_pairing_op_result` with the TASK runtime id: 0 pending, 1 done,
  /// 2 failed, -3 too small (nothing consumed), -4 unknown op.
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap);

  /// `ak_pairing_poll_event` with the TASK runtime id: 1 event JSON
  /// written, 0 queue empty, -3 too small (event stays queued).
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap);

  /// `ak_poll_progress` (the ENGINE-WIDE progress queue — no runtime id):
  /// 1 progress JSON line written, 0 queue empty, -3 too small (the line
  /// stays queued). Task-plane rows — send AND receive alike — key their
  /// progress by the connection id (ffi-v2 §8.4's wave-31 revision; a
  /// joiner receiver never knew the code id) — the wave-30 P2-10
  /// progress-renewal source.
  int pollProgress(Uint8List buffer, int cap);

  /// The native error detail of the most recent negative status, or null
  /// (read from `ak_engine_last_error` right after the failing call).
  String? lastError();
}

// ---------------------------------------------------------------------------
// dart:ffi bindings (the only layer touching pointers).
// ---------------------------------------------------------------------------

typedef _StartC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _StartDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _OpStartC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);
typedef _OpStartDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);

typedef _SendStartC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);
typedef _SendStartDart = int Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>);

typedef _ListBufC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _ListBufDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

/// `ak_pairing_op_result(engine, runtime_id, op_id, buf, cap)` — the
/// shared op-result channel read with a TASK runtime id.
typedef _OpBufC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _OpBufDart = int Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, int);

typedef _IdIdC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _IdIdDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _IdStrC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _IdStrDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _ConsentC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Int32);
typedef _ConsentDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _EventBufC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _EventBufDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

/// `ak_poll_progress(engine, buf, cap)` — the engine-wide progress queue
/// (no runtime id; wave-30 P2-10 progress renewal reads it).
typedef _ProgressBufC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, IntPtr);
typedef _ProgressBufDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, int);

/// The eight `ak_task_*` exports plus the two shared-channel exports the
/// task plane reads (probe set of [AkTaskFfiBindings.missingTaskExports]).
const List<String> akTaskExportNames = <String>[
  'ak_task_start',
  'ak_task_create_code',
  'ak_task_join',
  'ak_task_send',
  'ak_task_list',
  'ak_task_disconnect',
  'ak_task_set_save_dir',
  'ak_task_consent',
  'ak_pairing_op_result',
  'ak_pairing_poll_event',
];

// ---------------------------------------------------------------------------
// C07: the owner/lease liveness probe (crash-analysis 2026-11-07).
// ---------------------------------------------------------------------------

/// C07 lease probe: the OWNER's liveness view of the engine handle this
/// layer borrows — `true` while the handle's owner (the facade that ran
/// `ak_engine_new`) still holds the engine alive. The owner flips it
/// `false` in its dispose BEFORE the native `engine_free` runs, so a
/// borrower racing the release gets the explicit [StateError] below
/// instead of a dangling native call.
typedef AkTaskLeaseProbe = bool Function();

/// The [StateError] every lease-checked call throws once the probe
/// reports the owner released the engine (C07).
StateError taskLeaseRevokedError() => StateError(
    '任务平面绑定已失效：引擎句柄的持有者已释放原生引擎（C07 lease）。'
    '请通过服务重建恢复。');

/// The one-line lease check shared by [AkTaskFfiBindings] and
/// [AkTaskLeaseGate]: a null probe (no owner — the fake, or a bindings
/// instance constructed bare) never refuses.
void _ensureTaskLease(AkTaskLeaseProbe? probe) {
  if (probe != null && !probe()) {
    throw taskLeaseRevokedError();
  }
}

/// C07: the lease check as a composable [AkTaskCalls] wrapper — every
/// exported call checks [isAlive] FIRST and throws [StateError] when the
/// owner already released the engine, WITHOUT touching [inner]. Wrap the
/// real bindings under an owner, or a fake in tests (the fake itself has
/// no owner and is never affected).
final class AkTaskLeaseGate implements AkTaskCalls {
  AkTaskLeaseGate(this.inner, {required this.isAlive});

  /// The wrapped seam (FFI bindings or a test fake).
  final AkTaskCalls inner;

  /// The owner's liveness probe of the borrowed engine handle.
  final AkTaskLeaseProbe isAlive;

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    _ensureTaskLease(isAlive);
    return inner.start(specJson, runtimeIdOut);
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    _ensureTaskLease(isAlive);
    return inner.createCode(runtimeId, specJson, opIdOut);
  }

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    _ensureTaskLease(isAlive);
    return inner.join(runtimeId, specJson, opIdOut);
  }

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson,
      Uint8List opIdOut) {
    _ensureTaskLease(isAlive);
    return inner.send(runtimeId, connId, pathsJson, opIdOut);
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    _ensureTaskLease(isAlive);
    return inner.list(runtimeId, buffer, cap);
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) {
    _ensureTaskLease(isAlive);
    return inner.disconnect(runtimeId, connId);
  }

  @override
  int setSaveDir(Uint8List runtimeId, String path) {
    _ensureTaskLease(isAlive);
    return inner.setSaveDir(runtimeId, path);
  }

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) {
    _ensureTaskLease(isAlive);
    return inner.consent(runtimeId, jobId, accept);
  }

  @override
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    _ensureTaskLease(isAlive);
    return inner.opResult(runtimeId, opId, buffer, cap);
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    _ensureTaskLease(isAlive);
    return inner.pollEvent(runtimeId, buffer, cap);
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    _ensureTaskLease(isAlive);
    return inner.pollProgress(buffer, cap);
  }

  /// Passes through unchanged: it reads the inner seam's CACHED Dart
  /// detail (never a native engine call), exactly like the E4 gate.
  @override
  String? lastError() => inner.lastError();
}

/// [AkTaskCalls] over one `DynamicLibrary` and a BORROWED engine handle
/// (ffi-v2.md §2: the shared facade borrows the handle and never
/// double-disposes — this class owns no native resource).
///
/// C07: [isAlive] is the OWNER's liveness probe of [engine]. When given,
/// every exported call checks it FIRST and throws the explicit
/// [StateError] of [taskLeaseRevokedError] instead of touching a handle
/// the owner already freed — the owner (see `AkEngineDartFacade`) flips
/// the probe false BEFORE the native `engine_free`. Omit it for ownerless
/// use (the seam then behaves exactly as before).
///
/// The constructor resolves all ten symbols; a library predating wave 28
/// makes `DynamicLibrary.lookup` throw `ArgumentError` — callers surface
/// that as the explicit "old DLL unsupported" case via
/// [missingTaskExports] first.
final class AkTaskFfiBindings implements AkTaskCalls {
  AkTaskFfiBindings(
      {required Pointer<AkEngine> engine,
      required DynamicLibrary library,
      AkTaskLeaseProbe? isAlive})
      : _engine = engine,
        _library = library,
        _isAlive = isAlive {
    _start = _library.lookupFunction<_StartC, _StartDart>('ak_task_start');
    _createCode =
        _library.lookupFunction<_OpStartC, _OpStartDart>('ak_task_create_code');
    _join = _library.lookupFunction<_OpStartC, _OpStartDart>('ak_task_join');
    _send = _library.lookupFunction<_SendStartC, _SendStartDart>('ak_task_send');
    _list = _library.lookupFunction<_ListBufC, _ListBufDart>('ak_task_list');
    _disconnect =
        _library.lookupFunction<_IdIdC, _IdIdDart>('ak_task_disconnect');
    _setSaveDir =
        _library.lookupFunction<_IdStrC, _IdStrDart>('ak_task_set_save_dir');
    _consent = _library.lookupFunction<_ConsentC, _ConsentDart>('ak_task_consent');
    _opResult = _library
        .lookupFunction<_OpBufC, _OpBufDart>('ak_pairing_op_result');
    _pollEvent = _library
        .lookupFunction<_EventBufC, _EventBufDart>('ak_pairing_poll_event');
    _pollProgress =
        _library.lookupFunction<_ProgressBufC, _ProgressBufDart>('ak_poll_progress');
    _engineLastError = _library.lookupFunction<EngineLastErrorC,
        EngineLastErrorDart>('ak_engine_last_error');
  }

  final Pointer<AkEngine> _engine;
  final DynamicLibrary _library;

  /// C07: the owner's liveness probe (null = ownerless, never refuses).
  final AkTaskLeaseProbe? _isAlive;

  late final _StartDart _start;
  late final _OpStartDart _createCode;
  late final _OpStartDart _join;
  late final _SendStartDart _send;
  late final _ListBufDart _list;
  late final _IdIdDart _disconnect;
  late final _IdStrDart _setSaveDir;
  late final _ConsentDart _consent;
  late final _OpBufDart _opResult;
  late final _EventBufDart _pollEvent;
  late final _ProgressBufDart _pollProgress;
  late final EngineLastErrorDart _engineLastError;

  /// The native detail captured right after the most recent failing call
  /// (null when the last call did not fail or carried no text).
  String? _lastErrorDetail;

  /// Captures the thread-local `ak_engine_last_error` IMMEDIATELY after a
  /// negative return; `-3` is excluded (the lossless grow loop retries).
  int _note(int code) {
    if (code < 0 && code != AkStatus.errBuffer) {
      _lastErrorDetail = readEngineLastError(_engineLastError, _engine);
    }
    return code;
  }

  @override
  String? lastError() => _lastErrorDetail;

  /// Probes [library] for every task-plane symbol and returns the MISSING
  /// names (empty list = fully supported) — the "old DLL explicitly
  /// unsupported" rule without catching loader exceptions.
  static List<String> missingTaskExports(DynamicLibrary library) => [
        for (final name in akTaskExportNames)
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
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
        final spec = toNativeUtf8(specJson, allocator: arena);
        final idOut = arena<Uint8>(akPairingIdLength);
        final code = _note(_start(_engine, spec, idOut));
        if (code == AkStatus.ok) {
          runtimeIdOut.setAll(0, idOut.asTypedList(akPairingIdLength));
        }
        return code;
      }, processAllocator);
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _opStart(_createCode, runtimeId, specJson, opIdOut);

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _opStart(_join, runtimeId, specJson, opIdOut);

  int _opStart(_OpStartDart invoke, Uint8List runtimeId, String specJson,
          Uint8List opIdOut) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
        final id = _idIn(runtimeId, arena);
        final spec = toNativeUtf8(specJson, allocator: arena);
        final opOut = arena<Uint8>(akPairingIdLength);
        final code = _note(invoke(_engine, id, spec, opOut));
        if (code == AkStatus.ok) {
          opIdOut.setAll(0, opOut.asTypedList(akPairingIdLength));
        }
        return code;
      }, processAllocator);
  }

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson,
          Uint8List opIdOut) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
        final runtime = _idIn(runtimeId, arena);
        final conn = _idIn(connId, arena);
        final paths = toNativeUtf8(pathsJson, allocator: arena);
        final opOut = arena<Uint8>(akPairingIdLength);
        final code = _note(_send(_engine, runtime, conn, paths, opOut));
        if (code == AkStatus.ok) {
          opIdOut.setAll(0, opOut.asTypedList(akPairingIdLength));
        }
        return code;
      }, processAllocator);
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      return _bufCall(
          (buf, capacity) => _list(_engine, runtime, buf, capacity), buffer, cap);
    }, processAllocator);
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      final conn = _idIn(connId, arena);
      return _note(_disconnect(_engine, runtime, conn));
    }, processAllocator);
  }

  @override
  int setSaveDir(Uint8List runtimeId, String path) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      final native = toNativeUtf8(path, allocator: arena);
      return _note(_setSaveDir(_engine, runtime, native));
    }, processAllocator);
  }

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      final job = _idIn(jobId, arena);
      return _note(_consent(_engine, runtime, job, accept));
    }, processAllocator);
  }

  @override
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      final op = _idIn(opId, arena);
      return _bufCall(
          (buf, capacity) => _opResult(_engine, runtime, op, buf, capacity),
          buffer,
          cap);
    }, processAllocator);
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return using((arena) {
      final runtime = _idIn(runtimeId, arena);
      return _bufCall(
          (buf, capacity) => _pollEvent(_engine, runtime, buf, capacity),
          buffer,
          cap);
    }, processAllocator);
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    _ensureTaskLease(_isAlive); // C07: never touch a freed handle
    return _bufCall(
        (buf, capacity) => _pollProgress(_engine, buf, capacity), buffer, cap);
  }

  /// Copies one validated 16-byte id into a fresh native buffer owned by
  /// [arena] (C14: a later allocation failure inside the same call site
  /// can no longer leak an earlier id buffer).
  Pointer<Uint8> _idIn(Uint8List id, Arena arena) {
    if (id.length != akPairingIdLength) {
      throw ArgumentError.value(id.length, 'id.length',
          'wave-28 ids are exactly $akPairingIdLength bytes');
    }
    final pointer = arena<Uint8>(akPairingIdLength);
    for (var i = 0; i < akPairingIdLength; i++) {
      pointer[i] = id[i];
    }
    return pointer;
  }

  /// Runs one buffer call via the shared C06 contract ([akBufferCall]);
  /// `_note` still captures the last-error detail of a negative status.
  int _bufCall(int Function(Pointer<Uint8>, int) invoke, Uint8List buffer,
          int cap) =>
      akBufferCall(
          (buf, capacity) => _note(invoke(buf, capacity)), buffer, cap);
}

// ---------------------------------------------------------------------------
// Friendly layer: op handles, runtime, typed views.
// ---------------------------------------------------------------------------

/// Which `*_start` produced a task op.
enum TaskOpKind { createCode, join, send }

/// One snapshot of an `ak_pairing_op_result` poll over a task op.
final class TaskOpResult {
  const TaskOpResult._(this.state, this.map);

  /// `'pending'`, `'done'` or `'failed'`.
  final String state;

  /// Decoded payload JSON for terminal states; null while pending.
  final Map<String, dynamic>? map;

  const TaskOpResult.pending() : this._('pending', null);

  factory TaskOpResult.done(Map<String, dynamic> map) => TaskOpResult._('done', map);

  factory TaskOpResult.failed(Map<String, dynamic> map) => TaskOpResult._('failed', map);

  bool get isPending => state == 'pending';
  bool get isDone => state == 'done';
  bool get isFailed => state == 'failed';

  /// The `error` string of a failed result (null otherwise).
  String? get error => isFailed ? map!['error'] as String? : null;
}

/// A handle to one accepted asynchronous task operation: the op id plus
/// the polling loop with automatic `-3` buffer growth (lossless).
final class TaskOpHandle {
  TaskOpHandle._(this._calls, this.runtimeId, this.opId, this.kind, this.symbol);

  final AkTaskCalls _calls;

  /// The task runtime this op belongs to.
  final Uint8List runtimeId;

  /// The 16-byte op id.
  final Uint8List opId;

  /// Hex form of [opId] (32 chars).
  late final String opIdHex = akIdToHex(opId);

  /// Which start call produced this op.
  final TaskOpKind kind;

  /// The C symbol that accepted the op (diagnostics on errors).
  final String symbol;

  /// One poll: `AK_PENDING` → pending, `AK_DONE` → decoded JSON,
  /// `AK_FAILED` → failed JSON, `-3` grows the buffer and retries
  /// (nothing consumed), other negatives throw [AkFfiException] carrying
  /// the native last-error detail.
  TaskOpResult poll() {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = _calls.opResult(runtimeId, opId, buffer, capacity);
      switch (code) {
        case AkStatus.pending:
          return const TaskOpResult.pending();
        case AkStatus.done:
          return TaskOpResult.done(decodeFfiJson(fromNativeUtf8Text(buffer)));
        case AkStatus.failed:
          return TaskOpResult.failed(decodeFfiJson(fromNativeUtf8Text(buffer)));
        case AkStatus.errBuffer:
          capacity *= 2; // nothing consumed; retry losslessly
          continue;
        default:
          throw akCallFailure(code, '$symbol 结果轮询失败', _calls.lastError);
      }
    }
    throw StateError('$symbol 结果超过 $_maxBufferBytes 字节');
  }

  /// Polls until the op settles and returns the done JSON map; a failed op
  /// throws [AkFfiException]. [ensureLive] is the P1-05 lifecycle hook: it
  /// runs at the TOP of every loop iteration, BEFORE the next native call.
  ///
  /// [onPoll] (wave-30 P2-10) turns [timeout] into a STALL budget instead
  /// of an overall wall-clock cap: it runs after every pending poll, and
  /// when it reports progress (`true`) the deadline is REFRESHED to
  /// `now + timeout`. Only a true no-progress stall of [timeout] elapses
  /// into the [TimeoutException] — a long transfer that keeps making
  /// progress is never cut off by the total duration.
  Future<Map<String, dynamic>> wait({
    Duration interval = const Duration(milliseconds: 200),
    Duration timeout = const Duration(seconds: 30),
    Future<void> Function(Duration)? delay,
    void Function()? ensureLive,
    bool Function()? onPoll,
  }) async {
    final wait = delay ?? Future<void>.delayed;
    var deadline = DateTime.now().add(timeout);
    while (true) {
      ensureLive?.call(); // disposed owner: abort BEFORE any native call
      final result = poll();
      if (result.isDone) {
        return result.map!;
      }
      if (result.isFailed) {
        throw AkFfiException(
            AkStatus.failed, result.error ?? '$symbol 操作失败');
      }
      if (onPoll != null && onPoll()) {
        deadline = DateTime.now().add(timeout); // progress renews the budget
      }
      if (DateTime.now().isAfter(deadline)) {
        throw TimeoutException('$symbol 等待结果超时（无进展停滞）', timeout);
      }
      await wait(interval);
    }
  }
}

/// One connection row of the `ak_task_list` snapshot.
final class TaskConnectionView {
  const TaskConnectionView({
    required this.connId,
    required this.role,
    required this.state,
    this.peerEndpoint,
    this.code,
  });

  /// Hex connection id (32 chars).
  final String connId;

  /// `sender` | `receiver`.
  final String role;

  /// `probing` | `connected` | `disconnected`.
  final String state;

  final String? peerEndpoint;

  final String? code;

  factory TaskConnectionView.fromJson(Map<String, dynamic> json) =>
      TaskConnectionView(
        connId: json['conn'] as String? ?? '',
        role: json['role'] as String? ?? '',
        state: json['state'] as String? ?? '',
        peerEndpoint: json['peer_endpoint'] as String?,
        code: json['code'] as String?,
      );

  @override
  String toString() =>
      'TaskConnectionView($connId $role $state→$peerEndpoint code=$code)';
}

/// The `ak_task_list` snapshot: connections plus the save folder.
final class TaskListSnapshot {
  const TaskListSnapshot({required this.saveDir, required this.connections});

  final String saveDir;

  final List<TaskConnectionView> connections;

  factory TaskListSnapshot.fromJson(Map<String, dynamic> json) =>
      TaskListSnapshot(
        saveDir: json['save_dir'] as String? ?? '',
        connections: [
          for (final row in (json['connections'] as List? ?? const []))
            TaskConnectionView.fromJson((row as Map).cast<String, dynamic>()),
        ],
      );
}

/// Entry point: starts one task runtime.
final class TaskFfi {
  TaskFfi({required AkTaskCalls calls}) : _calls = calls;

  final AkTaskCalls _calls;

  /// `ak_task_start`: accepts the runtime and returns the bound facade.
  /// Throws [AkFfiException] on a negative status.
  TaskRuntimeFfi start(AkTaskStartSpec spec) {
    final runtimeId = Uint8List(akPairingIdLength);
    final code = _calls.start(spec.toJsonString(), runtimeId);
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_task_start 未受理', _calls.lastError);
    }
    return TaskRuntimeFfi._(_calls, runtimeId);
  }
}

/// One live task runtime (`runtime_id`-bound view of the call seam).
final class TaskRuntimeFfi {
  TaskRuntimeFfi._(this._calls, this.runtimeId);

  final AkTaskCalls _calls;

  /// The 16-byte runtime id issued by `ak_task_start`.
  final Uint8List runtimeId;

  /// Hex form of [runtimeId].
  late final String runtimeIdHex = akIdToHex(runtimeId);

  /// `ak_task_create_code` (idempotent per [requestId]).
  TaskOpHandle createCode(AkTaskCreateCodeSpec spec) => _startOp(
      TaskOpKind.createCode,
      'ak_task_create_code',
      (opOut) => _calls.createCode(runtimeId, spec.toJsonString(), opOut));

  /// `ak_task_join`.
  TaskOpHandle join(AkTaskJoinSpec spec) => _startOp(TaskOpKind.join,
      'ak_task_join', (opOut) => _calls.join(runtimeId, spec.toJsonString(), opOut));

  /// `ak_task_send` (paths array; candidate fallback engine-side).
  TaskOpHandle send(String connIdHex, AkTaskSendSpec spec) => _startOp(
      TaskOpKind.send,
      'ak_task_send',
      (opOut) => _calls.send(
          runtimeId, akIdFromHex(connIdHex), spec.toJsonString(), opOut));

  /// `ak_task_list` with automatic `-3` growth (lossless).
  TaskListSnapshot list() {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = _calls.list(runtimeId, buffer, capacity);
      if (code == AkStatus.done) {
        return TaskListSnapshot.fromJson(
            decodeFfiJson(fromNativeUtf8Text(buffer)));
      }
      if (code == AkStatus.errBuffer) {
        capacity *= 2; // nothing consumed; retry losslessly
        continue;
      }
      throw akCallFailure(code, 'ak_task_list 失败', _calls.lastError);
    }
    throw StateError('ak_task_list 结果超过 $_maxBufferBytes 字节');
  }

  /// `ak_task_disconnect` (idempotent).
  void disconnect(String connIdHex) {
    final code = _calls.disconnect(runtimeId, akIdFromHex(connIdHex));
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_task_disconnect 失败', _calls.lastError);
    }
  }

  /// `ak_task_set_save_dir` (the one default save folder).
  void setSaveDir(String path) {
    final code = _calls.setSaveDir(runtimeId, path);
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_task_set_save_dir 失败', _calls.lastError);
    }
  }

  /// `ak_task_consent`: decides one pending `incoming_request`. Returns
  /// false for an unknown or already-decided request (`-4`); other
  /// failures throw.
  bool consent(String jobIdHex, {required bool accept}) {
    final code =
        _calls.consent(runtimeId, akIdFromHex(jobIdHex), accept ? 1 : 0);
    if (code == AkStatus.ok) {
      return true;
    }
    if (code == AkStatus.errNotFound) {
      return false;
    }
    throw akCallFailure(code, 'ak_task_consent 失败', _calls.lastError);
  }

  /// Pops the oldest task-plane event, typed (null when the queue is
  /// empty). Reads the SHARED `ak_pairing_poll_event` export with this
  /// runtime's id.
  TaskEvent? pollEvent() {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = _calls.pollEvent(runtimeId, buffer, capacity);
      if (code == AkStatus.pending) {
        return null;
      }
      if (code == AkStatus.event) {
        return TaskEvent.fromJson(decodeFfiJson(fromNativeUtf8Text(buffer)));
      }
      if (code == AkStatus.errBuffer) {
        capacity *= 2; // event stays queued; retry losslessly
        continue;
      }
      throw akCallFailure(code, '任务事件轮询失败', _calls.lastError);
    }
    throw StateError('任务事件超过 $_maxBufferBytes 字节');
  }

  /// Pops the oldest ENGINE-WIDE progress line (no runtime scoping — the
  /// `ak_poll_progress` queue is one per engine; correlate by the line's
  /// `job` field: the connection id for sends, the code id for receives,
  /// ffi-v2 §8.4). Returns the raw JSON line string, or null when the
  /// queue is empty. The `-3` grow loop is lossless like every other
  /// buffer call. (Wave-30 P2-10: the progress-renewal source for
  /// stall-based timeouts.)
  String? pollProgressLine() {
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buffer = Uint8List(capacity);
      final code = _calls.pollProgress(buffer, capacity);
      if (code == AkStatus.pending) {
        return null;
      }
      if (code == AkStatus.event) {
        return fromNativeUtf8Text(buffer);
      }
      if (code == AkStatus.errBuffer) {
        capacity *= 2; // line stays queued; retry losslessly
        continue;
      }
      throw akCallFailure(code, '进度事件轮询失败', _calls.lastError);
    }
    throw StateError('进度事件超过 $_maxBufferBytes 字节');
  }

  TaskOpHandle _startOp(
      TaskOpKind kind, String symbol, int Function(Uint8List opOut) invoke) {
    final opOut = Uint8List(akPairingIdLength);
    final code = invoke(opOut);
    if (code != AkStatus.ok) {
      throw akCallFailure(code, '$symbol 未受理', _calls.lastError);
    }
    return TaskOpHandle._(_calls, runtimeId, opOut, kind, symbol);
  }
}
