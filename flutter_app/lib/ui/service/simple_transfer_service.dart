/// Wave-28 (v1 极简配对) frozen UI⇄service contract.
///
/// The wave-28 UI codes ONLY against this abstraction — no IP/port/
/// fingerprint forms anywhere. The real implementation (agent C, class
/// name `SimpleTransferService`, constructor-injected facade) and the
/// development fake (`lib/services/simple_transfer_fake.dart`,
/// `LocalSimpleTransferService`) both implement it; the single switch
/// point is `lib/main.dart`'s constructor call. The member shapes below
/// are FROZEN by the wave-28 spec — signatures may not drift.
///
/// Product rules this contract encodes (user-frozen spec, wave-28):
/// * pairing = one short one-time code; creating it declares MY direction
///   (我要发送 / 我要接收); the connection stays one-way — the reverse
///   direction needs a fresh pairing;
/// * after pairing both ends run an automatic connectivity probe and the
///   UI then shows the connection as live ("连接成功");
/// * multiple connections coexist; any end (or a network change) drops
///   one and its code dies with it;
/// * receiving always lands in the single default save folder.
library;

/// Direction declared when the pairing code was created, from THIS
/// device's point of view: [send] = 我要发送 (I push files to the peer),
/// [receive] = 我要接收 (the peer pushes files to me). Frozen for the
/// lifetime of the connection (one-way; reverse needs a new pairing).
enum TaskRole { send, receive }

/// A freshly minted one-time pairing code.
class TaskCode {
  const TaskCode({required this.code, required this.role});

  /// The code in canonical display form: 8 characters split by a hyphen
  /// (e.g. `K3P7-QM2X`), cased as created. Consumed once; invalid after
  /// the pairing completes or either side disconnects.
  final String code;

  /// The direction this device declared when creating the code.
  final TaskRole role;
}

/// Live state of one [TaskConnection].
enum TaskConnectionState {
  /// Pairing finished; the link is being established.
  connecting,

  /// Automatic connectivity probe (spec: 配对成功后自动做连通测试).
  testing,

  /// Probe passed — transfer-ready ("测试成功").
  connected,

  /// The connection dropped (either end or a network change). Services
  /// remove dropped rows from the `connections` snapshot; this state
  /// exists so the UI can label a row observed mid-teardown.
  disconnected,
}

/// One live pairing: peer name, frozen direction and state. Until either
/// side disconnects; several may coexist at once.
class TaskConnection {
  const TaskConnection({
    required this.id,
    required this.peerName,
    required this.role,
    required this.state,
  });

  /// Opaque connection id (also the `sendFiles`/`disconnect` argument).
  final String id;

  /// Display name of the remote device.
  final String peerName;

  /// MY direction for this connection (frozen at pairing).
  final TaskRole role;

  final TaskConnectionState state;
}

/// Result of joining a code: ok carries the new connection id; failure
/// carries a user-readable Chinese reason (shown verbatim by the UI with
/// a retry affordance).
class JoinResult {
  const JoinResult({required this.ok, this.connectionId, this.error});

  const JoinResult._ok(this.connectionId)
      : ok = true,
        error = null;

  const JoinResult._failed(this.error)
      : ok = false,
        connectionId = null;

  factory JoinResult.success(String connectionId) =>
      JoinResult._ok(connectionId);

  factory JoinResult.failure(String error) => JoinResult._failed(error);

  final bool ok;
  final String? connectionId;
  final String? error;
}

/// An incoming transfer offer: the peer wants to push [fileCount] files
/// totalling [totalBytes] bytes. The UI asks 接受 / 拒绝 and answers via
/// `consent(jobId, ...)`; accepted transfers land in the default save
/// folder and the `consent` future completes when the transfer finishes
/// (failure rejects it with a readable reason).
class IncomingRequest {
  const IncomingRequest({
    required this.jobId,
    required this.fileCount,
    required this.totalBytes,
  });

  final String jobId;
  final int fileCount;
  final int totalBytes;
}

/// The wave-28 minimal transfer service (frozen shape).
///
/// Lifecycle: `start()` once at app launch; `dispose()` at shutdown.
/// Streams are broadcast; `connections` replays the current snapshot to
/// late subscribers, `incomingRequests` does not replay.
abstract class SimpleTransferService {
  /// Initializes the service (identity, listeners). Call once before any
  /// other member.
  Future<void> start();

  /// Mints a fresh one-time pairing code declaring MY direction [role].
  /// Any previously minted unconsumed code dies.
  Future<TaskCode> createCode(TaskRole role);

  /// Joins (consumes) a pairing code. Implementations tolerate hyphens,
  /// spaces and lower case in [code]. On success the new connection
  /// (state [TaskConnectionState.connecting] — the auto probe follows)
  /// appears in `connections`.
  Future<JoinResult> joinCode(String code);

  /// Live connection snapshots (id/对端名/方向/状态); dropped connections
  /// leave the list.
  Stream<List<TaskConnection>> get connections;

  /// Incoming transfer offers (jobId/文件数/总字节), one event per offer.
  Stream<IncomingRequest> get incomingRequests;

  /// Answers an [IncomingRequest]. With `accept: true` the returned future
  /// completes when the receive finishes and rejects on failure; the files
  /// land in the default save folder. With `accept: false` the offer is
  /// declined (nothing is received) and the future completes immediately.
  Future<void> consent(String jobId, bool accept);

  /// Sends [paths] (files and/or one directory) over connection [connId]
  /// and completes with a readable summary when the peer has everything;
  /// rejects with a readable reason on failure. Only valid on connections
  /// whose direction is [TaskRole.send].
  Future<String> sendFiles(String connId, List<String> paths);

  /// Drops connection [connId]; its pairing code (if still around) dies.
  Future<void> disconnect(String connId);

  /// Sets the default receive folder (settings page).
  Future<void> setSaveDir(String path);

  /// The current default receive folder (never empty; implementations
  /// seed a platform default).
  String get saveDir;

  /// Releases resources. Idempotent.
  void dispose();
}

/// Canonical `XXXX-XXXX` display of an 8-char code body; shorter/other
/// input is uppercased and hyphenated on the 4th character when possible,
/// otherwise returned as-is (uppercased, hyphens stripped).
String formatTaskCode(String raw) {
  final body = raw.toUpperCase().replaceAll(RegExp(r'[^0-9A-Z]'), '');
  if (body.length == 8) {
    return '${body.substring(0, 4)}-${body.substring(4)}';
  }
  return body;
}

/// Normalizes user-typed join input for `joinCode`: uppercases, strips
/// hyphens/spaces. Implementations accept both this and the hyphenated
/// form (连字符容错).
String normalizeJoinCode(String input) =>
    input.toUpperCase().replaceAll(RegExp(r'[^0-9A-Z]'), '');

/// Human byte size (`12.3 MB`) for banner/detail copy.
String formatBytes(int bytes) {
  if (bytes < 1024) {
    return '$bytes B';
  }
  const units = ['KB', 'MB', 'GB', 'TB'];
  var value = bytes.toDouble();
  var unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  return value >= 100
      ? '${value.toStringAsFixed(0)} ${units[unit]}'
      : '${value.toStringAsFixed(1)} ${units[unit]}';
}
