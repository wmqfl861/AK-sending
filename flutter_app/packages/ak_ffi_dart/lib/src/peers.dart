/// Decoded payload of the `ak_peer_list` export: the paired-peer book at
/// `<id_dir>/peers.book` (schema frozen in `crates/ak-ffi/src/lib.rs`).
///
/// The book is the same file the CLI's `pair` subcommands and the
/// `expect: "book"` admission policy use, managed through
/// `AkEngineDart.peerAdd` / `AkEngineDart.peerRemove`. Like the identity and
/// jobs inspection calls, the answer comes from disk, so it works for any
/// directory, before or between transfers. FFI-free plain data:
/// unit-testable without the DLL.
library;

/// One paired peer returned by `AkEngineDart.peerList`.
///
/// JSON shape: `{"fingerprint":"<64 hex>","name":"laptop",
/// "paired_unix_ms":1730000000000}`.
final class PeerEntry {
  const PeerEntry({
    required this.fingerprint,
    required this.name,
    required this.pairedUnixMs,
  });

  /// The peer's BLAKE3-based device fingerprint, 64 lowercase hex
  /// characters — the value senders pin in `fp_hex` and receivers admit
  /// through `expect: "book"`.
  final String fingerprint;

  /// Display name exactly as stored: a peer paired through
  /// `AkEngineDart.peerAdd` without a name carries `"(unnamed)"`; one the
  /// CLI paired namelessly carries the empty string.
  final String name;

  /// When the peer was paired, Unix epoch milliseconds. Survives re-adds
  /// (CLI TOFU semantics: renaming an entry keeps its original time).
  final int pairedUnixMs;

  /// Decode one element of the `ak_peer_list` array.
  factory PeerEntry.fromJson(Map<String, dynamic> json) => PeerEntry(
        fingerprint: json['fingerprint'] as String,
        name: json['name'] as String,
        pairedUnixMs: json['paired_unix_ms'] as int,
      );

  @override
  bool operator ==(Object other) =>
      other is PeerEntry &&
      other.fingerprint == fingerprint &&
      other.name == name &&
      other.pairedUnixMs == pairedUnixMs;

  @override
  int get hashCode => Object.hash(fingerprint, name, pairedUnixMs);

  @override
  String toString() => 'PeerEntry($fingerprint ${name.isEmpty ? '<unnamed>' : name} '
      'paired=$pairedUnixMs)';
}
