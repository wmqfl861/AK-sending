/// Unit tests for lib/src/state/job_controller.dart: full event sequences
/// for a single-connection send and a two-connection receive, phase
/// transitions, aggregated views, and the result handshake. Pure Dart.
library;

import 'dart:convert';

import 'package:ak_ffi_dart/src/model/progress_events.dart';
import 'package:ak_ffi_dart/src/state/job_controller.dart';
import 'package:test/test.dart';

const String jobHex = 'a1b2c3d4e5f60718293a4b5c6d7e8f90';
const String peerHex =
    '0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0';

ProgressEvent parse(String line) => ProgressEvent.parse(line);

void feedAll(JobController controller, List<String> lines) {
  for (final line in lines) {
    controller.feed(parse(line));
  }
}

void main() {
  group('JobController single-connection send', () {
    JobController build() {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","event":"connecting"}',
        '{"job":"$jobHex","event":"connected","peer":"$peerHex"}',
        '{"job":"$jobHex","event":"offered","files":3,"bytes":196608}',
        '{"job":"$jobHex","event":"manifest_page_sent","page":0}',
        '{"job":"$jobHex","event":"manifest_received"}',
        '{"job":"$jobHex","event":"file_started","file":0,'
            '"path":"photos/a.bin","bytes":131072}',
        '{"job":"$jobHex","event":"chunk_sent","file":0,"chunk":0}',
        '{"job":"$jobHex","event":"chunk_sent","file":0,"chunk":1}',
        '{"job":"$jobHex","event":"chunk_sent","file":0,"chunk":2}',
        '{"job":"$jobHex","event":"chunk_received","file":0,"chunk":0}',
        '{"job":"$jobHex","event":"bytes_acked","file":0,"bytes":65536}',
        '{"job":"$jobHex","event":"paused"}',
        '{"job":"$jobHex","event":"resumed"}',
        '{"job":"$jobHex","event":"file_started","file":1,'
            '"path":"photos/b.bin","bytes":65536}',
        '{"job":"$jobHex","event":"bytes_acked","file":1,"bytes":65536}',
        '{"job":"$jobHex","event":"file_verified","file":0}',
        '{"job":"$jobHex","event":"file_committed","file":0,'
            '"path":"photos/a.bin"}',
        '{"job":"$jobHex","event":"verifying"}',
        '{"job":"$jobHex","event":"final_ack","verified":true}',
        '{"job":"$jobHex","event":"done"}',
      ]);
      return controller;
    }

    test('phase walks idle -> connecting -> transferring -> verifying -> done',
        () {
      final controller = JobController();
      expect(controller.phase, JobPhase.idle);
      expect(controller.connections, isEmpty);
      expect(controller.fileTable, isEmpty);

      controller.feed(parse('{"job":"$jobHex","event":"connecting"}'));
      expect(controller.phase, JobPhase.connecting);

      controller.feed(
        parse('{"job":"$jobHex","event":"connected","peer":"$peerHex"}'),
      );
      expect(controller.phase, JobPhase.connecting);

      controller.feed(
        parse('{"job":"$jobHex","event":"offered","files":3,"bytes":196608}'),
      );
      controller.feed(parse('{"job":"$jobHex","event":"manifest_received"}'));
      expect(controller.phase, JobPhase.connecting,
          reason: 'negotiation events stay in connecting');

      controller.feed(parse(
        '{"job":"$jobHex","event":"file_started","file":0,'
        '"path":"photos/a.bin","bytes":131072}',
      ));
      expect(controller.phase, JobPhase.transferring);

      controller.feed(parse('{"job":"$jobHex","event":"verifying"}'));
      expect(controller.phase, JobPhase.verifying);

      controller.feed(parse('{"job":"$jobHex","event":"done"}'));
      expect(controller.phase, JobPhase.done);
    });

    test('conn-less events land in the implicit connection 0', () {
      final controller = build();
      expect(controller.connections.keys, [0]);
      final conn = controller.connections[0]!;
      expect(conn.peer, peerHex);
      expect(conn.phase, JobPhase.done);
      expect(conn.expectedFiles, 3);
      expect(conn.expectedBytes, 196608);
      expect(conn.finalVerified, isTrue);
      expect(conn.lastManifestPage, 0);
      expect(conn.files.keys, [0, 1]);
    });

    test('file table tracks per-file progress', () {
      final controller = build();
      final table = controller.fileTable;
      expect(table.keys.toSet(), {'0:0', '0:1'});
      final a = table['0:0']!;
      expect(a.path, 'photos/a.bin');
      expect(a.size, 131072);
      expect(a.ackedBytes, 65536);
      expect(a.sentChunks, 3);
      expect(a.lastChunkSent, 2);
      expect(a.receivedChunks, 1);
      expect(a.lastChunkReceived, 0);
      expect(a.verified, isTrue);
      expect(a.committed, isTrue);
      final b = table['0:1']!;
      expect(b.path, 'photos/b.bin');
      expect(b.ackedBytes, 65536);
      expect(b.verified, isFalse);
      expect(b.committed, isFalse);
      expect(controller.ackedBytes, 131072);
      expect(controller.expectedFiles, 3);
      expect(controller.expectedBytes, 196608);
      expect(controller.verifiedFileCount, 1);
      expect(controller.committedFileCount, 1);
      expect(controller.allFiles.length, 2);
    });

    test('paused/resumed toggles the flag without leaving transferring', () {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","event":"connecting"}',
        '{"job":"$jobHex","event":"file_started","file":0,'
            '"path":"a.bin","bytes":10}',
      ]);
      expect(controller.paused, isFalse);
      controller.feed(parse('{"job":"$jobHex","event":"paused"}'));
      expect(controller.paused, isTrue);
      expect(controller.phase, JobPhase.transferring);
      controller.feed(parse('{"job":"$jobHex","event":"resumed"}'));
      expect(controller.paused, isFalse);
      expect(controller.phase, JobPhase.transferring);
    });

    test('bytes_acked keeps progress monotonic on counter restarts', () {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","event":"file_started","file":0,"path":"a.bin","bytes":100}',
        '{"job":"$jobHex","event":"bytes_acked","file":0,"bytes":100}',
        '{"job":"$jobHex","event":"bytes_acked","file":0,"bytes":40}',
      ]);
      expect(controller.fileTable['0:0']!.ackedBytes, 100);
    });

    test('late chunks after verifying never regress the phase', () {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","event":"file_started","file":0,"path":"a.bin","bytes":10}',
        '{"job":"$jobHex","event":"verifying"}',
        '{"job":"$jobHex","event":"chunk_received","file":0,"chunk":9}',
      ]);
      expect(controller.phase, JobPhase.verifying);
      expect(controller.fileTable['0:0']!.receivedChunks, 1);
    });

    test('success result fills the report and keeps done', () {
      final controller = build();
      controller.setResultJson((jsonDecode(
        '{"job":"$jobHex","wire_job":"00112233445566778899aabbccddeeff",'
        '"files":3,"bytes":196608,"duration_ms":81200,"resumed":0,'
        '"peer":"$peerHex","roots":["photos"]}',
      ) as Map).cast<String, Object?>());
      expect(controller.phase, JobPhase.done);
      expect(controller.hasResult, isTrue);
      expect(controller.report, isNotNull);
      expect(controller.report!.files, 3);
      expect(controller.report!.durationText, '01:21');
      expect(controller.error, isNull);
      expect(controller.lastEvent, isA<Done>());
    });

    test('listeners fire once per feed and per result', () {
      final controller = JobController();
      var notifications = 0;
      final other = JobController();
      controller.addListener(() => notifications++);
      final lines = <String>[
        '{"job":"$jobHex","event":"connecting"}',
        '{"job":"$jobHex","event":"file_started","file":0,"path":"a.bin","bytes":1}',
        '{"job":"$jobHex","event":"done"}',
      ];
      feedAll(controller, lines);
      expect(notifications, 3);
      controller.setResultJson(
        (jsonDecode('{"job":"$jobHex","error":"late failure"}') as Map)
            .cast<String, Object?>(),
      );
      expect(notifications, 4);
      // removeListener stops delivery; the other controller is untouched.
      void echo() => notifications++;
      controller.addListener(echo);
      controller.removeListener(echo);
      controller.removeListener(echo); // removing twice is a no-op
      other.feed(parse('{"job":"$jobHex","event":"connecting"}'));
      expect(notifications, 4);
    });
  });

  group('JobController two-connection receive', () {
    JobController build() {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","conn":0,"event":"accepted","files":2,"bytes":100}',
        '{"job":"$jobHex","conn":1,"event":"accepted","files":1,"bytes":50}',
        '{"job":"$jobHex","conn":0,"event":"file_started","file":0,'
            '"path":"x.bin","bytes":100}',
        '{"job":"$jobHex","conn":1,"event":"file_started","file":0,'
            '"path":"y.bin","bytes":50}',
        '{"job":"$jobHex","conn":0,"event":"chunk_received","file":0,"chunk":0}',
        '{"job":"$jobHex","conn":0,"event":"bytes_acked","file":0,"bytes":60}',
        '{"job":"$jobHex","conn":1,"event":"bytes_acked","file":0,"bytes":50}',
        '{"job":"$jobHex","conn":1,"event":"file_verified","file":0}',
        '{"job":"$jobHex","conn":1,"event":"verifying"}',
        '{"job":"$jobHex","conn":1,"event":"final_ack","verified":true}',
        '{"job":"$jobHex","conn":1,"event":"done"}',
      ]);
      return controller;
    }

    test('connections aggregate per conn index', () {
      final controller = build();
      expect(controller.connections.keys.toSet(), {0, 1});
      expect(controller.expectedFiles, 3);
      expect(controller.expectedBytes, 150);
      expect(controller.ackedBytes, 110);
      expect(controller.connections[1]!.phase, JobPhase.done);
      expect(controller.connections[0]!.phase, JobPhase.transferring);
      expect(controller.connections[1]!.finalVerified, isTrue);
    });

    test('overall phase follows the slowest active connection', () {
      final controller = build();
      expect(controller.phase, JobPhase.transferring,
          reason: 'conn 1 is done but conn 0 still transfers');
      controller.feed(parse('{"job":"$jobHex","conn":0,"event":"verifying"}'));
      expect(controller.phase, JobPhase.verifying);
      controller.feed(parse('{"job":"$jobHex","conn":0,"event":"done"}'));
      expect(controller.phase, JobPhase.done);
    });

    test('same file index in different conns stays distinct', () {
      final controller = build();
      final table = controller.fileTable;
      expect(table.keys.toSet(), {'0:0', '1:0'});
      expect(table['0:0']!.path, 'x.bin');
      expect(table['0:0']!.conn, 0);
      expect(table['1:0']!.path, 'y.bin');
      expect(table['1:0']!.conn, 1);
      expect(table['1:0']!.verified, isTrue);
      expect(table['0:0']!.verified, isFalse);
      expect(controller.allFiles.length, 2);
    });

    test('aggregate multi-connection success result', () {
      final controller = build();
      controller.setResultJson((jsonDecode(
        '{"job":"$jobHex","jobs":2,'
        '"wire_jobs":["00112233445566778899aabbccddeeff",'
        '"ffeeddccbbaa99887766554433221100"],'
        '"files":3,"bytes":150,"duration_ms":500,"resumed":0,"roots":[]}',
      ) as Map).cast<String, Object?>());
      expect(controller.phase, JobPhase.done);
      expect(controller.report!.jobs, 2);
      expect(controller.report!.wireJobs!.length, 2);
      expect(controller.report!.wireJob, isNull);
      expect(controller.outcome!.job, jobHex);
    });

    test('multi-connection failure result is failed and sticky', () {
      final controller = build();
      controller.setResultJson((jsonDecode(
        '{"job":"$jobHex","jobs":2,"ok":1,"failed":1,'
        '"error":"conn 0 died","errors":["conn 0 died"]}',
      ) as Map).cast<String, Object?>());
      expect(controller.phase, JobPhase.failed);
      expect(controller.error, 'conn 0 died');
      expect(controller.report, isNull);
      // Events after the result still fold in, but never revive failed.
      controller.feed(
        parse('{"job":"$jobHex","conn":0,"event":"chunk_received",'
            '"file":0,"chunk":1}'),
      );
      expect(controller.phase, JobPhase.failed);
      expect(controller.lastEvent, isA<ChunkReceived>());
    });
  });

  group('JobController edge behavior', () {
    test('unknown events update lastEvent only', () {
      final controller = JobController();
      controller.feed(parse('{"job":"$jobHex","event":"connecting"}'));
      controller.feed(
        parse('{"job":"$jobHex","event":"quantum","answer":42}'),
      );
      expect(controller.phase, JobPhase.connecting);
      final last = controller.lastEvent!;
      expect(last, isA<UnknownEvent>());
      expect((last as UnknownEvent).name, 'quantum');
    });

    test('result-only job reaches a terminal phase with no events', () {
      final ok = JobController();
      ok.setResultJson((jsonDecode(
        '{"job":"$jobHex","wire_job":"00112233445566778899aabbccddeeff",'
        '"files":1,"bytes":1,"duration_ms":5,"resumed":0,"peer":null,"roots":[]}',
      ) as Map).cast<String, Object?>());
      expect(ok.phase, JobPhase.done);
      expect(ok.report!.peer, isNull);

      final failed = JobController();
      failed.setResultJson(
        (jsonDecode('{"job":"$jobHex","error":"bind failed"}') as Map)
            .cast<String, Object?>(),
      );
      expect(failed.phase, JobPhase.failed);
      expect(failed.error, 'bind failed');
    });

    test('a success result outranks a stalled event stream', () {
      final controller = JobController();
      controller.feed(parse('{"job":"$jobHex","event":"connecting"}'));
      controller.setResultJson((jsonDecode(
        '{"job":"$jobHex","wire_job":"00112233445566778899aabbccddeeff",'
        '"files":1,"bytes":1,"duration_ms":5,"resumed":0,"peer":"$peerHex",'
        '"roots":[]}',
      ) as Map).cast<String, Object?>());
      expect(controller.phase, JobPhase.done);
      controller.feed(parse('{"job":"$jobHex","event":"connecting"}'));
      expect(controller.phase, JobPhase.done,
          reason: 'a success result pins done');
    });

    test('resumed session without file_started still builds the file table',
        () {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","event":"connected","peer":"$peerHex"}',
        '{"job":"$jobHex","event":"resuming","chunks":12}',
        '{"job":"$jobHex","event":"file_committed","file":0,"path":"a.bin"}',
      ]);
      expect(controller.connections[0]!.resumedChunks, 12);
      final row = controller.fileTable['0:0']!;
      expect(row.path, 'a.bin');
      expect(row.committed, isTrue);
      expect(controller.phase, JobPhase.transferring,
          reason: 'file_committed bumps an idle connection to transferring');
    });

    test('listening keeps a receiver in connecting and records the address',
        () {
      final controller = JobController();
      controller.feed(parse(
        '{"job":"$jobHex","event":"listening","addr":"127.0.0.1:52341"}',
      ));
      expect(controller.phase, JobPhase.connecting);
      expect(controller.connections[0]!.addr, '127.0.0.1:52341');
    });

    test('paused on one conn marks the job paused', () {
      final controller = JobController();
      feedAll(controller, [
        '{"job":"$jobHex","conn":0,"event":"file_started","file":0,'
            '"path":"a.bin","bytes":1}',
        '{"job":"$jobHex","conn":1,"event":"file_started","file":0,'
            '"path":"b.bin","bytes":1}',
        '{"job":"$jobHex","conn":1,"event":"paused"}',
      ]);
      expect(controller.paused, isTrue);
      expect(controller.connections[0]!.paused, isFalse);
      expect(controller.connections[1]!.paused, isTrue);
    });

    test('reset returns to a fresh idle state but keeps listeners', () {
      final controller = JobController();
      var notifications = 0;
      controller.addListener(() => notifications++);
      feedAll(controller, [
        '{"job":"$jobHex","event":"connecting"}',
        '{"job":"$jobHex","event":"file_started","file":0,"path":"a.bin","bytes":1}',
      ]);
      controller.setResultJson(
        (jsonDecode('{"job":"$jobHex","error":"x"}') as Map)
            .cast<String, Object?>(),
      );
      controller.reset();
      expect(controller.phase, JobPhase.idle);
      expect(controller.connections, isEmpty);
      expect(controller.fileTable, isEmpty);
      expect(controller.error, isNull);
      expect(controller.report, isNull);
      expect(controller.hasResult, isFalse);
      expect(controller.lastEvent, isNull);
      expect(controller.expectedFiles, isNull);
      expect(controller.ackedBytes, 0);
      // The surviving listener was notified by reset.
      expect(notifications, 4);
    });
  });
}
