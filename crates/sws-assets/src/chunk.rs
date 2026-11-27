//! Chunking plan for D1 blob rows.

use crate::limits::CHUNK_BYTES;

/// `(seq, start, end)` byte ranges covering `len` bytes in [`CHUNK_BYTES`] pieces.
pub fn plan(len: usize) -> Vec<(u32, usize, usize)> {
    plan_with(len, CHUNK_BYTES)
}

pub fn plan_with(len: usize, chunk: usize) -> Vec<(u32, usize, usize)> {
    assert!(chunk > 0);
    (0..len.div_ceil(chunk))
        .map(|i| (i as u32, i * chunk, ((i + 1) * chunk).min(len)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_exactly_without_overlap() {
        for len in [
            0,
            1,
            CHUNK_BYTES - 1,
            CHUNK_BYTES,
            CHUNK_BYTES + 1,
            1024 * 1024,
        ] {
            let p = plan(len);
            let mut next = 0;
            for (i, (seq, s, e)) in p.iter().enumerate() {
                assert_eq!(*seq as usize, i);
                assert_eq!(*s, next);
                assert!(e - s <= CHUNK_BYTES && e > s);
                next = *e;
            }
            assert_eq!(next, len);
        }
        assert_eq!(plan(1024 * 1024).len(), 4);
    }
}
