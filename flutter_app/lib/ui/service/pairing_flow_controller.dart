/// State machine driving the wave-28 pairing flows (发起流 / 加入流) on
/// the home page, on top of the frozen [SimpleTransferService] contract.
///
/// ```text
/// create: creating → awaitingPeer → paired → probing → success | failed
/// join:   joining → paired → probing → success | failed
/// ```
///
/// The auto connectivity probe (spec 3) is tracked through the
/// `connections` stream: the latched connection's state maps
/// connecting→paired, testing→probing, connected→success.
library;

import 'dart:async' show StreamSubscription;

import 'package:flutter/foundation.dart' show ChangeNotifier;

import 'simple_transfer_service.dart';

/// Which of the two home actions is running.
enum PairingFlowKind { create, join }

/// One phase of a pairing flow (see the library diagram).
enum PairingFlowPhase {
  /// `createCode` future pending.
  creating,

  /// Code minted and shown; waiting for the peer to join.
  awaitingPeer,

  /// `joinCode` future pending.
  joining,

  /// Paired; the connection is being established (auto probe starting).
  paired,

  /// Connectivity probe running.
  probing,

  /// Probe passed — the connection is live ("连接成功").
  success,

  /// Failed with a readable reason ([PairingFlowController.error]).
  failed,
}

class PairingFlowController extends ChangeNotifier {
  PairingFlowController(this._service) {
    _subscription = _service.connections.listen(
      _onConnections,
      onError: (Object _) {},
    );
  }

  final SimpleTransferService _service;
  late final StreamSubscription<List<TaskConnection>> _subscription;

  /// Connection ids already accounted for: pre-existing rows (present on
  /// the stream's snapshot replay) never complete a flow.
  final Set<String> _knownConnectionIds = {};

  /// Most recent connections snapshot (lets [startJoin] latch a row whose
  /// event arrived before the `joinCode` future resolved).
  List<TaskConnection>? _lastSnapshot;

  PairingFlowKind? kind;
  PairingFlowPhase? phase;
  TaskCode? code;
  String? error;
  TaskConnection? connection;

  /// The connection id a join flow waits for (from [JoinResult]).
  String? _expectedConnectionId;

  bool get active => phase != null;

  /// Creates a code with MY direction [role] (发起流).
  Future<void> startCreate(TaskRole role) async {
    _begin(kind: PairingFlowKind.create, phase: PairingFlowPhase.creating);
    try {
      code = await _service.createCode(role);
      _set(PairingFlowPhase.awaitingPeer);
    } on Object catch (e) {
      _fail('生成配对码失败：$e');
    }
  }

  /// Joins a code typed by the user (加入流). Input is normalized here
  /// (uppercase, hyphen/space tolerance) before hitting the service.
  Future<void> startJoin(String input) async {
    final normalized = normalizeJoinCode(input);
    if (normalized.length != 8) {
      _begin(kind: PairingFlowKind.join, phase: PairingFlowPhase.failed);
      error = '配对码为 8 位字符（当前 ${normalized.length} 位），请检查后重试。';
      notifyListeners();
      return;
    }
    _begin(kind: PairingFlowKind.join, phase: PairingFlowPhase.joining);
    try {
      final result = await _service.joinCode(normalized);
      if (!result.ok) {
        _fail(result.error ?? '配对失败，请重试。');
        return;
      }
      _expectedConnectionId = result.connectionId;
      // The service typically adds the connection row BEFORE joinCode
      // resolves — that event may already have passed through the
      // listener (marked known, no latch). Recover the row from the
      // latest snapshot; if it truly hasn't arrived yet, the next
      // snapshot latches it.
      if (connection == null) {
        TaskConnection? expected;
        final snapshot = _lastSnapshot;
        if (snapshot != null && _expectedConnectionId != null) {
          for (final row in snapshot) {
            if (row.id == _expectedConnectionId) {
              expected = row;
              break;
            }
          }
        }
        if (expected != null) {
          connection = expected;
          _applyConnectionState(expected);
        } else {
          _set(PairingFlowPhase.paired);
        }
      }
    } on Object catch (e) {
      _fail('配对失败：$e');
    }
  }

  /// Abandons the flow and returns to the two home actions.
  void reset() {
    _begin(kind: null, phase: null);
  }

  @override
  void dispose() {
    _subscription.cancel();
    super.dispose();
  }

  void _begin({required PairingFlowKind? kind, PairingFlowPhase? phase}) {
    this.kind = kind;
    this.phase = phase;
    code = null;
    error = null;
    connection = null;
    _expectedConnectionId = null;
    notifyListeners();
  }

  void _set(PairingFlowPhase next) {
    phase = next;
    notifyListeners();
  }

  void _fail(String reason) {
    phase = PairingFlowPhase.failed;
    error = reason;
    notifyListeners();
  }

  void _onConnections(List<TaskConnection> snapshot) {
    _lastSnapshot = snapshot;
    final latched = connection;
    TaskConnection? candidate;
    final expectedId = _expectedConnectionId;
    for (final row in snapshot) {
      if (_knownConnectionIds.contains(row.id)) {
        continue;
      }
      final completesFlow =
          (kind == PairingFlowKind.create && phase == PairingFlowPhase.awaitingPeer) ||
              (expectedId != null && row.id == expectedId);
      if (completesFlow) {
        candidate = row;
        break;
      }
    }
    _knownConnectionIds.addAll(snapshot.map((row) => row.id));

    if (candidate != null) {
      connection = candidate;
      _expectedConnectionId = candidate.id;
      _applyConnectionState(candidate);
      return;
    }

    if (latched != null) {
      TaskConnection? current;
      for (final row in snapshot) {
        if (row.id == latched.id) {
          current = row;
          break;
        }
      }
      if (current == null) {
        // Dropped mid-flow (peer left / network change) — except after
        // success, where the panel is already done and the list card
        // simply disappears.
        if (phase != PairingFlowPhase.success) {
          _fail('连接已断开（对端离开或网络变化），请重新配对。');
        }
      } else if (current.state != latched.state) {
        connection = current;
        _applyConnectionState(current);
      }
    }
  }

  void _applyConnectionState(TaskConnection candidate) {
    switch (candidate.state) {
      case TaskConnectionState.connecting:
        _set(PairingFlowPhase.paired);
      case TaskConnectionState.testing:
        _set(PairingFlowPhase.probing);
      case TaskConnectionState.connected:
        _set(PairingFlowPhase.success);
      case TaskConnectionState.disconnected:
        _fail('连接已断开（对端离开或网络变化），请重新配对。');
    }
  }
}
