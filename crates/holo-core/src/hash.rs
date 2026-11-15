//! Content hashing used to compare local files with remote hashes.

use base64::Engine as _;

use crate::model::{ContentHash, HashAlgo};

pub trait ContentHasher: Send {
    fn update(&mut self, data: &[u8]);
    fn finish(self: Box<Self>) -> ContentHash;
}

/// Hasher for `algo`, or `None` if not implemented yet.
pub fn hasher_for(algo: HashAlgo) -> Option<Box<dyn ContentHasher>> {
    match algo {
        HashAlgo::QuickXor => Some(Box::new(QuickXorHash::new())),
        _ => None,
    }
}

/// Hash a local file with `algo` (blocking).
pub fn hash_file(path: &std::path::Path, algo: HashAlgo) -> std::io::Result<Option<ContentHash>> {
    use std::io::Read;
    let Some(mut h) = hasher_for(algo) else {
        return Ok(None);
    };
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(h.finish()))
}

/// Microsoft's QuickXorHash (OneDrive / SharePoint), ported from the reference
/// implementation: <https://learn.microsoft.com/onedrive/developer/code-snippets/quickxorhash>.
#[derive(Debug, Clone)]
pub struct QuickXorHash {
    data: [u64; 3],
    length_so_far: u64,
    shift_so_far: usize,
}

impl QuickXorHash {
    const WIDTH_IN_BITS: usize = 160;
    const SHIFT: usize = 11;
    const BITS_IN_LAST_CELL: usize = Self::WIDTH_IN_BITS % 64; // 32

    pub fn new() -> Self {
        Self {
            data: [0; 3],
            length_so_far: 0,
            shift_so_far: 0,
        }
    }

    pub fn update(&mut self, array: &[u8]) {
        let cb_size = array.len();
        let mut vector_array_index = self.shift_so_far / 64;
        let mut vector_offset = self.shift_so_far % 64;
        let iterations = cb_size.min(Self::WIDTH_IN_BITS);
        let last = self.data.len() - 1;

        for i in 0..iterations {
            let is_last_cell = vector_array_index == last;
            let bits_in_cell = if is_last_cell {
                Self::BITS_IN_LAST_CELL
            } else {
                64
            };

            if vector_offset <= bits_in_cell - 8 {
                let mut j = i;
                while j < cb_size {
                    self.data[vector_array_index] ^= (array[j] as u64) << vector_offset;
                    j += Self::WIDTH_IN_BITS;
                }
            } else {
                let index1 = vector_array_index;
                let index2 = if is_last_cell {
                    0
                } else {
                    vector_array_index + 1
                };
                let low = bits_in_cell - vector_offset;
                let mut xored: u8 = 0;
                let mut j = i;
                while j < cb_size {
                    xored ^= array[j];
                    j += Self::WIDTH_IN_BITS;
                }
                self.data[index1] ^= (xored as u64) << vector_offset;
                self.data[index2] ^= (xored as u64) >> low;
            }

            vector_offset += Self::SHIFT;
            while vector_offset >= bits_in_cell {
                vector_array_index = if is_last_cell {
                    0
                } else {
                    vector_array_index + 1
                };
                vector_offset -= bits_in_cell;
            }
        }

        self.shift_so_far = (self.shift_so_far + Self::SHIFT * (cb_size % Self::WIDTH_IN_BITS))
            % Self::WIDTH_IN_BITS;
        self.length_so_far = self.length_so_far.wrapping_add(cb_size as u64);
    }

    pub fn finalize(&self) -> [u8; 20] {
        let mut rgb = [0u8; 20];
        rgb[0..8].copy_from_slice(&self.data[0].to_le_bytes());
        rgb[8..16].copy_from_slice(&self.data[1].to_le_bytes());
        rgb[16..20].copy_from_slice(&self.data[2].to_le_bytes()[..4]);
        let len = self.length_so_far.to_le_bytes();
        for (i, b) in len.iter().enumerate() {
            rgb[Self::WIDTH_IN_BITS / 8 - 8 + i] ^= b;
        }
        rgb
    }

    pub fn finalize_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.finalize())
    }
}

impl Default for QuickXorHash {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentHasher for QuickXorHash {
    fn update(&mut self, data: &[u8]) {
        QuickXorHash::update(self, data)
    }
    fn finish(self: Box<Self>) -> ContentHash {
        ContentHash {
            algo: HashAlgo::QuickXor,
            value: self.finalize_base64(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_shot(data: &[u8]) -> String {
        let mut h = QuickXorHash::new();
        h.update(data);
        h.finalize_base64()
    }

    #[test]
    fn empty_input_is_all_zero() {
        assert_eq!(one_shot(b""), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }

    #[test]
    fn single_byte_layout() {
        let mut h = QuickXorHash::new();
        h.update(b"a");
        let out = h.finalize();
        assert_eq!(out[0], b'a');
        assert_eq!(out[12], 1, "length xored into the last 8 bytes");
    }

    #[test]
    fn chunking_does_not_change_result() {
        let data: Vec<u8> = (0..10_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let expected = one_shot(&data);
        for chunk in [1usize, 7, 159, 160, 161, 320, 4096] {
            let mut h = QuickXorHash::new();
            for c in data.chunks(chunk) {
                h.update(c);
            }
            assert_eq!(h.finalize_base64(), expected, "chunk size {chunk}");
        }
    }
}
