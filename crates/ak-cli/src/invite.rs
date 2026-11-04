//! `ak-cli invite` (wave 21, B7): the pair-invite subcommand family over
//! the real v2 pairing plane.
//!
//! Composition: the flows that live in one process (create/register,
//! list, redeem-to-ready with the transactional `peers.book` commit) go
//! through `ak_session::pairing::PairingOrchestrator` — the shared
//! orchestration layer. One piece is CLI-local by necessity: the
//! **cross-process owner answer** (`daemon --pairing`). The orchestrator
//! keeps invite secrets in process memory (populated by `create_invite`),
//! so a daemon in a second process cannot answer redeems of a code the
//! `invite create` process created; until ak-session grows a public
//! "remember persisted code" seam, [`owner_pairing_loop`] composes the
//! same frozen transcript from `RegistryClient`'s public frame APIs plus
//! `ak-identity` proofs and this CLI's own persisted invite store
//! (`<id-dir>/invites.store`, the same bearer-secret discipline as
//! `device.key`).
#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use ak_identity::invite::InviteCode;
use ak_identity::pairing_txn::{random_nonce32, PairTranscript, PairingRecord, VerifyExpectations};
use ak_identity::peer_store::PeerBookStore;
use ak_identity::{DeviceKeyPair, Fingerprint, PeerRecord};
use ak_session::pairing::{CreatedInvite, PairingOrchestrator, RegistryClient};
use ak_signal::protocol_v2::{
    AbortReason, CodeId, CtrlMessage, StateFilter, CURSOR_END, CURSOR_START,
};
use ak_signal::server_v2::hello2;

use crate::{now_unix_ms, whoami};

/// File holding the registry endpoint pin under the identity directory:
/// `AKREG1 <server-addr> <registry-id-64hex>`.
pub(crate) const REGISTRY_PIN_FILE: &str = "registry.pin";

/// File holding this device's own invite codes under the identity
/// directory: one `AKINV1 <code-id-32hex> <expires-ms> <revision>
/// <display-code>` line per code. Bearer secrets: same protection level
/// as `device.key`, never printed by `invite list`.
pub(crate) const INVITE_STORE_FILE: &str = "invites.store";

/// How long the owner answer waits for each peer step before aborting the
/// transaction (the orchestrator uses the same order of magnitude).
const OWNER_STEP_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Atomic small-file replace (temp file + rename; on Windows the
/// destination is removed first so the rename cannot fail with
/// `AlreadyExists`). Readers see either the old or the new whole file.
fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, bytes)?;
    if path.exists() {
        std::fs::remove_file(path)?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredInvite {
    pub(crate) code_id: [u8; 16],
    /// 0 = long-lived.
    pub(crate) expires_ms: u64,
    pub(crate) revision: u64,
    /// Canonical `AKI1-…` display form (the bearer credential).
    pub(crate) display: String,
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

// ---------------------------------------------------------------------------
// Cross-process owner answer (`daemon --pairing`)
// ---------------------------------------------------------------------------

/// Wire proof → identity proof.
fn wire_proof(proof: ak_signal::protocol_v2::PairProof) -> ak_identity::pairing_txn::PairProofData {
    ak_identity::pairing_txn::PairProofData {
        mac: proof.mac,
        signature: proof.signature,
    }
}

/// Wait for the next pairing-plane frame of `pairing_id` accepted by
/// `want` within [`OWNER_STEP_TIMEOUT`]. Frames of other transactions and
/// unrelated pushes are skipped; `PairCommit` carries no pairing id, so
/// its correlation happens inside `want` (the transcript match).
async fn wait_frame<T>(
    plane: &mut tokio::sync::mpsc::Receiver<CtrlMessage>,
    pairing_id: &[u8; 16],
    mut want: impl FnMut(CtrlMessage) -> Option<T>,
) -> Result<T> {
    let deadline = tokio::time::Instant::now() + OWNER_STEP_TIMEOUT;
    loop {
        let frame = tokio::time::timeout_at(deadline, plane.recv())
            .await
            .map_err(|_| anyhow::anyhow!("pairing step timed out"))?
            .ok_or_else(|| anyhow::anyhow!("registry pairing plane closed"))?;
        let ours = match &frame {
            CtrlMessage::PairProof { pairing_id: id, .. } => id == pairing_id,
            CtrlMessage::PairCommit { .. } => true, // matched by transcript in `want`
            CtrlMessage::PairReady { pairing_id: id, .. } => id == pairing_id,
            _ => false,
        };
        if !ours {
            continue;
        }
        if let Some(matched) = want(frame) {
            return Ok(matched);
        }
    }
}

/// Answer one redeem offer as the code owner, driving the frozen
/// transcript to `PairReady` and committing the redeemer into
/// `peers.book`. Returns the redeemer's fingerprint.
///
/// The record is built with a throwaway key in the redeemer half
/// (`ak-identity`'s only public constructor signs both halves; the real
/// redeemer proof arrives over the wire and the whole record is verified
/// with `PairingRecord::verify_transcript` before anything is committed).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn answer_offer_as_owner(
    client: &RegistryClient,
    key: &DeviceKeyPair,
    code: &InviteCode,
    book_path: &Path,
    code_id: CodeId,
    code_revision: u64,
    pairing_id: [u8; 16],
    redeemer_device_id: [u8; 32],
    redeem: ak_signal::protocol_v2::RedeemRequest,
) -> Result<Fingerprint> {
    let mut plane = client.subscribe_plane();
    let transcript = PairTranscript {
        registry_id: client.registry_id(),
        code_id,
        code_revision,
        pairing_id,
        request_id: redeem.request_id,
        lookup_digest: code.lookup_digest(),
        owner_device_id: key.public_key(),
        redeemer_device_id,
        owner_nonce: random_nonce32(),
        redeemer_nonce: redeem.redeemer_nonce,
        deadline_ms: redeem.deadline_ms,
    };
    let auth_key = code.auth_key();
    let throwaway = DeviceKeyPair::generate();
    let mut record = PairingRecord::build(transcript, &auth_key, key, &throwaway);
    let owner_proof = record.owner_proof;
    client
        .send_pair_challenge(
            transcript.to_bytes(),
            ak_signal::protocol_v2::PairProof {
                mac: owner_proof.mac,
                signature: owner_proof.signature,
            },
        )
        .await
        .context("sending the pairing challenge failed")?;
    let digest = transcript.transcript_digest();

    // Redeemer proof.
    record.redeemer_proof = wire_proof(
        wait_frame(&mut plane, &pairing_id, |frame| match frame {
            CtrlMessage::PairProof {
                pairing_id: id,
                transcript_digest,
                redeemer_proof,
            } if id == pairing_id && transcript_digest == digest => Some(redeemer_proof),
            _ => None,
        })
        .await
        .context("the redeemer's PairProof never arrived")?,
    );

    // Verify the whole record under the invite auth key before anything
    // is trusted (both MACs, both signatures, both nonces, the deadline).
    let verified = record
        .verify_transcript(&VerifyExpectations {
            auth_key: &auth_key,
            expected_owner_nonce: &transcript.owner_nonce,
            expected_redeemer_nonce: &transcript.redeemer_nonce,
            now_unix_ms: now_unix_ms(),
        })
        .context("the redeemer's pairing proof failed verification")?;

    // Prepared (the frozen server commits once BOTH sides sent theirs).
    let prepared_signature = key.sign(&hello2::prepared_message(&pairing_id, &digest));
    client
        .send_pair_prepared(pairing_id, digest, prepared_signature)
        .await
        .context("sending PairPrepared failed")?;

    // Commit: the authoritative record must be byte-identical to ours.
    let commit = wait_frame(&mut plane, &pairing_id, |frame| match frame {
        CtrlMessage::PairCommit {
            transcript,
            commit_revision,
            ..
        } if transcript == verified.record().transcript.to_bytes() => Some(commit_revision),
        _ => None,
    })
    .await
    .context("PairCommit never matched the verified transcript")?;

    // Local trust-book apply, then the server-side confirmation.
    let peer_fingerprint = Fingerprint::from_public_key(&redeemer_device_id);
    PeerBookStore::new()
        .commit_paired_peer(
            book_path,
            PeerRecord {
                fingerprint: peer_fingerprint,
                name: String::new(),
                paired_unix_ms: now_unix_ms(),
            },
            &pairing_id,
        )
        .context("trust book commit failed")?;
    let applied_signature = key.sign(&hello2::applied_message(&pairing_id, &digest, commit));
    client
        .send_pair_applied(pairing_id, digest, commit, applied_signature)
        .await
        .context("sending PairApplied failed")?;
    wait_frame(&mut plane, &pairing_id, |frame| match frame {
        CtrlMessage::PairReady { pairing_id: id, .. } if id == pairing_id => Some(()),
        _ => None,
    })
    .await
    .context("PairReady never arrived")?;
    Ok(peer_fingerprint)
}

/// The resident owner loop of `daemon --pairing`: consumes the pairing
/// plane and answers every redeem offer for a live code in the local
/// store. Unknown codes are ignored (the server holds the redeem pending
/// until its deadline — a code this daemon does not hold is not ours to
/// answer).
///
/// The registry keeps ONE session per device: any other process of this
/// identity connecting (e.g. a later `ak-cli invite create`) replaces this
/// connection. The plane channel itself survives that (its sender lives in
/// the shared dispatch state), so the loss is observed on the
/// connection-state watcher; the loop then reconnects with a bounded
/// backoff — the server re-pushes pending redeem offers on the new Hello,
/// and the per-pairing dedup set (kept across reconnects) skips
/// already-handled transactions.
pub(crate) async fn owner_pairing_loop(
    client: RegistryClient,
    key: DeviceKeyPair,
    id_dir: PathBuf,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    on_paired: std::sync::Arc<dyn Fn(Fingerprint) + Send + Sync>,
) {
    use ak_session::pairing::ClientState;

    /// One step of the owner loop's inner select.
    #[allow(clippy::large_enum_variant)]
    enum Step {
        Frame(CtrlMessage),
        Dropped,
        Disconnected,
        Stop,
    }

    let book_path = crate::peer_book_path(&id_dir);
    let mut seen: HashSet<[u8; 16]> = HashSet::new();
    'session: loop {
        let mut plane = client.subscribe_plane();
        let mut state = client.state_watcher();
        loop {
            let step = tokio::select! {
                _ = shutdown.changed() => Step::Stop,
                frame = plane.recv() => match frame {
                    Some(frame) => Step::Frame(frame),
                    None => Step::Dropped,
                },
                _ = state.changed() => {
                    if matches!(*state.borrow_and_update(), ClientState::Disconnected) {
                        Step::Disconnected
                    } else {
                        continue
                    }
                }
            };
            let frame = match step {
                Step::Stop => return,
                // The plane sender is never dropped on a connection loss
                // (see the loop docs), so both loss paths reconnect.
                Step::Dropped | Step::Disconnected => break,
                Step::Frame(frame) => frame,
            };
            if let CtrlMessage::InviteRedeemOffered {
                code_id,
                code_revision,
                pairing_id,
                redeemer_device_id,
                redeem,
            } = frame
            {
                if !seen.insert(pairing_id) {
                    continue; // Hello catch-up re-push of a handled flow.
                }
                let Ok(Some(held)) = InviteStore::open(&id_dir).find(&code_id) else {
                    log::warn!(
                        "redeem offer for unknown code {} — not answering",
                        hex_16(&code_id)
                    );
                    continue;
                };
                if !held.live(now_unix_ms()) {
                    log::warn!(
                        "redeem offer for expired code {} — not answering",
                        hex_16(&code_id)
                    );
                    continue;
                }
                let Ok(code) = held.code() else {
                    continue;
                };
                let client = client.clone();
                let key = key.clone();
                let book_path = book_path.clone();
                let on_paired = std::sync::Arc::clone(&on_paired);
                tokio::spawn(async move {
                    match answer_offer_as_owner(
                        &client,
                        &key,
                        &code,
                        &book_path,
                        code_id,
                        code_revision,
                        pairing_id,
                        redeemer_device_id,
                        redeem,
                    )
                    .await
                    {
                        Ok(peer) => on_paired(peer),
                        Err(err) => {
                            log::warn!("owner pairing flow failed: {err}");
                            let _ = client.pair_abort(pairing_id, AbortReason::Protocol).await;
                        }
                    }
                });
            }
        }
        // The connection is gone (replacement or network). Reconnect with
        // a bounded backoff until shutdown fires.
        let mut backoff = Duration::from_millis(200);
        loop {
            if *shutdown.borrow() {
                return;
            }
            match client.reconnect().await {
                Ok(()) => {
                    log::info!("pairing service reconnected");
                    continue 'session;
                }
                Err(err) => {
                    log::warn!("pairing service reconnect failed: {err}");
                }
            }
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = tokio::time::sleep(backoff) => {},
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
