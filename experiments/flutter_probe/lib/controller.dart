import 'native_api.dart';

// Pure Dart state machine. Terminal status never implies that a worker exited.
final class ProbeController {
  final ProbeApi api;
  int? _handle;
  bool foreground = true;
  bool disposed = false;
  bool poisoned = false;
  String status = 'idle';
  ProbeController(this.api);
  bool get active => _handle != null;
  bool get canStart => foreground && !disposed && !poisoned && !active;

  void start() {
    if (!canStart) return;
    try {
      if (api.version() != 0x00010000) {
        poisoned = true;
        status = 'abi-mismatch';
        return;
      }
      final handle = api.start();
      if (handle <= 0) {
        // Busy without an owned handle can follow hot restart: do not adopt it.
        poisoned = true;
        status = 'start-rejected-restart-required';
        return;
      }
      _handle = handle;
      status = 'running';
    } catch (_) {
      poisoned = true;
      status = 'native-unavailable';
    }
  }

  void tick() {
    final handle = _handle;
    if (handle == null) return;
    try {
      final result = api.poll(handle);
      if (result == 0) return;
      final released = api.release(handle);
      status = switch (result) {
        1 => 'loopback-passed-not-lan',
        -2 => 'cancelled',
        _ => 'probe-failed',
      };
      if (released == -4) {
        status += '-releasing';
        return;
      }
      _handle = null;
      if (released != result || ![1, -1, -2].contains(result)) {
        poisoned = true;
        status = 'invalid-native-state-restart-required';
      }
    } catch (_) {
      poisoned = true;
      status = 'native-error-restart-required';
      // Keep ownership; no new task and no library unload after unknown failure.
    }
  }

  void cancel() {
    final handle = _handle;
    if (handle == null) return;
    try {
      api.cancel(handle);
    } catch (_) {
      poisoned = true;
    }
    tick();
  }

  void setForeground(bool value) {
    foreground = value;
    if (!value) cancel();
    // Resuming never starts another task automatically.
  }

  void dispose() {
    disposed = true;
    cancel();
  }
}
