/// W7.3 (batch W hand-off §4.7 item 3, 2026-11-22): keep the screen on
/// while any transfer is in progress — the Dart half of the narrow
/// `dev.ak/screen` platform channel (`MainActivity` owns the Kotlin
/// half: `on` adds `FLAG_KEEP_SCREEN_ON`, `off` clears it; no
/// third-party plugin).
///
/// The judgment is SHARED with the W1 state machine:
/// [SimpleTransferService.isTransferInProgress] (a send between entry
/// and settle, or a live progress fold) is the one fact this coordinator
/// keys on — [SimpleTransferService.transferActivity] fans out its edge
/// transitions, and THIS class turns them into channel calls:
///
/// * any transfer enters in-progress → `on`;
/// * the LAST one ends (completed / failed / cancelled — the in-progress
///   set drains) → `off`;
/// * non-Android platforms (Windows/Linux) call NOTHING (`isAndroid`
///   decides; the channel exists only on Android).
///
/// The channel is best-effort by design: a missing handler (a test
/// shell, a desktop embedding) or a failed invocation is swallowed and
/// never propagates into the transfer state machine — a screen flag must
/// not break a transfer. [dispose] fires a final `off` when the flag was
/// held, so a disposed service cannot keep the screen awake forever.
library;

import 'dart:async' show StreamSubscription;

import 'dart:io' show Platform;

import 'package:flutter/services.dart' show MethodChannel;

import 'simple_transfer_service.dart' as task_engine;

/// Turns the engine service's "any transfer in progress" edges into
/// `dev.ak/screen` channel calls (Android only).
final class ScreenKeepAwake {
  ScreenKeepAwake({
    required this.service,
    bool Function()? isAndroid,
  }) : _isAndroid = isAndroid ?? _platformIsAndroid;

  /// The platform gate — injectable so tests can drive the non-Android
  /// branch deterministically (production reads `Platform.isAndroid`).
  static bool _platformIsAndroid() => Platform.isAndroid;

  /// The narrow channel (W7.3): `on` adds `FLAG_KEEP_SCREEN_ON` in
  /// `MainActivity`, `off` clears it. Const by name — tests intercept
  /// it through the test binding's mock method-call handler.
  static const MethodChannel _channel = MethodChannel('dev.ak/screen');

  /// The engine service whose in-progress judgment this follows
  /// (borrowed — its lifecycle is the owner's).
  final task_engine.SimpleTransferService service;

  final bool Function() _isAndroid;

  StreamSubscription<bool>? _subscription;

  /// Whether the last channel call was `on` (the held state).
  bool _on = false;

  /// The observable flag state (tests assert on it; the channel mock
  /// asserts the calls themselves).
  bool get isOn => _on;

  /// Start following the service's transfer activity (idempotent). Call
  /// after [task_engine.SimpleTransferService.start]; the subscription's
  /// FIRST event is the current snapshot, so an already-in-progress
  /// transfer turns the flag on immediately.
  void start() {
    _subscription ??= service.transferActivity.listen(_apply);
  }

  /// Stop following and release a held flag (best-effort `off`). Never
  /// throws; safe to call without [start].
  void dispose() {
    _subscription?.cancel();
    _subscription = null;
    if (_on) {
      _set(false);
    }
  }

  void _apply(bool active) {
    if (!_isAndroid()) {
      // W7.3: Windows/Linux are a deliberate no-op — the channel is
      // Android-only and must never be invoked elsewhere.
      return;
    }
    _set(active);
  }

  void _set(bool on) {
    if (on == _on) {
      return; // no edge — the channel only sees transitions
    }
    _on = on;
    _channel.invokeMethod<void>(on ? 'on' : 'off').catchError((Object _) {
      // Best-effort by contract: a missing handler or a failed call
      // never reaches the transfer state machine. The state still
      // flipped — a retry storm helps nobody.
    });
  }
}
