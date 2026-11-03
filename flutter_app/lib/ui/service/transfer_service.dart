/// UI-facing service contract for AK-sending desktop pages.
///
/// The UI layer never imports `dart:ffi`; every page depends only on this
/// abstraction plus the pure model/state layer of `ak_ffi_dart`
/// (`JobController`, `ProgressEvent`, `TransferReport`, ...).
///
/// ## Integration note (resolved with the services agent)
///
/// The layering below is the wave-11 architecture:
///
/// ```text
/// lib/ui pages ──▶ this contract (TransferService)
///                        ▲ implemented by
/// lib/services/ffi_transfer_service.dart   (sessions/status folding)
///                        ▲
/// lib/services/real_transfer_service.dart  (engine registry + event pump;
///        implements the lower-level lib/services/transfer_service.dart)
/// ```
///
/// Widget tests inject their own fake from `test/` and never touch the
/// real service; the member signatures here are final for this wave.
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show JobController;

/// Coarse service state rendered as the HomePage status dot.
enum TransferServiceStatus {
  /// Engine loaded, nothing running.
  idle,

  /// A receive listener is bound and waiting for senders.
  listening,

  /// A transfer is moving data.
  busy,

  /// The service is unusable (engine error, disposed, ...).
  error,
}

/// Direction of a (historical) job.
enum JobDirection { send, receive }

/// Terminal/running state of a job history record.
enum JobRecordState { running, done, failed }

/// This device's identity, as surfaced on the HomePage identity card and
/// the ReceivePage listening banner.
///
/// Wave-12 wiring: a real implementation fills this from
/// `engine.identityInfo(idDir)` (`device_id`/`fingerprint`/`pairing_code`/
/// `peers`/`created` keys); see [TransferService.refreshIdentity]. A missing
/// engine degrades to the hostname-only shape (empty fingerprint/pairing
/// code, [peersCount] 0).
class DeviceInfo {
  const DeviceInfo({
    required this.deviceName,
    required this.fingerprintHex,
    required this.pairingCode,
    this.peersCount = 0,
    this.identityCreated = false,
  });

  /// Human-readable device name sent to peers (`device_name`).
  final String deviceName;

  /// This device's fingerprint, 64 lowercase hex characters.
  final String fingerprintHex;

  /// This device's pairing code in canonical `123-456` form.
  final String pairingCode;

  /// Entries in the pairing book (`peers` of `identityInfo`).
  final int peersCount;

  /// True when `identityInfo` reported `created` — the engine just minted
  /// the `<idDir>/device.key` identity on this call (first run).
  final bool identityCreated;
}

/// Which peers a receive listener admits — the CLI's three admission
/// forms (docs/UX.zh-CN.md, "配对流程（准入三形态）").
enum AdmissionMode {
  /// Pairing book only (CLI default: `receive` without `--expect`).
  /// Enforced by the service layer, like ak-cli does with peers.book.
  book,

  /// Accept every peer (`--expect any`); the UI warns before starting.
  any,

  /// A pinned fingerprint or a 6-digit pairing code typed by the user.
  pinned,
}

/// Options for [TransferService.startReceive], mirroring the FFI receive
/// spec (`AkReceiveSpec`) plus the book admission default of ak-cli.
class ReceiveOptions {
  const ReceiveOptions({
    required this.outDir,
    this.port = 0,
    this.admission = AdmissionMode.book,
    this.expectText,
    this.conflict = 'fail',
    this.timeoutSecs = 30,
  });

  /// Output directory (created when missing). Non-empty.
  final String outDir;

  /// UDP port 0..66535; 0 asks the OS for an ephemeral port.
  final int port;

  /// Admission form; [AdmissionMode.pinned] requires [expectText].
  final AdmissionMode admission;

  /// The pinned fingerprint (64 hex) or pairing code text; ignored unless
  /// [admission] is [AdmissionMode.pinned] (or the string `'any'`).
  final String? expectText;

  /// `'fail'` (CLI `error`, default) or `'rename'`.
  final String conflict;

  /// Idle budget in seconds (frame-level, not whole-run).
  final int timeoutSecs;
}

/// Options for [TransferService.startSend], mirroring `AkSendSpec`.
class SendOptions {
  const SendOptions({
    required this.sourceRoots,
    required this.peer,
    required this.fingerprint,
    this.multistream = false,
    this.noInternet = false,
    this.conflict = 'fail',
    this.timeoutSecs = 60,
  });

  /// At least one file/directory path to send.
  final List<String> sourceRoots;

  /// Receiver address as literal `ip:port`.
  final String peer;

  /// Receiver fingerprint: full 64 hex, or a unique prefix resolved
  /// against the pairing book by the service (ak-cli `send --fp`).
  final String fingerprint;

  /// Allow multistream when the peer advertises the capability.
  final bool multistream;

  /// Refuse non-LAN/loopback paths (policy gate; loopback unaffected).
  final bool noInternet;

  /// `'fail'` (default) or `'rename'` for colliding names.
  final String conflict;

  /// Idle budget in seconds.
  final int timeoutSecs;
}

/// One record of the job history — the jobs checkpoint store read back via
/// `engine.jobsList(idDir)` (rows: `id`/`direction`/`state`/`files`/`bytes`/
/// `updated_unix_ms`/`peer`/`root`); see JobsPage.
class JobRecord {
  const JobRecord({
    required this.id,
    required this.direction,
    required this.startedAt,
    required this.state,
    this.files = 0,
    this.bytes = 0,
    this.summary = '',
    this.error,
    this.peer,
    this.root,
  });

  /// Short display id (fingerprint prefix / short job id).
  final String id;

  /// Send or receive.
  final JobDirection direction;

  /// When the job was last updated (the engine reports `updated_unix_ms`,
  /// not a start timestamp — this is that wall clock, newest first).
  final DateTime startedAt;

  /// Running / done / failed.
  final JobRecordState state;

  /// Files transferred (0 while unknown).
  final int files;

  /// Bytes transferred (0 while unknown).
  final int bytes;

  /// Free-form context: roots, peer address, ...
  final String summary;

  /// Failure message of a failed record.
  final String? error;

  /// Peer fingerprint of the record (64 hex) when the engine reported one.
  final String? peer;

  /// Destination/source root of the record when the engine reported one.
  final String? root;
}

/// The abstract transfer service consumed by every page.
///
/// Implementations own the FFI engine: they start jobs, route poll lines
/// into the matching [JobController] (folding terminal results via
/// `setResult`), expose listener lifecycle and history, and must be safe
/// to construct exactly once per app run.
abstract interface class TransferService {
  /// Engine/library version string, e.g. `ak-ffi 1 (wire v1, caps 0x0007)`.
  String version();

  /// This device's identity (name, fingerprint, pairing code, pairing-book
  /// size). Synchronous: implementations return the cached snapshot filled
  /// by [refreshIdentity] (or an honest degraded placeholder before the
  /// first refresh / when the engine is unavailable).
  DeviceInfo deviceInfo();

  /// Re-reads the identity from the engine's identity store under the
  /// configured identity directory (see [setIdentityDir]), caches it for
  /// [deviceInfo] and notifies status listeners.
  ///
  /// Never throws for an unavailable engine: the call degrades to the
  /// hostname-only identity so the pages stay usable.
  Future<DeviceInfo> refreshIdentity();

  /// Overrides the identity directory used by [refreshIdentity],
  /// [listJobs] and job specs (`id_dir`; default
  /// `%USERPROFILE%\.ak-sending`). An empty [dir] resets to the default.
  void setIdentityDir(String dir);

  /// Current coarse state for the status dot.
  TransferServiceStatus get status;

  /// Status change notifications (listener list, JobController-style).
  void addStatusListener(void Function() listener);

  /// Removes a previously added status listener (no-op when absent).
  void removeStatusListener(void Function() listener);

  /// True while a receive listener is bound.
  bool get isListening;

  /// Starts a receive listener and returns the [JobController] folding
  /// its progress events until [stopReceive]. Throws [StateError] when
  /// already listening; spec violations throw (implementation-defined).
  JobController startReceive(ReceiveOptions options);

  /// Stops the active listener (aborting an in-flight transfer job).
  Future<void> stopReceive();

  /// Starts a one-shot send and returns its folding [JobController].
  JobController startSend(SendOptions options);

  /// Job history records, newest first — a REAL read of the jobs checkpoint
  /// store (`engine.jobsList(idDir)`) in the wired implementation; rows are
  /// mapped to [JobRecord] (update time, direction/state/files/bytes, peer
  /// and root). Implementations without the store return an empty list
  /// rather than throwing (missing engine degrades gracefully).
  Future<List<JobRecord>> listJobs();

  /// Releases engine resources. Idempotent.
  void dispose();
}
