use super::*;
// G5 / E1: the crate-level `Instant` import is probe-only (gated
// above); these default-build timing tests bring their own.
use ak_identity::DeviceKeyPair;
use ak_store::{
    CheckpointStore, Direction, FileCheckpoint, FileCheckpointStore, TaskRecord as StoredTaskRecord,
};
use std::time::Instant;

/// Unique temp directory helper (tests may use expect/unwrap).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ak-cli-tests-{tag}-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `registry-id` (deploy identity-continuity check): parses with the
/// global --id-dir in either position, reports exactly the pin
/// `registry-server` serves for the same identity (one formatter), and
/// never creates the directory or a key.
#[test]
fn registry_id_reads_the_served_pin_and_never_creates() {
    for argv in [
        ["ak-cli", "--id-dir", "/x", "registry-id"],
        ["ak-cli", "registry-id", "--id-dir", "/x"],
    ] {
        let cli = Cli::try_parse_from(argv).expect("argv must parse");
        assert!(matches!(cli.command, Command::RegistryId));
        assert_eq!(id_dir_path(&cli), PathBuf::from("/x"));
    }
    let tmp = TempDir::new("registry-id");
    let absent = tmp.path().join("absent");
    let err = registry_id_of(&absent).expect_err("a missing identity is an error");
    assert!(
        format!("{err:#}").contains("no usable registry identity"),
        "{err:#}"
    );
    assert!(!absent.exists(), "registry-id must not create the id dir");

    let (served, created) =
        DeviceKeyPair::load_or_create(&tmp.path().join("registry.key")).expect("create");
    assert!(created);
    let pin = registry_id_of(tmp.path()).expect("existing identity");
    assert_eq!(pin, registry_pin(&served));
    assert_eq!(pin.len(), 64);
    assert!(pin.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    let names: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("registry.key")]);

    std::fs::write(tmp.path().join("registry.key"), b"short").expect("write");
    assert!(
        registry_id_of(tmp.path()).is_err(),
        "an invalid seed is an error"
    );
}

fn random_fp() -> Fingerprint {
    DeviceKeyPair::generate().fingerprint()
}

// ---- wave 27: scheduler admission over a Ready relation -------------

/// Render one `.akpair` journal record exactly like
/// `ak_store::pairing_journal` (frozen AKPAIR1 format) and write it
/// under `<id>/pairing/<pairing hex>.akpair`.
fn write_akpair(
    id: &Path,
    pairing_id: &[u8; 16],
    code_id: &[u8; 16],
    peer_public_key: &[u8; 32],
    phase: &str,
    updated_ms: u64,
) {
    let dir = id.join(ak_session::pairing::JOURNAL_SUBDIR);
    std::fs::create_dir_all(&dir).expect("journal dir");
    let text = format!(
        "AKPAIR1\npairing {}\ncode {}\nlookup {}\nsecret {}\npeer {}\nphase {}\ncreated 1\nupdated {updated_ms}\n",
        hex_of(pairing_id),
        hex_of(code_id),
        hex_of(&[0x44u8; 32]),
        hex_of(&[0x55u8; 16]),
        hex_of(peer_public_key),
        phase,
    );
    std::fs::write(dir.join(format!("{}.akpair", hex_of(pairing_id))), text)
        .expect("write akpair record");
}

fn write_registry_pin(id: &Path, registry_id: &[u8; 32]) {
    std::fs::write(
        id.join(invite::REGISTRY_PIN_FILE),
        format!("AKREG1 127.0.0.1:1 {}\n", hex_of(registry_id)),
    )
    .expect("write registry pin");
}

fn relation_fixture(id: &Path, peer: &DeviceKeyPair) -> ([u8; 16], [u8; 16], [u8; 32]) {
    // One applied Ready relation for `peer` plus the registry pin.
    let pairing_id = [0xA1u8; 16];
    let code_id = [0xC3u8; 16];
    let registry_id = [0x91u8; 32];
    write_akpair(
        id,
        &pairing_id,
        &code_id,
        &peer.public_key(),
        "applied",
        5_000,
    );
    write_registry_pin(id, &registry_id);
    (pairing_id, code_id, registry_id)
}

#[test]
fn task_open_group_uses_the_shared_wire_form_convention() {
    // The send path feeds AdmissionCtx::group from the wave-27 shared
    // helper (BLAKE3 of the 16 code bytes) — the exact form the
    // receiving daemon's AdmissionValidator accepts. Pin both the
    // derivation and the all-nonzero shape so a divergence on either
    // side fails here, not on the wire.
    let code_id = [0x0Du8; 16];
    let group = ak_session::runtime::code_group_wire_form(&code_id);
    assert_eq!(group, *blake3::hash(&code_id).as_bytes());
    assert_ne!(group, [0u8; 32], "a real code group is never the sentinel");
}

#[test]
fn ready_relation_resolves_the_applied_relation_and_registry_pin() {
    let dir = TempDir::new("ready-relation");
    let peer = DeviceKeyPair::generate();
    let (pairing_id, code_id, registry_id) = relation_fixture(dir.path(), &peer);

    let relation = ready_relation_for(dir.path(), &peer.fingerprint())
        .expect("lookup succeeds")
        .expect("the applied relation matches");
    assert_eq!(relation.pairing_id, pairing_id);
    assert_eq!(relation.code_id, code_id);
    assert_eq!(relation.registry_id, registry_id);
}

#[test]
fn ready_relation_skips_pending_other_peers_and_revoked_codes() {
    let dir = TempDir::new("ready-skips");
    let peer = DeviceKeyPair::generate();
    let pinned = peer.fingerprint();
    let registry_id = [0x91u8; 32];
    write_registry_pin(dir.path(), &registry_id);

    // Pending transaction for the right peer: not Ready yet.
    write_akpair(
        dir.path(),
        &[0x01u8; 16],
        &[0x0Au8; 16],
        &peer.public_key(),
        "pending",
        9_000,
    );
    // Applied, but for a different peer.
    let other = DeviceKeyPair::generate();
    write_akpair(
        dir.path(),
        &[0x02u8; 16],
        &[0x0Bu8; 16],
        &other.public_key(),
        "applied",
        9_000,
    );
    // Applied for the right peer, but the owner revoked this code
    // locally (the store exists and no longer lists it).
    std::fs::write(
        dir.path().join(invite::INVITE_STORE_FILE),
        "AKINV1 0102030405060708090a0b0c0d0e0f10 0 1 AKI1-XXXX\n",
    )
    .expect("invite store");
    write_akpair(
        dir.path(),
        &[0x03u8; 16],
        &[0xCCu8; 16],
        &peer.public_key(),
        "applied",
        9_000,
    );
    assert!(
        code_revoked_locally(dir.path(), &[0xCCu8; 16]),
        "the store exists without the code: revoked"
    );
    assert!(
        ready_relation_for(dir.path(), &pinned)
            .expect("lookup succeeds")
            .is_none(),
        "pending / foreign / revoked relations never match"
    );

    // A fresh applied relation for a still-held code resolves and the
    // most recently updated one wins over an older relation.
    write_akpair(
        dir.path(),
        &[0x04u8; 16],
        &[0x0Du8; 16],
        &peer.public_key(),
        "applied",
        7_000,
    );
    write_akpair(
        dir.path(),
        &[0x05u8; 16],
        &[0x0Eu8; 16],
        &peer.public_key(),
        "applied",
        8_000,
    );
    // The store still lists the older code, so both stay eligible;
    // recency picks the 0x05 relation.
    std::fs::write(
        dir.path().join(invite::INVITE_STORE_FILE),
        format!(
            "AKINV1 {} 0 1 AKI1-XXXX\nAKINV1 {} 0 1 AKI1-YYYY\n",
            hex_of(&[0x0Du8; 16]),
            hex_of(&[0x0Eu8; 16])
        ),
    )
    .expect("invite store");
    let relation = ready_relation_for(dir.path(), &pinned)
        .expect("lookup succeeds")
        .expect("an eligible relation exists");
    assert_eq!(relation.pairing_id, [0x05u8; 16]);
    assert_eq!(relation.code_id, [0x0Eu8; 16]);
    assert_eq!(relation.registry_id, registry_id);
}

#[test]
fn ready_relation_without_journal_or_pin_keeps_legacy_none() {
    // Fresh identity dir: nothing exists at all.
    let fresh = TempDir::new("ready-fresh");
    assert!(ready_relation_for(fresh.path(), &random_fp())
        .expect("lookup succeeds")
        .is_none());
    assert!(!fresh
        .path()
        .join(ak_session::pairing::JOURNAL_SUBDIR)
        .exists());

    // Journal with a Ready relation but no registry pin: no valid
    // AdmissionCtx is possible (group_registry must name the real
    // registry), so the send keeps the legacy protocol.
    let dir = TempDir::new("ready-nopin");
    let peer = DeviceKeyPair::generate();
    write_akpair(
        dir.path(),
        &[0x21u8; 16],
        &[0x22u8; 16],
        &peer.public_key(),
        "applied",
        1_000,
    );
    assert!(ready_relation_for(dir.path(), &peer.fingerprint())
        .expect("lookup succeeds")
        .is_none());
}

#[test]
fn ready_relation_fails_closed_on_corrupt_journal() {
    let dir = TempDir::new("ready-corrupt");
    let peer = DeviceKeyPair::generate();
    relation_fixture(dir.path(), &peer);
    let journal_dir = dir.path().join(ak_session::pairing::JOURNAL_SUBDIR);
    std::fs::write(journal_dir.join("deadbeef.akpair"), "garbage\n").expect("corrupt record");
    let err = ready_relation_for(dir.path(), &peer.fingerprint())
        .expect_err("a corrupt journal must fail the send, not downgrade it");
    assert!(
        err.to_string().contains("corrupt"),
        "error names the journal problem: {err}"
    );
}

#[test]
fn admission_denied_hint_carries_busy_and_retry_copy() {
    use ak_wire::AdmissionReason;
    for reason in [
        AdmissionReason::SerialWait,
        AdmissionReason::Capacity,
        AdmissionReason::StalePermit,
    ] {
        let hint = admission_denied_hint(reason);
        assert!(hint.contains("BUSY"), "{reason:?}: {hint}");
        assert!(hint.contains("重跑同一命令"), "{reason:?}: {hint}");
        assert!(hint.contains("未发送任何数据"), "{reason:?}: {hint}");
    }
    assert!(
        admission_denied_hint(AdmissionReason::SerialWait).contains("serial_wait"),
        "serial_wait names the same-code constraint"
    );
    assert!(
        admission_denied_hint(AdmissionReason::Capacity).contains("capacity"),
        "capacity names the queue/limit condition"
    );
}

#[test]
fn hex_helpers_round_trip() {
    assert_eq!(hex_to_bytes("00ff10").unwrap(), vec![0, 255, 16]);
    assert!(hex_to_bytes("0").is_err());
    assert!(hex_to_bytes("zz").is_err());
}

#[test]
fn conflict_policy_parses_known_values() {
    assert_eq!(conflict_policy("error").unwrap(), ConflictPolicy::Fail);
    assert_eq!(conflict_policy("rename").unwrap(), ConflictPolicy::Rename);
    assert!(conflict_policy("overwrite").is_err());
}

#[test]
fn fingerprint_parser_rejects_junk() {
    assert!(parse_fingerprint("not-a-fingerprint").is_err());
    assert!(parse_fingerprint(&"x".repeat(64)).is_err());
}

#[test]
fn human_bytes_formats() {
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.00 GiB");
}

// -- expect policy ------------------------------------------------------

#[test]
fn expect_policy_parses_all_forms() {
    let dir = TempDir::new("expect-forms");
    let book_path = dir.path().join("peers.book");
    let fp = random_fp();
    assert!(matches!(
        ExpectPolicy::parse("any", &book_path).unwrap(),
        ExpectPolicy::Any
    ));
    assert!(matches!(
        ExpectPolicy::parse("ANY", &book_path).unwrap(),
        ExpectPolicy::Any
    ));
    assert!(matches!(
        ExpectPolicy::parse(&fp.to_hex(), &book_path).unwrap(),
        ExpectPolicy::Pinned(parsed) if parsed == fp
    ));
    assert!(matches!(
        ExpectPolicy::parse("123-456", &book_path).unwrap(),
        ExpectPolicy::PairingCode(code) if code == "123-456"
    ));
    assert!(matches!(
        ExpectPolicy::parse(" 123 456 ", &book_path).unwrap(),
        ExpectPolicy::PairingCode(_)
    ));
    // Separator placement is lenient (ak-identity filters separators);
    // "123-45-6" means the same code as "123456".
    assert!(matches!(
        ExpectPolicy::parse("123-45-6", &book_path).unwrap(),
        ExpectPolicy::PairingCode(code) if code == "123-45-6"
    ));
    for junk in ["junk", "", "12345", "1234567", "12a456", &"x".repeat(63)] {
        assert!(
            ExpectPolicy::parse(junk, &book_path).is_err(),
            "unexpectedly accepted {junk:?}"
        );
    }
}

#[test]
fn expect_policy_allows_only_matching_peers() {
    let dir = TempDir::new("expect-allows");
    let book_path = dir.path().join("peers.book");
    let pinned = random_fp();
    let stranger = random_fp();
    let policy = ExpectPolicy::parse(&pinned.to_hex(), &book_path).unwrap();
    assert!(policy.allows(&pinned));
    assert!(!policy.allows(&stranger));

    let any = ExpectPolicy::parse("any", &book_path).unwrap();
    assert!(any.allows(&pinned) && any.allows(&stranger));

    let code = ak_identity::pairing_code(&pinned);
    let policy = ExpectPolicy::parse(&code, &book_path).unwrap();
    assert!(policy.allows(&pinned));
    assert!(!policy.allows(&stranger));
}

#[test]
fn expect_policy_completes_unique_book_prefix() {
    let dir = TempDir::new("expect-prefix");
    let book_path = dir.path().join("peers.book");
    let pinned = random_fp();
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &pinned, Some("laptop"));
    book.save(&book_path).unwrap();

    // A unique prefix pins the same peer as the full fingerprint.
    let policy = ExpectPolicy::parse(&pinned.to_hex()[..12], &book_path).unwrap();
    assert!(matches!(
        policy,
        ExpectPolicy::Pinned(parsed) if parsed == pinned
    ));
    assert!(policy.allows(&pinned));
    assert!(!policy.allows(&random_fp()));
}

#[test]
fn book_policy_rereads_peers_book_per_call() {
    let dir = TempDir::new("book-policy");
    let path = dir.path().join("peers.book");
    let peer = random_fp();
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &peer, Some("laptop"));
    book.save(&path).unwrap();

    let policy = ExpectPolicy::Book(path.clone());
    assert!(policy.allows(&peer));
    assert!(!policy.allows(&random_fp()));

    // Removing the peer takes effect without rebuilding the policy.
    let mut book = PeerBook::load(&path).unwrap();
    assert!(book.remove(&peer));
    book.save(&path).unwrap();
    assert!(!policy.allows(&peer));

    // A missing book denies everyone rather than opening up.
    let missing = ExpectPolicy::Book(dir.path().join("nope.book"));
    assert!(!missing.allows(&peer));
}

#[test]
fn resolve_expect_defaults_to_book_and_requires_a_peer() {
    let dir = TempDir::new("resolve-expect");
    let book_path = peer_book_path(dir.path());

    // Missing or empty book: explicit authorization is required.
    assert!(resolve_expect(None, &book_path).is_err());
    PeerBook::default().save(&book_path).unwrap();
    assert!(resolve_expect(None, &book_path).is_err());

    let mut book = PeerBook::default();
    let peer = random_fp();
    upsert_peer(&mut book, &peer, Some("phone"));
    book.save(&book_path).unwrap();
    let policy = resolve_expect(None, &book_path).unwrap();
    assert!(policy.allows(&peer));
    assert!(!policy.allows(&random_fp()));

    // Explicit --expect overrides the book default.
    assert!(matches!(
        resolve_expect(Some("any"), &book_path).unwrap(),
        ExpectPolicy::Any
    ));
}

// -- fingerprint prefix resolution ---------------------------------------

/// Deterministic fingerprint with a controlled hex head (first 4 bytes)
/// and a distinguishing tail byte, for prefix-collision tests.
fn fp_with_head(head: [u8; 4], tail: u8) -> Fingerprint {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&head);
    bytes[31] = tail;
    Fingerprint::from_bytes(bytes)
}

#[test]
fn resolve_fingerprint_arg_completes_unique_prefix() {
    let dir = TempDir::new("fp-unique");
    let book_path = dir.path().join("peers.book");
    let a = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x11); // hex head abcdef01
    let b = fp_with_head([0x12, 0x34, 0x56, 0x78], 0x22); // hex head 12345678
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &a, Some("alpha"));
    upsert_peer(&mut book, &b, Some("beta"));
    book.save(&book_path).unwrap();

    // Minimum length (8), longer, and uppercase input all complete.
    assert_eq!(resolve_fingerprint_arg("abcdef01", &book_path).unwrap(), a);
    assert_eq!(
        resolve_fingerprint_arg(&a.to_hex()[..16], &book_path).unwrap(),
        a
    );
    assert_eq!(
        resolve_fingerprint_arg(&b.to_hex()[..20].to_uppercase(), &book_path).unwrap(),
        b
    );
    // Whitespace is trimmed like everywhere else.
    assert_eq!(
        resolve_fingerprint_arg(" 12345678 ", &book_path).unwrap(),
        b
    );
}

#[test]
fn resolve_fingerprint_arg_rejects_ambiguous_and_unknown_prefixes() {
    let dir = TempDir::new("fp-ambiguous");
    let book_path = dir.path().join("peers.book");
    let a = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x11);
    let b = fp_with_head([0xab, 0xcd, 0xef, 0x01], 0x33); // same head
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &a, Some("alpha"));
    upsert_peer(&mut book, &b, Some("beta"));
    book.save(&book_path).unwrap();

    let ambiguous = resolve_fingerprint_arg("abcdef01", &book_path)
        .unwrap_err()
        .to_string();
    assert!(ambiguous.contains("ambiguous"), "{ambiguous}");
    assert!(ambiguous.contains("matches 2 peers"), "{ambiguous}");
    // A longer prefix disambiguates again (tails differ at hex char 62+).
    assert_eq!(
        resolve_fingerprint_arg(&a.to_hex()[..63], &book_path).unwrap(),
        a
    );

    let unknown = resolve_fingerprint_arg("ffffffffff", &book_path)
        .unwrap_err()
        .to_string();
    assert!(unknown.contains("not found in the peer book"), "{unknown}");
    assert!(
        unknown.contains("needs the full 64-hex fingerprint"),
        "{unknown}"
    );
}

#[test]
fn resolve_fingerprint_arg_full_value_skips_the_book() {
    let dir = TempDir::new("fp-full");
    // No peers.book exists in this directory at all.
    let book_path = dir.path().join("peers.book");
    let fp = random_fp();
    assert_eq!(
        resolve_fingerprint_arg(&fp.to_hex(), &book_path).unwrap(),
        fp
    );
    assert!(resolve_fingerprint_arg(&fp.to_hex()[..63], &book_path).is_err());

    // Malformed shapes keep their explicit errors.
    for bad in ["nope", "", "abcd", &"a".repeat(65), &"z".repeat(64)] {
        assert!(
            resolve_fingerprint_arg(bad, &book_path).is_err(),
            "unexpectedly accepted {bad:?}"
        );
    }
    let short = resolve_fingerprint_arg("abcdef0", &book_path)
        .unwrap_err()
        .to_string();
    assert!(short.contains("8 to 63"), "{short}");
}

#[test]
fn pair_remove_accepts_unique_prefix() {
    let dir = TempDir::new("pair-rm-prefix");
    let fp = random_fp();
    pair_add(dir.path(), &fp.to_hex(), Some("laptop")).unwrap();
    // A unique 8-char prefix removes the entry.
    let removed = pair_remove(dir.path(), &fp.to_hex()[..8]).unwrap();
    assert_eq!(removed, fp);
    // Afterwards the prefix matches nothing.
    let err = pair_remove(dir.path(), &fp.to_hex()[..8])
        .unwrap_err()
        .to_string();
    assert!(err.contains("not found in the peer book"), "{err}");
}

// -- multi-path send ------------------------------------------------------

#[test]
fn send_roots_keeps_primary_and_extra_order() {
    let dir = TempDir::new("send-roots");
    let file1 = dir.path().join("file1.bin");
    let file2 = dir.path().join("file2.bin");
    let sub = dir.path().join("dir1");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(&file1, b"one").unwrap();
    std::fs::write(&file2, b"two").unwrap();

    let (primary, extras) = send_roots(&[file1.clone(), file2.clone(), sub.clone()]).unwrap();
    assert_eq!(primary, file1.canonicalize().unwrap());
    assert_eq!(
        extras,
        vec![file2.canonicalize().unwrap(), sub.canonicalize().unwrap()]
    );

    // Single path: primary only, no extras. No path: explicit error.
    let (primary, extras) = send_roots(&[file1]).unwrap();
    assert!(extras.is_empty());
    assert_eq!(
        primary,
        dir.path().join("file1.bin").canonicalize().unwrap()
    );
    assert!(send_roots(&[]).is_err());
    // A missing path fails naming the path.
    let err = send_roots(&[dir.path().join("missing.bin")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing.bin"), "{err}");
}

#[test]
fn send_spec_extra_roots_follow_add_root_order() {
    let dir = TempDir::new("send-spec-order");
    let file1 = dir.path().join("file1.bin");
    let file2 = dir.path().join("file2.bin");
    let sub = dir.path().join("dir1");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(&file1, b"1").unwrap();
    std::fs::write(&file2, b"2").unwrap();

    let (primary, extras) = send_roots(&[file1, file2, sub]).unwrap();
    let key = DeviceKeyPair::generate();
    let bundle = ak_identity::tls::make_self_signed(&key);
    let mut spec = SendSpec::new(
        primary.clone(),
        "127.0.0.1:7100".parse().unwrap(),
        key.fingerprint(),
        bundle,
    );
    for extra in extras {
        spec.add_root(extra);
    }
    assert_eq!(spec.source_root, primary);
    assert_eq!(spec.extra_roots.len(), 2);
    // add_root order must survive: file2 first, then dir1.
    assert!(spec.extra_roots[0].ends_with("file2.bin"));
    assert!(spec.extra_roots[1].ends_with("dir1"));
}

#[test]
fn send_args_accept_multiple_paths_in_order() {
    let fp = "a".repeat(64);
    let cli = cli_of(&[
        "send",
        "file1.bin",
        "file2.bin",
        "dir1",
        "--to",
        "127.0.0.1:7100",
        "--fp",
        &fp,
    ]);
    let Command::Send { paths, .. } = cli.command else {
        panic!("expected send subcommand");
    };
    let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(names, vec!["file1.bin", "file2.bin", "dir1"]);
}

#[test]
fn duplicate_roots_error_is_recognized_for_a_friendly_hint() {
    let dup = EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "duplicate transfer root names: [photos, photos]",
    ));
    let io = duplicate_roots_error(&dup).expect("duplicate recognized");
    assert!(io.to_string().contains("photos"));

    // Other InvalidInput messages and other kinds are not misdetected.
    let bad_name = EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "root name not usable as a transfer directory name: \"com1\"",
    ));
    assert!(duplicate_roots_error(&bad_name).is_none());
    let not_found = EngineError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "duplicate transfer root names: [x]",
    ));
    assert!(duplicate_roots_error(&not_found).is_none());
}

// -- wave 25: multi-address --to ------------------------------------------

#[test]
fn send_to_flag_parses_single_multiple_space_and_ipv6() {
    let fp = "a".repeat(64);
    let parse_to = |to: &str| -> Vec<SocketAddr> {
        let cli = cli_of(&["send", "x.bin", "--to", to, "--fp", &fp]);
        let Command::Send { to, .. } = cli.command else {
            panic!("expected send subcommand");
        };
        to
    };
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();

    // Single address stays a one-element list (classic compatibility).
    assert_eq!(parse_to("1.2.3.4:7000"), vec![addr("1.2.3.4:7000")]);
    // Comma-separated candidates, with and without spaces after commas.
    assert_eq!(
        parse_to("1.2.3.4:7000,5.6.7.8:7000"),
        vec![addr("1.2.3.4:7000"), addr("5.6.7.8:7000")]
    );
    assert_eq!(
        parse_to("1.2.3.4:7000, 5.6.7.8:7000"),
        vec![addr("1.2.3.4:7000"), addr("5.6.7.8:7000")]
    );
    // IPv6 must be bracketed; mixed families parse in one list.
    assert_eq!(
        parse_to("[2001:db8::1]:7000,1.2.3.4:7000"),
        vec![addr("[2001:db8::1]:7000"), addr("1.2.3.4:7000")]
    );
    // Repeated --to flags accumulate into the same candidate list.
    let cli = cli_of(&[
        "send",
        "x.bin",
        "--to",
        "1.2.3.4:7000",
        "--to",
        "5.6.7.8:7000",
        "--fp",
        &fp,
    ]);
    let Command::Send { to, .. } = cli.command else {
        panic!("expected send subcommand");
    };
    assert_eq!(to, vec![addr("1.2.3.4:7000"), addr("5.6.7.8:7000")]);

    // Missing --to, junk entries, bare IPs, hostnames and stray empty
    // items (trailing comma) are all refused at parse time.
    assert!(Cli::try_parse_from(["ak-cli", "send", "x.bin", "--fp", &fp]).is_err());
    for bad in [
        "1.2.3.4",
        "1.2.3.4:7000,",
        "1.2.3.4:7000,,5.6.7.8:7000",
        "example.com:7000",
        "1.2.3.4:7000,nope",
        "2001:db8::1:7000", // unbracketed v6 is not a socket address
    ] {
        let argv = ["ak-cli", "send", "x.bin", "--to", bad, "--fp", &fp];
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "unexpectedly accepted --to {bad:?}"
        );
    }
}

#[test]
fn order_send_targets_dedupes_and_puts_lan_first() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    let ordered = order_send_targets(vec![
        addr("103.236.80.135:7000"), // public (VPN egress)
        addr("192.168.1.100:7000"),  // LAN
        addr("[2001:db8::1]:7000"),  // IPv6
        addr("10.1.2.3:7000"),       // LAN
        addr("192.168.1.100:7000"),  // duplicate of an earlier target
        addr("172.20.0.9:7000"),     // LAN (172.16-31 range)
        addr("172.32.0.1:7000"),     // public (outside 16-31)
    ]);
    // LAN group first in input order (deduped), then public+v6 in input
    // order (v6 came before 172.32.0.1 in the input and the sort is
    // stable within the two groups).
    assert_eq!(
        ordered,
        vec![
            addr("192.168.1.100:7000"),
            addr("10.1.2.3:7000"),
            addr("172.20.0.9:7000"),
            addr("103.236.80.135:7000"),
            addr("[2001:db8::1]:7000"),
            addr("172.32.0.1:7000"),
        ]
    );
}

#[test]
fn order_send_targets_normalizes_v4_mapped_candidates() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    // v4-mapped v6 candidates dial as plain IPv4 (the engine's send-path
    // family fast-fail compares raw families), and the normalization
    // also makes the dedup family-aware.
    let ordered = order_send_targets(vec![
        addr("[::ffff:127.0.0.1]:7000"),
        addr("127.0.0.1:7000"), // duplicate after normalization
        addr("[::ffff:192.168.1.5]:7000"),
    ]);
    assert_eq!(
        ordered,
        vec![
            addr("127.0.0.1:7000"),
            addr("192.168.1.5:7000"), // LAN tier keeps it after loopback
        ]
    );
    // Native v6 addresses are left alone.
    assert_eq!(
        order_send_targets(vec![addr("[2001:db8::1]:7000")]),
        vec![addr("[2001:db8::1]:7000")]
    );
}

#[test]
fn send_bind_for_matches_candidate_family_and_keeps_port() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    // IPv4 candidates keep the classic wildcard; the explicit --port
    // survives on both families.
    assert_eq!(
        send_bind_for(addr("192.168.1.5:7000"), 7300),
        addr("0.0.0.0:7300")
    );
    assert_eq!(send_bind_for(addr("127.0.0.1:7000"), 0), addr("0.0.0.0:0"));
    // IPv6 candidates get the same-family [::] wildcard (review P2-01:
    // 0.0.0.0 made every v6 candidate fail the family fast-fail).
    assert_eq!(send_bind_for(addr("[::1]:7000"), 7300), addr("[::]:7300"));
    assert_eq!(send_bind_for(addr("[2001:db8::1]:7000"), 0), addr("[::]:0"));
}

#[test]
fn candidates_from_nets_filters_and_labels_ipv4() {
    let net = |ip: &str| ak_net::classify::Net {
        addr: ip.parse().unwrap(),
        prefix_len: 24,
    };
    let nets = vec![
        net("127.0.0.1"),      // loopback (defensive second lock)
        net("169.254.7.7"),    // link-local (DHCP failure / VPN ghost)
        net("192.168.1.100"),  // LAN
        net("103.236.80.135"), // public
        net("fe80::1"),        // v6 link-local: no v6 in the list at all
        net("2001:db8::2"),    // v6 global
        net("192.168.1.100"),  // duplicate interface address
        net("10.8.0.2"),       // LAN (VPN-typical private range)
    ];
    // Wildcard IPv4 bind: the wave-25 expansion over every dialable v4
    // interface (the daemon's fixed 0.0.0.0 bind is exactly this form).
    let candidates = candidates_from_nets(&nets, "0.0.0.0:57210".parse().unwrap());
    let rendered: Vec<String> = candidates
        .iter()
        .map(|candidate| format!("{} ({})", candidate.addr, candidate.label))
        .collect();
    assert_eq!(
        rendered,
        vec![
            "192.168.1.100:57210 (lan)".to_owned(),
            "103.236.80.135:57210 (public)".to_owned(),
            "10.8.0.2:57210 (lan)".to_owned(),
        ]
    );
    // The NDJSON `listening` values are bare socket addresses.
    assert_eq!(
        address_candidates_json(&candidates),
        vec![
            "192.168.1.100:57210".to_owned(),
            "103.236.80.135:57210".to_owned(),
            "10.8.0.2:57210".to_owned(),
        ]
    );
}

#[test]
fn candidates_from_nets_specific_bind_publishes_only_that_endpoint() {
    let net = |ip: &str| ak_net::classify::Net {
        addr: ip.parse().unwrap(),
        prefix_len: 24,
    };
    let nets = vec![
        net("192.168.1.100"),  // LAN interface
        net("10.8.0.2"),       // another interface
        net("103.236.80.135"), // public interface
    ];
    // A bind to one concrete interface listens ONLY there (review
    // P2-02): advertising the other interfaces' ports would send peers
    // to endpoints nobody answers on.
    let candidates = candidates_from_nets(&nets, "192.168.1.5:57210".parse().unwrap());
    assert_eq!(
        candidates,
        vec![AddressCandidate {
            addr: "192.168.1.5:57210".parse().unwrap(),
            label: "lan",
        }]
    );
    // An explicit loopback bind is published as the loopback endpoint.
    assert_eq!(
        candidates_from_nets(&nets, "127.0.0.1:57210".parse().unwrap()),
        vec![AddressCandidate {
            addr: "127.0.0.1:57210".parse().unwrap(),
            label: "loopback",
        }]
    );
    // So is a concrete public bind, and a concrete IPv6 bind (ULA
    // labels "lan", global labels "public").
    assert_eq!(
        candidates_from_nets(&nets, "103.236.80.135:57210".parse().unwrap()),
        vec![AddressCandidate {
            addr: "103.236.80.135:57210".parse().unwrap(),
            label: "public",
        }]
    );
    assert_eq!(
        candidates_from_nets(&nets, "[fd00::9]:57210".parse().unwrap()),
        vec![AddressCandidate {
            addr: "[fd00::9]:57210".parse().unwrap(),
            label: "lan",
        }]
    );
    assert_eq!(
        candidates_from_nets(&nets, "[::1]:57210".parse().unwrap()),
        vec![AddressCandidate {
            addr: "[::1]:57210".parse().unwrap(),
            label: "loopback",
        }]
    );
}

#[test]
fn candidates_from_nets_v6_wildcard_lists_v6_interfaces_only() {
    let net = |ip: &str| ak_net::classify::Net {
        addr: ip.parse().unwrap(),
        prefix_len: 64,
    };
    let nets = vec![
        net("192.168.1.100"), // v4 interfaces are NOT claimed for a v6
        net("10.8.0.2"),      // wildcard bind (v6only is the safe read)
        net("fe80::1"),       // v6 link-local: needs a zone id, not dialable
        net("::1"),           // v6 loopback
        net("fd00::10"),      // unique-local (lan)
        net("2001:db8::2"),   // global (public)
        net("fd00::10"),      // duplicate interface address
    ];
    let candidates = candidates_from_nets(&nets, "[::]:57210".parse().unwrap());
    let rendered: Vec<String> = candidates
        .iter()
        .map(|candidate| format!("{} ({})", candidate.addr, candidate.label))
        .collect();
    assert_eq!(
        rendered,
        vec![
            "[fd00::10]:57210 (lan)".to_owned(),
            "[2001:db8::2]:57210 (public)".to_owned(),
        ],
        "v6 interfaces only, loopback/link-local filtered, deduped: {rendered:?}"
    );
}

#[test]
fn address_candidates_lines_render_aligned_block() {
    // Nothing to advertise: no lines at all (the bind address line above
    // still names the port).
    assert!(address_candidates_lines(&[]).is_empty());
    let candidate = |ip: &str, label: &'static str| AddressCandidate {
        addr: SocketAddr::new(ip.parse().unwrap(), 57210),
        label,
    };
    let lines = address_candidates_lines(&[
        candidate("103.236.80.135", "public"),
        candidate("192.168.1.100", "lan"),
    ]);
    assert_eq!(lines[0], "address candidates (try these):");
    // Column alignment: the shorter LAN address pads to the public one.
    assert_eq!(lines[1], "  103.236.80.135:57210 (public)");
    assert_eq!(lines[2], "  192.168.1.100:57210  (lan)");
}

/// A connect-deadline-shaped failure for the driver tests.
fn fake_timeout_failure(addr: SocketAddr) -> AttemptFailure {
    AttemptFailure::connect_timeout(addr)
}

/// A minimal finished report for the driver tests.
fn fake_report(bytes: u64) -> Report {
    Report {
        job_id: [7u8; 16],
        files: 1,
        bytes,
        duration: Duration::from_millis(1),
        resumed_chunks: 0,
        peer_fingerprint: None,
        roots: Vec::new(),
    }
}

#[tokio::test]
async fn fallback_driver_moves_to_next_address_on_connect_failure() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    let targets = vec![addr("192.168.1.100:7000"), addr("10.0.0.5:7000")];
    let tried = std::sync::Mutex::new(Vec::new());
    let (seen, targets) = (&tried, &targets);
    let attempt = |addr: SocketAddr| async move {
        seen.lock().unwrap().push(addr);
        if addr == targets[0] {
            Err(fake_timeout_failure(addr))
        } else {
            Ok(fake_report(42))
        }
    };
    let report = run_send_fallback(targets, attempt)
        .await
        .expect("the second address delivers");
    assert_eq!(report.bytes, 42);
    assert_eq!(
        *tried.lock().unwrap(),
        vec![addr("192.168.1.100:7000"), addr("10.0.0.5:7000")],
        "tried in order, stopped at success"
    );
}

#[tokio::test]
async fn fallback_driver_reports_every_attempt_when_all_fail() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    let targets = vec![
        addr("192.168.1.100:7000"),
        addr("10.0.0.5:7000"),
        addr("103.236.80.135:7000"),
    ];
    let attempt = |addr: SocketAddr| async move { Err(fake_timeout_failure(addr)) };
    let failures = run_send_fallback(&targets, attempt)
        .await
        .expect_err("all dead");
    assert_eq!(failures.iter().map(|f| f.addr).collect::<Vec<_>>(), targets);
    assert!(
        failures.iter().all(|f| f.reason.starts_with(&format!(
            "timeout after {}s",
            SEND_CONNECT_TIMEOUT.as_secs()
        ))),
        "every reason names the deadline: {failures:?}"
    );
}

#[tokio::test]
async fn fallback_driver_stops_at_first_post_connect_failure() {
    let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
    let targets = vec![addr("192.168.1.100:7000"), addr("10.0.0.5:7000")];
    // The first address connects and then the transfer fails: another
    // candidate cannot help, so the driver must surface it immediately.
    let targets_ref = &targets;
    let attempt = |addr: SocketAddr| async move {
        if addr == targets_ref[0] {
            Err(AttemptFailure {
                addr,
                reason: "peer vanished mid-transfer".to_owned(),
                err: EngineError::PeerClosed,
                after_connect: true,
                hint: None,
            })
        } else {
            panic!("the second address must not be tried after connect");
        }
    };
    let failures = run_send_fallback(&targets, attempt)
        .await
        .expect_err("fatal");
    assert_eq!(
        failures.len(),
        1,
        "only the connected failure: {failures:?}"
    );
    assert_eq!(failures[0].addr, targets[0]);
    assert!(failures[0].after_connect);
}

#[test]
fn send_failures_error_keeps_single_address_wording_classic() {
    let addr: SocketAddr = "1.2.3.4:7000".parse().unwrap();
    let error = send_failures_error(
        vec![AttemptFailure {
            addr,
            reason: "connect failed: io error: timed out".to_owned(),
            err: EngineError::Connect(ak_net::ConnectError::Timeout(SEND_CONNECT_TIMEOUT)),
            after_connect: false,
            hint: None,
        }],
        &[addr],
    );
    let text = format!("{error:#}");
    assert!(
        !text.contains("peer address attempts failed"),
        "single-address errors stay classic: {text}"
    );
    assert!(text.contains("connect failed"), "{text}");
    // The friendly idle-timeout hint rides as context for hinted shapes.
    let hinted = send_failures_error(
        vec![AttemptFailure {
            addr,
            reason: "idle timeout".to_owned(),
            err: EngineError::IdleTimeout(SEND_CONNECT_TIMEOUT),
            after_connect: false,
            hint: None,
        }],
        &[addr],
    );
    let text = format!("{hinted:#}");
    assert!(text.contains("断点已保留"), "hint attached: {text}");
}

#[test]
fn send_failures_error_lists_every_attempt_for_multi_address_runs() {
    let lan: SocketAddr = "192.168.1.100:7000".parse().unwrap();
    let public: SocketAddr = "103.236.80.135:7000".parse().unwrap();
    let error = send_failures_error(
        vec![
            fake_timeout_failure(lan),
            AttemptFailure {
                addr: public,
                reason: "connect failed: io error: connection refused".to_owned(),
                err: EngineError::Connect(ak_net::ConnectError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "connection refused",
                ))),
                after_connect: false,
                hint: None,
            },
        ],
        &[lan, public],
    );
    let text = format!("{error:#}");
    assert!(
        text.contains("all 2 peer address attempts failed"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "192.168.1.100:7000: timeout after {}s",
            SEND_CONNECT_TIMEOUT.as_secs()
        )),
        "{text}"
    );
    assert!(
        text.contains("103.236.80.135:7000: connect failed"),
        "{text}"
    );
}

/// The full wave-25 fallback against the real engine: the first address
/// is a black hole (a bound UDP socket that never answers the QUIC
/// handshake, so the 3-second connect deadline fires), the second is a
/// live loopback receiver -- the send must land on the second and
/// deliver byte-identical content.
#[tokio::test]
async fn send_fallback_times_out_dead_address_and_lands_on_live_one() {
    let dir = TempDir::new("send-fallback-live");
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("black hole bind");
    let dead = blackhole.local_addr().expect("black hole addr");

    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_fp = receiver_key.fingerprint();
    let mut recv_spec = ReceiveSpec::new(
        out.clone(),
        ak_identity::tls::make_self_signed(&receiver_key),
    );
    recv_spec.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    recv_spec.idle_timeout = Duration::from_secs(20);
    recv_spec.authorize = Box::new(|_| true);
    recv_spec.checkpoint_dir = dir.path().join("recv-checkpoints");
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver =
        tokio::spawn(
            async move { ak_engine::receive_with_addr(recv_spec, &|_| {}, addr_tx).await },
        );
    let live = addr_rx.await.expect("receiver bound");

    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let payload: Vec<u8> = (0..60_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("fallback.bin"), &payload).unwrap();

    // Both targets are loopback (LAN tier), so the given order survives:
    // dead first, live second -- exactly the multi-NIC user story.
    let targets = order_send_targets(vec![dead, live]);
    assert_eq!(targets, vec![dead, live]);

    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let (root, checkpoints, sender_bundle) =
        (&src, &dir.path().join("send-checkpoints"), &sender_bundle);
    let printer: &(dyn Fn(Progress) + Send + Sync) = &|_| {};
    let started = Instant::now();
    let attempt = |addr: SocketAddr| async move {
        let mut spec = SendSpec::new(root.clone(), addr, receiver_fp, sender_bundle.clone());
        spec.idle_timeout = Duration::from_secs(20);
        spec.checkpoint_dir = checkpoints.clone();
        attempt_send_to(spec, printer).await
    };
    let report = tokio::time::timeout(
        Duration::from_secs(30),
        run_send_fallback(&targets, attempt),
    )
    .await
    .expect("bounded run")
    .expect("the live address delivers");
    assert_eq!(report.files, 1);
    assert_eq!(report.bytes, payload.len() as u64);
    // The dead address consumed the full connect deadline before the
    // fallback moved on.
    assert!(
        started.elapsed() >= SEND_CONNECT_TIMEOUT,
        "the dead address must burn the full connect deadline"
    );
    assert_eq!(
        std::fs::read(out.join("fallback.bin")).unwrap(),
        payload,
        "byte-identical delivery through the fallback path"
    );
    let recv_report = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("receiver joins")
        .expect("receiver task ok")
        .expect("receive ok");
    assert_eq!(recv_report.bytes, payload.len() as u64);
}

// -- split connect budgets (review P1-01) -----------------------------
//
// Synthetic futures stand in for the engine: the wrapper cannot tell a
// slow `prepare_send` from any other pre-Connecting delay — that is the
// point of the split — so the budget semantics are pinned with cheap
// delays and a shrunken budget instead of real multi-second scans.

/// A future that behaves like `ak_engine::send`: delays before the
/// Connecting mark (the synchronous source scan), then the connect
/// phase, then the connected transfer, driven by `script`.
async fn scripted_send(
    marks: Arc<ConnectPhaseMarks>,
    scan: Duration,
    connect: Duration,
    transfer: Duration,
) -> Result<Report, EngineError> {
    tokio::time::sleep(scan).await;
    marks.mark(&Progress::Connecting);
    tokio::time::sleep(connect).await;
    marks.mark(&Progress::Connected {
        peer: "peer".to_owned(),
    });
    tokio::time::sleep(transfer).await;
    Ok(fake_report(7))
}

#[tokio::test]
async fn connect_budget_ignores_slow_source_scan() {
    // A scan (800ms) far longer than the connect budget (200ms) must
    // not consume it: the attempt still succeeds once the connect phase
    // itself is quick. The pre-fix wrapper (one deadline around the
    // whole future) abandoned this attempt at 200ms.
    let marks = Arc::new(ConnectPhaseMarks::new());
    let send = scripted_send(
        marks.clone(),
        Duration::from_millis(800),
        Duration::from_millis(50),
        Duration::from_millis(10),
    );
    tokio::pin!(send);
    let outcome =
        drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(200)).await;
    assert!(
        matches!(outcome, Ok(Ok(ref report)) if report.bytes == 7),
        "slow scan must not burn the connect budget: {outcome:?}"
    );
}

#[tokio::test]
async fn connect_budget_fires_on_stalled_handshake() {
    // The scan finishes fast, but the handshake never completes: the
    // budget (200ms) fires well before the would-be 5s "engine"
    // timeout, and only because the scan did not eat into it.
    let marks = Arc::new(ConnectPhaseMarks::new());
    let send = scripted_send(
        marks.clone(),
        Duration::from_millis(50),
        Duration::from_secs(5),
        Duration::from_millis(10),
    );
    tokio::pin!(send);
    let started = Instant::now();
    let outcome =
        drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(200)).await;
    assert!(outcome.is_err(), "stalled handshake must hit the budget");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "budget fired near 200ms, not at the 5s inner timeout"
    );
}

#[tokio::test]
async fn connected_transfer_outlives_connect_budget() {
    // Connected inside the budget, then a transfer (800ms) longer than
    // the budget (200ms): the deadline no longer applies and the
    // transfer runs to completion under its own idle budget.
    let marks = Arc::new(ConnectPhaseMarks::new());
    let send = scripted_send(
        marks.clone(),
        Duration::from_millis(10),
        Duration::from_millis(50),
        Duration::from_millis(800),
    );
    tokio::pin!(send);
    let outcome =
        drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(200)).await;
    assert!(
        matches!(outcome, Ok(Ok(ref report)) if report.bytes == 7),
        "connected transfers are governed by the idle budget: {outcome:?}"
    );
}

#[tokio::test]
async fn failure_before_connecting_keeps_engine_error() {
    // An attempt that fails during preparation (duplicate roots, bind
    // error, ...) never reaches the connect phase: the engine error
    // surfaces untouched, not a connect-budget timeout, so the classic
    // single-address wording still applies.
    let marks = Arc::new(ConnectPhaseMarks::new());
    let send = async { Err::<Report, EngineError>(EngineError::PeerClosed) };
    tokio::pin!(send);
    let outcome =
        drive_with_connect_budget(&marks, send.as_mut(), Duration::from_millis(200)).await;
    match outcome {
        Ok(Err(EngineError::PeerClosed)) => {}
        other => panic!("engine error must pass through: {other:?}"),
    }
    assert!(!marks.connected.load(std::sync::atomic::Ordering::Relaxed));
}

// -- IPv6 candidates vs the local bind (review P2-01) -----------------

/// Real-engine IPv6 loopback send: with the family-matched `[::]` bind
/// the candidate connects and delivers; with the old fixed `0.0.0.0`
/// bind the engine's family fast-fail rejects it before the handshake.
/// Skips (passes with a notice) on hosts without IPv6 loopback.
#[tokio::test]
async fn ipv6_loopback_candidate_needs_family_matched_bind() {
    let dir = TempDir::new("send-ipv6-loopback");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_fp = receiver_key.fingerprint();
    let mut recv_spec = ReceiveSpec::new(
        out.clone(),
        ak_identity::tls::make_self_signed(&receiver_key),
    );
    recv_spec.bind = "[::1]:0".parse().unwrap();
    recv_spec.idle_timeout = Duration::from_secs(20);
    recv_spec.authorize = Box::new(|_| true);
    recv_spec.checkpoint_dir = dir.path().join("recv-checkpoints");
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver =
        tokio::spawn(
            async move { ak_engine::receive_with_addr(recv_spec, &|_| {}, addr_tx).await },
        );
    let live = match addr_rx.await {
        Ok(addr) => addr,
        Err(_) => {
            eprintln!("skipping: [::1] bind unavailable on this host");
            return;
        }
    };
    assert!(live.is_ipv6(), "receiver really bound v6: {live}");

    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 249) as u8).collect();
    std::fs::write(src.join("v6.bin"), &payload).unwrap();

    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let checkpoints = dir.path().join("send-checkpoints");
    let printer: &(dyn Fn(Progress) + Send + Sync) = &|_| {};
    let spec_for = |bind: SocketAddr| {
        let mut spec = SendSpec::new(src.clone(), live, receiver_fp, sender_bundle.clone());
        spec.bind = bind;
        spec.idle_timeout = Duration::from_secs(20);
        spec.checkpoint_dir = checkpoints.clone();
        spec
    };

    // The old fixed v4 bind is rejected by the engine's family
    // fast-fail before any handshake (fast, no 3s burn).
    let mismatch = attempt_send_to(spec_for("0.0.0.0:0".parse().unwrap()), printer)
        .await
        .expect_err("v4 socket cannot dial a native v6 peer");
    assert!(
        mismatch.reason.contains("address family mismatch"),
        "engine fast-fail reason: {}",
        mismatch.reason
    );
    assert!(!mismatch.after_connect);

    // The family-matched [::] bind (send_bind_for's choice for v6
    // candidates) connects and delivers byte-identical content.
    let report = tokio::time::timeout(
        Duration::from_secs(20),
        attempt_send_to(spec_for(send_bind_for(live, 0)), printer),
    )
    .await
    .expect("bounded run")
    .expect("the v6 loopback candidate delivers");
    assert_eq!(report.bytes, payload.len() as u64);
    assert_eq!(std::fs::read(out.join("v6.bin")).unwrap(), payload);
    let recv_report = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("receiver joins")
        .expect("receiver task ok")
        .expect("receive ok");
    assert_eq!(recv_report.bytes, payload.len() as u64);
}

/// Real-engine fallback across families (review P2-01): an IPv4 black
/// hole burns its 3s connect budget, the fallback moves to the IPv6
/// loopback receiver and delivers. Both targets are loopback tier, so
/// the dead-first order survives the LAN sort.
#[tokio::test]
async fn send_fallback_moves_from_dead_ipv4_to_live_ipv6() {
    let dir = TempDir::new("send-fallback-v6");
    let blackhole = match tokio::net::UdpSocket::bind("127.0.0.1:0").await {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!("skipping: blackhole bind failed ({err})");
            return;
        }
    };
    let dead = blackhole.local_addr().expect("black hole addr");

    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let receiver_key = DeviceKeyPair::generate();
    let receiver_fp = receiver_key.fingerprint();
    let mut recv_spec = ReceiveSpec::new(
        out.clone(),
        ak_identity::tls::make_self_signed(&receiver_key),
    );
    recv_spec.bind = "[::1]:0".parse().unwrap();
    recv_spec.idle_timeout = Duration::from_secs(20);
    recv_spec.authorize = Box::new(|_| true);
    recv_spec.checkpoint_dir = dir.path().join("recv-checkpoints");
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let receiver =
        tokio::spawn(
            async move { ak_engine::receive_with_addr(recv_spec, &|_| {}, addr_tx).await },
        );
    let live = match addr_rx.await {
        Ok(addr) => addr,
        Err(_) => {
            eprintln!("skipping: [::1] bind unavailable on this host");
            return;
        }
    };

    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("fallback-v6.bin"), &payload).unwrap();

    let targets = order_send_targets(vec![dead, live]);
    assert_eq!(targets, vec![dead, live], "loopback tier keeps order");

    let sender_bundle = ak_identity::tls::make_self_signed(&DeviceKeyPair::generate());
    let (root, checkpoints, sender_bundle) =
        (&src, &dir.path().join("send-checkpoints"), &sender_bundle);
    let printer: &(dyn Fn(Progress) + Send + Sync) = &|_| {};
    let started = Instant::now();
    let attempt = |addr: SocketAddr| async move {
        let mut spec = SendSpec::new(root.clone(), addr, receiver_fp, sender_bundle.clone());
        spec.bind = send_bind_for(addr, 0); // per-candidate family (P2-01)
        spec.idle_timeout = Duration::from_secs(20);
        spec.checkpoint_dir = checkpoints.clone();
        attempt_send_to(spec, printer).await
    };
    let report = tokio::time::timeout(
        Duration::from_secs(30),
        run_send_fallback(&targets, attempt),
    )
    .await
    .expect("bounded run")
    .expect("the IPv6 address delivers after the IPv4 failure");
    assert_eq!(report.bytes, payload.len() as u64);
    assert!(
        started.elapsed() >= SEND_CONNECT_TIMEOUT,
        "the dead IPv4 address burned its full connect budget first"
    );
    assert_eq!(
        std::fs::read(out.join("fallback-v6.bin")).unwrap(),
        payload,
        "byte-identical delivery over the v6 fallback path"
    );
    let recv_report = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("receiver joins")
        .expect("receiver task ok")
        .expect("receive ok");
    assert_eq!(recv_report.bytes, payload.len() as u64);
}

#[test]
fn roots_line_joins_report_roots_and_hides_when_empty() {
    let report = |roots: Vec<&str>| Report {
        job_id: [0u8; 16],
        files: 3,
        bytes: 10,
        duration: Duration::from_millis(1),
        resumed_chunks: 0,
        peer_fingerprint: Some("ab".repeat(32)),
        roots: roots.into_iter().map(str::to_owned).collect(),
    };
    assert_eq!(roots_line(&report(Vec::new())), None);
    assert_eq!(
        roots_line(&report(vec!["file1.bin", "file2.bin", "dir1"])),
        Some("roots: file1.bin, file2.bin, dir1".to_owned())
    );
}

// -- probe same-egress notice ---------------------------------------------
// G5 / E1: probe-only tests compile only with the legacy feature.
#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn same_egress_notice_fires_exactly_once() {
    let mut notice = OnceNotice::new();
    assert_eq!(notice.take(), Some(SAME_EGRESS_NOTICE));
    assert_eq!(notice.take(), None);
    assert_eq!(notice.take(), None);
}

// -- pair book round trip ------------------------------------------------

#[test]
fn pair_add_list_remove_round_trip() {
    let dir = TempDir::new("pair-roundtrip");
    let fp = random_fp();

    let (parsed, added) = pair_add(dir.path(), &fp.to_hex(), Some("laptop")).unwrap();
    assert_eq!(parsed, fp);
    assert!(added);
    let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(book.iter().count(), 1);
    assert_eq!(book.get(&fp).map(|r| r.name.as_str()), Some("laptop"));

    // Re-adding updates the name instead of duplicating the entry.
    let (_, added) = pair_add(dir.path(), &fp.to_hex(), Some("deck")).unwrap();
    assert!(!added);
    let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(book.iter().count(), 1);
    assert_eq!(book.get(&fp).map(|r| r.name.as_str()), Some("deck"));

    let removed = pair_remove(dir.path(), &fp.to_hex()).unwrap();
    assert_eq!(removed, fp);
    assert_eq!(
        PeerBook::load(&peer_book_path(dir.path()))
            .unwrap()
            .iter()
            .count(),
        0
    );
    // Removing an unknown fingerprint is an error, not a silent no-op.
    assert!(pair_remove(dir.path(), &fp.to_hex()).is_err());
    // Malformed fingerprints never reach the book.
    assert!(pair_add(dir.path(), "nope", None).is_err());
}

#[test]
fn sender_tofu_preserves_existing_names() {
    let dir = TempDir::new("tofu");
    let known = random_fp();
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &known, Some("alice"));
    book.save(&peer_book_path(dir.path())).unwrap();

    // Known peer: no change, no new record.
    assert!(!record_peer_after_send(dir.path(), &known).unwrap());
    let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(book.iter().count(), 1);
    assert_eq!(book.get(&known).map(|r| r.name.as_str()), Some("alice"));

    // Unknown peer: added with an empty name for later renaming.
    let stranger = random_fp();
    assert!(record_peer_after_send(dir.path(), &stranger).unwrap());
    let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(book.iter().count(), 2);
    assert_eq!(
        book.get(&stranger).map(|r| r.name.as_str()),
        Some("") // renamed via peer_display_name in the UI
    );
}

// -- jobs ----------------------------------------------------------------

fn stored_record(job_byte: u8, state: &str) -> StoredTaskRecord {
    StoredTaskRecord {
        job_id: [job_byte; 16],
        direction: Direction::Send,
        root: PathBuf::from(r"D:\send dir\photos"),
        peer_fingerprint: Some(Fingerprint::from_bytes([9u8; 32])),
        chunk_size: 262_144,
        total_files: 3,
        total_bytes: 10_000_000,
        state: state.to_owned(),
        created_unix_ms: 1_730_000_000_000,
        updated_unix_ms: 1_730_000_500_000,
        files: vec![FileCheckpoint {
            file_index: 0,
            size: 4_000,
            verified_prefix_chunks: 2,
        }],
    }
}

#[test]
fn jobs_list_parses_file_checkpoint_store_output() {
    let dir = TempDir::new("jobs-list");
    let store = FileCheckpointStore::open(dir.path()).unwrap();
    store.save(&stored_record(7, "completed")).unwrap();
    store.save(&stored_record(8, "transferring")).unwrap();

    let listed = list_job_files(dir.path());
    assert_eq!(listed.len(), 2);
    let entry = listed
        .iter()
        .find(|(_, r)| r.job_hex == "07".repeat(16))
        .map(|(_, r)| r.clone())
        .expect("completed record listed");
    assert_eq!(entry.direction, "send");
    assert_eq!(entry.root, r"D:\send dir\photos");
    assert_eq!(entry.peer_short.as_deref(), Some("09090909"));
    assert_eq!(entry.total_files, 3);
    assert_eq!(entry.total_bytes, 10_000_000);
    assert_eq!(entry.state, "completed");
    assert_eq!(entry.updated_unix_ms, 1_730_000_500_000);

    // Corrupt and foreign files are skipped, never fatal.
    std::fs::write(
        dir.path().join("beef00112233445566778899aabbccdd.akjob"),
        b"junk",
    )
    .unwrap();
    std::fs::write(dir.path().join("keep.akpart"), b"parts").unwrap();
    assert_eq!(list_job_files(dir.path()).len(), 2);
}

#[test]
fn parse_job_record_rejects_tampered_lines() {
    let dir = TempDir::new("jobs-tamper");
    let store = FileCheckpointStore::open(dir.path()).unwrap();
    store.save(&stored_record(1, "transferring")).unwrap();
    let original =
        std::fs::read_to_string(dir.path().join(format!("{}.akjob", "01".repeat(16)))).unwrap();
    assert!(parse_job_record(&original).is_some());

    let variants = [
        original.replacen("AKJOB1", "AKJOB2", 1),
        original.replacen("state transferring", "state", 1),
        original.replacen("dir send", "dir sideways", 1),
        original.replacen("root ", "roots ", 1),
        format!("{original}extra junk\n"),
    ];
    for variant in &variants {
        assert!(
            parse_job_record(variant).is_none(),
            "tampered record parsed: {variant:?}"
        );
    }
}

#[test]
fn jobs_clean_filters_by_scope_and_keeps_part_files() {
    let dir = TempDir::new("jobs-clean");
    let store = FileCheckpointStore::open(dir.path()).unwrap();
    store.save(&stored_record(1, "completed")).unwrap();
    store.save(&stored_record(2, "transferring")).unwrap();
    // A corrupt record file still counts for --all but not --completed.
    std::fs::write(
        dir.path().join("beef00112233445566778899aabbccdd.akjob"),
        b"junk",
    )
    .unwrap();
    // Part files must survive every clean scope.
    std::fs::write(dir.path().join("0001.akpart"), b"partial data").unwrap();

    let (removed, kept) = clean_job_records(dir.path(), CleanScope::Completed).unwrap();
    assert_eq!((removed, kept), (1, 1));
    assert!(dir.path().join("0001.akpart").exists());
    assert!(dir
        .path()
        .join(format!("{}.akjob", "02".repeat(16)))
        .exists());
    assert!(!dir
        .path()
        .join(format!("{}.akjob", "01".repeat(16)))
        .exists());

    let (removed, kept) = clean_job_records(dir.path(), CleanScope::All).unwrap();
    assert_eq!(removed, 2); // remaining valid record + corrupt file
    assert_eq!(kept, 0);
    assert!(dir.path().join("0001.akpart").exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .count(),
        1
    );

    // Cleaning a missing directory is a no-op, not an error.
    let (removed, kept) = clean_job_records(&dir.path().join("nope"), CleanScope::All).unwrap();
    assert_eq!((removed, kept), (0, 0));
}

#[test]
fn jobs_status_renders_detail_from_real_store_fixture() {
    let dir = TempDir::new("jobs-status");
    let store = FileCheckpointStore::open(dir.path()).unwrap();
    let mut record = stored_record(7, "transferring");
    record.files = (0..25)
        .map(|i| FileCheckpoint {
            file_index: i,
            size: 1000 + u64::from(i),
            verified_prefix_chunks: u64::from(i),
        })
        .collect();
    store.save(&record).unwrap();
    // A second record proves status picks exactly the requested job and
    // resolves uppercase hex input to the lowercase record id.
    store.save(&stored_record(0xab, "completed")).unwrap();

    let text = jobs_status_text(dir.path(), &"07".repeat(16)).unwrap();
    assert!(text.contains(&format!("job {}", "07".repeat(16))), "{text}");
    assert!(text.contains("direction: send"), "{text}");
    assert!(text.contains("state:     transferring"), "{text}");
    assert!(text.contains(r"root:      D:\send dir\photos"), "{text}");
    assert!(text.contains("peer:      09090909"), "{text}");
    assert!(
        text.contains("totals:    3 files, 10000000 bytes (9.54 MiB)"),
        "{text}"
    );
    assert!(text.contains("chunk:     262144 bytes"), "{text}");
    assert!(
        text.contains("created:") && text.contains("updated:"),
        "{text}"
    );
    // First 20 per-file rows render; the remaining 5 fold into a count.
    assert!(
        text.contains("files:     25 checkpointed file(s)"),
        "{text}"
    );
    assert!(
        text.contains("  file 0: 1000 bytes, 0 verified chunk(s)"),
        "{text}"
    );
    assert!(
        text.contains("  file 19: 1019 bytes, 19 verified chunk(s)"),
        "{text}"
    );
    assert!(!text.contains("file 20:"), "{text}");
    assert!(text.contains("... and 5 more file(s) not shown"), "{text}");

    let upper = jobs_status_text(dir.path(), &"AB".repeat(16)).unwrap();
    assert!(upper.contains("state:     completed"), "{upper}");
    assert!(
        upper.contains(&format!("job {}", "ab".repeat(16))),
        "{upper}"
    );
}

#[test]
fn jobs_status_rejects_unknown_or_malformed_job_ids() {
    let dir = TempDir::new("jobs-status-bad");
    let store = FileCheckpointStore::open(dir.path()).unwrap();
    store.save(&stored_record(7, "completed")).unwrap();

    let unknown = jobs_status_text(dir.path(), &"09".repeat(16))
        .unwrap_err()
        .to_string();
    assert!(unknown.contains("no checkpoint"), "{unknown}");
    assert!(unknown.contains(&"09".repeat(16)), "{unknown}");
    assert!(unknown.contains("jobs list"), "{unknown}");
    for bad in ["", "not-hex", "0707", &"0".repeat(33)] {
        let err = jobs_status_text(dir.path(), bad).unwrap_err().to_string();
        assert!(err.contains("32 hex"), "{err} for {bad:?}");
    }
}

#[test]
fn jobs_status_args_parse() {
    let cli = cli_of(&["jobs", "status", &"07".repeat(16)]);
    let Command::Jobs {
        cmd: Some(JobsCommand::Status { job }),
    } = cli.command
    else {
        panic!("expected jobs status");
    };
    assert_eq!(job, "07".repeat(16));
    // The job id argument is required.
    assert!(Cli::try_parse_from(["ak-cli", "jobs", "status"]).is_err());
    // Plain `jobs` keeps listing.
    assert!(matches!(
        cli_of(&["jobs"]).command,
        Command::Jobs { cmd: None }
    ));
}

// -- display helpers -----------------------------------------------------

#[test]
fn civil_dates_and_utc_formatting_match_known_values() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    assert_eq!(civil_from_days(20_666), (2026, 8, 1));
    assert_eq!(civil_from_days(20_759), (2026, 11, 2));
    assert_eq!(format_utc_ms(0), "1970-01-01 00:00:00 UTC");
    // 2026-11-02 00:00:00 UTC = 1793577600 s; plus 3661 s = 01:01:01.
    let day = 20_759 * 86_400;
    assert_eq!(
        format_utc_ms((day as u64 + 3661) * 1000),
        "2026-11-02 01:01:01 UTC"
    );
}

#[test]
fn fingerprint_two_line_display_has_short_and_full() {
    let fp = random_fp();
    let text = fingerprint_lines("fingerprint", &fp);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], format!("fingerprint: {}", fp.short_id()));
    assert_eq!(lines[1], format!("fingerprint (full): {}", fp.to_hex()));
    assert_eq!(peer_display_name(""), "(unnamed)");
    assert_eq!(peer_display_name("laptop"), "laptop");
}

#[test]
fn json_escape_handles_quotes_controls_and_unicode() {
    assert_eq!(json_escape("plain"), "plain");
    assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    assert_eq!(json_escape("line\nnext\ttab"), "line\\nnext\\ttab");
    assert_eq!(json_escape("\u{1}"), "\\u0001");
    assert_eq!(json_escape("设备"), "设备");
}

#[test]
fn discover_json_marks_paired_candidates() {
    let book_path_dir = TempDir::new("discover-json");
    let peer = random_fp();
    let mut book = PeerBook::default();
    upsert_peer(&mut book, &peer, Some("Alice \"Lap\"top"));
    book.save(&book_path_dir.path().join("peers.book")).unwrap();
    let book = PeerBook::load(&book_path_dir.path().join("peers.book")).unwrap();

    let candidates = vec![
        ak_discovery::PeerCandidate {
            addr: "192.168.1.9:7100".parse().expect("addr"),
            fingerprint: Some(peer),
            source: ak_discovery::CandidateSource::Mdns,
            device_name: None,
        },
        ak_discovery::PeerCandidate {
            addr: "192.168.1.10:7100".parse().expect("addr"),
            fingerprint: Some(random_fp()),
            source: ak_discovery::CandidateSource::Mdns,
            device_name: None,
        },
        ak_discovery::PeerCandidate {
            addr: "192.168.1.11:7100".parse().expect("addr"),
            fingerprint: None,
            source: ak_discovery::CandidateSource::Manual,
            device_name: None,
        },
    ];
    let json = discover_json(&candidates, &book);
    assert!(json.starts_with("[\n") && json.ends_with(']'));
    assert!(json.contains(&format!("\"fingerprint\": \"{}\"", peer.to_hex())));
    assert!(json.contains("\"paired\": true"));
    assert!(json.contains("\"name\": \"Alice \\\"Lap\\\"top\""));
    assert!(json.contains("\"paired\": false"));
    assert!(json.contains("\"fingerprint\": null"));
    assert!(json.contains("\"source\": \"mdns\""));
    assert!(json.contains("\"source\": \"manual\""));
    assert_eq!(json.matches('\n').count(), 4); // no trailing junk
}

// -- bench ---------------------------------------------------------------

/// Parse a Cli from argv (tests may use expect/unwrap).
fn cli_of(args: &[&str]) -> Cli {
    let mut argv = vec!["ak-cli"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).expect("argv must parse")
}

#[test]
fn bench_args_defaults_and_overrides() {
    let cli = cli_of(&["bench"]);
    let Command::Bench {
        bytes,
        files,
        chunk_kb,
        multistream,
        once,
        json,
    } = cli.command
    else {
        panic!("expected bench subcommand");
    };
    assert_eq!((bytes, files, chunk_kb), (268_435_456, 8, 1024));
    assert!(!multistream && !once && !json);

    let cli = cli_of(&[
        "bench",
        "--bytes",
        "1000",
        "--files",
        "4",
        "--chunk-kb",
        "64",
        "--multistream",
        "--once",
        "--json",
    ]);
    let Command::Bench {
        bytes,
        files,
        chunk_kb,
        multistream,
        once,
        json,
    } = cli.command
    else {
        panic!("expected bench subcommand");
    };
    assert_eq!((bytes, files, chunk_kb), (1000, 4, 64));
    assert!(multistream && once && json);
}

#[test]
fn bench_args_reject_out_of_range_values() {
    for bad in [
        vec!["bench", "--bytes", "0"],
        vec!["bench", "--files", "0"],
        vec!["bench", "--chunk-kb", "0"],
        vec!["bench", "--chunk-kb", "8193"], // above the engine decode budget
    ] {
        let mut argv = vec!["ak-cli"];
        argv.extend_from_slice(&bad);
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "unexpectedly accepted {bad:?}"
        );
    }
}

#[test]
fn doctor_args_defaults_overrides_and_ranges() {
    // Defaults: STUN skipped, 3s mDNS window, text output.
    let cli = cli_of(&["doctor"]);
    let Command::Doctor {
        stun,
        mdns_seconds,
        json,
    } = cli.command
    else {
        panic!("expected doctor subcommand");
    };
    assert_eq!(stun, None);
    assert_eq!(mdns_seconds, 3);
    assert!(!json);

    // Explicit server, window and JSON output.
    let cli = cli_of(&[
        "doctor",
        "--stun",
        "127.0.0.1:3478",
        "--mdns-seconds",
        "1",
        "--json",
    ]);
    let Command::Doctor {
        stun,
        mdns_seconds,
        json,
    } = cli.command
    else {
        panic!("expected doctor subcommand");
    };
    assert_eq!(stun, Some("127.0.0.1:3478".parse().expect("stun addr")));
    assert_eq!(mdns_seconds, 1);
    assert!(json);

    // The mDNS window is bounded on both ends; malformed servers reject.
    for bad in [
        vec!["doctor", "--mdns-seconds", "0"],
        vec!["doctor", "--mdns-seconds", "31"],
        vec!["doctor", "--stun", "not-an-addr"],
        vec!["doctor", "--stun", "127.0.0.1"],
    ] {
        let mut argv = vec!["ak-cli"];
        argv.extend_from_slice(&bad);
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "unexpectedly accepted {bad:?}"
        );
    }
}

#[test]
fn timeout_flag_maps_to_idle_timeout_on_both_verbs() {
    // send: default 30, explicit override 5.
    let fp = "a".repeat(64);
    let send_args = |extra: &[&str]| -> Vec<String> {
        let mut argv: Vec<String> = vec![
            "ak-cli".into(),
            "send".into(),
            "x.bin".into(),
            "--to".into(),
            "127.0.0.1:7100".into(),
            "--fp".into(),
            fp.clone(),
        ];
        argv.extend(extra.iter().map(|arg| String::from(*arg)));
        argv
    };
    let cli = Cli::try_parse_from(send_args(&[])).unwrap();
    let Command::Send { timeout, .. } = cli.command else {
        panic!("expected send subcommand");
    };
    assert_eq!(timeout, 30);
    assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(30));

    let cli = Cli::try_parse_from(send_args(&["--timeout", "5"])).unwrap();
    let Command::Send { timeout, .. } = cli.command else {
        panic!("expected send subcommand");
    };
    assert_eq!(timeout, 5);
    assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(5));

    // receive: same default and override.
    let recv_args = |extra: &[&str]| -> Vec<String> {
        let mut argv: Vec<String> = vec!["ak-cli".into(), "receive".into()];
        argv.extend(extra.iter().map(|arg| String::from(*arg)));
        argv
    };
    let cli = Cli::try_parse_from(recv_args(&[])).unwrap();
    let Command::Receive { timeout, .. } = cli.command else {
        panic!("expected receive subcommand");
    };
    assert_eq!(timeout, 30);
    let cli = Cli::try_parse_from(recv_args(&["--timeout", "17"])).unwrap();
    let Command::Receive { timeout, .. } = cli.command else {
        panic!("expected receive subcommand");
    };
    assert_eq!(timeout, 17);
    assert_eq!(idle_timeout_from_secs(timeout), Duration::from_secs(17));

    // Zero is an idle budget of nothing; rejected for both verbs.
    assert!(Cli::try_parse_from(send_args(&["--timeout", "0"])).is_err());
    assert!(Cli::try_parse_from(recv_args(&["--timeout", "0"])).is_err());
}

#[test]
fn xorshift_filler_is_deterministic_and_distinct() {
    // Same seed, same stream.
    let mut a = Xorshift64::new(42);
    let mut b = Xorshift64::new(42);
    let (mut buf_a, mut buf_b, mut buf_c) = ([0u8; 20], [0u8; 20], [0u8; 20]);
    a.fill(&mut buf_a);
    b.fill(&mut buf_b);
    Xorshift64::new(43).fill(&mut buf_c);
    assert_eq!(buf_a, buf_b);
    assert_ne!(buf_a, buf_c);
    assert!(buf_a.iter().any(|byte| *byte != 0), "degenerate stream");

    // Zero seed recovers instead of freezing.
    let mut zero = Xorshift64::new(0);
    let mut buf_zero = [0u8; 8];
    zero.fill(&mut buf_zero);
    assert!(buf_zero.iter().any(|byte| *byte != 0), "zero seed froze");

    // Word-stream continuity: one 16-byte fill equals two 8-byte fills.
    let mut once = Xorshift64::new(7);
    let mut twice = Xorshift64::new(7);
    let (mut whole, mut left, mut right) = ([0u8; 16], [0u8; 8], [0u8; 8]);
    once.fill(&mut whole);
    twice.fill(&mut left);
    twice.fill(&mut right);
    assert_eq!(&whole[..8], &left);
    assert_eq!(&whole[8..], &right);
}

#[test]
fn bench_file_sizes_split_evenly_and_sum_to_total() {
    assert_eq!(bench_file_sizes(10, 3), vec![4, 3, 3]);
    assert_eq!(bench_file_sizes(8, 8), vec![1; 8]);
    assert_eq!(bench_file_sizes(3, 5), vec![1, 1, 1, 0, 0]);
    for (total, files) in [(1u64, 1u32), (1_000_001, 7), (268_435_456, 8)] {
        let sizes = bench_file_sizes(total, files);
        assert_eq!(sizes.len(), files as usize);
        assert_eq!(sizes.iter().sum::<u64>(), total);
    }
}

#[test]
fn bench_json_has_machine_readable_shape() {
    let rounds = vec![
        BenchRound {
            mode: "single",
            duration: Duration::from_millis(2000),
            bytes: 100_000_000,
        },
        BenchRound {
            mode: "multistream",
            duration: Duration::from_millis(1000),
            bytes: 100_000_000,
        },
    ];
    let json = bench_json(100_000_000, 8, 1024, &rounds, Some(2.0));
    assert!(json.starts_with('{') && json.ends_with('}'));
    assert!(json.contains("\"bytes\": 100000000"));
    assert!(json.contains("\"files\": 8"));
    assert!(json.contains("\"chunk_kb\": 1024"));
    assert!(json.contains("\"mode\": \"single\""));
    assert!(json.contains("\"mode\": \"multistream\""));
    assert!(json.contains("\"duration_ms\": 2000.000"));
    assert!(json.contains("\"mb_per_s\": 50.000"));
    assert!(json.contains("\"gib_per_s\": 0.046566"));
    assert!(json.contains("\"ratio\": 2.0000"));
    assert!(json.contains(BENCH_DISCLAIMER));
    assert!(!json.contains("\"ratio\": null"));

    let one = vec![BenchRound {
        mode: "single",
        duration: Duration::from_millis(500),
        bytes: 1000,
    }];
    let json = bench_json(1000, 1, 1, &one, None);
    assert!(json.contains("\"ratio\": null"));
    assert!(!json.contains("inf") && !json.contains("NaN"));
}

#[test]
fn clip_line_truncates_on_char_boundary_within_width() {
    assert_eq!(clip_line("short", 10), "short");
    let clipped = clip_line(&"x".repeat(100), 79);
    assert_eq!(clipped.chars().count(), 79);
    assert!(clipped.ends_with("..."));
    // Unicode path characters never split a char.
    let clipped = clip_line(&"设".repeat(100), 79);
    assert_eq!(clipped.chars().count(), 79);
    // Tiny widths degrade to a plain truncation, never wider than width.
    assert_eq!(clip_line("abc", 2), "ab");
}

// -- --version --------------------------------------------------------------

#[test]
fn version_line_carries_wire_facts_and_profile() {
    // Wire facts now come from ak-wire itself; pin the shipped values.
    assert_eq!(WIRE_VERSION, 1);
    assert_eq!(
        (
            ak_wire::CAP_RESUME,
            ak_wire::CAP_PAUSE_RESUME,
            ak_wire::CAP_MULTISTREAM
        ),
        (0x0001, 0x0002, 0x0004)
    );
    assert_eq!(WIRE_CAPS, 0x0007);

    let line = version_line();
    assert!(line.starts_with(&format!("{} (", env!("CARGO_PKG_VERSION"))));
    assert!(line.contains("wire v1"));
    assert!(line.contains("caps 0x0007"));
    assert!(line.contains("[resume|pause-resume|multistream]"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    // CR-P2-09: the source revision is part of the payload, and the
    // stamped value is whatever build.rs decided (real sha, sha-dirty,
    // or the hermetic "unknown") -- never empty.
    assert!(!build_git_sha().is_empty());
    assert!(line.contains(&format!("{profile} build, git ")));
    assert!(line.ends_with(&format!("git {})", build_git_sha())));
}

#[test]
fn version_line_formats_an_injected_git_sha() {
    // CR-P2-09: formatting is pure so it can be tested with an
    // injected sha instead of the real one.
    let line = version_line_with("abc1234");
    assert!(line.starts_with(&format!("{} (", env!("CARGO_PKG_VERSION"))));
    assert!(line.ends_with(", git abc1234)"));
    // The hermetic fallback (source export without git) stays readable.
    let unknown = version_line_with("unknown");
    assert!(unknown.ends_with(", git unknown)"));
    // A dirty stamp renders verbatim, suffix included.
    assert!(version_line_with("abc1234-dirty").ends_with(", git abc1234-dirty)"));
}

#[test]
fn subcommands_answer_version_with_the_same_payload() {
    use clap::CommandFactory;
    // CR-P2-09: `ak-cli registry-server --version` (and every other
    // subcommand) must answer with the same payload as the top level,
    // so the ops runbook can pin the running server binary without
    // remembering which flag position clap accepts.
    let mut cmd = Cli::command();
    cmd.build();
    // G5 / E1: `signal-server` exists only in legacy-feature builds, so
    // it joins the sweep when compiled and is skipped (not asserted
    // absent) in the default build.
    #[cfg(feature = "legacy-plaintext-signal")]
    let legacy = ["signal-server"];
    #[cfg(not(feature = "legacy-plaintext-signal"))]
    let legacy = [];
    for name in ["registry-server", "stun-server", "task"]
        .into_iter()
        .chain(legacy)
    {
        let sub = cmd
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("{name} subcommand must exist"));
        let rendered = sub.render_long_version();
        assert!(
            rendered.contains(&version_line()),
            "{name} --version must carry the full payload, got: {rendered}"
        );
        assert!(
            rendered.contains(name),
            "{name} --version must identify its own subcommand: {rendered}"
        );
    }
}

#[test]
fn long_version_flag_renders_the_enhanced_line() {
    use clap::CommandFactory;
    // `--version` must print exactly "ak-cli " + the payload, so two
    // machines can paste outputs and diff them.
    let rendered = Cli::command().render_long_version();
    assert_eq!(rendered.trim(), format!("ak-cli {}", version_line()));
    // The plain `-V` short form keeps the bare workspace version.
    let short = Cli::command().render_version();
    assert_eq!(
        short.trim(),
        format!("ak-cli {}", env!("CARGO_PKG_VERSION"))
    );
}

// -- pair export / import ------------------------------------------------------

#[test]
fn pair_subcommands_parse_export_and_import() {
    let cli = cli_of(&["pair", "export"]);
    assert!(matches!(
        cli.command,
        Command::Pair {
            cmd: PairCommand::Export
        }
    ));

    let cli = cli_of(&["pair", "import", "AKPAIR1\ndevice 00\n"]);
    let Command::Pair {
        cmd: PairCommand::Import { text },
    } = cli.command
    else {
        panic!("expected import subcommand");
    };
    assert!(text.starts_with("AKPAIR1"));

    // import needs its block argument.
    assert!(Cli::try_parse_from(["ak-cli", "pair", "import"]).is_err());
}

#[test]
fn pair_export_shape_is_public_only() {
    let dir = TempDir::new("pair-export");
    // Sentinel key with a fixed seed: the test can then prove the private
    // half never leaks into the export (AGENTS.md key red line).
    let seed: [u8; 32] = core::array::from_fn(|i| (i as u8) ^ 0xA5);
    std::fs::write(dir.path().join("device.key"), seed).unwrap();
    let (key, _) = DeviceKeyPair::load_or_create(&dir.path().join("device.key")).unwrap();
    let fp = key.fingerprint();
    let peer = random_fp();
    pair_add(dir.path(), &peer.to_hex(), Some("laptop")).unwrap();
    let paired_ms = PeerBook::load(&peer_book_path(dir.path()))
        .unwrap()
        .get(&peer)
        .unwrap()
        .paired_unix_ms;

    let text = pair_export_text(dir.path()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        vec![
            "AKPAIR1",
            &format!("device {}", hex_of(&key.public_key())),
            &format!("fingerprint {}", fp.to_hex()),
            &format!("code {}", ak_identity::pairing_code(&fp)),
            &format!("peer {} {paired_ms} laptop", peer.to_hex()),
        ]
    );
    assert!(text.ends_with('\n'));

    // Private key red line: the seed (device.key content) must not appear
    // in any hex form, lower or upper case.
    let seed_hex = hex_of(&seed);
    assert!(!text.contains(&seed_hex), "export leaked the seed hex");
    assert!(
        !text.contains(&seed_hex.to_uppercase()),
        "export leaked seed hex (upper)"
    );
    // The device line is the PUBLIC key, not the seed.
    assert!(text.contains(&hex_of(&key.public_key())));
}

#[test]
fn pair_export_import_round_trip_restores_the_book() {
    let dir = TempDir::new("pair-export-roundtrip");
    let (key, _) = DeviceKeyPair::load_or_create(&dir.path().join("device.key")).unwrap();
    let _ = key;
    let a = random_fp();
    let b = random_fp();
    let c = random_fp();
    pair_add(dir.path(), &a.to_hex(), Some("alice laptop")).unwrap();
    pair_add(dir.path(), &b.to_hex(), Some("设备 名字 空格")).unwrap();
    pair_add(dir.path(), &c.to_hex(), None).unwrap();
    let before = PeerBook::load(&peer_book_path(dir.path())).unwrap();

    let text = pair_export_text(dir.path()).unwrap();
    // Wipe the book completely, then rebuild it from the export alone.
    std::fs::remove_file(peer_book_path(dir.path())).unwrap();
    let outcome = pair_import_text(dir.path(), &text).unwrap();
    assert_eq!(
        (
            outcome.peers,
            outcome.added,
            outcome.updated,
            outcome.unchanged,
            outcome.bad
        ),
        (3, 3, 0, 0, 0)
    );

    let after = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    let mut before_list: Vec<PeerRecord> = before.iter().cloned().collect();
    let mut after_list: Vec<PeerRecord> = after.iter().cloned().collect();
    before_list.sort_by_key(|record| record.fingerprint.to_hex());
    after_list.sort_by_key(|record| record.fingerprint.to_hex());
    assert_eq!(after_list, before_list);

    // Re-importing the same block changes nothing (idempotent).
    let again = pair_import_text(dir.path(), &text).unwrap();
    assert_eq!(
        (
            again.peers,
            again.added,
            again.updated,
            again.unchanged,
            again.bad
        ),
        (3, 0, 0, 3, 0)
    );
}

#[test]
fn pair_import_merges_by_fingerprint_keeping_newer_paired_time() {
    let dir = TempDir::new("pair-merge");
    let x = random_fp();
    let y = random_fp();
    let z = random_fp();
    let mut book = PeerBook::default();
    book.upsert(PeerRecord {
        fingerprint: x,
        name: "local-name".to_owned(),
        paired_unix_ms: 100,
    });
    book.upsert(PeerRecord {
        fingerprint: z,
        name: "kept".to_owned(),
        paired_unix_ms: 300,
    });
    book.save(&peer_book_path(dir.path())).unwrap();

    let block = format!(
        "AKPAIR1\ndevice {}\nfingerprint {}\ncode 123-456\n\
         peer {} 200 remote-name\npeer {} 150 stale-dup\n\
         peer {} 50 fresh peer name\npeer {} 100 older\n",
        "11".repeat(32),
        "22".repeat(32),
        x.to_hex(),
        x.to_hex(),
        y.to_hex(),
        z.to_hex()
    );
    let outcome = pair_import_text(dir.path(), &block).unwrap();
    // x: in-block newer (200) beats the stale duplicate, then updates the
    // local entry (100); y: new; z: import older (100 < 300), unchanged.
    assert_eq!(
        (
            outcome.peers,
            outcome.added,
            outcome.updated,
            outcome.unchanged,
            outcome.bad
        ),
        (3, 1, 1, 1, 0)
    );
    assert_eq!(outcome.source_short.as_deref(), Some("11111111"));

    let merged = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(
        merged.get(&x).map(|r| (r.name.as_str(), r.paired_unix_ms)),
        Some(("remote-name", 200))
    );
    assert_eq!(
        merged.get(&y).map(|r| (r.name.as_str(), r.paired_unix_ms)),
        Some(("fresh peer name", 50))
    );
    assert_eq!(
        merged.get(&z).map(|r| (r.name.as_str(), r.paired_unix_ms)),
        Some(("kept", 300))
    );
}

#[test]
fn pair_import_counts_bad_lines_and_enforces_the_header() {
    let dir = TempDir::new("pair-import-bad");
    let good = random_fp();
    let block = format!(
        "AKPAIR1\r\npeer {} 12 ok\r\ngarbage line\r\npeer nothex 12 x\r\n\
         peer {} abc ts-not-a-number\r\ndevice nothex\r\nfingerprint short\r\n\
         code 12-34\r\npeer {}\r\npeer {} 12 tab\tname\r\n",
        good.to_hex(),
        good.to_hex(),
        good.to_hex(),
        good.to_hex()
    );
    let outcome = pair_import_text(dir.path(), &block).unwrap();
    // One valid peer line survives; the other eight are counted bad.
    assert_eq!(
        (
            outcome.peers,
            outcome.added,
            outcome.updated,
            outcome.unchanged,
            outcome.bad
        ),
        (1, 1, 0, 0, 8)
    );
    let book = PeerBook::load(&peer_book_path(dir.path())).unwrap();
    assert_eq!(
        book.get(&good).map(|r| (r.name.as_str(), r.paired_unix_ms)),
        Some(("ok", 12))
    );

    // A wrong or missing header is fatal: nothing merges at all.
    for bad in [
        "",
        "AKPEERS1\npeer 00 1 x\n",
        &format!("peer {} 1 x\n", good.to_hex()),
    ] {
        assert!(
            pair_import_text(dir.path(), bad).is_err(),
            "unexpectedly accepted block {bad:?}"
        );
    }
    // A header-only block is a valid no-op.
    let empty = pair_import_text(dir.path(), "AKPAIR1\n").unwrap();
    assert_eq!(
        (
            empty.peers,
            empty.added,
            empty.updated,
            empty.unchanged,
            empty.bad
        ),
        (0, 0, 0, 0, 0)
    );
}

// -- probe --json -----------------------------------------------------------
// G5 / E1: probe-only tests compile only with the legacy feature.
#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn probe_args_parse_json_flag() {
    let peer = "a".repeat(64);
    let cli = cli_of(&[
        "probe",
        "--server",
        "127.0.0.1:7100",
        "--peer",
        &peer,
        "--json",
        "--no-stun",
    ]);
    let Command::Probe {
        server,
        peer,
        no_stun,
        dual_stack: _,
        json,
    } = cli.command
    else {
        panic!("expected probe subcommand");
    };
    assert_eq!(server, "127.0.0.1:7100".parse::<SocketAddr>().unwrap());
    assert_eq!(peer, "a".repeat(64));
    assert!(no_stun && json);

    // Default output stays textual.
    let cli = cli_of(&["probe", "--server", "127.0.0.1:7100", "--peer", &peer]);
    let Command::Probe { json, .. } = cli.command else {
        panic!("expected probe subcommand");
    };
    assert!(!json);
}

#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn probe_json_renders_full_run_shape() {
    let peer_hex = "aa".repeat(32);
    let run = ProbeRun {
        server: "203.0.113.9:7100".to_owned(),
        peer: peer_hex.clone(),
        device_id: "bb".repeat(32),
        fingerprint: "cc".repeat(32),
        stun: ProbeStun::Ok("203.0.113.9:52000".parse().unwrap()),
        registered_candidates: 3,
        candidates: vec![
            "192.168.1.5:52000".to_owned(),
            "127.0.0.1:52000".to_owned(),
            "203.0.113.9:52000".to_owned(),
        ],
        session_open: true,
        session_peer: Some(peer_hex.clone()),
        peer_candidates: vec!["192.168.1.7:7100".to_owned()],
        same_egress: true,
        punch: Some(ProbePunch {
            packets_sent: 4,
            packets_received: 2,
            reachable: vec!["192.168.1.7:7100".parse().unwrap()],
            peer_observed_from: Some("192.168.1.7:7100".parse().unwrap()),
        }),
        role: Some("client"),
        handshake: ProbeHandshake {
            attempted: true,
            ok: true,
            path: Some("local-direct"),
            peer_fingerprint: Some(peer_hex),
            error: None,
        },
        conclusion: "connected",
    };
    let json = probe_json(&run);
    assert!(json.starts_with('{') && json.ends_with('}'));
    // Every field name is a stable script-facing contract.
    for key in [
        "server",
        "peer",
        "device_id",
        "fingerprint",
        "stun",
        "registered_candidates",
        "candidates",
        "session_open",
        "session_peer",
        "peer_candidates",
        "same_egress",
        "punch",
        "role",
        "handshake",
        "conclusion",
    ] {
        assert!(json.contains(&format!("\"{key}\":")), "missing key {key}");
    }
    assert!(json.contains("\"status\": \"ok\""));
    assert!(json.contains("\"mapped\": \"203.0.113.9:52000\""));
    assert!(json.contains("\"registered_candidates\": 3"));
    assert!(json.contains(
        "\"candidates\": [\"192.168.1.5:52000\", \"127.0.0.1:52000\", \"203.0.113.9:52000\"]"
    ));
    assert!(json.contains("\"session_open\": true"));
    assert!(json.contains(&format!("\"session_peer\": \"{}\"", "aa".repeat(32))));
    assert!(json.contains("\"same_egress\": true"));
    assert!(json.contains("\"packets_sent\": 4"));
    assert!(json.contains("\"packets_received\": 2"));
    assert!(json.contains("\"reachable\": [\"192.168.1.7:7100\"]"));
    assert!(json.contains("\"peer_observed_from\": \"192.168.1.7:7100\""));
    assert!(json.contains("\"role\": \"client\""));
    assert!(json.contains("\"attempted\": true"));
    assert!(json.contains("\"ok\": true"));
    assert!(json.contains("\"path\": \"local-direct\""));
    assert!(json.contains(&format!("\"peer_fingerprint\": \"{}\"", "aa".repeat(32))));
    assert!(json.contains("\"conclusion\": \"connected\""));
    // Exactly one top-level object: 16 lines with nothing after it.
    assert_eq!(json.matches('\n').count(), 15 + 1);
}

#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn probe_json_renders_minimal_run_with_nulls() {
    let json = probe_json(&ProbeRun::default());
    assert!(json.starts_with('{') && json.ends_with('}'));
    assert!(json.contains("\"stun\": {\"status\": \"skipped\", \"mapped\": null, \"error\": null}"));
    assert!(json.contains("\"registered_candidates\": 0"));
    assert!(json.contains("\"candidates\": []"));
    assert!(json.contains("\"session_open\": false"));
    assert!(json.contains("\"session_peer\": null"));
    assert!(json.contains("\"peer_candidates\": []"));
    assert!(json.contains("\"same_egress\": false"));
    assert!(json.contains("\"punch\": null"));
    assert!(json.contains("\"role\": null"));
    assert!(json.contains(
        "\"handshake\": {\"attempted\": false, \"ok\": false, \"path\": null, \
         \"peer_fingerprint\": null, \"error\": null}"
    ));
    assert!(json.contains("\"conclusion\": \"no-peer-candidates\""));
}

#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn probe_json_covers_remaining_stun_states_and_escapes_errors() {
    let run = ProbeRun {
        stun: ProbeStun::TimedOut,
        ..ProbeRun::default()
    };
    assert!(probe_json(&run).contains("\"status\": \"timeout\""));

    let run = ProbeRun {
        stun: ProbeStun::Failed("bind \"refused\"".to_owned()),
        ..ProbeRun::default()
    };
    let json = probe_json(&run);
    assert!(json.contains("\"status\": \"failed\""));
    assert!(json.contains("bind \\\"refused\\\""));

    // Handshake error strings ride through the same escaper.
    let run = ProbeRun {
        handshake: ProbeHandshake {
            attempted: true,
            ok: false,
            path: None,
            peer_fingerprint: None,
            error: Some("timeout after 15s\n(dial failed)".to_owned()),
        },
        ..ProbeRun::default()
    };
    let json = probe_json(&run);
    assert!(json.contains("timeout after 15s\\n(dial failed)"));
    assert!(json.contains("\"ok\": false"));
}

// -- --bind listen overrides ----------------------------------------------

#[test]
fn bind_arg_parses_bare_ips_and_full_addresses() {
    let ip = |text: &str| BindArg::Ip(text.parse::<IpAddr>().unwrap());
    let addr = |text: &str| BindArg::Addr(text.parse::<SocketAddr>().unwrap());
    assert_eq!(BindArg::parse("0.0.0.0").unwrap(), ip("0.0.0.0"));
    assert_eq!(BindArg::parse("::").unwrap(), ip("::"));
    assert_eq!(BindArg::parse("192.168.1.5").unwrap(), ip("192.168.1.5"));
    assert_eq!(BindArg::parse("fe80::1").unwrap(), ip("fe80::1"));
    assert_eq!(
        BindArg::parse("0.0.0.0:7100").unwrap(),
        addr("0.0.0.0:7100")
    );
    assert_eq!(BindArg::parse("[::]:7100").unwrap(), addr("[::]:7100"));
    assert_eq!(BindArg::parse("[::1]:9").unwrap(), addr("[::1]:9"));
    // Whitespace is trimmed like every other value.
    assert_eq!(BindArg::parse("  [::1]:9  ").unwrap(), addr("[::1]:9"));
    for bad in [
        "junk",
        "",
        "[::1",
        ":7100",
        "192.168.1.5:70000",
        "host:7100",
    ] {
        let err = BindArg::parse(bad).unwrap_err().to_string();
        assert!(err.contains("invalid bind address"), "{err} for {bad:?}");
    }
}

#[test]
fn resolve_listen_addr_combines_and_notifies_on_port_conflicts() {
    // No flags: wildcard v4 plus the verb default.
    let (addr, note) = resolve_listen_addr(None, "--bind", None, "--port", 7100);
    assert_eq!(addr, "0.0.0.0:7100".parse().unwrap());
    assert_eq!(note, None);
    // Port flag only.
    let (addr, note) = resolve_listen_addr(None, "--bind", Some(7200), "--port", 7100);
    assert_eq!(addr, "0.0.0.0:7200".parse().unwrap());
    assert_eq!(note, None);
    // Bare-IP bind combines with the port flag (or the default).
    let (addr, note) = resolve_listen_addr(
        Some(BindArg::parse("::").unwrap()),
        "--bind",
        None,
        "--port",
        7100,
    );
    assert_eq!(addr, "[::]:7100".parse().unwrap());
    assert_eq!(note, None);
    let (addr, note) = resolve_listen_addr(
        Some(BindArg::parse("192.168.1.5").unwrap()),
        "--bind",
        Some(7001),
        "--port",
        7100,
    );
    assert_eq!(addr, "192.168.1.5:7001".parse().unwrap());
    assert_eq!(note, None);
    // Full-address bind: its own port wins over a DIFFERING explicit port
    // flag, with a notice; an equal value or no flag stays silent.
    let (addr, note) = resolve_listen_addr(
        Some(BindArg::parse("[::]:7200").unwrap()),
        "--bind",
        Some(7300),
        "--port",
        7100,
    );
    assert_eq!(addr, "[::]:7200".parse().unwrap());
    let note = note.unwrap_or_default();
    assert!(note.contains("ignoring --port 7300"), "{note}");
    let (addr, note) = resolve_listen_addr(
        Some(BindArg::parse("[::]:7200").unwrap()),
        "--bind",
        Some(7200),
        "--port",
        7100,
    );
    assert_eq!(addr, "[::]:7200".parse().unwrap());
    assert_eq!(note, None);
    let (addr, note) = resolve_listen_addr(
        Some(BindArg::parse("0.0.0.0:7000").unwrap()),
        "--bind",
        None,
        "--port",
        7100,
    );
    assert_eq!(addr, "0.0.0.0:7000".parse().unwrap());
    assert_eq!(note, None);
}

#[test]
fn receive_bind_flag_parses_all_address_shapes() {
    // Defaults: port and bind both unset (resolved later in code).
    let cli = cli_of(&["receive"]);
    let Command::Receive { port, bind, .. } = cli.command else {
        panic!("expected receive subcommand");
    };
    assert_eq!(port, None);
    assert_eq!(bind, None);

    // Bare v6 bind; --port keeps combining with it.
    let cli = cli_of(&["receive", "--bind", "::"]);
    let Command::Receive { port, bind, .. } = cli.command else {
        panic!("expected receive subcommand");
    };
    assert_eq!(port, None);
    assert_eq!(bind, Some(BindArg::Ip("::".parse().unwrap())));

    // Full-address bind plus an explicit (ignored) port.
    let cli = cli_of(&["receive", "--bind", "[::]:7200", "--port", "9"]);
    let Command::Receive { port, bind, .. } = cli.command else {
        panic!("expected receive subcommand");
    };
    assert_eq!(port, Some(9));
    assert_eq!(bind, Some(BindArg::Addr("[::]:7200".parse().unwrap())));

    for bad in [
        vec!["receive", "--bind", "junk"],
        vec!["receive", "--bind", "[::1"],
        vec!["receive", "--bind", "host:7100"],
    ] {
        let mut argv = vec!["ak-cli"];
        argv.extend_from_slice(&bad);
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "unexpectedly accepted {bad:?}"
        );
    }
}

// G5 / E1: the signal-server subcommand exists only with the legacy
// feature; its parse test is gated with it. The stun-server sibling
// below stays in every build (CR-P2-13).
#[cfg(feature = "legacy-plaintext-signal")]
#[test]
fn signal_server_bind_flags_parse_and_default_ports_apply() {
    let cli = cli_of(&["signal-server"]);
    let Command::SignalServer {
        tcp_port,
        stun_port,
        bind,
        stun_bind,
    } = cli.command
    else {
        panic!("expected signal-server subcommand");
    };
    assert_eq!((tcp_port, stun_port), (None, None));
    assert!(bind.is_none() && stun_bind.is_none());
    // No flags land on the documented default ports.
    let (tcp_addr, note) = resolve_listen_addr(None, "--bind", None, "--tcp-port", 7100);
    assert_eq!(tcp_addr, "0.0.0.0:7100".parse().unwrap());
    assert_eq!(note, None);

    let cli = cli_of(&[
        "signal-server",
        "--tcp-port",
        "7200",
        "--stun-port",
        "7201",
        "--bind",
        "[::]:8000",
        "--stun-bind",
        "::",
    ]);
    let Command::SignalServer {
        tcp_port,
        stun_port,
        bind,
        stun_bind,
    } = cli.command
    else {
        panic!("expected signal-server subcommand");
    };
    // --bind carries a port that beats the differing --tcp-port (noted).
    let (tcp_addr, tcp_note) = resolve_listen_addr(bind, "--bind", tcp_port, "--tcp-port", 7100);
    assert_eq!(tcp_addr, "[::]:8000".parse().unwrap());
    let note = tcp_note.as_deref().unwrap_or_default();
    assert!(note.contains("ignoring --tcp-port 7200"), "{note}");
    // Bare v6 --stun-bind combines with --stun-port silently.
    let (stun_addr, stun_note) =
        resolve_listen_addr(stun_bind, "--stun-bind", stun_port, "--stun-port", 7101);
    assert_eq!(stun_addr, "[::]:7201".parse().unwrap());
    assert_eq!(stun_note, None);
}

// CR-P2-13: the standalone STUN responder subcommand parses like its
// siblings (--port + --bind via the shared resolver, default 7101/udp).
#[test]
fn stun_server_bind_flags_parse_and_default_port_applies() {
    let cli = cli_of(&["stun-server"]);
    let Command::StunServer { port, bind } = cli.command else {
        panic!("expected stun-server subcommand");
    };
    assert_eq!(port, None);
    assert!(bind.is_none());
    // No flags land on the documented default port.
    let (addr, note) = resolve_listen_addr(None, "--bind", None, "--port", 7101);
    assert_eq!(addr, "0.0.0.0:7101".parse().unwrap());
    assert_eq!(note, None);

    let cli = cli_of(&["stun-server", "--port", "7202", "--bind", "[::]:7101"]);
    let Command::StunServer { port, bind } = cli.command else {
        panic!("expected stun-server subcommand");
    };
    // The bind value carries a port that beats the differing --port.
    let (addr, note) = resolve_listen_addr(bind, "--bind", port, "--port", 7101);
    assert_eq!(addr, "[::]:7101".parse().unwrap());
    let note = note.as_deref().unwrap_or_default();
    assert!(note.contains("ignoring --port 7202"), "{note}");

    // A bare v6 bind combines with --port silently.
    let cli = cli_of(&["stun-server", "--bind", "::"]);
    let Command::StunServer { port, bind } = cli.command else {
        panic!("expected stun-server subcommand");
    };
    let (addr, note) = resolve_listen_addr(bind, "--bind", port, "--port", 7101);
    assert_eq!(addr, "[::]:7101".parse().unwrap());
    assert_eq!(note, None);

    for bad in ["junk", "[::1]", "host:7101"] {
        assert!(
            Cli::try_parse_from(["ak-cli", "stun-server", "--bind", bad]).is_err(),
            "unexpectedly accepted --bind {bad:?}"
        );
    }
}

// -- completions -----------------------------------------------------------

#[test]
fn completions_args_parse_every_shell_and_reject_others() {
    for (name, expected) in [
        ("bash", ShellKind::Bash),
        ("zsh", ShellKind::Zsh),
        ("fish", ShellKind::Fish),
        ("powershell", ShellKind::Powershell),
    ] {
        let cli = cli_of(&["completions", name]);
        let Command::Completions { shell } = cli.command else {
            panic!("expected completions subcommand");
        };
        assert_eq!(shell, expected);
    }
    assert!(Cli::try_parse_from(["ak-cli", "completions", "tcsh"]).is_err());
    assert!(Cli::try_parse_from(["ak-cli", "completions"]).is_err());
}

#[test]
fn completions_generation_is_nonempty_and_names_the_binary() {
    for shell in [
        ShellKind::Bash,
        ShellKind::Zsh,
        ShellKind::Fish,
        ShellKind::Powershell,
    ] {
        let script = completions_script(shell).unwrap();
        assert!(!script.is_empty(), "{shell:?} script empty");
        assert!(script.contains("ak-cli"), "{shell:?} missing binary name");
    }
    // One stable marker per shell so a mis-mapped shell cannot pass.
    assert!(completions_script(ShellKind::Bash)
        .unwrap()
        .contains("complete -F _ak__cli"));
    assert!(completions_script(ShellKind::Zsh)
        .unwrap()
        .starts_with("#compdef ak-cli"));
    assert!(completions_script(ShellKind::Fish)
        .unwrap()
        .contains("complete -c ak-cli"));
    assert!(completions_script(ShellKind::Powershell)
        .unwrap()
        .contains("Register-ArgumentCompleter -Native -CommandName 'ak-cli'"));
}

// ---- wave 21 (B7): invite family + registry-server parsing ----------

#[test]
fn invite_create_parses_ttl_flags_and_registry_pair() {
    // Defaults: no explicit ttl, not long-lived (600s is applied later).
    let cli = Cli::try_parse_from(["ak-cli", "invite", "create"]).expect("parses");
    match cli.command {
        Command::Invite {
            cmd:
                InviteCommand::Create {
                    ttl_secs,
                    long_lived,
                    name,
                    server,
                    registry_fp,
                },
        } => {
            assert_eq!(ttl_secs, None);
            assert!(!long_lived);
            assert_eq!(name, None);
            assert_eq!(server, None);
            assert_eq!(registry_fp, None);
        }
        other => panic!("expected invite create, got {other:?}"),
    }
    // Explicit ttl.
    let cli = Cli::try_parse_from([
        "ak-cli",
        "invite",
        "create",
        "--ttl-secs",
        "3600",
        "--name",
        "desk",
    ])
    .expect("parses");
    match cli.command {
        Command::Invite {
            cmd:
                InviteCommand::Create {
                    ttl_secs,
                    long_lived,
                    name,
                    ..
                },
        } => {
            assert_eq!(ttl_secs, Some(3600));
            assert!(!long_lived);
            assert_eq!(name.as_deref(), Some("desk"));
        }
        other => panic!("expected invite create, got {other:?}"),
    }
    // Long-lived conflicts with --ttl-secs; registry flags pair up.
    assert!(Cli::try_parse_from([
        "ak-cli",
        "invite",
        "create",
        "--ttl-secs",
        "60",
        "--long-lived"
    ])
    .is_err());
    assert!(
        Cli::try_parse_from(["ak-cli", "invite", "create", "--server", "127.0.0.1:7200"]).is_err()
    );
    assert!(Cli::try_parse_from([
        "ak-cli",
        "invite",
        "create",
        "--registry-fp",
        &"ab".repeat(32)
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "ak-cli",
        "invite",
        "create",
        "--long-lived",
        "--server",
        "127.0.0.1:7200",
        "--registry-fp",
        &"ab".repeat(32),
    ])
    .is_ok());
}

#[test]
fn invite_list_revoke_redeem_parse_their_positionals() {
    let cli = Cli::try_parse_from(["ak-cli", "invite", "list"]).expect("parses");
    assert!(matches!(
        cli.command,
        Command::Invite {
            cmd: InviteCommand::List { .. }
        }
    ));
    let cli = Cli::try_parse_from([
        "ak-cli",
        "invite",
        "revoke",
        &"cd".repeat(16),
        "--server",
        "10.0.0.1:7200",
        "--registry-fp",
        &"ab".repeat(32),
    ])
    .expect("parses");
    match cli.command {
        Command::Invite {
            cmd:
                InviteCommand::Revoke {
                    code_id,
                    server,
                    registry_fp,
                },
        } => {
            assert_eq!(code_id, "cd".repeat(16));
            assert_eq!(server, Some("10.0.0.1:7200".parse().unwrap()));
            assert_eq!(registry_fp.as_deref(), Some(&"ab".repeat(32)[..]));
        }
        other => panic!("expected invite revoke, got {other:?}"),
    }
    let cli = Cli::try_parse_from([
        "ak-cli",
        "invite",
        "redeem",
        "AKI1-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH",
        "--out",
        "D:/inbox",
        "--once",
        "--conflict",
        "rename",
        "--timeout",
        "7",
    ])
    .expect("parses");
    match cli.command {
        Command::Invite {
            cmd:
                InviteCommand::Redeem {
                    code,
                    out,
                    once,
                    conflict,
                    timeout,
                    ..
                },
        } => {
            assert_eq!(code, "AKI1-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH");
            assert_eq!(out, PathBuf::from("D:/inbox"));
            assert!(once);
            assert_eq!(conflict, "rename");
            assert_eq!(timeout, 7);
        }
        other => panic!("expected invite redeem, got {other:?}"),
    }
    // A redeem without a code is a usage error.
    assert!(Cli::try_parse_from(["ak-cli", "invite", "redeem", "--out", "x"]).is_err());
}

#[test]
fn registry_server_defaults_to_the_frozen_v2_port() {
    let id = PathBuf::from("id");
    let (config, note) = registry_server_config(&id, None, None, None);
    assert_eq!(
        config.listen,
        SocketAddr::from(([0, 0, 0, 0], ak_signal::server_v2::config::DEFAULT_V2_PORT))
    );
    assert_eq!(config.db_path, registry_db_path(&id));
    assert!(note.is_none(), "no note without overrides: {note:?}");

    let (config, _note) =
        registry_server_config(&id, Some(7300), None, Some(PathBuf::from("C:/reg.sqlite")));
    assert_eq!(config.listen.port(), 7300);
    assert_eq!(config.db_path, PathBuf::from("C:/reg.sqlite"));

    // A bare --bind IP keeps the port flag; a full address wins.
    let (config, _) = registry_server_config(
        &id,
        None,
        Some(BindArg::Ip("127.0.0.1".parse().unwrap())),
        None,
    );
    assert_eq!(
        config.listen,
        SocketAddr::from((
            [127, 0, 0, 1],
            ak_signal::server_v2::config::DEFAULT_V2_PORT
        ))
    );
    let (config, note) = registry_server_config(
        &id,
        Some(7300),
        Some(BindArg::Addr("[::]:7400".parse().unwrap())),
        None,
    );
    assert_eq!(config.listen, "[::]:7400".parse::<SocketAddr>().unwrap());
    assert!(
        note.is_some(),
        "full-address bind explains its port: {note:?}"
    );
}

/// 2026-11-19 review: `registry-server` binds and opens its store BEFORE
/// it prints the pin, so a port that is already taken ends the command at
/// once with the address in the error. It used to print the pin and then
/// idle without listening until Ctrl-C.
#[tokio::test]
async fn registry_server_fails_fast_when_its_port_is_taken() {
    let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = holder.local_addr().expect("held port").port();
    let tmp = tempfile::tempdir().expect("tempdir");
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        registry_server_command(
            tmp.path(),
            Some(port),
            Some(BindArg::Ip("127.0.0.1".parse().unwrap())),
            Some(tmp.path().join("registry.db")),
        ),
    )
    .await
    .expect("a taken port must end the command, not leave it idling");
    let error = outcome.expect_err("a taken port must fail the command");
    let text = format!("{error:#}");
    assert!(text.contains(&format!("127.0.0.1:{port}")), "{text}");
    drop(holder);
}

#[test]
fn expect_accepts_the_explicit_book_word() {
    let dir = tempfile::tempdir().unwrap();
    let book_path = peer_book_path(dir.path());
    let policy = ExpectPolicy::parse("book", &book_path).expect("book parses");
    assert!(matches!(policy, ExpectPolicy::Book(path) if path == book_path));
    // Case-insensitive like 'any'.
    let upper = ExpectPolicy::parse("BOOK", &book_path).expect("BOOK parses");
    assert!(matches!(upper, ExpectPolicy::Book(_)));
}
