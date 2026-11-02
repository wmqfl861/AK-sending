//! Local filesystem adapter: source scanning and receive-side save targets.
//!
//! Guarantees (see AGENTS.md): no path escapes (`..`, absolute, drive letters
//! rejected by `ak_core::manifest`), no symlink races (every existing path
//! component is re-checked with `symlink_metadata` before use), bounded
//! memory (streaming digests), and atomic file commit (temp `.akpart` file,
//! fsync, rename; the final name only appears after the whole digest matches).
#![forbid(unsafe_code)]

use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use ak_core::handle::{ConflictPolicy, FileSink, SaveTarget, SourceHandle};
use ak_core::manifest::Manifest;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("path not found: {0}")]
    NotFound(PathBuf),
    #[error("not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("root itself is a symlink: {0}")]
    RootIsSymlink(PathBuf),
    #[error("path component is a symlink: {0}")]
    SymlinkComponent(PathBuf),
    #[error("relative path escapes the target root")]
    Escape,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Send side: one file or one directory tree, scanned and digested at open.
pub struct LocalSource {
    root: PathBuf,
    manifest: Manifest,
}

impl LocalSource {
    /// Scan `root` (file or directory), compute BLAKE3 whole and chunk digests
    /// with streaming reads, and record size/mtime for source versioning.
    /// Empty directories become manifest entries with empty chunk digests only
    /// if representable; the caller decides how to page them.
    pub fn open(root: &Path, chunk_size: NonZeroU32) -> Result<Self, OpenError> {
        todo!("owned by ak-storage agent")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Display name of the transfer root (file or directory name).
    pub fn root_name(&self) -> String {
        todo!("owned by ak-storage agent")
    }
}

impl SourceHandle for LocalSource {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn read_chunk(&self, file_index: u32, chunk_index: u64, buf: &mut [u8]) -> io::Result<usize> {
        todo!("owned by ak-storage agent")
    }

    fn verify_stable(&self) -> Result<(), ak_core::error::ErrorCode> {
        todo!("owned by ak-storage agent")
    }
}

/// Receive side: one destination directory tree.
pub struct LocalSaveTarget {
    root: PathBuf,
}

impl LocalSaveTarget {
    /// Open the destination root. It must already exist, be a real directory
    /// (not a symlink), and be writable.
    pub fn open(root: &Path) -> Result<Self, OpenError> {
        todo!("owned by ak-storage agent")
    }
}

impl SaveTarget for LocalSaveTarget {
    fn root(&self) -> &Path {
        &self.root
    }

    fn available_bytes(&self) -> Option<u64> {
        todo!("owned by ak-storage agent")
    }

    /// Prepare the sink for one entry. If a `.akpart` file for this entry
    /// already exists, the sink re-reads it, re-verifies the chunk-digest
    /// prefix, and resumes from there (transparent resume).
    fn prepare_file(&self, entry: &ak_core::manifest::FileEntry, conflict: ConflictPolicy) -> io::Result<Box<dyn FileSink>> {
        todo!("owned by ak-storage agent")
    }
}

/// File-suffix appended to in-progress files; never exposed as a final name.
pub const PART_SUFFIX: &str = ".akpart";

/// Rename suffix used by [`ConflictPolicy::Rename`]: `name (2).ext`.
pub fn conflict_free_name(existing: &Path) -> PathBuf {
    todo!("owned by ak-storage agent")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "replaced by the ak-storage agent"]
    fn placeholder() {}
}
