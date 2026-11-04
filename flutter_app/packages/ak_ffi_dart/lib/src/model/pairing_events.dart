/// Typed model for the pairing events polled via `ak_pairing_poll_event`
/// (wave-22). The pairing event queue is SEPARATE from the transfer
/// progress queue (`ak_poll_progress`): pairing lines carry no `"job"`
/// field and must never be fed to the progress parser (ffi-v2.md §5).
///
/// Wire names of the wave-22 Rust side (crates/ak-ffi/src/pairing.rs,
/// `push_event`/`sched_event_json` sites), mapped to the semantic event
/// classes the UI timeline renders:
///
/// ```json
/// {"event":"runtime_ready","runtime":"<32hex>"}          → ServiceOnlineEvent
/// {"event":"runtime_failed","runtime":"<32hex>","error":"..."} → ServiceOfflineEvent
/// {"event":"op_done","op_id":"<32hex>","op":"create"}    → OpDoneEvent
/// {"event":"op_failed","op_id":"<32hex>","op":"redeem"}  → OpFailedEvent
/// {"event":"redeem_offered","code_id":"<32hex>","pairing_id":"<32hex>",
///  "peer":"<64hex>"}                                     → RedeemPendingEvent
/// ```
///
/// The remaining timeline events (`peer_prepared`, `pairing_complete`,
/// `redeem_failed`, `invitation_revoked`) have no pairing-queue wire name
/// this wave: the service layer SYNTHESIZES them locally from op results
/// (a finished redeem op carries `pairing_id` + `peer`; a finished revoke
/// op carries `code_id`) and the event-driven fakes drive them directly —
/// same vocabulary on every layer.
///
/// Parsing policy mirrors `progress_events.dart`: an unknown `"event"`
/// name decodes to [PairingUnknownEvent] (forward compatibility with newer
/// engines); a missing or wrongly typed payload field throws
/// [FormatException] because it means the contract is broken, not merely
/// newer.
library;

/// Base of the seven typed pairing events plus the unknown fallback.
///
/// Instances compare by runtime type and payload fields.
sealed class PairingEvent {
  const PairingEvent({this.atMs});

  /// Optional engine timestamp of the event, Unix epoch milliseconds
  /// (`at_ms` when present; null when the engine omitted it).
  final int? atMs;

  /// The `"event"` name as it appears on the wire.
  String get name;

  /// Payload fields used for equality/toString.
  Map<String, Object?> get fields => const {};

  /// Decodes one `ak_pairing_poll_event` line (wire names above).
  factory PairingEvent.fromJson(Map<String, Object?> json) {
    final event = _string(json, 'event');
    final at = json['at_ms'] is int ? json['at_ms'] as int : null;
    switch (event) {
      case 'runtime_ready':
        return ServiceOnlineEvent(atMs: at);
      case 'service_online':
        return ServiceOnlineEvent(atMs: at);
      case 'runtime_failed':
        return ServiceOfflineEvent(
          atMs: at,
          reason: _optionalString(json, 'error') ??
              _optionalString(json, 'reason'),
        );
      case 'service_offline':
        return ServiceOfflineEvent(
          atMs: at,
          reason: _optionalString(json, 'reason'),
        );
      case 'op_done':
        return OpDoneEvent(
          atMs: at,
          opId: _string(json, 'op_id'),
          op: _string(json, 'op'),
        );
      case 'op_failed':
        return OpFailedEvent(
          atMs: at,
          opId: _string(json, 'op_id'),
          op: _string(json, 'op'),
        );
      case 'redeem_offered':
      case 'redeem_pending':
        return RedeemPendingEvent(
          atMs: at,
          codeId: _string(json, 'code_id'),
          peer: _string(json, 'peer'),
          pairingId: _optionalString(json, 'pairing_id'),
        );
      case 'peer_prepared':
        return PeerPreparedEvent(
          atMs: at,
          pairingId: _string(json, 'pairing_id'),
        );
      case 'pairing_complete':
        return PairingCompleteEvent(
          atMs: at,
          pairingId: _string(json, 'pairing_id'),
          peer: _string(json, 'peer'),
        );
      case 'redeem_failed':
        return RedeemFailedEvent(
          atMs: at,
          codeId: _optionalString(json, 'code_id'),
          error: _string(json, 'error'),
        );
      case 'invitation_revoked':
        return InvitationRevokedEvent(
          atMs: at,
          codeId: _string(json, 'code_id'),
          synced: json['synced'] is bool ? json['synced'] as bool : null,
        );
      default:
        return PairingUnknownEvent(atMs: at, name: event, fields: Map.of(json)
          ..remove('event')
          ..remove('at_ms'));
    }
  }

  @override
  String toString() {
    final parts = <String>[
      name,
      if (atMs != null) 'at=$atMs',
      for (final entry in fields.entries) '${entry.key}=${entry.value}',
    ];
    return '$runtimeType(${parts.join(' ')})';
  }

  @override
  bool operator ==(Object other) =>
      other.runtimeType == runtimeType &&
      other is PairingEvent &&
      other.name == name &&
      other.atMs == atMs &&
      _mapEquals(other.fields, fields);

  @override
  int get hashCode => Object.hash(runtimeType, name, atMs,
      Object.hashAllUnordered(fields.keys), Object.hashAll(fields.values));

  static bool _mapEquals(Map<String, Object?> a, Map<String, Object?> b) {
    if (a.length != b.length) {
      return false;
    }
    for (final entry in a.entries) {
      if (!b.containsKey(entry.key) || b[entry.key] != entry.value) {
        return false;
      }
    }
    return true;
  }
}

/// The control connection came online (`runtime_ready` on the wire: start
/// returning "accepted" is NOT online — this event is, ffi-v2.md §2).
final class ServiceOnlineEvent extends PairingEvent {
  const ServiceOnlineEvent({super.atMs});

  @override
  String get name => 'service_online';
}

/// The control connection dropped (`runtime_failed` on the wire);
/// [reason] carries the engine diagnosis.
final class ServiceOfflineEvent extends PairingEvent {
  const ServiceOfflineEvent({super.atMs, this.reason});

  final String? reason;

  @override
  String get name => 'service_offline';

  @override
  Map<String, Object?> get fields =>
      {if (reason != null) 'reason': reason};
}

/// One asynchronous pairing op finished successfully (`op_done`).
final class OpDoneEvent extends PairingEvent {
  const OpDoneEvent({super.atMs, required this.opId, required this.op});

  /// Hex op id (32 chars) — correlate with the `PairingOpHandle.opIdHex`.
  final String opId;

  /// Which op finished: `create` / `redeem` / `list` / `revoke`.
  final String op;

  @override
  String get name => 'op_done';

  @override
  Map<String, Object?> get fields => {'op_id': opId, 'op': op};
}

/// One asynchronous pairing op failed (`op_failed`); the op-result body
/// carries the engine error string.
final class OpFailedEvent extends PairingEvent {
  const OpFailedEvent({super.atMs, required this.opId, required this.op});

  final String opId;

  final String op;

  @override
  String get name => 'op_failed';

  @override
  Map<String, Object?> get fields => {'op_id': opId, 'op': op};
}

/// A redeemer started redeeming one of this device's codes
/// (InviteRedeemOffered delivered, owner verification pending).
final class RedeemPendingEvent extends PairingEvent {
  const RedeemPendingEvent({
    super.atMs,
    required this.codeId,
    required this.peer,
    this.pairingId,
  });

  /// The code being redeemed, 32 hex characters.
  final String codeId;

  /// The redeemer's fingerprint, 64 hex characters.
  final String peer;

  /// The pairing transaction id when the registry already assigned one.
  final String? pairingId;

  @override
  String get name => 'redeem_pending';

  @override
  Map<String, Object?> get fields => {
        'code_id': codeId,
        'peer': peer,
        if (pairingId != null) 'pairing_id': pairingId,
      };
}

/// One side persisted the pending pairing journal (PairPrepared seen).
final class PeerPreparedEvent extends PairingEvent {
  const PeerPreparedEvent({super.atMs, required this.pairingId});

  final String pairingId;

  @override
  String get name => 'peer_prepared';

  @override
  Map<String, Object?> get fields => {'pairing_id': pairingId};
}

/// The pairing transaction reached Ready on both sides; the peer is now in
/// the local pairing book.
final class PairingCompleteEvent extends PairingEvent {
  const PairingCompleteEvent({
    super.atMs,
    required this.pairingId,
    required this.peer,
  });

  final String pairingId;

  /// The newly paired peer's fingerprint, 64 hex characters.
  final String peer;

  @override
  String get name => 'pairing_complete';

  @override
  Map<String, Object?> get fields => {'pairing_id': pairingId, 'peer': peer};
}

/// A redeem attempt failed (unified `PAIR_UNAVAILABLE`-style reason).
final class RedeemFailedEvent extends PairingEvent {
  const RedeemFailedEvent({
    super.atMs,
    this.codeId,
    required this.error,
  });

  /// The code the failed attempt referenced, when known.
  final String? codeId;

  /// Engine reason string (e.g. `PAIR_UNAVAILABLE`, `RATE_LIMITED`).
  final String error;

  @override
  String get name => 'redeem_failed';

  @override
  Map<String, Object?> get fields => {
        if (codeId != null) 'code_id': codeId,
        'error': error,
      };
}

/// One of this device's codes was revoked (locally or by another session);
/// [synced] is false while the revoke is still pending server sync.
final class InvitationRevokedEvent extends PairingEvent {
  const InvitationRevokedEvent({
    super.atMs,
    required this.codeId,
    this.synced,
  });

  final String codeId;

  final bool? synced;

  @override
  String get name => 'invitation_revoked';

  @override
  Map<String, Object?> get fields => {
        'code_id': codeId,
        if (synced != null) 'synced': synced,
      };
}

/// Forward-compatibility fallback: an engine newer than this binding
/// emitted an event name it does not know.
final class PairingUnknownEvent extends PairingEvent {
  const PairingUnknownEvent(
      {super.atMs, required this.name, required this.fields});

  @override
  final String name;

  @override
  final Map<String, Object?> fields;
}

String _string(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! String || value.isEmpty) {
    throw FormatException('配对事件 "$key" 字段缺失或类型错误');
  }
  return value;
}

String? _optionalString(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value == null) {
    return null;
  }
  if (value is! String || value.isEmpty) {
    throw FormatException('配对事件 "$key" 字段类型错误');
  }
  return value;
}
