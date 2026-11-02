//! Task checkpoint persistence.
//!
//! v1 is a line-based file store with atomic writes (write temp, fsync,
//! rename). The trait matches what a SQLite backend (BACKLOG P1-03) will
//! implement later; the engine only sees [`CheckpointStore`].
//!
//! Rule from PROTOCOL: a completed marker is written strictly after final
//! verification and save commit, never before.
#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

use ak_identity::Fingerprint;

pub type JobIdBytes = [u8; 16];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

/// Per-file resume state: verified contiguous prefix, in chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileCheckpoint {
    pub file_index: u32,
    pub size: u64,
    pub verified_prefix_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub job_id: JobIdBytes,
    pub direction: Direction,
    /// Source root (send) or destination root (receive).
    pub root: PathBuf,
    pub peer_fingerprint: Option<Fingerprint>,
    pub chunk_size: u32,
    pub total_files: u64,
    pub total_bytes: u64,
    pub state: String,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub files: Vec<FileCheckpoint>,
}

impl TaskRecord {
    /// Sum of verified chunks across files (progress + resume accounting).
    pub fn verified_chunks(&self) -> u64 {
        self.files.iter().map(|f| f.verified_prefix_chunks).sum()
    }
}

pub trait CheckpointStore: Send + Sync {
    /// Persist atomically; readers never observe a torn record.
    fn save(&self, record: &TaskRecord) -> io::Result<()>;
    fn load(&self, job_id: &JobIdBytes) -> io::Result<Option<TaskRecord>>;
    fn list(&self) -> io::Result<Vec<TaskRecord>>;
    /// Remove after successful completion and commit; returns false if absent.
    fn remove(&self, job_id: &JobIdBytes) -> io::Result<bool>;
}

/// File-per-job store under a directory, files named `<job hex>.akjob`.
pub struct FileCheckpointStore {
    dir: PathBuf,
}

impl FileCheckpointStore {
    pub fn open(dir: &Path) -> io::Result<Self> {
        todo!("owned by ak-store agent")
    }
}

impl CheckpointStore for FileCheckpointStore {
    fn save(&self, record: &TaskRecord) -> io::Result<()> {
        todo!("owned by ak-store agent")
    }

    fn load(&self, job_id: &JobIdBytes) -> io::Result<Option<TaskRecord>> {
        todo!("owned by ak-store agent")
    }

    fn list(&self) -> io::Result<Vec<TaskRecord>> {
        todo!("owned by ak-store agent")
    }

    fn remove(&self, job_id: &JobIdBytes) -> io::Result<bool> {
        todo!("owned by ak-store agent")
    }
}

/// Current time in Unix milliseconds (single source for record timestamps).
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-store agent")
    fn placeholder() {}
}
