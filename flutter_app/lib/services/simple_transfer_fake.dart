/// Deterministic in-memory [SimpleTransferService] for wave-28 UI
/// development and widget tests (agent C's real `SimpleTransferService`
/// replaces it at the single switch point in `lib/main.dart`).
///
/// No FFI, no real network, no timers: every async member completes in a
/// microtask, and every multi-step transition (probe, transfer) is driven
/// EXPLICITLY by the test through the public knobs below, so widget tests
/// assert each UI state deterministically:
///
/// ```text
/// create-flow:  createCode(role) ─▶ simulatePeerJoin() ─▶ beginProbe()
///               ─▶ completeProbe()          (or autoProbe: joins land connected)
/// join-flow:    joinCode(code)   ─▶ beginProbe()/completeProbe() on the new id
/// receive:      emitIncomingRequest(...) ─▶ consent(jobId,true) ─▶
///               finishIncoming(jobId) / failIncoming(jobId, reason)
/// send:         sendFiles(...)            ─▶ completeSend(text)/failSend(reason)
/// teardown:     disconnect(id) / simulateNetworkChange() (drops everything)
/// ```
///
/// Spec rules the fake honors: codes are one-time (consumed by a join or
/// killed by their connection dropping); connections are one-way
/// (the frozen [TaskRole]); several connections coexist; the default
/// save dir is a plain field.
library;

import 'dart:async'
    show Completer, StreamController, scheduleMicrotask;

import '../ui/service/simple_transfer_service.dart';

/// The wave-28 fake [SimpleTransferService].
class LocalSimpleTransferService implements SimpleTransferService {
  LocalSimpleTransferService({
    String initialSaveDir = 'Download/AK-接收',
    this.remotePeerName = 'DESKTOP-PEER',
    this.autoProbe = false,
  }) : _saveDir = initialSaveDir;

  /// Peer name used for connections the fake creates
  /// ([simulatePeerJoin] overrides per call).
  String remotePeerName;

  /// When true, every new connection walks `connecting → testing →
  /// connected` over microtasks without explicit knobs (a
  /// `pumpAndSettle` then lands on 已连接). Default false: tests drive
  /// each probe state explicitly.
  bool autoProbe;

  int startCalls = 0;
  int disposeCalls = 0;
  int createCodeCalls = 0;
  int joinCodeCalls = 0;
  int consentCalls = 0;
  int sendFilesCalls = 0;
  int disconnectCalls = 0;
  int setSaveDirCalls = 0;

  /// Every code minted by [createCode], oldest first.
  final List<TaskCode> createdCodes = [];

  /// Arguments of every [joinCode] call, in order.
  final List<String> joinRequests = [];

  /// Optional role arguments of every [joinCode] call (null = not given),
  /// in the same order as [joinRequests].
  final List<TaskRole?> joinRoleRequests = [];

  /// `(jobId, accept)` pairs in call order.
  final List<(String, bool)> consentRequests = [];

  /// `(connId, paths)` pairs in call order.
  final List<(String, List<String>)> sendRequests = [];

  /// connId arguments of [disconnect] calls, in order.
  final List<String> disconnectRequests = [];

  /// Paths accepted by [setSaveDir], in order.
  final List<String> saveDirRequests = [];

  String? _joinError;
  String _saveDir;

  TaskCode? _pendingCode;
  int _codeSeq = 0;
  int _connSeq = 0;
  int _jobSeq = 0;

  final List<TaskConnection> _connections = [];
  final StreamController<List<TaskConnection>> _connectionsController =
      StreamController<List<TaskConnection>>.broadcast();

  final StreamController<IncomingRequest> _incomingController =
      StreamController<IncomingRequest>.broadcast();

  /// Wave-29 P2-11: job ids whose consent window closed (driven
  /// explicitly by tests via [expireIncoming] — no timers in the fake).
  final StreamController<String> _expiredIncomingController =
      StreamController<String>.broadcast();

  /// Job ids the fake reported expired (assertion material).
  final List<String> expiredJobs = [];

  /// Job ids already decided through [consent] (an expire after a
  /// decision is ignored, like the engine's window).
  final Set<String> _decidedJobs = {};

  /// Completers of accepted-but-unfinished receives, by jobId.
  final Map<String, Completer<void>> _pendingReceives = {};

  /// Completers of started-but-unfinished sends, in call order.
  final List<Completer<String>> _pendingSends = [];

  /// Live connection rows (defensive copy).
  List<TaskConnection> get connectionsSnapshot => List.of(_connections);

  /// The not-yet-consumed created code, if any.
  TaskCode? get pendingCode => _pendingCode;

  // ---------------------------------------------------------------------
  // Contract members.
  // ---------------------------------------------------------------------

  @override
  Future<void> start() async {
    startCalls += 1;
  }

  @override
  Future<TaskCode> createCode(TaskRole role) async {
    createCodeCalls += 1;
    _codeSeq += 1;
    // Real display shape (wave-29 P1-02): AKI1- + 8×4 Crockford Base32
    // symbols (I/L/O/U excluded), deterministic from the counter — the
    // first two symbols encode it so every minted code is distinct.
    const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
    String symbol(int index) => alphabet[(index + _codeSeq) % 32];
    final firstGroup = '${alphabet[_codeSeq % 32]}'
        '${alphabet[(_codeSeq ~/ 32) % 32]}${symbol(2)}${symbol(3)}';
    final groups = <String>[
      firstGroup,
      for (var group = 1; group < 8; group++)
        [for (var i = 0; i < 4; i++) symbol(group * 4 + i)].join(),
    ];
    final code = TaskCode(
      code: 'AKI1-${groups.join('-')}',
      role: role,
    );
    _pendingCode = code;
    createdCodes.add(code);
    return code;
  }

  @override
  Future<JoinResult> joinCode(String code, {TaskRole? role}) async {
    joinCodeCalls += 1;
    joinRequests.add(code);
    joinRoleRequests.add(role);
    final error = _joinError;
    if (error != null) {
      _joinError = null; // one-shot failure injection
      return JoinResult.failure(error);
    }
    final normalized = normalizeJoinCode(code);
    final pending = _pendingCode;
    if (pending == null || normalizeJoinCode(pending.code) != normalized) {
      return JoinResult.failure('配对码不存在或已失效，请向对方确认最新配对码。');
    }
    // The joiner's direction: the user's EXPLICIT intent (wave-29 P1-05)
    // when given, else the complement of the creator's declaration (the
    // creator of a "我要接收" code receives; we send).
    final connection = _consumePendingCode(
        peerName: remotePeerName, role: role ?? _opposite(pending.role));
    return JoinResult.success(connection.id);
  }

  @override
  Stream<List<TaskConnection>> get connections async* {
    // Replay the current snapshot to every subscriber (late subscribers —
    // e.g. a detail page opened later — see the live list immediately),
    // then forward live updates. Delivery is one microtask hop behind the
    // emission (standard async* stream semantics).
    yield List.of(_connections);
    yield* _connectionsController.stream;
  }

  @override
  Stream<IncomingRequest> get incomingRequests => _incomingController.stream;

  @override
  Stream<String> get expiredIncomingRequests =>
      _expiredIncomingController.stream;

  @override
  Future<void> consent(String jobId, bool accept) async {
    consentCalls += 1;
    consentRequests.add((jobId, accept));
    _decidedJobs.add(jobId);
    if (!accept) {
      return;
    }
    final completer = Completer<void>();
    _pendingReceives[jobId] = completer;
    return completer.future;
  }

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    sendFilesCalls += 1;
    sendRequests.add((connId, List.of(paths)));
    final completer = Completer<String>();
    _pendingSends.add(completer);
    return completer.future;
  }

  @override
  Future<void> disconnect(String connId) async {
    disconnectCalls += 1;
    disconnectRequests.add(connId);
    _dropConnection(connId);
  }

  @override
  Future<void> setSaveDir(String path) async {
    setSaveDirCalls += 1;
    saveDirRequests.add(path);
    _saveDir = path;
  }

  @override
  String get saveDir => _saveDir;

  @override
  void dispose() {
    disposeCalls += 1;
    _connections.clear();
    _connectionsController.close();
    _incomingController.close();
    _expiredIncomingController.close();
  }

  // ---------------------------------------------------------------------
  // Test knobs — pairing lifecycle.
  // ---------------------------------------------------------------------

  /// Makes the NEXT [joinCode] fail with [reason] (one-shot).
  set nextJoinError(String reason) => _joinError = reason;

  /// Simulates the remote joining my pending code (create-flow): consumes
  /// the code and adds a `connecting` connection with my declared role.
  /// Returns the new connection id.
  String simulatePeerJoin({String? peerName}) {
    final pending = _pendingCode;
    if (pending == null) {
      throw StateError('fake: no pending code to join (call createCode)');
    }
    final connection = _consumePendingCode(peerName: peerName ?? remotePeerName);
    return connection.id;
  }

  static TaskRole _opposite(TaskRole role) =>
      role == TaskRole.send ? TaskRole.receive : TaskRole.send;

  /// Moves connection [connId] into `testing` (probe running).
  void beginProbe(String connId) {
    _updateState(connId, TaskConnectionState.testing);
  }

  /// Moves connection [connId] into `connected` (probe passed).
  void completeProbe(String connId) {
    _updateState(connId, TaskConnectionState.connected);
  }

  /// Moves connection [connId] into `unreachable` (probe FAILED — the
  /// wave-29 P2-09 honest negative verdict).
  void failProbe(String connId) {
    _updateState(connId, TaskConnectionState.unreachable);
  }

  /// Drops every connection at once (spec: 网络变化自动断开) and kills the
  /// pending code.
  void simulateNetworkChange() {
    _connections.clear();
    _pendingCode = null;
    _emitConnections();
  }

  // ---------------------------------------------------------------------
  // Test knobs — receive flow.
  // ---------------------------------------------------------------------

  /// Emits an incoming transfer offer. Returns the jobId.
  String emitIncomingRequest({
    String? jobId,
    required int fileCount,
    required int totalBytes,
  }) {
    final id = jobId ?? 'job-${_jobSeq += 1}';
    _incomingController.add(IncomingRequest(
      jobId: id,
      fileCount: fileCount,
      totalBytes: totalBytes,
    ));
    return id;
  }

  /// Simulates the 30 s window closing on [jobId] undecided (wave-29
  /// P2-11): emits on [SimpleTransferService.expiredIncomingRequests]
  /// unless the request was already decided through [consent]. A LATER
  /// decline of an expired job still completes normally (idempotent —
  /// nothing was received either way).
  void expireIncoming(String jobId) {
    if (_decidedJobs.contains(jobId)) {
      return;
    }
    expiredJobs.add(jobId);
    if (!_expiredIncomingController.isClosed) {
      _expiredIncomingController.add(jobId);
    }
  }

  /// Finishes an accepted receive (its `consent` future completes).
  void finishIncoming(String jobId) {
    _pendingReceives.remove(jobId)?.complete();
  }

  /// Fails an accepted receive (its `consent` future rejects).
  void failIncoming(String jobId, String reason) {
    _pendingReceives.remove(jobId)?.completeError(StateError(reason));
  }

  // ---------------------------------------------------------------------
  // Test knobs — send flow.
  // ---------------------------------------------------------------------

  /// Completes the oldest unfinished [sendFiles] with [summary].
  void completeSend([String summary = '发送完成']) {
    _popPendingSend()?.complete(summary);
  }

  /// Fails the oldest unfinished [sendFiles] with [reason].
  void failSend(String reason) {
    _popPendingSend()?.completeError(StateError(reason));
  }

  Completer<String>? _popPendingSend() {
    _pendingSends.removeWhere((c) => c.isCompleted);
    return _pendingSends.isEmpty ? null : _pendingSends.removeAt(0);
  }

  // ---------------------------------------------------------------------
  // Internals.
  // ---------------------------------------------------------------------

  TaskConnection _consumePendingCode({
    required String peerName,
    TaskRole? role,
  }) {
    final pending = _pendingCode!;
    _pendingCode = null; // one-time code: consumed
    final connection = TaskConnection(
      id: 'conn-${_connSeq += 1}',
      peerName: peerName,
      role: role ?? pending.role,
      state: TaskConnectionState.connecting,
    );
    _connections.add(connection);
    _emitConnections();
    if (autoProbe) {
      scheduleMicrotask(() {
        _updateState(connection.id, TaskConnectionState.testing);
        scheduleMicrotask(
          () => _updateState(connection.id, TaskConnectionState.connected),
        );
      });
    }
    return connection;
  }

  void _updateState(String connId, TaskConnectionState state) {
    final index = _connections.indexWhere((c) => c.id == connId);
    if (index < 0) {
      return;
    }
    _connections[index] = TaskConnection(
      id: _connections[index].id,
      peerName: _connections[index].peerName,
      role: _connections[index].role,
      state: state,
    );
    _emitConnections();
  }

  void _dropConnection(String connId) {
    // One-time codes are consumed at pairing time in this model, so a
    // dropped connection can never be re-joined through its code (spec 9:
    // 断开配对码即失效).
    _connections.removeWhere((c) => c.id == connId);
    _emitConnections();
  }

  void _emitConnections() {
    if (!_connectionsController.isClosed) {
      _connectionsController.add(List.of(_connections));
    }
  }
}
