/// End-to-end tests for the pure-Dart bindings over the real `ak_ffi.dll`
/// (workspace `target/release`), mirroring the Rust-side `crates/ak-ffi/
/// tests/ffi.rs` claims: exact version string, Dart-side spec pre-validation,
/// Rust-side `-2` revalidation, a real loopback QUIC transfer through the
/// boundary, mid-transfer cancellation, the `-3` buffer retry semantics,
/// identity summaries (`ak_identity_info`), persisted job records
/// (`ak_jobs_list`), and `expect:"book"` admission from a pre-written
/// `peers.book`.
///
/// Receiver identities: the seeds below are the RFC 8032 Ed25519
/// test-vector seeds; the fingerprints are the first 32 bytes of
/// BLAKE3(public key), cross-checked against ed25519-dalek 2.2.0 + blake3
/// 1.8.7 (the exact versions in the workspace lockfile) at fixture time.
/// Writing the seed to `<id_dir>/device.key` pins the receiver identity the
/// engine will load, so the sender knows which fingerprint to pin.
library;

import 'dart:convert' show jsonDecode, utf8;

import 'dart:ffi';

import 'dart:io' show Directory, File, FileMode, sleep;

import 'dart:typed_data' show Uint8List;

import 'package:test/test.dart';

import 'package:ak_ffi_dart/src/bindings.dart';
import 'package:ak_ffi_dart/src/engine.dart';
import 'package:ak_ffi_dart/src/ffi_json.dart';

void main() {
  tearDown(() {
    for (final dir in _tempDirs) {
      try {
        dir.deleteSync(recursive: true);
      } on Exception {
        // best effort; temp dirs live under the system temp root
      }
    }
    _tempDirs.clear();
  });

  _declareTests();
}

const _versionString = 'ak-ffi 1 (wire v1, caps 0x0007)';

/// RFC 8032 test 1 seed; fp = blake3(d75a9801..511a)[0..32].
const _seed1Hex = '9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60';
const _fp1Hex = '6c31041268f471609c79f5f2dbcc38e4a4ab2f4d416109a4e09fcf50fd0f0062';

/// RFC 8032 test 2 seed; fp = blake3(3d4017c3..660c)[0..32].
const _seed2Hex = '4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb';
const _fp2Hex = '1027e035b26b605dc6d4b78d07dc29660fcc3498b598a2e57c4e6b1b673a1e95';

// ---------------------------------------------------------------------------
// Fixtures and helpers.
// ---------------------------------------------------------------------------

final _tempDirs = <Directory>[];

Directory _newTempDir(String tag) {
  final dir = Directory.systemTemp.createTempSync('ak_ffi_dart_${tag}_');
  _tempDirs.add(dir);
  return dir;
}

Uint8List _seedBytes(String hex) =>
    Uint8List.fromList(List.generate(32, (i) => int.parse(hex.substring(i * 2, i * 2 + 2), radix: 16)));

String _jobHex(Uint8List job) =>
    job.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();

/// Deterministic xorshift stream, identical in spirit to the Rust fixture.
class _Pattern {
  int _state;

  _Pattern(this._state);

  Uint8List bytes(int length) {
    final out = Uint8List(length);
    for (var i = 0; i < length; i++) {
      _state ^= _state << 13;
      _state ^= _state >>> 7;
      _state ^= _state << 17;
      out[i] = (_state >>> 33) & 0xFF;
    }
    return out;
  }
}

Matcher _throwsFfi(int code) => throwsA(
    isA<AkFfiException>().having((e) => e.code, 'code', code));

void _expectSpecError(Object Function() build, String needle) {
  expect(
    build,
    throwsA(isA<AkSpecException>()
        .having((e) => e.message, 'message', contains(needle))),
  );
}

/// Nested fixture tree: CRLF text, binary payloads, a deep path, an empty
/// dir, and a filename with spaces/semicolons. Returns (bytes, entries).
(int, int) _makeSourceTree(Directory base) {
  final pattern = _Pattern(0xFF11000000000002);
  void write(String rel, List<int> bytes) {
    final file = File('${base.path}/$rel');
    file.parent.createSync(recursive: true);
    file.writeAsBytesSync(bytes);
  }

  const crlf = 'line one\r\nline two\r\nno final newline';
  write('notes-crlf.txt', utf8.encode(crlf));
  final binary = pattern.bytes(160 * 1024);
  write('photos/img-0001.bin', binary);
  final tail = pattern.bytes(53 * 1024 + 7);
  write('photos/raw/img-0002.bin', tail);
  Directory('${base.path}/photos/empty-album').createSync(recursive: true);
  const deep = 'dart ffi loopback: byte-identical';
  write('docs/deep/manual; v2 (draft).txt', utf8.encode(deep));
  // 4 files + directories photos, photos/raw, photos/empty-album, docs,
  // docs/deep.
  return (binary.length + tail.length + crlf.length + deep.length, 9);
}

void _collectTree(Directory root, String prefix, Set<String> dirs,
    Map<String, Uint8List> files) {
  for (final entity in root.listSync()) {
    final name = entity.path.split(RegExp(r'[\\/]')).last;
    final rel = prefix.isEmpty ? name : '$prefix/$name';
    if (entity is Directory) {
      dirs.add(rel);
      _collectTree(entity, rel, dirs, files);
    } else if (entity is File) {
      files[rel] = entity.readAsBytesSync();
    } else {
      fail('unexpected dir entry: $entity');
    }
  }
}

/// Byte-exact tree comparison (dirs + files), first divergence reported.
void _assertTreesIdentical(Directory src, Directory dst) {
  final srcDirs = <String>{};
  final srcFiles = <String, Uint8List>{};
  final dstDirs = <String>{};
  final dstFiles = <String, Uint8List>{};
  _collectTree(src, '', srcDirs, srcFiles);
  _collectTree(dst, '', dstDirs, dstFiles);
  expect(srcDirs, equals(dstDirs), reason: 'directory structure differs');
  expect(srcFiles.keys, equals(dstFiles.keys), reason: 'file sets differ');
  for (final entry in srcFiles.entries) {
    expect(dstFiles[entry.key], equals(entry.value),
        reason: 'file contents differ for ${entry.key}');
  }
}

/// Wait for the receiver's `listening` event and return its port (the
/// receiver was started with port 0, so this is the only way to learn it).
int _awaitListening(AkEngineDart engine, Uint8List recvJob,
    List<Map<String, dynamic>> events) {
  final recvHex = _jobHex(recvJob);
  final deadline = DateTime.now().add(const Duration(seconds: 30));
  while (true) {
    final event = engine.pollProgress();
    if (event != null) {
      events.add(event);
      if (event['job'] == recvHex && event['event'] == 'listening') {
        final addr = event['addr'] as String;
        return int.parse(addr.split(':').last);
      }
    }
    if (DateTime.now().isAfter(deadline)) {
      fail('no listening event within 30 s (events so far: $events)');
    }
    sleep(const Duration(milliseconds: 5));
  }
}

/// Pump the progress queue and poll [jobs] until every one finished ok;
/// returns each report JSON keyed by job hex. A failed job fails the test.
Map<String, Map<String, dynamic>> _pumpUntilDone(
    AkEngineDart engine, List<Uint8List> jobs,
    [Duration budget = const Duration(seconds: 90)]) {
  final reports = <String, Map<String, dynamic>>{};
  final open = <String, Uint8List>{for (final job in jobs) _jobHex(job): job};
  final deadline = DateTime.now().add(budget);
  while (open.isNotEmpty) {
    engine.pollProgress(); // keep the queue drained
    open.removeWhere((hex, job) {
      final result = engine.jobResult(job);
      if (result.isDone) {
        reports[hex] = result.map!;
        return true;
      }
      if (result.isFailed) {
        fail('job $hex failed through the ABI: ${result.map}');
      }
      return false;
    });
    if (open.isEmpty) {
      break;
    }
    if (DateTime.now().isAfter(deadline)) {
      fail('jobs did not finish within ${budget.inSeconds} s: $open');
    }
    sleep(const Duration(milliseconds: 2));
  }
  return reports;
}

// ---------------------------------------------------------------------------
// Test declarations.
// ---------------------------------------------------------------------------

void _declareTests() {
// ---------------------------------------------------------------------------
// Version, status codes, last-error, lifecycle.
// ---------------------------------------------------------------------------

test('version string is exact; -3/-1 paths and last-error semantics hold',
    () {
  final bindings = AkFfiBindings();
  final engine = AkEngineDart();
  try {
    // The friendly API returns the exact frozen string.
    expect(engine.version(), _versionString);

    // Tiny buffer at the raw layer: -3, capacity hint, lossless reread.
    final tiny = processAllocator.allocate<Uint8>(4);
    try {
      expect(bindings.version(tiny, 4), AkStatus.errBuffer);
      final message = engine.lastError();
      expect(message, contains('buffer too small'));
      expect(message, contains('need'));
      // Reading does not clear the stored message.
      expect(engine.lastError(), message);

      // Null buffer: -1.
      expect(bindings.version(nullptr, 128), AkStatus.errNull);

      // A successful call writes payload + NUL and clears the slot.
      final buf = processAllocator.allocate<Uint8>(64);
      try {
        final length = bindings.version(buf, 64);
        expect(length, _versionString.length);
        expect(fromNativeUtf8(buf), _versionString);
        expect(buf[length], 0, reason: 'NUL terminator right after payload');
      } finally {
        processAllocator.free(buf);
      }
      expect(engine.lastError(), '');

      // engine_free(null) is a documented no-op.
      bindings.engineFree(nullptr);
    } finally {
      processAllocator.free(tiny);
    }
  } finally {
    engine.dispose();
  }
});

test('dispose is prompt, idempotent, and guards further use', () {
  final engine = AkEngineDart();
  expect(engine.version(), _versionString);
  engine.dispose();
  expect(engine.isDisposed, isTrue);
  engine.dispose(); // idempotent
  expect(() => engine.version(), throwsStateError);
  expect(() => engine.pollProgress(), throwsStateError);
  expect(() => engine.sendStartJson('{}'), throwsStateError);
  expect(
      () => engine.jobResult(Uint8List(16)), throwsStateError);
});

test('job ids must be 16 bytes (argument checks)', () {
  final engine = AkEngineDart();
  try {
    expect(() => engine.jobResult(Uint8List(5)), throwsArgumentError);
    expect(() => engine.cancel(Uint8List(17)), throwsArgumentError);
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// Dart-side spec pre-validation (friendly errors before the DLL).
// ---------------------------------------------------------------------------

AkSendSpec sendSpec({
  List<String>? roots,
  String peer = '127.0.0.1:7100',
  String fp = _fp1Hex,
  int? chunkKb,
  int? timeoutSecs,
  String? conflict,
  String? deviceName,
}) =>
    AkSendSpec(
      sourceRoots: roots ?? const ['C:/pick/dir'],
      peer: peer,
      fpHex: fp,
      chunkKb: chunkKb,
      timeoutSecs: timeoutSecs,
      conflict: conflict,
      deviceName: deviceName,
    );

test('send spec: Dart-side mirror rejects every invalid branch', () {
  _expectSpecError(() => sendSpec(roots: const []).toJsonString(), 'source_roots');
  _expectSpecError(
      () => sendSpec(roots: const ['']).toJsonString(), 'non-empty paths');
  _expectSpecError(
      () => sendSpec(roots: const ['C:/ok', 'C:/bad\ttab']).toJsonString(),
      'control');
  _expectSpecError(
      () => sendSpec(peer: 'localhost:7100').toJsonString(), 'ip:port');
  _expectSpecError(() => sendSpec(peer: '127.0.0.1').toJsonString(), 'ip:port');
  _expectSpecError(
      () => sendSpec(peer: '127.0.0.1:99999').toJsonString(), 'ip:port');
  _expectSpecError(
      () => sendSpec(peer: ' 127.0.0.1:7100').toJsonString(), 'ip:port');
  _expectSpecError(() => sendSpec(fp: 'ab' * 31).toJsonString(), '64 hex');
  _expectSpecError(
      () => sendSpec(fp: 'ab' * 32 + 'zz').toJsonString(), '64 hex');
  _expectSpecError(
      () => sendSpec(chunkKb: 0).toJsonString(), 'between 1 and 8192');
  _expectSpecError(
      () => sendSpec(chunkKb: 8193).toJsonString(), 'between 1 and 8192');
  _expectSpecError(
      () => sendSpec(timeoutSecs: 0).toJsonString(), 'timeout_secs');
  _expectSpecError(
      () => sendSpec(timeoutSecs: 86401).toJsonString(), 'timeout_secs');
  _expectSpecError(
      () => sendSpec(conflict: 'overwrite').toJsonString(), '"fail" or "rename"');
  _expectSpecError(
      () => sendSpec(deviceName: 'x' * 65).toJsonString(), '64 characters');
  _expectSpecError(
      () => sendSpec(deviceName: 'two\nlines').toJsonString(), 'control');

  // Valid minimal spec: exactly the required fields, compact output.
  final json = sendSpec().toJsonString();
  expect(jsonDecode(json), <String, dynamic>{
    'source_roots': const ['C:/pick/dir'],
    'peer': '127.0.0.1:7100',
    'fp_hex': _fp1Hex,
  });
  // Uppercase fingerprints pass (Rust parses case-insensitively).
  expect(jsonDecode(sendSpec(fp: _fp1Hex.toUpperCase()).toJsonString())['fp_hex'],
      _fp1Hex.toUpperCase());
  // Compact separators: no space after ':' or ','.
  final spaced = sendSpec(deviceName: 'with space', timeoutSecs: 5).toJsonString();
  expect(spaced, isNot(contains('": ')));
  expect(spaced, isNot(contains(', "')));
});

test('receive spec: Dart-side mirror rejects every invalid branch', () {
  AkReceiveSpec recv({
    String? outDir,
    int? port,
    String? expect,
    int? onceJobs,
    int? timeoutSecs,
    String? conflict,
    String? deviceName,
  }) =>
      AkReceiveSpec(
        outDir: outDir ?? 'C:/inbox',
        port: port,
        expect: expect,
        onceJobs: onceJobs,
        timeoutSecs: timeoutSecs,
        conflict: conflict,
        deviceName: deviceName,
      );

  _expectSpecError(() => recv(outDir: '').toJsonString(), '"out_dir"');
  _expectSpecError(
      () => recv(outDir: 'C:/in\nbad').toJsonString(), 'control');
  _expectSpecError(() => recv(port: 65536).toJsonString(), '"port"');
  _expectSpecError(() => recv(port: -1).toJsonString(), '"port"');
  _expectSpecError(() => recv(onceJobs: 0).toJsonString(), 'between 1 and 64');
  _expectSpecError(() => recv(onceJobs: 65).toJsonString(), 'between 1 and 64');
  _expectSpecError(
      () => recv(port: 0, onceJobs: 2).toJsonString(), 'non-zero "port"');
  _expectSpecError(() => recv(expect: 'nope').toJsonString(), '"expect"');
  _expectSpecError(() => recv(expect: '12345').toJsonString(), '"expect"');
  _expectSpecError(
      () => recv(timeoutSecs: 86401).toJsonString(), 'timeout_secs');
  _expectSpecError(
      () => recv(conflict: 'clobber').toJsonString(), '"fail" or "rename"');
  _expectSpecError(
      () => recv(deviceName: 'y' * 65).toJsonString(), '64 characters');

  // Valid defaults: only out_dir is emitted.
  expect(jsonDecode(recv().toJsonString()), <String, dynamic>{
    'out_dir': 'C:/inbox',
  });
  // Port 0 (ephemeral), case-insensitive any, 6-digit codes, 64-hex and
  // once_jobs > 1 with an explicit port all pass.
  expect(
    jsonDecode(recv(port: 0, expect: 'ANY').toJsonString()),
    <String, dynamic>{'out_dir': 'C:/inbox', 'port': 0, 'expect': 'ANY'},
  );
  expect(jsonDecode(recv(expect: '123-456').toJsonString())['expect'],
      '123-456');
  expect(
      jsonDecode(recv(expect: 'ab' * 32).toJsonString())['expect'], 'ab' * 32);
  expect(
    jsonDecode(recv(port: 7100, onceJobs: 2).toJsonString()),
    <String, dynamic>{
      'out_dir': 'C:/inbox',
      'port': 7100,
      'once_jobs': 2,
    },
  );
});

// ---------------------------------------------------------------------------
// Rust-side revalidation (-2) through the raw-JSON escape hatch.
// ---------------------------------------------------------------------------

test('rust side revalidates raw spec JSON with -2', () {
  final engine = AkEngineDart();
  final outDir = _newTempDir('rust2');
  // Raw JSON must escape Windows separators itself: use forward slashes.
  final out = outDir.path.replaceAll('\\', '/');

  void expectInvalid(void Function() call, String needle) {
    try {
      call();
      fail('expected an AkFfiException');
    } on AkFfiException catch (e) {
      expect(e.code, AkStatus.errInvalid, reason: e.message);
      expect(e.message, contains(needle));
    }
  }

  try {
    expectInvalid(
      () => engine.sendStartJson(
          '{"source_roots":["C:/definitely/not/here"],"peer":"127.0.0.1:1","fp_hex":"ab"}'),
      'fp_hex',
    );
    expectInvalid(
      () => engine.sendStartJson(
          '{"source_roots":["C:/x"],"peer":"127.0.0.1:1","fp_hex":"${'ab' * 31}"}'),
      '64 hex',
    );
    expectInvalid(
      () => engine.sendStartJson(
          '{"source_roots":["C:/x"],"peer":123,"fp_hex":"${'ab' * 32}"}'),
      'peer',
    );
    expectInvalid(
      () => engine.receiveStartJson('{"out_dir":"$out","port":70000}'),
      'port',
    );
    expectInvalid(
      () => engine.receiveStartJson(
          '{"out_dir":"$out","once_jobs":2,"port":0}'),
      'once_jobs',
    );
    // Malformed JSON: code only (the parser's message text is its own).
    try {
      engine.receiveStartJson('{"out_dir":');
      fail('expected an AkFfiException');
    } on AkFfiException catch (e) {
      expect(e.code, AkStatus.errInvalid, reason: e.message);
    }
    // Errors surfaced through lastError as well (not cleared by reading).
    expect(engine.lastError(), isNot(''));
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// Real loopback transfer through the boundary.
// ---------------------------------------------------------------------------

test('loopback transfer: listening..done, equal reports, identical bytes',
    timeout: const Timeout(Duration(seconds: 240)), () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('loop');
    final src = Directory('${base.path}/ffi-source')
      ..createSync(recursive: true);
    final (totalBytes, totalEntries) = _makeSourceTree(src);
    final idDir = _newTempDir('id');
    File('${idDir.path}/device.key').writeAsBytesSync(_seedBytes(_seed1Hex));
    final dst = _newTempDir('dst');

    final recvJob = engine.receiveStart(AkReceiveSpec(
      outDir: dst.path,
      port: 0,
      expect: 'any',
      idDir: idDir.path,
      deviceName: 'dart-recv',
      timeoutSecs: 30,
    ));
    final recvHex = _jobHex(recvJob);

    final events = <Map<String, dynamic>>[];
    final port = _awaitListening(engine, recvJob, events);
    expect(port, greaterThan(0));

    final sendJob = engine.sendStart(AkSendSpec(
      sourceRoots: [src.path],
      peer: '127.0.0.1:$port',
      fpHex: _fp1Hex,
      chunkKb: 64,
      deviceName: 'dart-send',
      timeoutSecs: 30,
    ));
    final sendHex = _jobHex(sendJob);
    expect(sendJob.any((byte) => byte != 0), isTrue,
        reason: 'job ids must never be all-zero');
    expect(sendHex, isNot(recvHex), reason: 'each job gets its own id');

    Map<String, dynamic>? sendJson;
    Map<String, dynamic>? recvJson;
    final deadline = DateTime.now().add(const Duration(seconds: 90));
    while (true) {
      final event = engine.pollProgress();
      if (event != null) {
        events.add(event);
      }
      if (sendJson == null) {
        final result = engine.jobResult(sendJob);
        if (result.isDone) {
          sendJson = result.map;
        } else {
          expect(result.isRunning, isTrue,
              reason: 'sender failed through the ABI: ${result.map}');
        }
      }
      if (recvJson == null) {
        final result = engine.jobResult(recvJob);
        if (result.isDone) {
          recvJson = result.map;
        } else {
          expect(result.isRunning, isTrue,
              reason: 'receiver failed through the ABI: ${result.map}');
        }
      }
      if (sendJson != null && recvJson != null) {
        // Both terminal: drain the remaining events before judging.
        while (true) {
          final tail = engine.pollProgress();
          if (tail == null) break;
          events.add(tail);
        }
        break;
      }
      if (DateTime.now().isAfter(deadline)) {
        fail('jobs did not finish within 90 s (events: $events)');
      }
      sleep(const Duration(milliseconds: 2));
    }

    // Reports agree with each other and with the payload.
    expect(sendJson['files'], totalEntries,
        reason: 'files + directories in the manifest');
    expect(sendJson['bytes'], totalBytes);
    expect(recvJson['files'], sendJson['files']);
    expect(recvJson['bytes'], sendJson['bytes']);
    expect(recvJson['wire_job'], sendJson['wire_job'],
        reason: 'both sides report the same protocol job id');
    expect(sendJson['peer'], _fp1Hex);
    expect(sendJson['resumed'], 0, reason: 'fresh first transfer');
    expect(sendJson['duration_ms'] as int, greaterThan(0));
    expect(sendJson['roots'], ['ffi-source'],
        reason: 'roots list the primary root name');

    // Event streams: full ordered sequence per side, done last.
    final sendEvents = events
        .where((e) => e['job'] == sendHex)
        .map((e) => e['event'] as String)
        .toList();
    final recvEvents = events
        .where((e) => e['job'] == recvHex)
        .map((e) => e['event'] as String)
        .toList();
    expect(sendEvents.first, 'connecting');
    expect(sendEvents.last, 'done');
    expect(recvEvents.first, 'listening');
    expect(recvEvents.last, 'done');
    int position(List<String> stream, String name) {
      final index = stream.indexOf(name);
      expect(index, greaterThanOrEqualTo(0), reason: 'stream needs $name');
      return index;
    }

    expect(
        position(sendEvents, 'connecting') < position(sendEvents, 'connected'),
        isTrue);
    expect(
        position(sendEvents, 'connected') < position(sendEvents, 'offered'),
        isTrue);
    expect(
        position(sendEvents, 'offered') < position(sendEvents, 'accepted'),
        isTrue);
    expect(position(sendEvents, 'accepted') < position(sendEvents, 'done'),
        isTrue);
    expect(
        position(recvEvents, 'listening') < position(recvEvents, 'connected'),
        isTrue);
    expect(recvEvents, contains('manifest_received'));
    expect(recvEvents, contains('file_committed'));
    expect(recvEvents.where((name) => name == 'done'), hasLength(1));
    expect(sendEvents.where((name) => name == 'done'), hasLength(1));

    // Events reference only the two known jobs.
    for (final event in events) {
      expect(event['job'], anyOf(sendHex, recvHex),
          reason: 'stray job id in event $event');
    }

    // The bytes that crossed the ABI are the bytes on disk.
    _assertTreesIdentical(src, dst);

    // Unknown job ids are rejected, not guessed.
    final bogus = Uint8List.fromList(List.filled(16, 9));
    expect(() => engine.jobResult(bogus), _throwsFfi(AkStatus.errNotFound));
    expect(() => engine.cancel(bogus), _throwsFfi(AkStatus.errNotFound));

    // jobResult with a tiny buffer: -3, then a lossless retry with the
    // same report (direct bindings-layer probe).
    final tiny = processAllocator.allocate<Uint8>(8);
    final jobPointer = processAllocator.allocate<Uint8>(16);
    try {
      for (var i = 0; i < 16; i++) {
        jobPointer[i] = sendJob[i];
      }
      expect(engine.bindings.jobResult(engine.nativeHandle, jobPointer, tiny, 8),
          AkStatus.errBuffer);
      expect(engine.lastError(), contains('buffer too small'));
      final retried = engine.jobResult(sendJob);
      expect(retried.isDone, isTrue);
      expect(retried.map, sendJson, reason: 'lossless retry, same report');
    } finally {
      processAllocator.free(jobPointer);
      processAllocator.free(tiny);
    }
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// Cancellation.
// ---------------------------------------------------------------------------

test('cancel mid-transfer: failed result, no fake done anywhere',
    timeout: const Timeout(Duration(seconds: 240)), () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('cancel');
    final src = Directory('${base.path}/bulk')..createSync(recursive: true);
    // One big deterministic file so the cancel lands mid-chunk-phase.
    final pattern = _Pattern(0xCA11000000000003);
    final block = pattern.bytes(1024 * 1024);
    final bulk = File('${src.path}/bulk.bin').openSync(mode: FileMode.write);
    try {
      for (var i = 0; i < 128; i++) {
        bulk.writeFromSync(block);
      }
    } finally {
      bulk.closeSync();
    }
    final idDir = _newTempDir('id2');
    File('${idDir.path}/device.key').writeAsBytesSync(_seedBytes(_seed2Hex));
    final dst = _newTempDir('dst2');

    final recvJob = engine.receiveStart(AkReceiveSpec(
      outDir: dst.path,
      port: 0,
      expect: 'any',
      idDir: idDir.path,
      timeoutSecs: 10, // receiver fails fast once the sender vanishes
    ));
    final events = <Map<String, dynamic>>[];
    final port = _awaitListening(engine, recvJob, events);

    final sendJob = engine.sendStart(AkSendSpec(
      sourceRoots: [src.path],
      peer: '127.0.0.1:$port',
      fpHex: _fp2Hex,
      chunkKb: 64,
      timeoutSecs: 60,
    ));
    final sendHex = _jobHex(sendJob);

    // Wait until chunks are actually flowing; fail loudly if the transfer
    // completes before a cancel could land.
    var sawChunk = false;
    final chunkDeadline = DateTime.now().add(const Duration(seconds: 60));
    while (!sawChunk) {
      final event = engine.pollProgress();
      if (event != null) {
        events.add(event);
        if (event['job'] == sendHex && event['event'] == 'chunk_sent') {
          sawChunk = true;
          break;
        }
      } else {
        final result = engine.jobResult(sendJob);
        expect(result.isRunning, isTrue,
            reason: 'transfer finished before cancel could land: ${result.map}');
        if (DateTime.now().isAfter(chunkDeadline)) {
          fail('no chunk events within 60 s');
        }
        sleep(const Duration(milliseconds: 1));
      }
    }
    expect(sawChunk, isTrue);
    expect(engine.jobResult(sendJob).isRunning, isTrue,
        reason: 'still running before cancel');

    engine.cancel(sendJob);
    final cancelled = engine.jobResult(sendJob);
    expect(cancelled.isFailed, isTrue);
    expect(cancelled.error, contains('cancelled by caller'));

    // The receiver observes the vanished peer and also fails (idle
    // timeout); it must never fake success.
    var recvFailed = false;
    final failDeadline = DateTime.now().add(const Duration(seconds: 30));
    while (!recvFailed) {
      final event = engine.pollProgress();
      if (event != null) {
        events.add(event);
      }
      final result = engine.jobResult(recvJob);
      if (result.isFailed) {
        expect(result.map, isNotNull,
            reason: 'error body present: ${result.map}');
        recvFailed = true;
        break;
      }
      expect(result.isRunning, isTrue,
          reason: 'receiver must not fake success: ${result.map}');
      if (DateTime.now().isAfter(failDeadline)) {
        fail('receiver did not fail within 30 s after the sender vanished');
      }
      sleep(const Duration(milliseconds: 2));
    }
    expect(recvFailed, isTrue);

    // Drain everything left: no done anywhere — no fake completion.
    while (true) {
      final tail = engine.pollProgress();
      if (tail == null) break;
      events.add(tail);
    }
    for (final event in events) {
      expect(event['event'], isNot('done'),
          reason: 'no done event may survive a cancelled run: $event');
    }
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// -3 buffer retry on the progress queue (not-consumed semantics).
// ---------------------------------------------------------------------------

test('pollProgress: -3 keeps the event queued, retry is lossless',
    timeout: const Timeout(Duration(seconds: 60)), () {
  final engine = AkEngineDart();
  final outDir = _newTempDir('p3');
  try {
    final job = engine
        .receiveStart(AkReceiveSpec(outDir: outDir.path, port: 0, timeoutSecs: 5));
    final tiny = processAllocator.allocate<Uint8>(4);
    try {
      // Cap 4 can never hold a real event line; wait until one is queued.
      var sawBuffer = false;
      final deadline = DateTime.now().add(const Duration(seconds: 15));
      while (!sawBuffer) {
        final code = engine.bindings.pollProgress(engine.nativeHandle, tiny, 4);
        if (code == AkStatus.errBuffer) {
          sawBuffer = true;
          break;
        }
        expect(code, AkStatus.pending,
            reason: 'tiny poll failed: ${engine.lastError()}');
        if (DateTime.now().isAfter(deadline)) {
          fail('no event became pending within 15 s');
        }
        sleep(const Duration(milliseconds: 10));
      }
      final message = engine.lastError();
      expect(message, contains('buffer too small'));
      expect(message, contains('need'));

      // Nothing was consumed: the tiny poll stays -3 and the message
      // repeats.
      expect(engine.bindings.pollProgress(engine.nativeHandle, tiny, 4),
          AkStatus.errBuffer);

      // The full-size poll through the engine now consumes the same event.
      final event = engine.pollProgress();
      expect(event, isNotNull);
      expect(event!['event'], 'listening');
      expect(event['job'], _jobHex(job));

      // The job is still abortable and then reports the cancellation.
      engine.cancel(job);
      final result = engine.jobResult(job);
      expect(result.isFailed, isTrue);
      expect(result.error, contains('cancelled by caller'));
    } finally {
      processAllocator.free(tiny);
    }
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// Identity info (ak_identity_info) + persisted jobs list (ak_jobs_list).
// ---------------------------------------------------------------------------

test('identityInfo: creates once, is stable, flags bad id_dir and books',
    () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('idinfo');
    final fresh = Directory('${base.path}/fresh-id'); // does NOT exist yet
    expect(fresh.existsSync(), isFalse);

    // First call: the directory and device.key are created.
    final first = engine.identityInfo(fresh.path);
    expect(first.created, isTrue, reason: 'device.key created by the call');
    expect(File('${fresh.path}/device.key').existsSync(), isTrue);
    expect(RegExp(r'^[0-9a-f]{64}$').hasMatch(first.fingerprint), isTrue,
        reason: 'fingerprint is 64 lowercase hex: ${first.fingerprint}');
    expect(RegExp(r'^[0-9a-f]{64}$').hasMatch(first.deviceId), isTrue,
        reason: 'device_id is the 64-hex public key');
    expect(RegExp(r'^\d{3}-\d{3}$').hasMatch(first.pairingCode), isTrue,
        reason: 'pairing code is ddd-ddd: ${first.pairingCode}');
    expect(first.peers, 0, reason: 'no peers.book yet');

    // Second call: same identity, created is now false (exactly once).
    final second = engine.identityInfo(fresh.path);
    expect(second.created, isFalse);
    expect(second.fingerprint, first.fingerprint);
    expect(second.deviceId, first.deviceId);
    expect(second.pairingCode, first.pairingCode);
    expect(second.peers, 0);

    // A corrupt peers.book still succeeds with peers 0, leaving a note in
    // the last-error buffer (read right after the call).
    File('${fresh.path}/peers.book')
        .writeAsStringSync('AKPEERS1\nzz\tbad\t1\n');
    final corrupt = engine.identityInfo(fresh.path);
    expect(corrupt.peers, 0);
    expect(corrupt.created, isFalse);
    expect(engine.lastError(), contains('peers.book'),
        reason: 'corrupt-book note surfaces through lastError');

    // Empty id_dir: -2 with the engine's message.
    try {
      engine.identityInfo('');
      fail('expected an AkFfiException');
    } on AkFfiException catch (e) {
      expect(e.code, AkStatus.errInvalid, reason: e.message);
      expect(e.message, contains('id_dir'));
    }

    // Tiny buffer at the raw layer: -3 names the needed size, the friendly
    // call retries internally and is unaffected.
    final idPtr = toNativeUtf8(fresh.path);
    final tiny = processAllocator.allocate<Uint8>(4);
    try {
      expect(
          engine.bindings.identityInfo(engine.nativeHandle, idPtr, tiny, 4),
          AkStatus.errBuffer);
      expect(engine.lastError(), contains('buffer too small'));
      expect(engine.lastError(), contains('need'));
    } finally {
      processAllocator.free(tiny);
      processAllocator.free(idPtr);
    }
    expect(engine.identityInfo(fresh.path).fingerprint, first.fingerprint,
        reason: 'lossless retry after the -3 probe');
  } finally {
    engine.dispose();
  }
});

test('identityInfo fingerprints are the wire identity (pinned loopback)',
    timeout: const Timeout(Duration(seconds: 240)), () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('pin');
    final src = Directory('${base.path}/pin-source')
      ..createSync(recursive: true);
    final payload = _Pattern(0x1212000000000004).bytes(120 * 1024);
    File('${src.path}/payload.bin').writeAsBytesSync(payload);

    // Both identities are created (and summarized) BEFORE any transfer.
    final recvId = Directory('${base.path}/recv-id');
    final sendId = Directory('${base.path}/send-id');
    final recvInfo = engine.identityInfo(recvId.path);
    final sendInfo = engine.identityInfo(sendId.path);
    expect(recvInfo.created, isTrue);
    expect(sendInfo.created, isTrue);

    final dst = _newTempDir('pin-dst');
    // Each side pins the fingerprint identityInfo reported for the other:
    // if either summary were wrong, admission or TLS would refuse.
    final recvJob = engine.receiveStart(AkReceiveSpec(
      outDir: dst.path,
      port: 0,
      expect: sendInfo.fingerprint,
      idDir: recvId.path,
      timeoutSecs: 30,
    ));
    final events = <Map<String, dynamic>>[];
    final port = _awaitListening(engine, recvJob, events);
    final sendJob = engine.sendStart(AkSendSpec(
      sourceRoots: [src.path],
      peer: '127.0.0.1:$port',
      fpHex: recvInfo.fingerprint,
      idDir: sendId.path,
      timeoutSecs: 30,
    ));

    final reports = _pumpUntilDone(engine, [sendJob, recvJob]);
    final sendJson = reports[_jobHex(sendJob)]!;
    final recvJson = reports[_jobHex(recvJob)]!;
    expect(sendJson['peer'], recvInfo.fingerprint,
        reason: 'sender authenticated the receiver at the reported fp');
    expect(recvJson['peer'], sendInfo.fingerprint,
        reason: 'receiver admitted the sender pinned at the reported fp');
    expect(sendJson['files'], 1);
    expect(sendJson['bytes'], payload.length);
    _assertTreesIdentical(src, dst);
  } finally {
    engine.dispose();
  }
});

test('jobsList: fresh, key-only, and missing directories are []', () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('jl0');
    expect(engine.jobsList(base.path), isEmpty,
        reason: 'existing dir without checkpoints/');
    expect(engine.jobsList('${base.path}/never-created'), isEmpty,
        reason: 'a missing id_dir is also the empty list');

    final withKey = Directory('${base.path}/has-key');
    engine.identityInfo(withKey.path); // creates the dir + device.key only
    expect(engine.jobsList(withKey.path), isEmpty,
        reason: 'an identity without transfers lists nothing');

    // Empty id_dir: -2 with the engine's message.
    try {
      engine.jobsList('');
      fail('expected an AkFfiException');
    } on AkFfiException catch (e) {
      expect(e.code, AkStatus.errInvalid, reason: e.message);
      expect(e.message, contains('id_dir'));
    }

    // Raw layer: a null buffer is -1.
    final idPtr = toNativeUtf8(base.path);
    try {
      expect(
          engine.bindings.jobsList(engine.nativeHandle, idPtr, nullptr, 64),
          AkStatus.errNull);
    } finally {
      processAllocator.free(idPtr);
    }
  } finally {
    engine.dispose();
  }
});

test('jobsList: id_dir loopback persists one completed record per side',
    timeout: const Timeout(Duration(seconds: 240)), () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('jl');
    final src = Directory('${base.path}/jl-source')
      ..createSync(recursive: true);
    final pattern = _Pattern(0x1213000000000005);
    File('${src.path}/one.bin').writeAsBytesSync(pattern.bytes(50 * 1024));
    Directory('${src.path}/sub').createSync(recursive: true);
    File('${src.path}/sub/two.bin')
        .writeAsBytesSync(pattern.bytes(3 * 1024 + 9));
    const totalBytes = 50 * 1024 + 3 * 1024 + 9;
    const totalEntries = 3; // two files + the directory "sub"

    final recvId = Directory('${base.path}/recv-id');
    final sendId = Directory('${base.path}/send-id');
    final recvInfo = engine.identityInfo(recvId.path);
    final sendInfo = engine.identityInfo(sendId.path);
    expect(engine.jobsList(recvId.path), isEmpty,
        reason: 'nothing persisted before the transfer');

    final dst = _newTempDir('jl-dst');
    final recvJob = engine.receiveStart(AkReceiveSpec(
      outDir: dst.path,
      port: 0,
      expect: 'any',
      idDir: recvId.path,
      timeoutSecs: 30,
    ));
    final events = <Map<String, dynamic>>[];
    final port = _awaitListening(engine, recvJob, events);
    final sendJob = engine.sendStart(AkSendSpec(
      sourceRoots: [src.path],
      peer: '127.0.0.1:$port',
      fpHex: recvInfo.fingerprint,
      idDir: sendId.path,
      timeoutSecs: 30,
    ));

    final reports = _pumpUntilDone(engine, [sendJob, recvJob]);
    final wireJob = reports[_jobHex(sendJob)]!['wire_job'] as String;
    expect(reports[_jobHex(recvJob)]!['wire_job'], wireJob);

    // Receiver side: one completed receive record naming the sender.
    final recvRecords = engine.jobsList(recvId.path);
    expect(recvRecords, hasLength(1));
    final recvRecord = recvRecords.single;
    expect(RegExp(r'^[0-9a-f]{32}$').hasMatch(recvRecord.id), isTrue,
        reason: 'id is the 32-hex wire job id: ${recvRecord.id}');
    expect(recvRecord.id, wireJob);
    expect(recvRecord.direction, 'receive');
    expect(recvRecord.state, 'completed');
    expect(recvRecord.files, totalEntries);
    expect(recvRecord.bytes, totalBytes);
    expect(recvRecord.updatedUnixMs, greaterThan(0));
    expect(recvRecord.peer, sendInfo.fingerprint);
    expect(recvRecord.root, dst.path, reason: 'receive root is the out_dir');

    // Sender side: one completed send record pinned to the receiver.
    final sendRecords = engine.jobsList(sendId.path);
    expect(sendRecords, hasLength(1));
    final sendRecord = sendRecords.single;
    expect(sendRecord.id, wireJob);
    expect(sendRecord.direction, 'send');
    expect(sendRecord.state, 'completed');
    expect(sendRecord.files, totalEntries);
    expect(sendRecord.bytes, totalBytes);
    expect(sendRecord.peer, recvInfo.fingerprint);
    expect(sendRecord.root, src.path, reason: 'send root is the source');

    // Whole-array -3 at the raw layer: nothing consumed; the friendly
    // retry returns the identical record (also proving == on the type).
    final idPtr = toNativeUtf8(sendId.path);
    final tiny = processAllocator.allocate<Uint8>(8);
    try {
      expect(engine.bindings.jobsList(engine.nativeHandle, idPtr, tiny, 8),
          AkStatus.errBuffer);
      expect(engine.lastError(), contains('buffer too small'));
    } finally {
      processAllocator.free(tiny);
      processAllocator.free(idPtr);
    }
    expect(engine.jobsList(sendId.path), [sendRecord],
        reason: 'lossless retry after the -3 probe');
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// expect:"book" admission (peer book under id_dir).
// ---------------------------------------------------------------------------

test('expect book admits a paired sender end to end',
    timeout: const Timeout(Duration(seconds: 240)), () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('book');
    final src = Directory('${base.path}/book-src')
      ..createSync(recursive: true);
    final payload = _Pattern(0xB00C000000000006).bytes(200 * 1024);
    File('${src.path}/payload.bin').writeAsBytesSync(payload);

    // Receiver identity from the RFC 8032 seed 1 fixture (fp known); the
    // book pre-pairs the sender (seed 2 => _fp2Hex).
    final recvId = _newTempDir('book-recv');
    File('${recvId.path}/device.key').writeAsBytesSync(_seedBytes(_seed1Hex));
    File('${recvId.path}/peers.book')
        .writeAsStringSync('AKPEERS1\n$_fp2Hex\tdart-book\t1700000000000\n');
    final sendId = _newTempDir('book-send');
    File('${sendId.path}/device.key').writeAsBytesSync(_seedBytes(_seed2Hex));
    final dst = _newTempDir('book-dst');

    // identityInfo sees the paired peer; the seeded key was not created.
    final info = engine.identityInfo(recvId.path);
    expect(info.peers, 1, reason: 'one peers.book entry');
    expect(info.created, isFalse, reason: 'device.key pre-existed');

    final recvJob = engine.receiveStart(AkReceiveSpec(
      outDir: dst.path,
      port: 0,
      expect: 'book',
      idDir: recvId.path,
      deviceName: 'book-recv',
      timeoutSecs: 30,
    ));
    final events = <Map<String, dynamic>>[];
    final port = _awaitListening(engine, recvJob, events);
    final sendJob = engine.sendStart(AkSendSpec(
      sourceRoots: [src.path],
      peer: '127.0.0.1:$port',
      fpHex: _fp1Hex,
      idDir: sendId.path,
      deviceName: 'book-send',
      timeoutSecs: 30,
    ));

    final reports = _pumpUntilDone(engine, [sendJob, recvJob]);
    expect(reports[_jobHex(recvJob)]!['peer'], _fp2Hex,
        reason: 'the book-admitted sender is reported as the peer');
    expect(reports[_jobHex(sendJob)]!['peer'], _fp1Hex);
    _assertTreesIdentical(src, dst);

    // The admitted transfer persisted a completed record naming the sender.
    final records = engine.jobsList(recvId.path);
    expect(records, hasLength(1));
    expect(records.single.direction, 'receive');
    expect(records.single.state, 'completed');
    expect(records.single.peer, _fp2Hex);
    expect(records.single.files, 1);
    expect(records.single.bytes, payload.length);
  } finally {
    engine.dispose();
  }
});

test('expect book refuses empty, missing, and corrupt books at start', () {
  final engine = AkEngineDart();
  try {
    final outDir = _newTempDir('bookref-out');

    void expectRefusal(void Function() start, List<String> needles) {
      try {
        start();
        fail('expected an AkFfiException');
      } on AkFfiException catch (e) {
        expect(e.code, AkStatus.errInvalid, reason: e.message);
        for (final needle in needles) {
          expect(e.message, contains(needle),
              reason: 'message was: ${e.message}');
        }
      }
    }

    AkReceiveSpec bookSpec(String idDir) => AkReceiveSpec(
          outDir: outDir.path,
          port: 0,
          expect: 'book',
          idDir: idDir.isEmpty ? null : idDir,
        );

    // Missing book (id_dir holds only device.key): "pair first".
    final missing = _newTempDir('bookref-missing');
    File('${missing.path}/device.key')
        .writeAsBytesSync(_seedBytes(_seed1Hex));
    expectRefusal(() => engine.receiveStart(bookSpec(missing.path)),
        ['no paired peers', 'pair first']);

    // Header-only (empty) book: the same refusal.
    final empty = _newTempDir('bookref-empty');
    File('${empty.path}/device.key').writeAsBytesSync(_seedBytes(_seed1Hex));
    File('${empty.path}/peers.book').writeAsStringSync('AKPEERS1\n');
    expectRefusal(() => engine.receiveStart(bookSpec(empty.path)),
        ['no paired peers', 'pair first']);

    // Corrupt book: a load failure naming the peer book.
    final corrupt = _newTempDir('bookref-corrupt');
    File('${corrupt.path}/device.key')
        .writeAsBytesSync(_seedBytes(_seed1Hex));
    File('${corrupt.path}/peers.book')
        .writeAsStringSync('AKPEERS1\nzz\tname\t1\n');
    expectRefusal(() => engine.receiveStart(bookSpec(corrupt.path)),
        ['peer book']);

    // "book" without an id_dir cannot resolve a book at all.
    expectRefusal(() => engine.receiveStart(bookSpec('')), ['id_dir']);
  } finally {
    engine.dispose();
  }
});

test('expect spec: "book" is a valid value, near-misses are not', () {
  AkReceiveSpec recv(String? expect) => AkReceiveSpec(
        outDir: 'C:/inbox',
        port: 0,
        expect: expect,
        timeoutSecs: 5,
      );

  // "book" round-trips verbatim in every case shape the engine parses.
  expect(jsonDecode(recv('book').toJsonString())['expect'], 'book');
  expect(jsonDecode(recv('BOOK').toJsonString())['expect'], 'BOOK');
  expect(jsonDecode(recv(' Book ').toJsonString())['expect'], ' Book ');

  // Near-misses stay rejected, with the updated message naming "book".
  _expectSpecError(() => recv('books').toJsonString(), '"book"');
  _expectSpecError(() => recv('bookk').toJsonString(), '"book"');
  _expectSpecError(() => recv('nope').toJsonString(), '"book"');
  _expectSpecError(() => recv('12345').toJsonString(), '6-digit');
});
}
