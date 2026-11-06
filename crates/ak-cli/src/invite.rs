//! `ak-cli invite` (wave 21, B7): the pair-invite subcommand family over
//! the real v2 pairing plane.
//!
//! Composition: the flows that live in one process (create/register,
//! list, redeem-to-ready with the transactional `peers.book` commit) go
//! through `ak_session::pairing::PairingOrchestrator` — the shared
//! orchestration layer. One piece is CLI-local by necessity: the
//! **cross-process owner answer** (`daemon --pairing`, wave 27): the
//! daemon runs the ORCHESTRATOR's owner loop (`PairingOrchestrator::
//! spawn_owner_loop_notifying`) and adopts every live code from this
//! CLI's persisted invite store (`<id-dir>/invites.store`, the same
//! bearer-secret discipline as `device.key`) through the public
//! `adopt_persisted_code` seam — the answer drives the frozen transcript
//! AND writes the pairing journal the wave-27 admission validator reads.
//! The wave-21 hand-rolled loop this replaced lived here; git history
//! keeps it.
#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};

use ak_identity::invite::InviteCode;
use ak_identity::DeviceKeyPair;
use ak_session::pairing::{CreatedInvite, PairingOrchestrator, RegistryClient};
use ak_signal::protocol_v2::{StateFilter, CURSOR_END, CURSOR_START};

use crate::whoami;

/// File holding the registry endpoint pin under the identity directory:
/// `AKREG1 <server-addr> <registry-id-64hex>`.
pub(crate) const REGISTRY_PIN_FILE: &str = "registry.pin";

/// File holding this device's own invite codes under the identity
/// directory: one `AKINV1 <code-id-32hex> <expires-ms> <revision>
/// <display-code>` line per code. Bearer secrets: same protection level
/// as `device.key`, never printed by `invite list`.
pub(crate) const INVITE_STORE_FILE: &str = "invites.store";

/// The registry endpoint a device talks to (server address + the registry
/// identity pin its TLS certificate must match).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegistrySettings {
    pub(crate) server: SocketAddr,
    pub(crate) registry_id: [u8; 32],
}

impl RegistrySettings {
    /// Parse the one-line `AKREG1 <addr> <64hex>` pin file.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split_whitespace();
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("AKREG1"), Some(addr), Some(hex), None) => {
                let server: SocketAddr = addr
                    .parse()
                    .with_context(|| format!("bad registry address in pin file: {addr}"))?;
                let registry_id = hex_to_32(hex)
                    .with_context(|| format!("bad registry id in pin file: {hex:?}"))?;
                Ok(RegistrySettings {
                    server,
                    registry_id,
                })
            }
            _ => bail!("malformed registry pin file (expected AKREG1 <addr> <64hex>)"),
        }
    }

    fn render(&self) -> String {
        format!(
            "AKREG1 {} {}\n",
            self.server,
            self.registry_id
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
}

fn hex_to_32(text: &str) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected 64 hex characters");
    }
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(out)
}

/// Atomic small-file replace, following the `ak-identity` `atomic_write`
/// discipline (review 2026-11-05, P2 — this file carries bearer secrets):
/// an EXCLUSIVELY created (`create_new`) temp file with a unique name
/// (pid + sequence — concurrent writers never share a temp), fsync, then
/// rename over the destination. Exclusive creation means a file or
/// symlink planted at a predictable temp name is never written through;
/// on Unix the secret temp is created 0o600. Readers see either the old
/// or the new whole file.
fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    /// Distinguishes concurrent temp files within one process.
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_replace needs a file path",
        )
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = file_name.to_os_string();
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), seq));
    let temp = parent.join(temp_name);

    let write = || -> io::Result<()> {
        #[cfg(unix)]
        let mut options = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options.mode(0o600);
            options
        };
        #[cfg(not(unix))]
        let mut options = std::fs::OpenOptions::new();
        let mut file = options.write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(err) = write() {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    // std's rename replaces an existing destination on POSIX and (current
    // toolchains) on Windows; the remove-then-rename fallback only covers
    // std versions without the Windows replace behavior. The gap can at
    // worst leave the destination absent — never torn (same trade-off as
    // the ak-identity / ak-store writers).
    if std::fs::rename(&temp, path).is_ok() {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    }
    std::fs::rename(&temp, path)
}

fn hex_to_16(text: &str) -> Result<[u8; 16]> {
    let mut out = [0u8; 16];
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected 32 hex characters");
    }
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(out)
}

fn hex_16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolve the registry endpoint: explicit flags win and are persisted for
/// later invocations; otherwise the pin file must already exist (there is
/// no un-pinned default — silently trusting a server would defeat the
/// registry pin model).
pub(crate) fn resolve_registry(
    id_dir: &Path,
    server: Option<SocketAddr>,
    registry_fp: Option<String>,
) -> Result<RegistrySettings> {
    let pin_path = id_dir.join(REGISTRY_PIN_FILE);
    let from_flags = match (server, registry_fp) {
        (Some(server), Some(fp)) => Some(RegistrySettings {
            server,
            registry_id: hex_to_32(fp.trim())
                .context("--registry-fp must be the registry's 64-hex id (print it with `ak-cli registry-server`)")?,
        }),
        (Some(_), None) | (None, Some(_)) => {
            bail!("--server and --registry-fp must be given together (or neither, to reuse the saved pin)")
        }
        (None, None) => None,
    };
    match from_flags {
        Some(settings) => {
            atomic_replace(&pin_path, settings.render().as_bytes())
                .context("cannot persist the registry pin")?;
            Ok(settings)
        }
        None => {
            let text = std::fs::read_to_string(&pin_path)
                .with_context(|| format!("no registry pin at {}", pin_path.display()))?;
            RegistrySettings::parse(&text)
        }
    }
}

/// Connect the shared v2 control client (one per command invocation).
pub(crate) async fn connect_registry(
    settings: &RegistrySettings,
    key: &DeviceKeyPair,
) -> Result<RegistryClient> {
    let client = RegistryClient::connect(settings.server, settings.registry_id, key, &whoami())
        .await
        .context("cannot connect to the pairing registry")?;
    Ok(client)
}

// ---------------------------------------------------------------------------
// Local invite store (owner-side bearer secrets)
// ---------------------------------------------------------------------------

/// One stored invite code line.
///
/// `Debug` is hand-written (review 2026-11-05, P2): `display` is the
/// bearer credential and must never reach logs — only the masked hint
/// (best-effort; `<unparseable>` on a corrupted line) and the non-secret
/// metadata are printed.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StoredInvite {
    pub(crate) code_id: [u8; 16],
    /// 0 = long-lived.
    pub(crate) expires_ms: u64,
    pub(crate) revision: u64,
    /// Canonical `AKI1-…` display form (the bearer credential).
    pub(crate) display: String,
}

impl std::fmt::Debug for StoredInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hint = self
            .code()
            .map(|code| code.code_hint())
            .unwrap_or_else(|_| "<unparseable>".to_owned());
        f.debug_struct("StoredInvite")
            .field("code_id", &hex_16(&self.code_id))
            .field("expires_ms", &self.expires_ms)
            .field("revision", &self.revision)
            .field("display", &"<redacted: bearer secret>")
            .field("hint", &hint)
            .finish()
    }
}

impl StoredInvite {
    /// The parsed typed code (errors on a corrupted line).
    pub(crate) fn code(&self) -> Result<InviteCode> {
        InviteCode::parse(&self.display).context("stored invite code is malformed")
    }

    fn render(&self) -> String {
        format!(
            "AKINV1 {} {} {} {}\n",
            hex_16(&self.code_id),
            self.expires_ms,
            self.revision,
            self.display
        )
    }

    /// Parse one `AKINV1 …` line.
    pub(crate) fn parse(line: &str) -> Result<Self> {
        let mut parts = line.trim().splitn(5, ' ');
        match (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            (Some("AKINV1"), Some(id), Some(expires), Some(revision), Some(display)) => {
                Ok(StoredInvite {
                    code_id: hex_to_16(id).context("bad code id in invite store")?,
                    expires_ms: expires.parse().context("bad expiry in invite store")?,
                    revision: revision.parse().context("bad revision in invite store")?,
                    display: display.trim().to_owned(),
                })
            }
            _ => bail!("malformed invite store line"),
        }
    }

    /// Whether the code is still worth answering redeems for locally.
    pub(crate) fn live(&self, now_ms: u64) -> bool {
        self.expires_ms == 0 || self.expires_ms > now_ms
    }
}

/// The `<id-dir>/invites.store` file: append, remove, and read.
pub(crate) struct InviteStore {
    path: PathBuf,
}

impl InviteStore {
    pub(crate) fn open(id_dir: &Path) -> Self {
        InviteStore {
            path: id_dir.join(INVITE_STORE_FILE),
        }
    }

    pub(crate) fn load(&self) -> Result<Vec<StoredInvite>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).context("cannot read the invite store"),
        };
        let mut out = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            out.push(
                StoredInvite::parse(line)
                    .with_context(|| format!("invite store line {}", index + 1))?,
            );
        }
        Ok(out)
    }

    /// Persist the full set (atomic replace, like peers.book).
    fn save_all(&self, invites: &[StoredInvite]) -> Result<()> {
        let mut text = String::new();
        for invite in invites {
            text.push_str(&invite.render());
        }
        atomic_replace(&self.path, text.as_bytes()).context("cannot write the invite store")
    }

    pub(crate) fn add(&self, invite: &StoredInvite) -> Result<()> {
        let mut all = self.load()?;
        all.retain(|held| held.code_id != invite.code_id);
        all.push(invite.clone());
        self.save_all(&all)
    }

    pub(crate) fn remove(&self, code_id: &[u8; 16]) -> Result<bool> {
        let mut all = self.load()?;
        let before = all.len();
        all.retain(|held| &held.code_id != code_id);
        if all.len() == before {
            return Ok(false);
        }
        self.save_all(&all)?;
        Ok(true)
    }

    pub(crate) fn find(&self, code_id: &[u8; 16]) -> Result<Option<StoredInvite>> {
        Ok(self
            .load()?
            .into_iter()
            .find(|held| &held.code_id == code_id))
    }
}

// ---------------------------------------------------------------------------
// Command cores (testable; the clap layer only prints)
// ---------------------------------------------------------------------------

/// TTL choice of `invite create`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TtlChoice {
    /// `--ttl-secs N` (server bounds: 60..=2_592_000, checked here for a
    /// clear local error).
    Secs(u32),
    /// `--long-lived`.
    LongLived,
}

/// Server-side TTL bounds (storage-schema part 1 / server_v2 constants).
pub(crate) const INVITE_TTL_MIN_SECS: u32 = 60;
pub(crate) const INVITE_TTL_MAX_SECS: u32 = 2_592_000;

impl TtlChoice {
    pub(crate) fn ttl_secs(self) -> Result<u32> {
        match self {
            TtlChoice::LongLived => Ok(0),
            TtlChoice::Secs(secs) => {
                if !(INVITE_TTL_MIN_SECS..=INVITE_TTL_MAX_SECS).contains(&secs) {
                    bail!(
                        "--ttl-secs must be {INVITE_TTL_MIN_SECS}..={INVITE_TTL_MAX_SECS} \
                         (or use --long-lived)"
                    );
                }
                Ok(secs)
            }
        }
    }
}

/// One `invite list` row (server metadata + the local hint when known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InviteRow {
    pub(crate) code_id: [u8; 16],
    pub(crate) state: ak_signal::protocol_v2::InviteState,
    pub(crate) created_ms: u64,
    pub(crate) expires_ms: u64,
    pub(crate) revision: u64,
    /// Masked `AKI1-****-…` hint from the local store, when held.
    pub(crate) hint: Option<String>,
}

/// `invite create`: generate + register + persist locally.
pub(crate) async fn invite_create(
    id_dir: &Path,
    ttl: TtlChoice,
    settings: &RegistrySettings,
) -> Result<StoredInvite> {
    let (key, _) = crate::load_identity(id_dir)?;
    let client = connect_registry(settings, &key).await?;
    let orchestrator = PairingOrchestrator::with_defaults(client, key, id_dir)
        .context("cannot open the pairing journal")?;
    let created: CreatedInvite = orchestrator
        .create_invite(ttl.ttl_secs()?)
        .await
        .context("invite registration failed")?;
    let stored = StoredInvite {
        code_id: created.code_id,
        expires_ms: created.expires_ms,
        revision: created.revision,
        display: created.display.clone(),
    };
    InviteStore::open(id_dir).add(&stored)?;
    Ok(stored)
}

/// `invite list`: page the server metadata (never the plaintext secret).
pub(crate) async fn invite_list_rows(
    id_dir: &Path,
    settings: &RegistrySettings,
) -> Result<Vec<InviteRow>> {
    let (key, _) = crate::load_identity(id_dir)?;
    let client = connect_registry(settings, &key).await?;
    let store = InviteStore::open(id_dir);
    let mut rows = Vec::new();
    let mut cursor = CURSOR_START;
    loop {
        let (next, entries) = client
            .invite_list(cursor, 32, StateFilter::ALL)
            .await
            .context("invite list failed")?;
        for entry in entries {
            let hint = store
                .find(&entry.code_id)
                .ok()
                .flatten()
                .and_then(|held| held.code().ok().map(|code| code.code_hint()));
            rows.push(InviteRow {
                code_id: entry.code_id,
                state: entry.state,
                created_ms: entry.created_ms,
                expires_ms: entry.expires_ms,
                revision: entry.revision,
                hint,
            });
        }
        if next == CURSOR_END || cursor == next {
            break;
        }
        cursor = next;
    }
    Ok(rows)
}

/// `invite revoke <code-id-hex>`: server-side revoke (revision resolved
/// from the local store first, then from a list page), then forget the
/// local secret.
pub(crate) async fn invite_revoke_by_hex(
    id_dir: &Path,
    code_id_hex: &str,
    settings: &RegistrySettings,
) -> Result<[u8; 16]> {
    let code_id = hex_to_16(code_id_hex.trim())
        .context("code id must be 32 hex characters (see `ak-cli invite list`)")?;
    let (key, _) = crate::load_identity(id_dir)?;
    let client = connect_registry(settings, &key).await?;
    let store = InviteStore::open(id_dir);
    let revision = match store.find(&code_id)? {
        Some(held) => held.revision,
        None => {
            // Fall back to the authoritative metadata.
            let mut cursor = CURSOR_START;
            let mut found = None;
            loop {
                let (next, entries) = client
                    .invite_list(cursor, 32, StateFilter::ALL)
                    .await
                    .context("invite list failed")?;
                if let Some(entry) = entries.iter().find(|entry| entry.code_id == code_id) {
                    found = Some(entry.revision);
                    break;
                }
                if next == CURSOR_END || cursor == next {
                    break;
                }
                cursor = next;
            }
            found.with_context(|| {
                format!(
                    "code {} is not in the local store nor the registry list",
                    hex_16(&code_id)
                )
            })?
        }
    };
    let revoked = client
        .invite_revoke(code_id, revision)
        .await
        .context("invite revoke failed")?;
    store.remove(&code_id)?;
    let _ = revoked;
    Ok(code_id)
}

/// `invite redeem` / `daemon --invite-code`: redeem one typed code all the
/// way to `PairReady`, committing the peer into `peers.book`. Returns the
/// verified pairing.
pub(crate) async fn redeem_and_pair(
    id_dir: &Path,
    code: &str,
    settings: &RegistrySettings,
) -> Result<ak_identity::pairing_txn::VerifiedPairing> {
    let (key, _) = crate::load_identity(id_dir)?;
    let client = connect_registry(settings, &key).await?;
    let orchestrator = PairingOrchestrator::with_defaults(client, key, id_dir)
        .context("cannot open the pairing journal")?;
    orchestrator
        .redeem(code)
        .await
        .context("invite redeem / pairing failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::now_unix_ms;

    fn temp_id(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ak-cli-invite-tests-{tag}-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn registry_settings_roundtrip_and_rejects_junk() {
        let id = [7u8; 32];
        let settings = RegistrySettings {
            server: "127.0.0.1:7200".parse().unwrap(),
            registry_id: id,
        };
        let parsed = RegistrySettings::parse(&settings.render()).expect("roundtrip");
        assert_eq!(parsed, settings);
        assert!(RegistrySettings::parse("").is_err());
        assert!(RegistrySettings::parse("AKREG1 not-an-addr ab").is_err());
        assert!(RegistrySettings::parse("AKREG1 1.2.3.4:9 zz").is_err());
        assert!(RegistrySettings::parse("AKREG2 1.2.3.4:9 00").is_err());
    }

    #[test]
    fn resolve_registry_requires_flags_in_pairs_and_persists() {
        let dir = temp_id("resolve");
        // Neither flag and no pin file: explicit error naming the file.
        let err = resolve_registry(&dir, None, None).expect_err("no pin yet");
        assert!(err.to_string().contains("registry pin"), "{err}");
        // One flag alone is refused.
        let server: SocketAddr = "127.0.0.1:7201".parse().unwrap();
        assert!(resolve_registry(&dir, Some(server), None).is_err());
        assert!(resolve_registry(&dir, None, Some("00".repeat(32))).is_err());
        // Both flags persist and later resolve without flags.
        let settings =
            resolve_registry(&dir, Some(server), Some("ab".repeat(32))).expect("flags resolve");
        assert_eq!(settings.server, server);
        assert_eq!(settings.registry_id, hex_to_32(&"ab".repeat(32)).unwrap());
        let again = resolve_registry(&dir, None, None).expect("pin reused");
        assert_eq!(again, settings);
        // A bad fingerprint is rejected with a clear message.
        assert!(resolve_registry(&dir, Some(server), Some("nothex".to_owned())).is_err());
    }

    #[test]
    fn invite_store_roundtrip_add_find_remove() {
        let dir = temp_id("store");
        let store = InviteStore::open(&dir);
        assert!(store.load().unwrap().is_empty());
        let code = InviteCode::generate();
        let invite = StoredInvite {
            code_id: [1u8; 16],
            expires_ms: 0,
            revision: 3,
            display: code.to_display(),
        };
        store.add(&invite).expect("add");
        assert_eq!(store.find(&[1u8; 16]).unwrap().as_ref(), Some(&invite));
        // Replace on re-add (same code id, new revision).
        let bumped = StoredInvite {
            revision: 4,
            ..invite.clone()
        };
        store.add(&bumped).expect("re-add");
        assert_eq!(store.load().unwrap().len(), 1);
        assert_eq!(store.find(&[1u8; 16]).unwrap().unwrap().revision, 4);
        // The stored line parses back into the same typed code.
        assert_eq!(
            store
                .find(&[1u8; 16])
                .unwrap()
                .unwrap()
                .code()
                .unwrap()
                .secret(),
            code.secret()
        );
        assert!(store.remove(&[1u8; 16]).unwrap());
        assert!(!store.remove(&[1u8; 16]).unwrap());
        assert!(store.load().unwrap().is_empty());
        // Corrupt line fails closed.
        atomic_replace(&dir.join(INVITE_STORE_FILE), b"AKINV1 zz 0 0 AKI1-X\n").unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn ttl_choice_bounds_match_the_server_contract() {
        assert_eq!(TtlChoice::LongLived.ttl_secs().unwrap(), 0);
        assert_eq!(TtlChoice::Secs(600).ttl_secs().unwrap(), 600);
        assert_eq!(TtlChoice::Secs(INVITE_TTL_MIN_SECS).ttl_secs().unwrap(), 60);
        assert_eq!(
            TtlChoice::Secs(INVITE_TTL_MAX_SECS).ttl_secs().unwrap(),
            2_592_000
        );
        assert!(TtlChoice::Secs(59).ttl_secs().is_err());
        assert!(TtlChoice::Secs(2_592_001).ttl_secs().is_err());
    }

    #[test]
    fn stored_invite_debug_never_leaks_the_bearer_code() {
        let code = InviteCode::generate();
        let invite = StoredInvite {
            code_id: [5; 16],
            expires_ms: 0,
            revision: 2,
            display: code.to_display(),
        };
        let rendered = format!("{invite:?}");
        assert!(
            !rendered.contains(&invite.display),
            "Debug must not leak the bearer code: {rendered}"
        );
        assert!(rendered.contains("redacted"), "marks the field: {rendered}");
        assert!(
            rendered.contains(&code.code_hint()),
            "shows the masked hint: {rendered}"
        );
        // A corrupted line degrades to a placeholder instead of echoing the
        // raw bytes.
        let corrupt = StoredInvite {
            display: "not-a-code".to_owned(),
            ..invite.clone()
        };
        assert!(!format!("{corrupt:?}").contains("not-a-code"));
    }

    #[test]
    fn atomic_replace_is_exclusive_atomic_and_leaves_no_residue() {
        let dir = temp_id("atomic");
        let path = dir.join(INVITE_STORE_FILE);
        std::fs::write(&path, b"old").unwrap();
        atomic_replace(&path, b"new-content").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new-content");
        // Repeat replaces in place and never leaves temp files behind —
        // the secret never lingers under a predictable temp name.
        for i in 0..5 {
            atomic_replace(&path, format!("v{i}").as_bytes()).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"v4");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![INVITE_STORE_FILE.to_owned()], "{names:?}");
        // A path inside a missing directory fails cleanly without residue.
        assert!(atomic_replace(&dir.join("nope/f"), b"x").is_err());
    }
}
