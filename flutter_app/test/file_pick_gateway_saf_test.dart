/// Batch Z: the Android folder SEND of the production gateway over the
/// `dev.ak/storage` channel — pick, stage, progress, cancel, the refusal
/// dialog, release, the stale cleanup and the compile-time switch.
///
/// The channel is the test binding's mock method-call handler: every call
/// is recorded, each method is scripted per test, and `stagingProgress`
/// events arrive the way the platform sends them (`handlePlatformMessage`
/// on the same channel). A method nobody scripted throws
/// `MissingPluginException`, like a channel without a native half. The
/// Android branch is reached through the gateway's own seams on a desktop
/// host; the root navigator the dialogs need is a plain `MaterialApp`.
///
/// Nothing here touches a device. The channel's shapes are the frozen
/// contract's (`docs/HANDOFF-BATCH-Z-2026-11-24.zh-CN.md` §3.1), so these
/// tests pin the Dart side of that contract and say nothing about the
/// stager itself.
library;

import 'dart:async' show Completer, FutureOr;

import 'package:file_picker/file_picker.dart' as fp;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/saf_send_dialogs.dart';
import 'package:ak_sending_app/ui/service/ui_platform.dart';

// -- frozen widget keys ------------------------------------------------------

const Key _progressDialog = ValueKey('staging_progress_dialog');
const Key _progressText = ValueKey('staging_progress_text');
const Key _cancelButton = ValueKey('staging_cancel_button');
const Key _errorDialog = ValueKey('staging_error_dialog');
const Key _errorTitle = ValueKey('staging_error_title');
const Key _errorBody = ValueKey('staging_error_body');
const Key _retryButton = ValueKey('staging_error_retry_button');
const Key _errorCancelButton = ValueKey('staging_error_cancel_button');
const Key _unsupportedBody = ValueKey('folder_send_unsupported_body');
const Key _unsupportedOk = ValueKey('folder_send_unsupported_ok_button');

// -- fixtures ----------------------------------------------------------------

const MethodChannel _storage = MethodChannel('dev.ak/storage');

/// What the system tree picker hands out for the folder — a tree URI,
/// never a path.
const String _treeUri =
    'content://com.android.externalstorage.documents/tree/primary%3ADocuments';
const String _folderName = '相册';

const Map<String, Object?> _selected = {
  'status': 'selected',
  'treeUri': _treeUri,
  'displayName': _folderName,
};

/// The directories in front of a staged copy on the phone — the base
/// the platform answers before `<stageId>/payload/<name>`.
const String _stagingBase =
    '/data/user/0/dev.ak.ak_sending_app/no_backup/ak-saf-staging';

/// Where the platform stager puts the copy of [_treeUri].
String _stagedRoot(String stageId) =>
    '$_stagingBase/$stageId/payload/$_folderName';

/// A VALID ok answer of `stageSafTree` for [stageId]: the copy of the
/// folder [name] sits under that id, with its three counts.
Map<String, Object?> _okFor(String stageId, [String name = _folderName]) => {
      'status': 'ok',
      'stageId': stageId,
      'path': '$_stagingBase/$stageId/payload/$name',
      'displayName': name,
      'fileCount': 7,
      'byteCount': 1536,
      'emptyDirectoryCount': 2,
    };

/// The ok answer of a `stageSafTree` [call]: the copy sits under the id
/// the call carried.
Map<String, Object?> _ok(MethodCall call) => _okFor(_stageIdOf(call));

/// Another staging's id — well-formed, and never the one asked for.
String _otherId(String stageId) => stageId.startsWith('0')
    ? '1${stageId.substring(1)}'
    : '0${stageId.substring(1)}';

const String _filePath1 =
    '/data/user/0/dev.ak.ak_sending_app/cache/file_picker/照片.zip';
const String _filePath2 =
    '/data/user/0/dev.ak.ak_sending_app/cache/file_picker/报告.pdf';

List<fp.PlatformFile> get _twoPlatformFiles => [
      _PlatformFile('照片.zip', _filePath1, 1536),
      _PlatformFile('报告.pdf', _filePath2, 512 * 1024),
    ];

/// The refusal table of the contract: every code with every reason token
/// it can carry.
const List<(String, List<String>)> _refusals = [
  (
    'SAF_PERMISSION_DENIED',
    ['no-persisted-grant', 'grant-limit', 'provider-security', 'grant-lookup'],
  ),
  ('SAF_PROVIDER_UNTRUSTED', ['authority']),
  (
    'SAF_ENUMERATION_FAILED',
    ['query', 'duplicate-id', 'overflow', 'too-many-entries'],
  ),
  (
    'SAF_UNSUPPORTED_DOCUMENT',
    ['empty-root', 'root-name', 'virtual-document', 'mime'],
  ),
  (
    'STAGE_NAME_CONFLICT',
    [
      'duplicate',
      'reserved-stem',
      'trailing-dot-or-space',
      'control-char',
      'separator',
      'dot-component',
      'too-long',
      // The receiver's own namespace: `.ak-staging` in any letter case.
      'receiver-reserved',
    ],
  ),
  ('STAGE_NO_SPACE', ['no-space', 'enospc']),
  (
    'STAGE_IO_FAILED',
    [
      'create',
      'open',
      'read',
      'write',
      'containment',
      'symlink',
      'busy',
      'io',
      // An id that already carries a finished staging.
      'id-in-use',
      // Verified after the copy: the folder is not what was planned.
      'source-changed',
      'size-mismatch',
      // Stopped, and its directory could not be removed completely.
      'leftover',
    ],
  ),
  ('STAGE_INVALID_ID', ['format']),
];

/// One sentence each — the copy the dialogs are pinned to.
const String _noAccess = '没有这个文件夹的读取权限，请重新选择文件夹，并在系统提示中允许访问。';
const String _untrustedLocation = '不支持发送这个位置的文件夹，请改选手机本地存储里的文件夹，例如「下载」里的。';
const String _cannotRead = '无法完整读取这个文件夹里的内容，请换一个文件夹，或先把它复制到手机本地存储再发送。';
const String _tooMany = '这个文件夹里的项目太多，一次无法全部发送，请分成几个较小的文件夹分批发送。';
const String _empty = '这个文件夹是空的，没有可发送的内容，请改选一个里面有文件的文件夹。';
const String _asIs = '这个文件夹无法原样发送，请换一个文件夹，或只选择其中的文件。';
const String _names = '文件夹里有无法原样发送的名称（重名、过长，或含有电脑不支持的'
    '字符和设备名，或以点、空格结尾），请先整理这些名称，或只发送其中的文件。';
const String _receiverReserved =
    '文件夹里有接收方保留的名称（.ak-staging，不区分大小写），'
    '无法原样发送，请给它改名，或只发送其余的文件。';
const String _sameAsPicked = '这个文件夹与已选的某个文件同名，请单独发送这个文件夹，或去掉那个文件。';
const String _noSpace = '手机存储空间不足，无法复制这个文件夹，请清理出足够的空间后重试。';
const String _changed = '准备期间文件夹里的内容有变动，无法按原样发送，请重新选择这个文件夹。';
const String _leftover = '准备已中止，部分临时文件未能删除，会在下次启动时自动清理。';
const String _failed = '准备文件夹失败，请重新选择这个文件夹。';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late _StorageChannel storage;

  setUp(() {
    // A fresh Dart isolate: no stale-cleanup run and no listener guard —
    // and no listener that an earlier test's staging left on the channel,
    // so every test sees the FIRST staging of an isolate.
    PluginFilePickGateway.resetProcessStateForTest();
    _storage.setMethodCallHandler(null);
    storage = _StorageChannel()..install();
  });

  tearDown(() {
    storage.uninstall();
    PluginFilePickGateway.resetProcessStateForTest();
    _storage.setMethodCallHandler(null);
    UiPlatform.androidOverride = null;
  });

  // -----------------------------------------------------------------------
  // Platform and callers.
  // -----------------------------------------------------------------------

  testWidgets('desktop: pickSendDirectory IS pickDirectory — the channel '
      'is never called', (tester) async {
    await _pumpApp(tester);
    var directoryPicks = 0;
    for (final enabled in [true, false]) {
      // The switch is Android-only: on a desktop both values delegate.
      final gateway = PluginFilePickGateway(
        isAndroidPlatform: () => false,
        androidFolderSendEnabled: () => enabled,
        directoryPicker: () async {
          directoryPicks += 1;
          return r'C:\docs\相册';
        },
      );
      final entry = await gateway.pickSendDirectory();
      expect(entry?.path, r'C:\docs\相册');
      expect(entry?.name, '相册');
      expect(entry?.isDirectory, isTrue);
      expect(entry?.stagingId, isNull, reason: '桌面选择不暂存');
    }
    expect(directoryPicks, 2);
    expect(storage.calls, isEmpty);
    expect(find.byKey(_unsupportedBody), findsNothing);
  });

  testWidgets('Android: the picker cancelled answers null — nothing is '
      'staged and no dialog is shown', (tester) async {
    await _pumpApp(tester);
    storage.on('pickSendFolder', (_) => {'status': 'cancelled'});
    expect(await _finished(tester, _androidGateway().pickSendDirectory()),
        isNull);
    expect(storage.methods, ['pickSendFolder']);
    expect(find.byKey(_progressDialog), findsNothing);
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('Android with no navigator: the pick answers null and '
      'stageSafTree is never called — nothing is staged unattended, no '
      'dialog is waited for', (tester) async {
    storage.on('pickSendFolder', (_) => _selected);
    expect(await _finished(tester, _androidGateway().pickSendDirectory()),
        isNull);
    expect(storage.methods, ['pickSendFolder']);
  });

  testWidgets('Android: a second pickSendDirectory while one is open '
      'answers null at once — the channel sees one pick (two gateways: '
      'the guard is not per instance)', (tester) async {
    await _pumpApp(tester);
    final picking = storage.hold('pickSendFolder');
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);

    final first = _androidGateway().pickSendDirectory();
    await tester.pump();
    expect(storage.methods, ['pickSendFolder']);

    final second =
        await _finished(tester, _androidGateway().pickSendDirectory());
    expect(second, isNull);
    expect(storage.methods, ['pickSendFolder'],
        reason: '并发的一次不打开选择器，也不暂存');

    // The first pick runs its course; the pick is open again afterwards.
    picking.complete(_selected);
    final entry = await _finished(tester, first);
    expect(entry, isNotNull);
    expect(storage.methods, ['pickSendFolder', 'stageSafTree']);
  });

  // -----------------------------------------------------------------------
  // Staging under the progress dialog.
  // -----------------------------------------------------------------------

  testWidgets('Android: a selected and staged tree answers the entry — a '
      'filesystem path under a fresh id — after ONE stale cleanup that '
      'runs before the first staging', (tester) async {
    await _pumpApp(tester);
    UiPlatform.androidOverride = true;
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok)
      ..on('cleanupStaleStaging', (_) => {'removed': 2});
    final gateway = _androidGateway(realStaleCleanup: true);

    final first = await _finished(tester, gateway.pickSendDirectory());
    final second = await _finished(tester, gateway.pickSendDirectory());

    final id = first!.stagingId!;
    expect(id, matches(RegExp(r'^[0-9a-f]{32}$')));
    expect(first.path, startsWith('/'));
    expect(first.path, isNot(contains('content:')));
    expect(first.path, isNot(contains('msd:')));
    expect(first.path, isNot(_treeUri), reason: '树形 URI 不出现在路径里');
    expect(first.name, _folderName);
    expect(first.isDirectory, isTrue);
    expect(first.sizeBytes, isNull);
    expect(first.stagedFileCount, 7);
    expect(first.stagedByteCount, 1536);
    expect(storage.argsOf('stageSafTree').first, {
      'treeUri': _treeUri,
      'stageId': id,
    });

    // A fresh id per attempt.
    expect(second!.stagingId, matches(RegExp(r'^[0-9a-f]{32}$')));
    expect(second.stagingId, isNot(id));

    expect(storage.methods, [
      'cleanupStaleStaging',
      'pickSendFolder',
      'stageSafTree',
      'pickSendFolder',
      'stageSafTree',
    ], reason: '启动清理先于第一次暂存，两次选择之间只跑一次');
    // A copy that was handed over is neither released nor cancelled.
    expect(storage.methods, isNot(contains('cleanupStagedSend')));
    expect(storage.methods, isNot(contains('cancelStaging')));
    expect(find.byKey(_progressDialog), findsNothing);
  });

  testWidgets('Android: the first staging WAITS for the startup cleanup '
      '— the picker is not even opened before it answered', (tester) async {
    await _pumpApp(tester);
    UiPlatform.androidOverride = true;
    final cleanup = storage.hold('cleanupStaleStaging');
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);

    final pick = _androidGateway(realStaleCleanup: true).pickSendDirectory();
    await tester.pump();
    expect(storage.methods, ['cleanupStaleStaging']);

    cleanup.complete({'removed': 0});
    expect(await _finished(tester, pick), isNotNull);
    expect(storage.methods,
        ['cleanupStaleStaging', 'pickSendFolder', 'stageSafTree']);
  });

  testWidgets('Android: a failing startup cleanup is swallowed and does '
      'not keep the folder from being staged', (tester) async {
    await _pumpApp(tester);
    UiPlatform.androidOverride = true;
    storage
      ..on('cleanupStaleStaging', (_) {
        throw PlatformException(code: 'STAGE_IO_FAILED', message: 'io');
      })
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);

    final entry = await _finished(
        tester,
        _androidGateway(realStaleCleanup: true)
            .pickSendDirectory());
    expect(entry, isNotNull);
    expect(storage.methods,
        ['cleanupStaleStaging', 'pickSendFolder', 'stageSafTree']);
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('Android: the progress dialog is up while staging — an '
      'indeterminate bar and 正在读取文件夹内容… until the first event; '
      'the barrier does not dismiss it', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);

    expect(find.byKey(_progressDialog), findsOneWidget);
    expect(find.text('正在准备文件夹'), findsOneWidget);
    expect(_textOf(tester, _progressText), '正在读取文件夹内容…');
    expect(_bar(tester).value, isNull, reason: '枚举阶段：不确定进度条');
    expect(find.byKey(_cancelButton), findsOneWidget);

    await tester.tapAt(const Offset(10, 10));
    await tester.pump();
    expect(find.byKey(_progressDialog), findsOneWidget,
        reason: '点对话框外面不会关掉它');
    expect(storage.methods, isNot(contains('cancelStaging')));

    staged.complete(_ok(storage.callsFor('stageSafTree').single));
    expect(await _finished(tester, pick), isNotNull);
    expect(find.byKey(_progressDialog), findsNothing);
  });

  testWidgets('Android: stagingProgress events drive the dialog — and the '
      'listener is on the channel BEFORE stageSafTree is invoked',
      (tester) async {
    await _pumpApp(tester);
    final staged = Completer<Object?>();
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', (call) {
        // Delivered while the call is being made. A message the test
        // messenger hands to a channel with no listener is DROPPED, so the
        // dialog can only show this event if the listener was installed
        // before this call — not after it.
        _progress(
          stageId: _stageIdOf(call),
          phase: 'copy',
          filesCopied: 3,
          bytesCopied: 3 * 1024 * 1024,
          totalBytesEstimate: 10 * 1024 * 1024,
        );
        return staged.future;
    });

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final stageId = _stageIdOf(storage.callsFor('stageSafTree').single);

    // The dialog's FIRST frame already carries the event.
    expect(_textOf(tester, _progressText), '已复制 3 个文件 · 3.0 MB');
    expect(find.textContaining('已复制 3 个文件'), findsOneWidget);
    expect(_bar(tester).value, closeTo(0.3, 1e-9),
        reason: '估算总量已知：确定进度条');

    _progress(
      stageId: stageId,
      phase: 'copy',
      filesCopied: 5,
      bytesCopied: 5 * 1024 * 1024,
      totalBytesEstimate: 10 * 1024 * 1024,
    );
    await tester.pump();
    expect(_textOf(tester, _progressText), '已复制 5 个文件 · 5.0 MB');
    expect(_bar(tester).value, closeTo(0.5, 1e-9));

    // A size the provider did not report: the total stays unknown.
    _progress(stageId: stageId, phase: 'copy', filesCopied: 6,
        bytesCopied: 6 * 1024 * 1024);
    await tester.pump();
    expect(_textOf(tester, _progressText), '已复制 6 个文件 · 6.0 MB');
    expect(_bar(tester).value, isNull, reason: '总量未知：不确定进度条');

    // An event of another staging is not this dialog's business.
    _progress(
      stageId: 'f' * 32,
      phase: 'copy',
      filesCopied: 99,
      bytesCopied: 99,
    );
    await tester.pump();
    expect(_textOf(tester, _progressText), '已复制 6 个文件 · 6.0 MB');

    // A late enumeration event (a sub-tree being read) shows that phase.
    _progress(stageId: stageId, phase: 'enumerate');
    await tester.pump();
    expect(_textOf(tester, _progressText), '正在读取文件夹内容…');
    expect(_bar(tester).value, isNull);

    staged.complete(_ok(storage.callsFor('stageSafTree').single));
    expect(await _finished(tester, pick), isNotNull);
    expect(find.byKey(_progressDialog), findsNothing);
  });

  testWidgets('Android: one staging id per attempt — 32 lowercase hex, '
      'the same in stageSafTree and cancelStaging', (tester) async {
    await _pumpApp(tester);
    final ids = <String>{};
    for (var attempt = 0; attempt < 12; attempt += 1) {
      storage
        ..on('pickSendFolder', (_) => _selected)
        ..on('stageSafTree', _ok);
      final entry =
          await _finished(tester, _androidGateway().pickSendDirectory());
      ids.add(entry!.stagingId!);
    }
    expect(ids, hasLength(12), reason: '每次尝试一个新的 id');
    expect(ids.every((id) => RegExp(r'^[0-9a-f]{32}$').hasMatch(id)), isTrue);
  });

  // -----------------------------------------------------------------------
  // Cancel.
  // -----------------------------------------------------------------------

  testWidgets('Android: 取消 asks the platform to stop with the staging '
      'id, keeps the dialog up until the call answers, and never releases '
      'an open staging; STAGE_CANCELLED answers null with no dialog',
      (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final stageId = _stageIdOf(storage.callsFor('stageSafTree').single);

    await tester.tap(find.byKey(_cancelButton));
    await tester.pump();
    expect(storage.argsOf('cancelStaging'), [
      {'stageId': stageId},
    ]);
    expect(find.byKey(_progressDialog), findsOneWidget,
        reason: '暂存调用还没应答：对话框留着');
    expect(_textOf(tester, _progressText), '正在取消…');
    expect(
      tester.widget<TextButton>(find.byKey(_cancelButton)).onPressed,
      isNull,
      reason: '取消只请求一次',
    );
    expect(storage.methods, isNot(contains('cleanupStagedSend')),
        reason: '暂存调用未应答之前绝不 cleanupStagedSend');

    // Tapping outside or pressing back again changes nothing.
    await tester.tapAt(const Offset(10, 10));
    await tester.pump();
    expect(find.byKey(_progressDialog), findsOneWidget);
    expect(storage.callsFor('cancelStaging'), hasLength(1));

    staged.completeError(
      PlatformException(code: 'STAGE_CANCELLED', message: 'user'),
    );
    expect(await _finished(tester, pick), isNull);
    expect(find.byKey(_progressDialog), findsNothing);
    expect(find.byKey(_errorDialog), findsNothing,
        reason: '取消是用户自己的答复，不是失败');
    expect(storage.methods, isNot(contains('cleanupStagedSend')),
        reason: '取消后暂存目录已由原生删除');
    expect(find.text('home page'), findsOneWidget);
  });

  testWidgets('Android: the system back gesture means 取消 — it asks the '
      'platform to stop and does NOT close the dialog', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final stageId = _stageIdOf(storage.callsFor('stageSafTree').single);

    await tester.binding.handlePopRoute();
    await tester.pump();

    expect(storage.argsOf('cancelStaging'), [
      {'stageId': stageId},
    ]);
    expect(find.byKey(_progressDialog), findsOneWidget,
        reason: '返回手势等于取消，对话框留到暂存调用应答');
    expect(_textOf(tester, _progressText), '正在取消…');

    staged.completeError(
      PlatformException(code: 'STAGE_CANCELLED', message: 'user'),
    );
    expect(await _finished(tester, pick), isNull);
    expect(find.text('home page'), findsOneWidget);
  });

  testWidgets('Android: a copy that completed despite 取消 is released '
      'and answers null — never handed over', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    // The platform's new answer: something of the copy is left behind.
    storage.on('cleanupStagedSend', (_) => {'removed': false});
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final call = storage.callsFor('stageSafTree').single;
    await tester.tap(find.byKey(_cancelButton));
    await tester.pump();
    expect(storage.methods, isNot(contains('cleanupStagedSend')));

    staged.complete(_ok(call));
    expect(await _finished(tester, pick), isNull);
    expect(storage.argsOf('cleanupStagedSend'), [
      {'stageId': _stageIdOf(call)},
    ]);
    // Released only after the staging call had answered.
    expect(storage.methods.indexOf('cleanupStagedSend'),
        greaterThan(storage.methods.indexOf('stageSafTree')));
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('Android: a cancelStaging the channel refuses changes '
      'nothing — the staging call still decides', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('cancelStaging', (_) {
        throw PlatformException(code: 'STAGE_INVALID_ID', message: 'format');
      });

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    await tester.tap(find.byKey(_cancelButton));
    await tester.pump();
    expect(find.byKey(_progressDialog), findsOneWidget);

    staged.complete(_ok(storage.callsFor('stageSafTree').single));
    expect(await _finished(tester, pick), isNull,
        reason: '用户已请求取消：副本交回');
    expect(storage.argsOf('cleanupStagedSend'), hasLength(1));
  });

  testWidgets('Android: a cancelled staging whose directory could not be '
      'removed answers STAGE_IO_FAILED / leftover — the refusal dialog '
      'FOLLOWS the 取消, and nothing is released', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final stageId = _stageIdOf(storage.callsFor('stageSafTree').single);
    await tester.tap(find.byKey(_cancelButton));
    await tester.pump();
    expect(find.byKey(_progressDialog), findsOneWidget);

    // Only a staging whose directory is gone answers STAGE_CANCELLED.
    staged.completeError(
      PlatformException(code: 'STAGE_IO_FAILED', message: 'leftover'),
    );
    await tester.pumpAndSettle();

    expect(find.byKey(_progressDialog), findsNothing);
    expect(find.byKey(_errorDialog), findsOneWidget);
    expect(_textOf(tester, _errorBody), _leftover);
    // The platform could not remove it and said so: this side does not
    // try again on its own.
    expect(storage.argsOf('cancelStaging'), [
      {'stageId': stageId},
    ]);
    expect(storage.methods, isNot(contains('cleanupStagedSend')));

    await tester.tap(find.byKey(_errorCancelButton));
    await tester.pumpAndSettle();
    expect(await pick, isNull);
    expect(find.text('home page'), findsOneWidget);
  });

  testWidgets('Android: 重新选择 after that leftover refusal picks again '
      '— the next staging is a fresh id and is handed over',
      (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final refusedId = _stageIdOf(storage.callsFor('stageSafTree').single);
    await tester.tap(find.byKey(_cancelButton));
    await tester.pump();
    staged.completeError(
      PlatformException(code: 'STAGE_IO_FAILED', message: 'leftover'),
    );
    await tester.pumpAndSettle();
    expect(_textOf(tester, _errorBody), _leftover);

    await tester.tap(find.byKey(_retryButton));
    final entry = await _finished(tester, pick);

    expect(entry, isNotNull);
    expect(entry!.stagingId, isNot(refusedId));
    expect(storage.callsFor('pickSendFolder'), hasLength(2));
    expect(storage.callsFor('stageSafTree'), hasLength(2));
    expect(storage.methods, isNot(contains('cleanupStagedSend')));
  });

  // -----------------------------------------------------------------------
  // Refusals.
  // -----------------------------------------------------------------------

  test('the refusal copy: one sentence per code and token, the generic '
      'one for anything unknown', () {
    const expected = <(String, String, String)>[
      // SAF_PERMISSION_DENIED — a failed grant lookup is one too.
      ('SAF_PERMISSION_DENIED', 'no-persisted-grant', _noAccess),
      ('SAF_PERMISSION_DENIED', 'grant-limit', _noAccess),
      ('SAF_PERMISSION_DENIED', 'provider-security', _noAccess),
      ('SAF_PERMISSION_DENIED', 'grant-lookup', _noAccess),
      ('SAF_PERMISSION_DENIED', 'a-token-this-build-does-not-know', _noAccess),
      // SAF_PROVIDER_UNTRUSTED
      ('SAF_PROVIDER_UNTRUSTED', 'authority', _untrustedLocation),
      // SAF_ENUMERATION_FAILED
      ('SAF_ENUMERATION_FAILED', 'query', _cannotRead),
      ('SAF_ENUMERATION_FAILED', 'duplicate-id', _cannotRead),
      ('SAF_ENUMERATION_FAILED', 'overflow', _cannotRead),
      ('SAF_ENUMERATION_FAILED', 'too-many-entries', _tooMany),
      ('SAF_ENUMERATION_FAILED', '', _cannotRead),
      // SAF_UNSUPPORTED_DOCUMENT
      ('SAF_UNSUPPORTED_DOCUMENT', 'empty-root', _empty),
      ('SAF_UNSUPPORTED_DOCUMENT', 'root-name', _asIs),
      ('SAF_UNSUPPORTED_DOCUMENT', 'virtual-document', _asIs),
      ('SAF_UNSUPPORTED_DOCUMENT', 'mime', _asIs),
      ('SAF_UNSUPPORTED_DOCUMENT', 'later-token', _asIs),
      // STAGE_NAME_CONFLICT
      ('STAGE_NAME_CONFLICT', 'duplicate', _names),
      ('STAGE_NAME_CONFLICT', 'reserved-stem', _names),
      ('STAGE_NAME_CONFLICT', 'trailing-dot-or-space', _names),
      ('STAGE_NAME_CONFLICT', 'control-char', _names),
      ('STAGE_NAME_CONFLICT', 'separator', _names),
      ('STAGE_NAME_CONFLICT', 'dot-component', _names),
      ('STAGE_NAME_CONFLICT', 'too-long', _names),
      ('STAGE_NAME_CONFLICT', 'later-token', _names),
      // The receiver's reserved namespace, and the Dart-only token.
      ('STAGE_NAME_CONFLICT', 'receiver-reserved', _receiverReserved),
      ('STAGE_NAME_CONFLICT', 'duplicate-with-picked', _sameAsPicked),
      // STAGE_NO_SPACE
      ('STAGE_NO_SPACE', 'no-space', _noSpace),
      ('STAGE_NO_SPACE', 'enospc', _noSpace),
      ('STAGE_NO_SPACE', '', _noSpace),
      // STAGE_IO_FAILED: the folder changed under the copy, twice over.
      ('STAGE_IO_FAILED', 'source-changed', _changed),
      ('STAGE_IO_FAILED', 'size-mismatch', _changed),
      // Stopped with something left behind.
      ('STAGE_IO_FAILED', 'leftover', _leftover),
      // Everything else about the staging is the generic sentence.
      ('STAGE_IO_FAILED', 'id-in-use', _failed),
      ('STAGE_IO_FAILED', 'create', _failed),
      ('STAGE_IO_FAILED', 'open', _failed),
      ('STAGE_IO_FAILED', 'read', _failed),
      ('STAGE_IO_FAILED', 'write', _failed),
      ('STAGE_IO_FAILED', 'containment', _failed),
      ('STAGE_IO_FAILED', 'symlink', _failed),
      ('STAGE_IO_FAILED', 'busy', _failed),
      ('STAGE_IO_FAILED', 'io', _failed),
      ('STAGE_INVALID_ID', 'format', _failed),
      ('A_CODE_THIS_BUILD_DOES_NOT_KNOW', 'whatever', _failed),
      ('', '', _failed),
    ];
    for (final (code, token, body) in expected) {
      expect(
        safSendStagingErrorBody(code, token),
        body,
        reason: '$code / $token',
      );
    }
    // Every token of the contract's table has a sentence.
    for (final (code, tokens) in _refusals) {
      for (final token in tokens) {
        expect(safSendStagingErrorBody(code, token), isNotEmpty,
            reason: '$code / $token');
      }
    }
    // One sentence each: a cause, then what to do — no code leaks into
    // the copy.
    for (final body in [_noAccess, _untrustedLocation, _cannotRead, _tooMany,
      _empty, _asIs, _names, _receiverReserved, _sameAsPicked, _noSpace,
      _changed, _leftover, _failed]) {
      expect(body, endsWith('。'));
      expect(body, isNot(contains('STAGE_')));
      expect(body, isNot(contains('SAF_')));
      expect(body, isNot(contains('content:')));
    }
  });

  for (final (code, tokens) in _refusals) {
    testWidgets(
        'Android: every $code token shows the error dialog with its body '
        '— 重新选择 runs the picker again and a following success is '
        'returned; 取消 answers null', (tester) async {
      await _pumpApp(tester);
      var refuse = true;
      var token = tokens.first;
      storage
        ..on('pickSendFolder', (_) => _selected)
        ..on('stageSafTree', (call) {
          if (refuse) {
            throw PlatformException(code: code, message: token);
          }
          return _ok(call);
        });
      final gateway = _androidGateway();

      final pick = gateway.pickSendDirectory();
      await tester.pumpAndSettle();
      for (var attempt = 0; attempt < tokens.length; attempt += 1) {
        expect(find.byKey(_errorDialog), findsOneWidget);
        expect(_textOf(tester, _errorTitle), '无法发送此文件夹');
        expect(
          _textOf(tester, _errorBody),
          safSendStagingErrorBody(code, tokens[attempt]),
        );
        // The progress dialog left before the refusal was reported.
        expect(find.byKey(_progressDialog), findsNothing);
        expect(storage.callsFor('stageSafTree'), hasLength(attempt + 1));
        expect(storage.callsFor('pickSendFolder'), hasLength(attempt + 1));
        // A refused attempt leaves no directory behind to release.
        expect(storage.methods, isNot(contains('cleanupStagedSend')));

        // 重新选择: the loop picks again — refused once more with the
        // next token, or (after the last one) staged.
        if (attempt + 1 < tokens.length) {
          token = tokens[attempt + 1];
        } else {
          refuse = false;
        }
        await tester.tap(find.byKey(_retryButton));
        await tester.pumpAndSettle();
      }

      final entry = await _finished(tester, pick);
      expect(entry, isNotNull, reason: '重新选择之后成功的那次要交回条目');
      expect(storage.callsFor('stageSafTree'), hasLength(tokens.length + 1));
      expect(storage.callsFor('pickSendFolder'), hasLength(tokens.length + 1));
      expect(storage.methods, isNot(contains('cleanupStagedSend')));
      expect(find.byKey(_errorDialog), findsNothing);
      expect(find.text('home page'), findsOneWidget);

      // 取消 ends the pick with null — no further attempt.
      refuse = true;
      final cancelled = gateway.pickSendDirectory();
      await tester.pumpAndSettle();
      expect(find.byKey(_errorDialog), findsOneWidget);
      await tester.tap(find.byKey(_errorCancelButton));
      await tester.pumpAndSettle();
      expect(await cancelled, isNull);
      expect(storage.callsFor('pickSendFolder'), hasLength(tokens.length + 2));
      expect(storage.callsFor('stageSafTree'), hasLength(tokens.length + 2));
      expect(find.byKey(_errorDialog), findsNothing);
      expect(find.byKey(_progressDialog), findsNothing);
      expect(find.text('home page'), findsOneWidget);
    });
  }

  testWidgets('Android: a code this build does not know gets the generic '
      'refusal dialog', (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', (_) {
        throw PlatformException(code: 'FUTURE_CODE', message: 'x');
      });
    final pick = _androidGateway().pickSendDirectory();
    await tester.pumpAndSettle();
    expect(_textOf(tester, _errorBody), _failed);
    await tester.tap(find.byKey(_errorCancelButton));
    await tester.pumpAndSettle();
    expect(await pick, isNull);
  });

  testWidgets('Android: the picker ITSELF refuses (no progress dialog '
      'ever existed) — the refusal dialog, and nothing else is popped',
      (tester) async {
    await _pumpApp(tester);
    var picks = 0;
    storage
      ..on('pickSendFolder', (_) {
        picks += 1;
        if (picks == 1) {
          // The untrusted location is reported by the picker, before
          // anything could be staged.
          throw PlatformException(
            code: 'SAF_PROVIDER_UNTRUSTED',
            message: 'authority',
          );
        }
        return _selected;
      })
      ..on('stageSafTree', _ok);

    final pick = _androidGateway().pickSendDirectory();
    await tester.pumpAndSettle();
    expect(storage.methods, ['pickSendFolder'], reason: '没有任何暂存');
    expect(find.byKey(_progressDialog), findsNothing);
    expect(find.byKey(_errorDialog), findsOneWidget);
    expect(_textOf(tester, _errorBody), _untrustedLocation);
    expect(find.text('home page'), findsOneWidget);

    await tester.tap(find.byKey(_retryButton));
    final entry = await _finished(tester, pick);
    expect(entry, isNotNull);
    expect(storage.methods,
        ['pickSendFolder', 'pickSendFolder', 'stageSafTree']);
    expect(find.text('home page'), findsOneWidget,
        reason: '弹窗下面那一页还在：没有盲目的 pop');
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('Android: dismissing the refusal dialog answers 取消 — and '
      'with no navigator a refusal ends the pick without any dialog',
      (tester) async {
    // No app pumped: no navigator at all.
    storage.on('pickSendFolder', (_) {
      throw PlatformException(
        code: 'SAF_PROVIDER_UNTRUSTED',
        message: 'authority',
      );
    });
    expect(await _androidGateway().pickSendDirectory(), isNull);

    await _pumpApp(tester);
    final pick = _androidGateway().pickSendDirectory();
    await tester.pumpAndSettle();
    expect(find.byKey(_errorDialog), findsOneWidget);
    await tester.tapAt(const Offset(10, 10));
    await tester.pumpAndSettle();
    expect(await pick, isNull);
    expect(find.text('home page'), findsOneWidget);
  });

  // -----------------------------------------------------------------------
  // Dialog ownership: the gateway pops only the progress route it pushed.
  // -----------------------------------------------------------------------

  testWidgets('dialog ownership: an error answered by stageSafTree when '
      'the progress dialog was never pushed pops nothing', (tester) async {
    // A navigator that drops every route pushed on it: whatever the
    // gateway pushes is never on it.
    await tester.pumpWidget(const _DroppingNavigatorApp());
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', (_) {
        throw PlatformException(code: 'STAGE_IO_FAILED', message: 'io');
      });

    final pick = _androidGateway().pickSendDirectory();
    await tester.pumpAndSettle();

    expect(storage.methods, contains('stageSafTree'),
        reason: '前提：暂存调用确实发生了');
    expect(await pick, isNull,
        reason: '拒绝弹窗也弹不出来：按取消收尾，不等待弹不出的弹窗');
    expect(find.text('home page'), findsOneWidget,
        reason: '没有盲目的 pop：下面的页面还在');
    expect(find.byKey(_progressDialog), findsNothing);
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('dialog ownership: an error that arrives after the progress '
      'dialog was removed pops nothing else', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    // Somebody else takes the dialog off the navigator while the staging
    // call is still open.
    final dialogContext = tester.element(find.byKey(_progressDialog));
    Navigator.of(dialogContext)
        .removeRoute(ModalRoute.of(dialogContext)!);
    await tester.pumpAndSettle();
    expect(find.byKey(_progressDialog), findsNothing);

    staged.completeError(
      PlatformException(code: 'STAGE_IO_FAILED', message: 'io'),
    );
    await tester.pumpAndSettle();

    expect(find.byKey(_errorDialog), findsOneWidget);
    expect(find.text('home page'), findsOneWidget,
        reason: '没有盲目的 pop：下面的页面还在');
    await tester.tap(find.byKey(_errorCancelButton));
    await tester.pumpAndSettle();
    expect(await pick, isNull);
    expect(find.text('home page'), findsOneWidget);
  });

  testWidgets('dialog ownership: a success that arrives after the '
      'progress dialog was removed pops nothing else', (tester) async {
    await _pumpApp(tester);
    final staged = storage.hold('stageSafTree');
    storage.on('pickSendFolder', (_) => _selected);

    final pick = _androidGateway().pickSendDirectory();
    await _showDialog(tester);
    final call = storage.callsFor('stageSafTree').single;
    final dialogContext = tester.element(find.byKey(_progressDialog));
    final navigator = Navigator.of(dialogContext);
    navigator.removeRoute(ModalRoute.of(dialogContext)!);
    await tester.pumpAndSettle();

    staged.complete(_ok(call));
    final entry = await _finished(tester, pick);
    expect(entry?.stagingId, _stageIdOf(call));
    expect(find.text('home page'), findsOneWidget,
        reason: '没有盲目的 pop：下面的页面还在');
    expect(navigator.canPop(), isFalse,
        reason: '只弹出自己推入的那条路由');
    expect(navigator.mounted, isTrue);
  });

  // -----------------------------------------------------------------------
  // G1: an ok answer must belong to THIS staging.
  // -----------------------------------------------------------------------

  testWidgets('G1: an ok answer that is this staging\'s own is accepted — '
      'every rule at its boundary', (tester) async {
    await _pumpApp(tester);
    // One segment, with spaces and dots in it.
    const name = '相册 2024.backup';
    storage
      ..on('pickSendFolder', (_) => {..._selected, 'displayName': name})
      ..on('stageSafTree', (call) => {
            ..._okFor(_stageIdOf(call), name),
            // Every count may be zero.
            'fileCount': 0,
            'byteCount': 0,
            'emptyDirectoryCount': 0,
          });

    final entry =
        await _finished(tester, _androidGateway().pickSendDirectory());

    expect(entry, isNotNull);
    expect(entry!.name, name);
    expect(entry.path, '$_stagingBase/${entry.stagingId}/payload/$name');
    expect(entry.isDirectory, isTrue);
    expect(entry.sizeBytes, isNull);
    expect(entry.stagedFileCount, 0);
    expect(entry.stagedByteCount, 0);
    expect(storage.argsOf('stageSafTree').single, {
      'treeUri': _treeUri,
      'stageId': entry.stagingId,
    });
    // What was handed over is not released.
    expect(storage.methods, isNot(contains('cleanupStagedSend')));
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('G1: the folder keeps the name the PICK returned — the one '
      'the staged path has to end with — whatever the answer\'s own '
      'displayName says', (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', (call) => {
            ..._ok(call),
            'displayName': 'not-what-was-picked',
          });

    final entry =
        await _finished(tester, _androidGateway().pickSendDirectory());

    expect(entry, isNotNull);
    expect(entry!.name, _folderName);
    expect(entry.path, _stagedRoot(entry.stagingId!));
  });

  testWidgets('G1: a picked name that is not ONE segment is refused before '
      'anything is staged', (tester) async {
    await _pumpApp(tester);
    for (final name in <Object?>['a/b', '.', '..', '', null, 7]) {
      storage.on('pickSendFolder', (_) => {..._selected, 'displayName': name});
      final pick = _androidGateway().pickSendDirectory();
      await tester.pumpAndSettle();

      expect(find.byKey(_errorDialog), findsOneWidget);
      expect(_textOf(tester, _errorBody), _failed, reason: '$name');
      // Nothing was staged, so there is nothing to give back either.
      expect(storage.callsFor('stageSafTree'), isEmpty, reason: '$name');
      expect(storage.callsFor('cleanupStagedSend'), isEmpty, reason: '$name');

      await tester.tap(find.byKey(_errorCancelButton));
      await tester.pumpAndSettle();
      expect(await pick, isNull);
    }
  });

  // How an ok answer can fail to be the staging's own. Each builder gets
  // the id that was REQUESTED and returns the answer the platform gives.
  final notThisStaging = <(String, Map<String, Object?> Function(String id))>[
    // -- the path is not the one made for this id --------------------------
    (
      "another staging's absolute path",
      (id) => {..._okFor(id), 'path': _stagedRoot(_otherId(id))},
    ),
    // The suffix shifts: these are refused because `<stageId>` no longer
    // sits where it must.
    (
      'a `..` segment inside the staging directory',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase/$id/payload/../$_folderName',
      },
    ),
    (
      'a `..` segment in front of the staging directory',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase/../$id/payload/$_folderName',
      },
    ),
    // The suffix is intact: only the rule against `..` as a segment
    // refuses this one.
    (
      'a `..` segment inside the base',
      (id) => {
        ..._okFor(id),
        'path': '/data/user/0/dev.ak.ak_sending_app/other/../no_backup'
            '/ak-saf-staging/$id/payload/$_folderName',
      },
    ),
    (
      'a `.` segment',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase/./$id/payload/$_folderName',
      },
    ),
    (
      'an empty segment',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase//$id/payload/$_folderName',
      },
    ),
    (
      'a trailing slash',
      (id) => {..._okFor(id), 'path': '${_stagedRoot(id)}/'},
    ),
    (
      'a missing payload level',
      (id) => {..._okFor(id), 'path': '$_stagingBase/$id/$_folderName'},
    ),
    // The id and the name sit where they must — only the `payload` level
    // itself is not what it has to be.
    (
      'another directory where payload belongs',
      (id) => {..._okFor(id), 'path': '$_stagingBase/$id/copy/$_folderName'},
    ),
    (
      'the staging id one level too deep',
      (id) => {..._okFor(id), 'path': '$_stagingBase/payload/$id/$_folderName'},
    ),
    (
      'no base in front of the staging id',
      (id) => {..._okFor(id), 'path': '/$id/payload/$_folderName'},
    ),
    (
      'a wrong root name',
      (id) => {..._okFor(id), 'path': '$_stagingBase/$id/payload/别的名字'},
    ),
    // -- the path is not an absolute POSIX path of plain segments ---------
    (
      'a Windows path with backslashes',
      (id) => {
        ..._okFor(id),
        'path': r'C:\ak-saf-staging\' '$id\\payload\\$_folderName',
      },
    ),
    (
      'a Windows path with slashes',
      (id) => {
        ..._okFor(id),
        'path': 'C:/ak-saf-staging/$id/payload/$_folderName',
      },
    ),
    (
      'a relative path',
      (id) => {
        ..._okFor(id),
        'path': 'ak-saf-staging/$id/payload/$_folderName',
      },
    ),
    (
      'a colon in the path',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase/x:y/$id/payload/$_folderName',
      },
    ),
    // The backslash sits where a slash would be legal — only the rule
    // against `\` itself refuses this one.
    (
      'a backslash in the path',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase/x\\y/$id/payload/$_folderName',
      },
    ),
    (
      'a control character in the path',
      (id) => {
        ..._okFor(id),
        'path': '$_stagingBase\u{1}/$id/payload/$_folderName',
      },
    ),
    (
      'the tree URI that was picked',
      (id) => {..._okFor(id), 'path': _treeUri},
    ),
    (
      'an msd: pseudo path',
      (id) => {..._okFor(id), 'path': 'msd:2184'},
    ),
    (
      'a /storage/msd/… base (P1-H on Android)',
      (id) => {..._okFor(id), 'path': '/storage/msd/$id/payload/$_folderName'},
    ),
    // -- the answer is about another id, or not an ok one -----------------
    (
      'a foreign stageId',
      (id) => {..._okFor(id), 'stageId': _otherId(id)},
    ),
    (
      'no stageId at all',
      (id) => {..._okFor(id)}..remove('stageId'),
    ),
    (
      'a status other than ok',
      (id) => {..._okFor(id), 'status': 'done'},
    ),
    // -- a count that is not an integer of 0 or more ----------------------
    (
      'a negative file count',
      (id) => {..._okFor(id), 'fileCount': -1},
    ),
    (
      'a negative byte count',
      (id) => {..._okFor(id), 'byteCount': -1},
    ),
    (
      'a negative empty-directory count',
      (id) => {..._okFor(id), 'emptyDirectoryCount': -1},
    ),
    (
      "a count that isn't an integer (text)",
      (id) => {..._okFor(id), 'byteCount': '1536'},
    ),
    (
      "a count that isn't an integer (a fraction)",
      (id) => {..._okFor(id), 'fileCount': 1.5},
    ),
    (
      'a missing count',
      (id) => {..._okFor(id)}..remove('emptyDirectoryCount'),
    ),
  ];

  for (final (label, shape) in notThisStaging) {
    testWidgets('G1: an ok answer with $label is a refusal — the copy is '
        'released under the REQUESTED id, nothing is handed over',
        (tester) async {
      await _pumpApp(tester);
      storage
        ..on('pickSendFolder', (_) => _selected)
        ..on('stageSafTree', (call) => shape(_stageIdOf(call)));

      final pick = _androidGateway().pickSendDirectory();
      await tester.pumpAndSettle();

      final requested = _stageIdOf(storage.callsFor('stageSafTree').single);
      expect(find.byKey(_errorDialog), findsOneWidget);
      expect(_textOf(tester, _errorBody), _failed);
      expect(find.byKey(_progressDialog), findsNothing);
      // The staging call answered, so giving the copy back is safe — and
      // only ever THIS staging's id is given back.
      expect(storage.argsOf('cleanupStagedSend'), [
        {'stageId': requested},
      ], reason: '只交回这次请求的 id');

      await tester.tap(find.byKey(_errorCancelButton));
      await tester.pumpAndSettle();
      expect(await pick, isNull);
    });
  }

  testWidgets('Android: an answer of no known shape is a refusal, not a '
      'crash — of the picker and of the staging alike', (tester) async {
    await _pumpApp(tester);
    for (final answer in <Object?>[
      null,
      {'status': 'weird'},
      {'status': 'selected', 'treeUri': _treeUri},
      {'status': 'selected', 'displayName': _folderName},
    ]) {
      storage.on('pickSendFolder', (_) => answer);
      final pick = _androidGateway().pickSendDirectory();
      await tester.pumpAndSettle();
      expect(_textOf(tester, _errorBody), _failed, reason: '$answer');
      expect(storage.methods, isNot(contains('stageSafTree')));
      await tester.tap(find.byKey(_errorCancelButton));
      await tester.pumpAndSettle();
      expect(await pick, isNull);
    }

    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', (_) => {'status': 'weird'});
    final pick = _androidGateway().pickSendDirectory();
    await tester.pumpAndSettle();
    expect(_textOf(tester, _errorBody), _failed);
    expect(storage.callsFor('cleanupStagedSend'), hasLength(1));
    await tester.tap(find.byKey(_errorCancelButton));
    await tester.pumpAndSettle();
    expect(await pick, isNull);
  });

  // -----------------------------------------------------------------------
  // releaseStaged and the stale cleanup.
  // -----------------------------------------------------------------------

  test('releaseStaged: one cleanupStagedSend per distinct staging id, '
      'none for an entry without one — the platform\'s {"removed": …} '
      'answers are fine', () async {
    const gateway = PluginFilePickGateway();
    storage.on('cleanupStagedSend', (call) {
      // What the platform answers: true, or false when something is
      // left behind. Neither changes what this side does.
      return {'removed': _stageIdOf(call) != 'a'};
    });
    await gateway.releaseStaged([
      _staged('a'),
      _file('C:\\docs\\照片.zip'),
      _staged('a'),
      _staged('b'),
      _staged('a'),
    ]);
    expect(storage.argsOf('cleanupStagedSend'), [
      {'stageId': 'a'},
      {'stageId': 'b'},
    ]);
    await gateway.releaseStaged(<PickedEntry>[]);
    await gateway.releaseStaged([_file('C:\\docs\\报告.pdf')]);
    expect(storage.callsFor('cleanupStagedSend'), hasLength(2));
  });

  test('releaseStaged: a cleanup the channel refuses does not propagate '
      '— a staging still running (busy) is left alone, the other ids are '
      'still cleaned', () async {
    const gateway = PluginFilePickGateway();
    storage.on('cleanupStagedSend', (call) {
      if (_stageIdOf(call) == 'a') {
        throw PlatformException(code: 'STAGE_IO_FAILED', message: 'busy');
      }
      return {'removed': true};
    });
    await gateway.releaseStaged([_staged('a'), _staged('b'), _staged('c')]);
    expect(storage.argsOf('cleanupStagedSend'), [
      {'stageId': 'a'},
      {'stageId': 'b'},
      {'stageId': 'c'},
    ]);

    // Any other refusal is just as harmless.
    storage.on('cleanupStagedSend', (_) {
      throw PlatformException(code: 'STAGE_IO_FAILED', message: 'id-in-use');
    });
    await gateway.releaseStaged([_staged('d')]);
    expect(storage.argsOf('cleanupStagedSend'), hasLength(4));

    // A channel without the method at all.
    storage.on('cleanupStagedSend', (_) {
      throw MissingPluginException('cleanupStagedSend');
    });
    await gateway.releaseStaged([_staged('e')]);
    expect(storage.argsOf('cleanupStagedSend'), hasLength(5));
  });

  test('cleanupStaleStaging: calls the channel on Android — once, '
      'however often it is called', () async {
    UiPlatform.androidOverride = true;
    storage.on('cleanupStaleStaging', (_) => {'removed': 3});
    await PluginFilePickGateway.cleanupStaleStaging();
    await PluginFilePickGateway.cleanupStaleStaging();
    expect(storage.methods, ['cleanupStaleStaging']);
  });

  test('cleanupStaleStaging: never calls the channel off Android', () async {
    UiPlatform.androidOverride = false;
    await PluginFilePickGateway.cleanupStaleStaging();
    await PluginFilePickGateway.cleanupStaleStaging();
    expect(storage.calls, isEmpty);
  });

  test('cleanupStaleStaging: a failing cleanup is swallowed and not run '
      'again in the same isolate', () async {
    UiPlatform.androidOverride = true;
    storage.on('cleanupStaleStaging', (_) {
      throw PlatformException(code: 'STAGE_IO_FAILED', message: 'io');
    });
    await PluginFilePickGateway.cleanupStaleStaging();
    await PluginFilePickGateway.cleanupStaleStaging();
    expect(storage.methods, ['cleanupStaleStaging']);
  });

  testWidgets('cleanupStaleStaging: the app start and the first staging '
      'share ONE run', (tester) async {
    await _pumpApp(tester);
    UiPlatform.androidOverride = true;
    storage
      ..on('cleanupStaleStaging', (_) => {'removed': 1})
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);
    final gateway = _androidGateway(realStaleCleanup: true);

    await PluginFilePickGateway.cleanupStaleStaging(); // AkSendingApp
    expect(await _finished(tester, gateway.pickSendDirectory()), isNotNull);
    expect(await _finished(tester, gateway.pickSendDirectory()), isNotNull);
    expect(storage.methods, [
      'cleanupStaleStaging',
      'pickSendFolder',
      'stageSafTree',
      'pickSendFolder',
      'stageSafTree',
    ]);
  });

  // -----------------------------------------------------------------------
  // pickFiles() on Android.
  // -----------------------------------------------------------------------

  testWidgets('pickFiles on Android: the append offer still follows the '
      'files, and 添加文件夹 goes through pickSendFolder + stageSafTree — '
      'the staged folder last', (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);
    var directoryPicks = 0;
    final gateway = _androidGateway(
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async {
        directoryPicks += 1;
        return '/storage/emulated/0/Download/相册';
      },
    );

    final pick = gateway.pickFiles();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('还要添加整个文件夹吗？'), findsOneWidget);
    expect(storage.calls, isEmpty);

    await tester.tap(find.byKey(const ValueKey('folder_offer_add_button')));
    final picked = await _finished(tester, pick);

    expect(directoryPicks, 0, reason: 'Android 发送侧不再走插件的目录选择');
    expect(storage.methods, ['pickSendFolder', 'stageSafTree']);
    expect(picked, hasLength(3));
    expect(picked.map((entry) => entry.path), [
      _filePath1,
      _filePath2,
      _stagedRoot(_stageIdOf(storage.callsFor('stageSafTree').single)),
    ]);
    expect(picked.map((entry) => entry.name),
        ['照片.zip', '报告.pdf', _folderName]);
    expect(picked.map((entry) => entry.isDirectory), [false, false, true]);
    expect(picked.last.stagingId, isNotNull);
    for (final entry in picked) {
      expect(entry.path, startsWith('/'));
      expect(entry.path, isNot(contains('content:')));
      expect(entry.path, isNot(_treeUri), reason: '树形 URI 不进入返回的路径');
    }
  });

  testWidgets('pickFiles on Android: the folder-ONLY path goes through '
      'pickSendFolder + stageSafTree — never the plugin directory pick',
      (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);
    var directoryPicks = 0;
    final gateway = _androidGateway(
      filePicker: () async => const <fp.PlatformFile>[],
      directoryPicker: () async {
        directoryPicks += 1;
        return '/storage/emulated/0/Download/相册';
      },
    );

    final pick = gateway.pickFiles();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byKey(const ValueKey('folder_only_offer_title')),
        findsOneWidget);
    expect(storage.calls, isEmpty);

    await tester.tap(
        find.byKey(const ValueKey('folder_only_offer_pick_button')));
    final picked = await _finished(tester, pick);

    expect(directoryPicks, 0);
    expect(storage.methods, ['pickSendFolder', 'stageSafTree']);
    expect(picked, hasLength(1));
    final folder = picked.single;
    expect(folder.isDirectory, isTrue);
    expect(folder.name, _folderName);
    expect(folder.path, _stagedRoot(folder.stagingId!));
    expect(folder.path, isNot(contains('content:')));
  });

  testWidgets('pickFiles on Android: a cancelled tree pick ends the '
      'folder-only path with the empty list, and keeps the files in the '
      'append path', (tester) async {
    await _pumpApp(tester);
    storage.on('pickSendFolder', (_) => {'status': 'cancelled'});

    final only = await _androidGateway(
      filePicker: () async => const <fp.PlatformFile>[],
      folderOnlyOffer: () async => true,
    ).pickFiles();
    expect(only, isEmpty);

    final appended = await _androidGateway(
      filePicker: () async => _twoPlatformFiles,
      folderOffer: () async => true,
    ).pickFiles();
    expect(appended.map((entry) => entry.path), [_filePath1, _filePath2]);

    expect(storage.callsFor('pickSendFolder'), hasLength(2));
    expect(storage.methods, isNot(contains('stageSafTree')));
  });

  testWidgets('pickFiles on Android: P1-H still excludes /storage/msd/…, '
      'msd: and content:// FILE results', (tester) async {
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);
    final gateway = _androidGateway(
      filePicker: () async => [
        _PlatformFile('照片.zip', _filePath1, 1536),
        _PlatformFile('下载项', '/storage/msd/2184', 4096),
        _PlatformFile('msd项', 'msd:2185', 1),
        _PlatformFile('content项', 'content://downloads/document/12', 1),
      ],
      folderOffer: () async => false,
    );

    final picked = await _finished(tester, gateway.pickFiles());
    expect(picked.map((entry) => entry.path), [_filePath1],
        reason: 'P1-H：msd、content 与 /storage/msd/ 形态的文件都被剔除');
    expect(storage.calls, isEmpty);
  });

  // -----------------------------------------------------------------------
  // The append path: a staged folder named like a picked file.
  // -----------------------------------------------------------------------

  testWidgets('append path: a staged folder named like a picked file is '
      'released and refused with the duplicate-with-picked copy — 取消 '
      'keeps the files alone', (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => {..._selected, 'displayName': '报告.pdf'})
      ..on('stageSafTree', (call) => _okFor(_stageIdOf(call), '报告.pdf'));
    final gateway = _androidGateway(
      filePicker: () async => _twoPlatformFiles,
      folderOffer: () async => true,
    );

    final pick = gateway.pickFiles();
    await tester.pumpAndSettle();

    expect(find.byKey(_errorDialog), findsOneWidget);
    expect(find.text('无法发送此文件夹'), findsOneWidget);
    expect(_textOf(tester, _errorBody), _sameAsPicked);
    final stagedId = _stageIdOf(storage.callsFor('stageSafTree').single);
    expect(storage.argsOf('cleanupStagedSend'), [
      {'stageId': stagedId},
    ], reason: '被拒绝的副本交回');
    expect(find.byKey(_progressDialog), findsNothing);

    await tester.tap(find.byKey(_errorCancelButton));
    final picked = await _finished(tester, pick);

    expect(picked.map((entry) => entry.name), ['照片.zip', '报告.pdf'],
        reason: '取消：只保留文件');
    expect(picked.every((entry) => entry.stagingId == null), isTrue);
    expect(storage.callsFor('pickSendFolder'), hasLength(1));
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('append path: 重新选择 after the refusal picks again — a '
      'folder named differently is appended', (tester) async {
    await _pumpApp(tester);
    var picks = 0;
    storage
      ..on('pickSendFolder', (_) {
        picks += 1;
        return {
          ..._selected,
          'displayName': picks == 1 ? '照片.zip' : _folderName,
        };
      })
      ..on('stageSafTree', (call) => _okFor(
          _stageIdOf(call), picks == 1 ? '照片.zip' : _folderName));
    final gateway = _androidGateway(
      filePicker: () async => _twoPlatformFiles,
      folderOffer: () async => true,
    );

    final pick = gateway.pickFiles();
    await tester.pumpAndSettle();
    expect(_textOf(tester, _errorBody), _sameAsPicked);
    expect(storage.callsFor('cleanupStagedSend'), hasLength(1));

    await tester.tap(find.byKey(_retryButton));
    final picked = await _finished(tester, pick);

    expect(storage.callsFor('pickSendFolder'), hasLength(2));
    expect(storage.callsFor('cleanupStagedSend'), hasLength(1),
        reason: '只有重名的那一份被交回');
    expect(picked.map((entry) => entry.name),
        ['照片.zip', '报告.pdf', _folderName]);
    expect(picked.last.stagingId, isNotNull);
    expect(find.byKey(_errorDialog), findsNothing);
  });

  testWidgets('append path: a staged folder whose name differs from every '
      'file is appended without a word', (tester) async {
    await _pumpApp(tester);
    storage
      ..on('pickSendFolder', (_) => _selected)
      ..on('stageSafTree', _ok);
    final gateway = _androidGateway(
      filePicker: () async => _twoPlatformFiles,
      folderOffer: () async => true,
    );

    final picked = await _finished(tester, gateway.pickFiles());
    expect(picked.map((entry) => entry.name),
        ['照片.zip', '报告.pdf', _folderName]);
    expect(find.byKey(_errorDialog), findsNothing);
    expect(storage.methods, isNot(contains('cleanupStagedSend')));
  });

  testWidgets('desktop append: a plain folder named like a picked file is '
      'appended as before — the rule is for staged copies only',
      (tester) async {
    await _pumpApp(tester);
    final gateway = PluginFilePickGateway(
      isAndroidPlatform: () => false,
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async => r'C:\docs\照片.zip',
      folderOffer: () async => true,
    );

    final picked = await _finished(tester, gateway.pickFiles());
    expect(picked.map((entry) => entry.name),
        ['照片.zip', '报告.pdf', '照片.zip']);
    expect(picked.last.isDirectory, isTrue);
    expect(storage.calls, isEmpty);
    expect(find.byKey(_errorDialog), findsNothing);
  });

  // -----------------------------------------------------------------------
  // The compile-time switch, off.
  // -----------------------------------------------------------------------

  testWidgets('Android, folder send off: neither folder offer is shown — '
      'the append path answers the files alone, the folder-only path the '
      'empty list right after the cancelled file pick', (tester) async {
    await _pumpApp(tester);
    var offers = 0;
    final gateway = _androidGateway(
      folderSendEnabled: false,
      filePicker: () async => const <fp.PlatformFile>[],
      directoryPicker: () async => '/storage/emulated/0/Download/相册',
      folderOffer: () async {
        offers += 1;
        return true;
      },
      folderOnlyOffer: () async {
        offers += 1;
        return true;
      },
    );

    final appended = await _finished(
      tester,
      _androidGateway(
        folderSendEnabled: false,
        filePicker: () async => _twoPlatformFiles,
        folderOffer: () async {
          offers += 1;
          return true;
        },
      ).pickFiles(),
    );
    expect(appended.map((entry) => entry.path), [_filePath1, _filePath2]);

    final only = await _finished(tester, gateway.pickFiles());
    expect(only, isEmpty);

    expect(offers, 0, reason: '开关关闭：两个文件夹 offer 都不弹');
    expect(storage.calls, isEmpty);
    expect(find.byKey(const ValueKey('folder_offer_title')), findsNothing);
    expect(find.byKey(const ValueKey('folder_only_offer_title')), findsNothing);
  });

  testWidgets('Android, folder send off: pickSendDirectory shows the '
      'unsupported dialog, answers null, and never calls the channel',
      (tester) async {
    await _pumpApp(tester);
    final pick =
        _androidGateway(folderSendEnabled: false).pickSendDirectory();
    await _showDialog(tester);

    expect(find.text('暂时无法发送文件夹'), findsOneWidget);
    expect(
      _textOf(tester, _unsupportedBody),
      '这个版本还不支持直接发送文件夹，请只选择文件。',
    );
    expect(find.byKey(_unsupportedOk), findsOneWidget);
    expect(find.text('关闭'), findsOneWidget);
    expect(find.byKey(_progressDialog), findsNothing);

    await tester.tap(find.byKey(_unsupportedOk));
    expect(await pick, isNull);
    await tester.pumpAndSettle();
    expect(find.byKey(_unsupportedBody), findsNothing);
    expect(find.text('home page'), findsOneWidget);

    // Dismissing it answers the same.
    final dismissed =
        _androidGateway(folderSendEnabled: false).pickSendDirectory();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tapAt(const Offset(10, 10));
    expect(await dismissed, isNull);
    await tester.pumpAndSettle();

    expect(storage.calls, isEmpty);
  });

  testWidgets('Android, folder send off: with no navigator the pick '
      'answers null at once', (tester) async {
    expect(await _androidGateway(folderSendEnabled: false).pickSendDirectory(),
        isNull);
    expect(storage.calls, isEmpty);
  });
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Pumps an app whose root navigator is the one the gateway talks to.
Future<void> _pumpApp(WidgetTester tester) async {
  await tester.pumpWidget(
    const MaterialApp(home: Scaffold(body: Center(child: Text('home page')))),
  );
}

/// Lets the gateway run up to the dialog it is about to show and renders
/// that dialog's first frame (the first pump flushes the microtasks of
/// the pick, the second builds the route the gateway just pushed).
Future<void> _showDialog(WidgetTester tester) async {
  await tester.pump();
  await tester.pump();
}

/// Awaits a pick that must be over by now. The frames first let the
/// gateway run and its routes finish their transitions; a dialog that is
/// still up would keep the pick open, and fails the test at once with a
/// reason — instead of parking it until the test timeout.
Future<T> _finished<T>(WidgetTester tester, Future<T> pick) async {
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 500));
  await tester.pump();
  expect(find.byKey(_progressDialog), findsNothing,
      reason: '还在暂存：这次选择没有结束');
  expect(find.byKey(_errorDialog), findsNothing,
      reason: '出现了意料之外的拒绝弹窗');
  expect(find.byKey(_unsupportedBody), findsNothing,
      reason: '出现了意料之外的「暂不支持」弹窗');
  expect(find.byKey(const ValueKey('folder_offer_title')), findsNothing,
      reason: '出现了意料之外的文件夹 offer');
  expect(find.byKey(const ValueKey('folder_only_offer_title')), findsNothing,
      reason: '出现了意料之外的文件夹 offer');
  return pick;
}

String _textOf(WidgetTester tester, Key key) =>
    tester.widget<Text>(find.byKey(key)).data!;

LinearProgressIndicator _bar(WidgetTester tester) => tester
    .widget<LinearProgressIndicator>(find.descendant(
  of: find.byKey(_progressDialog),
  matching: find.byType(LinearProgressIndicator),
));

String _stageIdOf(MethodCall call) =>
    (call.arguments as Map<Object?, Object?>)['stageId']! as String;

/// Delivers a `stagingProgress` event the way the platform does — on the
/// same channel, while a staging is in flight.
void _progress({
  required String stageId,
  String phase = 'copy',
  int filesCopied = 0,
  int bytesCopied = 0,
  int totalBytesEstimate = -1,
}) {
  const MethodCodec codec = StandardMethodCodec();
  TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .handlePlatformMessage(
    _storage.name,
    codec.encodeMethodCall(MethodCall('stagingProgress', {
      'stageId': stageId,
      'phase': phase,
      'entriesSeen': 12,
      'filesCopied': filesCopied,
      'bytesCopied': bytesCopied,
      'totalBytesEstimate': totalBytesEstimate,
    })),
    (ByteData? reply) {},
  );
}

/// The Android branch on a desktop host. The startup cleanup is a no-op
/// unless a test is about it ([realStaleCleanup]: the real static).
PluginFilePickGateway _androidGateway({
  bool folderSendEnabled = true,
  bool realStaleCleanup = false,
  Future<List<fp.PlatformFile>> Function()? filePicker,
  Future<String?> Function()? directoryPicker,
  Future<bool> Function()? folderOffer,
  Future<bool> Function()? folderOnlyOffer,
}) {
  return PluginFilePickGateway(
    isAndroidPlatform: () => true,
    androidFolderSendEnabled: () => folderSendEnabled,
    staleStagingCleaner: realStaleCleanup ? null : () async {},
    filePicker: filePicker,
    directoryPicker: directoryPicker,
    folderOffer: folderOffer,
    folderOnlyOffer: folderOnlyOffer,
  );
}

PickedEntry _staged(String stageId) => PickedEntry(
      path: _stagedRoot(stageId),
      name: _folderName,
      isDirectory: true,
      stagingId: stageId,
    );

PickedEntry _file(String path) => PickedEntry(
      path: path,
      name: 'x',
      isDirectory: false,
      sizeBytes: 1,
    );

/// The scripted `dev.ak/storage`: every call is recorded, and each method
/// answers from [on], stays parked until its [hold] is completed, or —
/// when nobody scripted it — throws [MissingPluginException].
class _StorageChannel {
  final List<MethodCall> calls = [];
  final Map<String, FutureOr<Object?> Function(MethodCall call)> _answers = {};
  final Map<String, Completer<Object?>> _held = {};

  void install() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(_storage, _handle);
  }

  void uninstall() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(_storage, null);
  }

  Future<Object?> _handle(MethodCall call) {
    calls.add(call);
    final held = _held.remove(call.method);
    if (held != null) {
      return held.future;
    }
    final answer = _answers[call.method];
    if (answer == null) {
      throw MissingPluginException(call.method);
    }
    return Future<Object?>.sync(() => answer(call));
  }

  /// Scripts the [answer] of every later call of [method].
  void on(String method, FutureOr<Object?> Function(MethodCall call) answer) {
    _answers[method] = answer;
  }

  /// Parks the NEXT call of [method] until the test completes the result
  /// — a scripted answer of the same method is NOT run for that call.
  Completer<Object?> hold(String method) =>
      _held[method] = Completer<Object?>();

  List<MethodCall> callsFor(String method) => [
        for (final call in calls)
          if (call.method == method) call,
      ];

  List<String> get methods => [for (final call in calls) call.method];

  List<Map<Object?, Object?>> argsOf(String method) => [
        for (final call in callsFor(method))
          call.arguments as Map<Object?, Object?>,
      ];
}

/// An app whose OUTERMOST navigator drops every route pushed on it — the
/// state a staging is in when its progress dialog is not (or no longer)
/// on the navigator while its call is still answered.
class _DroppingNavigatorApp extends StatelessWidget {
  const _DroppingNavigatorApp();

  @override
  Widget build(BuildContext context) {
    return Localizations(
      locale: const Locale('en', 'US'),
      // What a MaterialApp would provide; a DialogRoute reads them.
      delegates: const [
        DefaultWidgetsLocalizations.delegate,
        DefaultMaterialLocalizations.delegate,
      ],
      child: Theme(
        data: ThemeData(),
        child: const Directionality(
          textDirection: TextDirection.ltr,
          child: _DroppingNavigator(),
        ),
      ),
    );
  }
}

/// A route for the one page under the dropping navigator.
class _HomePage extends MaterialPageRoute<void> {
  _HomePage() : super(builder: (_) => const Center(child: Text('home page')));
}

class _DroppingNavigator extends Navigator {
  const _DroppingNavigator() : super(onGenerateRoute: _homeRoute);

  static Route<void> _homeRoute(Object? settings) => _HomePage();

  @override
  NavigatorState createState() => _DroppingNavigatorState();
}

class _DroppingNavigatorState extends NavigatorState {
  @override
  Future<T?> push<T extends Object?>(Route<T> route) async => null;
}

/// Minimal `PlatformFile` double (the plugin's class can only be
/// extended) — just what the gateway reads: name, path, size. Same shape
/// as the one of `home_flow_test.dart`, which is private there.
base class _PlatformFile extends fp.PlatformFile {
  _PlatformFile(this.name, this.path, this.size);

  @override
  final String name;

  @override
  final String path;

  final int size;

  @override
  Uri get uri => Uri.file(path);

  /// The gateway never opens the file.
  @override
  Never get xFile => throw UnsupportedError('not used by the gateway');

  @override
  int? lengthSync() => size;

  @override
  Future<int?> length() async => size;

  @override
  Future<Uint8List> readAsBytes() async => Uint8List(0);

  @override
  Stream<Uint8List> readAsByteStream() => const Stream.empty();
}
