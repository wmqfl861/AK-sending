/// T1 (batch T): the app itself as the receiver, on the Android emulator.
///
/// Why this test exists (S 复核 / T 交接 §2.3): S1's retest exercised the
/// adb-pushed CLI, which runs in the shell domain; the app runs in the
/// untrusted_app domain, and Android's SELinux policy forbids hard links
/// exactly for that class of domains — the S-N3 commit path (renameat2 with
/// RENAME_NOREPLACE) is what makes receiving work here at all. A host CLI
/// cannot dial into the emulator, so both ends run inside the app process
/// over a real QUIC loopback on 127.0.0.1 through the FFI boundary. This is
/// a same-machine emulator test, NOT a physical-device verification.
///
/// One round (selected via `--dart-define=AK_T1_MIB=<8|64>`, default 8):
///   1. fresh identity via engine.identityInfo in this round's temp dir;
///   2. source tree under getApplicationSupportDirectory(): one payload
///      file (AK_T1_MIB MiB, fixed-seed PRNG), one Chinese-named small
///      file, one empty directory, one two-level nested directory — never
///      any user file;
///   3. receiveStart (port 0, expect any) -> listening -> sendStart to
///      `127.0.0.1:<port>` pinning the receiver fingerprint;
///   4. both jobResult terminal; every file compared by size and SHA-256;
///      empty dir and nested layout checked; round dir deleted afterwards
///      (identity dir lives only on the device for the round).
///
/// Fingerprints are logged truncated to 8 hex chars; pairing codes are
/// never printed. Product code is untouched (T 交接 §4 T1).
library;

import 'dart:io' show Directory, File, FileMode;
import 'dart:math' as math show Random;
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngineDart, AkReceiveSpec, AkSendSpec;
import 'package:ak_sending_app/services/ak_engine_facade.dart'
    show AkEngineDartFacade;
import 'package:crypto/crypto.dart' show Digest, sha256;

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

const int _mib = int.fromEnvironment('AK_T1_MIB', defaultValue: 8);

void _log(String message) {
  // ignore: avoid_print
  print('[t1-app-receive] $message');
}

/// Accumulates the single Digest a chunked sha256 conversion emits.
class _DigestSink implements Sink<Digest> {
  Digest? digest;

  @override
  void add(Digest data) {
    digest = data;
  }

  @override
  void close() {}
}

Future<String> _sha256(Stream<List<int>> data) async {
  final sink = _DigestSink();
  final input = sha256.startChunkedConversion(sink);
  await for (final chunk in data) {
    input.add(chunk);
  }
  input.close();
  return (sink.digest ?? Digest(const [])).toString();
}

String _jobHex(Uint8List job) =>
    job.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

Map<String, int> _relativeFiles(Directory root) {
  final out = <String, int>{};
  for (final entity in root.listSync(recursive: true, followLinks: false)) {
    if (entity is File) {
      final rel = entity.path
          .substring(root.path.length + 1)
          .replaceAll('\\', '/');
      out[rel] = entity.lengthSync();
    }
  }
  return out;
}

Future<int> _awaitListening(AkEngineDart engine, Uint8List recvJob) async {
  final recvHex = _jobHex(recvJob);
  final deadline = DateTime.now().add(const Duration(seconds: 30));
  while (true) {
    final event = engine.pollProgress();
    if (event != null && event['job'] == recvHex) {
      if (event['event'] == 'failed') {
        fail('receiver failed: $event');
      }
      if (event['event'] == 'listening') {
        final addr = event['addr'] as String;
        return int.parse(addr.split(':').last);
      }
    }
    if (DateTime.now().isAfter(deadline)) {
      fail('no listening event within 30 s');
    }
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

Future<Map<String, dynamic>> _awaitDone(
    AkEngineDart engine, Uint8List job, String side) async {
  final events = <Map<String, dynamic>>[];
  final deadline = DateTime.now().add(const Duration(seconds: 180));
  while (true) {
    final event = engine.pollProgress();
    if (event != null) {
      events.add(event);
      if (event['event'] == 'failed') {
        fail('$side failed: $event');
      }
    }
    final result = engine.jobResult(job);
    if (result.isDone) {
      return result.map!;
    }
    if (DateTime.now().isAfter(deadline)) {
      fail('$side did not finish within 180 s (events: $events)');
    }
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'app process receives over QUIC loopback via FFI (S-N3, $_mib MiB)',
    (tester) async {
      final facade = AkEngineDartFacade();
      AkEngineDart? engine;
      try {
        engine = facade.borrowEngine(); // throws when the .so cannot open
        final version = engine.version();
        _log('engine.version(): $version');
        expect(version, contains('0x0017'),
            reason: 'the bundled libak_ffi.so must carry S-N1 (T 交接 §2.2)');

        final support = await getApplicationSupportDirectory();
        final round = Directory(
            '${support.path}/t1-${_mib}mib-${DateTime.now().millisecondsSinceEpoch}');
        round.createSync(recursive: true);
        final src = Directory('${round.path}/src')..createSync(recursive: true);
        final dst = Directory('${round.path}/dst')..createSync(recursive: true);
        // Does NOT exist yet — identityInfo creates it; lives only on the
        // device and dies with the round.
        final idDir = Directory('${round.path}/id');

        // --- source tree: deterministic, no user files --------------------
        final payload = File('${src.path}/payload.bin');
        final random = math.Random(0xA11CE + _mib);
        final chunk = List<int>.generate(
            1024 * 1024, (_) => random.nextInt(256),
            growable: false);
        final writer = payload.openSync(mode: FileMode.write);
        for (var i = 0; i < _mib; i++) {
          writer.writeFromSync(chunk);
        }
        writer.closeSync();
        const chineseName = '说明-中文文件名.txt';
        File('${src.path}/$chineseName')
            .writeAsStringSync('AK-sending t1 app-receive round ($_mib MiB)\n');
        Directory('${src.path}/emptydir').createSync();
        Directory('${src.path}/nested/level2').createSync(recursive: true);
        File('${src.path}/nested/level2/inner.txt')
            .writeAsStringSync('nested payload\n');
        _log('source ready under support dir: payload $_mib MiB + '
            '$chineseName + emptydir + nested/level2');

        // --- receiver identity (this round only) --------------------------
        final identity = engine.identityInfo(idDir.path);
        final fp = identity.fingerprint;
        _log('receiver fingerprint (8): ${fp.substring(0, 8)}');
        expect(RegExp(r'^[0-9a-f]{64}$').hasMatch(fp), isTrue);

        // --- loopback transfer through the FFI boundary -------------------
        final recvJob = engine.receiveStart(AkReceiveSpec(
          outDir: dst.path,
          port: 0,
          expect: 'any',
          idDir: idDir.path,
          deviceName: 't1-app-recv',
          timeoutSecs: 120,
        ));
        final port = await _awaitListening(engine, recvJob);
        _log('receiver listening on 127.0.0.1:$port');

        final sendJob = engine.sendStart(AkSendSpec(
          sourceRoots: [src.path],
          peer: '127.0.0.1:$port',
          fpHex: fp,
          chunkKb: 64,
          deviceName: 't1-app-send',
          timeoutSecs: 120,
        ));

        final sendJson = await _awaitDone(engine, sendJob, 'send');
        final recvJson = await _awaitDone(engine, recvJob, 'recv');

        // 3 files + 3 directories (emptydir, nested, nested/level2) in the
        // manifest; count from the actual tree instead of a magic number.
        final expectedEntries =
            src.listSync(recursive: true, followLinks: false).length;
        expect(sendJson['files'], expectedEntries,
            reason: 'payload+chinese+inner files and 3 dirs');
        expect(recvJson['files'], sendJson['files']);
        expect(recvJson['bytes'], sendJson['bytes']);
        expect(sendJson['peer'], fp);
        _log('both jobs done: files=${sendJson['files']} '
            'bytes=${sendJson['bytes']} peer(8)=${fp.substring(0, 8)}');

        // --- byte-for-byte verdict ----------------------------------------
        final srcFiles = _relativeFiles(src);
        final dstFiles = _relativeFiles(dst);
        expect(dstFiles.keys.toSet(), srcFiles.keys.toSet(),
            reason: 'received tree must list exactly the source files');
        for (final rel in srcFiles.keys) {
          expect(dstFiles[rel], srcFiles[rel], reason: 'size mismatch: $rel');
          final a = await _sha256(File('${src.path}/$rel').openRead());
          final b = await _sha256(File('${dst.path}/$rel').openRead());
          expect(b, a, reason: 'SHA-256 mismatch: $rel');
          _log('$rel: ${srcFiles[rel]} bytes, sha256(8) ${a.substring(0, 8)}');
        }
        expect(Directory('${dst.path}/emptydir').existsSync(), isTrue,
            reason: 'empty directory must survive the transfer');
        expect(Directory('${dst.path}/nested/level2').existsSync(), isTrue);
        _log('PASS: $_mib MiB round — SHA-256 equal per file, layout equal');

        round.deleteSync(recursive: true); // includes the identity dir
      } finally {
        facade.dispose();
      }
    },
    timeout: const Timeout(Duration(minutes: 6)),
  );
}
