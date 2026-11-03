/// Wave-11 service-layer tests: DLL three-tier probe (pure), poll-pump event
/// routing over a fake engine backend, lifecycle/registry contracts, plus a
/// real-DLL loopback smoke that skips when ak_ffi.dll is absent.
library;

import 'dart:io';
import 'dart:typed_data';

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkJobResult, AkReceiveSpec, AkSendSpec, JobPhase, Listening, ProgressEvent;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/dll_path.dart';
import 'package:ak_sending_app/services/ffi_transfer_service.dart';
import 'package:ak_sending_app/services/real_transfer_service.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart'
    show AdmissionMode, ReceiveOptions, SendOptions, TransferServiceStatus;

final String _fpHex64 = 'a' * 64;

// ---------------------------------------------------------------------------
// DLL probe (pure function, injected inputs).
// ---------------------------------------------------------------------------

void main() {
  group('resolveAkDllPath 三级探测', () {
    test('tier 1: AK_FFI_DLL wins verbatim (no existence check)', () {
      final resolution = resolveAkDllPath(
        environment: {'AK_FFI_DLL': 'X:/custom/ak_ffi.dll'},
        resolvedExecutable: '/fake/exe/app.exe',
        workingDirectory: '/fake/cwd',
        exists: (path) => throw StateError('must not touch the filesystem'),
      );
      expect(resolution!.path, 'X:/custom/ak_ffi.dll');
      expect(resolution.source, DllSource.environment);
    });

    test('tier 2: exe-directory copy beats the dev path', () {
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      final exeDir = Directory('${root.path}/bundle')..createSync();
      File('${exeDir.path}/ak_ffi.dll').writeAsBytesSync([1, 2, 3]);
      final resolution = resolveAkDllPath(
        environment: const {},
        resolvedExecutable: '${exeDir.path}/ak_sending_app.exe',
        workingDirectory: root.path,
      );
      expect(resolution!.source, DllSource.executableDirectory);
      expect(resolution.path, endsWith('bundle/ak_ffi.dll'));
    });

    test('tier 3: ../target/release from a flutter_app-like cwd', () {
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      Directory('${root.path}/target/release').createSync(recursive: true);
      File('${root.path}/target/release/ak_ffi.dll').writeAsBytesSync([1]);
      // flutter_app sits ONE level below the repo root with target/.
      final cwd = Directory('${root.path}/flutter_app')..createSync();
      final resolution = resolveAkDllPath(
        environment: const {},
        resolvedExecutable: '${root.path}/elsewhere/tool.exe',
        workingDirectory: cwd.path,
      );
      expect(resolution!.source, DllSource.workingDirectory);
      expect(resolution.path, endsWith('../target/release/ak_ffi.dll'));
      expect(File(resolution.path).existsSync(), isTrue);
    });

    test('returns null when no tier resolves', () {
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      expect(
        resolveAkDllPath(
          environment: const {},
          resolvedExecutable: '${root.path}/exe/tool.exe',
          workingDirectory: '${root.path}/cwd',
        ),
        isNull,
      );
    });
  });

  // -------------------------------------------------------------------------
  // Service over a fake engine backend (no native library involved).
  // -------------------------------------------------------------------------

  group('RealTransferService over fake backend', () {
    late _FakeBackend backend;
    late RealTransferService service;

    setUp(() {
      backend = _FakeBackend();
      service = RealTransferService(
        backend: backend,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(service.dispose);
    });

    test('version/deviceInfo pass through (identity stays notCreated)', () {
      expect(service.version(), contains('fake'));
      final info = service.deviceInfo();
      expect(info.identityReady, isFalse);
      expect(info.fingerprintHex, isNull);
      expect(info.hint, isNotEmpty);
    });

    AkSendSpec sendSpec() => AkSendSpec(
          sourceRoots: const ['C:/pick/dir'],
          peer: '127.0.0.1:7100',
          fpHex: _fpHex64,
        );

    test('startSend/startReceive return 32-hex ids and reach the backend',
        () {
      final sendId = service.startSend(sendSpec());
      final recvId = service.startReceive(const AkReceiveSpec(
        outDir: 'C:/inbox',
        port: 0,
      ));
      expect(sendId.length, 32);
      expect(recvId.length, 32);
      expect(RegExp(r'^[0-9a-f]{32}$').hasMatch(sendId), isTrue);
      expect(backend.sendStarted, hasLength(1));
      expect(backend.receiveStarted, hasLength(1));
      expect(_hexOf(backend.sendStarted.single), sendId);
      expect(_hexOf(backend.receiveStarted.single), recvId);
      expect(service.listJobs(), isEmpty, reason: 'wave 11 contract');
    });

    test('poll pump decodes lines and routes them by job id', () async {
      final sendId = service.startSend(sendSpec());
      final otherId = service.startReceive(const AkReceiveSpec(
        outDir: 'C:/inbox',
        port: 0,
      ));
      final sendEvents = <ProgressEvent>[];
      final otherEvents = <ProgressEvent>[];
      final allEvents = <ProgressEvent>[];
      final subSend = service.eventsFor(sendId).listen(sendEvents.add);
      final subOther = service.eventsFor(otherId).listen(otherEvents.add);
      final subAll = service.events.listen(allEvents.add);
      addTearDown(() async {
        await subSend.cancel();
        await subOther.cancel();
        await subAll.cancel();
      });

      backend.lines.addAll([
        {'job': sendId, 'event': 'listening', 'addr': '127.0.0.1:52341'},
        {'job': otherId, 'event': 'connected', 'peer': _fpHex64},
        {
          'job': sendId,
          'event': 'file_started',
          'file': 0,
          'path': 'photos/img.bin',
          'bytes': 123,
        },
      ]);

      await _pumpUntil(
          () => sendEvents.length >= 2 && otherEvents.isNotEmpty);

      expect(sendEvents.every((e) => e.job == sendId), isTrue);
      expect(otherEvents.every((e) => e.job == otherId), isTrue);
      expect(allEvents, hasLength(3));
      expect(sendEvents.first, isA<Listening>());
      expect((sendEvents.first as Listening).addr, '127.0.0.1:52341');
      expect(sendEvents.last.name, 'file_started');
      expect(allEvents[1].name, 'connected');
    });

    test('a malformed line is skipped without killing the pump', () async {
      final sendId = service.startSend(sendSpec());
      final events = <ProgressEvent>[];
      final sub = service.events.listen(events.add);
      addTearDown(sub.cancel);
      backend.lines.addAll([
        {'job': sendId, 'event': 'listening'}, // missing "addr" -> FormatException
        {'job': sendId, 'event': 'done'},
      ]);
      await _pumpUntil(() => events.isNotEmpty);
      expect(service.skippedMalformedEvents, 1);
      expect(events.single.name, 'done');
    });

    test('result/cancel route to the backend; unknown ids are rejected', () {
      final sendId = service.startSend(sendSpec());
      expect(service.result(sendId).isRunning, isTrue);
      service.cancel(sendId);
      expect(backend.cancelled, hasLength(1));
      final failed = service.result(sendId);
      expect(failed.isFailed, isTrue);
      expect(failed.error, contains('cancel'));
      expect(() => service.result('ab' * 16), throwsArgumentError);
      expect(() => service.cancel('cd' * 16), throwsArgumentError);
    });

    test('dispose stops the pump and forbids further use', () async {
      final sendId = service.startSend(sendSpec());
      final events = <ProgressEvent>[];
      final sub = service.events.listen(events.add);
      await _pumpUntil(() => backend.polledAtLeastOnce);
      service.dispose();
      expect(backend.disposed, isTrue);
      backend.lines.add({'job': sendId, 'event': 'done'});
      await Future<void>.delayed(const Duration(milliseconds: 30));
      expect(events, isEmpty);
      expect(() => service.version(), throwsStateError);
      service.dispose(); // idempotent
      await sub.cancel();
    });
  });

  // -------------------------------------------------------------------------
  // FfiTransferService adapter over the UI contract (fake engine backend).
  // -------------------------------------------------------------------------

  group('FfiTransferService (UI contract adapter)', () {
    late _FakeBackend backend;
    late FfiTransferService service;

    setUp(() {
      backend = _FakeBackend();
      service = FfiTransferService(
        backend: backend,
        pollInterval: const Duration(milliseconds: 1),
        resultPollInterval: const Duration(milliseconds: 2),
      );
      addTearDown(service.dispose);
    });

    ReceiveOptions anyReceive() => const ReceiveOptions(
          outDir: 'C:/inbox',
          port: 0,
          admission: AdmissionMode.any,
        );

    test('identity placeholders, version passthrough, empty history', () {
      expect(service.version(), contains('fake'));
      final info = service.deviceInfo();
      expect(info.deviceName, isNotEmpty); // real hostname
      expect(info.fingerprintHex, isEmpty); // wave 11: no ak-ffi export
      expect(info.pairingCode, isEmpty);
      expect(service.status, TransferServiceStatus.idle);
      expect(service.listJobs(), completion(isEmpty));
    });

    test('book admission is refused with pairing guidance (empty book)', () {
      expect(
        () => service.startReceive(const ReceiveOptions(
          outDir: 'C:/inbox',
          admission: AdmissionMode.book,
        )),
        throwsStateError,
      );
      expect(service.isListening, isFalse);
    });

    test('any admission: listening status, event folding, terminal result',
        () async {
      final controller = service.startReceive(anyReceive());
      expect(service.status, TransferServiceStatus.listening);
      expect(service.isListening, isTrue);
      final hex = _hexOf(backend.receiveStarted.single);

      backend.lines.addAll([
        {'job': hex, 'event': 'listening', 'addr': '127.0.0.1:52341'},
        {'job': hex, 'event': 'connected', 'peer': _fpHex64},
        {'job': hex, 'event': 'done'},
      ]);
      await _pumpUntil(() => controller.connections[0]?.addr != null);
      expect(controller.connections[0]!.addr, '127.0.0.1:52341');

      backend.finishOk(hex);
      await _pumpUntil(() => controller.hasResult);
      expect(controller.phase, JobPhase.done);
      expect(controller.report, isNotNull);
      expect(service.status, TransferServiceStatus.idle);
      expect(service.isListening, isFalse, reason: '单连接监听完成后即结束');
    });

    test('stopReceive cancels, folds the failure and clears the listener',
        () async {
      final controller = service.startReceive(anyReceive());
      final hex = _hexOf(backend.receiveStarted.single);
      backend.lines
          .add({'job': hex, 'event': 'listening', 'addr': '127.0.0.1:52341'});
      await _pumpUntil(() => controller.connections[0]?.addr != null);

      await service.stopReceive();
      expect(backend.cancelled, hasLength(1));
      expect(controller.hasResult, isTrue);
      expect(controller.error, contains('cancel'));
      expect(controller.phase, JobPhase.failed);
      expect(service.isListening, isFalse);
      expect(service.status, TransferServiceStatus.idle);
    });

    test('send: prefix fingerprint refused, full fingerprint runs to done',
        () async {
      expect(
        () => service.startSend(SendOptions(
          sourceRoots: const ['C:/pick/dir'],
          peer: '127.0.0.1:7100',
          fingerprint: 'abcd1234', // 8-hex prefix — needs the pairing book
        )),
        throwsStateError,
      );

      final controller = service.startSend(SendOptions(
        sourceRoots: const ['C:/pick/dir'],
        peer: '127.0.0.1:7100',
        fingerprint: _fpHex64,
      ));
      expect(service.status, TransferServiceStatus.busy);
      final hex = _hexOf(backend.sendStarted.single);
      backend.lines.addAll([
        {'job': hex, 'event': 'offered', 'files': 2, 'bytes': 4096},
        {'job': hex, 'event': 'done'},
      ]);
      backend.finishOk(hex, files: 2, bytes: 4096);
      await _pumpUntil(() => controller.hasResult);
      expect(controller.phase, JobPhase.done);
      expect(controller.report!.files, 2);
      expect(service.status, TransferServiceStatus.idle);
    });

    test('engine-open failure degrades to status=error, not a crash', () {
      final failing = FfiTransferService(
        backend: _FakeBackend(versionThrows: true),
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(failing.dispose);
      expect(failing.status, TransferServiceStatus.error);
      expect(failing.version(), contains('不可用'));
      expect(() => failing.startReceive(anyReceive()), throwsStateError);
    });
  });

  // -------------------------------------------------------------------------
  // Real-DLL loopback smoke (skipped when ak_ffi.dll is not found).
  // -------------------------------------------------------------------------

  final dllProbe = resolveAkDllPath();
  test(
    'real engine smoke: version, port-0 listening event, cancel path',
    () async {
      final service = RealTransferService(
        pollInterval: const Duration(milliseconds: 5),
      );
      final scratch = Directory.systemTemp.createTempSync('ak_svc_smoke_');
      try {
        expect(service.version(), contains('ak-ffi'));
        final jobId = service.startReceive(AkReceiveSpec(
          outDir: scratch.path,
          port: 0,
          expect: 'any',
          timeoutSecs: 30,
        ));
        final listening = await service.eventsFor(jobId)
            .firstWhere((event) => event is Listening)
            .timeout(const Duration(seconds: 15));
        expect(listening is Listening, isTrue,
            reason: 'port 0 must surface a listening event');
        service.cancel(jobId);
        final deadline = DateTime.now().add(const Duration(seconds: 15));
        var result = service.result(jobId);
        while (result.isRunning && DateTime.now().isBefore(deadline)) {
          await Future<void>.delayed(const Duration(milliseconds: 20));
          result = service.result(jobId);
        }
        expect(result.isFailed, isTrue, reason: 'cancel must fail the job');
        expect(result.error, contains('cancel'));
      } finally {
        service.dispose();
        try {
          scratch.deleteSync(recursive: true);
        } on Exception {
          // best-effort temp cleanup
        }
      }
    },
    skip: dllProbe == null
        ? 'ak_ffi.dll 未找到（先 cargo build -p ak-ffi --release）'
        : false,
  );
}

/// In-memory engine double: queued progress lines pop one per poll; started
/// job ids are recorded; cancel pins a failed result for that id. With
/// [versionThrows] the engine "fails to open" (the service constructor
/// probes it with a version() round-trip).
class _FakeBackend implements EngineBackend {
  _FakeBackend({this.versionThrows = false});

  final bool versionThrows;

  final List<Map<String, Object?>> lines = [];
  final List<Uint8List> sendStarted = [];
  final List<Uint8List> receiveStarted = [];
  final List<Uint8List> cancelled = [];
  final Map<String, AkJobResult> results = {};
  bool disposed = false;
  bool polledAtLeastOnce = false;

  int _cursor = 0;
  int _seq = 0;

  /// Pins a success result body for [hex] (engine shape).
  void finishOk(String hex, {int files = 1, int bytes = 4096}) {
    results[hex] = AkJobResult.done(<String, dynamic>{
      'job': hex,
      'wire_job': hex,
      'files': files,
      'bytes': bytes,
      'duration_ms': 12,
      'resumed': 0,
      'peer': _fpHex64,
      'roots': <String>['C:/inbox'],
    });
  }

  @override
  String version() {
    if (versionThrows) {
      throw ArgumentError('Failed to load dynamic library ak_ffi.dll');
    }
    return 'ak-ffi 1 fake (wire v1, caps 0x0007)';
  }

  @override
  Uint8List sendStart(AkSendSpec spec) {
    final id = _newId();
    sendStarted.add(id);
    return id;
  }

  @override
  Uint8List receiveStart(AkReceiveSpec spec) {
    final id = _newId();
    receiveStarted.add(id);
    return id;
  }

  @override
  Map<String, Object?>? pollProgress() {
    polledAtLeastOnce = true;
    return _cursor < lines.length ? lines[_cursor++] : null;
  }

  @override
  AkJobResult jobResult(Uint8List job) =>
      results[_hexOf(job)] ?? const AkJobResult.running();

  @override
  void cancel(Uint8List job) {
    cancelled.add(job);
    final hex = _hexOf(job);
    results[hex] = AkJobResult.failed(<String, dynamic>{
      'job': hex,
      'error': 'cancelled by caller',
    });
  }

  @override
  void dispose() => disposed = true;

  Uint8List _newId() => (Uint8List(16)..[15] = ++_seq);
}

String _hexOf(Uint8List bytes) =>
    bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

Future<void> _pumpUntil(
  bool Function() condition, {
  Duration timeout = const Duration(seconds: 5),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) {
      fail('condition not reached within $timeout');
    }
    await Future<void>.delayed(const Duration(milliseconds: 1));
  }
}
