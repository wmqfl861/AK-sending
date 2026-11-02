//! Storage-facing traits shared by the engine and platform adapters.
//!
//! `SourceHandle` and `FileSink` are deliberately synchronous: the async engine
//! calls them through blocking tasks so the traits stay usable from any thread
//! and testable without an async runtime.
use std::io;
use std::num::NonZeroU32;

use crate::error::ErrorCode;
use crate::manifest::{FileEntry, Manifest};

/// Read side of a transfer source. Implementations must be safe to call from
/// any thread; chunk reads are bounded by the buffer the caller provides.
pub trait SourceHandle: Send {
    fn manifest(&self) -> &Manifest;

    /// Read the chunk `chunk_index` of file `file_index` into `buf`, returning
    /// the number of bytes read (exactly the manifest chunk length, except for
    /// the final chunk of a file). Errors if the source no longer matches the
    /// manifest version.
    fn read_chunk(&self, file_index: u32, chunk_index: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Re-check that the source still matches the manifest (size and mtime per
    /// file). Returns [`ErrorCode::SourceChanged`] on drift.
    fn verify_stable(&self) -> Result<(), ErrorCode>;
}

/// What the receiver does when the target file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Refuse the file; surface a conflict error. Never silently overwrite.
    Fail,
    /// Keep both: save under a non-conflicting generated name.
    Rename,
}

/// Write side for one file on the receiver. Chunks arrive in order except when
/// resuming, where the sink is first advanced over the already-verified prefix.
pub trait FileSink: Send {
    /// Write one chunk at its manifest position and record its verification.
    /// Implementations verify the chunk digest and must fail with
    /// `HASH_MISMATCH` data on mismatch instead of storing bad bytes silently.
    fn write_chunk(&mut self, chunk_index: u64, data: &[u8]) -> io::Result<()>;

    /// Length of the contiguous verified prefix, in chunks.
    fn verified_prefix_chunks(&self) -> u64;

    /// Verify the whole-file digest, durably finish the file (fsync plus atomic
    /// rename), making it visible at its final name.
    fn finish(&mut self) -> io::Result<()>;

    /// Discard the partial file. Consuming on purpose: a discarded sink cannot
    /// be finished later by accident.
    fn discard(self)
    where
        Self: Sized;
}

/// Destination side of a transfer on the receiver.
pub trait SaveTarget: Send + Sync {
    /// The root directory files are written into, for display and checkpointing.
    fn root(&self) -> &std::path::Path;

    /// Bytes available at the destination, if the platform can report it.
    fn available_bytes(&self) -> Option<u64> {
        None
    }

    /// Prepare a sink for one entry. Must reject path escapes and symlink races
    /// (see AGENTS.md) before any file is created.
    fn prepare_file(
        &self,
        entry: &FileEntry,
        conflict: ConflictPolicy,
    ) -> io::Result<Box<dyn FileSink>>;
}

/// Default chunk size for local transfers: 1 MiB. Not a product quota.
pub fn default_chunk_size() -> NonZeroU32 {
    NonZeroU32::new(1024 * 1024).expect("nonzero constant")
}
