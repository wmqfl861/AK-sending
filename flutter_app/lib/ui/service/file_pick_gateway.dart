/// Seam over the system file/folder pickers (`file_picker` plugin: Android
/// SAF, Windows common dialogs) so wave-28 pages stay testable — widget
/// tests inject a fake and never touch the platform channel.
///
/// file_picker 13.x notes: `FilePicker.pickFiles()` is a static
/// multi-select API (empty list = user cancelled) and
/// `FilePicker.getDirectoryPath()` returns null on cancel. The plugin's
/// one mixed files-and-folders dialog (`pickFileAndDirectoryPaths`) is
/// macOS only, so no single platform dialog offers「可多选，含文件夹」on
/// Android or Windows.
///
/// X4 P1-2 (batch X codex review): the X4 send path is「可多选，含文件夹」
/// (handoff §4.4), yet the platform multi-file pick selects FILES only —
/// only an injected fake could pick a folder.
/// [PluginFilePickGateway.pickFiles] therefore follows every NON-EMPTY
/// (and fully filesystem-pathed — see the provider-URI exclusions below)
/// file pick with ONE optional app-side offer to add a whole folder:
///
/// * 「添加文件夹」 runs the single folder pick — off Android the same
///   `getDirectoryPath()` as [PluginFilePickGateway.pickDirectory], on
///   Android the staged tree pick (batch Z, below) — and appends that
///   entry;
/// * 「不用了」 — or dismissing the dialog, or cancelling the folder pick —
///   keeps the files alone;
/// * a cancelled FILE pick opens the folder-ONLY offer (round 3): choosing
///   it sends just one folder; declining it — or cancelling the folder
///   pick — is the overall cancel (empty list).
///
/// The [FilePickGateway] interface is unchanged, so fakes and walkthroughs
/// that inject their own gateway never see the offer.
///
/// P1-H (fourth review): only FILESYSTEM paths leave the gateway. A pick
/// result (file or folder) whose path is not absolute-on-disk — no leading
/// `/`, no `X:\`/`X:/` drive letter, no `\\` UNC root, or a scheme-like
/// `xxx:` provider URI — is EXCLUDED from the returned entries: Android
/// 11+ Downloads-provider folder picks can return `msd:<numeric id>` URIs
/// that are selectable but UNSENDABLE (`sendFiles` needs filesystem
/// paths). If the whole selection is excluded, the folder-only path
/// behaves like a cancelled folder pick (empty overall result) and the
/// append path keeps the remaining files.
///
/// Batch Z (Android folder SEND): the plugin's Android folder pick can
/// only hand back what the engine cannot read — a `content://` tree URI
/// (SAF) or an `msd:` pseudo path — so the two folder paths above go
/// through [PluginFilePickGateway.pickSendDirectory]. On Android it uses
/// the `dev.ak/storage` channel: the system tree picker, then a copy of
/// the folder into app-private storage (the root name, the relative
/// structure, empty sub-directories and zero-byte files kept), and only
/// the staged root's FILESYSTEM path leaves the gateway. The read grant
/// of the picked tree is held only while a staging runs — the platform
/// takes and releases it per staging call. Off Android
/// [PluginFilePickGateway.pickSendDirectory] IS
/// [PluginFilePickGateway.pickDirectory], whose meaning does not change:
/// the settings page uses it for the RECEIVE save directory.
library;

import 'dart:async' show unawaited;
import 'dart:io' show Platform;
import 'dart:math' show Random;

import 'package:file_picker/file_picker.dart' as fp;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart'
    show MethodCall, MethodChannel, PlatformException;

import 'saf_send_dialogs.dart';
import 'ui_platform.dart';

/// One user-chosen item: a file (with size when the platform reports it)
/// or a directory (folder pick). [path] is what goes into
/// `SimpleTransferService.sendFiles`.
class PickedEntry {
  const PickedEntry({
    required this.path,
    required this.name,
    required this.isDirectory,
    this.sizeBytes,
    this.stagingId,
    this.stagedFileCount,
    this.stagedByteCount,
  });

  final String path;
  final String name;
  final bool isDirectory;

  /// File size in bytes when known; null for directories/unknown.
  final int? sizeBytes;

  /// Batch Z: non-null only for a folder the gateway staged into
  /// app-private storage before handing it over (the Android SAF send
  /// path). Whoever holds the entry owns the staged copy and hands it back
  /// through [FilePickGateway.releaseStaged].
  final String? stagingId;

  /// Batch Z: what the native stager copied for [stagingId]; null when the
  /// entry is not staged.
  final int? stagedFileCount;
  final int? stagedByteCount;
}

/// Chooser surface used by the connection-detail page and the settings
/// page.
abstract class FilePickGateway {
  /// Same-directory multi-file pick (Android SAF multi-select / Windows
  /// dialog). Returns an empty list when the user cancels.
  Future<List<PickedEntry>> pickFiles();

  /// Single-folder pick. Returns null on cancel.
  Future<PickedEntry?> pickDirectory();

  /// Batch Z: the SEND-side single-folder pick — what the send paths call
  /// where [pickDirectory] used to. Same shape (an entry or null), but on
  /// Android it goes through the SAF tree picker and a staged copy, since
  /// the engine only takes filesystem paths. [pickDirectory] itself keeps
  /// its plain meaning: the settings page uses it for the RECEIVE save
  /// directory.
  ///
  /// Every class that `implements FilePickGateway` declares this method:
  /// Dart does not inherit bodies through `implements`.
  Future<PickedEntry?> pickSendDirectory();

  /// Batch Z: hands the staged copies behind [entries] back to the
  /// platform. Entries without a [PickedEntry.stagingId] are ignored; a
  /// call is idempotent and never throws.
  Future<void> releaseStaged(Iterable<PickedEntry> entries);
}

/// Production gateway over the `file_picker` plugin.
///
/// The function members are the test seams — the plugin's static calls
/// cannot run without a platform, the same function-seam style as the
/// other services in this repo — and each defaults to the real call. The
/// batch Z seams cover the `dev.ak/storage` calls the same way.
class PluginFilePickGateway implements FilePickGateway {
  /// The seams are optional and default to the real calls: `const
  /// PluginFilePickGateway()` stays the production gateway every page
  /// already defaults to.
  const PluginFilePickGateway({
    this._filePicker,
    this._directoryPicker,
    this._folderOffer,
    this._folderOnlyOffer,
    this._isAndroidPlatform,
    this._sendFolderPicker,
    this._safStager,
    this._stagingCanceller,
    this._stagedCleaner,
    this._staleStagingCleaner,
    this._androidFolderSendEnabled,
  });

  /// The platform multi-file pick (`FilePicker.pickFiles`): an empty list
  /// means the user cancelled.
  final Future<List<fp.PlatformFile>> Function()? _filePicker;

  /// The platform single-folder pick (`FilePicker.getDirectoryPath`):
  /// null means the user cancelled.
  final Future<String?> Function()? _directoryPicker;

  /// The app-side offer「还要添加整个文件夹吗？」: true = the user chose
  /// 「添加文件夹」. Defaults to [_showFolderOffer].
  final Future<bool> Function()? _folderOffer;

  /// The app-side offer after a CANCELLED file pick:「只发送一个文件夹
  /// 吗？」— true = pick one folder and send only it. Defaults to
  /// [_showFolderOnlyOffer].
  final Future<bool> Function()? _folderOnlyOffer;

  /// Review round 6: the Android-only provider-path exclusions gate on
  /// this (injectable so tests on a desktop host can drive the Android
  /// branch deterministically). Defaults to `Platform.isAndroid`.
  final bool Function()? _isAndroidPlatform;

  /// Batch Z: the Android tree pick (`pickSendFolder` on
  /// `dev.ak/storage`) — the answer map: `status` `selected` with
  /// `treeUri` and `displayName`, or `cancelled`. Defaults to the real
  /// channel call.
  final Future<Map<Object?, Object?>?> Function()? _sendFolderPicker;

  /// Batch Z: the staging of a picked tree (`stageSafTree(treeUri,
  /// stageId)`) — the ok map (`path`, `displayName`, `fileCount`,
  /// `byteCount`), or the channel's `PlatformException`. It answers only
  /// once the platform side has stopped for good. Defaults to the real
  /// channel call.
  ///
  /// Two refusals worth knowing by name: an id that already carries a
  /// finished staging is `STAGE_IO_FAILED` / `id-in-use` (nothing is
  /// deleted), and a cancelled staging answers `STAGE_CANCELLED` only
  /// when its directory is gone — otherwise `STAGE_IO_FAILED` /
  /// `leftover`, so the refusal dialog can follow a 「取消」.
  final Future<Map<Object?, Object?>?> Function(String treeUri, String stageId)?
      _safStager;

  /// Batch Z: asks the staging [stageId] to stop (`cancelStaging`) — a
  /// flag on the platform side only. Defaults to the real channel call.
  final Future<void> Function(String stageId)? _stagingCanceller;

  /// Batch Z: hands one staged copy back (`cleanupStagedSend(stageId)`,
  /// idempotent). The platform answers `{"removed": true}` or
  /// `{"removed": false}` (something is left behind), or refuses with
  /// `STAGE_IO_FAILED` / `busy` while that staging still runs — this side
  /// reads none of it. Defaults to the real channel call.
  final Future<void> Function(String stageId)? _stagedCleaner;

  /// Batch Z: the stale cleanup the first staging of the isolate waits
  /// for. Defaults to [cleanupStaleStaging] — once per Dart isolate.
  final Future<void> Function()? _staleStagingCleaner;

  /// Batch Z: the compile-time switch [kAndroidFolderSendEnabled],
  /// injectable so tests on a desktop host can drive both values.
  final bool Function()? _androidFolderSendEnabled;

  @override
  Future<List<PickedEntry>> pickFiles() async {
    final files = await (_filePicker ?? fp.FilePicker.pickFiles)();
    if (files.isEmpty) {
      // X4 P1-2 (review round 3): a cancelled FILE pick is NOT yet the
      // end — the user may have come here to send ONLY a folder (§4.4
      // 「可多选，含文件夹」includes the folder-only case). One offer
      // follows; declining it (or cancelling the folder pick) is the
      // overall cancel, exactly as before.
      if (_folderSendOffered() &&
          await (_folderOnlyOffer ?? _showFolderOnlyOffer)()) {
        final folder = await pickSendDirectory();
        if (folder != null) {
          return <PickedEntry>[folder];
        }
      }
      return const <PickedEntry>[];
    }
    final entries = <PickedEntry>[
      // P1-H: EXCLUDE every non-filesystem path (e.g. an `msd:` provider
      // URI) — selectable but unsendable. A null platform path fails the
      // same check and is excluded too.
      for (final file in files)
        if (_isFileSystemPath(file.path ?? ''))
          PickedEntry(
            path: file.path!,
            name: file.name,
            isDirectory: false,
            sizeBytes: file.lengthSync(),
          ),
    ];
    if (entries.isEmpty) {
      // P1-H: the WHOLE (platform-non-empty) selection was provider URIs —
      // nothing sendable came back, so the pick returns the empty list
      // DIRECTLY: the append offer never fires (there was no real file
      // selection to build on) and the caller treats it as a cancel.
      return const <PickedEntry>[];
    }
    // X4 P1-2: ONE optional whole folder after a real selection.
    if (_folderSendOffered() &&
        await (_folderOffer ?? _showFolderOffer)()) {
      final folder = await _pickAppendableFolder(entries);
      if (folder != null) {
        entries.add(folder); // a cancelled folder pick keeps the files
      }
    }
    return entries;
  }

  bool _isAndroid() => _isAndroidPlatform?.call() ?? Platform.isAndroid;

  bool _folderSendEnabled() =>
      _androidFolderSendEnabled?.call() ?? kAndroidFolderSendEnabled;

  /// Batch Z: whether the send paths offer a folder at all — everywhere,
  /// except on Android with [kAndroidFolderSendEnabled] switched off.
  bool _folderSendOffered() => !_isAndroid() || _folderSendEnabled();

  /// Batch Z: the folder to append to [files] — [pickSendDirectory] until
  /// one can go with them. A STAGED folder named like a picked file is
  /// refused: an appended folder's entries reach the receiver under its
  /// root name, and the engine refuses a run whose root names repeat. The
  /// copy is released and the refusal dialog offers the choice — 「取消」
  /// keeps the files alone. A folder that is not staged (the desktop
  /// pick) passes unchanged.
  Future<PickedEntry?> _pickAppendableFolder(List<PickedEntry> files) async {
    while (true) {
      final folder = await pickSendDirectory();
      if (folder == null ||
          folder.stagingId == null ||
          !_isNamedLikeAFile(folder, files)) {
        return folder;
      }
      // Not awaited: the refusal can be shown while the copy is removed.
      unawaited(releaseStaged(<PickedEntry>[folder]));
      if (!await _showStagingError(_nameConflict, 'duplicate-with-picked')) {
        return null;
      }
    }
  }

  /// Is [folder] named exactly like one of the picked [files]? The
  /// engine's rule is the plain string comparison, so is this.
  static bool _isNamedLikeAFile(PickedEntry folder, List<PickedEntry> files) =>
      files.any((file) => file.name == folder.name);

  /// Batch Z: compile-time switch of the Android folder-send entry. Off =
  /// Android shows no folder offer and [pickSendDirectory] only says the
  /// folder is unsupported — it never hands the engine a picker pseudo path.
  static const bool kAndroidFolderSendEnabled = true;

  /// The native half of the Android folder send (batch Z): the channel
  /// `MainActivity` owns. Const by name — tests intercept it through the
  /// test binding's mock method-call handler.
  static const MethodChannel _storageChannel =
      MethodChannel('dev.ak/storage');

  static const String _stageCancelled = 'STAGE_CANCELLED';
  static const String _nameConflict = 'STAGE_NAME_CONFLICT';
  static const String _ioFailed = 'STAGE_IO_FAILED';

  /// Source of the staging ids: 32 lowercase hex digits from 16 random
  /// bytes each, the format the platform side validates.
  static final Random _stageIdRandom = Random.secure();

  /// Whether an Android folder pick is open. The Android branch admits one
  /// at a time — a second caller gets an immediate cancel. Static: the
  /// production gateway is a `const`, built several times.
  static bool _sendFolderPickOpen = false;

  /// Whether the `stagingProgress` listener is on the channel — installed
  /// once per Dart isolate, BEFORE the first `stageSafTree` call.
  static bool _stagingProgressListening = false;

  /// The staging in flight (the id whose progress events are accepted)
  /// and whether its cancel was requested.
  static String? _stagingStageId;
  static bool _stagingCancelRequested = false;

  /// The latest event of [_stagingStageId]; null until the first one.
  static final ValueNotifier<SafSendStagingProgress?> _stagingProgress =
      ValueNotifier<SafSendStagingProgress?>(null);

  /// The stale cleanup of this Dart isolate — started by its first caller
  /// and shared by every later one.
  static Future<void>? _staleStagingCleanup;

  @override
  Future<PickedEntry?> pickSendDirectory() async {
    if (!_isAndroid()) {
      return pickDirectory(); // desktop: unchanged behaviour
    }
    if (_sendFolderPickOpen) {
      return null; // one pick at a time: a second caller gets a cancel
    }
    _sendFolderPickOpen = true;
    try {
      if (!_folderSendEnabled()) {
        await _showFolderSendUnsupported();
        return null;
      }
      // Nothing is staged before the leftovers of earlier runs are gone.
      // The app start already ran the cleanup — this await is satisfied
      // at once then, and IS the run when it comes first. Once per Dart
      // isolate, see [cleanupStaleStaging].
      await (_staleStagingCleaner ?? cleanupStaleStaging)();
      final folder = await _pickStagedFolder();
      return folder;
    } finally {
      _sendFolderPickOpen = false;
    }
  }

  /// The Android loop: pick a tree, stage it. A refusal asks whether to
  /// start over (「重新选择」); null is the user's cancel at any point.
  Future<PickedEntry?> _pickStagedFolder() async {
    while (true) {
      final (String, String) refusal;
      try {
        final folder = await _stageOnce();
        return folder;
      } on PlatformException catch (e) {
        if (e.code == _stageCancelled) {
          return null; // the user cancelled; the dialog closed with it
        }
        refusal = (e.code, e.message ?? '');
      } on _StagingRefused catch (e) {
        refusal = (e.code, e.token);
      }
      if (!await _showStagingError(refusal.$1, refusal.$2)) {
        return null;
      }
    }
  }

  /// One attempt: the system tree picker, then the staging under the
  /// progress dialog. Null = the user backed out of the picker, or
  /// cancelled the staging.
  Future<PickedEntry?> _stageOnce() async {
    // The picker runs OUTSIDE the app and may refuse by itself.
    final picked = await (_sendFolderPicker ?? _pickSendFolderOnChannel)();
    if (picked?['status'] == 'cancelled') {
      return null;
    }
    final treeUri = picked?['treeUri'];
    final displayName = picked?['displayName'];
    if (picked?['status'] != 'selected' ||
        treeUri is! String ||
        treeUri.isEmpty ||
        displayName is! String ||
        !_isOneSegment(displayName)) {
      // Not a shape this side can stage — a name that is not ONE segment
      // could never be the staged root's name (see [_endsWithStaging]).
      throw const _StagingRefused(_ioFailed, 'io');
    }
    return _stageTree(treeUri, displayName);
  }

  /// Stages [treeUri] (the folder [displayName] names) under the progress
  /// dialog, with a fresh staging id.
  Future<PickedEntry?> _stageTree(String treeUri, String displayName) async {
    final navigator = _rootNavigator();
    if (navigator == null || !navigator.mounted) {
      // Nowhere to show progress or to cancel from: nothing is staged
      // unattended, and no dialog is waited for that cannot be shown.
      return null;
    }
    final stageId = _newStageId();
    // The listener is on the channel BEFORE the call, so no event of this
    // staging is missed.
    _listenForStagingProgress();
    _stagingStageId = stageId;
    _stagingCancelRequested = false;
    _stagingProgress.value = null;
    final dialog = DialogRoute<void>(
      context: navigator.context,
      barrierDismissible: false,
      builder: (context) => SafSendProgressDialog(
        progress: _stagingProgress,
        onCancel: () => _cancelStaging(stageId),
      ),
    );
    unawaited(navigator.push(dialog));
    try {
      final staged =
          await (_safStager ?? _stageSafTreeOnChannel)(treeUri, stageId);
      final folder = _stagedEntryOf(stageId, staged, displayName);
      if (folder != null && !_stagingCancelRequested) {
        return folder;
      }
      // A copy the user had cancelled after all, or one this side cannot
      // hand over. The staging call has answered, so releasing it is safe
      // (it is never released while the call is still open).
      await _cleanupStagedSend(stageId);
      if (folder != null) {
        return null;
      }
      throw const _StagingRefused(_ioFailed, 'io');
    } finally {
      _closeProgressDialog(navigator, dialog);
      _stagingStageId = null;
    }
  }

  /// The entry of a staged folder — ONLY for an ok answer that is this
  /// staging's own, see [_isStagedRootOf]: it must carry the requested
  /// [stageId], the path of the copy made for [displayName], and its
  /// three counts. The folder keeps the name the pick gave it, since
  /// that is the name the staged root must carry. Null for anything
  /// else: an answer this side cannot hand over.
  PickedEntry? _stagedEntryOf(
    String stageId,
    Map<Object?, Object?>? staged,
    String displayName,
  ) {
    final path = staged?['path'];
    final fileCount = _countOf(staged?['fileCount']);
    final byteCount = _countOf(staged?['byteCount']);
    final emptyDirectoryCount = _countOf(staged?['emptyDirectoryCount']);
    if (staged?['status'] != 'ok' ||
        staged?['stageId'] != stageId ||
        path is! String ||
        !_isStagedRootOf(stageId, displayName, path) ||
        fileCount == null ||
        byteCount == null ||
        emptyDirectoryCount == null) {
      return null;
    }
    return PickedEntry(
      path: path,
      name: displayName,
      isDirectory: true,
      stagingId: stageId,
      stagedFileCount: fileCount,
      stagedByteCount: byteCount,
    );
  }

  /// Is [path] the staged root of THIS staging — the one [stageId] made
  /// for the folder [displayName] names? Only such a path is handed over:
  /// a filesystem path like any other (P1-H: never the tree URI that was
  /// picked, never an `msd:` pseudo path) that ends with the staging's
  /// own `<stageId>/payload/<displayName>`. Anything else — another
  /// staging's path, a relative or a Windows one — is not this answer's
  /// to give, however well-formed it looks.
  bool _isStagedRootOf(String stageId, String displayName, String path) =>
      _isFileSystemPath(path) &&
      _endsWithStaging(stageId, displayName, path);

  /// Does [path] end with `<stageId>/payload/<displayName>`, as an
  /// absolute path of plain segments with a base in front of the three?
  /// Plain means: no `\`, no `:` and no control character anywhere, and
  /// no segment that is empty, `.` or `..`.
  static bool _endsWithStaging(
    String stageId,
    String displayName,
    String path,
  ) {
    if (!path.startsWith('/') ||
        path.contains(r'\') ||
        path.contains(':') ||
        path.codeUnits.any((unit) => unit < 0x20 || unit == 0x7F)) {
      return false;
    }
    final segments = path.substring(1).split('/');
    if (segments.length < 4) {
      return false; // a base, then the three of the suffix
    }
    for (final segment in segments) {
      if (!_isOneSegment(segment)) {
        return false;
      }
    }
    return segments[segments.length - 3] == stageId &&
        segments[segments.length - 2] == 'payload' &&
        segments.last == displayName;
  }

  /// Is [name] ONE path segment — what the pick's `displayName` has to
  /// be, since it names the staged root? What a name may otherwise
  /// contain is the platform's own rule, with its own reasons to report.
  static bool _isOneSegment(String name) =>
      name.isNotEmpty && name != '.' && name != '..' && !name.contains('/');

  /// One of the ok answer's three counts — null unless it is an integer
  /// of 0 or more.
  static int? _countOf(Object? value) =>
      value is int && value >= 0 ? value : null;

  /// 「取消」: asks the platform to stop [stageId] — once. The staging
  /// call itself is what keeps the dialog up until it answers.
  void _cancelStaging(String stageId) {
    if (_stagingCancelRequested) {
      return;
    }
    _stagingCancelRequested = true;
    unawaited(_bestEffort(
      () => (_stagingCanceller ?? _cancelStagingOnChannel)(stageId),
    ));
  }

  @override
  Future<void> releaseStaged(Iterable<PickedEntry> entries) async {
    final ids = {for (final entry in entries) entry.stagingId}.nonNulls;
    for (final id in ids) {
      await _cleanupStagedSend(id);
    }
  }

  Future<void> _cleanupStagedSend(String stageId) => _bestEffort(
        () => (_stagedCleaner ?? _cleanupStagedSendOnChannel)(stageId),
      );

  /// Batch Z: removes staging leftovers of earlier runs (interrupted
  /// copies, expired finished ones) and the read grants they left behind.
  /// Android only; errors are swallowed.
  ///
  /// Runs ONCE PER DART ISOLATE — not once per Android process: a rebuilt
  /// engine is a new isolate with fresh statics, and runs it again. Within
  /// an isolate the app start calls it, and the first staging waits for
  /// the same run instead of starting another.
  static Future<void> cleanupStaleStaging() =>
      _staleStagingCleanup ??= _runStaleStagingCleanup();

  static Future<void> _runStaleStagingCleanup() async {
    try {
      // A static has no instance seam: the Android fact comes from the
      // UI's platform seam.
      if (UiPlatform.isAndroid) {
        await _storageChannel.invokeMethod<void>('cleanupStaleStaging');
      }
    } on Object catch (_) {
      // Best effort by contract: whatever it left is removed next start.
    }
  }

  /// Test seam: puts the per-isolate statics back to their state at the
  /// start of an isolate (the stale-cleanup run, the listener guard, the
  /// open-pick guard), so a test isolate can observe the FIRST staging of
  /// an isolate more than once — and one test's abandoned pick cannot
  /// cancel the next test's. Tests call it from `setUp`; production never
  /// does.
  @visibleForTesting
  static void resetProcessStateForTest() {
    _staleStagingCleanup = null;
    _stagingProgressListening = false;
    _sendFolderPickOpen = false;
  }

  /// Runs a best-effort channel call: a failure never reaches the caller.
  static Future<void> _bestEffort(Future<void> Function() call) async {
    try {
      await call();
    } on Object catch (_) {
      // Best effort by contract: ignored on purpose.
    }
  }

  /// Installs the `stagingProgress` listener — idempotent, since every
  /// `const PluginFilePickGateway()` shares this one guard.
  static void _listenForStagingProgress() {
    if (_stagingProgressListening) {
      return;
    }
    _stagingProgressListening = true;
    _storageChannel.setMethodCallHandler(_onStorageCall);
  }

  /// The platform→Dart half of `dev.ak/storage`: the `stagingProgress`
  /// events of the staging in flight. Anything else is ignored.
  static Future<Object?> _onStorageCall(MethodCall call) async {
    if (call.method != 'stagingProgress') {
      return null;
    }
    final args = call.arguments;
    final foreign = _stagingStageId == null ||
        args is! Map ||
        args['stageId'] != _stagingStageId;
    if (foreign) {
      return null; // nothing in flight, or a late event of an earlier one
    }
    _stagingProgress.value = SafSendStagingProgress(
      copying: args['phase'] == 'copy',
      filesCopied: _intOf(args['filesCopied']) ?? 0,
      bytesCopied: _intOf(args['bytesCopied']) ?? 0,
      totalBytesEstimate: _intOf(args['totalBytesEstimate']) ?? -1,
    );
    return null;
  }

  /// Closes the progress route THIS staging pushed — and nothing else.
  /// The route object is the proof of ownership: once it is gone (the
  /// navigator was rebuilt underneath, the route was removed), nothing is
  /// popped — a blind `pop()` would close somebody else's page.
  static void _closeProgressDialog(
    NavigatorState navigator,
    Route<void> dialog,
  ) {
    if (!dialog.isActive) {
      return;
    }
    if (dialog.isCurrent) {
      navigator.pop();
    } else {
      navigator.removeRoute(dialog); // something was pushed on top of it
    }
  }

  static String _newStageId() {
    final bytes = [
      for (var i = 0; i < 16; i += 1) _stageIdRandom.nextInt(256),
    ];
    return [
      for (final byte in bytes) byte.toRadixString(16).padLeft(2, '0'),
    ].join();
  }

  static int? _intOf(Object? value) => value is int ? value : null;

  static Future<Map<Object?, Object?>?> _pickSendFolderOnChannel() =>
      _storageChannel.invokeMethod<Map<Object?, Object?>>('pickSendFolder');

  static Future<Map<Object?, Object?>?> _stageSafTreeOnChannel(
    String treeUri,
    String stageId,
  ) =>
      _storageChannel.invokeMethod<Map<Object?, Object?>>('stageSafTree', {
        'treeUri': treeUri,
        'stageId': stageId,
      });

  /// 「取消」on the channel: sets the platform's flag — the staging call
  /// itself is what answers.
  static Future<void> _cancelStagingOnChannel(String stageId) =>
      _storageChannel.invokeMethod<void>('cancelStaging', {'stageId': stageId});

  /// The platform answers `{"removed": true}` or `{"removed": false}`;
  /// this side asks for nothing back — the release is best effort either
  /// way, and a refusal (`STAGE_IO_FAILED` / `busy` for a staging that
  /// still runs) is swallowed by the caller.
  static Future<void> _cleanupStagedSendOnChannel(String stageId) =>
      _storageChannel
          .invokeMethod<void>('cleanupStagedSend', {'stageId': stageId});

  @override
  Future<PickedEntry?> pickDirectory() async {
    final path =
        await (_directoryPicker ?? fp.FilePicker.getDirectoryPath)();
    if (path == null) {
      return null;
    }
    if (!_isFileSystemPath(path)) {
      // P1-H: a provider URI (e.g. an Android 11+ Downloads `msd:<id>`)
      // is not a sendable folder — behave exactly like a cancelled
      // folder pick: the folder-only path yields the empty overall
      // result, the append path keeps the files.
      return null;
    }
    final name = path.split(RegExp(r'[\\/]')).last;
    return PickedEntry(
      path: path,
      name: name.isEmpty ? path : name,
      isDirectory: true,
    );
  }

  /// P1-H (fourth review): is [path] a FILESYSTEM path the engine can
  /// send? True only for an absolute on-disk path — a POSIX root
  /// (`/…`), a Windows drive (``C:\…` / `C:/…``), or a UNC root
  /// (`\\…`). Everything else — a relative path, an empty path, or a
  /// scheme-like provider URI (`msd:123`, `content://…`) — is excluded:
  /// the Downloads provider on Android 11+ hands out `msd:<numeric id>`
  /// URIs that `sendFiles` cannot use.
  bool _isFileSystemPath(String path) {
    final trimmed = path.trim();
    if (trimmed.isEmpty) {
      return false;
    }
    // Review rounds 5/6: the Android file_picker plugin can also hand
    // back a path BUILT from a provider document id (`msd:2184` →
    // `/storage/msd/2184/…`) — filesystem-SHAPED but not engine-sendable.
    // Android-only: a desktop user with a genuinely named `/storage/msd`
    // directory keeps it.
    if ((_isAndroidPlatform?.call() ?? Platform.isAndroid) &&
        trimmed.startsWith('/storage/msd/')) {
      return false;
    }
    if (trimmed.startsWith('/') || trimmed.startsWith(r'\\')) {
      return true;
    }
    return RegExp(r'^[A-Za-z]:[\\/]').hasMatch(trimmed);
  }
}

/// X4 P1-2: the app-side offer shown after a non-empty multi-file pick.
/// True only for 「添加文件夹」; 「不用了」, dismissing the dialog, or
/// having no app UI to ask on all answer false (the pick never blocks on
/// the offer).
///
/// The gateway has no `BuildContext` of its own — the frozen `pickFiles()`
/// signature carries none — so the offer reaches the app's root navigator
/// through the element tree, instead of a navigator key the shell would
/// have to wire in.
Future<bool> _showFolderOffer() async {
  final navigator = _rootNavigator();
  if (navigator == null || !navigator.mounted) {
    return false; // nowhere to ask: keep the files, never block the pick
  }
  final add = await navigator.push<bool>(
    DialogRoute<bool>(
      context: navigator.context,
      builder: (context) => const _FolderOfferDialog(),
    ),
  );
  return add == true;
}

/// X4 P1-2 (review round 3): the offer after a CANCELLED file pick — the
/// folder-ONLY path of the send main entry. Same rules as
/// [_showFolderOffer]: nowhere to ask answers false (the pick returns
/// empty, never blocks).
Future<bool> _showFolderOnlyOffer() async {
  final navigator = _rootNavigator();
  if (navigator == null || !navigator.mounted) {
    return false;
  }
  final pick = await navigator.push<bool>(
    DialogRoute<bool>(
      context: navigator.context,
      builder: (context) => const _FolderOnlyOfferDialog(),
    ),
  );
  return pick == true;
}

/// Batch Z: the refusal dialog of a folder send —「无法发送此文件夹」,
/// its body chosen by the error [code] and reason [token]. True only for
/// 「重新选择」; 「取消」, dismissing the dialog, or having no app UI to
/// ask on all answer false (the pick ends cancelled, never blocks). Same
/// rules as [_showFolderOffer].
Future<bool> _showStagingError(String code, String token) async {
  final navigator = _rootNavigator();
  if (navigator == null || !navigator.mounted) {
    return false;
  }
  final retry = await navigator.push<bool>(
    DialogRoute<bool>(
      context: navigator.context,
      builder: (context) =>
          SafSendStagingErrorDialog(code: code, token: token),
    ),
  );
  return retry == true;
}

/// Batch Z: the info dialog of a build without the Android folder send.
/// Nothing to answer — it only says why the pick ends cancelled.
Future<void> _showFolderSendUnsupported() async {
  final navigator = _rootNavigator();
  if (navigator == null || !navigator.mounted) {
    return;
  }
  await navigator.push<void>(
    DialogRoute<void>(
      context: navigator.context,
      builder: (context) => const SafSendFolderUnsupportedDialog(),
    ),
  );
}

/// Batch Z: a staging attempt this side refuses — an answer that is not
/// one of the channel's shapes. Carries what the channel's
/// `PlatformException` would: the error code and its reason token.
class _StagingRefused implements Exception {
  const _StagingRefused(this.code, this.token);

  final String code;
  final String token;
}

/// The app's root navigator, wherever it sits in the element tree (the
/// outermost `Navigator` — a depth-first walk meets it first).
NavigatorState? _rootNavigator() {
  NavigatorState? found;
  void visit(Element element) {
    if (found != null) {
      return;
    }
    final state = element is StatefulElement ? element.state : null;
    if (state is NavigatorState) {
      found = state;
      return;
    }
    element.visitChildren(visit);
  }

  WidgetsBinding.instance.rootElement?.visitChildren(visit);
  return found;
}

/// The offer itself. Copy per 审核 §4.5: the title asks, one sentence says
/// what happens, and each button is the answer it gives.
class _FolderOfferDialog extends StatelessWidget {
  const _FolderOfferDialog();

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text(
        '还要添加整个文件夹吗？',
        key: ValueKey('folder_offer_title'),
      ),
      content: const Text(
        '可以再选一个文件夹，里面的全部内容会和已选的文件一起发送。',
        key: ValueKey('folder_offer_body'),
      ),
      actions: [
        TextButton(
          key: const ValueKey('folder_offer_decline_button'),
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('不用了'),
        ),
        FilledButton(
          key: const ValueKey('folder_offer_add_button'),
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('添加文件夹'),
        ),
      ],
    );
  }
}

/// X4 P1-2 (review round 3): the folder-ONLY offer — the same copy
/// discipline as [_FolderOfferDialog].
class _FolderOnlyOfferDialog extends StatelessWidget {
  const _FolderOnlyOfferDialog();

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text(
        '只发送一个文件夹吗？',
        key: ValueKey('folder_only_offer_title'),
      ),
      content: const Text(
        '可以选一个文件夹，里面的全部内容会单独发送。',
        key: ValueKey('folder_only_offer_body'),
      ),
      actions: [
        TextButton(
          key: const ValueKey('folder_only_offer_cancel_button'),
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('取消'),
        ),
        FilledButton(
          key: const ValueKey('folder_only_offer_pick_button'),
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('选择文件夹'),
        ),
      ],
    );
  }
}
