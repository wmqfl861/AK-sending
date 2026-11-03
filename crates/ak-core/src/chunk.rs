//! Constant-memory chunk arithmetic. No file contents are read or allocated.
use std::num::NonZeroU32;
use std::ops::Range;

/// Fixed-size chunking of a byte length: pure arithmetic over
/// `total_bytes` and a nonzero `chunk_bytes`, no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPlan {
    total_bytes: u64,
    chunk_bytes: NonZeroU32,
}

impl ChunkPlan {
    /// The API requires a nonzero chunk size. This is not a file-size quota.
    pub fn new(total_bytes: u64, chunk_bytes: NonZeroU32) -> Self {
        Self {
            total_bytes,
            chunk_bytes,
        }
    }

    /// Total byte length the plan covers.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Number of chunks that cover `total_bytes` (ceiling division;
    /// zero for an empty input).
    pub fn chunk_count(&self) -> u64 {
        let size = u64::from(self.chunk_bytes.get());
        self.total_bytes / size + u64::from(!self.total_bytes.is_multiple_of(size))
    }

    /// Return an inclusive-start/exclusive-end byte range, without allocation.
    /// Avoid (length + chunk_size - 1), which can overflow at u64::MAX.
    pub fn range(&self, index: u64) -> Option<Range<u64>> {
        if index >= self.chunk_count() {
            return None;
        }
        let size = u64::from(self.chunk_bytes.get());
        let start = index.checked_mul(size)?;
        let length = size.min(self.total_bytes.checked_sub(start)?);
        Some(start..start.checked_add(length)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(bytes: u64, chunk: u32) -> ChunkPlan {
        ChunkPlan::new(bytes, NonZeroU32::new(chunk).unwrap())
    }

    #[test]
    fn empty_file_has_no_chunks() {
        let p = plan(0, 1024);
        assert_eq!(p.chunk_count(), 0);
        assert_eq!(p.range(0), None);
    }

    #[test]
    fn one_partial_chunk() {
        let p = plan(17, 1024);
        assert_eq!(p.chunk_count(), 1);
        assert_eq!(p.range(0), Some(0..17));
        assert_eq!(p.range(1), None);
    }

    #[test]
    fn exact_multiple_and_last_partial() {
        assert_eq!(plan(2048, 1024).range(1), Some(1024..2048));
        assert_eq!(plan(2050, 1024).range(2), Some(2048..2050));
    }

    #[test]
    fn handles_more_than_four_gibibytes() {
        let size = (1_u64 << 32) + 13;
        let p = plan(size, 1 << 20);
        assert_eq!(p.chunk_count(), 4097);
        assert_eq!(p.range(4096), Some((1_u64 << 32)..size));
    }

    #[test]
    fn maximum_u64_does_not_overflow() {
        let p = plan(u64::MAX, 4096);
        let last = p.range(p.chunk_count() - 1).unwrap();
        assert_eq!(last.end, u64::MAX);
        assert_eq!(last.end - last.start, 4095);
        assert_eq!(p.range(p.chunk_count()), None);
    }

    #[test]
    fn maximum_count_and_index_do_not_overflow() {
        let p = plan(u64::MAX, 1);
        assert_eq!(p.chunk_count(), u64::MAX);
        assert_eq!(p.range(u64::MAX - 1), Some((u64::MAX - 1)..u64::MAX));
        assert_eq!(p.range(u64::MAX), None);
    }

    #[test]
    fn ranges_are_contiguous_and_cover_small_inputs() {
        for size in 0..512 {
            for chunk in 1..33 {
                let p = plan(size, chunk);
                let mut end = 0;
                for index in 0..p.chunk_count() {
                    let r = p.range(index).unwrap();
                    assert_eq!(r.start, end);
                    assert!(r.end > r.start);
                    assert!(r.end - r.start <= u64::from(chunk));
                    end = r.end;
                }
                assert_eq!(end, size);
            }
        }
    }
}
