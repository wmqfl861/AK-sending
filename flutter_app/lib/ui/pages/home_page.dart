/// X4 (batch X, 2026-11-22, audit §4.3) home (「连接」 tab): the two
/// MAIN entries — 发送文件 / 接收文件 — plus each one's secondary
/// entry, and the live 「当前连接」 list. No IP/port/fingerprint forms
/// anywhere; the whole page runs on one-time pairing codes.
///
/// The direction is declared exactly ONCE, by the entry the user taps
/// (W-N1's proper fix):
///
/// | 入口 | 主路径 | 次要入口 |
/// |---|---|---|
/// | 发送文件 | 先选文件（可多选，含文件夹），再生成为发送方的码；对方加入且连通后自动开始发送 | 「对方已经给了你一个码？输入对方的码」（加入方为发送方） |
/// | 接收文件 | 输入对方的码（加入方为接收方），成功后等待对方发送 | 「想让对方输入你的码？生成接收码」（创建方为接收方） |
///
/// No role sheet before creating, no direction selector in the join
/// panel — the interface has no direction-less path (`startJoin` takes a
/// REQUIRED role). A failed create retries with the direction it declared
/// (`PairingFlowController.createRole`), never an implicit one.
///
/// The preselected payload is handed ONLY to the connection the shown
/// code produced (the flow controller correlates the latch by code), and
/// never while the code card's 更改 pick is in flight — see
/// `_HomePageState._changeSelection`.
///
/// Client UI U1 (2026-11-13): connection cards show the live progress of
/// a running transfer, and a 「最近传输」 section lists this session's
/// finished transfers (a v1 pairing carries one transfer and then leaves
/// the connection list, so this is where the user looks back).
///
/// Batch Z: a folder picked on Android is a STAGED copy in app-private
/// storage (the engine only takes filesystem paths). Whoever holds the
/// `PickedEntry` owns that copy. This page owns the primary send path's
/// preselected entries while they sit in `_pendingEntries`, and hands
/// their staged folders back through [FilePickGateway.releaseStaged] at
/// every point where that ownership ends — handing them to the connection
/// detail page is a TRANSFER, not a release.
library;

import 'dart:async' show StreamSubscription, unawaited;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart'
    show
        Clipboard,
        ClipboardData,
        FilteringTextInputFormatter,
        TextInputFormatter;

import '../service/file_pick_gateway.dart';
import '../service/pairing_flow_controller.dart';
import '../service/simple_transfer_service.dart';
import '../service/transfer_errors.dart';
import '../service/transfer_format.dart';
import '../service/transfer_history.dart';
import '../theme.dart';
import '../widgets/engine_error_card.dart';
import '../widgets/keep_foreground_hint.dart';
import '../widgets/task_connection_views.dart';
import '../widgets/transfer_error_card.dart';
import 'connection_detail_page.dart';

/// Home page: the two main paths + current connections.
class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    required this.service,
    this.picker = const PluginFilePickGateway(),
  });

  final SimpleTransferService service;

  /// File/folder picker seam. X4: the 发送文件 main path picks FIRST
  /// (multi-select) — the selection gates code creation and becomes the
  /// automatic send's payload. Tests inject a fake (the shell injects
  /// its own).
  final FilePickGateway picker;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late final PairingFlowController _flow;
  StreamSubscription<List<TaskConnection>>? _connectionsSub;
  StreamSubscription<EngineHealth>? _engineHealthSub;
  StreamSubscription<TransferProgress>? _progressSub;
  StreamSubscription<String>? _codeExpiredSub;
  List<TaskConnection> _connections = const [];

  /// U1: the latest progress snapshot per LIVE connection id.
  final Map<String, TransferProgress> _progress = {};

  /// The engine health as the health stream last reported it (R36-P2-03)
  /// — a latched failure renders the recoverable engine-failed card.
  EngineHealth _engineHealth = const EngineHealth.ok();

  /// X4: the join-code input panel's pre-declared direction (null = the
  /// panel is closed). The receive MAIN entry opens it as 接收文件; the
  /// send SECONDARY entry opens it as 发送文件 — never a direction-less
  /// form.
  TaskRole? _joinInputRole;

  /// X4: the primary send path's preselected entries (picked BEFORE the
  /// code is minted). Handed to the auto-opened connection detail page
  /// for the exactly-once automatic send once THIS code's connection is
  /// established (connected, or paired-but-unreachable). Null on every
  /// other path.
  List<PickedEntry>? _pendingEntries;

  /// W7.2/X4: the connection whose detail page this flow already
  /// auto-opened (one auto-open per pairing — a re-latch of the same
  /// connection never pushes twice).
  String? _autoOpenedConnectionId;

  /// X4 P1-1 (codex review): how many 更改 picks are currently in
  /// flight. The automatic-send handover is DEFERRED meanwhile — the
  /// connection completing behind the system picker must not hand over
  /// the OLD pending entries. [_changeSelection] runs it when the pick
  /// returns, with the selection the user ended up with: the NEW one, or
  /// the kept one after a cancelled change.
  ///
  /// P1-A (third review): a COUNTER, not a bool — the flow-generation
  /// guard makes two concurrent picks reachable (a stale flow's picker
  /// still open when another flow's own 更改 opens), and the first
  /// returning pick must not un-defer the handover while the second is
  /// still in flight.
  ///
  /// P2-D (fourth review): the counters are PER FLOW GENERATION. A stale
  /// flow's still-open picker must not defer (or, by returning, release)
  /// the CURRENT flow's handover — the guard below counts only picks the
  /// current generation opened.
  final Map<int, int> _picksInFlightByGeneration = <int, int>{};

  /// P2-E (fourth review): the shown code that expired at its TTL (the
  /// service's `codeExpired` fact) — the waiting card was reset for it
  /// and this notice card explains why, until the next flow begins.
  String? _expiredCodeNotice;

  @override
  void initState() {
    super.initState();
    _flow = PairingFlowController(widget.service)
      ..addListener(_onFlowChanged);
    _connectionsSub = widget.service.connections.listen(
      (snapshot) {
        if (mounted) {
          setState(() {
            _connections = snapshot;
            // U1: a retired row's progress goes with it.
            final live = {for (final connection in snapshot) connection.id};
            _progress.removeWhere((connId, _) => !live.contains(connId));
          });
        }
      },
      onError: (Object _) {},
    );
    _progressSub = widget.service.transferProgress.listen(
      (progress) {
        if (!mounted) return;
        final listed =
            _connections.any((connection) => connection.id == progress.connId);
        if (listed) {
          setState(() => _progress[progress.connId] = progress);
        }
      },
      onError: (Object _) {},
    );
    // R36-P2-03: an engine failure must be VISIBLE on the page the user
    // is looking at — the replayable health stream delivers the latch
    // (and replays it to a page rebuilt mid-failure) instead of leaving
    // stale waiting/connecting UI on screen.
    _engineHealthSub = widget.service.engineHealth.listen(
      (health) {
        if (!mounted) {
          return;
        }
        setState(() => _engineHealth = health);
        // P2-L (fourth review): the latch also FAILS every active flow —
        // the waiting card (with its copyable code) is a lie on a dead
        // engine: no peer can join a code the engine can no longer
        // redeem. Resetting the flow retires the panel (and its pending
        // selection) so the engine-failed card below is the one honest
        // state left on the page.
        if (health.isEngineFailed && _flow.active) {
          _flow.reset(); // notifies _onFlowChanged (picks invalidate too)
        }
      },
      onError: (Object _) {},
    );
    // P2-E (fourth review): the shown code's TTL expiry — the service's
    // `codeExpired` fact ends the waiting flow and surfaces the expiry
    // copy (the code card would otherwise wait forever on a code the
    // engine already refuses to redeem).
    _codeExpiredSub = widget.service.codeExpired.listen(_onCodeExpired,
        onError: (Object _) {});
  }

  @override
  void dispose() {
    // Batch Z: the page is gone — the selection it still owns goes back
    // with it. One already handed to the connection detail page left
    // `_pendingEntries` there and is that page's to release.
    _releaseStaged(widget.picker, _pendingEntries);
    _flow.removeListener(_onFlowChanged);
    _flow.dispose();
    _connectionsSub?.cancel();
    _engineHealthSub?.cancel();
    _progressSub?.cancel();
    _codeExpiredSub?.cancel();
    super.dispose();
  }

  /// P2-E: the awaited code THIS flow is showing died at its TTL — the
  /// waiting card is a lie now. Reset the flow (the pending selection
  /// dies with it, like every other flow end) and show the expiry notice
  /// until the next flow begins. Events for other codes (a stale flow's,
  /// or one already paired) change nothing.
  void _onCodeExpired(String codeId) {
    if (!mounted) return;
    if (_flow.code?.codeId != codeId ||
        _flow.phase != PairingFlowPhase.awaitingPeer) {
      return;
    }
    _flow.reset(); // notifies _onFlowChanged: the pending selection goes
    setState(() => _expiredCodeNotice = codeId);
  }

  void _onFlowChanged() {
    if (!mounted) return;
    if (!_flow.active) {
      // The flow ended (reset/cancel) — the pending selection of the
      // primary send path dies with it; the next send re-picks. Batch Z:
      // its staged folders go back with it.
      _releaseStaged(widget.picker, _pendingEntries);
      _pendingEntries = null;
    } else {
      // A new flow began — the previous flow's expiry notice retires.
      _expiredCodeNotice = null;
    }
    // X4/W7.2 连接成功直达 — when THIS device is the sender, the flow's
    // established state hands over to the connection detail page:
    //
    // * primary send path (preselected entries): the page receives the
    //   payload and sends it automatically (exactly once — the pending
    //   list is consumed here, and the `_autoOpenedConnectionId` guard
    //   keeps duplicate connection events from pushing twice). The
    //   handover fires on `success` AND on the paired-but-unreachable
    //   probe verdict: the send phase runs its own probe round and dial,
    //   so that verdict does not block the automatic send (审核 §4.3).
    // * join-send secondary path (no preselection): W7.2 keeps opening
    //   the page with the file picker already opening.
    //
    // The flow panel stays as-is beneath the pushed page and resets when
    // the user comes back, so back lands on the home actions with the
    // connection card listed.
    //
    // P1-1: a 更改 pick in flight defers the handover — `_pendingEntries`
    // is about to be replaced, and handing the old list over now would
    // send what the user is in the middle of changing. The deferred
    // handover runs from `_settleSelectionChange` when the pick returns.
    // P2-D: only the CURRENT generation's picks defer it — a stale flow's
    // still-open picker never blocked (or released) this flow's handover.
    final connection = _flow.connection;
    if (connection != null &&
        connection.role == TaskRole.send &&
        _autoOpenedConnectionId != connection.id &&
        !_picksInFlightFor(_flow.generation)) {
      final pending = _pendingEntries;
      // P2-K (fourth review): a TERMINAL row is never established — the
      // controller's state carry marks the row `disconnected` when the
      // connection dies after the unreachable verdict, so a deferred
      // handover must not open a dead connection's page and consume the
      // preselection.
      final terminal = connection.state == TaskConnectionState.disconnected;
      final established = !terminal &&
          (_flow.phase == PairingFlowPhase.success ||
              // X4: paired-but-unreachable counts as established for the
              // send — the controller's row state is the fresh fact here
              // (home's own snapshot may lag this notification by a
              // microtask).
              connection.state == TaskConnectionState.unreachable);
      if (established && (pending != null && pending.isNotEmpty ||
          _flow.phase == PairingFlowPhase.success)) {
        _autoOpenedConnectionId = connection.id;
        final autoSend = pending != null && pending.isNotEmpty;
        _pendingEntries = null; // exactly once
        final id = connection.id;
        // Batch Z: read here — the callback below may find the page gone.
        final picker = widget.picker;
        // Batch Z (U3): the handover belongs to THIS flow and to no other.
        // The callback runs a frame later, and the frame in between can
        // end the flow — e.g. a 取消配对 whose revoke round-trip lands
        // after the pairing already succeeded: the controller resets, the
        // established connection stays, and the old selection must not be
        // sent over it.
        final generation = _flow.generation;
        WidgetsBinding.instance.addPostFrameCallback((_) {
          // Batch Z (U3): re-check what the handover was scheduled for —
          // the page (the page left before this frame), the flow
          // generation (the flow was reset, failed or retried since), and
          // the connection (the flow's row must still be this one). Any
          // other case: no push, and the selection the callback holds goes
          // back — it was detached from `_pendingEntries` above, so nothing
          // else will ever free it.
          if (!mounted ||
              _flow.generation != generation ||
              _flow.connection?.id != id) {
            _releaseStaged(picker, pending);
            return;
          }
          Navigator.of(context)
              .push(
            MaterialPageRoute<void>(
              builder: (_) => ConnectionDetailPage(
                service: widget.service,
                connectionId: id,
                picker: widget.picker,
                // W7.2 lives on for the join-send secondary path.
                autoPickOnOpen: !autoSend,
                initialPicked: autoSend ? pending : const <PickedEntry>[],
                autoSendOnReady: autoSend,
              ),
            ),
          )
              .whenComplete(() {
          if (mounted) {
            _flow.reset();
          }
        });
        });
      }
    }
    setState(() {});
  }

  /// P2-D: opens one in-flight pick window for [generation] — the
  /// automatic-send handover of THAT generation (and no other) defers
  /// until the window closes.
  void _beginPick(int generation) {
    _picksInFlightByGeneration[generation] =
        (_picksInFlightByGeneration[generation] ?? 0) + 1;
  }

  /// P2-D: closes one in-flight pick window of [generation] (a stale
  /// generation's window closing never touches the current one's count).
  void _endPick(int generation) {
    final remaining = (_picksInFlightByGeneration[generation] ?? 1) - 1;
    if (remaining <= 0) {
      _picksInFlightByGeneration.remove(generation);
    } else {
      _picksInFlightByGeneration[generation] = remaining;
    }
  }

  /// P2-D: does [generation] have any 更改 pick in flight?
  bool _picksInFlightFor(int generation) =>
      (_picksInFlightByGeneration[generation] ?? 0) > 0;

  /// Batch Z: hands the staged folders behind [entries] back to [picker]
  /// (null = nothing owned). Fire-and-forget — it never delays or reorders
  /// the flow — and does nothing when no entry is staged.
  static void _releaseStaged(
      FilePickGateway picker, List<PickedEntry>? entries) {
    if (entries == null) return;
    final staged = [
      for (final entry in entries)
        if (entry.stagingId != null) entry,
    ];
    if (staged.isNotEmpty) {
      unawaited(picker.releaseStaged(staged));
    }
  }

  /// X4 发送文件主路径: pick the files FIRST (multi-select); only a
  /// non-empty selection mints the (sender-role) code. A cancelled pick
  /// (empty list) creates nothing — the home actions stay.
  ///
  /// P1-A (third review): the pick must land in the flow generation it
  /// started under. A create/join beginning while the picker is open (the
  /// user tapped another entry, or an earlier flow's stale pick returned)
  /// bumps [PairingFlowController.generation] — this selection is then
  /// discarded instead of minting a code the user already moved past.
  Future<void> _startSendFlow() async {
    final generation = _flow.generation;
    // Batch Z: read before the await — a result discarded below must still
    // reach the gateway.
    final picker = widget.picker;
    final entries = await picker.pickFiles();
    if (!mounted || entries.isEmpty) {
      if (!mounted) {
        // Batch Z: the page left with the picker up — a selection it can no
        // longer show belongs to no one: its staged folders go back.
        _releaseStaged(picker, entries);
      }
      return; // cancelled: no code
    }
    if (_flow.generation != generation) {
      // Batch Z: the discarded selection belongs to no flow — its staged
      // folders go back with it.
      _releaseStaged(picker, entries);
      return; // the flow moved on behind the picker — no code of ours
    }
    setState(() => _pendingEntries = entries);
    await _flow.startCreate(TaskRole.send);
  }

  /// X4 码卡片「更改」: re-run the multi-select. A cancelled change (empty
  /// list) KEEPS the last selection — only a real pick replaces it.
  ///
  /// X4 P1-1 (codex review): the automatic-send handover waits while this
  /// pick is in flight, which makes every interleaving deterministic:
  ///
  /// * the connection completes DURING the pick — nothing is handed over
  ///   with the OLD entries; when the pick returns the handover runs with
  ///   the NEW entries (or the kept ones after a cancelled change);
  /// * the pick returns FIRST — the selection is replaced (or kept) here
  ///   and the later completion hands over that selection as usual;
  /// * the completion lands between the tap's pointer-down and
  /// pointer-up — the handover has already taken the selection when the
  /// tap fires, so the tap starts no pick at all (the guard below;
  /// whichever of the two the app processes first wins).
  ///
  /// P1-A (third review): the pick belongs to the flow generation that
  /// owns the pending selection RIGHT NOW — remembered before the await,
  /// verified after. A 返回 (reset), a FAILURE (P1-D: `_fail` bumps the
  /// generation too, so a 更改 opened before a connection drop never
  /// lands in the RETRIED flow) or another flow's `_begin` while the
  /// picker is open moves [PairingFlowController.generation], and the
  /// result is DISCARDED: it must never land in the next flow's state
  /// (B's auto-send would carry A's files over B's connection). The
  /// handover window still closes in the `finally` (P2-D: keyed by the
  /// CAPTURED generation, so it never clears another flow's deferral),
  /// and a handover the open pick deferred for the CURRENT flow flushes
  /// either way.
  Future<void> _changeSelection() async {
    if (_pendingEntries == null) {
      // Nothing left to change: the selection was already handed over
      // (a tap landing in the same frame as the completion) or the flow
      // ended — either way it now belongs to someone else.
      return;
    }
    final generation = _flow.generation;
    // Batch Z: read before the await — a result discarded in the `finally`
    // must still reach the gateway, also once the page is gone.
    final picker = widget.picker;
    _beginPick(generation);
    List<PickedEntry> entries = const <PickedEntry>[];
    try {
      entries = await picker.pickFiles();
    } finally {
      // Settles on BOTH outcomes — a result, or a picker error — so a
      // deferred handover is never stranded behind a stuck count. An
      // error keeps the last selection like a cancelled change, then
      // propagates exactly as it did before this change.
      _endPick(generation);
      if (mounted) {
        if (_flow.generation == generation) {
          _settleSelectionChange(entries);
        } else {
          // A stale flow's pick: the result dies here — but the handover
          // check re-runs for whoever owns the pending selection now.
          // Batch Z: a result that dies here belongs to no flow — its
          // staged folders go back.
          _releaseStaged(picker, entries);
          _onFlowChanged();
        }
      } else if (entries.isNotEmpty) {
        // Batch Z: the page left with the picker up — nothing can take the
        // result any more, so its staged folders go back.
        _releaseStaged(picker, entries);
      }
    }
  }

  /// Applies the outcome of a 更改 pick and runs the handover it deferred.
  void _settleSelectionChange(List<PickedEntry> entries) {
    if (_pendingEntries == null) {
      // The flow ended while the picker was open (the selection dies with
      // it — see `_onFlowChanged`): the pick belongs to no flow. Batch Z:
      // nor do its staged folders — they go back.
      _releaseStaged(widget.picker, entries);
      return;
    }
    if (entries.isNotEmpty) {
      // Batch Z: the real pick replaces the last selection — the staged
      // folders of the replaced one go back.
      _releaseStaged(widget.picker, _pendingEntries);
      _pendingEntries = entries;
    }
    // Empty = cancelled: the last selection stays. Either way the flow may
    // have completed behind the picker — re-run the handover check now.
    _onFlowChanged();
  }

  /// The code card's 已选 summary line (null when nothing is pending).
  String? get _selectedSummary {
    final entries = _pendingEntries;
    if (entries == null || entries.isEmpty) {
      return null;
    }
    var bytes = 0;
    for (final entry in entries) {
      final size = entry.sizeBytes;
      if (entry.isDirectory || size == null) {
        return '已选：${entries.length} 项';
      }
      bytes += size;
    }
    return '已选：${entries.length} 项 · ${formatBytes(bytes)}';
  }

  void _openConnection(TaskConnection connection) {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ConnectionDetailPage(
          service: widget.service,
          connectionId: connection.id,
          picker: widget.picker,
        ),
      ),
    );
  }

  /// P2-J (fourth review): opening the join-code input panel is a NEW
  /// user intent on the home surface — invalidate every in-flight pick
  /// first. A send-pick still open behind the panel (the Windows
  /// parent-window activation shape: activating the app under the picker
  /// does not close it) must never confirm into a sender code that
  /// covers the panel being typed into.
  void _openJoinInput(TaskRole role) {
    _flow.invalidatePicks();
    setState(() => _joinInputRole = role);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final joinRole = _joinInputRole;
    return ListView(
      key: const ValueKey('home_page'),
      padding: const EdgeInsets.all(16),
      children: [
        if (_engineHealth.isEngineFailed)
          // R36-P2-03: the terminal engine-failure latch made VISIBLE —
          // the shared engine-error style, with the honest recoverable
          // affordance (restart the app; the engine is presumed broken
          // native-side, retrying actions on this service cannot heal it).
          EngineErrorCard(
            key: const ValueKey('engine_failed_card'),
            title: '传输引擎故障，请重启应用',
            detail: _engineHealth.failureReason ?? '连续原生轮询失败。',
            detailKey: const ValueKey('engine_failed_card_detail'),
          ),
        if (_expiredCodeNotice != null && !_flow.active)
          // P2-E: the shown code's TTL expiry — the waiting flow was
          // reset the moment the service announced it; this card is the
          // honest reason until the next flow begins.
          _CodeExpiredNotice(
            key: const ValueKey('flow_code_expired'),
            onDismiss: () => setState(() => _expiredCodeNotice = null),
          ),
        Text('连接', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 4),
        Text('无需设置任何信息——一个配对码即可互传文件',
            style: theme.textTheme.bodySmall),
        const SizedBox(height: 16),
        if (_flow.active)
          _PairingFlowView(
            controller: _flow,
            service: widget.service,
            selectedSummary: _selectedSummary,
            onChangeSelection: _changeSelection,
            showDirectUnavailableGuidance: _connections.any((connection) =>
                connection.id == _flow.connection?.id &&
                connection.state == TaskConnectionState.unreachable),
            onJoinRetry: () {
              // Capture the declared direction BEFORE the reset clears
              // it (X4: the retried panel keeps the entry's direction).
              final role = _flow.joinRole ?? TaskRole.receive;
              _flow.reset();
              setState(() => _joinInputRole = role);
            },
          )
        else if (joinRole != null)
          _JoinInputPanel(
            // X4: the direction was declared by the home ENTRY the user
            // tapped (W-N1's fix: the engine cannot infer the joiner's
            // direction across devices, so the UI declares it exactly
            // once, up front). The panel asks for the code only; it
            // closes on submit so the later flow reset (the sender-side
            // auto-open) returns to the actions, not back into this form.
            role: joinRole,
            onSubmit: (code) {
              setState(() => _joinInputRole = null);
              _flow.startJoin(code, role: joinRole);
            },
            onCancel: () => setState(() => _joinInputRole = null),
          )
        else
          _HomeActions(
            onSend: _startSendFlow,
            // P2-J: both join-panel entries retire any in-flight pick.
            onJoinSendCode: () => _openJoinInput(TaskRole.send),
            onReceive: () => _openJoinInput(TaskRole.receive),
            onCreateReceiveCode: () => _flow.startCreate(TaskRole.receive),
          ),
        const SizedBox(height: 24),
        _ConnectionsSection(
          connections: _connections,
          progress: _progress,
          onOpen: _openConnection,
        ),
        const _HistorySection(),
      ],
    );
  }
}

/// X4: the two MAIN entries plus each one's secondary entry. The tapped
/// button IS the one-time direction declaration (审核 §4.3 关键流程).
class _HomeActions extends StatelessWidget {
  const _HomeActions({
    required this.onSend,
    required this.onJoinSendCode,
    required this.onReceive,
    required this.onCreateReceiveCode,
  });

  /// 发送文件主路径: multi-select pick first, then a sender-role code.
  final VoidCallback onSend;

  /// 发送文件次要入口: 对方已经给了你一个码？输入对方的码 (join as sender).
  final VoidCallback onJoinSendCode;

  /// 接收文件主路径: 输入对方的码 (join as receiver).
  final VoidCallback onReceive;

  /// 接收文件次要入口: 想让对方输入你的码？生成接收码 (creator is receiver).
  final VoidCallback onCreateReceiveCode;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        FilledButton.icon(
          key: const ValueKey('home_send_button'),
          style: FilledButton.styleFrom(
            padding: const EdgeInsets.symmetric(vertical: 20),
          ),
          onPressed: onSend,
          icon: const Icon(Icons.upload_rounded, size: 28),
          label: const Text('发送文件', style: TextStyle(fontSize: 18)),
        ),
        const SizedBox(height: 8),
        TextButton(
          key: const ValueKey('home_send_join_code_button'),
          onPressed: onJoinSendCode,
          child: const Text('对方已经给了你一个码？输入对方的码'),
        ),
        const SizedBox(height: 12),
        FilledButton.tonalIcon(
          key: const ValueKey('home_receive_button'),
          style: FilledButton.styleFrom(
            padding: const EdgeInsets.symmetric(vertical: 20),
          ),
          onPressed: onReceive,
          icon: const Icon(Icons.download_rounded, size: 28),
          label: const Text('接收文件', style: TextStyle(fontSize: 18)),
        ),
        const SizedBox(height: 8),
        TextButton(
          key: const ValueKey('home_receive_create_code_button'),
          onPressed: onCreateReceiveCode,
          child: const Text('想让对方输入你的码？生成接收码'),
        ),
      ],
    );
  }
}

/// P2-E (fourth review): the shown code's TTL-expiry notice — the
/// failure-card shape (W-N9 family: 结果 + 原因 + 建议) with the
/// transfer_errors copy, shown after the service's `codeExpired` fact
/// reset the waiting flow. 返回 dismisses it; starting any new flow
/// retires it too.
class _CodeExpiredNotice extends StatelessWidget {
  const _CodeExpiredNotice({super.key, required this.onDismiss});

  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 16),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  Icon(Icons.timer_off, color: theme.colorScheme.error,
                      size: 32),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Text(
                      codeExpiredTitle,
                      key: const ValueKey('flow_code_expired_title'),
                      style: theme.textTheme.titleMedium
                          ?.copyWith(color: theme.colorScheme.error),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              Text(codeExpiredReason, style: theme.textTheme.bodyMedium),
              const SizedBox(height: 4),
              Text(codeExpiredSuggestion,
                  style: theme.textTheme.bodyMedium
                      ?.copyWith(color: theme.colorScheme.outline)),
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton(
                  key: const ValueKey('flow_code_expired_dismiss'),
                  onPressed: onDismiss,
                  child: const Text('返回'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The join-code input form (加入流 first step): auto-uppercase,
/// hyphen/space-tolerant paste of the FULL `AKI1-…` code (wave-29 P1-02),
/// live normalized-length counter — and NOTHING else: X4 removed the
/// direction selector because the direction was already declared by the
/// home entry that opened this panel (审核 §4.3 / W 复核 §4 — direction
/// from the first click, declared once; the engine cannot infer a
/// role-less join across devices, W-N1).
class _JoinInputPanel extends StatefulWidget {
  const _JoinInputPanel({
    required this.role,
    required this.onSubmit,
    required this.onCancel,
  });

  /// The direction the user declared by tapping the entry (X4). Fixed
  /// for the life of the panel — forwarded verbatim with the join.
  final TaskRole role;

  final void Function(String code) onSubmit;
  final VoidCallback onCancel;

  @override
  State<_JoinInputPanel> createState() => _JoinInputPanelState();
}

class _JoinInputPanelState extends State<_JoinInputPanel> {
  final TextEditingController _controller = TextEditingController();

  static final TextInputFormatter _upperCase = TextInputFormatter.withFunction(
    (oldValue, newValue) => newValue.copyWith(text: newValue.text.toUpperCase()),
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final normalized = normalizeJoinCode(_controller.text);
    final codeValid = isValidTaskCode(normalized);
    final ready = codeValid;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('输入对方的配对码', style: theme.textTheme.titleMedium),
            const SizedBox(height: 4),
            Text(
              widget.role == TaskRole.send ? '你将把文件发给对方。' : '对方将把文件发给你。',
              key: const ValueKey('join_role_hint'),
              style: theme.textTheme.bodySmall,
            ),
            const SizedBox(height: 12),
            TextField(
              key: const ValueKey('join_code_field'),
              controller: _controller,
              autofocus: true,
              inputFormatters: [
                FilteringTextInputFormatter.allow(RegExp('[0-9A-Za-z- ]')),
                _upperCase,
              ],
              textCapitalization: TextCapitalization.characters,
              onChanged: (_) => setState(() {}),
              onSubmitted: ready ? (_) => widget.onSubmit(_controller.text) : null,
              decoration: const InputDecoration(
                border: OutlineInputBorder(),
                hintText: 'AKI1-XXXX-XXXX-XXXX-…（完整复制粘贴即可）',
                helperText: '大小写、连字符、空格均可，自动识别',
                counterText: '',
              ),
              // The grouped display form is 44 characters; the slack also
              // tolerates pasted leading/trailing spaces.
              maxLength: 64,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 16,
                letterSpacing: 1,
              ),
            ),
            const SizedBox(height: 4),
            Text(
              '${normalized.length}/36 位'
              '${normalized.isNotEmpty && !codeValid ? '（需 AKI1 开头的 36 位字符）' : ''}',
              key: const ValueKey('join_code_counter'),
              style: theme.textTheme.bodySmall,
            ),
            const SizedBox(height: 12),
            FilledButton.icon(
              key: const ValueKey('join_submit_button'),
              onPressed: ready
                  ? () => widget.onSubmit(_controller.text)
                  : null,
              icon: const Icon(Icons.link),
              label: const Text('配对'),
            ),
            TextButton(
              key: const ValueKey('join_cancel_button'),
              onPressed: widget.onCancel,
              child: const Text('取消'),
            ),
          ],
        ),
      ),
    );
  }
}

/// Renders the active flow phase (code display / waiting / probe /
/// success / failure) driven by [PairingFlowController].
class _PairingFlowView extends StatelessWidget {
  const _PairingFlowView({
    required this.controller,
    required this.service,
    required this.selectedSummary,
    required this.onChangeSelection,
    required this.showDirectUnavailableGuidance,
    required this.onJoinRetry,
  });

  final PairingFlowController controller;
  final SimpleTransferService service;

  /// X4: the primary send path's 已选 summary (null on every other
  /// path) — rendered on the code card with a 「更改」 affordance.
  final String? selectedSummary;

  /// X4: the code card's 更改 callback (re-run the multi-select; a
  /// cancelled change keeps the last selection).
  final VoidCallback? onChangeSelection;

  final bool showDirectUnavailableGuidance;
  final VoidCallback onJoinRetry;

  @override
  Widget build(BuildContext context) {
    final phase = controller.phase;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: AnimatedSwitcher(
          duration: const Duration(milliseconds: 200),
          child: switch (phase) {
            PairingFlowPhase.creating => const _FlowProgress(
                key: ValueKey('flow_creating'), label: '正在生成配对码…'),
            PairingFlowPhase.awaitingPeer => _CodeShownView(
                controller: controller,
                selectedSummary: selectedSummary,
                onChangeSelection: onChangeSelection),
            PairingFlowPhase.joining => const _FlowProgress(
                key: ValueKey('flow_joining'), label: '正在连接…'),
            PairingFlowPhase.paired => const _FlowProgress(
                key: ValueKey('flow_paired'), label: '已配对，正在建立连接…'),
            PairingFlowPhase.probing => const _FlowProgress(
                key: ValueKey('flow_probing'), label: '正在检查网络…'),
            PairingFlowPhase.success => _FlowSuccessView(
                controller: controller, service: service),
            PairingFlowPhase.failed || null => _FlowFailedView(
                controller: controller,
                showDirectUnavailableGuidance: showDirectUnavailableGuidance,
                onJoinRetry: onJoinRetry,
              ),
          },
        ),
      ),
    );
  }
}

/// Spinner + label row for the indeterminate flow phases.
class _FlowProgress extends StatelessWidget {
  const _FlowProgress({super.key, required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        const SizedBox(
          width: 24,
          height: 24,
          child: CircularProgressIndicator(strokeWidth: 2.5),
        ),
        const SizedBox(width: 16),
        Expanded(child: Text(label, style: Theme.of(context).textTheme.bodyLarge)),
      ],
    );
  }
}

/// 大号配对码（等宽）+ 复制 + 等待动画. X4: the primary send path's code
/// card also carries the 已选 summary with a 「更改」 affordance.
class _CodeShownView extends StatelessWidget {
  const _CodeShownView({
    required this.controller,
    required this.selectedSummary,
    required this.onChangeSelection,
  });

  final PairingFlowController controller;
  final String? selectedSummary;
  final VoidCallback? onChangeSelection;

  Future<void> _cancel(PairingFlowController flow) async {
    // Wave-30 P2-04: cancelling the wait also REVOKES the unconsumed code
    // engine-side (disconnect(code_id)) — the code dies with the flow
    // instead of lingering redeemable until its TTL. A failed revoke
    // surfaces in the flow's failed state (honest, never silent).
    await flow.cancelWaiting();
  }

  Future<void> _copy(BuildContext context, String code) async {
    await Clipboard.setData(ClipboardData(text: code));
    if (context.mounted) {
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(content: Text('已复制配对码 $code'),
            duration: const Duration(seconds: 2)),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final code = controller.code?.code ?? '';
    final role = controller.code?.role;
    final summary = selectedSummary;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text('我的配对码', style: theme.textTheme.titleMedium),
            const SizedBox(width: 8),
            if (role != null)
              Chip(
                label: Text(taskRoleShortLabel(role)),
                visualDensity: VisualDensity.compact,
              ),
          ],
        ),
        const SizedBox(height: 8),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            // The full AKI1 display form is 44 characters: wrap over (at
            // most) two monospace lines instead of shrinking to dust at
            // 390dp.
            Expanded(
              child: SelectableText(
                code,
                key: const ValueKey('flow_pairing_code'),
                maxLines: 2,
                style: theme.textTheme.headlineMedium?.copyWith(
                  fontWeight: FontWeight.w700,
                  fontFamily: 'monospace',
                  letterSpacing: 1,
                  height: 1.25,
                ),
              ),
            ),
            const SizedBox(width: 8),
            IconButton.outlined(
              key: const ValueKey('flow_copy_code'),
              icon: const Icon(Icons.copy),
              tooltip: '复制配对码',
              onPressed: () => _copy(context, code),
            ),
          ],
        ),
        // X4: 已选摘要 + 更改 — only on the primary send path's card.
        if (summary != null) ...[
          const SizedBox(height: 8),
          Row(
            children: [
              Expanded(
                child: Text(
                  summary,
                  key: const ValueKey('flow_selected_summary'),
                  style: theme.textTheme.bodyMedium,
                ),
              ),
              TextButton(
                key: const ValueKey('flow_change_selection_button'),
                onPressed: onChangeSelection,
                child: const Text('更改'),
              ),
            ],
          ),
        ],
        const SizedBox(height: 16),
        Row(
          key: const ValueKey('flow_waiting'),
          children: [
            const SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Text(
                role == TaskRole.send
                    ? '把这个码给对方，对方输入后就会收到你的文件。'
                    : '把这个码给对方，对方输入后就可以把文件发给你。',
                style: theme.textTheme.bodyMedium,
              ),
            ),
          ],
        ),
        // P2-B (third review): the peer-join wait is a keep-foreground
        // stage too (a backgrounded process kills the registry keeper —
        // the W-N11 reproduction; W-N11 extended keep-awake, the hint
        // family follows). Android-only, W7.3 copy adapted per stage.
        const SizedBox(height: 6),
        const KeepForegroundHint(
          keyName: 'flow_waiting_keep_foreground',
          text: '等待对方加入期间请保持本应用在前台',
        ),
        const SizedBox(height: 12),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const ValueKey('flow_cancel_button'),
            onPressed: () => _cancel(controller),
            child: const Text('取消配对'),
          ),
        ),
      ],
    );
  }
}

/// 连接成功（连通检查通过）.
///
/// W7.1/W7.2: the card explains the user's ACTUAL role. The RECEIVER
/// waits here — no 完成 click needed; the sender variant only renders
/// for the one frame before the home page auto-opens the connection's
/// detail page (the automatic send of the primary path, or the file
/// picker of the W7.2 join-send handover — see
/// `_HomePageState._onFlowChanged`).
class _FlowSuccessView extends StatelessWidget {
  const _FlowSuccessView({required this.controller, required this.service});

  final PairingFlowController controller;
  final SimpleTransferService service;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final connection = controller.connection;
    final success = successColor(theme.colorScheme);
    final sending = connection?.role == TaskRole.send;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Icon(Icons.check_circle, color: success, size: 40),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('连接成功',
                      key: const ValueKey('flow_success'),
                      style: theme.textTheme.titleLarge
                          ?.copyWith(color: success)),
                  if (connection != null)
                    Text(
                      '已连接：${connection.peerName}',
                      style: theme.textTheme.bodySmall,
                    ),
                ],
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Text(
          sending ? '现在可以选择文件发给对方' : '对方将把文件发给你',
          key: const ValueKey('flow_success_role_hint'),
          style: theme.textTheme.bodyMedium,
        ),
        if (!sending) ...[
          const SizedBox(height: 4),
          Text(
            '等待对方发送文件…',
            key: const ValueKey('flow_waiting_peer_send'),
            style: theme.textTheme.bodyMedium
                ?.copyWith(color: theme.colorScheme.outline),
          ),
          const SizedBox(height: 4),
          Text(
            '接收的文件将保存到：${service.saveDir}',
            key: const ValueKey('flow_success_save_dir'),
            style: theme.textTheme.bodySmall,
          ),
        ] else ...[
          const SizedBox(height: 4),
          Text(
            '正在打开文件选择…',
            key: const ValueKey('flow_opening_picker'),
            style: theme.textTheme.bodySmall
                ?.copyWith(color: theme.colorScheme.outline),
          ),
        ],
        const SizedBox(height: 8),
        // W7.2: no 完成 button — the receiver simply waits (the receive
        // banner takes over when the peer sends); the sender already
        // moved on. 返回 dismisses ONLY the panel (the pairing stays).
        TextButton(
          key: const ValueKey('flow_back_button'),
          onPressed: controller.reset,
          child: const Text('返回'),
        ),
      ],
    );
  }
}

/// 失败态：可读原因 + 重试/重新输入.
///
/// W3 (V-N4): when the controller carries the RAW error object
/// (`errorCause`), the mapped failure card replaces the plain line —
/// title/reason/suggestion from the error code, original text only in
/// the expandable 「详情」. Controller-worded failures (probe verdict,
/// drop) keep their plain, already §4.5-compliant line.
class _FlowFailedView extends StatelessWidget {
  const _FlowFailedView({
    required this.controller,
    required this.showDirectUnavailableGuidance,
    required this.onJoinRetry,
  });

  final PairingFlowController controller;
  final bool showDirectUnavailableGuidance;
  final VoidCallback onJoinRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final isJoin = controller.kind == PairingFlowKind.join;
    final cause = controller.errorCause;
    // X4 P1-2: the create flow retries with the direction it DECLARED.
    // The role used to be read off `code`/`connection`, but a failed
    // `createCode` has neither — the retry then fell through to an
    // implicit `TaskRole.send`, turning a 接收-create retry into a
    // send-create. The controller remembers the role from before the
    // engine call; when even that is unknown there is no honest
    // direction to retry into, so the button stays disabled rather than
    // guess.
    final createRetryRole = controller.createRole;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (cause != null)
          TransferErrorCard(
            error: cause,
            // X1 follow-through (X4 wiring): the join flow's failures map
            // through the JOINING scene (W-N9's three-layer copy); the
            // create flow keeps the pairing scene.
            scene:
                isJoin ? TransferErrorScene.joining : TransferErrorScene.pairing,
            keyPrefix: 'flow_error',
            titleKey: const ValueKey('flow_error'),
          )
        else
          Row(
            children: [
              Icon(Icons.error_outline, color: theme.colorScheme.error, size: 32),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  controller.error ?? '配对失败，请重试。',
                  key: const ValueKey('flow_error'),
                  style: theme.textTheme.bodyLarge?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ),
            ],
          ),
        if (showDirectUnavailableGuidance) ...[
          const SizedBox(height: 12),
          const DirectUnavailableGuidance(),
        ],
        const SizedBox(height: 16),
        FilledButton.icon(
          key: const ValueKey('flow_retry_button'),
          onPressed: isJoin
              // X4: the HOME side owns the retry — it must capture the
              // flow's declared join direction BEFORE reset() clears it,
              // so the reopened panel keeps the entry's direction.
              ? onJoinRetry
              // P1-2: the create flow's remembered direction — never an
              // implicit default (null disables the button).
              : createRetryRole == null
                  ? null
                  : () => controller.startCreate(createRetryRole),
          icon: const Icon(Icons.refresh),
          label: Text(isJoin ? '重新输入配对码' : '重试'),
        ),
        TextButton(
          key: const ValueKey('flow_back_button'),
          onPressed: controller.reset,
          child: const Text('返回'),
        ),
      ],
    );
  }
}

/// 「当前连接」 list section.
class _ConnectionsSection extends StatelessWidget {
  const _ConnectionsSection({
    required this.connections,
    required this.progress,
    required this.onOpen,
  });

  final List<TaskConnection> connections;

  /// U1: the latest transfer progress per connection id.
  final Map<String, TransferProgress> progress;

  final ValueChanged<TaskConnection> onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text('当前连接',
                key: const ValueKey('connections_header'),
                style: theme.textTheme.titleMedium),
            const SizedBox(width: 8),
            Text('（${connections.length}）', style: theme.textTheme.bodySmall),
          ],
        ),
        const SizedBox(height: 4),
        if (connections.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Text(
              '暂无连接。发送或接收文件后，连接会显示在这里。',
              key: const ValueKey('connections_empty'),
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.outline),
            ),
          )
        else
          ...connections.map(
            (connection) => ConnectionCard(
              connection: connection,
              progress: progress[connection.id],
              onOpen: () => onOpen(connection),
            ),
          ),
      ],
    );
  }
}

/// U1: 「最近传输」 — this session's finished transfers, newest first.
/// Hidden when the app shell provides no [TransferHistoryScope] (direct
/// widget tests) or nothing finished yet.
class _HistorySection extends StatelessWidget {
  const _HistorySection();

  /// How many records the home page lists (the history keeps more).
  static const int shown = 10;

  @override
  Widget build(BuildContext context) {
    final history = TransferHistoryScope.watch(context);
    if (history == null || history.isEmpty) {
      return const SizedBox.shrink();
    }
    final theme = Theme.of(context);
    final records = history.records.take(shown).toList();
    return Padding(
      key: const ValueKey('history_section'),
      padding: const EdgeInsets.only(top: 24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Text('最近传输', style: theme.textTheme.titleMedium),
              ),
              TextButton(
                key: const ValueKey('history_clear_button'),
                onPressed: history.clear,
                child: const Text('清空'),
              ),
            ],
          ),
          Text('仅保留本次运行期间的记录，不含文件名。',
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.outline)),
          const SizedBox(height: 4),
          for (var i = 0; i < records.length; i++)
            _HistoryTile(key: ValueKey('history_item_$i'), record: records[i]),
        ],
      ),
    );
  }
}

/// One 「最近传输」 row: direction, size, result, time, duration, path.
class _HistoryTile extends StatelessWidget {
  const _HistoryTile({super.key, required this.record});

  final TransferRecord record;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final sending = record.role == TaskRole.send;
    final files = record.fileCount;
    final bytes = record.totalBytes;
    final title = StringBuffer(sending ? '发送' : '接收');
    if (files != null) {
      title.write(' $files 个文件');
    }
    if (bytes != null) {
      title.write('（${formatBytes(bytes)}）');
    }
    final finished = record.finishedAt;
    final clock = '${finished.hour.toString().padLeft(2, '0')}:'
        '${finished.minute.toString().padLeft(2, '0')}';
    final elapsed = record.elapsed;
    final details = <String>[
      record.succeeded ? '完成' : '失败',
      clock,
      if (elapsed != null) '用时 ${formatElapsed(elapsed)}',
      ?record.pathLabel,
      record.peerName,
    ];
    final color = record.succeeded ? successColor(scheme) : scheme.error;
    return Card(
      margin: const EdgeInsets.symmetric(vertical: 4),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        child: Row(
          children: [
            Icon(
              sending ? Icons.upload_outlined : Icons.download_outlined,
              color: color,
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title.toString(),
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.titleSmall,
                  ),
                  Text(
                    details.join(' · '),
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: scheme.onSurfaceVariant),
                  ),
                ],
              ),
            ),
            Icon(
              record.succeeded ? Icons.check_circle : Icons.error_outline,
              color: color,
              size: 20,
            ),
          ],
        ),
      ),
    );
  }
}
