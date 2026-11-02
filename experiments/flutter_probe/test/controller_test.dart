import 'package:flutter_test/flutter_test.dart';

import '../lib/controller.dart';
import '../lib/native_api.dart';

class FakeApi implements ProbeApi {
  int code = 0, releaseCode = -4, starts = 0, cancels = 0, abi = 0x10000;
  @override
  int version() => abi;
  @override
  int start() {
    starts++;
    return 1;
  }

  @override
  int poll(int handle) => code;
  @override
  int cancel(int handle) {
    cancels++;
    code = -2;
    return code;
  }

  @override
  int release(int handle) => releaseCode;
}

void main() {
  test('terminal status cannot permit restart before release', () {
    final api = FakeApi();
    final c = ProbeController(api)..start();
    api.code = 1;
    c.tick();
    c.start();
    expect(c.active, true);
    expect(api.starts, 1);
    api.releaseCode = 1;
    c.tick();
    expect(c.active, false);
    expect(c.canStart, true);
  });
  test('background cancels and resume never restarts', () {
    final api = FakeApi();
    final c = ProbeController(api)..start();
    c.setForeground(false);
    expect(api.cancels, 1);
    c.setForeground(true);
    expect(api.starts, 1);
    api.releaseCode = -2;
    c.tick();
    expect(c.active, false);
  });
  test('dispose cancels but continues to own unexited worker', () {
    final api = FakeApi();
    final c = ProbeController(api)
      ..start()
      ..dispose();
    expect(c.active, true);
    expect(c.canStart, false);
    api.releaseCode = -2;
    c.tick();
    expect(c.active, false);
    c.start();
    expect(api.starts, 1);
  });
  test('wrong ABI fails before start', () {
    final api = FakeApi()..abi = 2;
    final c = ProbeController(api)..start();
    expect(c.poisoned, true);
    expect(api.starts, 0);
  });
  test('invalid terminal or mismatched release poisons controller', () {
    final api = FakeApi();
    final c = ProbeController(api)..start();
    api.code = -3;
    api.releaseCode = -3;
    c.tick();
    expect(c.poisoned, true);
  });
}
