//! Read-only view of the job checkpoints the engine writes, and their
//! cleanup.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// Job checkpoints (read-only view of the AKJOB1 files the engine writes)
// ---------------------------------------------------------------------------

/// One `file <index> <size> <verified prefix>` line of an AKJOB1 record
/// (the on-disk twin of ak-store's FileCheckpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileProgress {
    pub(super) file_index: u32,
    pub(super) size: u64,
    pub(super) verified_prefix_chunks: u64,
}

/// The display-relevant slice of one `*.akjob` checkpoint record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JobEntry {
    pub(super) job_hex: String,
    pub(super) direction: &'static str,
    pub(super) root: String,
    pub(super) peer_short: Option<String>,
    pub(super) chunk_size: u32,
    pub(super) total_files: u64,
    pub(super) total_bytes: u64,
    pub(super) state: String,
    pub(super) created_unix_ms: u64,
    pub(super) updated_unix_ms: u64,
    pub(super) files: Vec<FileProgress>,
}

fn job_field<'a>(lines: &mut impl Iterator<Item = &'a str>, key: &str) -> Option<&'a str> {
    lines.next()?.strip_prefix(key)?.strip_prefix(' ')
}

/// Parse one AKJOB1 record (the format ak-store's FileCheckpointStore
/// writes). Unparsable input yields None; callers skip such files.
pub(super) fn parse_job_record(text: &str) -> Option<JobEntry> {
    let mut lines = text.lines();
    if lines.next()? != "AKJOB1" {
        return None;
    }
    let job_hex = job_field(&mut lines, "job")?.to_owned();
    if hex_to_bytes(&job_hex).ok()?.len() != 16 {
        return None;
    }
    let direction = match job_field(&mut lines, "dir")? {
        "send" => "send",
        "receive" => "receive",
        _ => return None,
    };
    let root_hex = job_field(&mut lines, "root")?;
    let root = String::from_utf8(hex_to_bytes(root_hex).ok()?).ok()?;
    let peer = job_field(&mut lines, "peer")?;
    let peer_short = if peer == "-" {
        None
    } else {
        Fingerprint::from_hex(peer)?;
        Some(peer[..8].to_owned())
    };
    let chunk_size: u32 = job_field(&mut lines, "chunk")?.parse().ok()?;
    let total = job_field(&mut lines, "total")?;
    let mut total_parts = total.split(' ');
    let total_files: u64 = total_parts.next()?.parse().ok()?;
    let total_bytes: u64 = total_parts.next()?.parse().ok()?;
    let state = job_field(&mut lines, "state")?.to_owned();
    let created_unix_ms: u64 = job_field(&mut lines, "created")?.parse().ok()?;
    let updated_unix_ms: u64 = job_field(&mut lines, "updated")?.parse().ok()?;
    let mut files = Vec::new();
    for line in lines {
        let rest = line.strip_prefix("file ")?;
        let mut parts = rest.split(' ');
        let file_index: u32 = parts.next()?.parse().ok()?;
        let size: u64 = parts.next()?.parse().ok()?;
        let verified_prefix_chunks: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        files.push(FileProgress {
            file_index,
            size,
            verified_prefix_chunks,
        });
    }
    Some(JobEntry {
        job_hex,
        direction,
        root,
        peer_short,
        chunk_size,
        total_files,
        total_bytes,
        state,
        created_unix_ms,
        updated_unix_ms,
        files,
    })
}

/// All parsable `*.akjob` files under `dir`, newest first, with their paths.
pub(super) fn list_job_files(dir: &Path) -> Vec<(PathBuf, JobEntry)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(record) = parse_job_record(&text) {
            out.push((path, record));
        }
    }
    out.sort_by_key(|(_, record)| std::cmp::Reverse(record.updated_unix_ms));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CleanScope {
    All,
    Completed,
}

/// Delete checkpoint record files per scope. Returns (removed, kept).
/// Part files (.akpart) are never touched, so interrupted transfers stay
/// resumable even after `--all`.
pub(super) fn clean_job_records(dir: &Path, scope: CleanScope) -> Result<(usize, usize)> {
    let mut removed = 0usize;
    let mut kept = 0usize;
    match scope {
        CleanScope::All => {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok((0, 0));
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("cannot read {}", dir.display()))
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("akjob") {
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
        CleanScope::Completed => {
            for (path, record) in list_job_files(dir) {
                if record.state != "completed" {
                    kept += 1;
                    continue;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => kept += 1,
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("cannot delete {}", path.display()))
                    }
                }
            }
        }
    }
    Ok((removed, kept))
}
