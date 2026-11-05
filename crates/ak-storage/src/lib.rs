//! Local filesystem adapter: source scanning and receive-side save targets.
//!
//! Guarantees (see AGENTS.md): no path escapes (`..`, absolute, drive letters
//! rejected by `ak_core::manifest`), symlink defenses (every existing path
//! component is refused when `symlink_metadata` shows a link, and each
//! step re-validates immediately before acting), bounded memory (streaming
//! digests), and atomic no-replace file commit (temp `.akpart` file,
//! fsync, then hard-link + unlink; the final name only appears after the
//! whole digest matches and is never created over an existing file).
//!
//! # Symlink race posture (review 2026-11-05, P1-2)
//!
//! `std` has no `openat2`/no-follow open on Windows, so every check-then-
//! act sequence below has a residual window: a concurrent local process
//! that swaps a checked component for a symlink between the check and the
//! following path operation can still redirect that one operation. The
//! defenses therefore (a) re-run `symlink_metadata` immediately before
//! each open/truncate/rename/link/removal, shrinking the window to the
//! minimum std allows, (b) fail closed when a link is found, and (c)
//! prefer handle-bound operations (truncation and writes go through the
//! already-open descriptor, never the path). The window is narrowed, not
//! eliminated; each site documents its residual risk.
//!
//! # Per-task staging namespace (wave 20)
//!
//! [`LocalSaveTarget::prepare_file_staged`] writes each task's `.akpart`
//! files under `<root>/.ak-staging/<task-hex>/...` instead of next to the
//! final name, so two concurrent tasks aiming at the same destination
//! never share (or clobber) one in-flight part. Commit stays the same
//! atomic no-replace [`commit_no_replace`] into the final location. A
//! legacy same-directory `.akpart` left by a pre-staging run is migrated
//! into the task's staging tree on prepare and resumed from there;
//! [`discard_staging`] removes one task's whole staging tree on cancel or
//! task end without touching any other task's parts. `.ak-staging` is a
//! reserved namespace: a manifest rel_path that names it is rejected at
//! prepare, so received content can never land inside (and be deleted by)
//! the engine's terminal staging cleanup.
#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use filetime::FileTime;

use ak_core::chunk::ChunkPlan;
use ak_core::error::ErrorCode;
use ak_core::handle::{default_chunk_size, ConflictPolicy, FileSink, SaveTarget, SourceHandle};
use ak_core::manifest::{validate_rel_path, FileEntry, Manifest, DIGEST_ALG_BLAKE3, DIGEST_LEN};

/// Why opening a source root or save target failed: missing path, wrong
/// type, symlink anywhere on the root path, escape attempt, or io error.
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

// ---------------------------------------------------------------------------
// Small helpers shared by source and sink
// ---------------------------------------------------------------------------

/// BLAKE3 digest of a byte slice, in manifest representation.
fn b3(data: &[u8]) -> [u8; DIGEST_LEN] {
    *blake3::hash(data).as_bytes()
}

/// Read until `buf` is full or EOF; returns the number of bytes read.
fn read_fill<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Streaming BLAKE3 over a reader using the provided scratch buffer.
fn hash_reader<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<[u8; DIGEST_LEN]> {
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = read_fill(reader, buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

/// mtime in Unix milliseconds, negative before the epoch.
fn unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Manifest mtime (Unix ms, may be pre-epoch) as a `FileTime` stamp.
fn mtime_file_time(ms: i64) -> FileTime {
    FileTime::from_unix_time(
        ms.div_euclid(1000),
        (ms.rem_euclid(1000) as u32) * 1_000_000,
    )
}

fn invalid_input_io(msg: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, msg.into())
}

/// Defensive storage-side cap on every chunk buffer this crate allocates:
/// 16 MiB. The engine validates `Offer.chunk_size` against its own limits
/// (8 MiB) before calling here, but this API must stay safe when driven
/// directly (review 2026-11-04, issue 1, storage side): an entry prepared
/// with `chunk_size = u32::MAX` must fail fast with `InvalidInput` instead
/// of attempting a ~4 GiB allocation. Double insurance with the engine
/// check, not a replacement for it.
pub const MAX_CHUNK_BUFFER_BYTES: usize = 16 << 20;

/// Reject a chunk size whose streaming buffer would exceed
/// [`MAX_CHUNK_BUFFER_BYTES`]. Called before any buffer is allocated.
fn ensure_chunk_size_within_cap(chunk_size: NonZeroU32) -> io::Result<()> {
    if chunk_size.get() as usize > MAX_CHUNK_BUFFER_BYTES {
        return Err(invalid_input_io(format!(
            "chunk size {} exceeds the storage-side cap of {} bytes",
            chunk_size.get(),
            MAX_CHUNK_BUFFER_BYTES
        )));
    }
    Ok(())
}

/// Buffer length for one streaming pass over `total_bytes` in chunks of
/// `chunk_size`: `min(chunk_size, total_bytes)`. An empty file needs no
/// buffer at all (zero allocation), and a file shorter than one chunk never
/// allocates a full chunk. The caller must have run
/// `ensure_chunk_size_within_cap` first, so the result always fits `usize`.
fn streaming_buffer_len(chunk_size: NonZeroU32, total_bytes: u64) -> usize {
    total_bytes.min(u64::from(chunk_size.get())) as usize
}

fn escape_err() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, OpenError::Escape)
}

/// Refuse when `path`'s final component is a symlink. This is a
/// re-validation immediately before a path-based action (open, mtime
/// stamp, unlink, hard-link claim): std has no openat2/no-follow open on
/// Windows, so each such check narrows — but cannot eliminate — the
/// check-to-use window (review 2026-11-05, P1-2). An attacker swapping
/// the component AFTER this check can still redirect the following
/// path operation; handle-bound operations (read/write/truncate through
/// an open descriptor) are immune by construction.
fn ensure_not_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            ErrorKind::InvalidInput,
            OpenError::SymlinkComponent(path.to_path_buf()),
        )),
        Ok(_) => Ok(()),
        // An absent path cannot be a link; the action that follows
        // (create/open) reports the authoritative error.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn conflict_err(path: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::AlreadyExists,
        format!("conflict: {} already exists", path.display()),
    )
}

// ---------------------------------------------------------------------------
// Send side
// ---------------------------------------------------------------------------

/// Send side: one file or one directory tree, scanned and digested at open.
pub struct LocalSource {
    root: PathBuf,
    /// True when `root` is the single file itself; its manifest entry names
    /// the file (for the receiver) but resolves back to `root`, not to a
    /// sibling of `root`.
    root_is_file: bool,
    manifest: Manifest,
}

impl LocalSource {
    /// Scan `root` (file or directory), compute BLAKE3 whole and chunk digests
    /// with streaming reads, and record size/mtime for source versioning.
    /// Empty directories become manifest entries with empty chunk digests only
    /// if representable; the caller decides how to page them.
    ///
    /// Directory traversal is an iterative pre-order walk (explicit stack, no
    /// recursion) with children sorted by name at each level, so a parent
    /// directory entry always precedes its children and the receiver can
    /// create entries in manifest order. Symlinks are never followed: the
    /// root itself is rejected as [`OpenError::RootIsSymlink`] and any symlink
    /// below it as [`OpenError::SymlinkComponent`].
    pub fn open(root: &Path, chunk_size: NonZeroU32) -> Result<Self, OpenError> {
        // Same defensive cap as the receive side: reject an absurd chunk
        // size before scan_file allocates its streaming buffer.
        ensure_chunk_size_within_cap(chunk_size).map_err(OpenError::Io)?;
        let meta = match fs::symlink_metadata(root) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(OpenError::NotFound(root.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            return Err(OpenError::RootIsSymlink(root.to_path_buf()));
        }
        let mut entries = Vec::new();
        let root_is_file;
        if file_type.is_dir() {
            root_is_file = false;
            scan_tree(root, chunk_size.get(), &mut entries)?;
        } else if file_type.is_file() {
            root_is_file = true;
            let name = root.file_name().and_then(OsStr::to_str).ok_or_else(|| {
                OpenError::Io(invalid_input_io(format!(
                    "non-UTF-8 root file name: {}",
                    root.display()
                )))
            })?;
            scan_file(root, name, chunk_size.get(), &mut entries)?;
        } else {
            return Err(OpenError::Io(invalid_input_io(format!(
                "unsupported file type at transfer root: {}",
                root.display()
            ))));
        }
        Ok(Self {
            root: root.to_path_buf(),
            root_is_file,
            manifest: Manifest {
                chunk_size,
                entries,
            },
        })
    }

    /// Absolute path of an entry's content on the sender.
    fn entry_path(&self, entry: &FileEntry) -> PathBuf {
        if self.root_is_file {
            self.root.clone()
        } else {
            self.root.join(&entry.rel_path)
        }
    }

    /// The canonicalized source root path being scanned.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Display name of the transfer root (file or directory name).
    pub fn root_name(&self) -> String {
        self.root
            .file_name()
            .unwrap_or_else(|| self.root.as_os_str())
            .to_string_lossy()
            .into_owned()
    }
}

/// Iterative pre-order walk over an explicit work stack: children are sorted
/// by name at each level, and a directory entry is emitted immediately before
/// its own children. No recursion, so arbitrarily deep trees cannot overflow
/// the thread stack.
fn scan_tree(root: &Path, chunk_size: u32, entries: &mut Vec<FileEntry>) -> Result<(), OpenError> {
    /// One unit of pending scan work.
    enum Frame {
        /// Read the directory's children, sort them, queue them.
        Dir {
            path: PathBuf,
            rel: String,
            /// False only for the transfer root, which gets no entry.
            emit_entry: bool,
        },
        /// Digest one regular file.
        File { path: PathBuf, rel: String },
    }
    let mut stack = vec![Frame::Dir {
        path: root.to_path_buf(),
        rel: String::new(),
        emit_entry: false,
    }];
    while let Some(frame) = stack.pop() {
        match frame {
            Frame::File { path, rel } => scan_file(&path, &rel, chunk_size, entries)?,
            Frame::Dir {
                path,
                rel,
                emit_entry,
            } => {
                if emit_entry {
                    entries.push(FileEntry::directory(&rel).map_err(|_| bad_rel_path(&rel))?);
                }
                let mut children: Vec<(OsString, PathBuf, fs::FileType)> = Vec::new();
                for child in fs::read_dir(&path)? {
                    let child = child?;
                    let path = child.path();
                    let meta = fs::symlink_metadata(&path)?;
                    children.push((child.file_name(), path, meta.file_type()));
                }
                children.sort_by(|a, b| a.0.cmp(&b.0));
                // Push in reverse so the stack pops children in sorted order;
                // each directory's whole subtree drains before the next
                // sibling, which keeps parents ahead of their children.
                for (name, path, file_type) in children.into_iter().rev() {
                    let name = name.to_str().ok_or_else(|| {
                        OpenError::Io(invalid_input_io(format!(
                            "non-UTF-8 file name: {}",
                            path.display()
                        )))
                    })?;
                    let child_rel = if rel.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{rel}/{name}")
                    };
                    if file_type.is_symlink() {
                        return Err(OpenError::SymlinkComponent(path));
                    } else if file_type.is_dir() {
                        stack.push(Frame::Dir {
                            path,
                            rel: child_rel,
                            emit_entry: true,
                        });
                    } else if file_type.is_file() {
                        stack.push(Frame::File {
                            path,
                            rel: child_rel,
                        });
                    } else {
                        return Err(OpenError::Io(invalid_input_io(format!(
                            "unsupported file type in source tree: {}",
                            path.display()
                        ))));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Stream one file once, computing the whole digest and per-chunk digests with
/// a single chunk-sized buffer (bounded memory).
fn scan_file(
    path: &Path,
    rel: &str,
    chunk_size: u32,
    entries: &mut Vec<FileEntry>,
) -> Result<(), OpenError> {
    let meta = fs::symlink_metadata(path)?;
    let mtime = unix_ms(meta.modified()?);
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; chunk_size as usize];
    let mut whole = blake3::Hasher::new();
    let mut chunk_digests = Vec::new();
    let mut size = 0u64;
    loop {
        let n = read_fill(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        whole.update(&buf[..n]);
        chunk_digests.push(b3(&buf[..n]));
        size += n as u64;
    }
    let entry = FileEntry::new(
        rel,
        size,
        mtime,
        *whole.finalize().as_bytes(),
        chunk_digests,
    )
    .map_err(|_| bad_rel_path(rel))?;
    entries.push(entry);
    Ok(())
}

fn bad_rel_path(rel: &str) -> OpenError {
    OpenError::Io(invalid_input_io(format!(
        "file name not representable as a transfer path: {rel:?}"
    )))
}

impl SourceHandle for LocalSource {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn read_chunk(&self, file_index: u32, chunk_index: u64, buf: &mut [u8]) -> io::Result<usize> {
        let entry = self
            .manifest
            .entries
            .get(file_index as usize)
            .ok_or_else(|| invalid_input_io("file index out of range"))?;
        if entry.is_dir {
            return Err(invalid_input_io("entry is a directory, not a file"));
        }
        let plan = ChunkPlan::new(entry.size, self.manifest.chunk_size);
        let range = plan
            .range(chunk_index)
            .ok_or_else(|| invalid_input_io("chunk index out of range"))?;
        let len = usize::try_from(range.end - range.start)
            .map_err(|_| invalid_input_io("chunk length out of usize range"))?;
        if buf.len() < len {
            return Err(invalid_input_io("destination buffer too small for chunk"));
        }
        // Open per call: cheap, and keeps `&self` callable from any thread.
        let mut file = File::open(self.entry_path(entry))?;
        file.seek(SeekFrom::Start(range.start))?;
        let n = read_fill(&mut file, &mut buf[..len])?;
        if n != len {
            return Err(io::Error::new(ErrorKind::InvalidData, "SOURCE_CHANGED"));
        }
        Ok(n)
    }

    fn verify_stable(&self) -> Result<(), ErrorCode> {
        for entry in &self.manifest.entries {
            if entry.is_dir {
                continue;
            }
            let path = self.entry_path(entry);
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => return Err(ErrorCode::SourceChanged),
            };
            if meta.file_type().is_symlink() || meta.len() != entry.size {
                return Err(ErrorCode::SourceChanged);
            }
            match meta.modified() {
                Ok(t) if unix_ms(t) == entry.mtime_unix_ms => {}
                _ => return Err(ErrorCode::SourceChanged),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Receive side
// ---------------------------------------------------------------------------

/// Receive side: one destination directory tree.
pub struct LocalSaveTarget {
    root: PathBuf,
}

impl LocalSaveTarget {
    /// Open the destination root. It must already exist, be a real directory
    /// (not a symlink), and be writable. The stored root is the canonicalized
    /// path so later joins cannot be redirected through parent symlinks.
    pub fn open(root: &Path) -> Result<Self, OpenError> {
        let meta = match fs::symlink_metadata(root) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(OpenError::NotFound(root.to_path_buf()));
            }
            Err(e) => return Err(e.into()),
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            return Err(OpenError::RootIsSymlink(root.to_path_buf()));
        }
        if !file_type.is_dir() {
            return Err(OpenError::NotDirectory(root.to_path_buf()));
        }
        let canonical = fs::canonicalize(root)?;
        Ok(Self { root: canonical })
    }

    /// [`SaveTarget::prepare_file`] variant that lets the engine state the
    /// chunk size of the manifest it is about to replay, and returns the
    /// concrete sink so consuming methods like [`FileSink::discard`] (which
    /// requires `Self: Sized`) are callable. Resume verification of an
    /// existing `.akpart` file needs the exact chunk boundaries; the trait
    /// method falls back to [`default_chunk_size`]. A wrong assumption can
    /// never corrupt anything: the first chunk digest simply fails and the
    /// part file is retransmitted from byte zero.
    pub fn prepare_file_with_chunk_size(
        &self,
        entry: &FileEntry,
        conflict: ConflictPolicy,
        chunk_size: NonZeroU32,
    ) -> io::Result<LocalFileSink> {
        // Reject an oversized chunk size at API entry, before any file is
        // opened or any buffer allocated (review 2026-11-04, issue 1).
        ensure_chunk_size_within_cap(chunk_size)?;
        let parts = checked_rel_components(entry)?;
        if entry.is_dir {
            ensure_real_dirs(&self.root, &parts)?;
            return Ok(LocalFileSink {
                kind: SinkKind::Directory,
            });
        }
        if entry.digest_alg != DIGEST_ALG_BLAKE3 {
            return Err(invalid_input_io("unsupported digest algorithm"));
        }
        let plan = ChunkPlan::new(entry.size, chunk_size);
        if plan.chunk_count() != entry.chunk_digests.len() as u64 {
            return Err(invalid_input_io(
                "entry chunk digests inconsistent with its size and the chunk size",
            ));
        }
        let (name, parents) = parts.split_last().ok_or_else(escape_err)?;
        ensure_real_dirs(&self.root, parents)?;
        let mut final_path = self.root.clone();
        for part in parents {
            final_path.push(part);
        }
        final_path.push(name);
        // Already-committed fast path (storage side of the engine retry
        // idempotence fix, review 2026-11-04): when the final file exists
        // and its whole digest matches the manifest entry, a previous run
        // committed this exact content. Return a sink that reports every
        // chunk as verified and whose finish() is an idempotent success
        // without rewriting either name — an engine retry after "job
        // already done" then needs no retransmission and cannot trip the
        // conflict policy against its own output. A final file with
        // DIFFERENT content is not a match and falls through to the normal
        // part-based path, where the conflict policy decides at finish().
        if let Ok(meta) = fs::symlink_metadata(&final_path) {
            if meta.file_type().is_file()
                && meta.len() == entry.size
                && final_matches_digest(&final_path, entry, chunk_size)?
            {
                return Ok(LocalFileSink {
                    kind: SinkKind::Committed {
                        chunk_count: entry.chunk_digests.len() as u64,
                    },
                });
            }
        }
        let part_path = with_part_suffix(&final_path);
        // Re-validated immediately before the open: a part swapped for a
        // symlink after an earlier check would otherwise be opened through
        // the link and its target truncated by the resume pass below.
        // Residual window (no openat2/no-follow in std): a swap between
        // this check and the open is still possible (P1-2).
        ensure_not_symlink(&part_path)?;
        // No truncate: an existing part is verified and resumed in place.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)?;
        let verified_prefix = verify_part_prefix(&mut file, &part_path, entry, chunk_size)?;
        let state = PartState {
            file,
            final_path,
            part_path,
            conflict,
            size: entry.size,
            mtime_unix_ms: entry.mtime_unix_ms,
            chunk_size,
            whole_digest: entry.whole_digest,
            chunk_digests: entry.chunk_digests.clone(),
            verified_prefix,
            finished: false,
            staged: false,
        };
        Ok(LocalFileSink {
            kind: SinkKind::File(state),
        })
    }
}

impl SaveTarget for LocalSaveTarget {
    fn root(&self) -> &Path {
        &self.root
    }

    /// Real free space on the destination volume (`fs2::available_space`:
    /// GetDiskFreeSpaceExW on Windows, statvfs on Unix). Any platform error
    /// maps to `None`, keeping the trait contract: unknown capacity is
    /// reported as unknown, never faked, and exhaustion is still surfaced by
    /// real write errors (see docs/STORAGE_AND_PLATFORMS.zh-CN.md).
    fn available_bytes(&self) -> Option<u64> {
        fs2::available_space(&self.root).ok()
    }

    /// Prepare the sink for one entry. If a `.akpart` file for this entry
    /// already exists, the sink re-reads it, re-verifies the chunk-digest
    /// prefix, and resumes from there (transparent resume).
    fn prepare_file(
        &self,
        entry: &ak_core::manifest::FileEntry,
        conflict: ConflictPolicy,
    ) -> io::Result<Box<dyn FileSink>> {
        Ok(Box::new(self.prepare_file_with_chunk_size(
            entry,
            conflict,
            default_chunk_size(),
        )?))
    }
}

/// Validate the entry's relative path component by component and return the
/// components. This re-check runs even for hand-built entries because
/// `FileEntry`'s fields are public: only `Component::Normal` parts are
/// accepted, then the full `validate_rel_path` rule set is applied to the
/// reconstructed `/`-separated path. Callers join the returned components
/// onto the root themselves; the raw `rel_path` is never joined directly.
///
/// Reserved namespace (review 2026-11-05, P2): a component naming the
/// staging directory `.ak-staging` is rejected — a received file placed
/// under `<root>/.ak-staging/<task>/…` would be committed inside this
/// task's own staging tree and then deleted by the engine's terminal
/// [`discard_staging`] ("success deletes the successful file"). The
/// comparison is ASCII-case-insensitive because NTFS resolves directory
/// names case-insensitively by default.
fn checked_rel_components(entry: &FileEntry) -> io::Result<Vec<&OsStr>> {
    let mut parts = Vec::new();
    let mut joined = String::new();
    for comp in entry.rel_path.components() {
        let Component::Normal(part) = comp else {
            return Err(escape_err());
        };
        if part
            .as_encoded_bytes()
            .eq_ignore_ascii_case(STAGING_DIR.as_bytes())
        {
            return Err(invalid_input_io(format!(
                "rel path collides with the reserved staging namespace '{STAGING_DIR}'"
            )));
        }
        let s = part.to_str().ok_or_else(escape_err)?;
        if !joined.is_empty() {
            joined.push('/');
        }
        joined.push_str(s);
        parts.push(part);
    }
    validate_rel_path(&joined).map_err(|_| escape_err())?;
    Ok(parts)
}

/// Create `root/parts[..]` level by level. An already-existing level is only
/// accepted if `symlink_metadata` proves it is a real directory (a symlink
/// to a directory would pass `exists()`). Honest limitation: without
/// openat2 this create-then-check sequence still has a swap window
/// between `create_dir` failing with `AlreadyExists` and the
/// `symlink_metadata` that classifies the level; every later step
/// re-validates again (see [`ensure_not_symlink`]).
fn ensure_real_dirs(root: &Path, parts: &[&OsStr]) -> io::Result<()> {
    let mut current = root.to_path_buf();
    for part in parts {
        current.push(part);
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let meta = fs::symlink_metadata(&current).map_err(|_| e)?;
                let ft = meta.file_type();
                if ft.is_symlink() {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        OpenError::SymlinkComponent(current),
                    ));
                }
                if !ft.is_dir() {
                    return Err(io::Error::new(
                        ErrorKind::AlreadyExists,
                        OpenError::NotDirectory(current),
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Append [`PART_SUFFIX`] to a full path (simple OsString append, keeps any
/// multi-dot extensions intact). When the final component is so long that the
/// suffixed name would exceed common filesystem component limits (255 chars),
/// fall back to a short deterministic sibling instead: `.akpart-<blake3 of the
/// final component, first 16 hex>`. The fallback is derived from the final
/// name only, so resume finds the same part file again (scenario test found
/// the 249-byte-name boundary on NTFS).
fn with_part_suffix(path: &Path) -> PathBuf {
    const COMPONENT_LIMIT: usize = 255;
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };
    let name_bytes = name.as_encoded_bytes();
    if name_bytes.len() + PART_SUFFIX.len() <= COMPONENT_LIMIT {
        let mut os = path.as_os_str().to_os_string();
        os.push(PART_SUFFIX);
        PathBuf::from(os)
    } else {
        let digest = blake3::hash(name_bytes);
        let short = format!(".akpart-{}", hex_prefix(digest.as_bytes(), 16));
        path.with_file_name(short)
    }
}

/// Lowercase hex of the first `n` bytes (no external hex dependency).
fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let mut out = String::with_capacity(n * 2);
    for byte in bytes.iter().take(n) {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// True when the existing final file's whole BLAKE3 digest matches the
/// entry. The caller has already checked the length; this streams the file
/// once with a buffer of `min(chunk_size, size)` (zero allocation for an
/// empty file, where the empty digest compares directly).
fn final_matches_digest(
    final_path: &Path,
    entry: &FileEntry,
    chunk_size: NonZeroU32,
) -> io::Result<bool> {
    if entry.size == 0 {
        // Length already checked by the caller: the only remaining
        // question is whether the manifest digest is the hash of nothing.
        return Ok(entry.whole_digest == b3(b""));
    }
    let mut file = File::open(final_path)?;
    let mut buf = vec![0u8; streaming_buffer_len(chunk_size, entry.size)];
    Ok(hash_reader(&mut file, &mut buf)? == entry.whole_digest)
}

/// Re-verify an existing `.akpart` file chunk by chunk against the manifest,
/// truncate at the first mismatch or truncation point, position the write
/// cursor there, and return the verified chunk count. Bounded memory: one
/// buffer of `min(chunk_size, file size)` — no allocation at all for an
/// empty entry, and a file shorter than one chunk never allocates a full
/// chunk (the short tail needs less than the chunk size).
///
/// The truncation targets the already-open descriptor, so a path swap
/// cannot redirect it; the `part_path` re-validation below is
/// belt-and-braces (P1-2: re-check before every action on the part path)
/// and catches a link planted between the caller's pre-open check and
/// here in time for the mtime/commit steps that DO act on the path.
fn verify_part_prefix(
    file: &mut File,
    part_path: &Path,
    entry: &FileEntry,
    chunk_size: NonZeroU32,
) -> io::Result<u64> {
    ensure_chunk_size_within_cap(chunk_size)?;
    ensure_not_symlink(part_path)?;
    let plan = ChunkPlan::new(entry.size, chunk_size);
    // Every chunk length is min(chunk_size, size - start), so a buffer of
    // min(chunk_size, size) covers each `want` slice below.
    let mut buf = vec![0u8; streaming_buffer_len(chunk_size, entry.size)];
    file.rewind()?;
    let mut verified = 0u64;
    for digest in &entry.chunk_digests {
        let Some(range) = plan.range(verified) else {
            break;
        };
        let want = usize::try_from(range.end - range.start)
            .map_err(|_| invalid_input_io("chunk length out of usize range"))?;
        let n = read_fill(file, &mut buf[..want])?;
        if n != want || b3(&buf[..want]) != *digest {
            break;
        }
        verified += 1;
    }
    let verified_bytes = if verified == 0 {
        0
    } else {
        plan.range(verified - 1).map_or(0, |r| r.end)
    };
    let current_len = file.metadata()?.len();
    if current_len > verified_bytes {
        file.set_len(verified_bytes)?;
    }
    file.seek(SeekFrom::Start(verified_bytes))?;
    Ok(verified)
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// Sink for one entry prepared by [`LocalSaveTarget`]. Directory entries get
/// a no-op sink; file entries write a `.akpart` sibling that is renamed to
/// the final name only after the whole digest matches.
pub struct LocalFileSink {
    kind: SinkKind,
}

enum SinkKind {
    Directory,
    File(PartState),
    /// The final file already exists with exactly the manifest content
    /// (whole digest verified at prepare). Every chunk counts as verified,
    /// `finish()` is an idempotent success, and neither the final nor the
    /// part name is touched again.
    Committed {
        chunk_count: u64,
    },
}

struct PartState {
    file: File,
    final_path: PathBuf,
    part_path: PathBuf,
    conflict: ConflictPolicy,
    size: u64,
    mtime_unix_ms: i64,
    chunk_size: NonZeroU32,
    whole_digest: [u8; DIGEST_LEN],
    chunk_digests: Vec<[u8; DIGEST_LEN]>,
    verified_prefix: u64,
    finished: bool,
    /// True when the part file lives in this task's staging tree (wave 20).
    /// Only staged sinks resolve a same-target finish race by content: an
    /// existing final whose whole digest already matches this sink's
    /// verified content is reused instead of surfacing as a conflict. The
    /// unstaged path keeps its exact historical behavior.
    staged: bool,
}

impl FileSink for LocalFileSink {
    fn write_chunk(&mut self, chunk_index: u64, data: &[u8]) -> io::Result<()> {
        let st = match &mut self.kind {
            SinkKind::File(st) => st,
            SinkKind::Directory => {
                return Err(invalid_input_io("directory entries accept no file chunks"))
            }
            SinkKind::Committed { chunk_count } => {
                // Already stored and verified via the whole digest of the
                // on-disk final; replaying any in-range chunk is a no-op
                // (same idempotence as a replayed verified prefix).
                let _ = data;
                return if chunk_index < *chunk_count {
                    Ok(())
                } else {
                    Err(invalid_input_io("chunk index out of range"))
                };
            }
        };
        let idx = usize::try_from(chunk_index)
            .map_err(|_| invalid_input_io("chunk index out of range"))?;
        let Some(expected) = st.chunk_digests.get(idx) else {
            return Err(invalid_input_io("chunk index out of range"));
        };
        // Verify first, write second: bad bytes are never stored.
        if b3(data) != *expected {
            return Err(io::Error::new(ErrorKind::InvalidData, "HASH_MISMATCH"));
        }
        let plan = ChunkPlan::new(st.size, st.chunk_size);
        let Some(range) = plan.range(chunk_index) else {
            return Err(invalid_input_io("chunk index out of range"));
        };
        let expected_len = usize::try_from(range.end - range.start)
            .map_err(|_| invalid_input_io("chunk length out of usize range"))?;
        if data.len() != expected_len {
            return Err(invalid_input_io(
                "chunk length does not match the manifest chunk plan",
            ));
        }
        if chunk_index == st.verified_prefix {
            st.file.seek(SeekFrom::Start(range.start))?;
            st.file.write_all(data)?;
            st.verified_prefix += 1;
            Ok(())
        } else if chunk_index < st.verified_prefix {
            // Already stored and verified in this or a previous session.
            Ok(())
        } else {
            Err(invalid_input_io("chunk index ahead of the verified prefix"))
        }
    }

    fn verified_prefix_chunks(&self) -> u64 {
        match &self.kind {
            SinkKind::File(st) => st.verified_prefix,
            SinkKind::Directory => 0,
            SinkKind::Committed { chunk_count } => *chunk_count,
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        let st = match &mut self.kind {
            SinkKind::File(st) => st,
            SinkKind::Directory => return Ok(()),
            SinkKind::Committed { .. } => return Ok(()),
        };
        if st.finished {
            return Ok(());
        }
        let total = st.chunk_digests.len() as u64;
        if st.verified_prefix < total {
            return Err(invalid_input_io(format!(
                "incomplete transfer: {}/{} chunks verified",
                st.verified_prefix, total
            )));
        }
        // Defense in depth: `st.chunk_size` was validated at prepare, and
        // no code path can store a larger one, but the allocation below is
        // guarded again anyway.
        ensure_chunk_size_within_cap(st.chunk_size)?;
        st.file.rewind()?;
        if st.size == 0 {
            // Empty file: no heap buffer at all. The digest gate still
            // catches stray bytes a previous session may have left in the
            // part file (verify_part_prefix normally truncated them).
            if st.file.metadata()?.len() != 0 || st.whole_digest != b3(b"") {
                return Err(io::Error::new(ErrorKind::InvalidData, "HASH_MISMATCH"));
            }
        } else {
            // Bounded buffer: min(chunk_size, file size) — a file shorter
            // than one chunk never allocates the full chunk size.
            let mut buf = vec![0u8; streaming_buffer_len(st.chunk_size, st.size)];
            if hash_reader(&mut st.file, &mut buf)? != st.whole_digest {
                // Keep the part file: its verified prefix is still reusable.
                return Err(io::Error::new(ErrorKind::InvalidData, "HASH_MISMATCH"));
            }
        }
        st.file.sync_all()?;
        // Every remaining part-path action below (mtime stamp, reuse
        // unlink, the hard-link claim) is a PATH operation: re-validate the
        // part path first. set_file_mtime follows symlinks and hard_link
        // claims the linked inode, so a swapped link must be refused here.
        // Residual window (no fd-relative mtime/link in std): a swap after
        // this check can still redirect one operation (P1-2).
        ensure_not_symlink(&st.part_path)?;
        // Best-effort metadata fidelity: stamp the sender's real mtime onto
        // the part file so it survives the rename. Byte fidelity is the hard
        // guarantee of finish(); a failed mtime stamp must not fail the
        // transfer, so its error is deliberately dropped.
        let _ = filetime::set_file_mtime(&st.part_path, mtime_file_time(st.mtime_unix_ms));
        // Pre-check keeps the common conflict path cheap and lets the Rename
        // policy pick a free name before any commit syscall. The atomic
        // no-replace guarantee itself comes from commit_no_replace below, so
        // a concurrent create between check and commit is still detected
        // (AlreadyExists) instead of silently replacing the target.
        let mut target = st.final_path.clone();
        if fs::symlink_metadata(&target).is_ok() {
            // Staged sinks (wave 20) resolve a same-target race by content:
            // when the existing final is byte-identical to this sink's
            // verified content, the concurrent task's commit is reused and
            // this task's staging part is dropped — "same content: exactly
            // one commit, one reuse". Unstaged sinks never take this path.
            if st.staged && staged_target_matches(st, &target)? {
                // The open handle does not block the unlink: std opens with
                // FILE_SHARE_DELETE on Windows and POSIX unlink is immune.
                // Re-check the part path after the digest pass (the window
                // grew while hashing), then drop the part. Best effort; an
                // undeletable part is later reaped by discard_staging /
                // discard_orphan_parts.
                ensure_not_symlink(&st.part_path)?;
                let _ = fs::remove_file(&st.part_path);
                st.finished = true;
                return Ok(());
            }
            match st.conflict {
                ConflictPolicy::Fail => return Err(conflict_err(&target)),
                ConflictPolicy::Rename => target = conflict_free_name(&target),
            }
        }
        let mut renames = 0u32;
        loop {
            // Re-validate right before each claim: the rename/mtime/hash
            // passes above widened the window again (P1-2).
            ensure_not_symlink(&st.part_path)?;
            match commit_no_replace(&st.part_path, &target) {
                Ok(()) => {
                    st.finished = true;
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    // A concurrent task claimed the target between the
                    // pre-check above and this atomic claim (P1-2,
                    // concurrent-finish adjudication): re-read the target
                    // and compare whole digests — identical content is a
                    // REUSE (drop this task's part, succeed idempotently),
                    // different content keeps the conflict policy.
                    if st.staged && staged_target_matches(st, &target)? {
                        ensure_not_symlink(&st.part_path)?;
                        let _ = fs::remove_file(&st.part_path);
                        st.finished = true;
                        return Ok(());
                    }
                    match st.conflict {
                        ConflictPolicy::Fail => return Err(conflict_err(&st.final_path)),
                        ConflictPolicy::Rename => {
                            renames += 1;
                            if renames > 4096 {
                                return Err(conflict_err(&st.final_path));
                            }
                            target = conflict_free_name(&target);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn discard(self) {
        match self.kind {
            SinkKind::File(st) => {
                drop(st.file);
                // Best effort; a vanished part is fine. A part path that
                // has become a symlink mid-flight is NOT removed:
                // remove_file would only delete the link, but the swap
                // itself signals tampering — leave it to the staging
                // cleanup / orphan sweep (P1-2).
                let real = fs::symlink_metadata(&st.part_path)
                    .map(|m| !m.file_type().is_symlink())
                    .unwrap_or(true);
                if real {
                    let _ = fs::remove_file(&st.part_path);
                }
            }
            // Nothing in flight: the final file stays, no part exists.
            SinkKind::Directory | SinkKind::Committed { .. } => {}
        }
    }
}

/// Atomically make `part` visible as `target` WITHOUT ever replacing an
/// existing file at `target` (review 2026-11-04, atomic no-replace commit).
///
/// Both platforms commit through `hard_link(part, target)` + unlink of the
/// part name: `link(2)` / `CreateHardLinkW` fail with `AlreadyExists` when
/// the target name exists, and on success the inode is immediately visible
/// under the final name — link + unlink is the standard atomic no-replace
/// commit.
///
/// Why not `rename`: POSIX `rename(2)` silently replaces an existing
/// target, and as of the current toolchain std's Windows `rename` does too
/// (it uses `FileRenameInformationEx` with replace-if-exists semantics —
/// empirically locked by `windows_rename_replaces_existing_files_is_locked`
/// on rustc 1.99). Neither platform's rename is a no-replace primitive, so
/// neither branch uses it as the commit primitive.
///
/// Windows fallback: on filesystems without hard-link support (FAT/exFAT
/// removable drives) the link fails with a not-supported error; the commit
/// then falls back to rename, where the caller's pre-check is the only
/// guard, leaving a theoretical check-to-commit race on such volumes (no
/// atomic no-replace primitive exists there at all).
///
/// Contract: `Ok(())` means the content is at `target` and nothing
/// pre-existing was overwritten; `AlreadyExists` means `target` was left
/// untouched and `part` still holds the data.
fn commit_no_replace(part: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        link_then_unlink(part, target)
    }
    #[cfg(windows)]
    {
        match link_then_unlink(part, target) {
            Err(e) if hard_links_unsupported(&e) => {
                // Link-less filesystem: best effort via rename; the
                // caller's pre-check caught the non-racing conflicts.
                fs::rename(part, target)
            }
            other => other,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No known primitive with no-replace semantics on this target;
        // refuse rather than risk a silent overwrite via plain rename.
        let _ = (part, target);
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "atomic no-replace commit is not implemented for this platform",
        ))
    }
}

/// Link `part` to `target` (atomic no-replace claim), then drop the part
/// name. Shared by every supported platform.
fn link_then_unlink(part: &Path, target: &Path) -> io::Result<()> {
    match fs::hard_link(part, target) {
        Ok(()) => {
            // The final name is live with the right inode; dropping the old
            // part name is cleanup. A failure here (e.g. a racing orphan
            // sweep) must not fail the commit — the file is already durable
            // under `target`; the stale part name is later reaped by
            // `discard_orphan_parts`.
            let _ = fs::remove_file(part);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Windows-only: error codes raised by filesystems without hard-link
/// support (FAT, exFAT), where `CreateHardLinkW` cannot work at all.
#[cfg(windows)]
fn hard_links_unsupported(e: &io::Error) -> bool {
    // ERROR_INVALID_FUNCTION (1) and ERROR_NOT_SUPPORTED (50) both mean
    // "this filesystem cannot do that"; std may also map them to
    // ErrorKind::Unsupported depending on version.
    e.kind() == ErrorKind::Unsupported || matches!(e.raw_os_error(), Some(1) | Some(50))
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// File-suffix appended to in-progress files; never exposed as a final name.
pub const PART_SUFFIX: &str = ".akpart";

/// Rename suffix used by [`ConflictPolicy::Rename`]: `stem (n).ext` with `n`
/// starting at 2, incremented until the candidate does not exist. Symlinks
/// count as existing (`symlink_metadata`), so a generated name never
/// overwrites a link planted at the destination.
pub fn conflict_free_name(existing: &Path) -> PathBuf {
    let parent = existing.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = existing
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let ext = existing
        .extension()
        .map(|e| e.to_string_lossy().into_owned());
    for n in 2u64..=1_000_000 {
        let candidate = match &ext {
            Some(ext) => parent.join(format!("{stem} ({n}).{ext}")),
            None => parent.join(format!("{stem} ({n})")),
        };
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    // Pathological destination with a million conflicts: hand back the base
    // candidate; the rename will surface the real error.
    parent.join(format!("{stem} (2)"))
}

// ---------------------------------------------------------------------------
// Orphan part cleanup
// ---------------------------------------------------------------------------

/// True when the path's file name is a part file: the regular
/// `<name>.akpart` form or the long-name fallback `.akpart-<hex>`.
fn has_part_suffix(path: &Path) -> bool {
    path.file_name().is_some_and(|n| {
        let bytes = n.as_encoded_bytes();
        bytes.ends_with(PART_SUFFIX.as_bytes()) || bytes.starts_with(b".akpart-")
    })
}

/// Delete stale `*.akpart` temp files anywhere under `root`: a part whose last
/// modification is older than `older_than` counts as an orphan left behind by
/// an interrupted transfer, so orphan detection is purely mtime-based.
/// Returns how many files were removed. The walk is iterative, never follows
/// symlinks, and never touches files without the part suffix. A part that is
/// still being written by a live sink keeps a fresh mtime and survives.
pub fn discard_orphan_parts(root: &Path, older_than: Duration) -> io::Result<u64> {
    let now = SystemTime::now();
    let mut removed = 0u64;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in fs::read_dir(&dir)? {
            let child = child?;
            let path = child.path();
            // symlink_metadata classifies the link itself; never follow it.
            let meta = fs::symlink_metadata(&path)?;
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                continue;
            } else if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && has_part_suffix(&path) {
                // Unreadable or future mtimes count as age zero: keep the file.
                let age = now
                    .duration_since(meta.modified().unwrap_or(now))
                    .unwrap_or_default();
                if age > older_than {
                    fs::remove_file(&path)?;
                    removed += 1;
                }
            }
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Per-task staging namespace (wave 20)
// ---------------------------------------------------------------------------

/// Directory directly under a save target root holding all per-task
/// staging trees: `<root>/.ak-staging/<task-hex>/`.
pub const STAGING_DIR: &str = ".ak-staging";

/// Staging subtree owned by one task: `<root>/.ak-staging/<task_hex>/`,
/// where `task_hex` is the 32-character lowercase hex of the 16-byte task
/// id (the wire `transfer_id`/scheduler task identity — B5 passes the same
/// id on every prepare of a task so same-instance resume reuses its parts).
/// Pure path arithmetic: no validation, no filesystem access.
pub fn staging_path(root: &Path, task_id: [u8; 16]) -> PathBuf {
    root.join(STAGING_DIR).join(hex_prefix(&task_id, 16))
}

impl LocalSaveTarget {
    /// [`SaveTarget::prepare_file`] variant that keeps this task's in-flight
    /// `.akpart` inside its own staging tree
    /// (`<root>/.ak-staging/<task_hex>/<entry dirs…>/<name>.akpart`)
    /// instead of next to the final name, so concurrent tasks aiming at
    /// the same destination never compete for one part file.
    ///
    /// Semantics beyond [`LocalSaveTarget::prepare_file_with_chunk_size`]:
    ///
    /// * same validation (chunk-size cap, path safety, digest algorithm,
    ///   chunk-plan consistency), same already-committed fast path, same
    ///   resume-by-verified-prefix, same atomic no-replace
    ///   [`commit_no_replace`] at finish;
    /// * a legacy same-directory `.akpart` (pre-staging run) is moved into
    ///   this task's staging tree on first prepare and resumed from there —
    ///   migration only runs when no staging part exists yet, so it can
    ///   never clobber a longer in-flight staged part;
    /// * at finish, a same-target race is resolved by content: when the
    ///   existing final already matches this sink's verified whole digest,
    ///   that commit is reused (idempotent success, own part dropped);
    ///   different content still honors the conflict policy (Fail errors,
    ///   Rename lands as ` (2)`).
    ///
    /// The engine calls [`discard_staging`] with the same `task_id` when
    /// the task reaches a terminal state (cancel or completion cleanup).
    /// The all-zero task id is reserved (contract sentinel) and rejected.
    pub fn prepare_file_staged(
        &self,
        task_id: [u8; 16],
        entry: &FileEntry,
        conflict: ConflictPolicy,
        chunk_size: NonZeroU32,
    ) -> io::Result<LocalFileSink> {
        if task_id == [0u8; 16] {
            return Err(invalid_input_io(
                "all-zero task id is reserved and cannot own a staging tree",
            ));
        }
        ensure_chunk_size_within_cap(chunk_size)?;
        let parts = checked_rel_components(entry)?;
        if entry.is_dir {
            ensure_real_dirs(&self.root, &parts)?;
            return Ok(LocalFileSink {
                kind: SinkKind::Directory,
            });
        }
        if entry.digest_alg != DIGEST_ALG_BLAKE3 {
            return Err(invalid_input_io("unsupported digest algorithm"));
        }
        let plan = ChunkPlan::new(entry.size, chunk_size);
        if plan.chunk_count() != entry.chunk_digests.len() as u64 {
            return Err(invalid_input_io(
                "entry chunk digests inconsistent with its size and the chunk size",
            ));
        }
        let (name, parents) = parts.split_last().ok_or_else(escape_err)?;
        ensure_real_dirs(&self.root, parents)?;
        let mut final_path = self.root.clone();
        for part in parents {
            final_path.push(part);
        }
        final_path.push(name);
        // Already-committed fast path, identical to the unstaged variant.
        if let Ok(meta) = fs::symlink_metadata(&final_path) {
            if meta.file_type().is_file()
                && meta.len() == entry.size
                && final_matches_digest(&final_path, entry, chunk_size)?
            {
                return Ok(LocalFileSink {
                    kind: SinkKind::Committed {
                        chunk_count: entry.chunk_digests.len() as u64,
                    },
                });
            }
        }
        // The staging part mirrors the entry's relative directory
        // structure inside the task's staging tree, so two files of one
        // task (and two tasks in one root) can never collide. Every level
        // is created through ensure_real_dirs: a planted symlink at
        // `.ak-staging`, at the task level, or at any entry level is
        // refused before anything is written.
        let hex = hex_prefix(&task_id, 16);
        let mut staging_parts: Vec<&OsStr> = vec![OsStr::new(STAGING_DIR), OsStr::new(&hex)];
        staging_parts.extend(parents.iter().copied());
        ensure_real_dirs(&self.root, &staging_parts)?;
        let mut part_path = self.root.clone();
        for part in &staging_parts {
            part_path.push(part);
        }
        part_path.push(name);
        let part_path = with_part_suffix(&part_path);

        // Legacy compat: a pre-staging `.akpart` beside the final name is
        // adopted into this task's staging tree. Only migrate when no
        // staging part exists yet — Windows rename replaces an existing
        // destination, and even on POSIX the in-flight staged part is the
        // fresher one. A legacy leftover that loses the race stays for
        // discard_orphan_parts to reap.
        let legacy = with_part_suffix(&final_path);
        if fs::symlink_metadata(&part_path).is_err() {
            match fs::symlink_metadata(&legacy) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(io::Error::new(
                            ErrorKind::InvalidInput,
                            OpenError::SymlinkComponent(legacy),
                        ));
                    }
                    if meta.file_type().is_file() {
                        fs::rename(&legacy, &part_path)?;
                    }
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        // Re-validated immediately before the open (same residual window
        // as the unstaged path: no openat2/no-follow in std — P1-2).
        ensure_not_symlink(&part_path)?;
        // No truncate: an existing staged part is verified and resumed.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)?;
        let verified_prefix = verify_part_prefix(&mut file, &part_path, entry, chunk_size)?;
        let state = PartState {
            file,
            final_path,
            part_path,
            conflict,
            size: entry.size,
            mtime_unix_ms: entry.mtime_unix_ms,
            chunk_size,
            whole_digest: entry.whole_digest,
            chunk_digests: entry.chunk_digests.clone(),
            verified_prefix,
            finished: false,
            staged: true,
        };
        Ok(LocalFileSink {
            kind: SinkKind::File(state),
        })
    }
}

/// Whether the existing final at `target` is a regular file whose whole
/// BLAKE3 digest equals this staged sink's verified content — the
/// same-content reuse check of the staging path. Never follows a symlink
/// and never allocates more than `min(chunk_size, size)`.
fn staged_target_matches(st: &PartState, target: &Path) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(target) {
        Ok(meta) => meta,
        // Vanished between the existence check and here: not a match; the
        // normal conflict/commit flow re-runs its own checks.
        Err(_) => return Ok(false),
    };
    if !meta.file_type().is_file() || meta.len() != st.size {
        return Ok(false);
    }
    if st.size == 0 {
        return Ok(st.whole_digest == b3(b""));
    }
    let mut file = match File::open(target) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let mut buf = vec![0u8; streaming_buffer_len(st.chunk_size, st.size)];
    Ok(hash_reader(&mut file, &mut buf)? == st.whole_digest)
}

/// Remove one task's whole staging tree `<root>/.ak-staging/<task_hex>/`
/// (cancel path and terminal cleanup helper). Iterative post-order walk,
/// touches nothing outside the task's own subtree, and is idempotent: an
/// absent tree is `Ok(())`. Sibling tasks' trees are untouched.
///
/// Symlink posture (review 2026-11-05, P1-2): the `.ak-staging` PARENT is
/// verified to be a real directory before anything is read — a link
/// planted there would otherwise resolve every path below (including the
/// `top` check, whose `symlink_metadata` only refuses a link at the FINAL
/// component) into an external tree and make the walk delete it. Each
/// entry is classified with `symlink_metadata` and re-classified right
/// before removal; a link found at removal time fails closed instead of
/// being unlinked. Residual window: std has no fd-relative
/// remove_file/remove_dir, so a component swapped between its check and
/// the removal can still be acted on by path — `remove_file` on a
/// symlink removes the link itself (never its target) on both platforms,
/// and `remove_dir` refuses non-empty directories, which bounds a
/// post-check swap to failing or deleting a planted link, not external
/// data. The destructive parent-link case is closed by the check above.
pub fn discard_staging(root: &Path, task_id: [u8; 16]) -> io::Result<()> {
    // The staging PARENT must be a real directory (or absent): a symlink
    // here would redirect the whole walk outside the receive root.
    let staging_root = root.join(STAGING_DIR);
    match fs::symlink_metadata(&staging_root) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    OpenError::SymlinkComponent(staging_root),
                ));
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let top = staging_path(root, task_id);
    let meta = match fs::symlink_metadata(&top) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_dir() {
        return Err(invalid_input_io(format!(
            "staging path is not a directory: {}",
            top.display()
        )));
    }
    // Collect directories pre-order (parent before children), then remove
    // deepest-first; files go as soon as they are seen.
    let mut stack: Vec<PathBuf> = vec![top.clone()];
    let mut dirs: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        for child in fs::read_dir(&dir)? {
            let path = child?.path();
            let file_type = fs::symlink_metadata(&path)?.file_type();
            if file_type.is_dir() {
                stack.push(path);
            } else {
                // Re-classify immediately before the removal; a link that
                // appeared (or was swapped in) since classification fails
                // closed instead of being unlinked (P1-2).
                ensure_not_symlink(&path)?;
                fs::remove_file(&path)?;
            }
        }
    }
    for dir in dirs.into_iter().rev() {
        // A directory swapped for a symlink after classification must not
        // be removed by path: remove_dir on a Windows dir-symlink removes
        // the link (POSIX rmdir refuses); both are non-destructive, but
        // refusing is the honest fail-closed choice given the swap.
        ensure_not_symlink(&dir)?;
        fs::remove_dir(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ak_core::manifest::DIGEST_LEN;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::TempDir;

    fn cs(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn wf(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    fn mtime_ms(path: &Path) -> i64 {
        unix_ms(fs::symlink_metadata(path).unwrap().modified().unwrap())
    }

    /// Process-wide counter of symlink-dependent assertions the OS could
    /// not set up (typically Windows without the symlink privilege).
    /// Counting makes the reduction visible instead of silent (review
    /// 2026-11-04, test honesty note): each skip prints a numbered SKIP
    /// line in the test output (run with `--nocapture` to always see it).
    static SYMLINK_SKIPS: AtomicUsize = AtomicUsize::new(0);

    /// Create a symlink; on Windows this needs the symlink privilege. Returns
    /// false (and prints why, counted) when the OS refuses, so tests skip
    /// honestly — the SKIP line is printed and counted, never silent.
    fn make_symlink(src: &Path, dst: &Path, dir_link: bool) -> bool {
        match symlink_once(src, dst, dir_link) {
            Ok(()) => true,
            Err(e) => {
                let n = SYMLINK_SKIPS.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!(
                    "SKIP #{n}: cannot create symlink at {} ({e}); \
                     this assertion did NOT run — test reduced",
                    dst.display()
                );
                false
            }
        }
    }

    #[cfg(windows)]
    fn symlink_once(src: &Path, dst: &Path, dir_link: bool) -> io::Result<()> {
        if dir_link {
            std::os::windows::fs::symlink_dir(src, dst)
        } else {
            std::os::windows::fs::symlink_file(src, dst)
        }
    }

    #[cfg(unix)]
    fn symlink_once(src: &Path, dst: &Path, _dir_link: bool) -> io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }

    // -----------------------------------------------------------------------
    // Source scanning
    // -----------------------------------------------------------------------

    #[test]
    fn scan_single_file_builds_manifest_matching_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("single.txt");
        let content = b"hello, local source!";
        wf(&path, content);

        let source = LocalSource::open(&path, cs(8)).unwrap();
        assert_eq!(source.root(), path);
        assert_eq!(source.root_name(), "single.txt");

        let manifest = source.manifest();
        assert_eq!(manifest.entries.len(), 1);
        let entry = &manifest.entries[0];
        assert!(!entry.is_dir);
        assert_eq!(entry.rel_path, PathBuf::from("single.txt"));
        assert_eq!(entry.size, content.len() as u64);
        assert_eq!(entry.mtime_unix_ms, mtime_ms(&path));
        assert_eq!(entry.chunk_digests.len(), 3);
        assert_eq!(entry.chunk_digests[0], b3(b"hello, l"));
        assert_eq!(entry.chunk_digests[1], b3(b"ocal sou"));
        assert_eq!(entry.chunk_digests[2], b3(b"rce!"));
        assert_eq!(entry.whole_digest, b3(content));
        assert_eq!(manifest.total_bytes(), content.len() as u64);
    }

    #[test]
    fn scan_nested_tree_orders_parent_first_and_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        wf(&root.join("a/b/deep.txt"), b"d6");
        wf(&root.join("a/z.txt"), b"az");
        wf(&root.join("multi1.txt"), b"m1");
        wf(&root.join("multi2.txt"), b"m2");

        let source = LocalSource::open(&root, cs(4)).unwrap();
        assert_eq!(source.root_name(), "src");
        let entries = &source.manifest().entries;
        let rels: Vec<&str> = entries
            .iter()
            .map(|e| e.rel_path.to_str().unwrap())
            .collect();
        assert_eq!(
            rels,
            [
                "a",
                "a/b",
                "a/b/deep.txt",
                "a/z.txt",
                "empty",
                "multi1.txt",
                "multi2.txt"
            ]
        );
        let dirs: Vec<bool> = entries.iter().map(|e| e.is_dir).collect();
        assert_eq!(dirs, [true, true, false, false, true, false, false]);

        let deep = &entries[2];
        assert_eq!(deep.size, 2);
        assert_eq!(deep.chunk_digests, vec![b3(b"d6")]);
        assert_eq!(deep.whole_digest, b3(b"d6"));
        assert_eq!(deep.mtime_unix_ms, mtime_ms(&root.join("a/b/deep.txt")));

        let empty = &entries[4];
        assert!(empty.is_dir && empty.chunk_digests.is_empty() && empty.size == 0);

        // Deterministic order on repeated scans.
        let again = LocalSource::open(&root, cs(4)).unwrap();
        assert_eq!(source.manifest(), again.manifest());
    }

    #[test]
    fn scan_handles_two_hundred_level_deep_chain() {
        let tmp = TempDir::new().unwrap();
        // Canonicalize: on Windows the returned \\?\ verbatim path lifts the
        // 260-char MAX_PATH limit, which a 200-level chain of 3-char names
        // would otherwise exceed.
        let root = tmp.path().canonicalize().unwrap();
        let names: Vec<String> = (0..200).map(|i| format!("d{:02}", i % 100)).collect();
        let mut deep = root.clone();
        for name in &names {
            deep.push(name);
            fs::create_dir(&deep).unwrap();
        }
        wf(&deep.join("leaf.txt"), b"leaf");

        let source = LocalSource::open(&root, cs(4)).unwrap();
        let entries = &source.manifest().entries;
        assert_eq!(entries.len(), 201);
        // Parents strictly before children, in chain order.
        let mut prefix = String::new();
        for (i, entry) in entries.iter().enumerate().take(names.len()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&names[i]);
            assert!(entry.is_dir);
            assert_eq!(entry.rel_path, PathBuf::from(&prefix));
        }
        let leaf = &entries[names.len()];
        assert!(!leaf.is_dir);
        assert_eq!(leaf.rel_path, PathBuf::from(format!("{prefix}/leaf.txt")));
        assert_eq!(leaf.whole_digest, b3(b"leaf"));
        assert!(source.verify_stable().is_ok());

        // Unwind deepest-first so TempDir cleanup stays within normal paths.
        fs::remove_file(deep.join("leaf.txt")).unwrap();
        while deep != root {
            fs::remove_dir(&deep).unwrap();
            deep.pop();
        }
    }

    #[test]
    fn scan_empty_root_dir_yields_no_entries() {
        let tmp = TempDir::new().unwrap();
        let source = LocalSource::open(tmp.path(), cs(8)).unwrap();
        assert!(source.manifest().entries.is_empty());
        assert_eq!(source.manifest().file_count(), 0);
        assert!(source.verify_stable().is_ok());
    }

    #[test]
    fn scan_missing_root_and_empty_file_edge_cases() {
        let tmp = TempDir::new().unwrap();
        let err = LocalSource::open(&tmp.path().join("nope"), cs(8))
            .err()
            .unwrap();
        assert!(matches!(err, OpenError::NotFound(_)));

        let empty = tmp.path().join("empty.bin");
        wf(&empty, b"");
        let source = LocalSource::open(&empty, cs(8)).unwrap();
        let entry = &source.manifest().entries[0];
        assert_eq!(entry.size, 0);
        assert!(entry.chunk_digests.is_empty());
        assert_eq!(entry.whole_digest, b3(b""));
    }

    // -----------------------------------------------------------------------
    // read_chunk / verify_stable
    // -----------------------------------------------------------------------

    #[test]
    fn read_chunk_covers_first_middle_last_and_short_tail() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.bin");
        wf(&path, b"0123456789");
        let source = LocalSource::open(&path, cs(4)).unwrap();

        let mut buf = [0u8; 8];
        assert_eq!(source.read_chunk(0, 0, &mut buf).unwrap(), 4);
        assert_eq!(&buf[..4], b"0123");
        assert_eq!(source.read_chunk(0, 1, &mut buf).unwrap(), 4);
        assert_eq!(&buf[..4], b"4567");
        buf.fill(0xAA);
        assert_eq!(source.read_chunk(0, 2, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"89");
        assert_eq!(buf[2], 0xAA, "short read must not touch the tail");

        // Exact-size destination works for the short final chunk.
        let mut tail = [0u8; 2];
        assert_eq!(source.read_chunk(0, 2, &mut tail).unwrap(), 2);
        assert_eq!(&tail, b"89");
    }

    #[test]
    fn read_chunk_rejects_out_of_range_indices_and_small_buffers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.bin");
        wf(&path, b"0123456789");
        let source = LocalSource::open(&path, cs(4)).unwrap();

        assert!(source.read_chunk(1, 0, &mut [0u8; 4]).is_err());
        assert!(source.read_chunk(0, 3, &mut [0u8; 4]).is_err());
        assert!(source.read_chunk(0, 0, &mut [0u8; 3]).is_err());

        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("d")).unwrap();
        wf(&tree.join("d/x"), b"x");
        wf(&tree.join("f"), b"x");
        let dir_source = LocalSource::open(&tree, cs(4)).unwrap();
        assert_eq!(
            dir_source.manifest().entries[0].rel_path,
            PathBuf::from("d")
        );
        assert!(
            dir_source.read_chunk(0, 0, &mut [0u8; 4]).is_err(),
            "dir entry is not readable"
        );
        assert!(dir_source.read_chunk(1, 0, &mut [0u8; 4]).is_ok());

        let empty = tmp.path().join("empty.bin");
        wf(&empty, b"");
        let empty_source = LocalSource::open(&empty, cs(4)).unwrap();
        assert!(
            empty_source.read_chunk(0, 0, &mut [0u8; 4]).is_err(),
            "empty file has no chunks"
        );
    }

    #[test]
    fn verify_stable_detects_size_and_mtime_drift_and_missing_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("watched.bin");
        wf(&path, b"01234567");
        let source = LocalSource::open(&path, cs(4)).unwrap();
        assert!(source.verify_stable().is_ok());

        // Size drift.
        sleep(Duration::from_millis(20));
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"89").unwrap();
        drop(f);
        assert_eq!(source.verify_stable(), Err(ErrorCode::SourceChanged));

        // Size restored; mtime alone still differs.
        sleep(Duration::from_millis(20));
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(8).unwrap();
        drop(f);
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.len(), 8);
        assert_eq!(source.verify_stable(), Err(ErrorCode::SourceChanged));

        // Missing file.
        fs::remove_file(&path).unwrap();
        assert_eq!(source.verify_stable(), Err(ErrorCode::SourceChanged));
    }

    #[test]
    fn verify_stable_also_covers_nested_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        fs::create_dir_all(root.join("d")).unwrap();
        wf(&root.join("d/x"), b"xx");
        wf(&root.join("top"), b"t");
        let source = LocalSource::open(&root, cs(4)).unwrap();
        assert!(source.verify_stable().is_ok());
        sleep(Duration::from_millis(20));
        wf(&root.join("d/x"), b"yy"); // same size, new mtime
        assert_eq!(source.verify_stable(), Err(ErrorCode::SourceChanged));
    }

    // -----------------------------------------------------------------------
    // Save target open
    // -----------------------------------------------------------------------

    #[test]
    fn save_target_open_rejects_missing_file_and_symlink_roots() {
        let tmp = TempDir::new().unwrap();
        let err = LocalSaveTarget::open(&tmp.path().join("missing"))
            .err()
            .unwrap();
        assert!(matches!(err, OpenError::NotFound(_)));

        let plain = tmp.path().join("plain.txt");
        wf(&plain, b"x");
        let err = LocalSaveTarget::open(&plain).err().unwrap();
        assert!(matches!(err, OpenError::NotDirectory(_)));

        let real = tmp.path().join("real_dir");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("root_link");
        if !make_symlink(&real, &link, true) {
            return;
        }
        let err = LocalSaveTarget::open(&link).err().unwrap();
        assert!(matches!(err, OpenError::RootIsSymlink(_)));

        let target = LocalSaveTarget::open(&real).unwrap();
        assert_eq!(target.root(), real.canonicalize().unwrap());
        assert!(target.available_bytes().is_some());
    }

    #[test]
    fn available_bytes_reports_positive_free_space_for_real_volume() {
        let tmp = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(tmp.path()).unwrap();
        assert!(
            target.available_bytes().is_some_and(|free| free > 0),
            "a mounted temp volume must report positive free space"
        );
    }

    // -----------------------------------------------------------------------
    // Sinks: write/finish/conflict
    // -----------------------------------------------------------------------

    fn transfer_entry(content: &[u8], chunk: u32) -> (TempDir, LocalSource, FileEntry) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("payload.bin");
        wf(&path, content);
        let source = LocalSource::open(&path, cs(chunk)).unwrap();
        let entry = source.manifest().entries[0].clone();
        (tmp, source, entry)
    }

    fn write_all_chunks(sink: &mut dyn FileSink, source: &LocalSource, entry: &FileEntry) {
        for i in 0..entry.chunk_digests.len() as u64 {
            let range = ChunkPlan::new(entry.size, source.manifest().chunk_size)
                .range(i)
                .unwrap();
            let len = usize::try_from(range.end - range.start).unwrap();
            let mut buf = vec![0u8; len];
            assert_eq!(source.read_chunk(0, i, &mut buf).unwrap(), len);
            sink.write_chunk(i, &buf).unwrap();
        }
    }

    #[test]
    fn write_all_chunks_then_finish_commits_identical_file() {
        let content = b"abcdefgh12345678@@"; // 18 bytes, chunk 8 -> 8+8+2
        let (_src_tmp, source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();

        let final_path = dst.path().join("payload.bin");
        let part_path = dst.path().join("payload.bin.akpart");
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 0);
        write_all_chunks(&mut sink, &source, &entry);
        assert_eq!(sink.verified_prefix_chunks(), 3);
        // Idempotent replay of an earlier chunk changes nothing.
        sink.write_chunk(0, b"abcdefgh").unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 3);
        sink.finish().unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), content);
        assert!(!part_path.exists(), "part file must disappear after finish");
    }

    #[test]
    fn finish_preserves_source_mtime_on_committed_file() {
        let content = b"mtime-payload";
        let src_tmp = TempDir::new().unwrap();
        let src_path = src_tmp.path().join("payload.bin");
        wf(&src_path, content);
        // A distinctive time one day in the past: a default "now" stamp on
        // the receiver would not pass the comparison below.
        let past = unix_ms(SystemTime::now()) - 86_400_000;
        filetime::set_file_mtime(&src_path, mtime_file_time(past)).unwrap();

        let source = LocalSource::open(&src_path, cs(8)).unwrap();
        let entry = source.manifest().entries[0].clone();
        assert!(
            (entry.mtime_unix_ms - past).abs() < 2_000,
            "source mtime should round-trip within fs precision"
        );

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink, &source, &entry);
        sink.finish().unwrap();

        let final_path = dst.path().join("payload.bin");
        assert_eq!(fs::read(&final_path).unwrap(), content);
        let got = mtime_ms(&final_path);
        assert!(
            (got - entry.mtime_unix_ms).abs() < 2_000,
            "final mtime {got} must match manifest {} within fs precision",
            entry.mtime_unix_ms
        );
    }

    #[test]
    fn write_chunk_out_of_order_is_rejected() {
        let (_src_tmp, _source, entry) = transfer_entry(b"abcdefgh12345678", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        let err = sink.write_chunk(1, b"12345678").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn chunk_digest_mismatch_fails_without_storing_bad_bytes() {
        let content = b"abcdefgh12345678";
        let (_src_tmp, _source, mut entry) = transfer_entry(content, 8);
        entry.chunk_digests[1] = [0xFF; DIGEST_LEN]; // attacker-tampered manifest

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");
        let part_path = dst.path().join("payload.bin.akpart");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.write_chunk(0, b"abcdefgh").unwrap();
        let err = sink.write_chunk(1, b"12345678").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("HASH_MISMATCH"));
        assert_eq!(
            fs::metadata(&part_path).unwrap().len(),
            8,
            "bad chunk must not touch the disk"
        );
        let err = sink.finish().unwrap_err(); // incomplete
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(!final_path.exists(), "final must not appear");
        assert!(part_path.exists());
    }

    #[test]
    fn whole_digest_mismatch_keeps_part_and_blocks_final() {
        let content = b"abcdefgh12345678";
        let (_src_tmp, source, mut entry) = transfer_entry(content, 8);
        entry.whole_digest = [0xEE; DIGEST_LEN]; // per-chunk digests still valid

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");
        let part_path = dst.path().join("payload.bin.akpart");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink, &source, &entry);
        let err = sink.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("HASH_MISMATCH"));
        assert!(!final_path.exists());
        assert_eq!(
            fs::read(&part_path).unwrap(),
            content,
            "verified prefix stays reusable"
        );
    }

    #[test]
    fn empty_file_commits_through_finish_only() {
        let (_src_tmp, _source, entry) = transfer_entry(b"", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 0);
        sink.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), Vec::<u8>::new());
        assert!(!dst.path().join("payload.bin.akpart").exists());
    }

    #[test]
    fn conflict_fail_errors_and_rename_generates_free_name() {
        // Two entries for the SAME final name with DIFFERENT content: a
        // real conflict. (An identical-content retry is no longer a
        // conflict: prepare detects the committed final and succeeds
        // idempotently — see
        // prepare_after_commit_returns_completed_sink_without_rewriting.)
        let content_a = b"abcdefgh12345678";
        let content_b = b"ZZZZZZZZYYYYYYYY"; // same size, different bytes
        let (_src_tmp_a, source_a, entry_a) = transfer_entry(content_a, 8);
        let (_src_tmp_b, source_b, entry_b) = transfer_entry(content_b, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        let mut first = target
            .prepare_file_with_chunk_size(&entry_a, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut first, &source_a, &entry_a);
        first.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), content_a);

        // Second transfer with different bytes: conflict under Fail.
        let mut second = target
            .prepare_file_with_chunk_size(&entry_b, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(second.verified_prefix_chunks(), 0);
        write_all_chunks(&mut second, &source_b, &entry_b);
        let err = second.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(
            dst.path().join("payload.bin.akpart").exists(),
            "part kept for retry"
        );
        drop(second);

        // Retry with Rename: the part resumes fully verified and lands as (2).
        let mut third = target
            .prepare_file_with_chunk_size(&entry_b, ConflictPolicy::Rename, cs(8))
            .unwrap();
        assert_eq!(
            third.verified_prefix_chunks(),
            2,
            "resume from the kept part"
        );
        third.finish().unwrap();
        assert_eq!(
            fs::read(dst.path().join("payload (2).bin")).unwrap(),
            content_b
        );
        assert_eq!(
            fs::read(&final_path).unwrap(),
            content_a,
            "original untouched"
        );
        assert!(!dst.path().join("payload.bin.akpart").exists());
    }

    #[test]
    fn rename_conflict_in_deep_dir_handles_multi_dot_extension() {
        let content = b"deep-archive-bytes";
        let (_src_tmp, source, entry) = transfer_entry(content, 8);
        let mut deep = entry.clone();
        deep.rel_path = PathBuf::from("l1/l2/l3/l4/a.tar.gz");

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let dir = dst.path().join("l1/l2/l3/l4");
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("a.tar.gz");
        wf(&existing, b"old-archive");

        let mut sink = target
            .prepare_file_with_chunk_size(&deep, ConflictPolicy::Rename, cs(8))
            .unwrap();
        write_all_chunks(&mut sink, &source, &deep);
        sink.finish().unwrap();

        // Multi-dot names rename on the last extension only: a.tar.gz ->
        // a.tar (2).gz, inside the same deep directory.
        assert_eq!(fs::read(dir.join("a.tar (2).gz")).unwrap(), content);
        assert_eq!(fs::read(&existing).unwrap(), b"old-archive");
        assert!(!dir.join("a.tar.gz.akpart").exists());
    }

    #[test]
    fn conflict_free_name_increments_from_two_and_handles_extensions() {
        let tmp = TempDir::new().unwrap();
        let name = tmp.path().join("a.tar.gz");
        assert_eq!(
            conflict_free_name(&name).file_name().unwrap(),
            "a.tar (2).gz"
        );
        wf(&tmp.path().join("a.tar (2).gz"), b"x");
        assert_eq!(
            conflict_free_name(&name).file_name().unwrap(),
            "a.tar (3).gz"
        );

        let plain = tmp.path().join("README");
        assert_eq!(
            conflict_free_name(&plain).file_name().unwrap(),
            "README (2)"
        );
        assert_eq!(
            conflict_free_name(Path::new("rel.txt"))
                .file_name()
                .unwrap(),
            "rel (2).txt"
        );
    }

    // -----------------------------------------------------------------------
    // Defensive chunk-size cap and bounded buffers (review 2026-11-04 #1)
    // -----------------------------------------------------------------------

    #[test]
    fn prepare_rejects_chunk_size_above_the_cap_without_allocating() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        // Size 0 with zero chunk digests is plan-consistent even for a
        // giant chunk size: exactly the review's small-frame trigger shape.
        let entry = FileEntry::new("huge.bin", 0, 0, b3(b""), Vec::new()).unwrap();

        // NonZeroU32::MAX (~4 GiB) must fail fast with InvalidInput instead
        // of attempting the allocation: the immediate Err is the observable
        // proof the cap ran first (an attempted OOM would abort instead).
        let err = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, NonZeroU32::MAX)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(err.to_string().contains("cap"), "got: {err}");

        // One byte over the cap is rejected as well...
        let over = NonZeroU32::new((MAX_CHUNK_BUFFER_BYTES + 1) as u32).unwrap();
        let err = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, over)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        // ...while exactly the cap is accepted; the empty entry commits with
        // no chunk buffer allocated at any point.
        let boundary = NonZeroU32::new(MAX_CHUNK_BUFFER_BYTES as u32).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, boundary)
            .unwrap();
        sink.finish().unwrap();
        assert_eq!(
            fs::read(dst.path().join("huge.bin")).unwrap(),
            Vec::<u8>::new()
        );

        // Send side carries the same defense at API entry.
        let src_file = dst.path().join("src.bin");
        wf(&src_file, b"x");
        let err = LocalSource::open(&src_file, NonZeroU32::MAX).err().unwrap();
        assert!(
            matches!(err, OpenError::Io(ref e) if e.kind() == ErrorKind::InvalidInput),
            "got: {err:?}"
        );
    }

    #[test]
    fn short_file_under_large_chunk_size_round_trips_with_clamped_buffer() {
        // A 10-byte file under a 1 MiB (valid, in-cap) chunk plan: the
        // streaming buffers are clamped to min(chunk_size, size) = 10, and
        // the tiny file still round-trips through prepare/write/finish.
        let content = b"0123456789";
        let (_src_tmp, source, entry) = transfer_entry(content, 1 << 20);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(1 << 20))
            .unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 0);
        write_all_chunks(&mut sink, &source, &entry);
        sink.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("payload.bin")).unwrap(), content);
    }

    // -----------------------------------------------------------------------
    // Atomic no-replace commit (review 2026-11-04, storage item 2)
    // -----------------------------------------------------------------------

    #[test]
    fn commit_no_replace_moves_part_and_never_overwrites() {
        let tmp = TempDir::new().unwrap();
        let part = tmp.path().join("f.bin.akpart");
        let target = tmp.path().join("f.bin");
        wf(&part, b"new-content");

        // Fresh target: commit succeeds, content lands, part name is gone.
        commit_no_replace(&part, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new-content");
        assert!(!part.exists());

        // Existing target: AlreadyExists, target untouched, part kept with
        // its data. Both platform branches go through hard_link (Windows
        // only falls back to rename on link-less filesystems, which never
        // happens on NTFS temp dirs), so this contract is exercised on
        // whichever platform the suite runs.
        wf(&part, b"newer-content");
        let err = commit_no_replace(&part, &target).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"new-content");
        assert_eq!(fs::read(&part).unwrap(), b"newer-content");
    }

    #[cfg(windows)]
    #[test]
    fn windows_rename_replaces_existing_files_is_locked() {
        // Locks the OBSERVED std behavior on this toolchain (rustc 1.99):
        // std::fs::rename on Windows uses FileRenameInformationEx with
        // replace-if-exists semantics, so it REPLACES an existing target
        // instead of failing like the historical MoveFileExW path. This is
        // exactly why commit_no_replace cannot use rename on Windows
        // either; if this test ever fails with AlreadyExists again, the
        // std behavior changed back and commit_no_replace's Windows
        // fallback rationale needs revisiting.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        wf(&a, b"a");
        wf(&b, b"b");
        fs::rename(&a, &b).expect("rename replaces existing targets here");
        assert_eq!(fs::read(&b).unwrap(), b"a", "target was replaced");
        assert!(!a.exists(), "source name is gone");
    }

    #[test]
    fn hard_link_onto_existing_name_fails_natively() {
        // Locks the platform assumption behind BOTH branches of
        // commit_no_replace: link(2) / CreateHardLinkW onto an existing
        // name fails with AlreadyExists instead of replacing it. Runs on
        // Windows and Unix alike.
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        wf(&a, b"a");
        wf(&b, b"b");
        let err = fs::hard_link(&a, &b).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"b", "target untouched");
        assert_eq!(fs::read(&a).unwrap(), b"a", "source untouched");
    }

    // -----------------------------------------------------------------------
    // Already-committed reuse (review 2026-11-04, storage item 3)
    // -----------------------------------------------------------------------

    #[test]
    fn prepare_after_commit_returns_completed_sink_without_rewriting() {
        let content = b"0123456789abcdefGHIJ"; // 20 bytes, chunk 8 -> 8+8+4
        let (_src_tmp, source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");
        let part_path = dst.path().join("payload.bin.akpart");

        // Full transfer, committed; the part name is gone.
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink, &source, &entry);
        sink.finish().unwrap();
        assert!(!part_path.exists());

        // Distinctive old mtime: any rewrite during the retry would move it.
        let past = unix_ms(SystemTime::now()) - 86_400_000;
        filetime::set_file_mtime(&final_path, mtime_file_time(past)).unwrap();
        let mtime_before = mtime_ms(&final_path);

        // Engine retry: part deleted, final present and digest-matching.
        let mut resumed = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(
            resumed.verified_prefix_chunks(),
            entry.chunk_digests.len() as u64,
            "already-committed final reports every chunk verified"
        );
        // Chunk replay is an idempotent no-op; out-of-range still errors.
        resumed.write_chunk(0, b"01234567").unwrap();
        assert!(resumed.write_chunk(99, b"x").is_err());
        resumed.finish().unwrap();
        resumed.finish().unwrap(); // idempotent success

        assert_eq!(fs::read(&final_path).unwrap(), content);
        assert_eq!(mtime_ms(&final_path), mtime_before, "final not rewritten");
        assert!(!part_path.exists(), "no part may be recreated");

        // Same fast path under the Rename policy: no " (2)" duplicate may
        // be generated against the transfer's own committed output.
        let again = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Rename, cs(8))
            .unwrap();
        assert_eq!(
            again.verified_prefix_chunks(),
            entry.chunk_digests.len() as u64
        );
        drop(again);
        assert!(!dst.path().join("payload (2).bin").exists());

        // Empty file: committed detection works with zero chunks too, and
        // no part file is created for the completed sink.
        let empty_entry = FileEntry::new("empty.bin", 0, 0, b3(b""), Vec::new()).unwrap();
        let mut first = target
            .prepare_file_with_chunk_size(&empty_entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        first.finish().unwrap();
        let mut second = target
            .prepare_file_with_chunk_size(&empty_entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(second.verified_prefix_chunks(), 0, "zero chunks, all done");
        second.finish().unwrap();
        assert!(!dst.path().join("empty.bin.akpart").exists());
    }

    #[test]
    fn final_with_different_content_is_still_a_conflict_not_a_match() {
        let content = b"0123456789abcdefGHIJ";
        let (_src_tmp, source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        // Same length (20 bytes) but different bytes: the length pre-check
        // passes and only the whole-digest comparison rejects the match.
        wf(&final_path, b"XXXXXXXXXXXXXXXXXXXX");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(
            sink.verified_prefix_chunks(),
            0,
            "no fast path: content differs"
        );
        write_all_chunks(&mut sink, &source, &entry);
        let err = sink.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(&final_path).unwrap(),
            b"XXXXXXXXXXXXXXXXXXXX",
            "pre-existing file untouched"
        );
    }

    // -----------------------------------------------------------------------
    // Resume
    // -----------------------------------------------------------------------

    #[test]
    fn dropped_sink_resumes_from_verified_prefix() {
        let content = b"0123456789abcdefGHIJ"; // 20 bytes, chunk 8 -> 8+8+4
        let (_src_tmp, _source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink); // no discard: the part survives for resume

        let mut resumed = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 2);
        // Wrong content for the next chunk is rejected without advancing.
        assert!(resumed.write_chunk(2, b"XXXX").is_err());
        assert_eq!(resumed.verified_prefix_chunks(), 2);
        resumed.write_chunk(2, b"GHIJ").unwrap();
        resumed.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), content);
    }

    #[test]
    fn corrupted_part_prefix_truncates_and_retransmits() {
        let content = b"0123456789abcdefGHIJ";
        let (_src_tmp, _source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let part_path = dst.path().join("payload.bin.akpart");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink);

        // Corrupt byte inside chunk 1 (offset 10).
        let mut part = fs::OpenOptions::new().write(true).open(&part_path).unwrap();
        part.seek(SeekFrom::Start(10)).unwrap();
        part.write_all(&[0x00]).unwrap();
        drop(part);

        let mut resumed = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(
            resumed.verified_prefix_chunks(),
            1,
            "chunk 1 no longer verifies"
        );
        assert_eq!(
            part_path.metadata().unwrap().len(),
            8,
            "truncated at the bad chunk"
        );
        resumed.write_chunk(1, b"89abcdef").unwrap();
        resumed.write_chunk(2, b"GHIJ").unwrap();
        resumed.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("payload.bin")).unwrap(), content);
    }

    #[test]
    fn part_longer_than_expected_is_truncated_on_prepare() {
        let content = b"0123456789abcdef";
        let (_src_tmp, _source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let part_path = dst.path().join("payload.bin.akpart");

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink);
        let mut part = fs::OpenOptions::new()
            .append(true)
            .open(&part_path)
            .unwrap();
        part.write_all(b"garbage").unwrap();
        drop(part);

        let resumed = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 2);
        assert_eq!(
            part_path.metadata().unwrap().len(),
            16,
            "stale tail dropped"
        );
    }

    #[test]
    fn trait_prepare_resumes_with_default_chunk_size() {
        let head = vec![0xA5u8; default_chunk_size().get() as usize];
        let tail = vec![0x5Au8; default_chunk_size().get() as usize / 2];
        let mut content = head.clone();
        content.extend_from_slice(&tail);

        let src = TempDir::new().unwrap();
        let path = src.path().join("big.bin");
        wf(&path, &content);
        let source = LocalSource::open(&path, default_chunk_size()).unwrap();
        assert_eq!(source.manifest().chunk_size, default_chunk_size());
        let entry = source.manifest().entries[0].clone();

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("big.bin");

        // Write chunk 0 through the trait method (default chunk size).
        let mut sink = target.prepare_file(&entry, ConflictPolicy::Fail).unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 0);
        let mut buf = vec![0u8; default_chunk_size().get() as usize];
        assert_eq!(source.read_chunk(0, 0, &mut buf).unwrap(), buf.len());
        sink.write_chunk(0, &buf).unwrap();
        drop(sink);

        let mut resumed = target.prepare_file(&entry, ConflictPolicy::Fail).unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 1);
        let half = default_chunk_size().get() as usize / 2;
        assert_eq!(source.read_chunk(0, 1, &mut buf[..half]).unwrap(), half);
        resumed.write_chunk(1, &buf[..half]).unwrap();
        resumed.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), content);
    }

    #[test]
    fn discard_removes_the_part_file() {
        let (_src_tmp, _source, entry) = transfer_entry(b"0123456789", 4);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let part_path = dst.path().join("payload.bin.akpart");
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4))
            .unwrap();
        sink.write_chunk(0, b"0123").unwrap();
        assert!(part_path.exists());
        sink.discard();
        assert!(!part_path.exists());
        assert!(!dst.path().join("payload.bin").exists());
    }

    #[test]
    fn discard_orphan_parts_removes_only_stale_parts() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        let stale = tmp.path().join("stale.bin.akpart");
        let fresh = tmp.path().join("fresh.bin.akpart");
        let nested_stale = tmp.path().join("sub/nested.bin.akpart");
        let old_plain = tmp.path().join("old-regular.txt");
        wf(&stale, b"s");
        wf(&fresh, b"f");
        wf(&nested_stale, b"n");
        wf(&old_plain, b"o");
        // One hour old (fresh parts keep their just-written mtime).
        let hour_ago = mtime_file_time(unix_ms(SystemTime::now()) - 3_600_000);
        filetime::set_file_mtime(&stale, hour_ago).unwrap();
        filetime::set_file_mtime(&nested_stale, hour_ago).unwrap();
        filetime::set_file_mtime(&old_plain, hour_ago).unwrap();

        let removed = discard_orphan_parts(tmp.path(), Duration::from_secs(600)).unwrap();
        assert_eq!(removed, 2, "only the two stale parts");
        assert!(!stale.exists());
        assert!(!nested_stale.exists());
        assert!(fresh.exists(), "recent part survives");
        assert!(old_plain.exists(), "non-part files are never touched");
    }

    // -----------------------------------------------------------------------
    // Path safety: symlinks and escapes
    // -----------------------------------------------------------------------

    #[test]
    fn source_tree_containing_symlink_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        fs::create_dir_all(&root).unwrap();
        wf(&root.join("f.txt"), b"x");
        let outside = tmp.path().join("outside.txt");
        wf(&outside, b"target");
        if !make_symlink(&outside, &root.join("link"), false) {
            return;
        }
        let err = LocalSource::open(&root, cs(4)).err().unwrap();
        assert!(matches!(err, OpenError::SymlinkComponent(_)));
    }

    #[test]
    fn symlink_component_inside_save_path_is_rejected() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let outside = TempDir::new().unwrap();
        let link = dst.path().join("link");
        if !make_symlink(outside.path(), &link, true) {
            return;
        }

        // File entry below the symlinked component.
        let entry = FileEntry::new(
            "link/file.txt",
            1,
            0,
            [0; DIGEST_LEN],
            vec![[0; DIGEST_LEN]],
        )
        .unwrap();
        let err = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            outside.path().read_dir().unwrap().next().is_none(),
            "nothing written through the link"
        );

        // Directory entry below the symlinked component.
        let dir_entry = FileEntry::directory("link/sub").unwrap();
        assert!(target
            .prepare_file_with_chunk_size(&dir_entry, ConflictPolicy::Fail, cs(4))
            .is_err());
        assert!(outside.path().read_dir().unwrap().next().is_none());

        // A file entry whose final name IS the symlink: conflict, never a
        // write through the link.
        let digest = b3(b"x");
        let entry = FileEntry::new("link", 1, 0, digest, vec![digest]).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(1))
            .unwrap();
        sink.write_chunk(0, b"x").unwrap();
        let err = sink.finish().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(outside.path().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn hand_built_escaping_entries_are_rejected() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let digest = b3(b"x");

        let mut escape = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        escape.rel_path = PathBuf::from("../evil.txt");
        assert!(target
            .prepare_file_with_chunk_size(&escape, ConflictPolicy::Fail, cs(4))
            .is_err());

        let mut absolute = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        absolute.rel_path = if cfg!(windows) {
            PathBuf::from("C:\\evil.txt")
        } else {
            PathBuf::from("/etc/evil.txt")
        };
        assert!(target
            .prepare_file_with_chunk_size(&absolute, ConflictPolicy::Fail, cs(4))
            .is_err());

        let mut rooted = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        rooted.rel_path = PathBuf::from("/evil.txt");
        assert!(target
            .prepare_file_with_chunk_size(&rooted, ConflictPolicy::Fail, cs(4))
            .is_err());

        let mut empty = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        empty.rel_path = PathBuf::new();
        assert!(target
            .prepare_file_with_chunk_size(&empty, ConflictPolicy::Fail, cs(4))
            .is_err());

        assert!(!dst.path().join("ok.txt.akpart").exists());
        assert!(!dst.path().parent().unwrap().join("evil.txt").exists());
    }

    #[test]
    fn directory_entry_creates_levels_and_finish_is_noop() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let entry = FileEntry::directory("a/b/c").unwrap();

        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4))
            .unwrap();
        assert!(dst.path().join("a/b/c").is_dir());
        assert_eq!(sink.verified_prefix_chunks(), 0);
        assert!(sink.write_chunk(0, b"x").is_err());
        sink.finish().unwrap();
        sink.discard(); // no-op for directories
        assert!(dst.path().join("a/b/c").is_dir());

        // Preparing the same directory entry again is an idempotent no-op.
        let mut again = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Rename, cs(4))
            .unwrap();
        again.finish().unwrap();
        assert!(dst.path().join("a/b/c").is_dir());
    }

    #[test]
    fn file_entry_creates_missing_parent_directories_safely() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let digest = b3(b"xy");
        let entry = FileEntry::new("docs/2026/note.txt", 2, 0, digest, vec![digest]).unwrap();
        let mut sink = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(2))
            .unwrap();
        sink.write_chunk(0, b"xy").unwrap();
        sink.finish().unwrap();
        assert_eq!(
            fs::read(dst.path().join("docs/2026/note.txt")).unwrap(),
            b"xy"
        );
    }

    // -----------------------------------------------------------------------
    // Per-task staging namespace (wave 20)
    // -----------------------------------------------------------------------

    const TASK_A: [u8; 16] = [0xAA, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0xBB];
    const TASK_B: [u8; 16] = [0xCC, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0xDD];

    #[test]
    fn staging_path_is_per_task_and_hex_encoded() {
        let root = Path::new("/dest");
        let path = staging_path(root, TASK_A);
        // Tail-component comparison keeps the assertion separator-agnostic.
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "aa0102030405060708090a0b0c0d0ebb"
        );
        assert_eq!(
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            STAGING_DIR
        );
        assert_eq!(
            path.parent().unwrap().parent().unwrap(),
            root,
            "staging tree lives directly under the destination root"
        );
        // Deterministic per id, distinct across ids.
        assert_eq!(staging_path(root, TASK_A), staging_path(root, TASK_A));
        assert_ne!(staging_path(root, TASK_A), staging_path(root, TASK_B));
        assert_eq!(
            staging_path(root, [0u8; 16])
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "00000000000000000000000000000000"
        );
    }

    #[test]
    fn staged_transfer_keeps_parts_out_of_destination_and_resumes() {
        let content = b"0123456789abcdefGHIJ"; // 20 bytes, chunk 8 -> 8+8+4
        let (_src_tmp, _source, entry) = transfer_entry(content, 8);
        let mut nested = entry.clone();
        nested.rel_path = PathBuf::from("docs/2026/note.txt");

        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("docs/2026/note.txt");
        let staged_part = staging_path(dst.path(), TASK_A).join("docs/2026/note.txt.akpart");

        let mut sink = target
            .prepare_file_staged(TASK_A, &nested, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        drop(sink); // interrupted: the part survives inside staging only

        assert!(staged_part.exists(), "part lives in the staging tree");
        assert!(
            !dst.path().join("docs/2026/note.txt.akpart").exists(),
            "no loose part beside the final name"
        );
        assert!(!final_path.exists());

        // Same task resumes its own staged prefix.
        let mut resumed = target
            .prepare_file_staged(TASK_A, &nested, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 1);
        resumed.write_chunk(1, b"89abcdef").unwrap();
        resumed.write_chunk(2, b"GHIJ").unwrap();
        resumed.finish().unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), content);
        assert!(!staged_part.exists(), "part consumed by the commit");
        // Staging directory levels survive until task cleanup.
        assert!(staging_path(dst.path(), TASK_A).join("docs/2026").is_dir());
        discard_staging(dst.path(), TASK_A).unwrap();
        assert!(!staging_path(dst.path(), TASK_A).exists());
        assert_eq!(fs::read(&final_path).unwrap(), content, "final stays");

        // Empty file through the staged path: zero-chunk commit, no part
        // residue in the destination tree.
        let empty = FileEntry::new("empty.bin", 0, 0, b3(b""), Vec::new()).unwrap();
        let mut sink = target
            .prepare_file_staged(TASK_A, &empty, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink.finish().unwrap();
        assert_eq!(
            fs::read(dst.path().join("empty.bin")).unwrap(),
            Vec::<u8>::new()
        );
        assert!(!dst.path().join("empty.bin.akpart").exists());
    }

    #[test]
    fn two_tasks_same_target_same_content_commit_once_and_reuse() {
        let content = b"shared-payload-0123456789"; // 23 bytes, chunk 8 -> 8+8+7
        let (_tmp_a, source_a, entry_a) = transfer_entry(content, 8);
        let (_tmp_b, _source_b, entry_b) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        let mut sink_a = target
            .prepare_file_staged(TASK_A, &entry_a, ConflictPolicy::Fail, cs(8))
            .unwrap();
        let mut sink_b = target
            .prepare_file_staged(TASK_B, &entry_b, ConflictPolicy::Fail, cs(8))
            .unwrap();
        // Interleaved writes: two live part files, one per task tree.
        let part_a = staging_path(dst.path(), TASK_A).join("payload.bin.akpart");
        let part_b = staging_path(dst.path(), TASK_B).join("payload.bin.akpart");
        assert!(part_a.exists() && part_b.exists());
        assert_ne!(part_a, part_b);
        for i in 0..entry_a.chunk_digests.len() as u64 {
            let range = ChunkPlan::new(entry_a.size, source_a.manifest().chunk_size)
                .range(i)
                .unwrap();
            let len = usize::try_from(range.end - range.start).unwrap();
            let mut buf = vec![0u8; len];
            assert_eq!(source_a.read_chunk(0, i, &mut buf).unwrap(), len);
            sink_a.write_chunk(i, &buf).unwrap();
            sink_b.write_chunk(i, &buf).unwrap();
        }

        // First finisher commits; second finisher must REUSE, not conflict
        // — under Fail policy, success for B is only reachable through the
        // content-match reuse branch.
        sink_a.finish().unwrap();
        sink_b.finish().unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), content);
        assert!(!part_a.exists() && !part_b.exists(), "both parts consumed");
        assert!(
            !dst.path().join("payload (2).bin").exists(),
            "identical content must not spawn a duplicate"
        );
        // Exactly one final name in the destination root (plus the staging
        // directory): no loose .akpart anywhere in the visible tree.
        let mut finals: Vec<_> = fs::read_dir(dst.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        finals.sort();
        assert_eq!(
            finals,
            vec![".ak-staging".to_owned(), "payload.bin".to_owned()]
        );
    }

    #[test]
    fn two_tasks_same_target_different_content_one_commit_one_rename() {
        let content_a = b"AAAAAAAAAAAAAAAA"; // 16 bytes, chunk 8
        let content_b = b"BBBBBBBBBBBBBBBB"; // same size, different bytes
        let (_tmp_a, source_a, entry_a) = transfer_entry(content_a, 8);
        let (_tmp_b, source_b, entry_b) = transfer_entry(content_b, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        let mut sink_a = target
            .prepare_file_staged(TASK_A, &entry_a, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink_a, &source_a, &entry_a);
        sink_a.finish().unwrap();

        let mut sink_b = target
            .prepare_file_staged(TASK_B, &entry_b, ConflictPolicy::Rename, cs(8))
            .unwrap();
        write_all_chunks(&mut sink_b, &source_b, &entry_b);
        sink_b.finish().unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), content_a, "original wins");
        assert_eq!(
            fs::read(dst.path().join("payload (2).bin")).unwrap(),
            content_b,
            "different content lands under a Rename name"
        );
        assert!(
            !staging_path(dst.path(), TASK_B)
                .join("payload.bin.akpart")
                .exists(),
            "loser's part consumed by its own commit"
        );
    }

    #[test]
    fn two_tasks_different_content_under_fail_keeps_loser_part() {
        let content_a = b"keep-me-01234567";
        let content_b = b"loser-89abcdefg";
        let (_tmp_a, source_a, entry_a) = transfer_entry(content_a, 8);
        let (_tmp_b, source_b, entry_b) = transfer_entry(content_b, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();

        let mut sink_a = target
            .prepare_file_staged(TASK_A, &entry_a, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink_a, &source_a, &entry_a);
        sink_a.finish().unwrap();

        let mut sink_b = target
            .prepare_file_staged(TASK_B, &entry_b, ConflictPolicy::Fail, cs(8))
            .unwrap();
        write_all_chunks(&mut sink_b, &source_b, &entry_b);
        let err = sink_b.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dst.path().join("payload.bin")).unwrap(), content_a);
        // The loser's fully verified part stays in ITS OWN staging tree for
        // a policy retry; the destination tree stays clean.
        let part_b = staging_path(dst.path(), TASK_B).join("payload.bin.akpart");
        assert!(part_b.exists(), "conflicting part kept in staging");
        assert!(
            !dst.path().join("payload.bin.akpart").exists(),
            "no loose part beside the final name"
        );
    }

    #[test]
    fn cancelling_one_task_never_touches_the_others_parts() {
        let content = b"0123456789abcdefGHIJ";
        let (_tmp, _source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let part_b = staging_path(dst.path(), TASK_B).join("payload.bin.akpart");

        let mut sink_a = target
            .prepare_file_staged(TASK_A, &entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        let mut sink_b = target
            .prepare_file_staged(TASK_B, &entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        sink_a.write_chunk(0, b"01234567").unwrap();
        sink_b.write_chunk(0, b"01234567").unwrap();

        // Cancel A: the sink-level discard removes only A's own part...
        sink_a.discard();
        assert!(!staging_path(dst.path(), TASK_A)
            .join("payload.bin.akpart")
            .exists());
        assert!(part_b.exists(), "B's part must survive A's cancel");
        // ...and the task-level cleanup removes only A's tree.
        discard_staging(dst.path(), TASK_A).unwrap();
        assert!(!staging_path(dst.path(), TASK_A).exists());
        assert!(part_b.exists(), "B's part still there after A's cleanup");
        assert!(
            !dst.path().join("payload.bin").exists(),
            "nothing committed"
        );

        // B continues from its untouched part and commits normally.
        let mut resumed_b = target
            .prepare_file_staged(TASK_B, &entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(resumed_b.verified_prefix_chunks(), 1, "B resumes its part");
        resumed_b.write_chunk(1, b"89abcdef").unwrap();
        resumed_b.write_chunk(2, b"GHIJ").unwrap();
        resumed_b.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("payload.bin")).unwrap(), content);
    }

    #[test]
    fn discard_staging_removes_only_that_tasks_tree_and_is_idempotent() {
        let dst = TempDir::new().unwrap();
        let tree_a = staging_path(dst.path(), TASK_A);
        fs::create_dir_all(tree_a.join("deep/dir")).unwrap();
        wf(&tree_a.join("deep/dir/f.bin.akpart"), b"x");
        wf(&tree_a.join("top.txt.akpart"), b"y");
        let tree_b = staging_path(dst.path(), TASK_B);
        fs::create_dir_all(&tree_b).unwrap();
        wf(&tree_b.join("keep.bin.akpart"), b"z");
        let sibling_of_staging = dst.path().join("real-file.txt");
        wf(&sibling_of_staging, b"keep");

        discard_staging(dst.path(), TASK_A).unwrap();
        assert!(!tree_a.exists(), "the whole task tree is gone");
        assert!(tree_b.join("keep.bin.akpart").exists(), "sibling task kept");
        assert!(sibling_of_staging.exists(), "destination files kept");

        // Idempotent: absent tree is Ok, not NotFound.
        discard_staging(dst.path(), TASK_A).unwrap();
        // A planted regular FILE where the task directory belongs refuses
        // rather than deleting an arbitrary file.
        wf(&tree_a, b"not-a-dir");
        assert!(discard_staging(dst.path(), TASK_A).is_err());
    }

    #[test]
    fn legacy_sibling_part_migrates_into_staging_and_resumes() {
        let content = b"0123456789abcdefGHIJ";
        let (_tmp, _source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let legacy = dst.path().join("payload.bin.akpart");
        let staged = staging_path(dst.path(), TASK_A).join("payload.bin.akpart");

        // Pre-staging run: classic sink leaves a verified prefix beside the
        // final name.
        let mut classic = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        classic.write_chunk(0, b"01234567").unwrap();
        classic.write_chunk(1, b"89abcdef").unwrap();
        drop(classic);
        assert!(legacy.exists());

        // Staged prepare adopts the legacy part into the task tree and
        // resumes its verified prefix.
        let mut staged_sink = target
            .prepare_file_staged(TASK_A, &entry, ConflictPolicy::Fail, cs(8))
            .unwrap();
        assert_eq!(staged_sink.verified_prefix_chunks(), 2);
        assert!(!legacy.exists(), "legacy part moved away, not duplicated");
        assert!(staged.exists(), "resumed inside the staging tree");
        assert_eq!(fs::read(&staged).unwrap(), &content[..16]);

        staged_sink.write_chunk(2, b"GHIJ").unwrap();
        staged_sink.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("payload.bin")).unwrap(), content);
        assert!(!staged.exists());
        assert!(!legacy.exists());
    }

    #[test]
    fn staged_prepare_rejects_reserved_zero_task_id() {
        let (_tmp, _source, entry) = transfer_entry(b"01234567", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let err = target
            .prepare_file_staged([0u8; 16], &entry, ConflictPolicy::Fail, cs(8))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            !dst.path().join(STAGING_DIR).exists(),
            "no staging tree created for a rejected id"
        );
    }

    #[test]
    fn staged_prepare_rejects_symlinked_staging_level() {
        let (_tmp, _source, entry) = transfer_entry(b"01234567", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let outside = TempDir::new().unwrap();
        let link = dst.path().join(STAGING_DIR);
        if !make_symlink(outside.path(), &link, true) {
            return;
        }
        let err = target
            .prepare_file_staged(TASK_A, &entry, ConflictPolicy::Fail, cs(8))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            outside.path().read_dir().unwrap().next().is_none(),
            "nothing written through the link"
        );
    }

    // -----------------------------------------------------------------------
    // Reserved staging namespace (review 2026-11-05, P2)
    // -----------------------------------------------------------------------

    #[test]
    fn prepare_rejects_rel_paths_entering_the_staging_namespace() {
        let (_tmp, _source, entry) = transfer_entry(b"0123", 4);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let task_hex = hex_prefix(&TASK_A, 16);
        let shapes = [
            format!("{STAGING_DIR}/payload.bin"),
            format!("{STAGING_DIR}/{task_hex}/payload.bin"),
            format!("docs/{STAGING_DIR}/nested.bin"),
            // NTFS resolves directory names case-insensitively: a differently
            // cased spelling lands in the same directory.
            ".AK-STAGING/payload.bin".to_owned(),
        ];
        for rel in shapes {
            let mut e = entry.clone();
            e.rel_path = PathBuf::from(&rel);
            let err = target
                .prepare_file_staged(TASK_A, &e, ConflictPolicy::Fail, cs(4))
                .err()
                .unwrap_or_else(|| panic!("staged prepare must reject {rel}"));
            assert_eq!(err.kind(), ErrorKind::InvalidInput, "{rel}: {err}");
            assert!(
                err.to_string().contains("staging namespace"),
                "{rel}: {err}"
            );
            // The shared component check covers the unstaged receive path
            // too (uniform rejection at every entry point).
            let err = target
                .prepare_file_with_chunk_size(&e, ConflictPolicy::Fail, cs(4))
                .err()
                .unwrap_or_else(|| panic!("unstaged prepare must reject {rel}"));
            assert_eq!(err.kind(), ErrorKind::InvalidInput, "{rel}: {err}");
        }
        // Nothing was created anywhere before the rejection.
        assert!(
            !dst.path().join(STAGING_DIR).exists(),
            "no staging tree may be created for rejected entries"
        );
        assert!(
            dst.path().read_dir().unwrap().next().is_none(),
            "destination stays untouched"
        );
    }

    // -----------------------------------------------------------------------
    // Concurrent same-target finish: one commit, one reuse (review
    // 2026-11-05, P2) — two threads released together after both parts are
    // fully verified; the loser of the atomic claim must reuse by content,
    // never fail (Fail policy) nor duplicate (Rename policy).
    // -----------------------------------------------------------------------

    #[test]
    fn concurrent_staged_finish_same_content_is_one_commit_one_reuse() {
        use std::sync::{Arc, Barrier};

        // Several chunks so the hash/sync phases give the race room to land
        // on either side of the pre-check; repeated rounds raise the odds
        // of exercising the AlreadyExists re-adjudication branch.
        let content = vec![0xA5u8; 128 * 1024];
        let chunk: u32 = 16 * 1024;
        let (_src_tmp, source, entry) = transfer_entry(&content, chunk);
        for _round in 0..5 {
            let dst = TempDir::new().unwrap();
            let target = LocalSaveTarget::open(dst.path()).unwrap();
            let final_path = dst.path().join("payload.bin");

            let mut sink_a = target
                .prepare_file_staged(TASK_A, &entry, ConflictPolicy::Fail, cs(chunk))
                .unwrap();
            let mut sink_b = target
                .prepare_file_staged(TASK_B, &entry, ConflictPolicy::Rename, cs(chunk))
                .unwrap();
            write_all_chunks(&mut sink_a, &source, &entry);
            write_all_chunks(&mut sink_b, &source, &entry);
            let part_a = staging_path(dst.path(), TASK_A).join("payload.bin.akpart");
            let part_b = staging_path(dst.path(), TASK_B).join("payload.bin.akpart");
            assert!(part_a.exists() && part_b.exists());

            let barrier = Arc::new(Barrier::new(2));
            let start_b = Arc::clone(&barrier);
            let handle = std::thread::spawn(move || {
                start_b.wait();
                sink_a.finish()
            });
            barrier.wait();
            let result_b = sink_b.finish();
            let result_a = handle.join().expect("finish thread");
            result_a.expect("first finisher commits");
            result_b.expect("loser reuses, never fails nor duplicates");

            assert_eq!(fs::read(&final_path).unwrap(), content);
            assert!(!part_a.exists() && !part_b.exists(), "both parts consumed");
            assert!(
                !dst.path().join("payload (2).bin").exists(),
                "same content must not spawn a duplicate"
            );
            let mut finals: Vec<_> = fs::read_dir(dst.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            finals.sort();
            assert_eq!(
                finals,
                vec![STAGING_DIR.to_owned(), "payload.bin".to_owned()],
                "exactly one final file plus the staging root"
            );
        }
    }

    // -----------------------------------------------------------------------
    // discard_staging parent-link guard (review 2026-11-05, P1-2)
    // -----------------------------------------------------------------------

    #[test]
    fn discard_staging_refuses_a_symlinked_staging_parent() {
        let dst = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        // The attacker's tree: `<outside>/.ak-staging/<task>/…` plus a
        // victim that must never be touched. `symlink_metadata(top)`
        // alone would see a real directory under the link.
        let task_hex = hex_prefix(&TASK_A, 16);
        let external_task = outside.path().join(STAGING_DIR).join(&task_hex);
        fs::create_dir_all(&external_task).unwrap();
        wf(&external_task.join("f.bin.akpart"), b"x");
        let victim = outside.path().join("victim.txt");
        wf(&victim, b"keep");
        if !make_symlink(
            &outside.path().join(STAGING_DIR),
            &dst.path().join(STAGING_DIR),
            true,
        ) {
            return;
        }

        let err = discard_staging(dst.path(), TASK_A).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            external_task.join("f.bin.akpart").exists(),
            "nothing inside the linked tree was touched"
        );
        assert!(victim.exists(), "external victim untouched");
        // A legit tree next to the refusal still cleans up normally.
        // (Windows needs remove_dir for a directory symlink.)
        let link = dst.path().join(STAGING_DIR);
        fs::remove_file(&link)
            .or_else(|_| fs::remove_dir(&link))
            .expect("drop the planted link");
        let real_tree = staging_path(dst.path(), TASK_B);
        fs::create_dir_all(real_tree.join("d")).unwrap();
        wf(&real_tree.join("d/x.akpart"), b"y");
        discard_staging(dst.path(), TASK_B).unwrap();
        assert!(!real_tree.exists());
    }
}
