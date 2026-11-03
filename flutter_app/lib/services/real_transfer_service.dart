/// [RealTransferService]: [TransferService] over a real ak-ffi engine.
///
/// Composition: a [Timer] drains `ak_poll_progress` (all queued lines, up to
/// [maxEventsPerTick] per tick) and adds the decoded [ProgressEvent]s to a
/// broadcast stream — the "poll → JobController" pattern of
/// packages/ak_ffi_dart/example/loopback_demo.dart, lifted into a service.
///
/// Threading: the underlying engine requires serialized calls; a single
/// isolate serializes them for free (Dart never runs two of our callbacks
/// concurrently), so keep ONE service per isolate and never share it across
/// isolates.
///
/// The [EngineBackend] seam exists for tests: [FakeEngineBackend]-style
/// doubles drive the pump and routing without any native library.
library;

import 'dart:async' show StreamController, Timer;
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngineDart, AkFfiException, AkJobResult, AkReceiveSpec, AkSendSpec, ProgressEvent;

import 'dll_path.dart';
import 'transfer_service.dart';

/// The engine operations [RealTransferService] needs — the exact surface of
/// [AkEngineDart] the service drives, extracted so tests can inject a fake.
abstract class EngineBackend {
  String version();

  Uint8List sendStart(AkSendSpec spec);

  Uint8List receiveStart(AkReceiveSpec spec);

  /// Pops the oldest pending progress line (decoded JSON), null when empty.
  Map<String, Object?>? pollProgress();

  AkJobResult jobResult(Uint8List job);

  void cancel(Uint8List job);

  void dispose();
}

/// [EngineBackend] over one real [AkEngineDart].
final class AkEngineBackend implements EngineBackend {
  AkEngineBackend._(this.engine, this.resolution);

  /// Resolves the DLL (three-tier probe, [resolveAkDllPath]), opens the
  /// engine and validates it with a `version()` round-trip.
  ///
  /// Throws [DllNotFoundException] when no tier resolves or the library
  /// fails to load.
  factory AkEngineBackend.open() {
    final resolution = resolveAkDllPath();
    if (resolution == null) {
      throw DllNotFoundException(akDllProbeCandidates());
    }
    final AkEngineDart engine;
    try {
      engine = AkEngineDart(dllPath: resolution.path);
    } on ArgumentError catch (error) {
      // DynamicLibrary.open reports OS loader failures as ArgumentError.
      throw DllNotFoundException(
        [resolution.path, ...akDllProbeCandidates()],
        error,
      );
    }
    return AkEngineBackend._(engine, resolution);
  }

  /// The wrapped engine (FFI-experiment escape hatch; the service only uses
  /// the [EngineBackend] members).
  final AkEngineDart engine;

  /// How the DLL was found (diagnostics card).
  final DllResolution? resolution;

  @override
  String version() => engine.version();

  @override
  Uint8List sendStart(AkSendSpec spec) => engine.sendStart(spec);

  @override
  Uint8List receiveStart(AkReceiveSpec spec) => engine.receiveStart(spec);

  @override
  Map<String, Object?>? pollProgress() => engine.pollProgress();

  @override
  AkJobResult jobResult(Uint8List job) => engine.jobResult(job);

  @override
  void cancel(Uint8List job) => engine.cancel(job);

  @override
  void dispose() => engine.dispose();
}

/// Real transfer service over one engine (see the library doc).
final class RealTransferService implements TransferService {
  RealTransferService({
    EngineBackend? backend,
    Duration pollInterval = const Duration(milliseconds: 50),
  })  : _backend = backend ?? AkEngineBackend.open(),
        backendResolution =
            backend is AkEngineBackend ? backend.resolution : null {
    _events =
        StreamController<ProgressEvent>.broadcast();
    _pump = Timer.periodic(pollInterval, (_) => _drain());
  }

  /// How many queued lines one tick may drain (bounds event-loop stall when
  /// a huge backlog sits in the engine; the next tick continues).
  static const int maxEventsPerTick = 512;

  final EngineBackend _backend;

  /// Non-null only when this service opened its own DLL (diagnostics card).
  final DllResolution? backendResolution;

  late final StreamController<ProgressEvent> _events;
  late final Timer _pump;

  /// hex job id → raw 16-byte FFI id (hex == the events' `"job"` field).
  final Map<String, Uint8List> _jobs = {};

  bool _disposed = false;

  /// Lines dropped because they failed [ProgressEvent.fromJson] — a
  /// malformed payload must never kill the pump. Forward-compatible
  /// *unknown event names* are NOT malformed (they decode to UnknownEvent).
  int get skippedMalformedEvents => _skippedMalformed;
  int _skippedMalformed = 0;

  /// Open job ids this service started (insertion order).
  Iterable<String> get startedJobIds => List.unmodifiable(_jobs.keys);

  @override
  String version() {
    _ensureLive();
    return _backend.version();
  }

  @override
  DeviceInfo deviceInfo() {
    // Wave-11 boundary (see DeviceInfo doc): ak-ffi exports no identity
    // introspection yet, so there is nothing real to read back.
    _ensureLive();
    return DeviceInfo.notCreated;
  }

  @override
  String startSend(AkSendSpec spec) => _start(spec, _backend.sendStart);

  @override
  String startReceive(AkReceiveSpec spec) =>
      _start(spec, _backend.receiveStart);

  String _start<T>(T spec, Uint8List Function(T spec) invoke) {
    _ensureLive();
    final raw = invoke(spec);
    final hex = _hex(raw);
    _jobs[hex] = raw;
    return hex;
  }

  @override
  Stream<ProgressEvent> get events => _events.stream;

  @override
  Stream<ProgressEvent> eventsFor(String jobId) =>
      events.where((event) => event.job == jobId);

  @override
  AkJobResult result(String jobId) {
    _ensureLive();
    return _backend.jobResult(_job(jobId));
  }

  @override
  void cancel(String jobId) {
    _ensureLive();
    _backend.cancel(_job(jobId));
  }

  @override
  List<JobSummary> listJobs() {
    // Wave 11: empty by contract (see TransferService.listJobs doc) — the
    // in-memory registry above exists only for result/cancel routing.
    return const [];
  }

  @override
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    _pump.cancel();
    _events.close();
    _jobs.clear();
    _backend.dispose();
  }

  void _ensureLive() {
    if (_disposed) {
      throw StateError('RealTransferService is disposed');
    }
  }

  Uint8List _job(String jobId) {
    final raw = _jobs[jobId];
    if (raw == null) {
      throw ArgumentError.value(jobId, 'jobId',
          'not a job id started by this service (${_jobs.length} known)');
    }
    return raw;
  }

  /// One pump tick: drain queued lines, decode, publish.
  void _drain() {
    var drained = 0;
    while (drained < maxEventsPerTick) {
      final Map<String, Object?>? line;
      try {
        line = _backend.pollProgress();
      } on AkFfiException {
        // Polling failed this tick (engine busy/erroring); retry next tick
        // rather than killing the pump.
        return;
      }
      if (line == null) {
        return;
      }
      drained++;
      try {
        _events.add(ProgressEvent.fromJson(line));
      } on FormatException {
        _skippedMalformed++;
      }
    }
  }

  static String _hex(Uint8List bytes) =>
      bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
}

TransferService? _sharedInstance;

/// Lazy singleton for `main` — constructed on first use so widget tests that
/// never start a transfer do not load the native library.
TransferService sharedTransferService() =>
    _sharedInstance ??= RealTransferService();
