/// End-to-end tests for the peer-book management API over the real
/// `ak_ffi.dll` (workspace `target/release`), mirroring the Rust-side
/// `crates/ak-ffi/tests/ffi.rs` peer claims: `ak_peer_list` on missing and
/// corrupt books, `ak_peer_add` pairing/TOFU upsert semantics with the
/// "(unnamed)" default, `ak_peer_remove` count accounting and -4 hints,
/// the -1/-2/-3/-4 error surfaces, and the `ak_identity_info` `peers`
/// count tracking the same `<id_dir>/peers.book` file.
library;

import 'dart:ffi';

import 'dart:io' show Directory, File;

import 'package:test/test.dart';

import 'package:ak_ffi_dart/src/bindings.dart';
import 'package:ak_ffi_dart/src/engine.dart';
import 'package:ak_ffi_dart/src/ffi_json.dart' show toNativeUtf8;
import 'package:ak_ffi_dart/src/peers.dart';

void main() {
  tearDown(() {
    for (final dir in _tempDirs) {
      try {
        dir.deleteSync(recursive: true);
      } on Exception {
        // best effort; temp dirs live under the system temp root
      }
    }
    _tempDirs.clear();
  });

  _declareTests();
}

// ---------------------------------------------------------------------------
// Fixtures and helpers.
// ---------------------------------------------------------------------------

/// Deterministic 64-hex fingerprints, byte-sorted in fixture order
/// (`_fpA` < `_fpB` < `_fpC` < `_fpX`).
final String _fpA = '11' * 32;
final String _fpB = '22' * 32;
final String _fpC = '33' * 32;
final String _fpX = 'ab' * 32;

final _tempDirs = <Directory>[];

Directory _newTempDir(String tag) {
  final dir = Directory.systemTemp.createTempSync('ak_ffi_dart_peers_${tag}_');
  _tempDirs.add(dir);
  return dir;
}

void _expectFfiError(void Function() call, int code, String needle) {
  try {
    call();
    fail('expected an AkFfiException with code $code');
  } on AkFfiException catch (e) {
    expect(e.code, code, reason: e.message);
    expect(e.message, contains(needle), reason: 'message was: ${e.message}');
  }
}

// ---------------------------------------------------------------------------
// Test declarations.
// ---------------------------------------------------------------------------

void _declareTests() {
// ---------------------------------------------------------------------------
// ak_peer_list: reads.
// ---------------------------------------------------------------------------

test('peerList: missing books are [] and reads create nothing on disk', () {
  final engine = AkEngineDart();
  try {
    final base = _newTempDir('pl');
    final fresh = Directory('${base.path}/fresh'); // exists, no book
    fresh.createSync(recursive: true);
    expect(engine.peerList(fresh.path), isEmpty,
        reason: 'an existing directory without peers.book');
    expect(File('${fresh.path}/peers.book').existsSync(), isFalse,
        reason: 'a read never creates the book');

    expect(engine.peerList('${base.path}/never-created'), isEmpty,
        reason: 'a missing id_dir is also the empty list');
    expect(Directory('${base.path}/never-created').existsSync(), isFalse,
        reason: 'a read never creates the id_dir');

    final withKey = Directory('${base.path}/has-key');
    engine.identityInfo(withKey.path); // creates dir + device.key only
    expect(engine.peerList(withKey.path), isEmpty,
        reason: 'an identity without pairings lists nothing');
    expect(File('${withKey.path}/peers.book').existsSync(), isFalse);

    // Empty id_dir: -2 with the engine's message (all three calls).
    _expectFfiError(() => engine.peerList(''), AkStatus.errInvalid, 'id_dir');
    _expectFfiError(
        () => engine.peerAdd('', _fpA), AkStatus.errInvalid, 'id_dir');
    _expectFfiError(
        () => engine.peerRemove('', _fpA), AkStatus.errInvalid, 'id_dir');

    // Raw layer: a null buffer is -1 (checked before any disk access).
    final idPtr = toNativeUtf8(fresh.path);
    try {
      expect(engine.bindings.peerList(engine.nativeHandle, idPtr, nullptr, 64),
          AkStatus.errNull);
    } finally {
      processAllocator.free(idPtr);
    }
  } finally {
    engine.dispose();
  }
});

test('peerList: corrupt books are -2; the writers refuse and never clobber',
    () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('plc');
    final book = File('${idDir.path}/peers.book');
    // The same corrupt fixtures the Rust side proves (bad fingerprint
    // line, bad timestamp, missing fields).
    final cases = <(String, String)>[
      ('bad fingerprint line', 'AKPEERS1\nzz\tname\t1\n'),
      ('bad timestamp', 'AKPEERS1\n$_fpX\tname\tnope\n'),
      ('missing fields', 'AKPEERS1\n$_fpX\tname\n'),
    ];
    for (final (label, content) in cases) {
      book.writeAsStringSync(content);
      _expectFfiError(() => engine.peerList(idDir.path),
          AkStatus.errInvalid, 'cannot load peer book');
      _expectFfiError(() => engine.peerAdd(idDir.path, _fpX, name: 'x'),
          AkStatus.errInvalid, 'cannot load peer book');
      _expectFfiError(() => engine.peerRemove(idDir.path, _fpX),
          AkStatus.errInvalid, 'cannot load peer book');
      expect(book.readAsStringSync(), content,
          reason: 'case $label: the corrupt book is left untouched');
    }
  } finally {
    engine.dispose();
  }
});

test('peerList: a CLI-written book round-trips; a -3 retry is lossless', () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('plrt');
    // Seed the book exactly the way the CLI (ak-identity PeerBook) writes
    // it: header + `fp\tname\tpaired_unix_ms` lines.
    File('${idDir.path}/peers.book').writeAsStringSync(
        'AKPEERS1\n'
        '$_fpA\tlaptop\t1700000000000\n'
        '$_fpX\t\t42\n');

    final peers = engine.peerList(idDir.path);
    expect(peers, hasLength(2));
    // Sorted by fingerprint (_fpA < _fpX), names as stored (a CLI-paired
    // peer without a name is the empty string, not "(unnamed)").
    expect(peers[0],
        PeerEntry(
            fingerprint: _fpA, name: 'laptop', pairedUnixMs: 1700000000000),
        reason: 'first entry decodes field-for-field (== on PeerEntry)');
    expect(peers[1],
        PeerEntry(fingerprint: _fpX, name: '', pairedUnixMs: 42),
        reason: 'a nameless CLI entry keeps its empty name');
    expect(peers[0].toString(), contains('1700000000000'),
        reason: 'large u64 timestamps survive the JSON decode');

    // Raw layer: a tiny buffer is -3 (nothing consumed, capacity hint in
    // the last error); the friendly call then returns the same entries.
    final idPtr = toNativeUtf8(idDir.path);
    final tiny = processAllocator.allocate<Uint8>(4);
    try {
      expect(engine.bindings.peerList(engine.nativeHandle, idPtr, tiny, 4),
          AkStatus.errBuffer);
      expect(engine.lastError(), contains('buffer too small'));
    } finally {
      processAllocator.free(tiny);
      processAllocator.free(idPtr);
    }
    expect(engine.peerList(idDir.path), peers,
        reason: 'lossless retry after the -3 probe');
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// ak_peer_add: pairing + TOFU upsert.
// ---------------------------------------------------------------------------

test('peerAdd: pairs new peers with a name or the "(unnamed)" default', () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('pa');
    final book = File('${idDir.path}/peers.book');

    // Named first peer; the count JSON's N is the return value.
    expect(engine.peerAdd(idDir.path, _fpA, name: 'laptop'), 1);
    // A Dart-default (empty) name and an explicit empty string both mean
    // "no name given" and store "(unnamed)".
    expect(engine.peerAdd(idDir.path, _fpB), 2);
    expect(engine.peerAdd(idDir.path, _fpC, name: ''), 3);

    // The book is the CLI's AKPEERS1 format: header + three entries.
    final saved = book.readAsStringSync();
    expect(saved.startsWith('AKPEERS1\n'), isTrue, reason: 'CLI header');
    expect(saved.split('\n').where((line) => line.isNotEmpty), hasLength(4),
        reason: 'header + three entries');

    // The list reflects all three, sorted by fingerprint, fields complete.
    final peers = engine.peerList(idDir.path);
    expect(peers, hasLength(3));
    expect(peers.map((peer) => peer.fingerprint), [_fpA, _fpB, _fpC],
        reason: 'sorted by fingerprint');
    expect(peers[0].name, 'laptop');
    expect(peers[1].name, '(unnamed)',
        reason: 'the Dart-default empty name stored the default');
    expect(peers[2].name, '(unnamed)',
        reason: 'an explicit empty name is the same "no name given"');
    for (final peer in peers) {
      expect(peer.pairedUnixMs, greaterThan(0),
          reason: 'a fresh pairing is time-stamped');
    }
  } finally {
    engine.dispose();
  }
});

test('peerAdd: upsert renames keeping the pairing time; no-name is a no-op',
    () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('up');
    // Seed the book the way the CLI paired this peer long ago: a fixed
    // ancient timestamp that must survive every FFI re-add below.
    File('${idDir.path}/peers.book')
        .writeAsStringSync('AKPEERS1\n$_fpX\told name\t1000\n');

    // Re-add with a new name: renamed, pairing time preserved, count stays.
    expect(engine.peerAdd(idDir.path, _fpX, name: 'renamed'), 1);
    final renamed = engine.peerList(idDir.path).single;
    expect(renamed.name, 'renamed');
    expect(renamed.pairedUnixMs, 1000,
        reason: 'CLI TOFU: the original pairing time is kept on rename');

    // Re-add without a name (default and explicit empty): the record is
    // left untouched — re-running an add is always idempotent.
    expect(engine.peerAdd(idDir.path, _fpX), 1);
    expect(engine.peerAdd(idDir.path, _fpX, name: ''), 1);
    final untouched = engine.peerList(idDir.path).single;
    expect(untouched.name, 'renamed', reason: 'a no-name re-add never renames');
    expect(untouched.pairedUnixMs, 1000);
  } finally {
    engine.dispose();
  }
});

test('peerAdd: bad fingerprints and control names are -2, nothing persisted',
    () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('pa2');
    final book = File('${idDir.path}/peers.book');

    // Wrong length, non-hex, empty: all -2 with the "64 hex" message.
    for (final bad in ['', 'ab', _fpA.substring(0, 63), '${_fpA}0', 'z' * 64]) {
      _expectFfiError(
          () => engine.peerAdd(idDir.path, bad, name: 'x'),
          AkStatus.errInvalid,
          '64 hex');
    }

    // Control characters in names are refused outright (raw bytes under
    // 0x20 — never silently sanitized).
    for (final badName in [
      'two\nlines',
      'tab\there',
      'carriage\rreturn',
      '\u0001beep',
    ]) {
      _expectFfiError(() => engine.peerAdd(idDir.path, _fpA, name: badName),
          AkStatus.errInvalid, 'control');
    }

    expect(book.existsSync(), isFalse,
        reason: 'rejected adds persist nothing');

    // Raw layer: null arguments are -1 BEFORE any disk access — id_dir
    // and fingerprint are required; `name` is the documented optional one;
    // a null buf is checked first of all.
    final idPtr = toNativeUtf8(idDir.path);
    final fpPtr = toNativeUtf8(_fpA);
    final buf = processAllocator.allocate<Uint8>(16);
    try {
      expect(
          engine.bindings
              .peerAdd(engine.nativeHandle, idPtr, nullptr, nullptr, buf, 16),
          AkStatus.errNull);
      expect(
          engine.bindings
              .peerAdd(engine.nativeHandle, nullptr, fpPtr, nullptr, buf, 16),
          AkStatus.errNull);
      expect(
          engine.bindings.peerAdd(
              engine.nativeHandle, idPtr, fpPtr, nullptr, nullptr, 64),
          AkStatus.errNull);
    } finally {
      processAllocator.free(buf);
      processAllocator.free(fpPtr);
      processAllocator.free(idPtr);
    }
    expect(book.existsSync(), isFalse,
        reason: 'a null buf persists nothing');

    // After all that, one valid add still works.
    expect(engine.peerAdd(idDir.path, _fpA, name: 'finally'), 1);
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// ak_peer_remove.
// ---------------------------------------------------------------------------

test('peerRemove: deletes an existing peer and decrements the count', () {
  final engine = AkEngineDart();
  try {
    final idDir = _newTempDir('pr');
    expect(engine.peerAdd(idDir.path, _fpA, name: 'one'), 1);
    expect(engine.peerAdd(idDir.path, _fpB, name: 'two'), 2);

    expect(engine.peerRemove(idDir.path, _fpA), 1,
        reason: 'the remaining count comes back');
    final peers = engine.peerList(idDir.path);
    expect(peers, hasLength(1));
    expect(peers.single.fingerprint, _fpB);
    expect(peers.single.name, 'two');

    // Removing the last peer empties the book for every reader.
    expect(engine.peerRemove(idDir.path, _fpB), 0);
    expect(engine.peerList(idDir.path), isEmpty);
    expect(engine.identityInfo(idDir.path).peers, 0);
  } finally {
    engine.dispose();
  }
});

test('peerRemove: unknown fingerprints and missing books are -4; bad fp -2',
    () {
  final engine = AkEngineDart();
  try {
    // A missing book has no peers: the same -4.
    final missing = _newTempDir('pr4');
    _expectFfiError(() => engine.peerRemove(missing.path, _fpX),
        AkStatus.errNotFound, 'not in the peer book');

    // An existing book without the fingerprint: -4 carrying the hint.
    final idDir = _newTempDir('pr4b');
    engine.peerAdd(idDir.path, _fpA, name: 'one');
    _expectFfiError(() => engine.peerRemove(idDir.path, _fpB),
        AkStatus.errNotFound, 'not in the peer book');
    expect(engine.peerList(idDir.path), hasLength(1),
        reason: 'a refused remove deletes nothing');

    // Bad fingerprints are -2 like on add.
    for (final bad in ['', 'ab', _fpA.substring(0, 63), '${_fpA}0', 'z' * 64]) {
      _expectFfiError(() => engine.peerRemove(idDir.path, bad),
          AkStatus.errInvalid, '64 hex');
    }
  } finally {
    engine.dispose();
  }
});

// ---------------------------------------------------------------------------
// identityInfo linkage.
// ---------------------------------------------------------------------------

test('identityInfo peers count tracks the peer book across add and remove',
    () {
  final engine = AkEngineDart();
  try {
    final idDir = Directory('${_newTempDir('link').path}/id');
    final first = engine.identityInfo(idDir.path);
    expect(first.created, isTrue);
    expect(first.peers, 0, reason: 'no peers.book yet');

    expect(engine.peerAdd(idDir.path, _fpA, name: 'laptop'), 1);
    expect(engine.peerAdd(idDir.path, _fpB), 2);

    // Same file, same count; the identity itself is untouched by pairing.
    final paired = engine.identityInfo(idDir.path);
    expect(paired.created, isFalse);
    expect(paired.fingerprint, first.fingerprint);
    expect(paired.peers, 2, reason: 'identityInfo counts peers.book entries');

    expect(engine.peerRemove(idDir.path, _fpA), 1);
    expect(engine.identityInfo(idDir.path).peers, 1);
    expect(engine.peerRemove(idDir.path, _fpB), 0);
    expect(engine.identityInfo(idDir.path).peers, 0);
    expect(engine.peerList(idDir.path), isEmpty);
  } finally {
    engine.dispose();
  }
});
}
