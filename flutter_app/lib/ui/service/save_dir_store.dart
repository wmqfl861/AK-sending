/// Persistence of the wave-28 default save folder (settings page's single
/// setting): the value survives app restarts and is re-applied to the
/// transfer service at startup.
library;

import 'package:shared_preferences/shared_preferences.dart';

/// Load/save seam for the default save folder. Widget tests inject
/// [InMemorySaveDirStore]; production uses [SharedPrefsSaveDirStore].
abstract class SaveDirStore {
  Future<String?> load();
  Future<void> save(String dir);
}

/// Production store: one `shared_preferences` string.
class SharedPrefsSaveDirStore implements SaveDirStore {
  const SharedPrefsSaveDirStore();

  static const String _key = 'ak.w28.saveDir';

  @override
  Future<String?> load() async {
    final prefs = await SharedPreferences.getInstance();
    return prefs.getString(_key);
  }

  @override
  Future<void> save(String dir) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_key, dir);
  }
}

/// Plain in-memory double for tests.
class InMemorySaveDirStore implements SaveDirStore {
  String? value;

  @override
  Future<String?> load() async => value;

  @override
  Future<void> save(String dir) async {
    value = dir;
  }
}
