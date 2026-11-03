import 'lan_api.dart';

// No Flutter or file I/O. Pending permission callbacks are scoped to one intent.
final class LanController {
  final LanApi api;
  final Future<String> Function() permission;
  LanController(this.api, this.permission);
  int? _handle;
  int _epoch = 0;
  bool foreground = true, disposed = false, poisoned = false, pending = false;
  bool paired = false, verified = false;
  int nativeState = 0;
  String status = 'idle', permissionStatus = 'not-tested';
  String localOffer = '', localFingerprint = '', peerFingerprint = '';
  String _ip = '';
  int _port = 0;
  bool get active => _handle != null;
  bool get canPrepare =>
      foreground && !disposed && !poisoned && !pending && !active;
  bool get canPair =>
      foreground &&
      !disposed &&
      !poisoned &&
      !pending &&
      nativeState == 11 &&
      active;
  bool get canStart =>
      foreground &&
      !disposed &&
      !poisoned &&
      !pending &&
      paired &&
      verified &&
      nativeState == 12 &&
      active;

  Future<bool> _allowed(int epoch) async {
    String result;
    try {
      result = await permission().timeout(const Duration(seconds: 45));
    } catch (_) {
      result = 'permission-error';
    }
    if (epoch != _epoch || disposed || !foreground) return false;
    pending = false;
    permissionStatus = result;
    final allowed = [
      'granted',
      'not-required',
      'not-applicable',
      'system-mediated',
    ].contains(result);
    if (!allowed) status = 'network-permission-denied-or-unavailable';
    return allowed;
  }

  Future<void> prepare(String ip, String portText) async {
    if (!canPrepare) return;
    final int address, port;
    try {
      address = lanIpv4(ip);
      port = lanPort(portText);
    } catch (_) {
      status = 'invalid-local-address-or-port';
      return;
    }
    final epoch = ++_epoch;
    pending = true;
    status = 'checking-network-permission';
    if (!await _allowed(epoch)) return;
    try {
      if (api.version() != 1) {
        poisoned = true;
        status = 'lan-abi-mismatch';
        return;
      }
      final handle = api.prepare(address, port);
      if (handle <= 0) {
        poisoned = true; // Do not adopt an unknown task after hot restart.
        status = 'prepare-rejected-restart-required';
        return;
      }
      _handle = handle;
      _ip = ip;
      _port = port;
      paired = verified = false;
      localOffer = localFingerprint = peerFingerprint = '';
      nativeState = 10;
      status = 'preparing-ephemeral-identity';
    } catch (_) {
      poisoned = true;
      status = 'native-unavailable-restart-required';
    }
  }

  List<int> _publicBytes(int handle, int kind) {
    final length = api.read(handle, kind, 0xffffffff);
    if (length <= 0 ||
        length > lanCertificateLimit ||
        (kind != 0 && length != 32)) {
      throw StateError('invalid-public-length');
    }
    final bytes = <int>[];
    for (var index = 0; index < length; index++) {
      final byte = api.read(handle, kind, index);
      if (byte < 0 || byte > 255) throw StateError('invalid-public-byte');
      bytes.add(byte);
    }
    return bytes;
  }

  void pair(String offer, {required bool sending}) {
    if (!canPair) return;
    final LanOffer peer;
    try {
      peer = LanOffer.parse(offer);
    } catch (_) {
      status = 'invalid-peer-offer';
      return;
    }
    final handle = _handle!;
    try {
      for (var i = 0; i < peer.certificate.length; i++) {
        if (api.push(handle, i, peer.certificate[i]) != 0)
          throw StateError('staging-rejected');
      }
      if (api.configure(handle, lanIpv4(peer.ip), peer.port, sending ? 1 : 0) !=
          12) {
        throw StateError('pairing-rejected');
      }
      peerFingerprint = hexBytes(_publicBytes(handle, 2));
      paired = true;
      verified = false;
      nativeState = 12;
      status = 'compare-full-peer-fingerprint-on-other-device';
    } catch (_) {
      // Staging cannot be reused after a partial import. Retire the entire identity.
      cancel();
      status = 'pairing-rejected-prepare-new-identity';
    }
  }

  void setVerified(bool value) {
    if (paired && nativeState == 12 && !disposed && foreground && !pending)
      verified = value;
  }

  Future<void> start() async {
    if (!canStart) return;
    final handle = _handle;
    final epoch = ++_epoch;
    pending = true;
    if (!await _allowed(epoch)) return;
    if (handle == null || handle != _handle || !canStart) return;
    try {
      if (api.start(handle) != 13) {
        cancel();
        status = 'start-rejected';
      } else {
        nativeState = 13;
        verified = false;
        status = 'starting-synthetic-session';
      }
    } catch (_) {
      cancel();
      poisoned = true;
      status = 'native-error-restart-required';
    }
  }

  void tick() {
    final handle = _handle;
    if (handle == null) return;
    try {
      final result = api.poll(handle);
      nativeState = result;
      if (result >= 10) {
        if (result == 11 && localOffer.isEmpty) {
          localOffer = LanOffer(_ip, _port, _publicBytes(handle, 0)).encode();
          localFingerprint = hexBytes(_publicBytes(handle, 1));
        }
        status = switch (result) {
          10 => 'preparing-ephemeral-identity',
          11 => 'exchange-public-offers',
          12 => 'compare-full-peer-fingerprint-on-other-device',
          13 => 'starting-synthetic-session',
          14 => 'authenticating-direct-peer',
          15 => 'synthetic-transfer-and-drain',
          16 => 'listening-start-sender-on-other-device',
          _ => throw StateError('unknown-native-state'),
        };
        return;
      }
      final released = api.release(handle);
      status = switch (result) {
        1 => 'synthetic-session-passed-not-file-save',
        -2 => 'cancelled',
        -7 => 'direct-connection-failed-no-relay',
        -8 => 'identity-or-session-expired',
        -9 => 'synthetic-content-mismatch',
        _ => 'experiment-failed',
      };
      paired = verified = false;
      ++_epoch;
      pending = false;
      if (released == -4) {
        status += '-releasing';
        return;
      }
      _handle = null;
      localOffer = localFingerprint = peerFingerprint = '';
      if (released != result || ![1, -1, -2, -7, -8, -9].contains(result)) {
        poisoned = true;
        status = 'invalid-native-state-restart-required';
      }
    } catch (_) {
      poisoned = true;
      status = 'native-error-restart-required';
      // Retain ownership, request cancellation, never assume the worker exited.
      try {
        api.cancel(handle);
      } catch (_) {
        /* No unload or restart. */
      }
    }
  }

  void cancel() {
    ++_epoch;
    pending = false;
    verified = false;
    final handle = _handle;
    if (handle == null) {
      status = 'cancelled';
      return;
    }
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
  }

  void dispose() {
    disposed = true;
    cancel();
  }
}
