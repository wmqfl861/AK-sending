/// Three-tier probe for the `ak_ffi` native library (see flutter_app/README.md
/// "DLL 三级探测" for the operator-facing contract), plus an Android
/// short-circuit (see [DllSource.bundled]).
///
/// **Android（tier 0）**: the cdylib ships inside the apk as
/// `jniLibs/<abi>/libak_ffi.so`; `DynamicLibrary.open('libak_ffi.so')` is
/// resolved by the OS linker against the app's native library directory
/// (System.loadLibrary semantics). No env/exe/CWD probing applies — not even
/// `$AK_FFI_DLL`, which cannot be set on Android anyway.
///
/// **桌面（tiers 1–3）**:
/// 1. **环境变量** `$AK_FFI_DLL` — explicit operator intent, returned verbatim
///    (existence is NOT checked; a typo surfaces as the OS loader's own error
///    naming that exact path).
/// 2. **可执行文件同目录** `<dir of Platform.resolvedExecutable>/ak_ffi.dll` —
///    the production layout: the Windows runner CMake step copies the DLL
///    next to the exe (see windows/runner/CMakeLists.txt).
/// 3. **开发相对路径** — CWD-relative `target/release` candidates covering
///    both dev entry points of this repository:
///    `../target/release/ak_ffi.dll` (CWD = flutter_app, e.g.
///    `flutter run` / `flutter test` / `flutter build`) and
///    `../../../target/release/ak_ffi.dll` (CWD =
///    flutter_app/packages/ak_ffi_dart, the Dart package's own default).
///
/// Everything injectable, no hidden I/O defaults beyond [Platform]: the unit
/// tests drive pure in-memory inputs.
library;

import 'dart:io' show Directory, File, Platform;

/// The SONAME the Android build packages into the apk (`jniLibs/<abi>/`).
const String androidAkLibName = 'libak_ffi.so';

/// Which tier produced the resolution.
enum DllSource {
  /// Android: the library is bundled inside the apk; the bare SONAME is
  /// handed to `DynamicLibrary.open` (System.loadLibrary semantics).
  bundled,

  /// `$AK_FFI_DLL` environment variable.
  environment,

  /// A copy bundled next to the running executable.
  executableDirectory,

  /// A CWD-relative `target/release` build (development only).
  workingDirectory,
}

/// The outcome of a successful probe: the library [path] plus the [source]
/// tier that produced it (diagnostics card / error messages).
final class DllResolution {
  const DllResolution({required this.path, required this.source});

  final String path;
  final DllSource source;

  @override
  String toString() => '$path (${source.name})';
}

/// Native file name of the ak-ffi cdylib on the current OS.
///
/// [isAndroid] is injectable for tests (defaults to `Platform.isAndroid`);
/// Android names the packaged cdylib [androidAkLibName].
String defaultAkDllFileName({bool? isAndroid}) {
  if (isAndroid ?? Platform.isAndroid) return androidAkLibName;
  if (Platform.isWindows) return 'ak_ffi.dll';
  if (Platform.isMacOS) return 'libak_ffi.dylib';
  return 'libak_ffi.so';
}

/// Resolves the ak-ffi native library, first tier wins.
///
/// * [isAndroid] — defaults to `Platform.isAndroid`; when true the Android
///   short-circuit returns the bundled [androidAkLibName] immediately
///   (System.loadLibrary semantics) and no other tier runs;
/// * [environment] — defaults to `Platform.environment`;
/// * [resolvedExecutable] — defaults to `Platform.resolvedExecutable`
///   (tier 2 looks in its directory);
/// * [workingDirectory] — defaults to `Directory.current.path` (tier 3);
/// * [exists] — defaults to real `File.existsSync`, injectable for tests;
/// * [dllFileName] — defaults to [defaultAkDllFileName].
///
/// Returns null when no tier resolves — callers should surface the tier list
/// instead of silently continuing.
DllResolution? resolveAkDllPath({
  bool? isAndroid,
  Map<String, String>? environment,
  String? resolvedExecutable,
  String? workingDirectory,
  bool Function(String path)? exists,
  String? dllFileName,
}) {
  // Tier 0 (Android): bundled in the apk, bare SONAME for the OS linker.
  if (isAndroid ?? Platform.isAndroid) {
    return const DllResolution(path: androidAkLibName, source: DllSource.bundled);
  }

  final env = environment ?? Platform.environment;
  final check = exists ?? (String path) => File(path).existsSync();
  final fileName = dllFileName ?? defaultAkDllFileName();

  // Tier 1: explicit override, verbatim.
  final fromEnv = env['AK_FFI_DLL'];
  if (fromEnv != null && fromEnv.isNotEmpty) {
    return DllResolution(path: fromEnv, source: DllSource.environment);
  }

  // Tier 2: bundled next to the executable (CMake copies it there).
  final exe = resolvedExecutable ?? Platform.resolvedExecutable;
  if (exe.isNotEmpty) {
    final beside = _join(_dirname(exe), fileName);
    if (check(beside)) {
      return DllResolution(path: beside, source: DllSource.executableDirectory);
    }
  }

  // Tier 3: CWD-relative dev builds. Both candidates are listed so error
  // messages can name them; the first existing one wins.
  final cwd = workingDirectory ?? Directory.current.path;
  for (final relative in const [
    '../target/release',
    '../../../target/release',
  ]) {
    final candidate = _join(cwd, '$relative/$fileName');
    if (check(candidate)) {
      return DllResolution(path: candidate, source: DllSource.workingDirectory);
    }
  }

  return null;
}

/// Every tier-2/3 candidate (for error/diagnostic messages). Tier 1 is
/// excluded because it is only "a candidate" when the variable is set.
/// On Android ([isAndroid], defaults to `Platform.isAndroid`) the single
/// candidate is the bundled [androidAkLibName].
List<String> akDllProbeCandidates({
  bool? isAndroid,
  String? resolvedExecutable,
  String? workingDirectory,
  String? dllFileName,
}) {
  if (isAndroid ?? Platform.isAndroid) {
    return const <String>[androidAkLibName];
  }
  final fileName = dllFileName ?? defaultAkDllFileName();
  final exe = resolvedExecutable ?? Platform.resolvedExecutable;
  final cwd = workingDirectory ?? Directory.current.path;
  return [
    if (exe.isNotEmpty) _join(_dirname(exe), fileName),
    _join(cwd, '../target/release/$fileName'),
    _join(cwd, '../../../target/release/$fileName'),
  ];
}

/// Directory part of [path] ('' when no separator). Both separators count so
/// Windows exe paths work whichever flavor the platform reports.
String _dirname(String path) {
  final slash = path.lastIndexOf('/');
  final backslash = path.lastIndexOf('\\');
  final cut = slash > backslash ? slash : backslash;
  return cut <= 0 ? '' : path.substring(0, cut);
}

/// Forward-slash join; Windows loaders accept them and tests compare simply.
String _join(String dir, String name) =>
    dir.isEmpty ? name : '$dir/$name';
