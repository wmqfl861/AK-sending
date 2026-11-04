/// Wave-11 service-layer tests: DLL three-tier probe (pure), poll-pump event
/// routing over a fake engine backend, lifecycle/registry contracts, plus a
/// real-DLL loopback smoke that skips when ak_ffi.dll is absent.
library;

import 'dart:async' show Completer;
import 'dart:io';
import 'dart:typed_data';

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkJobResult, AkReceiveSpec, AkSendSpec, JobPhase, Listening, ProgressEvent;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/ak_engine_facade.dart';
import 'package:ak_sending_app/services/dll_path.dart';
import 'package:ak_sending_app/services/ffi_transfer_service.dart';
import 'package:ak_sending_app/services/real_transfer_service.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart'
    show
        AdmissionMode,
        JobDirection,
        JobRecordState,
        ReceiveOptions,
        SendOptions,
        TransferServiceStatus;

import 'fake_transfer_service.dart' show FakeAkEngineFacade, kPeerFingerprint;

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
  // Android platform adaptation (dll_path tier-0 + service-layer dirs).
  // -------------------------------------------------------------------------

  group('Android 适配（dll_path tier-0 / 服务目录）', () {
    test('android branch: bundled libak_ffi.so short-circuits every tier', () {
      final resolution = resolveAkDllPath(
        isAndroid: true,
        // Even an explicit AK_FFI_DLL must not win — no env vars on Android.
        environment: const {'AK_FFI_DLL': 'X:/custom/ak_ffi.dll'},
        resolvedExecutable: '/fake/exe/app',
        workingDirectory: '/fake/cwd',
        exists: (path) => throw StateError('must not touch the filesystem'),
      );
      expect(resolution!.path, androidAkLibName);
      expect(resolution.source, DllSource.bundled);
      // Probe diagnostics on Android name only the bundled SONAME.
      expect(akDllProbeCandidates(isAndroid: true), const [androidAkLibName]);
    });

    test('defaultAkDllFileName: android names the packaged cdylib', () {
      expect(defaultAkDllFileName(isAndroid: true), androidAkLibName);
    });

    test('resolveDefaultReceiveOutDir: tier priority, guards, desktop empty',
        () async {
      // Desktop: no default — the receive page keeps its typed-path UX.
      expect(
        await FfiTransferService.resolveDefaultReceiveOutDir(isAndroid: false),
        '',
      );

      // Tier 1 wins verbatim: path_provider's Android downloads directory IS
      // the app-specific .../files/Download (no extra subdirectory).
      expect(
        await FfiTransferService.resolveDefaultReceiveOutDir(
          isAndroid: true,
          downloadsDirectory: () async =>
              Directory('/storage/emulated/0/Android/data/dev.ak/files/Download'),
        ),
        '/storage/emulated/0/Android/data/dev.ak/files/Download',
      );

      // Tier 2: app-specific external root gains a Download/ subdirectory;
      // a throwing downloads provider falls through instead of failing.
      expect(
        await FfiTransferService.resolveDefaultReceiveOutDir(
          isAndroid: true,
          downloadsDirectory: () async => throw StateError('no channel'),
          externalStorageDirectory: () async =>
              Directory('/storage/emulated/0/Android/data/dev.ak/files'),
        ),
        '/storage/emulated/0/Android/data/dev.ak/files/Download',
      );

      // Tier 3: internal app documents fallback.
      expect(
        await FfiTransferService.resolveDefaultReceiveOutDir(
          isAndroid: true,
          downloadsDirectory: () async => null,
          externalStorageDirectory: () async => null,
          applicationDocumentsDirectory: () async =>
              Directory('/data/user/0/dev.ak/app_flutter'),
        ),
        '/data/user/0/dev.ak/app_flutter/Download',
      );

      // Last resort when every provider is absent: relative 'Download'.
      expect(
        await FfiTransferService.resolveDefaultReceiveOutDir(
          isAndroid: true,
          downloadsDirectory: () async => null,
          externalStorageDirectory: () async => null,
          applicationDocumentsDirectory: () async => null,
        ),
        'Download',
      );
    });

    test('android identity dir: refreshIdentity/listJobs gate on the resolve '
        'and re-anchor every facade call', () async {
      final gate = Completer<String>();
      final facade = FakeAkEngineFacade(jobsRows: [
        <String, dynamic>{
          'id': '0123456789abcdef',
          'direction': 'receive',
          'state': 'completed',
          'files': 1,
          'bytes': 10,
          'updated_unix_ms': 1762000000000,
        },
      ]);
      final service = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: facade,
        pollInterval: const Duration(milliseconds: 1),
        isAndroid: true,
        androidIdentityDir: () => gate.future,
      );
      addTearDown(service.dispose);

      final identity = service.refreshIdentity();
      final jobs = service.listJobs();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(facade.idDirRequests, isEmpty, reason: '仍被 identityDirReady 门控');

      gate.complete('/data/user/0/dev.ak/files/.ak-sending');
      final info = await identity;
      expect(info.fingerprintHex, 'ab' * 32);
      expect(await jobs, hasLength(1));
      // Every facade request used the resolved dir — never the provisional.
      expect(
        facade.idDirRequests.map((request) => request.$2).toSet(),
        {'/data/user/0/dev.ak/files/.ak-sending'},
      );
      expect(service.identityDir, '/data/user/0/dev.ak/files/.ak-sending');
    });

    test('android identity dir: user override survives the late resolve; '
        'empty reset lands on the resolved dir', () async {
      final gate = Completer<String>();
      final service = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: FakeAkEngineFacade(),
        pollInterval: const Duration(milliseconds: 1),
        isAndroid: true,
        androidIdentityDir: () => gate.future,
      );
      addTearDown(service.dispose);

      // Pin a directory before the async resolve lands.
      service.setIdentityDir('/sdcard/custom-ids');
      gate.complete('/data/user/0/dev.ak/files/.ak-sending');
      await service.identityDirReady;
      expect(service.identityDir, '/sdcard/custom-ids',
          reason: '用户显式选择不可被平台解析覆盖');

      // Empty reset returns to the resolved platform default.
      service.setIdentityDir('  ');
      expect(service.identityDir, '/data/user/0/dev.ak/files/.ak-sending');
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
    late FakeAkEngineFacade facade;
    late FfiTransferService service;

    setUp(() {
      backend = _FakeBackend();
      facade = FakeAkEngineFacade();
      service = FfiTransferService(
        backend: backend,
        engineFacade: facade,
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

    test('identity: cached deviceInfo degrades until refreshed; empty '
        'history passthrough', () async {
      expect(service.version(), contains('fake'));
      final info = service.deviceInfo();
      expect(info.deviceName, isNotEmpty); // real hostname
      expect(info.fingerprintHex, isEmpty); // pre-refresh snapshot
      expect(info.pairingCode, isEmpty);
      expect(info.peersCount, 0);
      expect(service.status, TransferServiceStatus.idle);
      expect(service.listJobs(), completion(isEmpty));
    });

    test('identityInfo (normal form) fills fingerprint/code/peersCount', () async {
      final normal = FakeAkEngineFacade(identityInfo: {
        'device_id': '6c310412',
        'fingerprint': '6C' * 32, // engine may send uppercase
        'pairing_code': '123456',
        'peers': 3,
        'created': false,
      });
      final wired = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: normal,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(wired.dispose);

      final info = await wired.refreshIdentity();
      expect(info.fingerprintHex, '6c' * 32); // lowercased
      expect(info.pairingCode, '123456');
      expect(info.peersCount, 3);
      expect(info.identityCreated, isFalse);
      expect(wired.deviceInfo(), same(info)); // cached
      expect(
        normal.idDirRequests.single,
        ('identityInfo', wired.identityDir),
      );
      expect(wired.identityDir, FfiTransferService.defaultIdentityDir());
    });

    test('identityInfo (created form) reports the first-run hint', () async {
      final created = FakeAkEngineFacade(identityInfo: {
        'device_id': 'abababab',
        'fingerprint': 'ab' * 32,
        'pairing_code': '654321',
        'peers': 0,
        'created': true,
      });
      final wired = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: created,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(wired.dispose);

      final info = await wired.refreshIdentity();
      expect(info.identityCreated, isTrue);
      expect(info.peersCount, 0);
    });

    test('identityInfo (DLL missing) degrades instead of throwing', () async {
      final broken = FakeAkEngineFacade(identityThrows: true);
      final wired = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: broken,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(wired.dispose);

      final info = await wired.refreshIdentity();
      expect(info.deviceName, isNotEmpty); // hostname fallback
      expect(info.fingerprintHex, isEmpty);
      expect(info.pairingCode, isEmpty);
      expect(info.peersCount, 0);
      expect(info.identityCreated, isFalse);
      expect(wired.deviceInfo(), same(info));
    });

    test('setIdentityDir drives subsequent facade calls and job specs',
        () async {
      service.setIdentityDir('D:/ids/ak');
      await service.refreshIdentity();
      expect(service.identityDir, 'D:/ids/ak');
      expect(
        facade.idDirRequests.last,
        ('identityInfo', 'D:/ids/ak'),
      );

      service.startSend(SendOptions(
        sourceRoots: const ['C:/pick/dir'],
        peer: '127.0.0.1:7100',
        fingerprint: _fpHex64,
      ));
      expect(backend.sendSpecs.single.idDir, 'D:/ids/ak');

      // Empty resets to the platform default.
      service.setIdentityDir('  ');
      expect(service.identityDir, FfiTransferService.defaultIdentityDir());
    });

    test('book admission forwards expect:"book" to the engine (id_dir '
        'wired), no local refusal', () {
      final controller = service.startReceive(const ReceiveOptions(
        outDir: 'C:/inbox',
        admission: AdmissionMode.book,
      ));
      expect(controller, isNotNull);
      expect(service.isListening, isTrue);
      expect(backend.receiveSpecs.single.expect, 'book');
      expect(backend.receiveSpecs.single.idDir, service.identityDir);
    });

    test('empty-book engine error propagates to the caller (page copy)',
        () async {
      backend.receiveStartError =
          StateError('配对簿为空（peers.book 无条目）：请先完成一次配对');
      expect(
        () => service.startReceive(const ReceiveOptions(
          outDir: 'C:/inbox',
          admission: AdmissionMode.book,
        )),
        throwsA(isA<StateError>()
            .having((e) => e.message, 'message', contains('配对簿为空'))),
      );
      expect(service.isListening, isFalse);
      backend.receiveStartError = null;
    });

    test('jobsList rows map to JobRecord, newest first', () async {
      final rowsFacade = FakeAkEngineFacade(jobsRows: [
        <String, dynamic>{
          'id': '0123456789abcdef0123456789abcdef',
          'direction': 'receive',
          'state': 'failed_storage_full', // engine's failed* family
          'files': 0,
          'bytes': 0,
          'updated_unix_ms': 1762000000000,
          'peer': _fpHex64,
          'root': r'C:\inbox',
        },
        <String, dynamic>{
          'id': 'fedcba9876543210',
          'direction': 'send',
          'state': 'completed',
          'files': 12,
          'bytes': 3 * 1024 * 1024 * 1024,
          'updated_unix_ms': 1762100000000,
          'peer': '', // engine sends "" when a record has no peer
          'root': r'D:\photos',
        },
        <String, dynamic>{
          'id': 'aabbccdd',
          'direction': 'receive',
          'state': 'transferring',
          'files': 1,
          'bytes': 512,
          'updated_unix_ms': 1761900000000,
          'peer': '',
          'root': r'C:\inbox',
        },
      ]);
      final wired = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: rowsFacade,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(wired.dispose);

      final jobs = await wired.listJobs();
      expect(jobs, hasLength(3));
      // Newest (larger updated_unix_ms) first.
      expect(jobs.first.id, 'fedcba98'); // shortened display id
      expect(jobs.first.direction, JobDirection.send);
      expect(jobs.first.state, JobRecordState.done); // completed → done
      expect(jobs.first.files, 12);
      expect(jobs.first.bytes, 3 * 1024 * 1024 * 1024);
      expect(jobs.first.startedAt,
          DateTime.fromMillisecondsSinceEpoch(1762100000000));
      expect(jobs.first.peer, isNull); // "" normalized to null
      expect(jobs.first.root, r'D:\photos');
      expect(jobs[1].id, '01234567');
      expect(jobs[1].direction, JobDirection.receive);
      expect(jobs[1].state, JobRecordState.failed); // failed* → failed
      expect(jobs[1].peer, _fpHex64);
      expect(jobs[1].root, r'C:\inbox');
      expect(jobs.last.state, JobRecordState.running); // in-flight record
      expect(
        rowsFacade.idDirRequests.single,
        ('jobsList', wired.identityDir),
      );
    });

    test('jobsList empty and error forms degrade to an empty history',
        () async {
      final empty = FakeAkEngineFacade();
      final emptyService = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: empty,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(emptyService.dispose);
      expect(await emptyService.listJobs(), isEmpty);

      final broken = FakeAkEngineFacade(jobsThrows: true);
      final brokenService = FfiTransferService(
        backend: _FakeBackend(),
        engineFacade: broken,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(brokenService.dispose);
      expect(await brokenService.listJobs(), isEmpty);
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

    test('engine-open failure degrades to status=error, not a crash',
        () async {
      final failing = FfiTransferService(
        backend: _FakeBackend(versionThrows: true),
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(failing.dispose);
      expect(failing.status, TransferServiceStatus.error);
      expect(failing.version(), contains('不可用'));
      expect(() => failing.startReceive(anyReceive()), throwsStateError);
      // Identity and history also degrade honestly without the engine.
      final degraded = await failing.refreshIdentity();
      expect(degraded.fingerprintHex, isEmpty);
      expect(await failing.listJobs(), isEmpty);
    });
  });

  // -------------------------------------------------------------------------
  // FfiTransferService pairing book (wave-14, fake engine facade).
  // -------------------------------------------------------------------------

  group('FfiTransferService pairing book (wave-14)', () {
    FfiTransferService wired(
      FakeAkEngineFacade facade, {
      _FakeBackend? backend,
    }) {
      final service = FfiTransferService(
        backend: backend ?? _FakeBackend(),
        engineFacade: facade,
        pollInterval: const Duration(milliseconds: 1),
      );
      addTearDown(service.dispose);
      return service;
    }

    test('peerList rows map to UI entries sorted by fingerprint, through '
        'the configured id_dir', () async {
      final facade = FakeAkEngineFacade(peerRows: [
        <String, dynamic>{
          'fingerprint': 'CD' * 32, // engine may send uppercase
          'name': 'laptop',
          'paired_unix_ms': 1730000000000,
        },
        <String, dynamic>{
          'fingerprint': kPeerFingerprint,
          'name': '(unnamed)',
          'paired_unix_ms': 0,
        },
      ]);
      final service = wired(facade);
      service.setIdentityDir('D:/ids/ak');

      final peers = await service.listPeers();
      expect(peers, hasLength(2));
      // Sorted by fingerprint: 6c31... before cdcd....
      expect(peers.first.fingerprint, kPeerFingerprint);
      expect(peers.first.name, '(unnamed)');
      expect(peers.first.pairedAt,
          DateTime.fromMillisecondsSinceEpoch(0));
      expect(peers.last.fingerprint, 'cd' * 32); // lowercased
      expect(peers.last.name, 'laptop');
      expect(facade.idDirRequests.single, ('peerList', 'D:/ids/ak'));
    });

    test('listPeers propagates a corrupt book and an unusable engine '
        'instead of degrading to an empty list', () async {
      final corrupt = FakeAkEngineFacade(
        peerListError: StateError('cannot load peer book: peers.book 损坏'),
      );
      await expectLater(wired(corrupt).listPeers(), throwsStateError);

      // Engine open failure: the honest 传输引擎不可用 StateError.
      final failing =
          wired(FakeAkEngineFacade(), backend: _FakeBackend(versionThrows: true));
      await expectLater(failing.listPeers(), throwsStateError);
    });

    test('addPeer normalizes input, writes through the facade and refreshes '
        'the identity (home-card linkage)', () async {
      final facade = FakeAkEngineFacade();
      final service = wired(facade);
      await service.refreshIdentity();
      facade.idDirRequests.clear();

      final count = await service.addPeer('6C' * 32, '  笔记本  ');
      expect(count, 1);
      expect(
        facade.idDirRequests.where((r) => r.$1 == 'peerAdd').single,
        ('peerAdd', service.identityDir),
      );
      // The identity re-read after the write keeps 已配对设备 in sync.
      expect(facade.idDirRequests.last.$1, 'identityInfo');
      final peers = await service.listPeers();
      expect(peers.single.fingerprint, '6c' * 32); // lowercased
      expect(peers.single.name, '笔记本'); // trimmed
    });

    test('addPeer with a blank name pairs as (unnamed); malformed input '
        'never reaches the facade', () async {
      final facade = FakeAkEngineFacade();
      final service = wired(facade);

      expect(await service.addPeer(kPeerFingerprint, '   '), 1);
      expect((await service.listPeers()).single.name, '(unnamed)');

      // 8-hex prefix, non-hex junk and a 63-hex run are all local rejects.
      for (final bad in ['6c310412', 'zz' * 32, 'a' * 63]) {
        await expectLater(service.addPeer(bad, 'x'), throwsFormatException);
      }
      expect(
        facade.idDirRequests.where((r) => r.$1 == 'peerAdd'),
        hasLength(1),
        reason: '只有合法的那一次写入到达引擎',
      );
    });

    test('removePeer maps the engine -4 not-found to false; success '
        'refreshes the identity', () async {
      final facade = FakeAkEngineFacade(peerRows: [
        <String, dynamic>{
          'fingerprint': kPeerFingerprint,
          'name': 'laptop',
          'paired_unix_ms': 1730000000000,
        },
      ]);
      final service = wired(facade);
      await service.refreshIdentity();
      facade.idDirRequests.clear();

      expect(await service.removePeer(kPeerFingerprint), isTrue);
      expect(
        facade.idDirRequests.where((r) => r.$1 == 'peerRemove').single,
        ('peerRemove', service.identityDir),
      );
      expect(facade.idDirRequests.last.$1, 'identityInfo');
      // The peer is gone: the engine's -4 form becomes a plain false.
      expect(await service.removePeer(kPeerFingerprint), isFalse);
      expect(() => service.removePeer('abcd1234'), throwsFormatException);
      expect(await service.listPeers(), isEmpty);
    });

    test('the real AkEngineDartFacade peer seam drives the real DLL '
        '(wave-14 integration switched on)',
        () {
      final facade = AkEngineDartFacade();
      addTearDown(facade.dispose);
      // The DLL probe finds <repo>/target/release/ak_ffi.dll from the test
      // CWD, so this exercises the full seam: facade -> AkEngineDart -> C ABI.
      final idDir = Directory.systemTemp.createTempSync('ak-facade-peers-');
      addTearDown(() => idDir.deleteSync(recursive: true));
      expect(facade.peerList(idDir.path), isEmpty);
      final fp = 'ab' * 32;
      expect(facade.peerAdd(idDir.path, fp, name: 'host-a'), 1);
      final peers = facade.peerList(idDir.path);
      expect(peers, hasLength(1));
      expect(peers.first['fingerprint'], fp);
      expect(peers.first['name'], 'host-a');
      expect(facade.peerRemove(idDir.path, fp), 0);
      expect(facade.peerList(idDir.path), isEmpty);
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
  final List<AkReceiveSpec> receiveSpecs = [];
  final List<AkSendSpec> sendSpecs = [];
  final List<Uint8List> cancelled = [];
  final Map<String, AkJobResult> results = {};
  bool disposed = false;
  bool polledAtLeastOnce = false;

  /// When set, [receiveStart] throws it (engine-side rejection, e.g. an
  /// empty pairing book surfaced through ak_engine_last_error).
  Object? receiveStartError;

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
    sendSpecs.add(spec);
    return id;
  }

  @override
  Uint8List receiveStart(AkReceiveSpec spec) {
    final error = receiveStartError;
    if (error != null) {
      throw error;
    }
    final id = _newId();
    receiveStarted.add(id);
    receiveSpecs.add(spec);
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
