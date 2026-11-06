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

  /// The joiner's EXPLICIT direction of the running join flow (wave-29
  /// P1-05): the user's intent picked alongside the code — forwarded to
  /// `joinCode` so the engine never guesses the direction.
  TaskRole? joinRole;

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
  /// (uppercase, hyphen/space tolerance) and validated against the REAL
  /// task-code format (wave-29 P1-02: `AKI1` + 32 Crockford Base32
  /// symbols) before hitting the service. [role] is the joiner's declared
  /// direction; the submit affordance requires one.
  Future<void> startJoin(String input, {TaskRole? role}) async {
    joinRole = role;
    final normalized = normalizeJoinCode(input);
    if (!isValidTaskCode(normalized)) {
      _begin(kind: PairingFlowKind.join, phase: PairingFlowPhase.failed);
      error = normalized.length != taskCodePrefix.length + taskCodeSymbolCount
          ? '配对码为 AKI1 开头的 36 位字符（当前 ${normalized.length} 位），'
              '请输入完整的 AKI1 码后重试。'
          : '配对码格式不正确：AKI1 后应为 32 位 Crockford Base32 字符'
              '（不含 I/L/O/U），请检查后重试。';
      notifyListeners();
      return;
    }
    _begin(kind: PairingFlowKind.join, phase: PairingFlowPhase.joining);
    try {
      final result = await _service.joinCode(normalized, role: role);
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

  /// Wave-30 P2-04: the 取消等待 of a shown code — cancels the flow AND
  /// revokes the unconsumed code engine-side (`disconnect(codeId)`: the
  /// registry row is revoked, the code can never be redeemed). Flows
  /// without a cancel key (already joined, a code without codeId, or the
  /// join flow) just reset. A FAILED revoke is reported honestly — the
  /// flow lands in `failed` with the readable reason instead of silently
  /// leaving a live code behind.
  Future<void> cancelWaiting() async {
    final codeId = code?.codeId;
    final cancellable = kind == PairingFlowKind.create &&
        phase == PairingFlowPhase.awaitingPeer &&
        connection == null && // the peer never landed: the code is unconsumed
        codeId != null &&
        codeId.isNotEmpty;
    if (!cancellable) {
      reset();
      return;
    }
    try {
      await _service.disconnect(codeId);
      reset();
    } on Object catch (e) {
      _fail('取消配对失败：$e（配对码可能仍然有效，请重试取消）');
    }
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
    if (kind != PairingFlowKind.join) {
      joinRole = null; // a fresh join re-picks the direction
    }
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
      case TaskConnectionState.unreachable:
        // Wave-29 P2-09: the probe FAILED — an honest negative verdict
        // ends the flow with a readable reason (never "success").
        _fail('连通测试未通过：无法与对端建立数据直连，请检查网络后重新配对。');
      case TaskConnectionState.disconnected:
        _fail('连接已断开（对端离开或网络变化），请重新配对。');
    }
  }
}
