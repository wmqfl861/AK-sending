/// dart:ffi layer over the `ak_sched_*` exports of the wave-22 engine +
/// the scheduler spec builders.
///
/// WIRE ABI — the SHAPES THE ENGINE ACTUALLY SHIPS (crates/ak-ffi/src/
/// shim.rs; engine-scoped, no `runtime_id`/op-id out-params, synchronous
/// status answers), which DIVERGE from the ffi-v2.md §1 draft (that draft
/// named `ak_sched_listen_start`/`ak_sched_retry`/`ak_sched_task_result`/
/// `ak_sched_shutdown_start` and runtime-scoped signatures — none shipped;
/// reconciling doc and code is the integrator's contract item):
///
/// ```c
/// int32_t ak_sched_start(AkEngine*, const char *spec_json);          // 0 ok
/// int32_t ak_sched_enqueue(AkEngine*, const char *spec_json,
///                          uint8_t task_id_out[16]);                 // 0 ok
/// int32_t ak_sched_set_policy(AkEngine*, const char *policy_json);   // 0 ok
/// int32_t ak_sched_cancel(AkEngine*, const uint8_t task_id[16]);     // 0 ok
/// int32_t ak_sched_snapshot(AkEngine*, char *buf, size_t cap);       // 1 json
/// int32_t ak_sched_poll_event(AkEngine*, char *buf, size_t cap);     // 1 json
/// ```
///
/// `ak_sched_snapshot` is FROZEN as the THREE-argument
/// `(engine, buf, cap)` form — no query parameter crosses the ABI (P0-01
/// ruling: the integrator's frozen contract; the five-argument
/// runtime-scoped draft of ffi-v2.md §1 never shipped). The Dart seam
/// keeps a `queryJson` parameter for shape compatibility, but ONLY the
/// default `'{}'` is accepted — anything else is an [ArgumentError]
/// (query filtering is a v3 extension).
///
/// Scheduling and the resident receiver share ONE runtime: the sched
/// state is bound engine-side to the pairing runtime (`ak_sched_start`
/// names it by `runtime_id`, or binds the engine's active one). The
/// snapshot carries the effective policy object (`same_code`/
/// `cross_code` keys — see `SchedulerPolicy.fromJson`).
///
/// Layering mirrors `pairing_ffi.dart`: spec builders (pure) →
/// [SchedRuntimeFfi] (friendly: `-3` auto-grow, typed events) →
/// [AkSchedCalls] (pointer-free seam; fake-drivable) →
/// [AkSchedFfiBindings] (dart:ffi).
library;

import 'dart:convert' show jsonEncode;
import 'dart:ffi';
import 'dart:typed_data' show Uint8List;

import 'bindings.dart' show AkEngine, AkStatus, processAllocator;
import 'ffi_json.dart' show AkSpecException, decodeFfiJson, toNativeUtf8;
import 'model/sched_events.dart';
import 'model/sched_policy.dart';
import 'pairing_ffi.dart'
    show
        EngineLastErrorC,
        EngineLastErrorDart,
        akCallFailure,
        akPairingIdLength,
        fromNativeUtf8Text,
        isIpPortAddress,
        newRequestId,
        readEngineLastError;

/// Defensive ceiling for the automatic `-3` grow-retry loops.
const int _maxBufferBytes = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Spec builders.
// ---------------------------------------------------------------------------

final RegExp _hex32 = RegExp(r'^[0-9a-fA-F]{32}$');

void _checkRequestId(String requestId) {
  if (!_hex32.hasMatch(requestId)) {
    throw const AkSpecException('字段 "request_id" 必须是 32 个十六进制字符');
  }
}

/// Builder for the `ak_sched_start` spec: bind the scheduler to a pairing
/// runtime and set the initial policy in one shot. The engine REQUIRES the
/// pairing runtime to be Ready (`runtime_ready` already polled) — start
/// the pairing side first.
class AkSchedStartSpec {
  const AkSchedStartSpec({
    required this.requestId,
    this.runtimeIdHex,
    this.policy = const SchedulerPolicy(),
  });

  final String requestId;

  /// Hex runtime id of the pairing runtime; null binds the engine's
  /// active one (still requires one to exist).
  final String? runtimeIdHex;

  /// The initial policy (mode pair + caps; `toJson` wire keys).
  final SchedulerPolicy policy;

  Map<String, dynamic> toJson() {
    _checkRequestId(requestId);
    final runtime = runtimeIdHex;
    if (runtime != null && !_hex32.hasMatch(runtime)) {
      throw const AkSpecException('字段 "runtime_id" 必须是 32 个十六进制字符');
    }
    return <String, dynamic>{
      'spec_version': 1,
      'request_id': requestId.toLowerCase(),
      if (runtime != null && runtime.isNotEmpty)
        'runtime_id': runtime.toLowerCase(),
      ...policy.toJson(),
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

/// Builder for the `ak_sched_enqueue` spec — one multi-root send task
/// through a Ready pairing relation (the frozen send shape of
/// ffi-v2.md §4; `code_id` and the verified peer IDENTITY resolve
/// engine-side from the saved `pairing_id` — the UI must never overwrite
/// them; P1-06: the engine ALSO requires an explicit `peer` endpoint
/// `ip:port` — the transport address of the paired receiver, a REQUIRED
/// field without which `ak_sched_enqueue` answers `AK_ERR_INVALID`
/// before anything is submitted):
///
/// ```json
/// {"spec_version":1,"request_id":"<32hex>","pairing_id":"<32hex>",
///  "peer":"192.168.1.20:7300",
///  "source_roots":["<本地路径>"],"conflict":"fail","multistream":false,
///  "no_internet":false,"timeout_secs":30}
/// ```
class AkSchedEnqueueSpec {
  AkSchedEnqueueSpec({
    required this.requestId,
    required this.pairingId,
    required this.peer,
    required List<String> sourceRoots,
    this.conflict = 'fail',
    this.multistream = false,
    this.noInternet = false,
    this.timeoutSecs = 30,
  }) : sourceRoots = List.unmodifiable(sourceRoots);

  final String requestId;

  /// The Ready pairing relation this task flows through, 32 hex chars.
  final String pairingId;

  /// REQUIRED (P1-06): the paired receiver's transport endpoint as a
  /// literal `ip:port` (IPv4 dotted quad or bracketed IPv6) — parsed
  /// engine-side by `SocketAddr` (`crates/ak-ffi/src/pairing.rs`,
  /// `sched_enqueue` rejects the spec without it). The pairing FINGERPRINT
  /// and code group still resolve from `pairing_id`; this is only the
  /// network address.
  final String peer;

  /// At least one local path (one task = one multi-root manifest; two
  /// independent tasks need two enqueues).
  final List<String> sourceRoots;

  /// `'fail'` (default) or `'rename'`.
  final String conflict;

  /// Per-task multistream — independent of cross-task concurrency
  /// (scheduler.md §4).
  final bool multistream;

  /// Refuse non-LAN/loopback paths.
  final bool noInternet;

  /// Idle budget seconds, 1..=86400.
  final int timeoutSecs;

  Map<String, dynamic> toJson() {
    _checkRequestId(requestId);
    if (!_hex32.hasMatch(pairingId)) {
      throw const AkSpecException('字段 "pairing_id" 必须是 32 个十六进制字符');
    }
    if (peer.isEmpty || peer == 'ip:port') {
      throw const AkSpecException('字段 "peer" 必须是 ip:port 地址');
    }
    if (!isIpPortAddress(peer)) {
      throw AkSpecException('字段 "peer" 不是 ip:port 地址："$peer"');
    }
    if (sourceRoots.isEmpty) {
      throw const AkSpecException('字段 "source_roots" 至少需要一个路径');
    }
    for (final root in sourceRoots) {
      if (root.isEmpty) {
        throw const AkSpecException('"source_roots" 的条目不能是空路径');
      }
    }
    if (conflict != 'fail' && conflict != 'rename') {
      throw const AkSpecException('字段 "conflict" 必须是 "fail" 或 "rename"');
    }
    if (timeoutSecs < 1 || timeoutSecs > 86400) {
      throw const AkSpecException('字段 "timeout_secs" 必须在 1..=86400 之间');
    }
    return <String, dynamic>{
      'spec_version': 1,
      'request_id': requestId.toLowerCase(),
      'pairing_id': pairingId.toLowerCase(),
      'peer': peer,
      'source_roots': sourceRoots,
      'conflict': conflict,
      'multistream': multistream,
      'no_internet': noInternet,
      'timeout_secs': timeoutSecs,
    };
  }

  String toJsonString() => jsonEncode(toJson());
}

// ---------------------------------------------------------------------------
// The pointer-free call seam (fake-drivable).
// ---------------------------------------------------------------------------

/// Enforces the FROZEN snapshot-query contract of the [AkSchedCalls]
/// seam (P0-01 ruling): the shipped export is the THREE-argument
/// `ak_sched_snapshot(engine, buf, cap)` — the query JSON never crosses
/// the ABI, so only the default `'{}'` is expressible. Anything else is an
/// [ArgumentError]; query filtering is a v3 EXTENSION, not part of the
/// frozen wave-22/25 contract. Both the real bindings and every fake call
/// this, so the rule is uniform across the seam.
void checkSchedSnapshotQuery(String queryJson) {
  if (queryJson != '{}') {
    throw ArgumentError.value(
        queryJson,
        'queryJson',
        'ak_sched_snapshot 冻结为 (engine, buf, cap) 三参形态：查询过滤是 v3 扩展，'
        '当前仅接受 "{}"');
  }
}

/// One `ak_sched_*` call site without pointers (the six shipped exports);
/// raw `int32` status back ([AkStatus]). Buffer forms model `buf`/`cap`;
/// a `-3` consumes nothing.
abstract interface class AkSchedCalls {
  /// `ak_sched_start(engine, spec_json)`: 0 accepted (the pairing runtime
  /// must already be Ready; a second start is -2).
  int start(String specJson);

  /// `ak_sched_enqueue(engine, spec_json, task_id_out)`: 0 accepted
  /// (16-byte task id written).
  int enqueue(String specJson, Uint8List taskIdOut);

  /// `ak_sched_set_policy(engine, policy_json)`: 0 applied, -2 bad
  /// policy, -4 no scheduler runtime.
  int setPolicy(String policyJson);

  /// `ak_sched_cancel(engine, task_id)`: 0 ok, -4 unknown/terminal task.
  int cancel(Uint8List taskId);

  /// `ak_sched_snapshot(engine, buf, cap)` — the frozen THREE-argument
  /// form (P0-01): 1 snapshot JSON written, 0 empty, -3 too small (retry
  /// losslessly). The Dart-side [queryJson] parameter is shape-compat
  /// ONLY: the engine takes no query, so implementations must accept
  /// exactly `'{}'` ([checkSchedSnapshotQuery] throws otherwise).
  int snapshot(String queryJson, Uint8List buffer, int cap);

  /// `ak_sched_poll_event(engine, buf, cap)`: 1 event JSON written, 0
  /// queue empty, -3 too small (event stays queued).
  int pollEvent(Uint8List buffer, int cap);

  /// The native error detail of the most recent negative status, or null
  /// (P2-08 — same contract as `AkPairingCalls.lastError`; the bindings
  /// layer reads the thread-local `ak_engine_last_error` immediately
  /// after the failing call, fakes return their scripted detail).
  String? lastError();
}

// ---------------------------------------------------------------------------
// dart:ffi bindings.
// ---------------------------------------------------------------------------

typedef _SpecOnlyC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>);
typedef _SpecOnlyDart = int Function(Pointer<AkEngine>, Pointer<Uint8>);

typedef _SpecIdOutC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _SpecIdOutDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _IdOnlyC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>);
typedef _IdOnlyDart = int Function(Pointer<AkEngine>, Pointer<Uint8>);

// P0-01: `ak_sched_snapshot` shares the (engine, buf, cap) shape of
// `ak_sched_poll_event` — the four-argument query form never shipped.
typedef _EventBufC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>, IntPtr);
typedef _EventBufDart = int Function(Pointer<AkEngine>, Pointer<Uint8>, int);

/// Every `ak_sched_*` export the engine ships, in shim.rs order.
const List<String> akSchedExportNames = <String>[
  'ak_sched_start',
  'ak_sched_enqueue',
  'ak_sched_set_policy',
  'ak_sched_cancel',
  'ak_sched_snapshot',
  'ak_sched_poll_event',
];

/// [AkSchedCalls] over one `DynamicLibrary` and a BORROWED engine handle
/// (same rules as `AkPairingFfiBindings`).
final class AkSchedFfiBindings implements AkSchedCalls {
  AkSchedFfiBindings(
      {required Pointer<AkEngine> engine, required DynamicLibrary library})
      : _engine = engine,
        _library = library {
    _start = _library
        .lookupFunction<_SpecOnlyC, _SpecOnlyDart>('ak_sched_start');
    _enqueue = _library
        .lookupFunction<_SpecIdOutC, _SpecIdOutDart>('ak_sched_enqueue');
    _setPolicy = _library
        .lookupFunction<_SpecOnlyC, _SpecOnlyDart>('ak_sched_set_policy');
    _cancel =
        _library.lookupFunction<_IdOnlyC, _IdOnlyDart>('ak_sched_cancel');
    // P0-01: the frozen THREE-argument (engine, buf, cap) snapshot export —
    // the four-argument query form of the ffi-v2.md §1 draft never shipped
    // (binding it made Rust treat the query pointer as the output buffer).
    _snapshot = _library
        .lookupFunction<_EventBufC, _EventBufDart>('ak_sched_snapshot');
    _pollEvent = _library
        .lookupFunction<_EventBufC, _EventBufDart>('ak_sched_poll_event');
    _engineLastError = _library.lookupFunction<EngineLastErrorC,
        EngineLastErrorDart>('ak_engine_last_error');
  }

  final Pointer<AkEngine> _engine;
  final DynamicLibrary _library;

  late final _SpecOnlyDart _start;
  late final _SpecIdOutDart _enqueue;
  late final _SpecOnlyDart _setPolicy;
  late final _IdOnlyDart _cancel;
  late final _EventBufDart _snapshot;
  late final _EventBufDart _pollEvent;
  late final EngineLastErrorDart _engineLastError;

  /// The native detail captured right after the most recent failing call
  /// (null when the last call did not fail or carried no text).
  String? _lastErrorDetail;

  /// Captures the thread-local `ak_engine_last_error` IMMEDIATELY after a
  /// negative return — before any later native call can overwrite it
  /// (P2-08). `-3` is excluded: the lossless grow loop retries.
  int _note(int code) {
    if (code < 0 && code != AkStatus.errBuffer) {
      _lastErrorDetail = readEngineLastError(_engineLastError, _engine);
    }
    return code;
  }

  @override
  String? lastError() => _lastErrorDetail;

  /// Missing `ak_sched_*` symbol names of [library] (empty = supported).
  static List<String> missingSchedExports(DynamicLibrary library) => [
        for (final name in akSchedExportNames)
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
  int start(String specJson) => _withString(specJson, _start);

  @override
  int setPolicy(String policyJson) => _withString(policyJson, _setPolicy);

  int _withString(String json, int Function(Pointer<AkEngine>, Pointer<Uint8>) invoke) {
    final text = toNativeUtf8(json);
    try {
      return _note(invoke(_engine, text));
    } finally {
      processAllocator.free(text);
    }
  }

  @override
  int enqueue(String specJson, Uint8List taskIdOut) {
    final spec = toNativeUtf8(specJson);
    final out = processAllocator.allocate<Uint8>(akPairingIdLength);
    try {
      final code = _note(_enqueue(_engine, spec, out));
      if (code == AkStatus.ok) {
        taskIdOut.setAll(0, out.asTypedList(akPairingIdLength));
      }
      return code;
    } finally {
      processAllocator.free(out);
      processAllocator.free(spec);
    }
  }

  @override
  int cancel(Uint8List taskId) {
    final task = _idIn(taskId);
    try {
      return _note(_cancel(_engine, task));
    } finally {
      processAllocator.free(task);
    }
  }

  @override
  int snapshot(String queryJson, Uint8List buffer, int cap) {
    // P0-01: the frozen ABI takes NO query — only the default '{}' is
    // expressible; the argument stays for Dart-side shape compatibility.
    checkSchedSnapshotQuery(queryJson);
    return _bufCall(
        (buf, capacity) => _snapshot(_engine, buf, capacity), buffer, cap);
  }

  @override
  int pollEvent(Uint8List buffer, int cap) => _bufCall(
      (buf, capacity) => _pollEvent(_engine, buf, capacity), buffer, cap);

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

  int _bufCall(int Function(Pointer<Uint8>, int) invoke, Uint8List buffer,
      int cap) {
    final buf = processAllocator.allocate<Uint8>(cap);
    try {
      final code = _note(invoke(buf, cap));
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
// Friendly layer.
// ---------------------------------------------------------------------------

/// The engine-scoped scheduler view (the six shipped calls; the pairing
/// runtime id rides inside the start spec).
final class SchedRuntimeFfi {
  /// Creates the scheduler facade over [calls].
  SchedRuntimeFfi({required AkSchedCalls calls})
      : _calls = calls;

  final AkSchedCalls _calls;

  /// `ak_sched_start`: binds the scheduler to the (Ready) pairing runtime
  /// [runtimeIdHex] — or the engine's active one when null — applying
  /// [policy] as the initial policy. A second start is -2; a not-yet-Ready
  /// runtime is -2 with the engine's "poll the runtime_ready event first".
  void start(SchedulerPolicy policy, {String? runtimeIdHex, String? requestId}) {
    final spec = AkSchedStartSpec(
      requestId: requestId ?? newRequestId(),
      runtimeIdHex: runtimeIdHex,
      policy: policy,
    );
    final code = _calls.start(spec.toJsonString());
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_sched_start 未受理', _calls.lastError);
    }
  }

  /// `ak_sched_enqueue`: accepts one multi-root task through a Ready
  /// pairing relation and returns its stable 16-byte `task_id`
  /// immediately (queue admission, no scan — scheduler.md §7).
  Uint8List enqueue(AkSchedEnqueueSpec spec) {
    final taskIdOut = Uint8List(akPairingIdLength);
    final code = _calls.enqueue(spec.toJsonString(), taskIdOut);
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_sched_enqueue 未受理', _calls.lastError);
    }
    return taskIdOut;
  }

  /// `ak_sched_set_policy`: applies [policy] synchronously (0 ok). The
  /// `policy_changed` event follows on [pollEvent]; the EFFECTIVE policy
  /// (both ends intersect, scheduler.md §2) is read back via [snapshot].
  void setPolicy(SchedulerPolicy policy) {
    final code = _calls.setPolicy(jsonEncode(policy.toJson()));
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_sched_set_policy 失败', _calls.lastError);
    }
  }

  /// `ak_sched_cancel`: terminal cancel of one task (checkpoint retained).
  void cancelTask(Uint8List taskId) {
    final code = _calls.cancel(taskId);
    if (code != AkStatus.ok) {
      throw akCallFailure(code, 'ak_sched_cancel 失败', _calls.lastError);
    }
  }

  /// `ak_sched_snapshot` — the frozen THREE-argument export (P0-01): the
  /// engine takes NO query, so only the default `'{}'` of [queryJson] is
  /// accepted (anything else throws [ArgumentError]; query filtering is a
  /// v3 extension). Returns the decoded snapshot JSON —
  /// `{"policy":{"same_code":...,"cross_code":...,...},
  /// "policy_revision":N,"draining":B,"active_tasks":N,"queued_tasks":N,
  /// "shutting_down":B,"tasks":[...]}` — or null when the engine answers
  /// empty. `-3` grows the buffer losslessly.
  Map<String, dynamic>? snapshot({String queryJson = '{}'}) {
    checkSchedSnapshotQuery(queryJson);
    return _pollJson('ak_sched_snapshot',
        (buffer, cap) => _calls.snapshot(queryJson, buffer, cap),
        initialCapacity: 1024);
  }

  /// `ak_sched_poll_event`: oldest pending scheduler event, typed (null
  /// when empty); a SEPARATE queue from both progress and pairing events.
  SchedulerEvent? pollEvent() =>
      _pollJson('ak_sched_poll_event',
              (buffer, cap) => _calls.pollEvent(buffer, cap))
          ?.let(SchedulerEvent.fromJson);

  Map<String, dynamic>? _pollJson(
      String symbol, int Function(Uint8List buffer, int cap) invoke,
      {int initialCapacity = 256}) {
    var capacity = initialCapacity;
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
        capacity *= 2; // nothing consumed; retry losslessly
        continue;
      }
      throw akCallFailure(code, '$symbol 失败', _calls.lastError);
    }
    throw StateError('$symbol 超过 $_maxBufferBytes 字节');
  }
}

extension _Let<T> on T {
  R let<R>(R Function(T) transform) => transform(this);
}
