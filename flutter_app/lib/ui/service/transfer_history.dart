/// Client UI U1 (2026-11-13): the session's 「最近传输」 record.
///
/// In the v1 model one pairing carries ONE transfer: when it finishes the
/// connection row disappears from 「当前连接」. Without a record the user
/// would have nothing to look back at, so the shell keeps a small
/// in-memory list of finished transfers (both directions) for the running
/// session. Deliberately NOT persisted: it holds peer labels, counts and
/// sizes only — never file names or paths — and dies with the app.
///
/// [TransferHistoryScope] sits ABOVE the MaterialApp (see `app.dart`), so
/// pushed routes (the connection detail page) see the same instance as
/// the shell's tabs. Widgets built without a scope (direct widget tests)
/// simply record nothing and show no history section.
library;

import 'package:flutter/widgets.dart';

import 'simple_transfer_service.dart' show TaskRole;

/// One finished (or failed) transfer of this session.
class TransferRecord {
  const TransferRecord({
    required this.role,
    required this.peerName,
    required this.succeeded,
    required this.finishedAt,
    this.fileCount,
    this.totalBytes,
    this.elapsed,
    this.pathLabel,
    this.detail,
  });

  /// MY direction: [TaskRole.send] = I sent, [TaskRole.receive] = I
  /// received.
  final TaskRole role;

  /// The peer's display label (the connection's `peerName`).
  final String peerName;

  final bool succeeded;

  final DateTime finishedAt;

  final int? fileCount;

  final int? totalBytes;

  /// Wall-clock duration from the start of the send / the 接受 click to
  /// the end; null when it was not measured.
  final Duration? elapsed;

  /// The direct-path label (`局域网直连` / `互联网直连` / `IPv6 直连`),
  /// when the engine reported the endpoint the transfer used.
  final String? pathLabel;

  /// The readable failure reason (failed transfers only).
  final String? detail;
}

/// The session's transfer list, newest first, bounded to [capacity].
class TransferHistory extends ChangeNotifier {
  TransferHistory({this.capacity = 50});

  /// Oldest entries fall off past this many records.
  final int capacity;

  final List<TransferRecord> _records = <TransferRecord>[];

  /// Newest first (an unmodifiable view).
  List<TransferRecord> get records => List.unmodifiable(_records);

  bool get isEmpty => _records.isEmpty;

  void add(TransferRecord record) {
    _records.insert(0, record);
    if (_records.length > capacity) {
      _records.removeRange(capacity, _records.length);
    }
    notifyListeners();
  }

  void clear() {
    if (_records.isEmpty) {
      return;
    }
    _records.clear();
    notifyListeners();
  }
}

/// Provides one [TransferHistory] to the subtree; dependents rebuild when
/// it changes.
class TransferHistoryScope extends InheritedNotifier<TransferHistory> {
  const TransferHistoryScope({
    super.key,
    required TransferHistory history,
    required super.child,
  }) : super(notifier: history);

  /// The history for BUILD methods (registers a rebuild dependency);
  /// null when no scope is above [context].
  static TransferHistory? watch(BuildContext context) => context
      .dependOnInheritedWidgetOfExactType<TransferHistoryScope>()
      ?.notifier;

  /// The history for CALLBACKS (no dependency — safe outside build);
  /// null when no scope is above [context]. Read it BEFORE any `await`.
  static TransferHistory? read(BuildContext context) => context
      .getInheritedWidgetOfExactType<TransferHistoryScope>()
      ?.notifier;
}
