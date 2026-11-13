/// Client UI U1 (2026-11-13): the user-facing label of the direct path a
/// transfer used, derived from the peer endpoint the engine reported
/// (`task_send`'s `peer`: `ip:port` or `[v6]:port`).
///
/// Lives in the services layer (it needs `dart:io`'s address parser); the
/// UI layer only ever sees the finished label inside the send summary.
library;

import 'dart:io' show InternetAddress, InternetAddressType;

/// The UX rule (docs/UX §通道显示) allows only direct labels — there is no
/// relay to name — and the label must not over-claim: an address class is
/// all the client knows here, so
///
/// * private / link-local IPv4 and link-local / unique-local IPv6 →
///   `局域网直连`;
/// * any other IPv4 (public, carrier-grade NAT space) → `互联网直连`;
/// * a global IPv6 address → `IPv6 直连` (it may be on the same LAN or
///   across the internet — the address alone cannot tell);
/// * loopback → `本机直连`;
/// * anything unparsable → null (show no label rather than a guess).
String? directPathLabel(String? endpoint) {
  final host = _hostOf(endpoint);
  if (host == null) {
    return null;
  }
  final address = InternetAddress.tryParse(host);
  if (address == null) {
    return null;
  }
  if (address.isLoopback) {
    return '本机直连';
  }
  final raw = address.rawAddress;
  if (address.type == InternetAddressType.IPv4) {
    return _isPrivateV4(raw) ? '局域网直连' : '互联网直连';
  }
  if (address.type == InternetAddressType.IPv6) {
    // An IPv4-mapped IPv6 address (::ffff:a.b.c.d) is an IPv4 path.
    if (_isV4Mapped(raw)) {
      final v4 = raw.sublist(12);
      if (v4[0] == 127) {
        return '本机直连';
      }
      return _isPrivateV4(v4) ? '局域网直连' : '互联网直连';
    }
    final linkLocal = raw[0] == 0xfe && (raw[1] & 0xc0) == 0x80; // fe80::/10
    final uniqueLocal = (raw[0] & 0xfe) == 0xfc; // fc00::/7
    return linkLocal || uniqueLocal ? '局域网直连' : 'IPv6 直连';
  }
  return null;
}

/// The host part of `ip:port` / `[v6]:port` / a bare address, without an
/// IPv6 zone suffix (`fe80::1%eth0` → `fe80::1`).
String? _hostOf(String? endpoint) {
  final text = endpoint?.trim();
  if (text == null || text.isEmpty) {
    return null;
  }
  final String host;
  if (text.startsWith('[')) {
    final close = text.indexOf(']');
    if (close <= 1) {
      return null;
    }
    host = text.substring(1, close);
  } else if (':'.allMatches(text).length == 1) {
    host = text.substring(0, text.indexOf(':')); // ipv4:port
  } else {
    host = text; // a bare IPv4 (no colon) or a bare IPv6 (several colons)
  }
  final zone = host.indexOf('%');
  return zone > 0 ? host.substring(0, zone) : host;
}

bool _isPrivateV4(List<int> raw) {
  if (raw.length != 4) {
    return false;
  }
  final a = raw[0];
  final b = raw[1];
  return a == 10 || // 10.0.0.0/8
      (a == 172 && b >= 16 && b <= 31) || // 172.16.0.0/12
      (a == 192 && b == 168) || // 192.168.0.0/16
      (a == 169 && b == 254); // 169.254.0.0/16 (link-local)
}

bool _isV4Mapped(List<int> raw) {
  if (raw.length != 16) {
    return false;
  }
  for (var i = 0; i < 10; i++) {
    if (raw[i] != 0) {
      return false;
    }
  }
  return raw[10] == 0xff && raw[11] == 0xff;
}
