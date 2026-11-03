import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../lib/lan_api.dart';
import '../lib/lan_controller.dart';
import '../lib/lan_page.dart';

class FakeLan implements LanApi {
  int code = 11,
      releaseCode = -4,
      starts = 0,
      prepares = 0,
      cancels = 0,
      abi = 1;
  int staged = 0;
  @override
  int version() => abi;
  @override
  int prepare(int ip, int port) {
    prepares++;
    code = 11;
    return prepares;
  }

  @override
  int poll(int handle) => code;
  @override
  int read(int handle, int kind, int index) {
    if (index == 0xffffffff) return kind == 0 ? 2 : 32;
    return kind == 2 ? 7 : 5;
  }

  @override
  int push(int handle, int index, int byte) {
    staged++;
    return 0;
  }

  @override
  int configure(int handle, int ip, int port, int role) {
    code = 12;
    return 12;
  }

  @override
  int start(int handle) {
    starts++;
    code = 13;
    return 13;
  }

  @override
  int cancel(int handle) {
    cancels++;
    code = -2;
    return -2;
  }

  @override
  int release(int handle) => releaseCode;
}

const offer = 'AKL1|192.168.1.2|42424|00ff';
Future<LanController> paired(
  FakeLan api, {
  Future<String> Function()? permission,
}) async {
  final c = LanController(api, permission ?? () async => 'granted');
  await c.prepare('192.168.1.1', '42424');
  c.tick();
  c.pair(offer, sending: false);
  return c;
}

void main() {
  test('exact IPv4, bounded port and public offer roundtrip', () {
    expect(lanIpv4('127.0.0.1'), 0x7f000001);
    for (final ip in ['10.0.0.1', '172.16.1.1', '172.31.1.1', '192.168.1.1']) {
      expect(lanIpv4(ip), isPositive);
    }
    expect(LanOffer.parse(offer).encode(), offer);
    final data = LanOffer.parse(offer).certificate;
    expect(() => data[0] = 10, throwsUnsupportedError);
    expect(
      LanOffer.parse('AKL1|10.0.0.1|1024|${'00' * 4096}').certificate.length,
      4096,
    );
  });
  test('ambiguous, public, malformed and oversized offers fail', () {
    for (final ip in [
      '0.0.0.0',
      '8.8.8.8',
      'localhost',
      '127.0.0.2',
      '::1',
      '010.0.0.1',
      '172.32.0.1',
      '169.254.1.1',
      '10.0.0.256',
    ]) {
      expect(() => lanIpv4(ip), throwsFormatException);
    }
    for (final value in [
      'AKL2|10.0.0.1|42424|00',
      'AKL1|10.0.0.1|0|00',
      'AKL1|10.0.0.1|65536|00',
      'AKL1|10.0.0.1|+42424|00',
      'AKL1|10.0.0.1|٤٢٤٢٤|00',
      'AKL1|10.0.0.1|42424|0',
      'AKL1|10.0.0.1|42424|',
      'AKL1|10.0.0.1|42424|00 11',
      'AKL1|10.0.0.1|42424|00|x',
      'AKL1|10.0.0.1|42424|${'00' * 4097}',
    ]) {
      expect(() => LanOffer.parse(value), throwsFormatException);
    }
  });
  test(
    'pairing is not start; explicit verification required and consumed',
    () async {
      final api = FakeLan();
      final c = await paired(api);
      expect(c.localFingerprint.length, 64);
      expect(c.peerFingerprint.length, 64);
      expect(api.starts, 0);
      await c.start();
      expect(api.starts, 0);
      c.setVerified(true);
      await c.start();
      expect(api.starts, 1);
      expect(c.verified, false);
      await c.start();
      expect(api.starts, 1);
      c.dispose();
    },
  );
  test('permission denial creates no native task', () async {
    final api = FakeLan();
    for (final result in ['denied', 'busy', 'failed', 'loopback-only']) {
      final c = LanController(api, () async => result);
      await c.prepare('192.168.1.1', '42424');
      expect(c.active, false);
      expect(api.prepares, 0);
    }
  });
  test('permission exception fails closed', () async {
    final api = FakeLan();
    final c = LanController(api, () => Future.error(StateError('fixture')));
    await c.prepare('192.168.1.1', '42424');
    expect(c.pending, false);
    expect(api.prepares, 0);
  });
  test(
    'late prepare permission cannot survive background and resume',
    () async {
      final api = FakeLan();
      final p = Completer<String>();
      final c = LanController(api, () => p.future);
      final pending = c.prepare('192.168.1.1', '42424');
      c.setForeground(false);
      c.setForeground(true);
      p.complete('granted');
      await pending;
      expect(api.prepares, 0);
      expect(c.active, false);
    },
  );
  test(
    'late start permission cannot restart after background or disposal',
    () async {
      for (final disposing in [false, true]) {
        final api = FakeLan();
        final p = Completer<String>();
        var calls = 0;
        final c = await paired(
          api,
          permission: () => ++calls == 1 ? Future.value('granted') : p.future,
        );
        c.setVerified(true);
        final pending = c.start();
        if (disposing) {
          c.dispose();
        } else {
          c.setForeground(false);
          c.setForeground(true);
        }
        p.complete('granted');
        await pending;
        expect(api.starts, 0);
        expect(api.cancels, 1);
        api.releaseCode = -2;
        c.tick();
        expect(c.active, false);
      }
    },
  );
  test('cancelled worker remains owned until release succeeds', () async {
    final api = FakeLan();
    final c = await paired(api);
    c.cancel();
    expect(c.active, true);
    expect(c.canPrepare, false);
    await c.prepare('192.168.1.1', '42424');
    expect(api.prepares, 1);
    api.releaseCode = -2;
    c.tick();
    expect(c.active, false);
    expect(c.localOffer, '');
    expect(c.verified, false);
  });
  test('invalid native terminal or ABI is not adopted as success', () async {
    final api = FakeLan()..abi = 2;
    final c = LanController(api, () async => 'granted');
    await c.prepare('192.168.1.1', '42424');
    expect(c.poisoned, true);
    expect(api.prepares, 0);
    final other = FakeLan();
    final d = await paired(other);
    other.code = -3;
    other.releaseCode = -3;
    d.tick();
    expect(d.poisoned, true);
  });
  test('malformed offer does not stage any bytes', () async {
    final api = FakeLan();
    final c = LanController(api, () async => 'granted');
    await c.prepare('192.168.1.1', '42424');
    c.tick();
    c.pair('broken', sending: false);
    expect(api.staged, 0);
    expect(c.paired, false);
    c.dispose();
  });
  test('cancelled pending permission cannot supersede a new intent', () async {
    final api = FakeLan();
    final first = Completer<String>();
    var calls = 0;
    final c = LanController(
      api,
      () => ++calls == 1 ? first.future : Future.value('granted'),
    );
    final old = c.prepare('192.168.1.1', '42424');
    c.cancel();
    await c.prepare('192.168.1.1', '42425');
    first.complete('granted');
    await old;
    expect(api.prepares, 1);
    c.dispose();
  });
  testWidgets(
    'LAN screen cannot start without importing and confirming identity',
    (tester) async {
      final api = FakeLan()..releaseCode = -2;
      await tester.pumpWidget(
        MaterialApp(
          home: LanPage(api: api, permission: () async => 'granted'),
        ),
      );
      await tester.enterText(find.byKey(const Key('lan-ip')), '192.168.1.1');
      await tester.tap(find.byKey(const Key('lan-prepare')));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));
      await tester.ensureVisible(find.byKey(const Key('lan-start')));
      final start = tester.widget<FilledButton>(
        find.byKey(const Key('lan-start')),
      );
      expect(start.onPressed, isNull);
      expect(api.starts, 0);
      await tester.pumpWidget(const SizedBox());
      await tester.pump();
    },
  );
}
