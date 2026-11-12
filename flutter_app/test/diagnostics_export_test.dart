/// G2 (batchF 2026-11-13, CR-P3-06 diagnostics): the settings page's
/// 「导出连接诊断」entry — the export body carries the key diagnostic
/// event lines (receiver_bind / sender_bind / probe_observed /
/// connected / sent_via) plus the platform header, and the UI surfaces
/// the gateway's path. The gateway is injected so no platform plugin
/// runs here; the buffer is the shared singleton, cleared per test.
library;

import 'dart:convert' show jsonDecode;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show TaskConnectedEvent, TaskUnknownEvent;
import 'package:ak_sending_app/services/connection_diagnostics.dart';
import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/pages/settings_page.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';

/// Captures the export body; resolves with a fixed fake path.
class _CapturingExportGateway implements DiagnosticsExportGateway {
  final List<String> bodies = [];

  @override
  Future<String> exportDiagnostics(String content) async {
    bodies.add(content);
    return r'C:\temp\ak-diagnostics-test.jsonl';
  }
}

void main() {
  setUp(() {
    ConnectionDiagnosticsLog.shared.clear();
  });

  testWidgets('export carries the key diagnostic event lines',
      (tester) async {
    // Seed exactly the lines one loopback pairing + send records: the
    // bind facts (unknown-name events ride the tolerance contract), the
    // G2 probe_observed line, the pairing's connected fact and the send
    // op's sent_via address (the GUI's "connected via" analog).
    ConnectionDiagnosticsLog.shared.record(const TaskUnknownEvent(
      name: 'receiver_bind',
      fields: {
        'addr': '127.0.0.1:53112',
        'dual_stack': true,
        'fallback_reason': null,
        'reflexive': '127.0.0.1:53112',
        'stun_error': null,
      },
    ));
    ConnectionDiagnosticsLog.shared.record(const TaskUnknownEvent(
      name: 'sender_bind',
      fields: {
        'addr': '127.0.0.1:53110',
        'dual_stack': true,
        'fallback_reason': null,
        'reflexive': '127.0.0.1:53110',
        'stun_error': null,
      },
    ));
    ConnectionDiagnosticsLog.shared.record(const TaskUnknownEvent(
      name: 'probe_observed',
      fields: {
        'conn': 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        'observed': '127.0.0.1:53112',
        'kind': 'candidate',
        'phase': 'send',
        'reachable': 1,
        'packets_in': 2,
        'elapsed_ms': 31,
      },
    ));
    ConnectionDiagnosticsLog.shared.record(const TaskConnectedEvent(
      conn: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
      role: 'sender',
      peerEndpoint: '127.0.0.1:53112',
    ));
    ConnectionDiagnosticsLog.shared.recordRaw('sent_via', {
      'conn': 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
      'peer': '127.0.0.1:53112',
      'files': 1,
      'bytes': 42,
    });

    final gateway = _CapturingExportGateway();
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SettingsPage(
          service: LocalSimpleTransferService(),
          store: InMemorySaveDirStore(),
          exporter: gateway,
        ),
      ),
    ));

    await tester.tap(find.byKey(const ValueKey('settings_export_diag_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The snackbar surfaces the saved path (save-only flow: share_plus
    // is not a dependency — the path IS the share affordance).
    expect(find.textContaining('ak-diagnostics-test.jsonl'), findsOneWidget);
    expect(gateway.bodies, hasLength(1));
    final body = gateway.bodies.single;

    // The header line: export identity + platform summary + count.
    expect(body, contains('ak-sending-connection-diagnostics'));
    expect(body, contains('"platform"'));
    expect(body, contains('"os"'));
    expect(body, contains('"line_count":5'));

    // The key diagnostic event lines, one JSON object per line.
    expect(body, contains('"event":"receiver_bind"'));
    expect(body, contains('"event":"sender_bind"'));
    expect(body, contains('"event":"probe_observed"'));
    expect(body, contains('"kind":"candidate"'));
    expect(body, contains('"event":"connected"'));
    expect(body, contains('"peer_endpoint":"127.0.0.1:53112"'));
    expect(body, contains('"event":"sent_via"'));

    // Every line is valid JSON (JSON Lines contract of the export).
    for (final line in body.trimRight().split('\n')) {
      expect(jsonDecodeMap(line), isNotNull, reason: line);
    }
  });

  testWidgets('export works with an empty buffer (header only)',
      (tester) async {
    final gateway = _CapturingExportGateway();
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SettingsPage(
          service: LocalSimpleTransferService(),
          store: InMemorySaveDirStore(),
          exporter: gateway,
        ),
      ),
    ));

    await tester.tap(find.byKey(const ValueKey('settings_export_diag_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(gateway.bodies, hasLength(1));
    expect(gateway.bodies.single, contains('"line_count":0'));
    expect(find.textContaining('已导出'), findsOneWidget);
  });

  test('the ring buffer keeps only the most recent capacity lines', () {
    final log = ConnectionDiagnosticsLog(3);
    for (var i = 0; i < 5; i++) {
      log.recordRaw('sent_via', {'seq': i});
    }
    final lines = log.lines;
    expect(lines, hasLength(3));
    expect(lines.first, contains('"seq":2'));
    expect(lines.last, contains('"seq":4'));
  });

  test('non-diagnostic events are not recorded', () {
    ConnectionDiagnosticsLog.shared
        .record(const TaskUnknownEvent(name: 'runtime_ready', fields: {}));
    expect(ConnectionDiagnosticsLog.shared.lines, isEmpty);
  });
}

/// Decodes one JSON line as a map (the export's per-line contract).
Map<String, Object?>? jsonDecodeMap(String line) {
  final decoded = jsonDecode(line);
  return decoded is Map<String, Object?> ? decoded : null;
}

