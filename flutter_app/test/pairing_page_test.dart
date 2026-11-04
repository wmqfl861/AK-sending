import 'package:ak_sending_app/ui/pages/pairing_page.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';
import 'package:flutter/material.dart' show TextField, ValueKey;
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

/// Wave-14 pairing-management page tests over [FakeTransferService]:
/// identity card, peer list (short id + expand), empty state, add-form
/// validation (bad hex / prefix / blank name), add-then-refresh linkage,
/// delete confirmation and the not-found race, corrupt-book inline error.
void main() {
  PeerEntryUi peer(String fingerprint, {String name = '', int? pairedMs}) =>
      PeerEntryUi(
        fingerprint: fingerprint,
        name: name,
        pairedAt: DateTime.fromMillisecondsSinceEpoch(pairedMs ?? 1730000000000),
      );

  testWidgets('renders the identity card (fingerprint chip, large pairing '
      'code) and the empty-state guidance when no peers', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    expect(find.text('DESKTOP-AK1'), findsOneWidget);
    // Identity chip shows the short id of the fake's 'ab'*32 fingerprint.
    expect(find.text('指纹 abababab'), findsOneWidget);
    expect(find.byKey(const ValueKey('pairing_pairing_code')), findsOneWidget);
    expect(find.text('123-456'), findsOneWidget);
    // Empty book: honest guidance, no spinner, no error row.
    expect(find.text('还没有配对设备'), findsOneWidget);
    expect(find.textContaining('会自动记入配对簿'), findsOneWidget);
    expect(find.byKey(const ValueKey('pairing_loading')), findsNothing);
    expect(find.byKey(const ValueKey('pairing_list_error')), findsNothing);
    expect(find.textContaining('已配对设备（0 台）'), findsOneWidget);
  });

  testWidgets('renders the peer list: names (with 未命名 fallback), short '
      'fingerprints, pairing time, sorted by fingerprint', (tester) async {
    final fake = FakeTransferService(peers: [
      peer('cd' * 32), // nameless → 未命名设备 fallback
      peer(kPeerFingerprint, name: '笔记本'),
    ]);
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    expect(find.textContaining('已配对设备（2 台）'), findsOneWidget);
    expect(find.text('笔记本'), findsOneWidget);
    expect(find.text('未命名设备'), findsOneWidget);
    expect(find.text('指纹 6c310412'), findsOneWidget);
    expect(find.text('指纹 cdcdcdcd'), findsOneWidget);
    expect(find.textContaining('配对于'), findsNWidgets(2));
    // listPeers sorts by fingerprint: 6c31... before cdcd....
    expect(
      tester.getTopLeft(find.text('指纹 6c310412')).dy,
      lessThan(tester.getTopLeft(find.text('指纹 cdcdcdcd')).dy),
    );
  });

  testWidgets('tapping a peer fingerprint chip expands the full 64-hex form',
      (tester) async {
    final fake = FakeTransferService(peers: [peer(kPeerFingerprint, name: '笔记本')]);
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    expect(find.text(kPeerFingerprint), findsNothing);
    await tester.tap(find.text('指纹 6c310412'));
    await tester.pumpAndSettle();
    expect(find.text(kPeerFingerprint), findsOneWidget);
  });

  testWidgets('add form rejects non-hex input inline without calling the '
      'service', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    await enterField(tester, const ValueKey('pairing_add_fp'), 'zz-not-hex!');
    await tester.tap(find.byKey(const ValueKey('pairing_add_button')));
    await tester.pump();

    expect(find.byKey(const ValueKey('pairing_form_error')), findsOneWidget);
    expect(find.text('指纹须为 64 位十六进制字符。'), findsOneWidget);
    expect(fake.addPeerCalls, 0);
  });

  testWidgets('add form rejects an 8–63 hex prefix with the needs-full '
      'fingerprint hint', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    await enterField(tester, const ValueKey('pairing_add_fp'), '6c310412');
    await tester.tap(find.byKey(const ValueKey('pairing_add_button')));
    await tester.pump();

    expect(find.text('指纹前缀不能用于添加：请粘贴完整 64 位指纹。'), findsOneWidget);
    expect(fake.addPeerCalls, 0);

    // A 63-hex prefix gets the same hint.
    await enterField(
        tester, const ValueKey('pairing_add_fp'), 'a' * 63);
    await tester.tap(find.byKey(const ValueKey('pairing_add_button')));
    await tester.pump();
    expect(fake.addPeerCalls, 0);
  });

  testWidgets('adding a valid fingerprint calls the service, refreshes the '
      'list + identity and clears the form', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();
    final identityRefreshesAtStart = fake.refreshIdentityCalls;

    await enterField(tester, const ValueKey('pairing_add_fp'), kPeerFingerprint);
    await enterField(tester, const ValueKey('pairing_add_name'), '笔记本');
    await tester.tap(find.byKey(const ValueKey('pairing_add_button')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(fake.addPeerCalls, 1);
    expect(fake.lastAddPeerFingerprint, kPeerFingerprint);
    expect(fake.lastAddPeerName, '笔记本');
    // New tile rendered after the reload.
    expect(find.text('笔记本'), findsOneWidget);
    expect(find.text('指纹 6c310412'), findsOneWidget);
    expect(find.textContaining('已配对设备（1 台）'), findsOneWidget);
    expect(find.textContaining('已添加配对设备'), findsOneWidget);
    // Home-card linkage: the identity was refreshed after the mutation.
    expect(fake.refreshIdentityCalls,
        greaterThanOrEqualTo(identityRefreshesAtStart + 1));
    // Form cleared for the next entry.
    final field =
        tester.widget<TextField>(find.byKey(const ValueKey('pairing_add_fp')));
    expect(field.controller!.text, '');
  });

  testWidgets('adding with a blank name pairs the peer unnamed '
      '(未命名设备 tile)', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    await enterField(tester, const ValueKey('pairing_add_fp'), kPeerFingerprint);
    await tester.tap(find.byKey(const ValueKey('pairing_add_button')));
    await tester.pump();
    await tester.pumpAndSettle();

    expect(fake.addPeerCalls, 1);
    expect(fake.lastAddPeerName, '');
    expect(fake.peers.single.name, '(unnamed)');
    expect(find.text('未命名设备'), findsOneWidget);
    expect(find.text('指纹 6c310412'), findsOneWidget);
  });

  testWidgets('delete asks for confirmation: cancel keeps the peer, confirm '
      'removes it and refreshes list + identity', (tester) async {
    final fake = FakeTransferService(peers: [peer(kPeerFingerprint, name: '笔记本')]);
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();
    final removeKey = ValueKey('pairing_remove_$kPeerFingerprint');

    // Cancel: nothing removed.
    await tester.ensureVisible(find.byKey(removeKey));
    await tester.tap(find.byKey(removeKey));
    await tester.pumpAndSettle();
    expect(find.text('移除配对设备'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('pairing_remove_cancel')));
    await tester.pumpAndSettle();
    expect(fake.removePeerCalls, 0);
    expect(find.text('笔记本'), findsOneWidget);

    // Confirm: removed, list reloads to the empty state, identity follows.
    await tester.ensureVisible(find.byKey(removeKey));
    await tester.tap(find.byKey(removeKey));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('pairing_remove_confirm')));
    await tester.pumpAndSettle();
    expect(fake.removePeerCalls, 1);
    expect(fake.lastRemovePeerFingerprint, kPeerFingerprint);
    expect(find.text('笔记本'), findsNothing);
    expect(find.text('还没有配对设备'), findsOneWidget);
    expect(fake.refreshIdentityCalls, greaterThanOrEqualTo(2));
  });

  testWidgets('removePeer returning false (peer vanished behind the page) '
      'surfaces the honest inline message', (tester) async {
    final fake = FakeTransferService(peers: [peer(kPeerFingerprint, name: '笔记本')]);
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();
    final removeKey = ValueKey('pairing_remove_$kPeerFingerprint');

    // The book changes elsewhere after our list loaded.
    fake.peers.clear();
    await tester.ensureVisible(find.byKey(removeKey));
    await tester.tap(find.byKey(removeKey));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('pairing_remove_confirm')));
    await tester.pumpAndSettle();

    expect(fake.removePeerCalls, 1);
    expect(find.byKey(const ValueKey('pairing_action_error')), findsOneWidget);
    expect(find.textContaining('已不在配对簿中'), findsOneWidget);
    // The reload still lands on the (now honest) empty state.
    expect(find.text('还没有配对设备'), findsOneWidget);
  });

  testWidgets('a corrupt pairing book renders the inline error row, never a '
      'fake empty state', (tester) async {
    final fake = FakeTransferService(
      listPeersError: StateError('cannot load peer book: peers.book 损坏'),
    );
    await pumpPage(tester, PairingPage(service: fake));
    await tester.pump();

    expect(find.byKey(const ValueKey('pairing_list_error')), findsOneWidget);
    expect(find.textContaining('配对簿读取失败'), findsOneWidget);
    expect(find.textContaining('cannot load peer book'), findsOneWidget);
    expect(find.text('还没有配对设备'), findsNothing);
  });
}
