// LIVE GUI helper (deployed desktop app): create a code, dump it to a
// temp file, STAY ALIVE so an external joiner can redeem it. X4 (batch
// X): the new home has no role sheet — this uses the receive SECONDARY
// entry (生成接收码), which mints a receiver-role code without any file
// pick, so the external joiner redeems it as the sender
// (`ak-cli task join-send <code> <file>`).
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/material.dart' show Text, ValueKey, EditableText;
import 'package:integration_test/integration_test.dart';
import 'package:ak_sending_app/main.dart' as app;
import 'dart:io';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('live: GUI creates a code, dump it, STAY ALIVE for the joiner', (tester) async {
    app.main();
    await tester.pumpAndSettle(const Duration(seconds: 3));
    await tester.tap(find.byKey(const ValueKey('home_receive_create_code_button')));
    await tester.pump();
    final deadline = DateTime.now().add(const Duration(seconds: 30));
    String? code;
    while (DateTime.now().isBefore(deadline)) {
      await tester.pump(const Duration(milliseconds: 200));
      for (final w in tester.widgetList(find.textContaining('AKI1-'))) {
        String? t;
        if (w is Text) t = w.data ?? w.textSpan?.toPlainText();
        if (w is EditableText) t = w.controller.text;
        if (t != null && t.startsWith('AKI1-')) { code = t; break; }
      }
      if (code != null) break;
    }
    // ignore: avoid_print
    print('GUI_CODE: $code');
    File(r'C:\Users\Administrator\AppData\Local\Temp\gui-code.txt').writeAsStringSync(code ?? 'NONE');
    if (code == null) return;
    // Stay alive 40s so an external joiner can redeem.
    final stayUntil = DateTime.now().add(const Duration(seconds: 200));
    while (DateTime.now().isBefore(stayUntil)) {
      await tester.pump(const Duration(milliseconds: 500));
    }
  }, timeout: const Timeout(Duration(minutes: 2)));
}
