/// UI-facing transfer contract: everything a page needs to drive ak-engine
/// jobs without touching dart:ffi (see flutter_app/README.md architecture).
///
/// One service instance owns one native engine; create it from the main
/// isolate only (`RealTransferService` documents the threading rules). The
/// UI agent's pages depend on this abstraction, never on the real
/// implementation, so widget tests can drive a fake.
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkJobResult,
        AkReceiveSpec,
        AkSendSpec,
        JobOutcome,
        ProgressEvent,
        TransferReport;

/// Identity card model of the Home page.
///
/// Wave-11 boundary: ak-ffi exports no identity introspection (no
/// `ak_identity_info` yet), so a real fingerprint cannot be read back. This
/// wave always reports [DeviceInfo.notCreated]; the UI renders the hint.
/// When ak-ffi grows the identity export, `deviceInfo()` starts returning
/// `identityReady: true` plus the fingerprint — the shape below is final.
final class DeviceInfo {
  const DeviceInfo({
    required this.identityReady,
    required this.fingerprintHex,
    required this.hint,
  });

  /// A `<id_dir>/device.key` identity exists and could be displayed.
  final bool identityReady;

  /// Own fingerprint (64 hex chars); null until ak-ffi exports it.
  final String? fingerprintHex;

  /// Short user-facing explanation shown on the identity card.
  final String hint;

  /// The wave-11 placeholder (see the class doc).
  static const DeviceInfo notCreated = DeviceInfo(
    identityReady: false,
    fingerprintHex: null,
    hint: '身份尚未创建：运行 ak-cli keygen 生成，'
        '或完成一次接收后由引擎创建。指纹展示待 ak-ffi 导出身份查询 API。',
  );
}

/// Direction of a started job.
enum TransferDirection { send, receive }

/// One row of the (future) job history list.
final class JobSummary {
  const JobSummary({
    required this.jobId,
    required this.direction,
    required this.startedAt,
    required this.outcome,
  });

  /// Opaque FFI job id, 32 hex chars.
  final String jobId;

  final TransferDirection direction;

  final DateTime startedAt;

  /// Terminal outcome once known; null while running or unknown.
  final JobOutcome? outcome;
}

/// Raised when the native ak_ffi library cannot be loaded; [candidates]
/// lists every probed path so the operator sees what was tried.
final class DllNotFoundException implements Exception {
  const DllNotFoundException(this.candidates, [this.cause]);

  final List<String> candidates;
  final Object? cause;

  @override
  String toString() {
    final tried = candidates.map((c) => '  - $c').join('\n');
    final suffix = cause == null ? '' : '\ncause: $cause';
    return '未找到 ak_ffi 动态库。探测过：\n$tried\n'
        '可设置 AK_FFI_DLL 指向 ak_ffi.dll，或先运行 '
        'cargo build -p ak-ffi --release。$suffix';
  }
}

/// The UI-facing transfer API. All job ids are 32-char hex strings matching
/// the `"job"` field of every [ProgressEvent], so events route by equality.
abstract class TransferService {
  /// Engine/library version line (e.g. `ak-ffi 1 (wire v1, caps 0x0007)`).
  ///
  /// Throws [DllNotFoundException]-family errors when the engine is missing.
  String version();

  /// Identity card data; see [DeviceInfo] for the wave-11 boundary.
  DeviceInfo deviceInfo();

  /// Starts a send job and returns its 32-hex job id.
  ///
  /// Throws [AkSpecException]-style validation errors before touching FFI.
  String startSend(AkSendSpec spec);

  /// Starts a receive job and returns its 32-hex job id.
  String startReceive(AkReceiveSpec spec);

  /// Every progress event of every job (broadcast; subscribe anytime).
  ///
  /// Events with no listener are dropped — subscribe before starting a job
  /// when completeness matters. Route per job with [eventsFor].
  Stream<ProgressEvent> get events;

  /// [events] filtered to one job id — the stream a job page consumes.
  Stream<ProgressEvent> eventsFor(String jobId);

  /// Snapshot of one job's terminal state (`running` until it finishes).
  ///
  /// `done`/`failed` carry the decoded body: [TransferReport] fields on
  /// success, `{"job","error"}` on failure.
  AkJobResult result(String jobId);

  /// Aborts a running job (it then reports `failed` with
  /// "cancelled by caller"); an already-finished job is left untouched.
  void cancel(String jobId);

  /// Job history rows.
  ///
  /// Wave 11: always empty — persistent job listing belongs to the jobs
  /// checkpoint store (docs/BACKLOG.zh-CN.md P1-03) and is deliberately NOT
  /// read from disk this wave. TODO(wave 12+): back this with the jobs
  /// checkpoint once ak-ffi/ak-engine expose an enumeration API.
  List<JobSummary> listJobs();

  /// Releases the native engine (aborting still-running jobs). Idempotent.
  void dispose();
}
