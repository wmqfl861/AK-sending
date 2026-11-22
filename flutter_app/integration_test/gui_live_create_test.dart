// LIVE GUI test against the deployed desktop app: drive the REAL app
// process through a real code creation. Runs via flutter test -d windows
// with the real bootstrap (dart-defines come from this command line).
// X4 (batch X): the new home has no role sheet — the receive SECONDARY
// entry (生成接收码) mints a receiver-role code without any file pick;
// an external peer redeems it as the sender (`task join-send`).
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:ak_sending_app/main.dart' as app;
import 'package:flutter/material.dart' show Text, ValueKey;

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('live: home -> create receive code -> code appears', (tester) async {
    app.main();
    await tester.pumpAndSettle(const Duration(seconds: 3));

    // Home: the receive secondary entry (X4).
    await tester.tap(find.byKey(const ValueKey('home_receive_create_code_button')));
    await tester.pump();

    // The pairing flow now dials the real registry; wait up to 40s for
    // the AKI1 code or the failure text.
    String? code;
    Object? failure;
    final deadline = DateTime.now().add(const Duration(seconds: 40));
    while (DateTime.now().isBefore(deadline)) {
      await tester.pump(const Duration(milliseconds: 200));
      final codeFinder = find.textContaining('AKI1-');
      if (tester.any(codeFinder)) {
        for (final w in tester.widgetList(codeFinder)) {
          if (w is Text) {
            final t = w.data ?? w.textSpan?.toPlainText();
            if (t != null && t.startsWith('AKI1-')) { code = t; break; }
          }
        }
        if (code != null) break;
      }
      final errFinder = find.textContaining('生成配对码失败');
      if (tester.any(errFinder)) {
        for (final w in tester.widgetList(errFinder)) {
          if (w is Text) {
            final t = w.data ?? w.textSpan?.toPlainText();
            if (t != null) { failure = t; break; }
          }
        }
        if (failure != null) break;
      }
    }
    // ignore: avoid_print
    print(code != null ? 'LIVE CREATE OK: $code' : 'LIVE CREATE FAILED: $failure');
    if (code == null) {
      // Dump every visible Text for the stuck-state diagnosis.
      final texts = <String>[];
      for (final w in tester.widgetList<Text>(find.byType(Text))) {
        final t = w.data ?? w.textSpan?.toPlainText();
        if (t != null && t.trim().isNotEmpty) texts.add(t);
      }
      // ignore: avoid_print
      print('VISIBLE: ${texts.join(' | ')}');
    }
    expect(code, isNotNull, reason: '$failure');
  }, timeout: const Timeout(Duration(minutes: 2)));
}
