/// Decoded payloads of the `ak_identity_info` / `ak_jobs_list` exports:
/// the on-disk identity summary under an `id_dir` and the persisted
/// checkpoint records under `<id_dir>/checkpoints` (both schemas frozen in
/// `crates/ak-ffi/src/lib.rs`).
///
/// Neither call needs engine state — both read the ak-cli on-disk
/// conventions — so they work for any directory, before or between
/// transfers. FFI-free plain data: unit-testable without the DLL.
library;

/// Identity summary returned by `AkEngineDart.identityInfo`.
///
/// JSON shape: `{"device_id":"<64 hex>","fingerprint":"<64 hex>",
/// "pairing_code":"123-456","peers":2,"created":false}`.
final class IdentityInfo {
  const IdentityInfo({
    required this.deviceId,
    required this.fingerprint,
    required this.pairingCode,
    required this.peers,
    required this.created,
  });

  /// The Ed25519 public key of `<id_dir>/device.key`, 64 hex characters.
  final String deviceId;

  /// BLAKE3-based device fingerprint, 64 hex characters; the value senders
  /// pin in `fp_hex` and the peer book lists.
  final String fingerprint;

  /// Human-friendly 6-digit pairing code (`ddd-ddd`) derived from
  /// [fingerprint]; typing it as a receive `expect` admits exactly this
  /// device.
  final String pairingCode;

  /// Number of entries in `<id_dir>/peers.book` (0 when missing or, with a
  /// note left in `AkEngineDart.lastError`, when corrupt).
  final int peers;

  /// Whether THIS call created `id_dir` and its `device.key` (true exactly
  /// once per identity directory; every later call reports false).
  final bool created;

  /// Decode the JSON of a successful `ak_identity_info` call.
  factory IdentityInfo.fromJson(Map<String, dynamic> json) => IdentityInfo(
        deviceId: json['device_id'] as String,
        fingerprint: json['fingerprint'] as String,
        pairingCode: json['pairing_code'] as String,
        peers: json['peers'] as int,
        created: json['created'] as bool,
      );

  @override
  bool operator ==(Object other) =>
      other is IdentityInfo &&
      other.deviceId == deviceId &&
      other.fingerprint == fingerprint &&
      other.pairingCode == pairingCode &&
      other.peers == peers &&
      other.created == created;

  @override
  int get hashCode =>
      Object.hash(deviceId, fingerprint, pairingCode, peers, created);

  @override
  String toString() =>
      'IdentityInfo(fingerprint: $fingerprint, pairingCode: $pairingCode, '
      'peers: $peers, created: $created)';
}

/// One persisted job record returned by `AkEngineDart.jobsList`.
///
/// JSON shape: `{"id":"<32 hex>","direction":"send"|"receive",
/// "state":"completed","files":3,"bytes":123456,
/// "updated_unix_ms":1730000000000,"peer":"<64 hex or empty>","root":"C:/in"}`.
final class JobRecordJson {
  const JobRecordJson({
    required this.id,
    required this.direction,
    required this.state,
    required this.files,
    required this.bytes,
    required this.updatedUnixMs,
    required this.peer,
    required this.root,
  });

  /// The wire job id (the `wire_job` field of result JSONs), 32 hex
  /// characters — not the opaque 16-byte FFI job id.
  final String id;

  /// `"send"` or `"receive"`.
  final String direction;

  /// Checkpoint state at the moment of the last update, e.g.
  /// `"connecting"`, `"transferring"`, or `"completed"`.
  final String state;

  /// Files + directories in the transfer manifest.
  final int files;

  /// Total payload bytes.
  final int bytes;

  /// When the record was last written, Unix epoch milliseconds.
  final int updatedUnixMs;

  /// The other side's fingerprint, 64 hex characters, or the empty string
  /// when the record has no peer yet.
  final String peer;

  /// Source root (send) or destination directory (receive).
  final String root;

  /// Decode one element of the `ak_jobs_list` array.
  factory JobRecordJson.fromJson(Map<String, dynamic> json) => JobRecordJson(
        id: json['id'] as String,
        direction: json['direction'] as String,
        state: json['state'] as String,
        files: json['files'] as int,
        bytes: json['bytes'] as int,
        updatedUnixMs: json['updated_unix_ms'] as int,
        peer: json['peer'] as String,
        root: json['root'] as String,
      );

  @override
  bool operator ==(Object other) =>
      other is JobRecordJson &&
      other.id == id &&
      other.direction == direction &&
      other.state == state &&
      other.files == files &&
      other.bytes == bytes &&
      other.updatedUnixMs == updatedUnixMs &&
      other.peer == peer &&
      other.root == root;

  @override
  int get hashCode =>
      Object.hash(id, direction, state, files, bytes, updatedUnixMs, peer,
          root);

  @override
  String toString() =>
      'JobRecordJson($direction $id $state files=$files bytes=$bytes '
      'peer=${peer.isEmpty ? "<none>" : peer})';
}
