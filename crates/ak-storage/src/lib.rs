//! Local filesystem adapter: source scanning and receive-side save targets.
//!
//! Guarantees (see AGENTS.md): no path escapes (`..`, absolute, drive letters
//! rejected by `ak_core::manifest`), no symlink races (every existing path
//! component is re-checked with `symlink_metadata` before use), bounded
//! memory (streaming digests), and atomic file commit (temp `.akpart` file,
//! fsync, rename; the final name only appears after the whole digest matches).
#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ak_core::chunk::ChunkPlan;
use ak_core::error::ErrorCode;
use ak_core::handle::{default_chunk_size, ConflictPolicy, FileSink, SaveTarget, SourceHandle};
use ak_core::manifest::{validate_rel_path, FileEntry, Manifest, DIGEST_ALG_BLAKE3, DIGEST_LEN};

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

fn invalid_input_io(msg: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, msg.into())
}

fn escape_err() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, OpenError::Escape)
}

fn conflict_err(path: &Path) -> io::Error {
    io::Error::new(ErrorKind::AlreadyExists, format!("conflict: {} already exists", path.display()))
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
    /// Directory traversal is a pre-order walk with children sorted by name at
    /// each level, so a parent directory entry always precedes its children
    /// and the receiver can create entries in manifest order. Symlinks are
    /// never followed: the root itself is rejected as [`OpenError::RootIsSymlink`]
    /// and any symlink below it as [`OpenError::SymlinkComponent`].
    pub fn open(root: &Path, chunk_size: NonZeroU32) -> Result<Self, OpenError> {
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
            scan_dir(root, "", chunk_size.get(), &mut entries)?;
        } else if file_type.is_file() {
            root_is_file = true;
            let name = root
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| OpenError::Io(invalid_input_io(format!("non-UTF-8 root file name: {}", root.display()))))?;
            scan_file(root, name, chunk_size.get(), &mut entries)?;
        } else {
            return Err(OpenError::Io(invalid_input_io(format!(
                "unsupported file type at transfer root: {}",
                root.display()
            ))));
        }
        Ok(Self { root: root.to_path_buf(), root_is_file, manifest: Manifest { chunk_size, entries } })
    }

    /// Absolute path of an entry's content on the sender.
    fn entry_path(&self, entry: &FileEntry) -> PathBuf {
        if self.root_is_file {
            self.root.clone()
        } else {
            self.root.join(&entry.rel_path)
        }
    }

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

/// Pre-order walk: children sorted by name; a directory entry is emitted
/// immediately before its own children.
fn scan_dir(dir: &Path, prefix: &str, chunk_size: u32, entries: &mut Vec<FileEntry>) -> Result<(), OpenError> {
    let mut children: Vec<(OsString, PathBuf, fs::FileType)> = Vec::new();
    for child in fs::read_dir(dir)? {
        let child = child?;
        let path = child.path();
        let meta = fs::symlink_metadata(&path)?;
        children.push((child.file_name(), path, meta.file_type()));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path, file_type) in children {
        let name = name
            .to_str()
            .ok_or_else(|| OpenError::Io(invalid_input_io(format!("non-UTF-8 file name: {}", path.display()))))?;
        let rel = if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
        if file_type.is_symlink() {
            return Err(OpenError::SymlinkComponent(path));
        } else if file_type.is_dir() {
            entries.push(FileEntry::directory(&rel).map_err(|_| bad_rel_path(&rel))?);
            scan_dir(&path, &rel, chunk_size, entries)?;
        } else if file_type.is_file() {
            scan_file(&path, &rel, chunk_size, entries)?;
        } else {
            return Err(OpenError::Io(invalid_input_io(format!(
                "unsupported file type in source tree: {}",
                path.display()
            ))));
        }
    }
    Ok(())
}

/// Stream one file once, computing the whole digest and per-chunk digests with
/// a single chunk-sized buffer (bounded memory).
fn scan_file(path: &Path, rel: &str, chunk_size: u32, entries: &mut Vec<FileEntry>) -> Result<(), OpenError> {
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
    let entry = FileEntry::new(rel, size, mtime, *whole.finalize().as_bytes(), chunk_digests)
        .map_err(|_| bad_rel_path(rel))?;
    entries.push(entry);
    Ok(())
}

fn bad_rel_path(rel: &str) -> OpenError {
    OpenError::Io(invalid_input_io(format!("file name not representable as a transfer path: {rel:?}")))
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
        let parts = checked_rel_components(entry)?;
        if entry.is_dir {
            ensure_real_dirs(&self.root, &parts)?;
            return Ok(LocalFileSink { kind: SinkKind::Directory });
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
        let part_path = with_part_suffix(&final_path);
        if let Ok(meta) = fs::symlink_metadata(&part_path) {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(ErrorKind::InvalidInput, OpenError::SymlinkComponent(part_path)));
            }
        }
        // No truncate: an existing part is verified and resumed in place.
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&part_path)?;
        let verified_prefix = verify_part_prefix(&mut file, entry, chunk_size)?;
        let state = PartState {
            file,
            final_path,
            part_path,
            conflict,
            size: entry.size,
            chunk_size,
            whole_digest: entry.whole_digest,
            chunk_digests: entry.chunk_digests.clone(),
            verified_prefix,
            finished: false,
        };
        Ok(LocalFileSink { kind: SinkKind::File(state) })
    }
}

impl SaveTarget for LocalSaveTarget {
    fn root(&self) -> &Path {
        &self.root
    }

    /// `std` has no stable free-space API on Windows; the `Option` design
    /// exists exactly for platforms that cannot report it. Space exhaustion is
    /// surfaced as a real write error instead of a fake capacity figure.
    fn available_bytes(&self) -> Option<u64> {
        None
    }

    /// Prepare the sink for one entry. If a `.akpart` file for this entry
    /// already exists, the sink re-reads it, re-verifies the chunk-digest
    /// prefix, and resumes from there (transparent resume).
    fn prepare_file(&self, entry: &ak_core::manifest::FileEntry, conflict: ConflictPolicy) -> io::Result<Box<dyn FileSink>> {
        Ok(Box::new(self.prepare_file_with_chunk_size(entry, conflict, default_chunk_size())?))
    }
}

/// Validate the entry's relative path component by component and return the
/// components. This re-check runs even for hand-built entries because
/// `FileEntry`'s fields are public: only `Component::Normal` parts are
/// accepted, then the full `validate_rel_path` rule set is applied to the
/// reconstructed `/`-separated path. Callers join the returned components
/// onto the root themselves; the raw `rel_path` is never joined directly.
fn checked_rel_components(entry: &FileEntry) -> io::Result<Vec<&OsStr>> {
    let mut parts = Vec::new();
    let mut joined = String::new();
    for comp in entry.rel_path.components() {
        let Component::Normal(part) = comp else {
            return Err(escape_err());
        };
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
/// accepted if `symlink_metadata` proves it is a real directory (this is the
/// symlink-race check: a symlink to a directory would pass `exists()`).
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
                    return Err(io::Error::new(ErrorKind::InvalidInput, OpenError::SymlinkComponent(current)));
                }
                if !ft.is_dir() {
                    return Err(io::Error::new(ErrorKind::AlreadyExists, OpenError::NotDirectory(current)));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Append [`PART_SUFFIX`] to a full path (simple OsString append, keeps any
/// multi-dot extensions intact).
fn with_part_suffix(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(PART_SUFFIX);
    PathBuf::from(os)
}

/// Re-verify an existing `.akpart` file chunk by chunk against the manifest,
/// truncate at the first mismatch or truncation point, position the write
/// cursor there, and return the verified chunk count. Bounded memory: one
/// chunk-sized buffer, sequential reads.
fn verify_part_prefix(file: &mut File, entry: &FileEntry, chunk_size: NonZeroU32) -> io::Result<u64> {
    let plan = ChunkPlan::new(entry.size, chunk_size);
    let mut buf = vec![0u8; chunk_size.get() as usize];
    file.rewind()?;
    let mut verified = 0u64;
    for digest in &entry.chunk_digests {
        let Some(range) = plan.range(verified) else { break };
        let want = usize::try_from(range.end - range.start)
            .map_err(|_| invalid_input_io("chunk length out of usize range"))?;
        let n = read_fill(file, &mut buf[..want])?;
        if n != want || b3(&buf[..want]) != *digest {
            break;
        }
        verified += 1;
    }
    let verified_bytes = if verified == 0 { 0 } else { plan.range(verified - 1).map_or(0, |r| r.end) };
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
}

struct PartState {
    file: File,
    final_path: PathBuf,
    part_path: PathBuf,
    conflict: ConflictPolicy,
    size: u64,
    chunk_size: NonZeroU32,
    whole_digest: [u8; DIGEST_LEN],
    chunk_digests: Vec<[u8; DIGEST_LEN]>,
    verified_prefix: u64,
    finished: bool,
}

impl FileSink for LocalFileSink {
    fn write_chunk(&mut self, chunk_index: u64, data: &[u8]) -> io::Result<()> {
        let st = match &mut self.kind {
            SinkKind::File(st) => st,
            SinkKind::Directory => return Err(invalid_input_io("directory entries accept no file chunks")),
        };
        let idx = usize::try_from(chunk_index).map_err(|_| invalid_input_io("chunk index out of range"))?;
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
            return Err(invalid_input_io("chunk length does not match the manifest chunk plan"));
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
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        let st = match &mut self.kind {
            SinkKind::File(st) => st,
            SinkKind::Directory => return Ok(()),
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
        st.file.rewind()?;
        let mut buf = vec![0u8; st.chunk_size.get() as usize];
        if hash_reader(&mut st.file, &mut buf)? != st.whole_digest {
            // Keep the part file: its verified prefix is still reusable.
            return Err(io::Error::new(ErrorKind::InvalidData, "HASH_MISMATCH"));
        }
        st.file.sync_all()?;
        // Pre-check the target so ConflictPolicy::Fail can never silently
        // replace an existing file (POSIX rename would do so). A concurrent
        // create between check and rename is still caught by the AlreadyExists
        // retry below on Windows, where rename onto an existing name fails.
        let mut target = st.final_path.clone();
        if fs::symlink_metadata(&target).is_ok() {
            match st.conflict {
                ConflictPolicy::Fail => return Err(conflict_err(&target)),
                ConflictPolicy::Rename => target = conflict_free_name(&target),
            }
        }
        let mut renames = 0u32;
        loop {
            match fs::rename(&st.part_path, &target) {
                Ok(()) => {
                    st.finished = true;
                    return Ok(());
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => match st.conflict {
                    ConflictPolicy::Fail => return Err(conflict_err(&st.final_path)),
                    ConflictPolicy::Rename => {
                        renames += 1;
                        if renames > 4096 {
                            return Err(conflict_err(&st.final_path));
                        }
                        target = conflict_free_name(&target);
                    }
                },
                Err(e) => return Err(e),
            }
        }
    }

    fn discard(self) {
        if let SinkKind::File(st) = self.kind {
            drop(st.file);
            // Best effort; a vanished part is fine.
            let _ = fs::remove_file(&st.part_path);
        }
    }
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
    let ext = existing.extension().map(|e| e.to_string_lossy().into_owned());
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

#[cfg(test)]
mod tests {
    use super::*;
    use ak_core::manifest::DIGEST_LEN;
    use std::fs;
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

    /// Create a symlink; on Windows this needs the symlink privilege. Returns
    /// false (and prints why) when the OS refuses, so tests can skip honestly.
    fn make_symlink(src: &Path, dst: &Path, dir_link: bool) -> bool {
        match symlink_once(src, dst, dir_link) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("SKIP: cannot create symlink at {} ({}); test reduced", dst.display(), e);
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
        assert_eq!(rels, ["a", "a/b", "a/b/deep.txt", "a/z.txt", "empty", "multi1.txt", "multi2.txt"]);
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
        let err = LocalSource::open(&tmp.path().join("nope"), cs(8)).err().unwrap();
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
        assert_eq!(dir_source.manifest().entries[0].rel_path, PathBuf::from("d"));
        assert!(dir_source.read_chunk(0, 0, &mut [0u8; 4]).is_err(), "dir entry is not readable");
        assert!(dir_source.read_chunk(1, 0, &mut [0u8; 4]).is_ok());

        let empty = tmp.path().join("empty.bin");
        wf(&empty, b"");
        let empty_source = LocalSource::open(&empty, cs(4)).unwrap();
        assert!(empty_source.read_chunk(0, 0, &mut [0u8; 4]).is_err(), "empty file has no chunks");
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
        let err = LocalSaveTarget::open(&tmp.path().join("missing")).err().unwrap();
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
        assert_eq!(target.available_bytes(), None);
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
            let range = ChunkPlan::new(entry.size, source.manifest().chunk_size).range(i).unwrap();
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
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
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
    fn write_chunk_out_of_order_is_rejected() {
        let (_src_tmp, _source, entry) = transfer_entry(b"abcdefgh12345678", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
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

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        sink.write_chunk(0, b"abcdefgh").unwrap();
        let err = sink.write_chunk(1, b"12345678").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("HASH_MISMATCH"));
        assert_eq!(fs::metadata(&part_path).unwrap().len(), 8, "bad chunk must not touch the disk");
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

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        write_all_chunks(&mut sink, &source, &entry);
        let err = sink.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("HASH_MISMATCH"));
        assert!(!final_path.exists());
        assert_eq!(fs::read(&part_path).unwrap(), content, "verified prefix stays reusable");
    }

    #[test]
    fn empty_file_commits_through_finish_only() {
        let (_src_tmp, _source, entry) = transfer_entry(b"", 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        assert_eq!(sink.verified_prefix_chunks(), 0);
        sink.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), Vec::<u8>::new());
        assert!(!dst.path().join("payload.bin.akpart").exists());
    }

    #[test]
    fn conflict_fail_errors_and_rename_generates_free_name() {
        let content = b"abcdefgh12345678";
        let (_src_tmp, source, entry) = transfer_entry(content, 8);
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let final_path = dst.path().join("payload.bin");

        let mut first = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        write_all_chunks(&mut first, &source, &entry);
        first.finish().unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), content);

        // Second transfer of the same entry.
        let mut second = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        write_all_chunks(&mut second, &source, &entry);
        let err = second.finish().err().unwrap();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(dst.path().join("payload.bin.akpart").exists(), "part kept for retry");
        drop(second);

        // Retry with Rename: the part resumes fully verified and lands as (2).
        let mut third = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Rename, cs(8)).unwrap();
        assert_eq!(third.verified_prefix_chunks(), 2, "resume from the kept part");
        third.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("payload (2).bin")).unwrap(), content);
        assert_eq!(fs::read(&final_path).unwrap(), content, "original untouched");
        assert!(!dst.path().join("payload.bin.akpart").exists());
    }

    #[test]
    fn conflict_free_name_increments_from_two_and_handles_extensions() {
        let tmp = TempDir::new().unwrap();
        let name = tmp.path().join("a.tar.gz");
        assert_eq!(conflict_free_name(&name).file_name().unwrap(), "a.tar (2).gz");
        wf(&tmp.path().join("a.tar (2).gz"), b"x");
        assert_eq!(conflict_free_name(&name).file_name().unwrap(), "a.tar (3).gz");

        let plain = tmp.path().join("README");
        assert_eq!(conflict_free_name(&plain).file_name().unwrap(), "README (2)");
        assert_eq!(conflict_free_name(Path::new("rel.txt")).file_name().unwrap(), "rel (2).txt");
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

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink); // no discard: the part survives for resume

        let mut resumed = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
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

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink);

        // Corrupt byte inside chunk 1 (offset 10).
        let mut part = fs::OpenOptions::new().write(true).open(&part_path).unwrap();
        part.seek(SeekFrom::Start(10)).unwrap();
        part.write_all(&[0x00]).unwrap();
        drop(part);

        let mut resumed = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 1, "chunk 1 no longer verifies");
        assert_eq!(part_path.metadata().unwrap().len(), 8, "truncated at the bad chunk");
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

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        sink.write_chunk(0, b"01234567").unwrap();
        sink.write_chunk(1, b"89abcdef").unwrap();
        drop(sink);
        let mut part = fs::OpenOptions::new().append(true).open(&part_path).unwrap();
        part.write_all(b"garbage").unwrap();
        drop(part);

        let resumed = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(8)).unwrap();
        assert_eq!(resumed.verified_prefix_chunks(), 2);
        assert_eq!(part_path.metadata().unwrap().len(), 16, "stale tail dropped");
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
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4)).unwrap();
        sink.write_chunk(0, b"0123").unwrap();
        assert!(part_path.exists());
        sink.discard();
        assert!(!part_path.exists());
        assert!(!dst.path().join("payload.bin").exists());
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
        let entry = FileEntry::new("link/file.txt", 1, 0, [0; DIGEST_LEN], vec![[0; DIGEST_LEN]]).unwrap();
        let err = target
            .prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4))
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(outside.path().read_dir().unwrap().next().is_none(), "nothing written through the link");

        // Directory entry below the symlinked component.
        let dir_entry = FileEntry::directory("link/sub").unwrap();
        assert!(target.prepare_file_with_chunk_size(&dir_entry, ConflictPolicy::Fail, cs(4)).is_err());
        assert!(outside.path().read_dir().unwrap().next().is_none());

        // A file entry whose final name IS the symlink: conflict, never a
        // write through the link.
        let digest = b3(b"x");
        let entry = FileEntry::new("link", 1, 0, digest, vec![digest]).unwrap();
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(1)).unwrap();
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
        assert!(target.prepare_file_with_chunk_size(&escape, ConflictPolicy::Fail, cs(4)).is_err());

        let mut absolute = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        absolute.rel_path = if cfg!(windows) { PathBuf::from("C:\\evil.txt") } else { PathBuf::from("/etc/evil.txt") };
        assert!(target.prepare_file_with_chunk_size(&absolute, ConflictPolicy::Fail, cs(4)).is_err());

        let mut rooted = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        rooted.rel_path = PathBuf::from("/evil.txt");
        assert!(target.prepare_file_with_chunk_size(&rooted, ConflictPolicy::Fail, cs(4)).is_err());

        let mut empty = FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).unwrap();
        empty.rel_path = PathBuf::new();
        assert!(target.prepare_file_with_chunk_size(&empty, ConflictPolicy::Fail, cs(4)).is_err());

        assert!(!dst.path().join("ok.txt.akpart").exists());
        assert!(!dst.path().parent().unwrap().join("evil.txt").exists());
    }

    #[test]
    fn directory_entry_creates_levels_and_finish_is_noop() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let entry = FileEntry::directory("a/b/c").unwrap();

        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(4)).unwrap();
        assert!(dst.path().join("a/b/c").is_dir());
        assert_eq!(sink.verified_prefix_chunks(), 0);
        assert!(sink.write_chunk(0, b"x").is_err());
        sink.finish().unwrap();
        sink.discard(); // no-op for directories
        assert!(dst.path().join("a/b/c").is_dir());

        // Preparing the same directory entry again is an idempotent no-op.
        let mut again = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Rename, cs(4)).unwrap();
        again.finish().unwrap();
        assert!(dst.path().join("a/b/c").is_dir());
    }

    #[test]
    fn file_entry_creates_missing_parent_directories_safely() {
        let dst = TempDir::new().unwrap();
        let target = LocalSaveTarget::open(dst.path()).unwrap();
        let digest = b3(b"xy");
        let entry = FileEntry::new("docs/2026/note.txt", 2, 0, digest, vec![digest]).unwrap();
        let mut sink = target.prepare_file_with_chunk_size(&entry, ConflictPolicy::Fail, cs(2)).unwrap();
        sink.write_chunk(0, b"xy").unwrap();
        sink.finish().unwrap();
        assert_eq!(fs::read(dst.path().join("docs/2026/note.txt")).unwrap(), b"xy");
    }
}
