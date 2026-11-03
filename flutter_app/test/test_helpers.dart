import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Pumps [child] inside a MaterialApp/Scaffold on a tall surface, so the
/// long receive/send forms render fully without manual scrolling in
/// widget tests.
Future<void> pumpPage(WidgetTester tester, Widget child) async {
  await tester.binding.setSurfaceSize(const Size(1280, 1800));
  addTearDown(() => tester.binding.setSurfaceSize(null));
  await tester.pumpWidget(MaterialApp(home: Scaffold(body: child)));
}

/// Enters [text] into the field with [key] and pumps one frame.
Future<void> enterField(
  WidgetTester tester,
  Key key,
  String text,
) async {
  await tester.enterText(find.byKey(key), text);
  await tester.pump();
}
