/// CR-P2-05 (review 2026-11-11, item 2): the app-lifecycle bridge onto
/// the engine service's adaptive event pump.
///
/// Before this, `SimpleTransferService`'s event pump ticked at a fixed
/// 50 ms forever — backgrounded, lock-screened, overnight. This observer
/// is the widgets-binding half of the fix; the pacing rules themselves
/// live on the engine service (`setAppBackgrounded`,
/// `effectiveEventPollInterval`, `hasActivity`) so they stay unit-
/// testable without a widget tree:
///
/// * `paused`/`hidden` → backgrounded: the pump drops to the 1 s idle
///   cadence while a task/pairing is in flight, and pauses ENTIRELY when
///   nothing is (a hidden app with no live row has nothing to poll for —
///   the data plane runs in the engine, not in Dart).
/// * `resumed`/`inactive` (visible again) → foreground: the connection
///   and task state refresh IMMEDIATELY (one synchronous drain of the
///   events that queued while hidden) and the pump re-arms at the
///   activity-decided cadence.
///
/// The frozen UI contract (`lib/ui/service/simple_transfer_service.dart`)
/// carries no lifecycle knob, so the app shell reaches the ENGINE service
/// through `FfiSimpleTransferService.engine` and hands it here; dev fakes
/// and test doubles that are not the Ffi adapter simply get no observer.
///
/// `detached` is ignored on purpose: the process is going away and the
/// honest teardown (waiters invalidated, pump halted, borrowers before
/// the native owner) already belongs to `dispose()`.
library;

import 'package:flutter/widgets.dart'
    show AppLifecycleState, WidgetsBindingObserver;

import 'simple_transfer_service.dart' as task_engine;

/// Forwards `AppLifecycleState` changes onto
/// [task_engine.SimpleTransferService.setAppBackgrounded].
class TransferServiceLifecycleObserver with WidgetsBindingObserver {
  TransferServiceLifecycleObserver({required this.service});

  /// The engine service whose pump is paced (borrowed — its lifecycle is
  /// the owner's, this observer only flips the background flag).
  final task_engine.SimpleTransferService service;

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
      case AppLifecycleState.inactive:
        // Foreground (again) — `inactive` is still the visible regime
        // (a system dialog, a notification shade): refresh now and keep
        // the activity-decided cadence.
        service.setAppBackgrounded(false);
      case AppLifecycleState.paused:
      case AppLifecycleState.hidden:
        // Backgrounded — slow heartbeat while anything runs, no timer
        // at all once quiet.
        service.setAppBackgrounded(true);
      case AppLifecycleState.detached:
        break; // teardown belongs to dispose()
    }
  }
}
