import 'dart:ffi';
import 'dart:io';

abstract interface class ProbeApi {
  int version();
  int start();
  int poll(int handle);
  int cancel(int handle);
  int release(int handle);
}

typedef _HandleC = Int32 Function(Uint64);
typedef _HandleDart = int Function(int);

final class NativeProbe implements ProbeApi {
  // The library is retained for the process lifetime. Never unload a live worker.
  final DynamicLibrary _library;
  late final int Function() _version = _library
      .lookupFunction<Uint32 Function(), int Function()>('ak_m0_probe_version');
  late final int Function() _start = _library
      .lookupFunction<Int64 Function(), int Function()>('ak_m0_probe_start');
  late final _HandleDart _poll = _library.lookupFunction<_HandleC, _HandleDart>(
    'ak_m0_probe_poll',
  );
  late final _HandleDart _cancel = _library
      .lookupFunction<_HandleC, _HandleDart>('ak_m0_probe_cancel');
  late final _HandleDart _release = _library
      .lookupFunction<_HandleC, _HandleDart>('ak_m0_probe_release');

  NativeProbe({String? libraryPath}) : _library = _open(libraryPath);
  static DynamicLibrary _open(String? path) {
    if (path != null) return DynamicLibrary.open(path);
    final directory = File(Platform.resolvedExecutable).parent.path;
    if (Platform.isLinux)
      return DynamicLibrary.open('$directory/lib/libak_m0_probe.so');
    if (Platform.isWindows)
      return DynamicLibrary.open('$directory/ak_m0_probe.dll');
    if (Platform.isAndroid) return DynamicLibrary.open('libak_m0_probe.so');
    if (Platform.isIOS || Platform.isMacOS) return DynamicLibrary.process();
    throw UnsupportedError('unsupported-platform');
  }

  @override
  int version() => _version();
  @override
  int start() => _start();
  @override
  int poll(int handle) => _poll(handle);
  @override
  int cancel(int handle) => _cancel(handle);
  @override
  int release(int handle) => _release(handle);
}
