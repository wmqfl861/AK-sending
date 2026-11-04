import 'dart:async' show StreamController;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show InviteCreated, InviteState, InviteSummary, PairingEvent;

import 'package:ak_sending_app/services/invite_service.dart'
    show InviteService, InviteTtlKind, SchedulerPreference;

/// Deterministic in-memory [InviteService] for the invite-page widget
/// tests: an invite registry the tests can seed and mutate, error
/// injection per operation, a broadcast event stream driven by [emit],
/// and an in-memory scheduler-preference store. No FFI, no timers.
final class FakeInviteService implements InviteService {
  FakeInviteService({
    List<InviteSummary>? invites,
    this.listError,
    this.createError,
    this.redeemError,
    this.revokeError,
    SchedulerPreference? preference,
  })  : _invites = List.of(invites ?? const <InviteSummary>[]),
        _savedPreference = preference;

  /// Registry rows; [createInvite]/[revokeInvite] mutate this list.
  final List<InviteSummary> _invites;

  /// When non-null, [listInvites] throws it (engine down / old DLL form).
  Object? listError;

  /// When non-null, [createInvire] throws it.
  Object? createError;

  /// When non-null, [redeemInvite] throws it.
  Object? redeemError;

  /// When non-null, [revokeInvite] throws it.
  Object? revokeError;

  /// When non-null, [saveSchedulerPreference] throws it.
  Object? savePreferenceError;

  /// The persisted preference (null = never saved → defaults).
  SchedulerPreference? _savedPreference;

  /// Call counters.
  int listInvitesCalls = 0;
  int createInviteCalls = 0;
  int redeemInviteCalls = 0;
  int revokeInviteCalls = 0;
  int savePreferenceCalls = 0;

  /// Arguments captured by the latest calls.
  bool? lastCreateShortTerm;
  String? lastRedeemCode;
  String? lastRevokeCodeId;
  SchedulerPreference? lastSavedPreference;

  /// `includeTerminal` captured by the latest [listInvites] call.
  bool? lastListIncludeTerminal;

  /// Fixed clock for created/expires stamps (deterministic assertions).
  int clockMs = 1730000000000;

  int _codeCounter = 0;

  final StreamController<PairingEvent> _events =
      StreamController<PairingEvent>.broadcast();

  /// The codes handed out by [createInvite], in order (plaintext shown
  /// once — tests correlate rows to codes through this).
  final List<String> issuedCodes = [];

  /// Live registry fixture (mutations visible to the next [listInvites]).
  List<InviteSummary> get invites => _invites;

  @override
  Stream<PairingEvent> get events => _events.stream;

  /// Feeds one pairing event into the stream (the "engine pump" double).
  void emit(PairingEvent event) => _events.add(event);

  @override
  Future<List<InviteSummary>> listInvites({bool includeTerminal = false}) async {
    listInvitesCalls += 1;
    lastListIncludeTerminal = includeTerminal;
    final error = listError;
    if (error != null) {
      throw error;
    }
    return List.of(_invites);
  }

  @override
  Future<InviteCreated> createInvite(
      {InviteTtlKind ttl = InviteTtlKind.longLived}) async {
    createInviteCalls += 1;
    lastCreateShortTerm = ttl == InviteTtlKind.shortLived;
    final error = createError;
    if (error != null) {
      throw error;
    }
    final sequence = _codeCounter;
    _codeCounter += 1;
    final codeId = (sequence + 1).toRadixString(16).padLeft(32, '0');
    // A valid deterministic Crockford payload (no I/L/O/U): a repeating
    // 0123 pattern keeps every generated code well-formed.
    final payload = '0123456789abcdefghjkmnpqrstvwxyz0123'
        .substring(0, 32 - 0)
        .substring(sequence % 4);
    final padded = payload.padRight(32, '0');
    final groups = <String>[
      for (var g = 0; g < 8; g++) padded.substring(g * 4, g * 4 + 4),
    ];
    final invitationCode = 'AKI1-${groups.join('-')}';
    issuedCodes.add(invitationCode);
    final created = InviteCreated(
      summary: InviteSummary(
        codeId: codeId,
        state: InviteState.active,
        createdMs: clockMs,
        expiresMs:
            ttl == InviteTtlKind.shortLived ? clockMs + 3600 * 1000 : 0,
        revision: 1,
      ),
      invitationCode: invitationCode,
    );
    _invites.insert(0, created.summary);
    return created;
  }

  @override
  Future<void> redeemInvite(String inviteCode) async {
    redeemInviteCalls += 1;
    lastRedeemCode = inviteCode;
    final error = redeemError;
    if (error != null) {
      throw error;
    }
  }

  @override
  Future<void> revokeInvite(String codeId) async {
    revokeInviteCalls += 1;
    lastRevokeCodeId = codeId;
    final error = revokeError;
    if (error != null) {
      throw error;
    }
    final index = _invites.indexWhere((row) => row.codeId == codeId);
    if (index < 0) {
      throw StateError('邀请码不存在（可能已在其他窗口撤销）');
    }
    final row = _invites[index];
    _invites[index] = InviteSummary(
      codeId: row.codeId,
      state: InviteState.revoked,
      createdMs: row.createdMs,
      expiresMs: row.expiresMs,
      revision: row.revision + 1,
    );
  }

  @override
  Future<SchedulerPreference> loadSchedulerPreference() async =>
      _savedPreference ?? SchedulerPreference.defaults;

  @override
  Future<void> saveSchedulerPreference(SchedulerPreference preference) async {
    savePreferenceCalls += 1;
    lastSavedPreference = preference;
    final error = savePreferenceError;
    if (error != null) {
      throw error;
    }
    _savedPreference = preference;
  }

  @override
  void dispose() {
    _events.close();
  }
}
