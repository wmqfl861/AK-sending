/// Unit tests for lib/src/model/: progress event decoding, report/result
/// decoding and formatting, display helpers, and the accept policy.
/// Pure Dart — no FFI anywhere.
library;

import 'dart:convert';

import 'package:ak_ffi_dart/src/model/accept_policy.dart';
import 'package:ak_ffi_dart/src/model/display.dart';
import 'package:ak_ffi_dart/src/model/progress_events.dart';
import 'package:ak_ffi_dart/src/model/report.dart';
import 'package:test/test.dart';

const String jobHex = 'a1b2c3d4e5f60718293a4b5c6d7e8f90';
const String peerHex =
    '0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0';

ProgressEvent parse(String line) => ProgressEvent.parse(line);

Map<String, Object?> decode(String line) =>
    (jsonDecode(line) as Map).cast<String, Object?>();

void main() {
  group('ProgressEvent.parse', () {
    test('decodes all 19 contract event names', () {
      expect(knownEventNames.length, 19);
      expect(
        parse('{"job":"$jobHex","event":"connecting"}'),
        isA<Connecting>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"listening","addr":"127.0.0.1:52341"}'),
        isA<Listening>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"connected","peer":"$peerHex"}'),
        isA<Connected>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"offered","files":3,"bytes":123456}'),
        isA<Offered>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"accepted","files":3,"bytes":123456}'),
        isA<Accepted>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"manifest_page_sent","page":0}'),
        isA<ManifestPageSent>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"manifest_received"}'),
        isA<ManifestReceived>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"resuming","chunks":12}'),
        isA<Resuming>(),
      );
      expect(
        parse(
          '{"job":"$jobHex","event":"file_started","file":0,'
          '"path":"photos/img.bin","bytes":65536}',
        ),
        isA<FileStarted>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"chunk_sent","file":0,"chunk":3}'),
        isA<ChunkSent>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"chunk_received","file":0,"chunk":3}'),
        isA<ChunkReceived>(),
      );
      expect(
        parse('{"job":"$jobHex","event":"bytes_acked","file":0,"bytes":65536}'),
        isA<BytesAcked>(),
      );
      expect(parse('{"job":"$jobHex","event":"paused"}'), isA<Paused>());
      expect(parse('{"job":"$jobHex","event":"resumed"}'), isA<Resumed>());
      expect(
        parse('{"job":"$jobHex","event":"file_verified","file":0}'),
        isA<FileVerified>(),
      );
      expect(
        parse(
          '{"job":"$jobHex","event":"file_committed","file":0,'
          '"path":"photos/img.bin"}',
        ),
        isA<FileCommitted>(),
      );
      expect(parse('{"job":"$jobHex","event":"verifying"}'), isA<Verifying>());
      expect(
        parse('{"job":"$jobHex","event":"final_ack","verified":true}'),
        isA<FinalAck>(),
      );
      expect(parse('{"job":"$jobHex","event":"done"}'), isA<Done>());
    });

    test('payload fields land on the typed classes', () {
      final listening =
          parse('{"job":"$jobHex","event":"listening","addr":"127.0.0.1:52341"}')
              as Listening;
      expect(listening.addr, '127.0.0.1:52341');
      expect(listening.name, 'listening');
      expect(listening.conn, isNull);

      final started =
          parse(
            '{"job":"$jobHex","event":"file_started","file":7,'
            '"path":"photos/img.bin","bytes":65536}',
          )
              as FileStarted;
      expect(started.file, 7);
      expect(started.path, 'photos/img.bin');
      expect(started.bytes, 65536);

      final offered =
          parse('{"job":"$jobHex","event":"offered","files":3,"bytes":123456}')
              as Offered;
      expect(offered.files, 3);
      expect(offered.bytes, 123456);

      final ack =
          parse('{"job":"$jobHex","event":"final_ack","verified":false}')
              as FinalAck;
      expect(ack.verified, isFalse);
    });

    test('conn index parses on multi-connection receive lines', () {
      final event =
          parse(
            '{"job":"$jobHex","conn":1,"event":"file_started","file":0,'
            '"path":"x.bin","bytes":10}',
          )
              as FileStarted;
      expect(event.conn, 1);
      expect(event.job, jobHex);
      final same = FileStarted(
        job: jobHex,
        conn: 1,
        file: 0,
        path: 'x.bin',
        bytes: 10,
      );
      expect(event, same);
      expect(event.hashCode, same.hashCode);
      expect(event == FileStarted(job: jobHex, file: 0, path: 'x.bin', bytes: 10),
          isFalse, reason: 'conn differs');
    });

    test('unknown event names decode to UnknownEvent without throwing', () {
      final event = parse('{"job":"$jobHex","event":"quantum","answer":42}');
      expect(event, isA<UnknownEvent>());
      final unknown = event as UnknownEvent;
      expect(unknown.name, 'quantum');
      expect(unknown.job, jobHex);
      expect(unknown.data, {'answer': 42});
      // Unknown events compare by name + data too.
      expect(
        unknown,
        UnknownEvent(job: jobHex, name: 'quantum', data: {'answer': 42}),
      );
      expect(
        unknown == UnknownEvent(job: jobHex, name: 'other', data: {'answer': 42}),
        isFalse,
      );
    });

    test('equality ignores unknown extra fields on known events', () {
      final plain = parse('{"job":"$jobHex","event":"connecting"}');
      final withExtra = parse('{"job":"$jobHex","event":"connecting","hint":1}');
      expect(plain, withExtra);
    });

    test('missing job or event throws FormatException', () {
      expect(() => parse('{"event":"connecting"}'), throwsFormatException);
      expect(() => parse('{"job":"$jobHex"}'), throwsFormatException);
      expect(() => parse('{}'), throwsFormatException);
    });

    test('missing payload fields throw FormatException', () {
      expect(
        () => parse('{"job":"$jobHex","event":"offered","files":3}'),
        throwsFormatException,
        reason: 'offered without bytes',
      );
      expect(
        () => parse('{"job":"$jobHex","event":"file_started","file":0,"bytes":1}'),
        throwsFormatException,
        reason: 'file_started without path',
      );
      expect(
        () => parse('{"job":"$jobHex","event":"final_ack"}'),
        throwsFormatException,
        reason: 'final_ack without verified',
      );
      expect(
        () => parse('{"job":"$jobHex","event":"manifest_page_sent"}'),
        throwsFormatException,
        reason: 'manifest_page_sent without page',
      );
    });

    test('wrong payload types throw FormatException', () {
      expect(
        () => parse('{"job":"$jobHex","event":"offered","files":"3","bytes":1}'),
        throwsFormatException,
      );
      expect(
        () => parse(
          '{"job":"$jobHex","event":"file_started","file":0,"path":7,"bytes":1}',
        ),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":"$jobHex","event":"resuming","chunks":1.5}'),
        throwsFormatException,
        reason: 'fractional number is not an integer token',
      );
      expect(
        () => parse('{"job":"$jobHex","event":"final_ack","verified":"yes"}'),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":"$jobHex","event":"connected","peer":64}'),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":42,"event":"connecting"}'),
        throwsFormatException,
      );
    });

    test('bad conn values throw FormatException', () {
      expect(
        () => parse('{"job":"$jobHex","conn":"0","event":"connecting"}'),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":"$jobHex","conn":-1,"event":"connecting"}'),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":"$jobHex","conn":0.5,"event":"connecting"}'),
        throwsFormatException,
      );
      expect(
        parse('{"job":"$jobHex","conn":0,"event":"connecting"}').conn,
        0,
        reason: 'conn 0 is a valid multi-connection index',
      );
    });

    test('negative payload integers are rejected', () {
      expect(
        () => parse('{"job":"$jobHex","event":"manifest_page_sent","page":-2}'),
        throwsFormatException,
      );
      expect(
        () => parse('{"job":"$jobHex","event":"chunk_sent","file":0,"chunk":-1}'),
        throwsFormatException,
      );
    });

    test('non-object or invalid JSON lines throw FormatException', () {
      expect(() => parse('not json'), throwsFormatException);
      expect(() => parse('[1,2]'), throwsFormatException);
      expect(() => parse('null'), throwsFormatException);
      expect(() => parse('"connecting"'), throwsFormatException);
    });

    test('fromJson accepts a decoded map directly', () {
      final event = ProgressEvent.fromJson(decode('{"job":"$jobHex","event":"done"}'));
      expect(event, isA<Done>());
      expect(event.job, jobHex);
    });

    test('toString includes name, job, conn and fields', () {
      final event = parse(
        '{"job":"$jobHex","conn":2,"event":"file_started","file":1,'
        '"path":"a.bin","bytes":5}',
      );
      expect(
        event.toString(),
        'file_started job=$jobHex conn=2 file=1 path=a.bin bytes=5',
      );
    });
  });

  group('TransferReport', () {
    test('decodes the single-connection success body', () {
      final report = TransferReport.fromJson(decode(
        '{"job":"$jobHex","wire_job":"00112233445566778899aabbccddeeff",'
        '"files":3,"bytes":123456,"duration_ms":812,"resumed":0,'
        '"peer":"$peerHex","roots":["dir","extra"]}',
      ));
      expect(report.job, jobHex);
      expect(report.wireJob, '00112233445566778899aabbccddeeff');
      expect(report.files, 3);
      expect(report.bytes, 123456);
      expect(report.durationMs, 812);
      expect(report.resumed, 0);
      expect(report.peer, peerHex);
      expect(report.roots, ['dir', 'extra']);
      expect(report.jobs, isNull);
      expect(report.wireJobs, isNull);
      expect(report.durationText, '00:00');
      expect(report.bytesText, '120.56 KiB');
      expect(report.speedText, '148.48 KiB/s');
      expect(report.bytesPerSecond, closeTo(152039.41, 0.5));
    });

    test('decodes the multi-connection aggregate success body', () {
      final report = TransferReport.fromJson(decode(
        '{"job":"$jobHex","jobs":2,'
        '"wire_jobs":["00112233445566778899aabbccddeeff",'
        '"ffeeddccbbaa99887766554433221100"],'
        '"files":5,"bytes":246912,"duration_ms":900,"resumed":2,"roots":[]}',
      ));
      expect(report.wireJob, isNull);
      expect(report.jobs, 2);
      expect(report.wireJobs!.length, 2);
      expect(report.wireJobs![1], 'ffeeddccbbaa99887766554433221100');
      expect(report.files, 5);
      expect(report.resumed, 2);
      expect(report.roots, isEmpty);
      expect(report.peer, isNull);
    });

    test('missing or mistyped required fields throw FormatException', () {
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","bytes":1,"duration_ms":1}'),
        ),
        throwsFormatException,
        reason: 'files missing',
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","files":1,"duration_ms":1}'),
        ),
        throwsFormatException,
        reason: 'bytes missing',
      );
      expect(
        () => TransferReport.fromJson(decode('{"job":"j","files":1,"bytes":1}')),
        throwsFormatException,
        reason: 'duration_ms missing',
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","files":1,"bytes":1,"duration_ms":"812"}'),
        ),
        throwsFormatException,
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"files":1,"bytes":1,"duration_ms":1}'),
        ),
        throwsFormatException,
        reason: 'job missing',
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","files":1,"bytes":1,"duration_ms":1,"roots":["a",2]}'),
        ),
        throwsFormatException,
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","files":1,"bytes":1,"duration_ms":1,"peer":5}'),
        ),
        throwsFormatException,
      );
      expect(
        () => TransferReport.fromJson(
          decode('{"job":"j","files":1,"bytes":1,"duration_ms":1,"resumed":-1}'),
        ),
        throwsFormatException,
      );
    });

    test('equality covers every field', () {
      final a = TransferReport.fromJson(decode(
        '{"job":"j","wire_job":"w","files":1,"bytes":2,"duration_ms":3,'
        '"resumed":4,"peer":"p","roots":["r"]}',
      ));
      final b = TransferReport.fromJson(decode(
        '{"roots":["r"],"peer":"p","resumed":4,"duration_ms":3,"bytes":2,'
        '"files":1,"wire_job":"w","job":"j"}',
      ));
      expect(a, b);
      expect(a.hashCode, b.hashCode);
      final c = TransferReport.fromJson(decode(
        '{"job":"j","wire_job":"w","files":1,"bytes":2,"duration_ms":3,'
        '"resumed":4,"peer":"p","roots":["other"]}',
      ));
      expect(a == c, isFalse);
    });
  });

  group('JobOutcome', () {
    test('routes a success body to JobSucceeded', () {
      final outcome = JobOutcome.fromJson(decode(
        '{"job":"$jobHex","wire_job":"00112233445566778899aabbccddeeff",'
        '"files":3,"bytes":123456,"duration_ms":812,"resumed":0,'
        '"peer":"$peerHex","roots":["dir"]}',
      ));
      expect(outcome, isA<JobSucceeded>());
      expect(outcome.job, jobHex);
      expect((outcome as JobSucceeded).report.files, 3);
    });

    test('routes a failure body to JobFailed', () {
      final outcome = JobFailed.fromJson(decode('{"job":"$jobHex","error":"boom"}'));
      expect(outcome.job, jobHex);
      expect(outcome.error, 'boom');
      expect(outcome.jobs, isNull);
      expect(outcome.errors, isEmpty);
    });

    test('decodes the multi-connection failure body', () {
      final outcome = JobFailed.fromJson(decode(
        '{"job":"$jobHex","jobs":2,"ok":1,"failed":1,'
        '"error":"conn 1 died","errors":["conn 1 died"]}',
      ));
      expect(outcome.jobs, 2);
      expect(outcome.ok, 1);
      expect(outcome.failed, 1);
      expect(outcome.errors, ['conn 1 died']);
      expect(JobOutcome.fromJson(decode('{"job":"j","error":"x"}')),
          isA<JobFailed>());
    });

    test('failure body with a mistyped error throws FormatException', () {
      expect(
        () => JobFailed.fromJson(decode('{"job":"j","error":7}')),
        throwsFormatException,
      );
      expect(
        () => JobFailed.fromJson(decode('{"job":"j"}')),
        throwsFormatException,
      );
    });
  });

  group('humanBytes', () {
    test('binary units with two decimals, B as integer', () {
      expect(humanBytes(0), '0 B');
      expect(humanBytes(512), '512 B');
      expect(humanBytes(1023), '1023 B');
      expect(humanBytes(1024), '1.00 KiB');
      expect(humanBytes(1536), '1.50 KiB');
      expect(humanBytes(1048576), '1.00 MiB');
      expect(humanBytes(1073741824), '1.00 GiB');
      expect(humanBytes(1099511627776), '1.00 TiB');
      expect(humanBytes(1125899906842624), '1.00 PiB');
      // No unit beyond PiB, mirroring the Rust loop.
      expect(humanBytes(1152921504606846976), '1024.00 PiB');
    });

    test('negative input is an argument error', () {
      expect(() => humanBytes(-1), throwsArgumentError);
    });
  });

  group('formatDurationMs', () {
    test('mm:ss, truncating sub-seconds, minutes unbounded', () {
      expect(formatDurationMs(0), '00:00');
      expect(formatDurationMs(812), '00:00');
      expect(formatDurationMs(59999), '00:59');
      expect(formatDurationMs(60000), '01:00');
      expect(formatDurationMs(81200), '01:21');
      expect(formatDurationMs(3723500), '62:03');
      expect(formatDurationMs(-5), '00:00');
    });
  });

  group('speed helpers', () {
    test('average and formatted rate', () {
      expect(averageBytesPerSecond(1048576, 1000), 1048576.0);
      expect(averageBytesPerSecond(2000, 500), 4000.0);
      expect(averageBytesPerSecond(100, 0), isNull);
      expect(averageBytesPerSecond(100, -10), isNull);
      expect(formatSpeed(1048576, 1000), '1.00 MiB/s');
      expect(formatSpeed(1024, 500), '2.00 KiB/s');
      expect(formatSpeed(512, 1000), '512 B/s');
      expect(formatSpeed(0, 1000), '0 B/s');
      expect(formatSpeed(100, 0), isNull);
    });
  });

  group('display helpers', () {
    test('two-line fingerprint display matches ak-cli layout', () {
      final fp64 =
          'A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4E5F60718293A4B5C6D7E8F90';
      final lower = fp64.toLowerCase();
      expect(fp64.length, 64);
      expect(fingerprintLines('peer', fp64), 'peer: a1b2c3d4\npeer (full): $lower');
      expect(fingerprintShortId(fp64), 'a1b2c3d4');
      expect(isFingerprintHex(fp64), isTrue);
    });

    test('fingerprint shape validation', () {
      expect(isFingerprintHex('zz' * 32), isFalse);
      expect(isFingerprintHex('ab' * 31), isFalse);
      expect(isFingerprintHex('ab' * 33), isFalse);
      expect(() => fingerprintShortId('ab' * 31), throwsFormatException);
      expect(() => fingerprintLines('peer', 'nope'), throwsFormatException);
    });

    test('pairing code normalization to ###-###', () {
      expect(formatPairingCode('123456'), '123-456');
      expect(formatPairingCode('123-456'), '123-456');
      expect(formatPairingCode('123 456'), '123-456');
      expect(formatPairingCode('1-2-3-4-5-6'), '123-456');
      expect(formatPairingCode('000000'), '000-000');
      expect(formatPairingCode('999999'), '999-999');
    });

    test('pairing code rejects wrong shapes', () {
      expect(() => formatPairingCode('12345'), throwsFormatException);
      expect(() => formatPairingCode('1234567'), throwsFormatException);
      expect(() => formatPairingCode(''), throwsFormatException);
      expect(() => formatPairingCode('12a456'), throwsFormatException);
      expect(() => formatPairingCode('123-45'), throwsFormatException);
      expect(() => formatPairingCode('123456x'), throwsFormatException);
      expect(() => formatPairingCode('١٢٣٤٥٦'), throwsFormatException,
          reason: 'non-ASCII digits rejected like Rust byte matching');
    });

    test('progress percent rounds and clamps', () {
      expect(progressPercent(1, 4), 25);
      expect(progressPercent(1, 3), 33);
      expect(progressPercent(2, 3), 67);
      expect(progressPercent(4, 4), 100);
      expect(progressPercent(5, 4), 100);
      expect(progressPercent(-3, 4), 0);
      expect(progressPercent(0, 0), 0);
    });

    test('progress bar text layout', () {
      expect(
        progressBarText(transferred: 1, total: 4),
        '[###-------] 25% 1 B/4 B',
      );
      expect(
        progressBarText(transferred: 3, total: 10),
        '[###-------] 30% 3 B/10 B',
      );
      expect(
        progressBarText(transferred: 4, total: 4),
        '[##########] 100% 4 B/4 B',
      );
      expect(
        progressBarText(transferred: 0, total: 0),
        '[----------] 0% 0 B/0 B',
      );
      expect(
        progressBarText(transferred: 5, total: 4),
        '[##########] 100% 5 B/4 B',
        reason: 'bar clamps, byte counts stay honest',
      );
      expect(
        progressBarText(transferred: 4294967296, total: 8589934592),
        '[#####-----] 50% 4.00 GiB/8.00 GiB',
      );
      expect(
        progressBarText(transferred: 2, total: 4, width: 4),
        '[##--] 50% 2 B/4 B',
      );
      expect(
        () => progressBarText(transferred: 1, total: 1, width: 0),
        throwsArgumentError,
      );
    });
  });

  group('AcceptPolicy', () {
    test('"any" parses case-insensitively and warns', () {
      expect(AcceptPolicy.parse('any'), isA<AcceptAnyPolicy>());
      expect(AcceptPolicy.parse('ANY'), isA<AcceptAnyPolicy>());
      expect(AcceptPolicy.parse(' Any '), isA<AcceptAnyPolicy>());
      final any = AcceptPolicy.parse('any');
      expect(any.warnsUser, isTrue, reason: 'accepting anyone needs a warning');
      expect(any.canMatchLocally, isTrue);
      expect(any.matchFingerprint(peerHex), isTrue);
    });

    test('64-hex pins a fingerprint', () {
      final pinned = AcceptPolicy.parse(peerHex);
      expect(pinned, isA<AcceptPinnedPolicy>());
      expect(pinned.warnsUser, isFalse);
      expect((pinned as AcceptPinnedPolicy).fingerprintHex, peerHex);
      expect(pinned.matchFingerprint(peerHex), isTrue);
      expect(pinned.matchFingerprint(peerHex.toUpperCase()), isTrue,
          reason: 'hex compare is case-insensitive');
      final other = peerHex.replaceRange(0, 1, '9');
      expect(pinned.matchFingerprint(other), isFalse);
    });

    test('a 64-digit string is a fingerprint, not a code (Rust order)', () {
      final digits64 = '123456789012345678901234567890'
          '1234567890123456789012345678901234';
      expect(digits64.length, 64);
      expect(AcceptPolicy.parse(digits64), isA<AcceptPinnedPolicy>());
    });

    test('6-digit codes parse in every separator shape', () {
      for (final text in ['123456', '123-456', '123 456', '1-2-3-4-5-6']) {
        final policy = AcceptPolicy.parse(text);
        expect(policy, isA<AcceptPairingCodePolicy>(), reason: text);
        final code = policy as AcceptPairingCodePolicy;
        expect(code.code, text, reason: 'typed form preserved like Rust');
        expect(code.digits, '123456');
        expect(code.warnsUser, isFalse);
      }
    });

    test('unrecognized shapes throw the engine message', () {
      for (final text in [
        '12345',
        '1234567',
        'abc-def',
        '',
        '   ',
        'anyone',
        '12345x',
        '١٢٣٤٥٦',
      ]) {
        expect(
          () => AcceptPolicy.parse(text),
          throwsA(
            isA<FormatException>().having(
              (e) => e.message,
              'message',
              expectPolicyErrorMessage,
            ),
          ),
          reason: text,
        );
      }
    });

    test('tryParse returns null instead of throwing', () {
      expect(AcceptPolicy.tryParse('123-456'), isA<AcceptPairingCodePolicy>());
      expect(AcceptPolicy.tryParse('nope'), isNull);
    });

    test('pairing-code matching stays in Rust (no blake3 in Dart)', () {
      final code = AcceptPolicy.parse('123-456') as AcceptPairingCodePolicy;
      expect(code.canMatchLocally, isFalse);
      expect(
        () => code.matchFingerprint(peerHex),
        throwsUnsupportedError,
      );
    });

    test('matchFingerprint validates the fingerprint shape first', () {
      expect(
        () => AcceptPolicy.parse('any').matchFingerprint('ab'),
        throwsFormatException,
      );
      expect(
        () => (AcceptPolicy.parse(peerHex) as AcceptPinnedPolicy)
            .matchFingerprint('nope'),
        throwsFormatException,
      );
    });
  });
}
