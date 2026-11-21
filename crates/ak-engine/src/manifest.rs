//! Manifest pages, sharded ResumeRequest frames and their collectors.
//!
//! Moved out of `lib.rs` unchanged (CR-P2-08, batch U review).

use super::*;

/// Assemble manifest pages under the byte budget.
pub(super) fn assemble_pages(manifest: &Manifest, limits: &Limits) -> Vec<Vec<WireEntry>> {
    let mut pages: Vec<Vec<WireEntry>> = Vec::new();
    let mut current: Vec<WireEntry> = Vec::new();
    let mut current_bytes = 0usize;
    for entry in &manifest.entries {
        let wire = WireEntry::from(entry);
        let size = 128 + wire.rel_path.len() + 32 * wire.chunk_digests.len();
        let cap = usize::from(limits.max_entries_per_page);
        if !current.is_empty() && (current_bytes + size > PAGE_BUDGET_BYTES || current.len() >= cap)
        {
            pages.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += size;
        current.push(wire);
    }
    if !current.is_empty() || pages.is_empty() {
        pages.push(current);
    }
    pages
}

/// Shard the receiver's resume answer into ResumeRequest frames of at most
/// `max_per_frame` entries. Every frame but the last carries `more = true`;
/// an empty answer still produces exactly one empty frame with
/// `more = false`, so the exchange always terminates with a final frame the
/// sender can wait for. A `max_per_frame` of 0 is clamped to 1 (a shard
/// size, not a stall).
pub(super) fn resume_request_frames(
    job_id: JobId,
    files: &[ResumeEntry],
    max_per_frame: usize,
) -> Vec<Message> {
    let max = max_per_frame.max(1);
    let mut frames: Vec<Message> = files
        .chunks(max)
        .map(|shard| Message::ResumeRequest {
            job_id,
            files: shard.to_vec(),
            more: true,
        })
        .collect();
    if let Some(Message::ResumeRequest { more, .. }) = frames.last_mut() {
        *more = false;
    } else {
        frames.push(Message::ResumeRequest {
            job_id,
            files: Vec::new(),
            more: false,
        });
    }
    frames
}

/// Receiver-side builder of the sharded resume answer while the sinks are
/// being prepared (review 2026-11-18). Instead of answering only after
/// the whole prepare pass, a `more = true` shard leaves as soon as it
/// reaches the per-frame cap, or when the liveness interval has passed
/// since the last frame — then possibly empty, which the sender's
/// [`ResumeCollector`] accepts as "keep waiting". Whatever is left goes
/// out through [`resume_request_frames`] as the final `more = false`
/// frame once the pass is over.
pub(super) struct ResumeShards {
    pending: Vec<ResumeEntry>,
    max_per_frame: usize,
    every: Duration,
    pub(super) last_frame: Instant,
}

impl ResumeShards {
    pub(super) fn new(max_per_frame: usize, every: Duration, now: Instant) -> Self {
        ResumeShards {
            pending: Vec::new(),
            max_per_frame: max_per_frame.max(1),
            every,
            last_frame: now,
        }
    }

    pub(super) fn push(&mut self, entry: ResumeEntry) {
        self.pending.push(entry);
    }

    /// The `more = true` shard to send at `now`, if one is due: the
    /// pending entries once they fill a frame, or whatever is pending
    /// (possibly nothing) once the liveness interval has passed.
    pub(super) fn take_due(&mut self, now: Instant) -> Option<Vec<ResumeEntry>> {
        let full = self.pending.len() >= self.max_per_frame;
        let quiet_too_long = now.saturating_duration_since(self.last_frame) >= self.every;
        if full || quiet_too_long {
            self.last_frame = now;
            Some(std::mem::take(&mut self.pending))
        } else {
            None
        }
    }

    /// The entries for the final frame(s).
    pub(super) fn into_rest(self) -> Vec<ResumeEntry> {
        self.pending
    }
}

/// Sender-side accumulator for the sharded ResumeRequest exchange: folds in
/// frames until one arrives with `more = false`. Cross-frame validation
/// (review round 17): every frame must name the expected job; the total
/// entry count is budgeted by the negotiated manifest (one entry per
/// transferable file, so an endless `more = true` stream cannot grow memory
/// unboundedly); file indexes must be in range, must not name directories,
/// must not repeat; and each verified prefix may not exceed the file's
/// actual chunk count. Violations abort with a protocol error.
pub(super) struct ResumeCollector {
    job_id: JobId,
    /// Per manifest entry: `None` for directories (never resumable), `Some`
    /// chunk count for files.
    chunk_counts: Vec<Option<u64>>,
    /// Number of transferable (non-directory) files: the total entry budget.
    capacity: usize,
    pub(super) entries: Vec<ResumeEntry>,
    seen: HashSet<u32>,
    max_per_frame: usize,
}

impl ResumeCollector {
    pub(super) fn new(job_id: JobId, manifest: &Manifest, limits: &Limits) -> Self {
        let chunk_counts: Vec<Option<u64>> = manifest
            .entries
            .iter()
            .map(|entry| {
                if entry.is_dir {
                    None
                } else {
                    Some(entry.chunk_digests.len() as u64)
                }
            })
            .collect();
        let capacity = chunk_counts.iter().filter(|slot| slot.is_some()).count();
        ResumeCollector {
            job_id,
            chunk_counts,
            capacity,
            entries: Vec::new(),
            seen: HashSet::new(),
            max_per_frame: usize::from(limits.max_resume_entries),
        }
    }

    /// Fold one frame's entries in; returns `true` when the exchange is
    /// complete (the frame carried `more = false`).
    pub(super) fn push(
        &mut self,
        frame_job_id: JobId,
        files: Vec<ResumeEntry>,
        more: bool,
    ) -> Result<bool, EngineError> {
        if frame_job_id != self.job_id {
            return Err(protocol_error("resume frame names a different job"));
        }
        if files.len() > self.max_per_frame {
            return Err(protocol_error(format!(
                "resume frame carries {} entries, above the per-frame budget {}",
                files.len(),
                self.max_per_frame
            )));
        }
        for entry in files {
            let Some(chunks) = self
                .chunk_counts
                .get(entry.file_index as usize)
                .copied()
                .flatten()
            else {
                return Err(protocol_error(format!(
                    "resume entry names file index {}, which is not a file of this manifest",
                    entry.file_index
                )));
            };
            if !self.seen.insert(entry.file_index) {
                return Err(protocol_error(format!(
                    "resume entry repeats file index {}",
                    entry.file_index
                )));
            }
            if entry.verified_prefix_chunks > chunks {
                return Err(protocol_error(format!(
                    "resume prefix {} exceeds the {} chunks of file index {}",
                    entry.verified_prefix_chunks, chunks, entry.file_index
                )));
            }
            self.entries.push(entry);
        }
        if self.entries.len() > self.capacity {
            return Err(protocol_error(
                "resume answer carries more entries than the manifest has files",
            ));
        }
        Ok(!more)
    }
}

/// Receiver-side accumulator for ManifestPage frames with full cross-frame
/// validation (review round 17): every page must name the offered job;
/// `page_index` must be strictly 0-based and consecutive (no gaps, no
/// repeats); the running entry count may never exceed the offered
/// `file_count` (too many is refused immediately); the `last_page` frame
/// must close exactly at `file_count` entries whose sizes sum to the
/// offered `total_bytes`. This bounds the accumulated entry memory by the
/// negotiated offer instead of trusting an endless page stream.
pub(super) struct ManifestAssembler {
    job_id: JobId,
    expected_files: u64,
    expected_bytes: u64,
    expected_page: u32,
    max_per_page: usize,
    pub(super) entries: Vec<ak_core::manifest::FileEntry>,
    bytes: u64,
}

impl ManifestAssembler {
    pub(super) fn new(job_id: JobId, file_count: u64, total_bytes: u64, limits: &Limits) -> Self {
        ManifestAssembler {
            job_id,
            expected_files: file_count,
            expected_bytes: total_bytes,
            expected_page: 0,
            max_per_page: usize::from(limits.max_entries_per_page),
            entries: Vec::new(),
            bytes: 0,
        }
    }

    /// Fold one page in; returns `Ok(true)` when the `last_page` frame
    /// closed a complete, offer-consistent manifest.
    pub(super) fn push(
        &mut self,
        job_id: JobId,
        page_index: u32,
        last_page: bool,
        page: Vec<WireEntry>,
    ) -> Result<bool, EngineError> {
        if job_id != self.job_id {
            return Err(protocol_error("manifest page names a different job"));
        }
        if page_index != self.expected_page {
            return Err(protocol_error(format!(
                "manifest page_index {} violates the strict 0-based page order (expected {})",
                page_index, self.expected_page
            )));
        }
        if page.len() > self.max_per_page {
            return Err(protocol_error(format!(
                "manifest page carries {} entries, above the per-page budget {}",
                page.len(),
                self.max_per_page
            )));
        }
        if self.entries.len() as u64 + page.len() as u64 > self.expected_files {
            return Err(protocol_error(
                "manifest carries more entries than the offer announced",
            ));
        }
        for wire in page {
            let entry: ak_core::manifest::FileEntry = wire
                .try_into()
                .map_err(|_| protocol_error("peer sent an invalid manifest entry"))?;
            self.bytes = self.bytes.saturating_add(entry.size);
            self.entries.push(entry);
        }
        self.expected_page += 1;
        if last_page {
            if self.entries.len() as u64 != self.expected_files {
                return Err(protocol_error(
                    "manifest ended with fewer entries than the offer announced",
                ));
            }
            if self.bytes != self.expected_bytes {
                return Err(protocol_error(
                    "manifest entry sizes disagree with the offered total bytes",
                ));
            }
            return Ok(true);
        }
        Ok(false)
    }
}
