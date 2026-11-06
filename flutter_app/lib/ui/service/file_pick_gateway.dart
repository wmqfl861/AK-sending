/// Seam over the system file/folder pickers (`file_picker` plugin: Android
/// SAF, Windows common dialogs) so wave-28 pages stay testable — widget
/// tests inject a fake and never touch the platform channel.
///
/// file_picker 13.x notes: `FilePicker.pickFiles()` is a static
/// multi-select API (empty list = user cancelled) and
/// `FilePicker.getDirectoryPath()` returns null on cancel.
library;

import 'package:file_picker/file_picker.dart' as fp;

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
class PluginFilePickGateway implements FilePickGateway {
  const PluginFilePickGateway();

  @override
  Future<List<PickedEntry>> pickFiles() async {
    final files = await fp.FilePicker.pickFiles();
    if (files.isEmpty) {
      return const <PickedEntry>[];
    }
    return files
        .map((file) => PickedEntry(
              path: file.path ?? '',
              name: file.name,
              isDirectory: false,
              sizeBytes: file.lengthSync(),
            ))
        .toList();
  }

  @override
  Future<PickedEntry?> pickDirectory() async {
    final path = await fp.FilePicker.getDirectoryPath();
    if (path == null) {
      return null;
    }
    final name = path.split(RegExp(r'[\\/]')).last;
    return PickedEntry(
      path: path,
      name: name.isEmpty ? path : name,
      isDirectory: true,
    );
  }
}
