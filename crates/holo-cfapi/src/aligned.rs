//! Cloud Filter transfer alignment rules, kept pure for testing.
//!
//! `CfExecute(TRANSFER_DATA)` requires the offset to be 4 KiB aligned and the length
//! to be a multiple of 4 KiB, except for a final chunk that ends at EOF.

use std::ops::Range;

pub const PAGE: u64 = 4096;

fn floor(v: u64) -> u64 {
    v / PAGE * PAGE
}
fn ceil(v: u64) -> u64 {
    v.div_ceil(PAGE) * PAGE
}

/// Expand a requested range to legal transfer boundaries, prefetching at least
/// `min_fetch` bytes (fewer round trips for sequential readers).
pub fn align_range(required: Range<u64>, file_size: u64, min_fetch: u64) -> Range<u64> {
    let start = floor(required.start.min(file_size));
    let end = required.end.max(start.saturating_add(min_fetch));
    let end = ceil(end).min(file_size);
    start..end.max(start)
}

/// Buffers sequential bytes and emits only legal transfer writes.
pub struct AlignedWriter<W: FnMut(u64, &[u8]) -> std::io::Result<()>> {
    write: W,
    offset: u64,
    end: u64,
    file_size: u64,
    buf: Vec<u8>,
    chunk: usize,
}

impl<W: FnMut(u64, &[u8]) -> std::io::Result<()>> AlignedWriter<W> {
    /// `range` must come from [`align_range`]. `chunk` is rounded to a page multiple.
    pub fn new(range: Range<u64>, file_size: u64, chunk: usize, write: W) -> Self {
        debug_assert_eq!(range.start % PAGE, 0);
        let chunk = (chunk.max(PAGE as usize) / PAGE as usize) * PAGE as usize;
        Self {
            write,
            offset: range.start,
            end: range.end,
            file_size,
            buf: Vec::with_capacity(chunk),
            chunk,
        }
    }

    pub fn written(&self) -> u64 {
        self.offset
    }

    pub fn push(&mut self, mut data: &[u8]) -> std::io::Result<()> {
        // Never write past the requested range even if the server sends more.
        let room = (self.end - self.offset) as usize - self.buf.len();
        if data.len() > room {
            data = &data[..room];
        }
        while !data.is_empty() {
            let take = (self.chunk - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == self.chunk {
                self.flush_full()?;
            }
        }
        Ok(())
    }

    fn flush_full(&mut self) -> std::io::Result<()> {
        (self.write)(self.offset, &self.buf)?;
        self.offset += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    /// Flush the tail. Errors if the stream ended early.
    pub fn finish(mut self) -> std::io::Result<u64> {
        let total = self.offset + self.buf.len() as u64;
        if total < self.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("download ended at {total}, expected {}", self.end),
            ));
        }
        if !self.buf.is_empty() {
            let at_eof = total == self.file_size;
            if !at_eof && !(self.buf.len() as u64).is_multiple_of(PAGE) {
                return Err(std::io::Error::other("unaligned tail before EOF"));
            }
            self.flush_full()?;
        }
        Ok(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_and_prefetches() {
        assert_eq!(align_range(5000..6000, 1 << 30, 0), 4096..8192);
        assert_eq!(align_range(0..10, 100, 0), 0..100);
        assert_eq!(align_range(0..10, 1 << 30, 1 << 20), 0..(1 << 20));
        assert_eq!(align_range(8192..9000, 9000, 1 << 20), 8192..9000);
    }

    fn run(range: Range<u64>, size: u64, chunk: usize, pieces: &[usize]) -> Vec<(u64, usize)> {
        let mut writes = Vec::new();
        let mut w = AlignedWriter::new(range, size, chunk, |off, d| {
            writes.push((off, d.len()));
            Ok(())
        });
        for p in pieces {
            w.push(&vec![0u8; *p]).unwrap();
        }
        w.finish().unwrap();
        writes
    }

    #[test]
    fn writes_are_page_aligned_except_eof_tail() {
        let size = 3 * 4096 + 100;
        let writes = run(0..size, size, 8192, &[1000, 9000, 2000, 388]);
        assert_eq!(writes, vec![(0, 8192), (8192, 4196)]);
        for (off, _) in &writes {
            assert_eq!(off % PAGE, 0);
        }
    }

    #[test]
    fn truncates_excess_and_detects_short_stream() {
        let writes = run(4096..8192, 1 << 20, 4096, &[10_000]);
        assert_eq!(writes, vec![(4096, 4096)]);

        let mut w = AlignedWriter::new(0..8192, 1 << 20, 4096, |_, _| Ok(()));
        w.push(&[0; 100]).unwrap();
        assert!(w.finish().is_err());
    }
}
