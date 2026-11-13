/// Client UI U1 (2026-11-13) service-layer tests: the engine service's
/// progress fold (task-plane `ak_poll_progress` lines → one
/// [SimpleTransferProgress] snapshot per transfer per drain), the
/// adapter's mapping onto the UI contract, the send summary's direct-path
/// label, and the pure display helpers. Driven by [FakeAkTaskCalls]: the
/// lines are scripted the way the ffi-v2 §8.4 U1 revision renders them
/// (`files`/`bytes`/`file`/`chunk`/`path` payload fields).
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter/material.dart' show Brightness, ColorScheme;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/direct_path_label.dart';
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show IncomingRequest, TaskRole, TransferPhase, TransferProgress;
import 'package:ak_sending_app/ui/service/transfer_format.dart';
import 'package:ak_sending_app/ui/service/transfer_history.dart';
import 'package:ak_sending_app/ui/theme.dart';

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

const int mib = 1024 * 1024;

/// Zero-delay stand-in: op wait loops drain synchronously under the fake.
Future<void> immediate(Duration d) async {}

SimpleTransferService engineService(FakeAkTaskCalls fake, String idDir,
    {DateTime Function()? clock}) {
  final service = SimpleTransferService(
    calls: fake,
    delay: immediate,
    eventPollInterval: const Duration(days: 1), // tests pump manually
    clock: clock,
  );
  service.start(registry: '127.0.0.1:7200', registryIdHex: regId, idDir: idDir);
  return service;
}

/// A receiver (the code creator) and a sender (the joiner) paired on one
/// fake engine, both rows pumped in — the sender's row carries the
/// `sender` role the awaiting-consent phase depends on.
Future<
        ({
          SimpleTransferService receiver,
          SimpleTransferService sender,
          String conn,
        })>
    pairedEngines(FakeAkTaskCalls fake, {DateTime Function()? clock}) async {
  final receiver = engineService(fake, 'C:/ak/u1-receiver');
  final created = await receiver.createCode(SimpleTransferRole.receiver);
  final sender = engineService(fake, 'C:/ak/u1-sender', clock: clock);
  final joined = await sender.join(created.code);
  sender.pumpEvents();
  receiver.pumpEvents();
  return (receiver: receiver, sender: sender, conn: joined.connId);
}

/// Queues one task-plane progress row (the engine-wide queue).
void progressLine(FakeAkTaskCalls fake, String job, String event,
    [Map<String, Object> fields = const <String, Object>{}]) {
  fake.progressLines.add(<String, dynamic>{
    'job': job,
    'event': event,
    ...fields,
  });
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('U1 engine fold', () {
    test('sender: connecting → awaiting consent → bytes → verifying → '
        'completed, one snapshot per drain', () async {
      final fake = FakeAkTaskCalls();
      var now = DateTime.utc(2026, 11, 13, 12);
      final pair = await pairedEngines(fake, clock: () => now);
      final sender = pair.sender;
      final snapshots = <SimpleTransferProgress>[];
      final sub = sender.transferProgress.listen(snapshots.add);

      // The first row announces the transfer before any byte moves.
      progressLine(fake, pair.conn, 'connecting');
      expect(sender.drainProgress().clean, isTrue);
      await pumpEventQueue();
      expect(snapshots, hasLength(1));
      expect(snapshots.single.phase, 'connecting');
      expect(snapshots.single.totalBytes, isNull);

      // Several rows in one drain → ONE snapshot (the latest fold).
      progressLine(fake, pair.conn, 'connected', {'peer': '192.168.1.9:5000'});
      progressLine(fake, pair.conn, 'offered', {'files': 2, 'bytes': 3 * mib});
      expect(sender.drainProgress().clean, isTrue);
      await pumpEventQueue();
      expect(snapshots, hasLength(2), reason: '一次 drain 每个传输至多一个快照');
      expect(snapshots.last.phase, 'awaiting_consent',
          reason: '发送方发出 offer 后在等对方点「接受」');
      expect(snapshots.last.totalFiles, 2);
      expect(snapshots.last.totalBytes, 3 * mib);

      progressLine(fake, pair.conn, 'accepted', {'files': 2, 'bytes': 3 * mib});
      progressLine(fake, pair.conn, 'file_started',
          {'file': 0, 'path': 'photos/a.bin', 'bytes': 2 * mib});
      progressLine(fake, pair.conn, 'chunk_sent', {'file': 0, 'chunk': 0});
      sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.phase, 'transferring');
      expect(snapshots.last.transferredBytes, mib,
          reason: '块位置把进度细化到 1 MiB 粒度');
      expect(snapshots.last.currentFile, 'a.bin', reason: '只给末段文件名');
      expect(snapshots.last.currentFileIndex, 0);
      expect(snapshots.last.completedFiles, 0);
      expect(snapshots.last.bytesPerSecond, isNull,
          reason: '不足 1 秒不给速率');

      // Two seconds later file 0 is complete and file 1 starts.
      now = now.add(const Duration(seconds: 2));
      progressLine(fake, pair.conn, 'chunk_sent', {'file': 0, 'chunk': 1});
      progressLine(fake, pair.conn, 'file_started',
          {'file': 1, 'path': 'photos/b.bin', 'bytes': mib});
      sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.transferredBytes, 2 * mib);
      expect(snapshots.last.completedFiles, 1);
      expect(snapshots.last.currentFile, 'b.bin');
      expect(snapshots.last.currentFileIndex, 1);
      expect(snapshots.last.bytesPerSecond, closeTo(mib.toDouble(), 1),
          reason: '2 MiB / 2 s');

      // The last byte moves: the sender now waits for the receiver's
      // whole-transfer verification.
      progressLine(fake, pair.conn, 'bytes_acked', {'file': 1, 'bytes': mib});
      sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.phase, 'verifying');
      expect(snapshots.last.transferredBytes, 3 * mib);
      expect(snapshots.last.completedFiles, 2);

      progressLine(fake, pair.conn, 'final_ack', {'verified': true});
      progressLine(fake, pair.conn, 'done');
      sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.phase, 'completed');
      expect(snapshots.last.transferredBytes, 3 * mib);

      // v1: nothing reopens a completed transfer on the same pairing.
      final settled = snapshots.length;
      progressLine(fake, pair.conn, 'chunk_sent', {'file': 0, 'chunk': 9});
      expect(sender.drainProgress().clean, isTrue);
      await pumpEventQueue();
      expect(snapshots, hasLength(settled));

      await sub.cancel();
      pair.receiver.dispose();
      sender.dispose();
    });

    test('rows WITHOUT payload fields (an engine predating the U1 '
        'revision) drain clean and leave the totals unknown', () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.sender.transferProgress.listen(snapshots.add);

      for (final event in ['offered', 'accepted', 'file_started',
          'chunk_sent', 'bytes_acked', 'chunk_sent']) {
        progressLine(fake, pair.conn, event);
      }
      final drain = pair.sender.drainProgress();
      await pumpEventQueue();
      expect(drain.clean, isTrue, reason: '旧引擎的行不算协议故障（C12）');
      expect(snapshots, hasLength(1));
      expect(snapshots.single.phase, 'transferring');
      expect(snapshots.single.totalBytes, isNull);
      expect(snapshots.single.totalFiles, isNull);
      expect(snapshots.single.transferredBytes, 0);
      expect(pair.sender.phase, isNot(SimpleTransferPhase.engineFailed));

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });

    test('oddly typed payload fields never throw and never count as '
        'malformed', () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.sender.transferProgress.listen(snapshots.add);

      progressLine(fake, pair.conn, 'accepted', {'files': '2', 'bytes': -5});
      progressLine(fake, pair.conn, 'file_started',
          {'file': 'zero', 'path': 42, 'bytes': 1.5});
      progressLine(fake, pair.conn, 'chunk_sent', {'file': -1, 'chunk': 3});
      progressLine(fake, pair.conn, 'bytes_acked', {'file': 0});
      progressLine(fake, pair.conn, 'file_committed', {'file': true});
      progressLine(fake, pair.conn, 'future_event_name', {'x': 1});
      final drain = pair.sender.drainProgress();
      await pumpEventQueue();
      expect(drain.clean, isTrue);
      expect(snapshots, hasLength(1));
      expect(snapshots.single.phase, 'transferring');
      expect(snapshots.single.totalBytes, isNull);
      expect(snapshots.single.currentFile, isNull);
      expect(snapshots.single.transferredBytes, 0);

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });

    test('the receiving side never shows an awaiting-consent phase; its '
        'totals still come from the offer', () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.receiver.transferProgress.listen(snapshots.add);

      progressLine(fake, pair.conn, 'connected', {'peer': '10.0.0.2:4000'});
      progressLine(fake, pair.conn, 'offered', {'files': 3, 'bytes': 2 * mib});
      pair.receiver.drainProgress();
      await pumpEventQueue();
      expect(snapshots.single.phase, 'connecting');
      expect(snapshots.single.totalFiles, 3);
      expect(snapshots.single.totalBytes, 2 * mib);

      progressLine(fake, pair.conn, 'accepted', {'files': 3, 'bytes': 2 * mib});
      progressLine(fake, pair.conn, 'file_started',
          {'file': 0, 'path': 'a.txt', 'bytes': 2 * mib});
      progressLine(fake, pair.conn, 'chunk_received', {'file': 0, 'chunk': 1});
      progressLine(fake, pair.conn, 'verifying');
      progressLine(fake, pair.conn, 'file_verified', {'file': 0});
      progressLine(fake, pair.conn, 'file_committed',
          {'file': 0, 'path': 'a.txt'});
      pair.receiver.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.phase, 'verifying');
      expect(snapshots.last.transferredBytes, 2 * mib);
      expect(snapshots.last.completedFiles, 1);

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });

    test('rows keyed by an id that is not a live connection of this '
        'service are ignored', () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.sender.transferProgress.listen(snapshots.add);

      progressLine(fake, 'ab' * 16, 'accepted', {'files': 1, 'bytes': 9});
      expect(pair.sender.drainProgress().clean, isTrue);
      await pumpEventQueue();
      expect(snapshots, isEmpty);

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });

    test('the task `done` event completes the transfer even before the '
        'progress queue says so; later rows of the row are ignored',
        () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.sender.transferProgress.listen(snapshots.add);

      progressLine(fake, pair.conn, 'accepted', {'files': 1, 'bytes': 4 * mib});
      progressLine(fake, pair.conn, 'file_started',
          {'file': 0, 'path': 'big.iso', 'bytes': 4 * mib});
      progressLine(fake, pair.conn, 'chunk_sent', {'file': 0, 'chunk': 0});
      pair.sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.last.transferredBytes, mib);

      // The event queue's `done` lands first (sender runtime = the second
      // one started on the fake).
      fake.emitDone(fake.runtimes.keys.last,
          connId: pair.conn, files: 1, bytes: 4 * mib);
      pair.sender.pumpEvents();
      await pumpEventQueue();
      expect(snapshots.last.phase, 'completed');
      expect(snapshots.last.transferredBytes, 4 * mib,
          reason: '完成快照报满总量');
      expect(pair.sender.currentConnections.single.state, 'done');

      final settled = snapshots.length;
      progressLine(fake, pair.conn, 'chunk_sent', {'file': 0, 'chunk': 3});
      progressLine(fake, pair.conn, 'done');
      pair.sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots, hasLength(settled), reason: '行已终态，迟到的进度行不复活');

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });

    test('an aborted transfer stops without a completed snapshot', () async {
      final fake = FakeAkTaskCalls();
      final pair = await pairedEngines(fake);
      final snapshots = <SimpleTransferProgress>[];
      final sub = pair.sender.transferProgress.listen(snapshots.add);

      progressLine(fake, pair.conn, 'accepted', {'files': 1, 'bytes': mib});
      pair.sender.drainProgress();
      await pumpEventQueue();
      expect(snapshots.single.phase, 'transferring');

      await pair.sender.disconnect(pair.conn);
      pair.sender.pumpEvents();
      await pumpEventQueue();
      expect(snapshots.map((s) => s.phase), isNot(contains('completed')));

      await sub.cancel();
      pair.receiver.dispose();
      pair.sender.dispose();
    });
  });

  group('U1 adapter', () {
    FfiSimpleTransferService adapter(FakeAkTaskCalls fake, String idDir) {
      final service = FfiSimpleTransferService(
        calls: fake,
        registry: '127.0.0.1:7200',
        registryIdHex: regId,
        idDir: idDir,
        delay: immediate,
      );
      service.start();
      return service;
    }

    test('transferProgress speaks the UI contract (phases, totals, file)',
        () async {
      final fake = FakeAkTaskCalls();
      final receiver = adapter(fake, 'C:/ak/u1-peer');
      final sender = adapter(fake, 'C:/ak/u1-bridge');
      final code = (await receiver.createCode(TaskRole.receive)).code;
      final joined = await sender.joinCode(code);
      final conn = joined.connectionId!;
      sender.engine.pumpEvents();

      final seen = <TransferProgress>[];
      final sub = sender.transferProgress.listen(seen.add);
      progressLine(fake, conn, 'offered', {'files': 1, 'bytes': 2 * mib});
      sender.engine.drainProgress();
      await pumpEventQueue();
      expect(seen.single.connId, conn);
      expect(seen.single.phase, TransferPhase.awaitingConsent);
      expect(seen.single.totalBytes, 2 * mib);
      expect(seen.single.fraction, 0.0);

      progressLine(fake, conn, 'accepted', {'files': 1, 'bytes': 2 * mib});
      progressLine(fake, conn, 'file_started',
          {'file': 0, 'path': 'docs/报告.pdf', 'bytes': 2 * mib});
      progressLine(fake, conn, 'chunk_sent', {'file': 0, 'chunk': 0});
      sender.engine.drainProgress();
      await pumpEventQueue();
      expect(seen.last.phase, TransferPhase.transferring);
      expect(seen.last.transferredBytes, mib);
      expect(seen.last.fraction, 0.5);
      expect(seen.last.currentFileName, '报告.pdf');
      expect(seen.last.totalFiles, 1);

      progressLine(fake, conn, 'chunk_sent', {'file': 0, 'chunk': 1});
      sender.engine.drainProgress();
      await pumpEventQueue();
      expect(seen.last.phase, TransferPhase.verifying);

      progressLine(fake, conn, 'done');
      sender.engine.drainProgress();
      await pumpEventQueue();
      expect(seen.last.phase, TransferPhase.completed);
      expect(seen.last.fraction, 1.0);

      await sub.cancel();
      receiver.dispose();
      sender.dispose();
    });

    test('the send summary names the direct path the engine used', () async {
      final fake = FakeAkTaskCalls();
      final receiver = adapter(fake, 'C:/ak/u1-peer2');
      final sender = adapter(fake, 'C:/ak/u1-bridge2');
      final code = (await receiver.createCode(TaskRole.receive)).code;
      final joined = await sender.joinCode(code);
      final summary =
          await sender.sendFiles(joined.connectionId!, const ['C:/a.jpg']);
      // The fake's task_send reports peer 127.0.0.1:45678.
      expect(summary, '已发送 1 个文件（1.0 KB） · 本机直连');
      receiver.dispose();
      sender.dispose();
    });

    test('an incoming request carries its connection id (the progress '
        'key)', () async {
      final fake = FakeAkTaskCalls();
      final service = adapter(fake, 'C:/ak/u1-recv');
      final code = (await service.createCode(TaskRole.receive)).code;
      final joined = await service.joinCode(code);
      service.engine.pumpEvents();
      final requests = <IncomingRequest>[];
      final sub = service.incomingRequests.listen(requests.add);
      fake.emitIncomingRequest(fake.runtimes.keys.first,
          connId: joined.connectionId, files: 1, bytes: 1);
      service.engine.pumpEvents();
      await pumpEventQueue();
      expect(requests, hasLength(1));
      expect(requests.single.connId, joined.connectionId);
      await sub.cancel();
      service.dispose();
    });
  });

  group('U1 display helpers', () {
    test('TransferProgress fraction and time left', () {
      const unknown = TransferProgress(
          connId: 'c', phase: TransferPhase.transferring, transferredBytes: 5);
      expect(unknown.fraction, isNull);
      expect(unknown.remaining, isNull);

      const half = TransferProgress(
        connId: 'c',
        phase: TransferPhase.transferring,
        totalBytes: 4 * mib,
        transferredBytes: 2 * mib,
        bytesPerSecond: 1.0 * mib,
      );
      expect(half.fraction, 0.5);
      expect(half.remaining, const Duration(seconds: 2));

      const empty = TransferProgress(
          connId: 'c', phase: TransferPhase.completed, totalBytes: 0);
      expect(empty.fraction, 1.0, reason: '空传输完成即满格');
    });

    test('speed, elapsed and remaining wording', () {
      expect(formatSpeed(0), '0 B/s');
      expect(formatSpeed(512), '512 B/s');
      expect(formatSpeed(3.2 * mib), '3.2 MB/s');
      expect(formatElapsed(const Duration(milliseconds: 300)), '不到 1 秒');
      expect(formatElapsed(const Duration(seconds: 45)), '45 秒');
      expect(formatElapsed(const Duration(minutes: 3, seconds: 5)),
          '3 分 05 秒');
      expect(formatElapsed(const Duration(hours: 1, minutes: 2)),
          '1 小时 02 分');
      expect(formatRemaining(const Duration(milliseconds: 1)), '约 1 秒');
      expect(formatRemaining(const Duration(seconds: 8)), '约 8 秒');
      expect(formatRemaining(const Duration(seconds: 121)), '约 3 分钟');
      expect(formatRemaining(const Duration(hours: 1, seconds: 1)), '约 2 小时');
    });

    test('directPathLabel names only direct paths and never guesses', () {
      expect(directPathLabel('192.168.1.5:5000'), '局域网直连');
      expect(directPathLabel('10.0.0.2:1'), '局域网直连');
      expect(directPathLabel('172.16.0.1:1'), '局域网直连');
      expect(directPathLabel('172.32.0.1:1'), '互联网直连');
      expect(directPathLabel('169.254.10.1:1'), '局域网直连');
      expect(directPathLabel('203.0.113.7:4000'), '互联网直连');
      expect(directPathLabel('100.64.1.1:1'), '互联网直连');
      expect(directPathLabel('[fe80::1%eth0]:5000'), '局域网直连');
      expect(directPathLabel('[fd00::1]:1'), '局域网直连');
      expect(directPathLabel('[2001:db8::1]:443'), 'IPv6 直连');
      expect(directPathLabel('[::ffff:192.168.1.2]:1'), '局域网直连');
      expect(directPathLabel('127.0.0.1:45678'), '本机直连');
      expect(directPathLabel('[::1]:1'), '本机直连');
      expect(directPathLabel('not-an-address:1'), isNull);
      expect(directPathLabel(''), isNull);
      expect(directPathLabel(null), isNull);
    });

    test('the history keeps the newest first, bounded, and notifies', () {
      final history = TransferHistory(capacity: 2);
      var notified = 0;
      history.addListener(() => notified++);
      for (var i = 0; i < 3; i++) {
        history.add(TransferRecord(
          role: TaskRole.send,
          peerName: 'peer-$i',
          succeeded: true,
          finishedAt: DateTime.utc(2026, 11, 13, 12, i),
        ));
      }
      expect(history.records.map((r) => r.peerName), ['peer-2', 'peer-1']);
      expect(notified, 3);
      history.clear();
      expect(history.isEmpty, isTrue);
      expect(notified, 4);
      history.clear(); // already empty: no extra notification
      expect(notified, 4);
      history.dispose();
    });

    test('success colors are legible per brightness', () {
      final light = ColorScheme.fromSeed(seedColor: appSeedColor);
      final dark = ColorScheme.fromSeed(
          seedColor: appSeedColor, brightness: Brightness.dark);
      expect(successColor(light), isNot(successColor(dark)));
      expect(buildAppTheme(Brightness.dark).colorScheme.brightness,
          Brightness.dark);
    });
  });
}
