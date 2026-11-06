/// Shared helpers for the wave-28 UI tests: app pump harness, surface
/// sizes, and the file-picker gateway fake.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show SystemChannels;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';

/// Phone width of the overflow regression (wave-25 finding).
const Size kPhoneSize = Size(390, 844);

/// Desktop-class size for the rail layout.
const Size kWideSize = Size(1280, 1800);

/// Pumps the full app shell on [size] with the fake service.
Future<LocalSimpleTransferService> pumpApp(
  WidgetTester tester, {
  Size size = kWideSize,
  SaveDirStore? store,
  FilePickGateway? picker,
}) async {
  await tester.binding.setSurfaceSize(size);
  addTearDown(() => tester.binding.setSurfaceSize(null));
  final service = LocalSimpleTransferService();
  await tester.pumpWidget(AkSendingApp(
    service: service,
    store: store ?? InMemorySaveDirStore(),
    picker: picker ?? const PluginFilePickGateway(),
  ));
  await tester.pump();
  return service;
}

/// Pumps until the fake's stream emissions / future continuations have
/// rendered: broadcast-stream delivery lands one frame after the
/// `controller.add` microtask, so knob calls need two pumps.
Future<void> pumpEvents(WidgetTester tester) async {
  await tester.pump();
  await tester.pump();
}

/// Clipboard double: an in-memory handler on the platform channel (the
/// real channel has no handler in widget tests and would hang
/// `await Clipboard.setData/getData`). Exposes the last copied text.
class MockClipboard {
  String _text = '';

  void install() {
    TestWidgetsFlutterBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') {
        final args = call.arguments;
        if (args is Map) {
          _text = args['text'] as String? ?? '';
        }
      } else if (call.method == 'Clipboard.getData') {
        return <String, Object>{'text': _text};
      }
      return null;
    });
  }

  String get text => _text;
}

/// Deterministic picker double: [filesResult] for the multi-file pick,
/// [directoryResult] for the folder pick.
class FakeFilePickGateway implements FilePickGateway {
  FakeFilePickGateway({
    this.filesResult = const <PickedEntry>[],
    this.directoryResult,
  });

  List<PickedEntry> filesResult;
  PickedEntry? directoryResult;

  int pickFilesCalls = 0;
  int pickDirectoryCalls = 0;

  @override
  Future<List<PickedEntry>> pickFiles() async {
    pickFilesCalls += 1;
    return filesResult;
  }

  @override
  Future<PickedEntry?> pickDirectory() async {
    pickDirectoryCalls += 1;
    return directoryResult;
  }
}

/// Two ready file entries used by the send-flow tests.
const List<PickedEntry> kTwoFiles = [
  PickedEntry(
      path: 'C:\\docs\\照片.zip', name: '照片.zip', isDirectory: false,
      sizeBytes: 1536),
  PickedEntry(
      path: 'C:\\docs\\报告.pdf', name: '报告.pdf', isDirectory: false,
      sizeBytes: 512 * 1024),
];
