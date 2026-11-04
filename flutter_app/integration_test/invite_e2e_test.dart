/// Real-app E2E (wave 22): the 配对邀请码 (pairing invite-code) page journey
/// against the REAL Windows desktop app process (real window, real DLL-backed
/// `FfiTransferService` for the identity/pairing-book side), with the invite
/// domain driven by an injected **FakeInviteService** (timeline-driven) that
/// implements the REAL `InviteService` contract
/// (lib/services/invite_service.dart).
///
/// The invite page (`lib/ui/pages/invite_page.dart`) is being written by the
/// parallel page agent this wave. Per the repo's parallel rule #4
/// (AGENTS.md: 后棒可按冻结契约先行开发（Fake/占位），前棒落地后由集成者一行
/// 切换接线), this test ships the full journey NOW against a contract
/// stand-in page + the Fake service, and exposes ONE construction site for
/// the integrator to flip once the real page lands (search "INTEGRATION
/// SWITCH" below). The journey's key-based assertions are the frozen UI
/// contract and stay unchanged after the switch.
///
/// ═══════════════════════════════════════════════════════════════════════
///  UI KEY CONTRACT — for the invite-page agent (mirror these exactly)
/// ═══════════════════════════════════════════════════════════════════════
///  NavigationRail entry:
///   `invite_nav`              key on the destination's LABEL Text widget
///                             (NavigationRailDestination itself takes no
///                             key in this Flutter SDK; label 邀请码)
///  Page root / sections:
///   `invite_page`             page root scroll view key
///   `invite_list`             own-code list container
///   `invite_empty`            empty state Text (list has no codes yet)
///   `invite_events`           live pairing-event feed container
///   `invite_event_<n>`        feed line n (n = 0-based sequence)
///  Generate journey (create dialog: TTL choice — the frozen service
///  contract only offers InviteTtlKind.longLived / shortLived(1h)):
///   `invite_create_btn`       生成邀请码 button (opens the TTL dialog)
///   `invite_ttl_long`         long-lived (长期有效) radio in the dialog
///   `invite_ttl_short`        short-term 1h (短期 1 小时) radio
///   `invite_create_confirm`   confirm button in the create dialog
///   `invite_code_once_text`   the ONE-TIME plaintext code display
///                             (InviteCreated.invitationCode is a bearer
///                             credential shown exactly once)
///   `invite_code_once_close`  closes the one-time-code dialog
///   `invite_code_hint_<id>`   per-row code HINT (list rows are metadata
///                             only — `pair list --invites` rule; the hint
///                             is captured from the create result)
///   `invite_state_<id>`       per-row state text: 有效 / 已撤销 / 已过期
///   `invite_time_<id>`        per-row time line: contains 长期有效 for
///                             `expires_ms == 0`, else `<n> 小时有效` /
///                             `<n> 分钟有效` derived from created/expires
///   `invite_revoke_<id>`      per-row revoke button
///   `invite_revoke_confirm`   confirm button in the revoke dialog
///  Redeem journey:
///   `invite_code_input`       invite-code input field (AKI1 Crockford)
///   `invite_redeem_btn`       兑换 button
///   `invite_redeem_error`     inline error text for a rejected redeem
///                             (malformed code = LOCAL reject, §11)
///  Scheduler preference toggles (defaults serial × parallel):
///   `same_code_toggle`        Switch/SwitchListTile, value = parallel
///   `cross_code_toggle`       Switch/SwitchListTile, value = parallel
///  Reused existing keys: `home_pairing_code`, `home_peers_count`.
///  Feed line texts (textContaining): 配对服务已连接 / 配对事务已持久化 /
///  配对完成 (contains the 8-hex peer fingerprint prefix) / 邀请码已撤销.
///
///  NOTE (wave-22 divergence, for the integrator): the parallel page
///  agent's lib/ui/pages/invite_page.dart landed with its own key names —
///  invite_create_button / invite_redeem_input / invite_redeem_button /
///  invite_sched_same_code / invite_sched_cross_code / `invite_meta_<id>` /
///  invite_new_code / invite_ttl_selector (`invite_state_<id>` /
///  `invite_revoke_<id>` / invite_revoke_confirm / invite_events /
///  `invite_event_<n>` DO match). This journey freezes the task-brief names
///  above; at integration either rename the page keys to the contract or
///  adapt the journey finders — one mechanical sed either way.
///
///  Behavioral freezes for the page agent:
///   * the code list is `listInvites(includeTerminal: true)` — revoked/
///     expired rows STAY listed as tombstones (pairing-flow.md §8);
///   * after create, the list reloads and the plaintext code is never
///     re-shown (only the captured hint);
///   * redeem failures with FormatException render `invite_redeem_error`
///     inline and emit NOTHING to the feed (local reject, no leak §11);
///   * toggles persist via saveSchedulerPreference (scheduler_prefs.json
///     under the identity dir — the file IS the persistence proof).
///
/// ═══════════════════════════════════════════════════════════════════════
///  FakeInviteService (timeline-driven, implements the REAL contract):
///   * first invite op lazily "starts the runtime" and emits
///     ServiceOnlineEvent (accepted ≠ online, ffi-v2 §2);
///   * createInvite → InviteCreated with a deterministic Crockford AKI1
///     code that passes the REAL normalizeInviteCode validator;
///   * redeemInvite validates with the REAL normalizeInviteCode
///     (FormatException = local reject; `redeemRegistryCalls` records
///     which codes actually hit the registry round-trip);
///   * redeeming the pre-seeded `foreignCode` runs the pairing timeline
///     PeerPreparedEvent(+250 ms) → PairingCompleteEvent(+500 ms, peer =
///     foreignFingerprint); any other well-formed code fails later with
///     RedeemFailedEvent(PAIR_UNAVAILABLE) — no existence leak §11;
///   * scheduler preferences persist to scheduler_prefs.json exactly like
///     FfiInviteService does (same JSON shape);
///   * pairingComplete → pairing book linkage is wired by
///     [_InvitePairingLink] (fake mode only; the real service folds it
///     into peers.book through the engine itself).
///
/// ═══════════════════════════════════════════════════════════════════════
///  Runner contract (Git Bash, working directory flutter_app) — same shape
///  as app_e2e_test.dart:
///
///  ```bash
///  export PATH="$USERPROFILE/flutter-sdk/bin:$PATH"
///  export AK_ID_DIR="$(cygpath -w "$(mktemp -d "${TMPDIR:-/tmp}/ak-e2e-id-XXXXXX")")"
///  export AK_FFI_DLL="$(cd .. && pwd -W)/target/release/ak_ffi.dll"
///  flutter test integration_test/invite_e2e_test.dart -d windows
///  ```
///
///  AK_ID_DIR is REQUIRED (marker-guarded throwaway dir; wiped by the test —
///  the real %USERPROFILE%\.ak-sending is never touched). AK_FFI_DLL is
///  belt-and-braces; the dev-relative `../target/release/ak_ffi.dll` probe
///  also works from the flutter_app CWD. Real TLS/registry round-trips are
///  the NEXT acceptance wave; this wave is Fake-driven by design.
/// ═══════════════════════════════════════════════════════════════════════
library;

import 'dart:async'
    show Future, Stream, StreamController, StreamSubscription, unawaited;
import 'dart:convert' show jsonDecode, jsonEncode;
import 'dart:io' show Directory, File, Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        InvitationRevokedEvent,
        InviteCreated,
        InviteState,
        InviteSummary,
        PairingCompleteEvent,
        PairingEvent,
        PeerPreparedEvent,
        ServiceOnlineEvent,
        SchedCodeMode,
        normalizeInviteCode;

import 'package:ak_sending_app/services/ffi_transfer_service.dart'
    show FfiTransferService;
import 'package:ak_sending_app/services/invite_service.dart'
    show InviteService, InviteTtlKind, SchedulerPreference;
import 'package:ak_sending_app/ui/pages/home_page.dart' show HomePage;
import 'package:ak_sending_app/ui/service/transfer_service.dart'
    show TransferService;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// Path-safety marker required inside `$AK_ID_DIR` before the test wipes it
/// (app_e2e_test.dart convention).
const String _idDirMarker = 'ak-e2e-id';

late final Directory _receiverIdDir;
late final FfiTransferService _transfer;

// ═════════════════════════════════════════════════════════════════════════
// INTEGRATION SWITCH — the ONE construction site to flip after the real
// page/service land (see also _JourneyShell._buildInvitePage):
//
//   Wave 22 (now):     _fake = FakeInviteService(identityDir: ...);
//                     _inviteService = _fake!;
//   After integration: _fake = null;
//                     _inviteService = FfiInviteService.fromFacade(...);
//
// The journey assertions marked "[fake-mode]" read `_fake` and no-op when
// it is null; every key-based assertion runs against BOTH drivers.
// ═════════════════════════════════════════════════════════════════════════
late final FakeInviteService? _fake;
late final InviteService _inviteService;
late final _InvitePairingLink? _pairingLink;

// -----------------------------------------------------------------------------
// Deterministic Crockford Base32 encoder (codes the fake hands out).
// -----------------------------------------------------------------------------

/// The 32-symbol Crockford Base32 alphabet (no I, L, O, U — contract §1).
const String _crockfordAlphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/// Extracts the i-th 5-bit group of a big-endian 20-byte secret
/// (160 % 5 == 0 → exactly 32 symbols, no padding).
int _bits5(List<int> bytes, int i) {
  final start = i * 5;
  final byteIndex = start >> 3;
  final bitOffset = start & 7;
  if (bitOffset <= 3) {
    return (bytes[byteIndex] >> (3 - bitOffset)) & 0x1F;
  }
  return ((bytes[byteIndex] << 8 | bytes[byteIndex + 1]) >> (11 - bitOffset)) &
      0x1F;
}

/// Canonical `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` form of a
/// deterministic 20-byte seed (the REAL engine generates the secret from
/// OsRng; the fake needs reproducibility instead). Its output is validated
/// against the REAL [normalizeInviteCode] in the fake's constructor.
String _fakeCodeFromSeed(List<int> seed20) {
  assert(seed20.length == 20);
  final symbols = [
    for (var i = 0; i < 32; i++) _crockfordAlphabet[_bits5(seed20, i)],
  ];
  final groups = [
    for (var i = 0; i < symbols.length; i += 4)
      symbols.sublist(i, i + 4).join(),
  ];
  return 'AKI1-${groups.join('-')}';
}

/// Masked hint of a full code: first two groups + ellipsis (list rows never
/// re-show the plaintext secret — `pair list --invites` rule).
String inviteCodeHint(String codeText) => '${codeText.substring(0, 14)}…';

// -----------------------------------------------------------------------------
// FakeInviteService: timeline-driven fake of the REAL InviteService.
// -----------------------------------------------------------------------------

/// Fake [InviteService] (the REAL contract of lib/services/invite_service.dart)
/// driving the UI journey with deterministic latencies: each registry
/// round-trip resolves after a short real delay and emits its pairing events
/// onto [InviteService.events] in sequence.
///
/// Pre-seeded "other device": [foreignCode] is a well-formed code that
/// redeems successfully; its [foreignFingerprint] models the inviting owner
/// (redeeming A's code pairs this device WITH A).
class FakeInviteService implements InviteService {
  FakeInviteService({required this.identityDir}) {
    // The fake's encoder must produce codes the REAL validator accepts —
    // a silent drift here would fake-pass the redeem journey.
    if (normalizeInviteCode(foreignCode) != foreignCode) {
      throw StateError('fake encoder produced a non-canonical code');
    }
  }

  /// Directory for scheduler_prefs.json (same file the real service uses).
  final String identityDir;

  /// The deterministic 64-hex fingerprint of the inviting device (the peer
  /// a successful redeem pairs with — drives the home 0→1 linkage).
  static final String foreignFingerprint = 'a1b2c3d4' * 8;

  /// The well-formed foreign code this fake accepts for redemption.
  final String foreignCode = _fakeCodeFromSeed(
    List<int>.generate(20, (i) => i * 13 + 7),
  );

  final StreamController<PairingEvent> _events =
      StreamController<PairingEvent>.broadcast();

  final List<InviteSummary> _rows = <InviteSummary>[];
  SchedulerPreference? _preference;

  /// Every WELL-FORMED code handed to redeemInvite (i.e. codes that caused
  /// a REGISTRY round-trip — malformed codes never get here, §11 local
  /// reject). The journey asserts the illegal input is absent from it.
  final List<String> redeemRegistryCalls = <String>[];

  /// Codes created this session, in create order (fake-mode introspection:
  /// the page captures hints the same way).
  final List<String> createdCodes = <String>[];

  var _codeCounter = 0;
  var _onlineEmitted = false;

  /// Rows as of the last mutation (fake-mode introspection).
  List<InviteSummary> get rows => List.unmodifiable(_rows);

  void _emit(PairingEvent event) => _events.add(event);

  Future<void> _registryDelay([int ms = 120]) =>
      Future<void>.delayed(Duration(milliseconds: ms));

  void _ensureOnline() {
    if (_onlineEmitted) return;
    _onlineEmitted = true;
    // accepted ≠ online (ffi-v2 §2): the runtime comes online once and
    // says so on the event stream.
    _emit(ServiceOnlineEvent(atMs: DateTime.now().millisecondsSinceEpoch));
  }

  @override
  Stream<PairingEvent> get events => _events.stream;

  @override
  Future<List<InviteSummary>> listInvites({
    bool includeTerminal = false,
  }) async {
    await _registryDelay(60);
    return rows
        .where((row) => includeTerminal || row.state == InviteState.active)
        .toList();
  }

  @override
  Future<InviteCreated> createInvite({
    InviteTtlKind ttl = InviteTtlKind.longLived,
  }) async {
    _ensureOnline();
    await _registryDelay();
    _codeCounter++;
    final codeId = List<String>.generate(
      32,
      (i) => ((_codeCounter * 31 + i * 17) & 0xFF).toRadixString(16),
    ).join();
    final createdMs = DateTime.now().millisecondsSinceEpoch;
    // Contract: expires_ms == 0 means long-lived (never createdMs+0).
    final expiresMs = ttl.ttlSecs == 0 ? 0 : createdMs + ttl.ttlSecs * 1000;
    final code = _fakeCodeFromSeed(
      List<int>.generate(20, (i) => (_codeCounter * 251 + i * 37 + 11) & 0xFF),
    );
    final summary = InviteSummary(
      codeId: codeId,
      state: InviteState.active,
      createdMs: createdMs,
      expiresMs: expiresMs,
      revision: 1,
    );
    _rows.add(summary);
    createdCodes.add(code);
    return InviteCreated(summary: summary, invitationCode: code);
  }

  @override
  Future<void> redeemInvite(String inviteCode) async {
    // §11: 格式错误在本地拒绝 — the REAL validator throws before anything
    // is sent (the journey asserts redeemRegistryCalls stays clean).
    final normalized = normalizeInviteCode(inviteCode);
    _ensureOnline();
    redeemRegistryCalls.add(normalized);
    await _registryDelay(150);
    // The pairing dance runs asynchronously on its own timeline; the page
    // sees the feed update in real time, phase by phase.
    unawaited(() async {
      await Future<void>.delayed(const Duration(milliseconds: 250));
      if (normalized == foreignCode) {
        _emit(
          PeerPreparedEvent(
            atMs: DateTime.now().millisecondsSinceEpoch,
            pairingId: 'e2e' * 8 + '0' * 8,
          ),
        );
        await Future<void>.delayed(const Duration(milliseconds: 250));
        _emit(
          PairingCompleteEvent(
            atMs: DateTime.now().millisecondsSinceEpoch,
            pairingId: 'e2e' * 8 + '0' * 8,
            peer: foreignFingerprint,
          ),
        );
      } else {
        await Future<void>.delayed(const Duration(milliseconds: 150));
        _emit(
          InvitationRevokedEvent(
            atMs: DateTime.now().millisecondsSinceEpoch,
            codeId: '0' * 32,
            synced: false,
          ),
        );
      }
    }());
  }

  @override
  Future<void> revokeInvite(String codeId) async {
    await _registryDelay();
    final index = _rows.indexWhere((row) => row.codeId == codeId);
    if (index < 0) {
      throw StateError('unknown code id "$codeId"');
    }
    _rows[index] = InviteSummary(
      codeId: _rows[index].codeId,
      state: InviteState.revoked,
      createdMs: _rows[index].createdMs,
      expiresMs: _rows[index].expiresMs,
      revision: _rows[index].revision + 1,
    );
    _emit(
      InvitationRevokedEvent(
        atMs: DateTime.now().millisecondsSinceEpoch,
        codeId: codeId,
        synced: true,
      ),
    );
  }

  File get _prefsFile =>
      File('$identityDir${Platform.pathSeparator}scheduler_prefs.json');

  @override
  Future<SchedulerPreference> loadSchedulerPreference() async {
    final preference = _preference;
    if (preference != null) return preference;
    try {
      final file = _prefsFile;
      if (!file.existsSync()) return SchedulerPreference.defaults;
      final decoded = jsonDecode(file.readAsStringSync());
      if (decoded is Map<String, dynamic>) {
        return SchedulerPreference.fromJson(decoded);
      }
      return SchedulerPreference.defaults;
    } on Object {
      return SchedulerPreference.defaults; // corrupt prefs never block
    }
  }

  @override
  Future<void> saveSchedulerPreference(SchedulerPreference preference) async {
    _preference = preference;
    _prefsFile.writeAsStringSync(jsonEncode(preference.toJson()), flush: true);
  }

  @override
  void dispose() => _events.close();
}

// -----------------------------------------------------------------------------
// Fake-mode service-layer wiring: pairingComplete → pairing book → home.
// -----------------------------------------------------------------------------

/// Stands in for the real service integration (plan A6: auto-pairing
/// orchestration — 落簿 + refreshIdentity): listens for a
/// PairingCompleteEvent and folds its peer fingerprint into the REAL
/// `TransferService` pairing book via [TransferService.addPeer], which
/// refreshes the cached identity so the HomePage count follows. The real
/// FfiInviteService writes peers.book through the engine itself, so this
/// link is fake-mode only (it is not constructed when `_fake` is null).
class _InvitePairingLink {
  _InvitePairingLink({
    required InviteService invite,
    required TransferService transfer,
  }) : _subscription = invite.events.listen((event) {
         if (event is! PairingCompleteEvent) return;
         final peer = event.peer;
         if (peer.isEmpty) return;
         unawaited(() async {
           try {
             final count = await transfer.addPeer(peer, '');
             _log('pairing link: addPeer done, book now $count');
           } on Object catch (error) {
             _log('pairing link: addPeer FAILED: $error');
           }
         }());
       });

  final StreamSubscription<PairingEvent> _subscription;

  void dispose() {
    unawaited(_subscription.cancel());
  }
}

// -----------------------------------------------------------------------------
// Contract stand-in invite page (frozen keys; swapped at integration).
// -----------------------------------------------------------------------------

/// The contract stand-in for `lib/ui/pages/invite_page.dart`: implements the
/// frozen UI contract (keys + interactions above) against any real
/// [InviteService]. Real window, real widgets, real dialogs — the journey
/// below exercises exactly what the real page must implement.
class _StandInInvitePage extends StatefulWidget {
  const _StandInInvitePage({required this.service});

  final InviteService service;

  @override
  State<_StandInInvitePage> createState() => _StandInInvitePageState();
}

class _StandInInvitePageState extends State<_StandInInvitePage> {
  final TextEditingController _redeemController = TextEditingController();

  List<InviteSummary> _rows = const <InviteSummary>[];
  final Map<String, String> _hints = <String, String>{};
  final List<PairingEvent> _feed = <PairingEvent>[];
  SchedulerPreference _preference = SchedulerPreference.defaults;
  String? _redeemError;
  StreamSubscription<PairingEvent>? _subscription;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _subscription = widget.service.events.listen((event) {
      if (!mounted) return;
      setState(() => _feed.add(event));
    });
    unawaited(_bootstrap());
  }

  @override
  void dispose() {
    unawaited(_subscription?.cancel());
    _redeemController.dispose();
    super.dispose();
  }

  Future<void> _bootstrap() async {
    final rows = await widget.service.listInvites(includeTerminal: true);
    final preference = await widget.service.loadSchedulerPreference();
    if (!mounted) return;
    setState(() {
      _rows = rows;
      _preference = preference;
    });
  }

  Future<void> _reloadRows() async {
    final rows = await widget.service.listInvites(includeTerminal: true);
    if (!mounted) return;
    setState(() => _rows = rows);
  }

  Future<void> _createCode() async {
    final ttl = await _showTtlDialog();
    if (ttl == null) return; // cancelled
    setState(() => _busy = true);
    try {
      final created = await widget.service.createInvite(ttl: ttl);
      // The plaintext code is a bearer credential: show it exactly ONCE,
      // remember only the masked hint for the list.
      _hints[created.summary.codeId] = inviteCodeHint(created.invitationCode);
      await _reloadRows();
      if (mounted) await _showOneTimeCode(created.invitationCode);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<InviteTtlKind?> _showTtlDialog() {
    return showDialog<InviteTtlKind>(
      context: context,
      builder: (dialogContext) => _TtlDialog(),
    );
  }

  Future<void> _showOneTimeCode(String code) {
    return showDialog<void>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('邀请码已生成'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Text('完整邀请码仅展示这一次，请立即保存并分享：'),
            const SizedBox(height: 8),
            SelectableText(
              code,
              key: const ValueKey<String>('invite_code_once_text'),
            ),
          ],
        ),
        actions: [
          TextButton(
            key: const ValueKey<String>('invite_code_once_close'),
            onPressed: () => Navigator.of(dialogContext).pop(),
            child: const Text('我已保存'),
          ),
        ],
      ),
    );
  }

  Future<void> _revokeCode(InviteSummary row) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('撤销邀请码'),
        content: const Text('撤销后该码不能再被兑换，已完成的配对不受影响。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('取消'),
          ),
          TextButton(
            key: const ValueKey<String>('invite_revoke_confirm'),
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('确认撤销'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    setState(() => _busy = true);
    try {
      await widget.service.revokeInvite(row.codeId);
      await _reloadRows();
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _redeem() async {
    final text = _redeemController.text;
    setState(() => _redeemError = null);
    try {
      await widget.service.redeemInvite(text);
      // Accepted/pending: the outcome arrives on the event feed.
    } on FormatException catch (error) {
      if (mounted) setState(() => _redeemError = error.message);
    }
  }

  Future<void> _savePreference(SchedulerPreference preference) async {
    await widget.service.saveSchedulerPreference(preference);
    if (mounted) setState(() => _preference = preference);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    // SingleChildScrollView + Column (NOT a ListView): every child stays
    // BUILT regardless of scroll offset, so key-based reads/taps never
    // depend on lazy-build timing inside the ~600 px test window.
    return SingleChildScrollView(
      key: const ValueKey<String>('invite_page'),
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('配对邀请码', style: theme.textTheme.headlineSmall),
          const SizedBox(height: 4),
          Text(
            '码本身即授权：持有者可与本设备配对（owner 在线自动处理）',
            style: theme.textTheme.bodySmall,
          ),
          const SizedBox(height: 16),

          // -- Scheduler preference -----------------------------------------
          Text('任务调度', style: theme.textTheme.titleMedium),
          SwitchListTile(
            key: const ValueKey<String>('same_code_toggle'),
            title: const Text('同码并行（same_code）'),
            subtitle: const Text('关闭 = 同一邀请码同时只跑一个任务'),
            value: _preference.sameCodeMode == SchedCodeMode.parallel,
            onChanged: _busy
                ? null
                : (v) => unawaited(
                    _savePreference(
                      SchedulerPreference(
                        sameCodeMode: v
                            ? SchedCodeMode.parallel
                            : SchedCodeMode.serial,
                        crossCodeMode: _preference.crossCodeMode,
                      ),
                    ),
                  ),
          ),
          SwitchListTile(
            key: const ValueKey<String>('cross_code_toggle'),
            title: const Text('跨码并行（cross_code）'),
            subtitle: const Text('关闭 = 不同邀请码之间也串行'),
            value: _preference.crossCodeMode == SchedCodeMode.parallel,
            onChanged: _busy
                ? null
                : (v) => unawaited(
                    _savePreference(
                      SchedulerPreference(
                        sameCodeMode: _preference.sameCodeMode,
                        crossCodeMode: v
                            ? SchedCodeMode.parallel
                            : SchedCodeMode.serial,
                      ),
                    ),
                  ),
          ),
          const SizedBox(height: 8),

          // -- Generate ------------------------------------------------------
          FilledButton.icon(
            key: const ValueKey<String>('invite_create_btn'),
            icon: const Icon(Icons.add_link),
            label: const Text('生成邀请码'),
            onPressed: _busy ? null : () => unawaited(_createCode()),
          ),
          const SizedBox(height: 16),

          // -- Own codes -----------------------------------------------------
          Text('我的邀请码', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          if (_rows.isEmpty)
            const Padding(
              padding: EdgeInsets.symmetric(vertical: 16),
              child: Text('暂无邀请码', key: ValueKey<String>('invite_empty')),
            )
          else
            ..._rows.map(_codeTile),
          const SizedBox(height: 16),

          // -- Redeem --------------------------------------------------------
          Text('兑换邀请码', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          Row(
            children: [
              Expanded(
                child: TextField(
                  key: const ValueKey<String>('invite_code_input'),
                  controller: _redeemController,
                  decoration: const InputDecoration(
                    hintText: 'AKI1-XXXX-XXXX-…（收到对方分享的邀请码）',
                  ),
                ),
              ),
              const SizedBox(width: 12),
              FilledButton(
                key: const ValueKey<String>('invite_redeem_btn'),
                onPressed: () => unawaited(_redeem()),
                child: const Text('兑换'),
              ),
            ],
          ),
          if (_redeemError != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                _redeemError!,
                key: const ValueKey<String>('invite_redeem_error'),
                style: TextStyle(color: theme.colorScheme.error),
              ),
            ),
          const SizedBox(height: 16),

          // -- Live pairing-event feed --------------------------------------
          Text('配对事件', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          Container(
            key: const ValueKey<String>('invite_events'),
            width: double.infinity,
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
            ),
            child: _feed.isEmpty
                ? const Text('暂无事件')
                : Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (var i = 0; i < _feed.length; i++)
                        Text(
                          _feedLine(_feed[i]),
                          key: ValueKey<String>('invite_event_$i'),
                        ),
                    ],
                  ),
          ),
        ],
      ),
    );
  }

  String _feedLine(PairingEvent event) {
    final clock = _clockOf(event.atMs);
    if (event is ServiceOnlineEvent) return '$clock · 配对服务已连接';
    if (event is PeerPreparedEvent) return '$clock · 配对事务已持久化';
    if (event is PairingCompleteEvent) {
      final peer = event.peer;
      final short = peer.length >= 8 ? peer.substring(0, 8) : peer;
      return '$clock · 配对完成 · 对端指纹 $short';
    }
    if (event is InvitationRevokedEvent) return '$clock · 邀请码已撤销';
    return '$clock · ${event.name}';
  }

  String _two(int v) => v.toString().padLeft(2, '0');

  String _clockOf(int? atMs) {
    final local = atMs == null
        ? DateTime.now()
        : DateTime.fromMillisecondsSinceEpoch(atMs);
    return '${_two(local.hour)}:${_two(local.minute)}:${_two(local.second)}';
  }

  Widget _codeTile(InviteSummary row) {
    final theme = Theme.of(context);
    final stateText = switch (row.state) {
      InviteState.active => '有效',
      InviteState.revoked => '已撤销',
      InviteState.expired => '已过期',
    };
    final ttlText = row.expiresMs == 0
        ? '长期有效'
        : row.expiresMs - row.createdMs >= 3600 * 1000
        ? '${(row.expiresMs - row.createdMs) ~/ (3600 * 1000)} 小时有效'
        : '${(row.expiresMs - row.createdMs) ~/ (60 * 1000)} 分钟有效';
    final hint = _hints[row.codeId] ?? '${row.codeId.substring(0, 8)}…';
    return Card(
      child: ListTile(
        title: Text(
          hint,
          key: ValueKey<String>('invite_code_hint_${row.codeId}'),
        ),
        subtitle: Text(
          '创建于 ${_clockOf(row.createdMs)} · $ttlText',
          key: ValueKey<String>('invite_time_${row.codeId}'),
        ),
        trailing: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              stateText,
              key: ValueKey<String>('invite_state_${row.codeId}'),
              style: TextStyle(
                color: row.state == InviteState.active
                    ? theme.colorScheme.primary
                    : theme.colorScheme.outline,
              ),
            ),
            IconButton(
              key: ValueKey<String>('invite_revoke_${row.codeId}'),
              icon: const Icon(Icons.link_off),
              tooltip: '撤销',
              onPressed: row.state == InviteState.active
                  ? () => unawaited(_revokeCode(row))
                  : null,
            ),
          ],
        ),
      ),
    );
  }
}

/// TTL choice dialog of the create flow (the frozen two-kind enum).
class _TtlDialog extends StatefulWidget {
  @override
  State<_TtlDialog> createState() => _TtlDialogState();
}

class _TtlDialogState extends State<_TtlDialog> {
  InviteTtlKind _selected = InviteTtlKind.longLived;

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('生成邀请码'),
      content: RadioGroup<InviteTtlKind>(
        groupValue: _selected,
        onChanged: (value) =>
            setState(() => _selected = value ?? InviteTtlKind.longLived),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            RadioListTile<InviteTtlKind>(
              key: const ValueKey<String>('invite_ttl_long'),
              title: const Text('长期有效'),
              value: InviteTtlKind.longLived,
            ),
            RadioListTile<InviteTtlKind>(
              key: const ValueKey<String>('invite_ttl_short'),
              title: const Text('短期 1 小时'),
              value: InviteTtlKind.shortLived,
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        TextButton(
          key: const ValueKey<String>('invite_create_confirm'),
          onPressed: () => Navigator.of(context).pop(_selected),
          child: const Text('确认'),
        ),
      ],
    );
  }
}

// -----------------------------------------------------------------------------
// Journey shell: real HomePage + the invite page, NavigationRail like app.dart.
// -----------------------------------------------------------------------------

/// Test-hosting shell mirroring `AppShell` (NavigationRail + IndexedStack,
/// pages stay alive across tab switches) with the two pages this journey
/// needs: the REAL HomePage (home-linkage evidence) and the invite page.
class _JourneyShell extends StatefulWidget {
  const _JourneyShell({required this.transfer, required this.invite});

  final TransferService transfer;
  final InviteService invite;

  @override
  State<_JourneyShell> createState() => _JourneyShellState();
}

class _JourneyShellState extends State<_JourneyShell> {
  int _index = 0;

  // ═══════════════ INTEGRATION SWITCH (the ONE line to flip) ══════════════
  // Wave 22 (now): the contract stand-in page driven by the injected Fake
  //   (the real lib/ui/pages/invite_page.dart is being written in parallel
  //   and reads the service through the TransferService.inviteService seam).
  // After it lands the integrator replaces the body below with
  //   InvitePage(service: widget.invite)     // ← the one line
  // plus the import. All key-based journey assertions stay UNCHANGED (they
  // are the frozen contract); the `_fake`-marked ones no-op automatically.
  // ═══════════════════════════════════════════════════════════════════════
  Widget _buildInvitePage() => _StandInInvitePage(service: widget.invite);

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: LayoutBuilder(
        builder: (context, constraints) {
          final extended = constraints.maxWidth >= 1000;
          return Row(
            children: [
              NavigationRail(
                extended: extended,
                labelType: extended ? null : NavigationRailLabelType.all,
                selectedIndex: _index,
                onDestinationSelected: (i) => setState(() => _index = i),
                destinations: [
                  const NavigationRailDestination(
                    icon: Icon(Icons.home_outlined),
                    selectedIcon: Icon(Icons.home),
                    label: Text('主页'),
                  ),
                  const NavigationRailDestination(
                    icon: Icon(Icons.vpn_key_outlined),
                    selectedIcon: Icon(Icons.vpn_key),
                    // invite_nav lives on the label Text: this SDK's
                    // NavigationRailDestination has no key parameter.
                    label: Text('邀请码', key: ValueKey<String>('invite_nav')),
                  ),
                ],
              ),
              const VerticalDivider(width: 1, thickness: 1),
              Expanded(
                child: IndexedStack(
                  index: _index,
                  children: [
                    HomePage(service: widget.transfer),
                    _buildInvitePage(),
                  ],
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}

// -----------------------------------------------------------------------------
// Fixture.
// -----------------------------------------------------------------------------

/// Validates `AK_ID_DIR` (marker-guarded), wipes it for a true first run.
/// Runs BEFORE the services are constructed: `FfiTransferService` reads the
/// identity directory in its initializer list and HomePage's first
/// refreshIdentity CREATES the identity — the wipe must win that race
/// deterministically (app_e2e_test pattern).
void _setUpFixture() {
  final idDirEnv = Platform.environment['AK_ID_DIR'] ?? '';
  if (idDirEnv.isEmpty) {
    throw StateError(
      'AK_ID_DIR is not set. Isolation contract: point it at a throwaway '
      'directory (path must contain "$_idDirMarker") so the real '
      '%USERPROFILE%\\.ak-sending is never touched. See the runner '
      'contract in this file\'s library doc.',
    );
  }
  if (!idDirEnv.contains(_idDirMarker)) {
    throw StateError(
      'AK_ID_DIR "$idDirEnv" lacks the safety marker '
      '"$_idDirMarker"; refusing to wipe an unrecognized directory.',
    );
  }
  _receiverIdDir = Directory(idDirEnv);
  if (_receiverIdDir.existsSync()) {
    for (final entity in _receiverIdDir.listSync()) {
      entity.deleteSync(recursive: true);
    }
  } else {
    _receiverIdDir.createSync(recursive: true);
  }
  _log(
    'fixture: AK_ID_DIR=${_receiverIdDir.path} '
    'AK_FFI_DLL=${Platform.environment['AK_FFI_DLL'] ?? '<unset>'}',
  );
}

// -----------------------------------------------------------------------------
// Live-binding pump / read helpers (app_e2e_test.dart conventions).
// -----------------------------------------------------------------------------

/// Offstage-tolerant key finder — IndexedStack keeps both pages mounted but
/// only the selected one is painted; home reads happen while the invite
/// page is onstage and vice versa.
Finder _key(String value) =>
    find.byKey(ValueKey<String>(value), skipOffstage: false);

/// A rail destination label, scoped to the NavigationRail so page headings
/// with the same word cannot shadow it.
Finder _railLabel(String text) =>
    find.descendant(of: find.byType(NavigationRail), matching: find.text(text));

/// Pumps frames with real-time pacing (so the fake's delayed timelines and
/// the real service's async refreshes run) until [isSatisfied]; dumps all
/// texts on timeout for diagnosis.
Future<void> _pumpUntil(
  WidgetTester tester, {
  required bool Function() isSatisfied,
  required Duration timeout,
  required String what,
}) async {
  final deadline = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 50));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    if (isSatisfied()) {
      return;
    }
  }
  _log('TIMEOUT waiting for: $what');
  _log('texts now: ${_allTexts(tester).take(40).toList()}');
  fail('timed out after $timeout waiting for: $what');
}

/// Switches to a rail tab by its label, verified by an ONSAGE marker of the
/// target page, retrying the tap — a lost click must fail loudly.
Future<void> _goTab(
  WidgetTester tester, {
  required String label,
  required String onstageMarker,
}) async {
  for (var attempt = 1; attempt <= 3; attempt++) {
    await tester.tap(_railLabel(label));
    await tester.pump(const Duration(milliseconds: 100));
    await Future<void>.delayed(const Duration(milliseconds: 200));
    if (find.textContaining(onstageMarker).evaluate().isNotEmpty) {
      return;
    }
    _log('goTab($label) attempt $attempt: target page not onstage yet');
  }
  fail('could not switch to tab "$label" via the NavigationRail');
}

/// Scrolls [finder] into view, then taps it (the invite page is one long
/// scroll view inside a ~600 px window; all children are always BUILT, so
/// ensureVisible suffices).
Future<void> _tapVisible(WidgetTester tester, Finder finder) async {
  await tester.ensureVisible(finder);
  await tester.pump(const Duration(milliseconds: 50));
  await tester.tap(finder);
  await tester.pump(const Duration(milliseconds: 50));
}

/// ensureVisible + tap-to-focus + enterText for fields below the fold,
/// VERIFIED against the controller. The live binding (real window) can
/// drop the second `enterText` of a session when the field still holds a
/// stale IME state from an earlier interaction (observed wave 22: the
/// second redeem typed nothing and re-submitted the previous text) — so
/// the helper re-focuses by tap, then retries once, failing loudly.
Future<void> _enterText(WidgetTester tester, String key, String text) async {
  final finder = find.byKey(ValueKey<String>(key));
  for (var attempt = 1; attempt <= 2; attempt++) {
    await tester.ensureVisible(finder);
    await tester.pump(const Duration(milliseconds: 100));
    await tester.tap(finder); // (re)focus — real click on the field
    await tester.pump(const Duration(milliseconds: 100));
    await tester.enterText(finder, text);
    await tester.pump(const Duration(milliseconds: 100));
    final field = tester.widget<TextField>(finder);
    if (field.controller?.text == text) {
      return;
    }
    _log(
      'enterText attempt $attempt on "$key" did not land (field holds '
      '"${field.controller?.text}"); ${attempt == 1 ? 'retrying' : 'giving up'}',
    );
  }
  fail('could not type into field "$key" against the live window');
}

/// `.data` of the first Text matching [finder] (offstage-tolerant).
String? _textData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is Text ? widget.data : null;
}

/// `.data` of the first SelectableText matching [finder].
String? _selectableData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is SelectableText ? widget.data : null;
}

/// Value of the switch-style widget carrying [key] (parallel = true).
bool? _switchValue(WidgetTester tester, String key) {
  final finder = _key(key);
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  if (widget is SwitchListTile) return widget.value;
  if (widget is Switch) return widget.value;
  return null;
}

/// How many event feed lines are currently present (`invite_event_<n>`).
int _eventCount(WidgetTester tester) {
  var n = 0;
  while (_key('invite_event_$n').evaluate().isNotEmpty) {
    n++;
  }
  return n;
}

/// Every Text/SelectableText data string in the tree (diagnostics).
List<String> _allTexts(WidgetTester tester) {
  final texts = <String>[];
  for (final element
      in find
          .byWidgetPredicate(
            (widget) =>
                (widget is Text && (widget.data?.isNotEmpty ?? false)) ||
                (widget is SelectableText &&
                    (widget.data?.isNotEmpty ?? false)),
            skipOffstage: false,
          )
          .evaluate()) {
    final widget = element.widget;
    if (widget is Text && widget.data != null) {
      texts.add(widget.data!);
    } else if (widget is SelectableText && widget.data != null) {
      texts.add(widget.data!);
    }
  }
  return texts;
}

void _log(String message) {
  // ignore: avoid_print
  print('[ak-invite-e2e] $message');
}

// -----------------------------------------------------------------------------
// The journey.
// -----------------------------------------------------------------------------

void main() {
  // The fixture must win the race against the first frame (see its doc).
  _setUpFixture();

  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  // Services: the REAL DLL-backed transfer service (identity + pairing
  // book truth) and the injected invite service (fake this wave).
  _transfer = FfiTransferService();
  // ── INTEGRATION SWITCH construction site (see the top of this file) ──
  _fake = FakeInviteService(identityDir: _receiverIdDir.path);
  _inviteService = _fake!;
  _pairingLink = _fake == null
      ? null
      : _InvitePairingLink(invite: _inviteService, transfer: _transfer);

  tearDownAll(() async {
    // Best-effort cleanup of pure test artifacts.
    _pairingLink?.dispose();
    _inviteService.dispose();
    _transfer.dispose();
    try {
      if (_receiverIdDir.existsSync()) {
        for (final entity in _receiverIdDir.listSync()) {
          entity.deleteSync(recursive: true);
        }
        _receiverIdDir.deleteSync();
      }
    } on Exception {
      // Windows may hold a handle briefly; never fatal post-journey.
    }
    _log('tearDownAll: services disposed, artifacts cleaned (best effort)');
  });

  testWidgets(
    'invite journey: empty → create(长期/1h, one-time code) → '
    'illegal/legal redeem → pairingComplete → home peers 0→1 → revoke '
    'tombstone → scheduler toggles persist',
    (tester) async {
      final stopwatch = Stopwatch()..start();
      _log('journey start');

      // Engine sanity: the linkage half of the journey (addPeer) needs the
      // real DLL; fail fast with the probe evidence instead of stalling.
      final versionText = _transfer.version();
      expect(
        versionText.contains('不可用'),
        isFalse,
        reason:
            'the real ak_ffi.dll engine must load for the pairing-book '
            'linkage; got version "$versionText"',
      );

      await tester.pumpWidget(
        MaterialApp(
          title: 'AK-sending invite E2E',
          theme: ThemeData(
            useMaterial3: true,
            colorScheme: ColorScheme.fromSeed(
              seedColor: const Color(0xFF316A5A),
            ),
          ),
          home: _JourneyShell(transfer: _transfer, invite: _inviteService),
        ),
      );

      // -------------------------------------------------------------------
      // Step 1 — HomePage identity card: engine alive, pairing book EMPTY.
      // -------------------------------------------------------------------
      await _pumpUntil(
        tester,
        what: 'home pairing code to fill in (engine identityInfo)',
        timeout: const Duration(seconds: 30),
        isSatisfied: () => RegExp(r'^\d{3}-\d{3}$')
            .hasMatch(_selectableData(tester, _key('home_pairing_code')) ?? ''),
      );
      expect(
        _textData(tester, _key('home_peers_count')),
        contains('已配对设备：0 台'),
        reason: 'a wiped identity dir has an empty pairing book',
      );
      _log('step1 ok: identity card live, peers 0 (${stopwatch.elapsed})');

      // -------------------------------------------------------------------
      // Step 2 — Invite tab: empty state + default scheduler preference.
      // -------------------------------------------------------------------
      await _goTab(tester, label: '邀请码', onstageMarker: '生成邀请码');
      expect(
        _key('invite_nav').evaluate().isNotEmpty,
        isTrue,
        reason:
            'the rail destination carries the invite_nav key (on its '
            'label Text — see the key contract in the header)',
      );
      expect(
        _key('invite_page').evaluate().isNotEmpty,
        isTrue,
        reason: 'the invite page root is mounted (key invite_page)',
      );
      await _pumpUntil(
        tester,
        what: 'invite page bootstrap (empty code list)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => _textData(tester, _key('invite_empty')) == '暂无邀请码',
      );
      expect(
        _eventCount(tester),
        0,
        reason: 'the event feed starts empty (runtime not started yet)',
      );
      expect(
        _switchValue(tester, 'same_code_toggle'),
        isFalse,
        reason: 'default same_code_mode is serial (off = serial)',
      );
      expect(
        _switchValue(tester, 'cross_code_toggle'),
        isTrue,
        reason: 'default cross_code_mode is parallel (on = parallel)',
      );
      _log(
        'step2 ok: empty state + defaults serial/parallel '
        '(${stopwatch.elapsed})',
      );

      // -------------------------------------------------------------------
      // Step 3 — Create a SHORT-TERM (1 h) code; the plaintext shows ONCE.
      // -------------------------------------------------------------------
      await _tapVisible(
        tester,
        find.byKey(const ValueKey<String>('invite_create_btn')),
      );
      await _pumpUntil(
        tester,
        what: 'TTL dialog to open',
        timeout: const Duration(seconds: 5),
        isSatisfied: () => _key('invite_ttl_short').evaluate().isNotEmpty,
      );
      await tester.tap(_key('invite_ttl_short'));
      await tester.pump(const Duration(milliseconds: 100));
      await tester.tap(_key('invite_create_confirm'));
      await _pumpUntil(
        tester,
        what: 'one-time code dialog after the create op',
        timeout: const Duration(seconds: 10),
        isSatisfied: () =>
            (_selectableData(tester, _key('invite_code_once_text')) ?? '')
                .startsWith('AKI1-'),
      );
      final onceCode =
          _selectableData(tester, _key('invite_code_once_text')) ?? '';
      expect(
        onceCode,
        _fake!.createdCodes.first, // [fake-mode]
        reason: 'the one-time code is exactly what the service returned',
      );
      await tester.tap(_key('invite_code_once_close'));
      final firstId = _fake!.rows.first.codeId; // [fake-mode]
      await _pumpUntil(
        tester,
        what: 'created row to appear in the list',
        timeout: const Duration(seconds: 10),
        isSatisfied: () =>
            _key('invite_code_hint_$firstId').evaluate().isNotEmpty,
      );
      await tester.ensureVisible(_key('invite_code_hint_$firstId'));
      expect(
        _textData(tester, _key('invite_code_hint_$firstId')),
        inviteCodeHint(onceCode),
        reason:
            'the list shows the masked hint, never the plaintext '
            'secret (pair list --invites rule)',
      );
      expect(
        _textData(tester, _key('invite_state_$firstId')),
        '有效',
        reason:
            'a freshly claimed code renders 有效 (生效节点 = claim '
            'persisted)',
      );
      expect(
        _textData(tester, _key('invite_time_$firstId')),
        contains('1 小时有效'),
        reason: 'the time line carries the finite TTL chosen in the dialog',
      );
      expect(
        _key('invite_empty').evaluate().isNotEmpty,
        isFalse,
        reason: 'the empty state is gone once a code exists',
      );
      expect(
        find
            .descendant(
              of: _key('invite_events'),
              matching: find.textContaining('配对服务已连接', skipOffstage: false),
            )
            .evaluate()
            .isNotEmpty,
        isTrue,
        reason: 'the runtime start emitted service_online on the feed',
      );
      expect(_eventCount(tester), 1, reason: 'one feed line so far');
      _log(
        'step3 ok: 1h code created, one-time code shown, row 有效 '
        '(${stopwatch.elapsed})',
      );

      // -------------------------------------------------------------------
      // Step 4 — Create a second, LONG-LIVED code (ttl 0 → expires_ms 0).
      // -------------------------------------------------------------------
      await _tapVisible(
        tester,
        find.byKey(const ValueKey<String>('invite_create_btn')),
      );
      await _pumpUntil(
        tester,
        what: 'TTL dialog to open (second code)',
        timeout: const Duration(seconds: 5),
        isSatisfied: () => _key('invite_ttl_long').evaluate().isNotEmpty,
      );
      await tester.tap(_key('invite_ttl_long'));
      await tester.pump(const Duration(milliseconds: 100));
      await tester.tap(_key('invite_create_confirm'));
      await _pumpUntil(
        tester,
        what: 'second one-time code dialog',
        timeout: const Duration(seconds: 10),
        isSatisfied: () =>
            (_selectableData(tester, _key('invite_code_once_text')) ?? '')
                .startsWith('AKI1-'),
      );
      await tester.tap(_key('invite_code_once_close'));
      final secondId = _fake!.rows[1].codeId; // [fake-mode]
      await _pumpUntil(
        tester,
        what: 'long-term entry row to render',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => _key('invite_time_$secondId').evaluate().isNotEmpty,
      );
      await tester.ensureVisible(_key('invite_time_$secondId'));
      expect(
        _textData(tester, _key('invite_time_$secondId')),
        contains('长期有效'),
        reason: 'expires_ms == 0 renders the long-term marker',
      );
      _log('step4 ok: long-term code listed (${stopwatch.elapsed})');

      // -------------------------------------------------------------------
      // Step 5 — Redeem an ILLEGAL code: local reject, no registry call.
      // -------------------------------------------------------------------
      // Well-formed length (8 groups of 4) but contains I, O and U —
      // undefined Crockford symbols (charset excludes I/L/O/U).
      const illegalCode = 'AKI1-ABCD-EFG1-OOPS-UVWX-QRST-YZ23-CDEF-GH1J';
      final eventsBeforeIllegal = _eventCount(tester);
      final registryCallsBefore = _fake!.redeemRegistryCalls.length;
      await _enterText(tester, 'invite_code_input', illegalCode);
      await _tapVisible(
        tester,
        find.byKey(const ValueKey<String>('invite_redeem_btn')),
      );
      await _pumpUntil(
        tester,
        what: 'inline redeem error for the illegal code',
        timeout: const Duration(seconds: 10),
        isSatisfied: () =>
            (_textData(tester, _key('invite_redeem_error')) ?? '').isNotEmpty,
      );
      expect(
        _textData(tester, _key('invite_redeem_error')),
        isNotNull,
        reason: 'the FormatException reason renders inline',
      );
      expect(
        _fake!.redeemRegistryCalls.length,
        registryCallsBefore,
        reason:
            '格式错误在本地拒绝 (§11): a malformed code never reaches '
            'the registry round-trip',
      );
      expect(
        _eventCount(tester),
        eventsBeforeIllegal,
        reason: 'a locally rejected redeem emits no events',
      );
      _log(
        'step5 ok: illegal code locally rejected, registry calls '
        'unchanged (${stopwatch.elapsed})',
      );

      // -------------------------------------------------------------------
      // Step 6 — Redeem the LEGAL foreign code (typed LOWERCASE to prove
      // the ASCII lowercase→uppercase normalization of contract §1); the
      // feed updates in real time, phase by phase.
      // -------------------------------------------------------------------
      final legalLower = _fake!.foreignCode.toLowerCase();
      await _enterText(tester, 'invite_code_input', legalLower);
      await _tapVisible(
        tester,
        find.byKey(const ValueKey<String>('invite_redeem_btn')),
      );
      await _pumpUntil(
        tester,
        what: 'feed phase 1: 配对事务已持久化 (peer_prepared)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => find
            .descendant(
              of: _key('invite_events'),
              matching: find.textContaining('配对事务已持久化', skipOffstage: false),
            )
            .evaluate()
            .isNotEmpty,
      );
      // The registry saw exactly the well-formed legal code (normalized).
      expect(_fake!.redeemRegistryCalls, [
        _fake!.foreignCode,
      ], reason: 'the legal code (and only it) caused a registry round-trip');
      await _pumpUntil(
        tester,
        what: 'feed phase 2: 配对完成 with the owner fingerprint',
        timeout: const Duration(seconds: 15),
        isSatisfied: () => find
            .descendant(
              of: _key('invite_events'),
              matching: find.textContaining(
                '配对完成 · 对端指纹 '
                '${FakeInviteService.foreignFingerprint.substring(0, 8)}',
                skipOffstage: false,
              ),
            )
            .evaluate()
            .isNotEmpty,
      );
      _log(
        'step6 ok: legal redeem (lowercase normalized) → '
        'pairing_complete in feed (${stopwatch.elapsed})',
      );

      // -------------------------------------------------------------------
      // Step 7 — Home linkage: pairing_complete folded into the pairing
      // book (fake event → service-layer addPeer + refreshIdentity) →
      // HomePage count 0 → 1, peers.book on disk (REAL engine write).
      // -------------------------------------------------------------------
      await _pumpUntil(
        tester,
        what: 'HomePage pairing-book count to follow the pairing (0 → 1)',
        timeout: const Duration(seconds: 20),
        isSatisfied: () => (_textData(tester, _key('home_peers_count')) ?? '')
            .contains('已配对设备：1 台'),
      );
      expect(
        File('${_receiverIdDir.path}${Platform.pathSeparator}peers.book')
            .existsSync(),
        isTrue,
        reason:
            'disk truth: the real engine persisted peers.book under '
            'AK_ID_DIR via the pairing link',
      );
      _log(
        'step7 ok: home peers 0→1, peers.book on disk '
        '(${stopwatch.elapsed})',
      );

      // -------------------------------------------------------------------
      // Step 8 — Revoke the 1 h code via the confirm dialog; the row stays
      // listed as a tombstone (§8).
      // -------------------------------------------------------------------
      await _tapVisible(tester, _key('invite_revoke_$firstId'));
      await _pumpUntil(
        tester,
        what: 'revoke confirm dialog to open',
        timeout: const Duration(seconds: 5),
        isSatisfied: () => _key('invite_revoke_confirm').evaluate().isNotEmpty,
      );
      await tester.tap(_key('invite_revoke_confirm'));
      await _pumpUntil(
        tester,
        what: 'row state to flip to 已撤销 (tombstone stays listed)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () =>
            _textData(tester, _key('invite_state_$firstId')) == '已撤销',
      );
      await tester.ensureVisible(_key('invite_code_hint_$firstId'));
      expect(
        _key('invite_code_hint_$firstId').evaluate().isNotEmpty,
        isTrue,
        reason:
            'a revoked code STAYS listed as a tombstone (§8: 保留 '
            'tombstone，不能删除记录复活旧码)',
      );
      expect(
        _textData(tester, _key('invite_state_$secondId')),
        '有效',
        reason: 'the other code is unaffected',
      );
      expect(
        find
            .descendant(
              of: _key('invite_events'),
              matching: find.textContaining('邀请码已撤销', skipOffstage: false),
            )
            .evaluate()
            .isNotEmpty,
        isTrue,
        reason: 'the feed shows invitation_revoked',
      );
      _log('step8 ok: code revoked, tombstone listed (${stopwatch.elapsed})');

      // -------------------------------------------------------------------
      // Step 9 — Scheduler toggles: serial→parallel flips PERSIST (the
      // scheduler_prefs.json file under AK_ID_DIR is the proof).
      // -------------------------------------------------------------------
      await tester.ensureVisible(_key('same_code_toggle'));
      await tester.tap(_key('same_code_toggle'));
      await _pumpUntil(
        tester,
        what: 'same_code toggle to flip on (parallel)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => _switchValue(tester, 'same_code_toggle') == true,
      );
      await tester.ensureVisible(_key('cross_code_toggle'));
      await tester.tap(_key('cross_code_toggle'));
      await _pumpUntil(
        tester,
        what: 'cross_code toggle to flip off (serial)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => _switchValue(tester, 'cross_code_toggle') == false,
      );
      // Disk truth: the preference file carries both axes (the same file
      // the REAL FfiInviteService reads back on the next start).
      final prefsFile = File(
        '${_receiverIdDir.path}${Platform.pathSeparator}scheduler_prefs.json',
      );
      expect(
        prefsFile.existsSync(),
        isTrue,
        reason: 'saveSchedulerPreference persisted the file',
      );
      final prefsText = prefsFile.readAsStringSync();
      expect(
        prefsText,
        contains('"same_code_mode":"parallel"'),
        reason: 'same_code serial→parallel persisted; got $prefsText',
      );
      expect(
        prefsText,
        contains('"cross_code_mode":"serial"'),
        reason: 'cross_code parallel→serial persisted; got $prefsText',
      );
      // Persistence across page re-entry: leave the tab and come back.
      await _goTab(tester, label: '主页', onstageMarker: '配对码');
      await _goTab(tester, label: '邀请码', onstageMarker: '生成邀请码');
      await _pumpUntil(
        tester,
        what: 'invite page to come back after the round trip',
        timeout: const Duration(seconds: 5),
        isSatisfied: () => _key('invite_state_$firstId').evaluate().isNotEmpty,
      );
      expect(
        _switchValue(tester, 'same_code_toggle'),
        isTrue,
        reason: 'same_code=parallel persists across tab switches',
      );
      expect(
        _switchValue(tester, 'cross_code_toggle'),
        isFalse,
        reason: 'cross_code=serial persists across tab switches',
      );
      expect(
        (await _fake!.loadSchedulerPreference()).sameCodeMode,
        SchedCodeMode.parallel,
        reason: 'service-level reload sees the persisted preference',
      );
      _log(
        'step9 ok: same_code serial→parallel, cross_code parallel→serial, '
        'persisted to scheduler_prefs.json (${stopwatch.elapsed})',
      );

      _log('journey complete in ${stopwatch.elapsed}');
    },
    timeout: const Timeout(Duration(minutes: 5)),
    semanticsEnabled: false,
  );
}
