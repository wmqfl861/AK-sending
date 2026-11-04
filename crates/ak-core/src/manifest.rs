//! Manifest entries and relative-path validation shared by every crate.
//!
//! A manifest describes only structure and digests; it never holds file content.
//! Relative paths use `/` as the canonical separator and are validated here so
//! that the wire codec, storage layer and engine share one rule set.
use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;

/// Digest algorithm identifier used in manifest entries and the wire protocol.
pub const DIGEST_ALG_BLAKE3: u8 = 0;
/// Byte length of every digest in the manifest and wire protocol.
pub const DIGEST_LEN: usize = 32;
/// Upper bound for a validated relative path in bytes (UTF-8).
pub const MAX_REL_PATH_BYTES: usize = 1024;

/// A relative path rejected by [`validate_rel_path`] (carry-all marker; the
/// rules are listed there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRelPath;

impl fmt::Display for InvalidRelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid relative path")
    }
}
impl std::error::Error for InvalidRelPath {}

/// Digest algorithm identifier rejected by [`validate_digest_alg`]: only
/// [`DIGEST_ALG_BLAKE3`] exists today. Unknown identifiers are surfaced
/// instead of being silently rewritten, so a peer (or caller) declaring an
/// algorithm this build cannot verify fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDigestAlg(pub u8);

impl fmt::Display for InvalidDigestAlg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown digest algorithm {}", self.0)
    }
}
impl std::error::Error for InvalidDigestAlg {}

/// Why a manifest entry could not be constructed: the relative path failed
/// validation, or the digest algorithm identifier is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidEntry {
    Path(InvalidRelPath),
    DigestAlg(InvalidDigestAlg),
}

impl From<InvalidRelPath> for InvalidEntry {
    fn from(err: InvalidRelPath) -> Self {
        InvalidEntry::Path(err)
    }
}

impl From<InvalidDigestAlg> for InvalidEntry {
    fn from(err: InvalidDigestAlg) -> Self {
        InvalidEntry::DigestAlg(err)
    }
}

impl fmt::Display for InvalidEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidEntry::Path(err) => err.fmt(f),
            InvalidEntry::DigestAlg(err) => err.fmt(f),
        }
    }
}
impl std::error::Error for InvalidEntry {}

/// Windows-reserved device names; files with these stems cannot exist portably.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a digest algorithm identifier and return it unchanged; only
/// [`DIGEST_ALG_BLAKE3`] is known. Entry constructors call this so an
/// entry's algorithm is either one this build can verify or an error —
/// never a silent rewrite to BLAKE3.
pub fn validate_digest_alg(alg: u8) -> Result<u8, InvalidDigestAlg> {
    if alg == DIGEST_ALG_BLAKE3 {
        Ok(alg)
    } else {
        Err(InvalidDigestAlg(alg))
    }
}

/// Validate a `/`-separated relative path and return it unchanged.
///
/// Rules: 1..=MAX_REL_PATH_BYTES UTF-8 bytes, no `\\`, no `:` (drive
/// letters), not rooted, no empty/`.`/`..` component, no control characters
/// (C0 U+0000..=001F, DEL U+007F, and C1 U+0080..=009F), no component
/// ending in space or dot, no Windows-reserved component stem.
pub fn validate_rel_path(path: &str) -> Result<&str, InvalidRelPath> {
    if path.is_empty() || path.len() > MAX_REL_PATH_BYTES {
        return Err(InvalidRelPath);
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(InvalidRelPath);
    }
    // Control characters are rejected per character (C0, DEL, C1 — the same
    // rule set as the wire codec and ak-signal): UTF-8 continuation bytes
    // occupy 0x80..=0xBF and overlap the C1 byte range, so a byte-level C1
    // scan would reject perfectly legal non-ASCII names. Backslash and
    // colon are ASCII-only separators and stay in the same pass.
    if path
        .chars()
        .any(|c| matches!(c, '\u{00}'..='\u{1f}' | '\u{7f}'..='\u{9f}') || c == '\\' || c == ':')
    {
        return Err(InvalidRelPath);
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(InvalidRelPath);
        }
        let stem = component.split_once('.').map_or(component, |(s, _)| s);
        if RESERVED_NAMES.contains(&stem.to_ascii_uppercase().as_str()) {
            return Err(InvalidRelPath);
        }
        if component.ends_with(' ') || component.ends_with('.') {
            return Err(InvalidRelPath);
        }
    }
    Ok(path)
}

/// One manifest entry: a file with its digests, or an empty directory
/// marker (directories carry no size or digests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Validated `/`-separated relative path (also the disk layout).
    pub rel_path: PathBuf,
    /// Directory entries carry no size or digests; the receiver just creates them.
    pub is_dir: bool,
    pub size: u64,
    /// Source modification time in Unix milliseconds; part of source versioning.
    pub mtime_unix_ms: i64,
    /// Algorithm identifier; only [`DIGEST_ALG_BLAKE3`] exists today, and
    /// constructors validate it via [`validate_digest_alg`] — unknown
    /// values are an error, never a silent rewrite.
    pub digest_alg: u8,
    /// BLAKE3 digest of the whole file.
    pub whole_digest: [u8; DIGEST_LEN],
    /// One digest per chunk in order; empty only when the file is empty.
    pub chunk_digests: Vec<[u8; DIGEST_LEN]>,
}

impl FileEntry {
    /// Build a file entry from already-validated parts under the only
    /// implemented digest algorithm ([`DIGEST_ALG_BLAKE3`]). The stamped
    /// algorithm is explicitly validated by [`FileEntry::with_digest_alg`],
    /// which this constructor delegates to.
    pub fn new(
        rel_path: &str,
        size: u64,
        mtime_unix_ms: i64,
        whole_digest: [u8; DIGEST_LEN],
        chunk_digests: Vec<[u8; DIGEST_LEN]>,
    ) -> Result<Self, InvalidEntry> {
        Self::with_digest_alg(
            rel_path,
            size,
            mtime_unix_ms,
            DIGEST_ALG_BLAKE3,
            whole_digest,
            chunk_digests,
        )
    }

    /// Build a file entry carrying an explicit digest algorithm identifier
    /// (e.g. decoded off the wire): the identifier is validated — unknown
    /// values are an error ([`InvalidEntry::DigestAlg`]), never silently
    /// rewritten to BLAKE3 — so downstream rejection checks see what the
    /// declaring side actually sent.
    pub fn with_digest_alg(
        rel_path: &str,
        size: u64,
        mtime_unix_ms: i64,
        digest_alg: u8,
        whole_digest: [u8; DIGEST_LEN],
        chunk_digests: Vec<[u8; DIGEST_LEN]>,
    ) -> Result<Self, InvalidEntry> {
        Ok(Self {
            rel_path: PathBuf::from(validate_rel_path(rel_path)?),
            is_dir: false,
            size,
            mtime_unix_ms,
            digest_alg: validate_digest_alg(digest_alg)?,
            whole_digest,
            chunk_digests,
        })
    }

    /// Build an empty-directory entry. Directories have no digests.
    pub fn directory(rel_path: &str) -> Result<Self, InvalidEntry> {
        Ok(Self {
            rel_path: PathBuf::from(validate_rel_path(rel_path)?),
            is_dir: true,
            size: 0,
            mtime_unix_ms: 0,
            digest_alg: DIGEST_ALG_BLAKE3,
            whole_digest: [0; DIGEST_LEN],
            chunk_digests: Vec::new(),
        })
    }
}

/// A complete transfer manifest: chunk size plus every file entry in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub chunk_size: NonZeroU32,
    pub entries: Vec<FileEntry>,
}

impl Manifest {
    /// Sum of every file entry's size (directories contribute zero).
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    /// Number of entries, files and directories alike.
    pub fn file_count(&self) -> u64 {
        self.entries.len() as u64
    }

    /// Split entries into fixed-size pages for bounded manifest exchange.
    pub fn pages(&self, page_size: usize) -> Vec<&[FileEntry]> {
        if self.entries.is_empty() {
            return vec![&[][..]];
        }
        self.entries.chunks(page_size.max(1)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names_and_nested_directories() {
        assert!(validate_rel_path("a.txt").is_ok());
        assert!(validate_rel_path("photos/2026/img 1.jpg").is_ok());
        assert!(validate_rel_path("deep/a/b/c/d.bin").is_ok());
    }

    #[test]
    fn rejects_traversal_absolute_and_separator_attacks() {
        for bad in [
            "", "/", "a/", "/a", "a//b", ".", "..", "a/..", "../a", "a/../b", "a/./b", "C:/x",
            "c:\\x", "a\\b", "a\u{0}b",
        ] {
            assert!(validate_rel_path(bad).is_err(), "accepted: {bad:?}");
        }
    }

    #[test]
    fn rejects_windows_unfriendly_components() {
        for bad in [
            "CON", "con.txt", "NUL.dat", "aux", "lpt1.log", "a/b:c", "a/b ", "a/b.", "name\t",
        ] {
            assert!(validate_rel_path(bad).is_err(), "accepted: {bad:?}");
        }
        assert!(validate_rel_path("console.txt").is_ok());
    }

    #[test]
    fn rejects_overlong_paths() {
        let long = "a".repeat(MAX_REL_PATH_BYTES + 1);
        assert!(validate_rel_path(&long).is_err());
        let edge = "a".repeat(MAX_REL_PATH_BYTES);
        assert!(validate_rel_path(&edge).is_ok());
    }

    #[test]
    fn entry_construction_validates_paths() {
        let digest = [0u8; DIGEST_LEN];
        assert!(FileEntry::new("ok.txt", 1, 0, digest, vec![digest]).is_ok());
        assert!(FileEntry::new("../escape", 1, 0, digest, vec![digest]).is_err());
    }

    #[test]
    fn digest_alg_is_validated_never_rewritten() {
        let digest = [0u8; DIGEST_LEN];
        assert!(validate_digest_alg(DIGEST_ALG_BLAKE3).is_ok());
        assert_eq!(validate_digest_alg(1), Err(InvalidDigestAlg(1)));
        assert_eq!(validate_digest_alg(u8::MAX), Err(InvalidDigestAlg(u8::MAX)));

        // Explicit unknown identifier: error, not a silent BLAKE3 rewrite.
        assert_eq!(
            FileEntry::with_digest_alg("f.bin", 1, 0, 1, digest, vec![digest]),
            Err(InvalidEntry::DigestAlg(InvalidDigestAlg(1)))
        );
        // Known identifier is carried through unchanged.
        let entry =
            FileEntry::with_digest_alg("f.bin", 1, 0, DIGEST_ALG_BLAKE3, digest, vec![digest])
                .unwrap();
        assert_eq!(entry.digest_alg, DIGEST_ALG_BLAKE3);
        // new() stamps the only implemented algorithm and validates it.
        assert_eq!(
            FileEntry::new("f.bin", 1, 0, digest, vec![digest])
                .unwrap()
                .digest_alg,
            DIGEST_ALG_BLAKE3
        );
        // Path failures still surface through the combined error.
        assert_eq!(
            FileEntry::with_digest_alg("../up", 1, 0, DIGEST_ALG_BLAKE3, digest, vec![digest]),
            Err(InvalidEntry::Path(InvalidRelPath))
        );
    }

    #[test]
    fn rejects_c0_del_and_c1_control_characters() {
        for bad in [
            "a\u{0}b",
            "nl\na",
            "cr\r",
            "tab\t",
            "del\u{7f}",
            "nel\u{85}",
            "csi\u{9b}",
            "\u{80}",
            "\u{9f}end",
            "mid\u{81}dle",
        ] {
            assert!(validate_rel_path(bad).is_err(), "accepted: {bad:?}");
        }
        // Legal multi-byte names whose UTF-8 continuation bytes fall inside
        // 0x80..=0x9F (U+0100 encodes as C4 80, U+011F as C4 9F): a
        // byte-level C1 scan would wrongly reject these.
        assert!(validate_rel_path("lat\u{100}.txt").is_ok());
        assert!(validate_rel_path("lat\u{11f}.txt").is_ok());
        assert!(validate_rel_path("设备/照片.bin").is_ok());
        assert!(validate_rel_path("emoji/\u{1f600}.png").is_ok());
    }

    #[test]
    fn manifest_pages_are_bounded_and_ordered() {
        let chunk = NonZeroU32::new(4).unwrap();
        let entries: Vec<FileEntry> = (0..10)
            .map(|i| {
                FileEntry::new(
                    &format!("f{i}"),
                    4,
                    0,
                    [0; DIGEST_LEN],
                    vec![[0; DIGEST_LEN]],
                )
                .unwrap()
            })
            .collect();
        let manifest = Manifest {
            chunk_size: chunk,
            entries,
        };
        let pages = manifest.pages(4);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].len(), 4);
        assert_eq!(pages[2].len(), 2);
        assert_eq!(pages.iter().map(|p| p.len()).sum::<usize>(), 10);
        assert_eq!(manifest.total_bytes(), 40);
        assert_eq!(manifest.file_count(), 10);
    }

    #[test]
    fn directory_entries_carry_no_digests_and_do_not_count_bytes() {
        let dir = FileEntry::directory("photos/empty").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.size, 0);
        assert!(dir.chunk_digests.is_empty());
        let manifest = Manifest {
            chunk_size: NonZeroU32::new(4).unwrap(),
            entries: vec![dir],
        };
        assert_eq!(manifest.total_bytes(), 0);
    }

    #[test]
    fn empty_manifest_has_one_empty_page() {
        let manifest = Manifest {
            chunk_size: NonZeroU32::new(4).unwrap(),
            entries: vec![],
        };
        assert_eq!(manifest.pages(4).len(), 1);
        assert!(manifest.pages(4)[0].is_empty());
    }
}
