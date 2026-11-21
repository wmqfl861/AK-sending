//! The `pair` and `jobs` subcommands.
//!
//! Moved out of `main.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub(super) fn pair_command(id: &Path, cmd: PairCommand) -> Result<()> {
    match cmd {
        PairCommand::Add { fingerprint, name } => {
            let (fp, added) = pair_add(id, &fingerprint, name.as_deref())?;
            let shown = name
                .as_deref()
                .map(str::to_owned)
                .unwrap_or_else(|| "(unnamed)".to_owned());
            if added {
                println!("paired {shown} ({})", fp.short_id());
            } else {
                println!(
                    "updated {shown} ({}) -- entry already existed",
                    fp.short_id()
                );
            }
            println!("fingerprint (full): {}", fp.to_hex());
            println!(
                "that peer's pairing code must be {} -- verify it on the peer device",
                ak_identity::pairing_code(&fp)
            );
            Ok(())
        }
        PairCommand::List => {
            let path = peer_book_path(id);
            let book = load_peer_book(&path)?;
            let count = book.iter().count();
            if count == 0 {
                println!("no paired peers yet; use `pair add <peer-fingerprint> --name NAME`");
                return Ok(());
            }
            println!("{count} paired peer(s) in {}", path.display());
            for peer in book.iter() {
                println!(
                    "{}  {}  paired {}",
                    peer_display_name(&peer.name),
                    peer.fingerprint.short_id(),
                    format_utc_ms(peer.paired_unix_ms)
                );
                println!("  fingerprint (full): {}", peer.fingerprint.to_hex());
            }
            Ok(())
        }
        PairCommand::Remove { fingerprint } => {
            let fp = pair_remove(id, &fingerprint)?;
            println!(
                "unpaired {} (removed from {})",
                fp.short_id(),
                peer_book_path(id).display()
            );
            Ok(())
        }
        PairCommand::Code => {
            let (key, _) = load_identity(id)?;
            print_identity(&key);
            Ok(())
        }
        PairCommand::Export => {
            // stdout carries ONLY the block so `ak-cli pair export > file`
            // round-trips through `pair import "$(cat file)"` untouched.
            let text = pair_export_text(id)?;
            print!("{text}");
            let _ = std::io::stdout().flush();
            Ok(())
        }
        PairCommand::Import { text } => {
            let outcome = pair_import_text(id, &text)?;
            let source = outcome
                .source_short
                .clone()
                .unwrap_or_else(|| "(unknown device)".to_owned());
            println!(
                "imported {} peer(s) from device {source} into {}: {} added, {} updated, \
                 {} unchanged",
                outcome.peers,
                peer_book_path(id).display(),
                outcome.added,
                outcome.updated,
                outcome.unchanged
            );
            if outcome.bad > 0 {
                println!(
                    "skipped {} malformed line(s); every valid line was still merged",
                    outcome.bad
                );
            }
            Ok(())
        }
    }
}

/// Per-file rows `jobs status` prints before folding the rest into a count.
const JOB_STATUS_FILE_ROWS: usize = 20;

/// Render the `jobs status <job-hex>` detail view for one checkpoint: the
/// record fields plus every file's verified prefix (first
/// [`JOB_STATUS_FILE_ROWS`] rows, then a count). Unknown or malformed job
/// ids are explicit errors.
pub(super) fn jobs_status_text(dir: &Path, job_text: &str) -> Result<String> {
    let wanted = job_text.trim().to_ascii_lowercase();
    if !hex_to_bytes(&wanted).is_ok_and(|bytes| bytes.len() == 16) {
        bail!("job id must be 32 hex characters, got {job_text:?}");
    }
    let Some((_, record)) = list_job_files(dir)
        .into_iter()
        .find(|(_, record)| record.job_hex == wanted)
    else {
        bail!(
            "no checkpoint for job {wanted} in {} (see `ak-cli jobs list`)",
            dir.display()
        );
    };
    let mut out = format!("job {}\n", record.job_hex);
    out.push_str(&format!("direction: {}\n", record.direction));
    out.push_str(&format!("state:     {}\n", record.state));
    out.push_str(&format!("root:      {}\n", record.root));
    out.push_str(&format!(
        "peer:      {}\n",
        record.peer_short.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "totals:    {} files, {} bytes ({})\n",
        record.total_files,
        record.total_bytes,
        human_bytes(record.total_bytes)
    ));
    out.push_str(&format!("chunk:     {} bytes\n", record.chunk_size));
    out.push_str(&format!(
        "created:   {}\nupdated:   {}\n",
        format_utc_ms(record.created_unix_ms),
        format_utc_ms(record.updated_unix_ms)
    ));
    out.push_str(&format!(
        "files:     {} checkpointed file(s), verified prefix in chunks\n",
        record.files.len()
    ));
    for file in record.files.iter().take(JOB_STATUS_FILE_ROWS) {
        out.push_str(&format!(
            "  file {}: {} bytes, {} verified chunk(s)\n",
            file.file_index, file.size, file.verified_prefix_chunks
        ));
    }
    let hidden = record.files.len().saturating_sub(JOB_STATUS_FILE_ROWS);
    if hidden > 0 {
        out.push_str(&format!("  ... and {hidden} more file(s) not shown\n"));
    }
    Ok(out)
}

pub(super) fn jobs_command(id: &Path, cmd: Option<JobsCommand>) -> Result<()> {
    let dir = checkpoint_dir(id);
    match cmd {
        None | Some(JobsCommand::List) => {
            let records = list_job_files(&dir);
            if records.is_empty() {
                println!("no job checkpoints in {}", dir.display());
                return Ok(());
            }
            println!("{} job checkpoint(s) in {}", records.len(), dir.display());
            for (_, record) in &records {
                println!(
                    "{} {} {} ({} files, {}) updated {}",
                    record.job_hex,
                    record.direction,
                    record.state,
                    record.total_files,
                    human_bytes(record.total_bytes),
                    format_utc_ms(record.updated_unix_ms)
                );
                println!(
                    "  root {} peer {}",
                    record.root,
                    record.peer_short.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        Some(JobsCommand::Status { job }) => {
            print!("{}", jobs_status_text(&dir, &job)?);
            Ok(())
        }
        Some(JobsCommand::Clean { all, completed }) => {
            if all && completed {
                bail!("choose one of --all or --completed, not both");
            }
            let scope = if all {
                CleanScope::All
            } else if completed {
                CleanScope::Completed
            } else {
                bail!(
                    "jobs clean needs --all or --completed (part files .akpart are never touched)"
                );
            };
            let (removed, kept) = clean_job_records(&dir, scope)?;
            println!(
                "removed {removed} checkpoint record(s), {kept} kept, in {}; \
                 .akpart part files are untouched",
                dir.display()
            );
            Ok(())
        }
    }
}
