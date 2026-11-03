import 'dart:convert';
import 'dart:io';

import '../lib/controller.dart';
import '../lib/native_api.dart';

Future<void> main(List<String> args) async {
  if (args.length != 1) throw ArgumentError('one native library path required');
  final api = NativeProbe(libraryPath: args.single);
  final controller = ProbeController(api);
  final times = <int>[];
  for (var i = 0; i < 3; i++) {
    final clock = Stopwatch()..start();
    controller.start();
    while (controller.active && clock.elapsed < const Duration(seconds: 15)) {
      controller.tick();
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    if (controller.active || controller.status != 'loopback-passed-not-lan') {
      throw StateError('native loopback failed: ${controller.status}');
    }
    times.add(clock.elapsedMilliseconds);
  }
  controller.start();
  controller.cancel();
  final clock = Stopwatch()..start();
  while (controller.active && clock.elapsed < const Duration(seconds: 15)) {
    controller.tick();
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
  if (controller.active || controller.poisoned)
    throw StateError('release failed');
  stdout.writeln(
    jsonEncode({
      'kind': 'actual-dart-ffi-loopback',
      'samples_ms': times,
      'cancel_terminal': controller.status,
      'device_acceptance': false,
    }),
  );
}
