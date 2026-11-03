import 'dart:convert';

import '../lib/lan_api.dart';
import '../lib/lan_controller.dart';

Future<void> main(List<String> args) async {
  if (args.length != 1) throw ArgumentError('native-library-required');
  final api = NativeLan(libraryPath: args.single);
  final c = LanController(api, () async => 'not-required');
  try {
    await c.prepare('127.0.0.1', '42424');
    final end = DateTime.now().add(const Duration(seconds: 10));
    while (c.active && c.localOffer.isEmpty) {
      c.tick();
      if (DateTime.now().isAfter(end)) throw StateError('prepare-timeout');
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    if (c.localFingerprint.length != 64 ||
        LanOffer.parse(c.localOffer).certificate.isEmpty) {
      throw StateError('public-identity-missing');
    }
    // No peer and no local confirmation: this test must not start any socket.
    await c.start();
    if (c.nativeState != 11) throw StateError('unpaired-start');
  } finally {
    c.cancel();
    final end = DateTime.now().add(const Duration(seconds: 15));
    while (c.active) {
      c.tick();
      if (DateTime.now().isAfter(end)) throw StateError('release-timeout');
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
  }
  if (c.poisoned) throw StateError('controller-poisoned');
  print(
    jsonEncode({
      'scope': 'actual-dart-lan-abi-identity-cancel-release',
      'passed': true,
      'network_transfer_tested': false,
      'physical_device': false,
      'identity_logged': false,
    }),
  );
}
