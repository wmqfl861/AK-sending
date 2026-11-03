import 'dart:ffi';
import 'dart:typed_data';

import 'native_api.dart';

const lanCertificateLimit = 4096;
const lanOfferLimit = lanCertificateLimit * 2 + 100;

int lanIpv4(String text) {
  final parts = text.split('.');
  if (parts.length != 4) throw const FormatException('ipv4-required');
  final octets = <int>[];
  for (final part in parts) {
    if (!RegExp(r'^(0|[1-9][0-9]{0,2})$').hasMatch(part)) {
      throw const FormatException('invalid-ipv4');
    }
    final value = int.parse(part);
    if (value > 255) throw const FormatException('invalid-ipv4');
    octets.add(value);
  }
  final allowed =
      octets[0] == 10 ||
      (octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31) ||
      (octets[0] == 192 && octets[1] == 168) ||
      text == '127.0.0.1';
  if (!allowed) throw const FormatException('outside-lan-experiment');
  return octets.fold(0, (value, byte) => (value << 8) | byte);
}

int lanPort(String text) {
  if (!RegExp(r'^[0-9]{1,5}$').hasMatch(text)) {
    throw const FormatException('invalid-port');
  }
  final port = int.parse(text);
  if (port < 1024 || port > 65535) throw const FormatException('invalid-port');
  return port;
}

String hexBytes(List<int> bytes) =>
    bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

final class LanOffer {
  final String ip;
  final int port;
  final Uint8List certificate;
  LanOffer(this.ip, this.port, List<int> certificate)
    : certificate = Uint8List.fromList(certificate).asUnmodifiableView();
  static LanOffer parse(String input) {
    if (input.length > lanOfferLimit)
      throw const FormatException('offer-too-large');
    final fields = input.trim().split('|');
    if (fields.length != 4 || fields[0] != 'AKL1') {
      throw const FormatException('offer-version');
    }
    lanIpv4(fields[1]);
    final port = lanPort(fields[2]);
    final der = fields[3];
    if (der.isEmpty ||
        der.length > lanCertificateLimit * 2 ||
        der.length.isOdd ||
        !RegExp(r'^[a-fA-F0-9]+$').hasMatch(der)) {
      throw const FormatException('invalid-certificate-encoding');
    }
    return LanOffer(fields[1], port, [
      for (var i = 0; i < der.length; i += 2)
        int.parse(der.substring(i, i + 2), radix: 16),
    ]);
  }

  String encode() => 'AKL1|$ip|$port|${hexBytes(certificate)}';
}

abstract interface class LanApi {
  int version();
  int prepare(int ip, int port);
  int poll(int handle);
  int read(int handle, int kind, int index);
  int push(int handle, int index, int byte);
  int configure(int handle, int ip, int port, int role);
  int start(int handle);
  int cancel(int handle);
  int release(int handle);
}

typedef _HandleC = Int32 Function(Uint64);
typedef _HandleD = int Function(int);
typedef _ReadC = Int32 Function(Uint64, Uint32, Uint32);
typedef _ReadD = int Function(int, int, int);

final class NativeLan implements LanApi {
  // No unload API; keep native code resident while either task slot may be alive.
  final DynamicLibrary _library;
  NativeLan({String? libraryPath}) : _library = openProbeLibrary(libraryPath);
  late final _version = _library
      .lookupFunction<Uint32 Function(), int Function()>('ak_m0_lan_version');
  late final _prepare = _library
      .lookupFunction<Int64 Function(Uint32, Uint32), int Function(int, int)>(
        'ak_m0_lan_prepare',
      );
  late final _poll = _library.lookupFunction<_HandleC, _HandleD>(
    'ak_m0_lan_poll',
  );
  late final _read = _library.lookupFunction<_ReadC, _ReadD>('ak_m0_lan_read');
  late final _push = _library.lookupFunction<_ReadC, _ReadD>('ak_m0_lan_push');
  late final _configure = _library
      .lookupFunction<
        Int32 Function(Uint64, Uint32, Uint32, Uint32),
        int Function(int, int, int, int)
      >('ak_m0_lan_configure');
  late final _start = _library.lookupFunction<_HandleC, _HandleD>(
    'ak_m0_lan_start',
  );
  late final _cancel = _library.lookupFunction<_HandleC, _HandleD>(
    'ak_m0_lan_cancel',
  );
  late final _release = _library.lookupFunction<_HandleC, _HandleD>(
    'ak_m0_lan_release',
  );
  @override
  int version() => _version();
  @override
  int prepare(int ip, int port) => _prepare(ip, port);
  @override
  int poll(int handle) => _poll(handle);
  @override
  int read(int handle, int kind, int index) => _read(handle, kind, index);
  @override
  int push(int handle, int index, int byte) => _push(handle, index, byte);
  @override
  int configure(int handle, int ip, int port, int role) =>
      _configure(handle, ip, port, role);
  @override
  int start(int handle) => _start(handle);
  @override
  int cancel(int handle) => _cancel(handle);
  @override
  int release(int handle) => _release(handle);
}
