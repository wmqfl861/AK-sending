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
/// * 「添加文件夹」 runs the same single `getDirectoryPath()` as
///   [PluginFilePickGateway.pickDirectory] and appends that entry;
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
library;

import 'package:file_picker/file_picker.dart' as fp;
import 'package:flutter/material.dart';

/// One user-chosen item: a file (with size when the platform reports it)
/// or a directory (folder pick). [path] is what goes into
/// `SimpleTransferService.sendFiles`.
class PickedEntry {
  const PickedEntry({
    required this.path,
    required this.name,
    required this.isDirectory,
    this.sizeBytes,
  });

  final String path;
  final String name;
  final bool isDirectory;

  /// File size in bytes when known; null for directories/unknown.
  final int? sizeBytes;
}

/// Chooser surface used by the connection-detail page and the settings
/// page.
abstract class FilePickGateway {
  /// Same-directory multi-file pick (Android SAF multi-select / Windows
  /// dialog). Returns an empty list when the user cancels.
  Future<List<PickedEntry>> pickFiles();

  /// Single-folder pick. Returns null on cancel.
  Future<PickedEntry?> pickDirectory();
}

/// Production gateway over the `file_picker` plugin.
///
/// The three function members are the test seams — the plugin's static
/// calls cannot run without a platform, the same function-seam style as
/// the other services in this repo — and each defaults to the real call.
class PluginFilePickGateway implements FilePickGateway {
  /// The seams are optional and default to the real calls: `const
  /// PluginFilePickGateway()` stays the production gateway every page
  /// already defaults to.
  const PluginFilePickGateway({
    this._filePicker,
    this._directoryPicker,
    this._folderOffer,
    this._folderOnlyOffer,
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

  @override
  Future<List<PickedEntry>> pickFiles() async {
    final files = await (_filePicker ?? fp.FilePicker.pickFiles)();
    if (files.isEmpty) {
      // X4 P1-2 (review round 3): a cancelled FILE pick is NOT yet the
      // end — the user may have come here to send ONLY a folder (§4.4
      // 「可多选，含文件夹」includes the folder-only case). One offer
      // follows; declining it (or cancelling the folder pick) is the
      // overall cancel, exactly as before.
      if (await (_folderOnlyOffer ?? _showFolderOnlyOffer)()) {
        final folder = await pickDirectory();
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
    if (await (_folderOffer ?? _showFolderOffer)()) {
      final folder = await pickDirectory();
      if (folder != null) {
        entries.add(folder); // a cancelled folder pick keeps the files
      }
    }
    return entries;
  }

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
  static bool _isFileSystemPath(String path) {
    final trimmed = path.trim();
    if (trimmed.isEmpty) {
      return false;
    }
    // Review round 5: the Android 11+ Downloads provider ALSO surfaces its
    // emulated subdirectories through the `/storage/msd/…` FUSE mount —
    // filesystem-SHAPED but not sendable by the engine. Same exclusion as
    // the bare provider URIs.
    if (trimmed.startsWith('/storage/msd/')) {
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
