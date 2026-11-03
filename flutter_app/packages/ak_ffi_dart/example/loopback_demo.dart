/// AK-sending loopback demo — a UI prototype without Flutter: the same
/// JobController state layer a future Flutter page will reuse, rendered
/// onto stdout instead of widgets.
///
/// Random tree -> receiver (port 0) + sender on one AkEngineDart -> poll
/// loop feeds two JobControllers, redrawing a `\r` line once per second
/// (progressBarText/humanBytes) -> TransferReport summaries and a
/// byte-exact comparison. Exit 0 = pass, 1 = fail. Run from the package
/// root (DLL path is CWD-relative; AK_FFI_DLL overrides):
///     dart run example/loopback_demo.dart
library;

import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:ak_ffi_dart/ak_ffi_dart.dart';

/// Fixed receiver identity: RFC 8032 seed 1 + fingerprint (see the test
/// suite), prewritten to `<id_dir>/device.key` so the sender can pin it.
const _seedHex =
    '9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60';
const _fpHex =
    '6c31041268f471609c79f5f2dbcc38e4a4ab2f4d416109a4e09fcf50fd0f0062';

Uint8List _seed() => Uint8List.fromList(List.generate(
    32, (i) => int.parse(_seedHex.substring(i * 2, i * 2 + 2), radix: 16)));

/// Random tree: CRLF text, three multi-MiB binaries with random tails, an
/// empty dir, and a name with spaces/semicolon.
void _makeTree(Directory base, Random rng) {
  void write(String rel, List<int> data) => File('${base.path}/$rel')
    ..parent.createSync(recursive: true)
    ..writeAsBytesSync(data);

  write('docs/manual; v2 (draft).txt', 'AK loopback\r\ndemo\r\n'.codeUnits);
  for (final miB in const [12, 5, 2]) { // + a random tail per binary
    final data = Uint8List((miB << 20) + rng.nextInt(64 * 1024));
    for (var j = 0; j < data.length; j++) {
      data[j] = rng.nextInt(256);
    }
    write('photos/raw/img-${miB}MiB.bin', data);
  }
  Directory('${base.path}/photos/empty-album').createSync(recursive: true);
}

/// Flattened `relative path -> bytes` view of a tree (files only).
Map<String, Uint8List> _scan(Directory root) => {
      for (final entity in root.listSync())
        if (entity is File)
          entity.path.split(RegExp(r'[\\/]')).last: entity.readAsBytesSync()
        else if (entity is Directory)
          ..._scan(entity).map((path, bytes) => MapEntry(
              '${entity.path.split(RegExp(r'[\\/]')).last}/$path', bytes)),
    };

/// Byte-exact comparison of both trees; every divergence is printed.
bool _treesIdentical(Directory src, Directory dst) {
  final from = _scan(src), to = _scan(dst);
  final issues = [
    ...from.keys.toSet().difference(to.keys.toSet()).map((p) => 'missing: $p'),
    ...to.keys.toSet().difference(from.keys.toSet()).map((p) => 'extra: $p'),
  ];
  for (final entry in from.entries) {
    final other = to[entry.key];
    if (other == null) continue;
    var same = other.length == entry.value.length;
    for (var i = 0; same && i < other.length; i++) {
      same = other[i] == entry.value[i];
    }
    if (!same) issues.add('content differs: ${entry.key}');
  }
  issues.forEach(stdout.writeln);
  stdout.writeln(issues.isEmpty
      ? 'compare: PASS — ${from.length} files byte-identical'
      : 'compare: FAIL — see above');
  return issues.isEmpty;
}

/// The live one-line status — exactly what a progress widget would show.
String _statusLine(JobController recv, JobController send) {
  final bar = progressBarText(
      transferred: send.ackedBytes, total: send.expectedBytes ?? 0, width: 20);
  return 'recv[${recv.phase.name}] ${recv.committedFileCount}/'
      '${recv.expectedFiles ?? '?'} files of '
      '${humanBytes(recv.expectedBytes ?? 0)} | send[${send.phase.name}] $bar';
}

/// TransferReport summary of one side (or its failure reason).
void _describe(String side, JobController c) {
  final r = c.report;
  stdout.writeln(r == null
      ? '$side: FAILED — ${c.error}'
      : '$side: ${r.files} entries, ${r.bytesText} in ${r.durationText}, '
          '${r.speedText ?? 'instant'}, roots ${r.roots}');
}

/// Snapshots one job; folds a terminal body into its controller.
void _pollResult(AkEngineDart engine, Uint8List job, JobController c) {
  if (c.hasResult) return;
  final result = engine.jobResult(job);
  if (!result.isRunning) c.setResultJson(result.map!);
}

Future<void> main() async {
  try {
    final ok = await _run(AkEngineDart());
    await stdout.flush();
    exit(ok ? 0 : 1);
  } catch (error, stack) {
    stderr.writeln('demo failed: $error\n$stack');
    await stderr.flush();
    exit(1);
  }
}

/// The whole transfer, orchestrated the way a Flutter page would.
Future<bool> _run(AkEngineDart engine) async {
  stdout.writeln('${engine.version()} — loopback demo');
  final tempDirs = <Directory>[];
  try {
    final base = Directory.systemTemp.createTempSync('ak_demo_tree_');
    final idDir = Directory.systemTemp.createTempSync('ak_demo_id_');
    final dst = Directory.systemTemp.createTempSync('ak_demo_dst_');
    tempDirs.addAll([base, idDir, dst]);
    final src = Directory('${base.path}/demo-tree')..createSync(recursive: true);
    _makeTree(src, Random());
    File('${idDir.path}/device.key').writeAsBytesSync(_seed());
    final recv = JobController(), send = JobController();
    final recvJob = engine.receiveStart(AkReceiveSpec(
        outDir: dst.path, port: 0, expect: 'any', idDir: idDir.path,
        deviceName: 'demo-recv', timeoutSecs: 30));

    // Port 0 binds an ephemeral port; only the `listening` event reveals it.
    var port = 0;
    final listenBy = DateTime.now().add(const Duration(seconds: 30));
    while (port == 0) {
      final line = engine.pollProgress();
      if (line != null) {
        final event = ProgressEvent.fromJson(line);
        recv.feed(event);
        if (event is Listening) port = int.parse(event.addr.split(':').last);
      } else if (DateTime.now().isAfter(listenBy)) {
        throw StateError('no listening event within 30 s');
      }
      await Future<void>.delayed(const Duration(milliseconds: 2));
    }

    final sendJob = engine.sendStart(AkSendSpec(
        sourceRoots: [src.path], peer: '127.0.0.1:$port', fpHex: _fpHex,
        chunkKb: 64, deviceName: 'demo-send', timeoutSecs: 60));
    final sendHex = sendJob.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
    stdout.writeln('receiver on 127.0.0.1:$port — sending the random tree');

    // The poll loop a Flutter page would run from a timer: fold events into
    // their controllers, redraw once per second, stop at both results.
    var lastDraw = DateTime.fromMillisecondsSinceEpoch(0);
    while (!send.hasResult || !recv.hasResult) {
      final line = engine.pollProgress();
      if (line != null) {
        final event = ProgressEvent.fromJson(line);
        (event.job == sendHex ? send : recv).feed(event);
      } else {
        _pollResult(engine, sendJob, send);
        _pollResult(engine, recvJob, recv);
      }
      final now = DateTime.now();
      if (now.difference(lastDraw).inMilliseconds >= 1000) {
        lastDraw = now;
        stdout.write('\r${_statusLine(recv, send).padRight(112)}');
      }
      await Future<void>.delayed(const Duration(milliseconds: 2));
    }
    stdout.writeln();
    _describe('send', send);
    _describe('recv', recv);
    return _treesIdentical(src, dst) &&
        send.phase == JobPhase.done && recv.phase == JobPhase.done;
  } finally {
    engine.dispose();
    for (final dir in tempDirs) {
      try {
        dir.deleteSync(recursive: true);
      } on Exception {
        // best-effort cleanup under the system temp root
      }
    }
  }
}
