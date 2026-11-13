/// G2 (batchF 2026-11-13, CR-P3-06 diagnostics): the process-wide ring
/// buffer of recent CONNECTION-PATH diagnostic facts behind the settings
/// page's「导出连接诊断」entry.
///
/// What lands here: the diagnostic-class task-plane events —
/// `receiver_bind` / `sender_bind` (bind + STUN facts), `probe_observed`
/// (the send-time probe round's observation, emitted since the same G2
/// batch), `reflexive_opener` (the post-pairing opener's packet count,
/// whitelisted since CR-P3-12 — a zero with an adopted socket is the
/// run-43304434327 fingerprint of an over-strict target filter),
/// `connected` (the pairing fact incl. `peer_endpoint`), the
/// `probe_result` verdicts, `handshake_failed`, data-plane
/// `disconnected` reasons — plus the send op's `sent_via` line (the GUI
/// analog of the CLI's "connected via" fact: the address the engine
/// actually transferred over, from the task_send report's `peer` field).
///
/// RED LINE NOTE (C4): the "repository records carry category info
/// only, never real IPs" red line governs WHAT GOES INTO REPO DOCS AND
/// COMMITTED TEST LOGS. It deliberately does NOT apply to the user's
/// LOCAL export: the exported file IS the user's own diagnostic data
/// (their own bind addresses, their own peer's address), written only
/// onto their own device — that is the entire point of the feature.
/// Nothing in this file ever moves diagnostics off the device: no
/// network, no registry, no shared logs — only the temp file the export
/// gateway writes.
///
/// Parsing tolerance contract (ffi-v2 §8.3): unknown `"event"` names
/// decode to `TaskUnknownEvent` Dart-side; older engines without
/// `probe_observed` simply never produce it, newer fields never break
/// this buffer (extra fields ride along verbatim in the line).
library;

import 'dart:convert' show jsonEncode;
import 'dart:io' show File, Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        TaskConnectedEvent,
        TaskDisconnectedEvent,
        TaskEvent,
        TaskHandshakeFailedEvent,
        TaskProbeResultEvent,
        TaskUnknownEvent;
import 'package:path_provider/path_provider.dart' show getTemporaryDirectory;

/// Default ring-buffer capacity: the "most recent N diagnostic events"
/// the export collects (bind + probe + connect + transfer lines of a
/// full pairing easily fit; a chatty session rolls the oldest off).
const int connectionDiagnosticsCapacity = 200;

/// One recorded diagnostic line, rendered as a single JSON object
/// (`{"ts":...,"event":...,<fields>}`) at RECORD time — the exported
/// file is then guaranteed valid JSON Lines regardless of what the
/// engine put in the fields.
class ConnectionDiagnosticsLog {
  ConnectionDiagnosticsLog([this._capacity = connectionDiagnosticsCapacity]);

  /// The process-wide instance the engine service records into and the
  /// settings page exports from (one app, one buffer; tests `clear()`).
  static final ConnectionDiagnosticsLog shared =
      ConnectionDiagnosticsLog();

  final int _capacity;
  final List<String> _lines = [];

  /// Recorded lines, oldest first (an unmodifiable view).
  List<String> get lines => List.unmodifiable(_lines);

  /// Drop every recorded line (test isolation).
  void clear() => _lines.clear();

  /// Record one polled task event IF it is diagnostic-class. Unknown
  /// names ride the existing tolerance contract: only the four
  /// registered diagnostic unknowns (`receiver_bind`, `sender_bind`,
  /// `probe_observed`, `reflexive_opener`) land; every other unknown is
  /// dropped exactly like the router drops it.
  void record(TaskEvent event) {
    if (event is TaskConnectedEvent) {
      _push('connected', {
        'conn': event.conn,
        'role': event.role,
        if (event.peerEndpoint != null) 'peer_endpoint': event.peerEndpoint,
        if (event.codeId != null) 'code_id': event.codeId,
      });
      return;
    }
    if (event is TaskProbeResultEvent) {
      _push('probe_result', {
        'conn': event.conn,
        if (event.endpoint != null) 'endpoint': event.endpoint,
        'reachable': event.reachable,
      });
      return;
    }
    if (event is TaskHandshakeFailedEvent) {
      _push('handshake_failed', {'code_id': event.codeId, 'reason': event.reason});
      return;
    }
    if (event is TaskDisconnectedEvent) {
      _push('disconnected', {
        if (event.conn != null) 'conn': event.conn,
        'reason': event.reason,
      });
      return;
    }
    if (event is TaskUnknownEvent) {
      switch (event.name) {
        case 'receiver_bind':
        case 'sender_bind':
        case 'probe_observed':
        case 'reflexive_opener': // CR-P3-12: opener packet count (C4 clue)
          _push(event.name, event.fields);
          return;
        default:
          return; // not diagnostic-class — the router drops it too
      }
    }
    // runtime_ready / op_done / op_failed / incoming_request / done /
    // consent_expired / request_closed: op/lifecycle bookkeeping, not
    // connection-path diagnostics.
  }

  /// Record one synthesized diagnostic line (the `sent_via` fact from a
  /// task_send report — see the library doc).
  void recordRaw(String name, Map<String, Object?> fields) =>
      _push(name, fields);

  void _push(String name, Map<String, Object?> fields) {
    final line = jsonEncode({
      'ts': DateTime.now().toIso8601String(),
      'event': name,
      ...fields,
    });
    _lines.add(line);
    while (_lines.length > _capacity) {
      _lines.removeAt(0);
    }
  }
}

/// The host/platform block of the export header (the fixed vocabulary
/// `dart:io` exposes; `localeName` is best-effort — it throws on some
/// embedded hosts and the export must not die for it).
Map<String, Object?> diagnosticsPlatformSummary() => {
      'os': Platform.operatingSystem,
      'os_version': Platform.operatingSystemVersion,
      'cpus': Platform.numberOfProcessors,
      'locale': _localeOrNull(),
    };

String? _localeOrNull() {
  try {
    return Platform.localeName;
  } on Object {
    return null;
  }
}

/// Render the export body: ONE header line (export identity, timestamp,
/// platform summary, line count) followed by the recorded lines — pure
/// JSON Lines, greppable one fact per line.
String buildDiagnosticsExport(
  Iterable<String> lines, {
  DateTime? now,
  Map<String, Object?> Function()? platform,
}) {
  final stamp = (now ?? DateTime.now()).toIso8601String();
  final host = (platform ?? diagnosticsPlatformSummary)();
  final body = lines.toList(growable: false);
  final header = jsonEncode({
    'diag': 'ak-sending-connection-diagnostics',
    'exported_at': stamp,
    'platform': host,
    'line_count': body.length,
  });
  return '${[header, ...body].join('\n')}\n';
}

/// Where the export lands. The real implementation writes the platform
/// TEMP directory via `path_provider` (the only storage dependency in
/// pubspec); `share_plus` is NOT a dependency, so the flow is
/// save-then-show-the-path (the UI surfaces the absolute path in a
/// snackbar). If share_plus lands later, share the SAME file from the
/// same path — the gateway contract does not change.
abstract interface class DiagnosticsExportGateway {
  /// Persists [content]; resolves with the absolute file path (shown to
  /// the user verbatim), rejects with a readable reason on failure.
  Future<String> exportDiagnostics(String content);
}

/// The real gateway: `<temp>/ak-diagnostics-<epoch-ms>.jsonl`.
final class TempFileDiagnosticsExportGateway
    implements DiagnosticsExportGateway {
  const TempFileDiagnosticsExportGateway();

  @override
  Future<String> exportDiagnostics(String content) async {
    final dir = await getTemporaryDirectory();
    final name =
        'ak-diagnostics-${DateTime.now().millisecondsSinceEpoch}.jsonl';
    final file = File('${dir.path}${Platform.pathSeparator}$name');
    await file.writeAsString(content, flush: true);
    return file.path;
  }
}
