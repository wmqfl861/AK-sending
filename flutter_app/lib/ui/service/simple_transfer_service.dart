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
  const TaskCode({required this.code, required this.role, this.codeId});

  /// The code in canonical display form: `AKI1-` + eight groups of four
  /// Crockford Base32 symbols, e.g.
  /// `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` (the Rust
  /// `InviteCode::to_display` shape — 160 bits of entropy; the `AKI1`
  /// prefix is the format version, not entropy). Consumed once; invalid
  /// after the pairing completes or either side disconnects.
  final String code;

  /// The direction this device declared when creating the code.
  final TaskRole role;

  /// The engine's code id (wave-30 P2-04): the CANCEL key — calling
  /// `disconnect(codeId)` on the code-creating service while the code is
  /// still unconsumed revokes it (the registry row goes away, the code
  /// can never be redeemed). Null from implementations without the fact
  /// (the dev fake pre-wave-30, an old engine): cancelling then simply
  /// resets the UI.
  final String? codeId;
}

/// Live state of one [TaskConnection].
enum TaskConnectionState {
  /// Pairing finished; the link is being established.
  connecting,

  /// Automatic connectivity probe (spec: 配对成功后自动做连通测试).
  testing,

  /// Probe passed — transfer-ready ("测试成功").
  connected,

  /// Probe FAILED (wave-29 P2-09): the pairing committed but no data
  /// path to the peer could be verified. Not terminal-disconnected — the
  /// user should re-pair (or check the network) rather than retry a send.
  unreachable,

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
    this.saveDir,
  });

  final String jobId;
  final int fileCount;
  final int totalBytes;

  /// The save folder THIS request will use (wave-30 P2-06): snapshotted
  /// by the engine at offer time and carried on the event — the receive
  /// lands there even if the settings change before the user answers, so
  /// the UI shows THIS value as the target. Null = the implementation
  /// lacks the fact; show the service's live `saveDir` instead.
  final String? saveDir;
}

/// The wave-28 minimal transfer service (frozen shape + the wave-29
/// review-fix additions documented per member).
///
/// Lifecycle: `start()` once at app launch; `dispose()` at shutdown.
/// Streams are broadcast.
abstract class SimpleTransferService {
  /// Initializes the service (identity, listeners). Call once before any
  /// other member. A failed start surfaces through the app shell's
  /// startup-error banner (wave-29 P1-01) — implementations may throw a
  /// readable reason.
  Future<void> start();

  /// Mints a fresh one-time pairing code declaring MY direction [role].
  /// Any previously minted unconsumed code dies.
  Future<TaskCode> createCode(TaskRole role);

  /// Joins (consumes) a pairing code. Implementations tolerate hyphens,
  /// spaces and lower case in [code]. The optional [role] (wave-29
  /// P1-05) is the joiner's EXPLICIT direction — the UI user's intent —
  /// forwarded to the engine so a cross-device join never guesses the
  /// direction; null keeps the engine's resolution. On success the new
  /// connection (state [TaskConnectionState.connecting] — the auto probe
  /// follows) appears in `connections`.
  Future<JoinResult> joinCode(String code, {TaskRole? role});

  /// Live connection snapshots (id/对端名/方向/状态); dropped connections
  /// leave the list.
  ///
  /// Replay contract (wave-29 P1-06): EVERY new subscription first
  /// receives the CURRENT snapshot, then the live change events — a
  /// detail page opened while another listener is already attached (the
  /// home page) still sees the live list immediately. Each call of this
  /// getter returns an independent stream.
  Stream<List<TaskConnection>> get connections;

  /// Incoming transfer offers (jobId/文件数/总字节), one event per offer.
  Stream<IncomingRequest> get incomingRequests;

  /// Job ids whose consent request became terminal undecided (wave-29
  /// P2-11, wave-30: driven by NATIVE facts — the engine's
  /// `consent_expired` event for the 30 s window, and the request's
  /// connection going away; never a Dart-side timer mirror): the UI
  /// closes their banners. A later decline click on such a job resolves
  /// normally (idempotent).
  Stream<String> get expiredIncomingRequests;

  /// Answers an [IncomingRequest]. With `accept: true` the returned future
  /// completes when the receive finishes and rejects on failure; the files
  /// land in the default save folder. With `accept: false` the offer is
  /// declined (nothing is received) and the future completes immediately —
  /// including when the request already expired (wave-29 P2-11).
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

/// Format version prefix of every task code (not entropy). Mirrors the
/// Rust `InviteCode` presentation (`AKI1`, crates/ak-identity).
const String taskCodePrefix = 'AKI1';

/// Symbol count after the prefix: 160 bits / 5 bits per symbol.
const int taskCodeSymbolCount = 32;

/// Groups of four symbols in the canonical display form (wave-29 P1-02).
const int taskCodeGroupCount = 8;

final RegExp _taskCodeShape = RegExp(r'^AKI1[0-9A-HJKMNP-TV-Z]{32}$');

/// Canonical `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` display of a
/// task code; input in ANY tolerant form (lower case, hyphens, spaces,
/// ungrouped) regroups when it is a valid code, otherwise the normalized
/// (uppercased, separator-stripped) input is returned as-is so the UI can
/// show what is wrong.
String formatTaskCode(String raw) {
  final normalized = normalizeJoinCode(raw);
  if (isValidTaskCode(normalized)) {
    final body = normalized.substring(taskCodePrefix.length);
    final groups = <String>[
      for (var i = 0; i < taskCodeGroupCount; i++)
        body.substring(i * 4, i * 4 + 4),
    ];
    return '$taskCodePrefix-${groups.join('-')}';
  }
  return normalized;
}

/// Normalizes user-typed join input for `joinCode`: uppercases, strips
/// hyphens and whitespace (大小写/连字符/空格容错 — the engine's
/// `normalize_code_key` accepts the same forms). The result of a valid
/// input is `AKI1` + 32 Crockford Base32 symbols (36 characters).
String normalizeJoinCode(String input) =>
    input.toUpperCase().replaceAll(RegExp(r'[-\s]'), '');

/// Whether [normalized] (a [normalizeJoinCode] result) is a WELL-FORMED
/// task code: the `AKI1` prefix plus exactly 32 Crockford Base32 symbols
/// (the alphabet excludes I, L, O and U — undefined symbols are refused,
/// never guessed, mirroring the Rust `InviteCode::parse`).
bool isValidTaskCode(String normalized) =>
    _taskCodeShape.hasMatch(normalized);

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
