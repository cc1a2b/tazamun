//! Content-defined chunking (FastCDC 2020) with BLAKE3 chunk hashes.
//!
//! Invariant: the cut function is pure and deterministic — identical bytes
//! always produce the identical `Vec<ChunkRef>`, regardless of whether they
//! arrive as a slice or as a stream.

use std::io::Read;

use crate::consts::{CDC_AVG, CDC_MAX, CDC_MIN};
use crate::proto::ChunkRef;

#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error("chunking io: {0}")]
    Io(#[from] std::io::Error),
}

fn chunk_ref(data: &[u8]) -> ChunkRef {
    ChunkRef {
        hash: *blake3::hash(data).as_bytes(),
        len: data.len() as u32,
    }
}

/// Pure, deterministic cut function over an in-memory slice.
///
/// An empty input produces zero chunks.
pub fn chunk_bytes(data: &[u8]) -> Vec<ChunkRef> {
    if data.is_empty() {
        return Vec::new();
    }
    fastcdc::v2020::FastCDC::new(data, CDC_MIN as usize, CDC_AVG as usize, CDC_MAX as usize)
        .map(|c| chunk_ref(&data[c.offset..c.offset + c.length]))
        .collect()
}

/// Streams a reader through FastCDC, invoking `sink` with each chunk's bytes
/// and returning the full chunk list plus the total size.
///
/// The cuts are identical to [`chunk_bytes`] over the same content.
pub fn chunk_stream<R: Read>(
    reader: R,
    mut sink: impl FnMut(&ChunkRef, Vec<u8>) -> Result<(), ChunkError>,
) -> Result<(Vec<ChunkRef>, u64), ChunkError> {
    let mut refs = Vec::new();
    let mut total: u64 = 0;
    for chunk in
        fastcdc::v2020::StreamCDC::new(reader, CDC_MIN as usize, CDC_AVG as usize, CDC_MAX as usize)
    {
        let chunk = chunk.map_err(std::io::Error::other)?;
        let r = chunk_ref(&chunk.data);
        total += u64::from(r.len);
        refs.push(r);
        sink(&r, chunk.data)?;
    }
    Ok((refs, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo_random(n: usize) -> Vec<u8> {
        // Deterministic xorshift so cuts are stable across test runs.
        let mut x: u64 = 0x243F_6A88_85A3_08D3;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(n);
        out
    }

    #[test]
    fn deterministic_cuts() {
        let data = pseudo_random(3 * 1024 * 1024);
        let a = chunk_bytes(&data);
        let b = chunk_bytes(&data);
        assert_eq!(a, b);
        assert!(a.len() > 1);
        // Streaming over the same bytes yields the same cuts and hashes.
        let (c, total) = chunk_stream(std::io::Cursor::new(&data), |_, _| Ok(())).unwrap();
        assert_eq!(a, c);
        assert_eq!(total, data.len() as u64);
        // Every chunk respects the configured bounds (the tail may be short).
        for r in &a[..a.len() - 1] {
            assert!(r.len >= CDC_MIN && r.len <= CDC_MAX, "len {}", r.len);
        }
        assert_eq!(
            a.iter().map(|r| u64::from(r.len)).sum::<u64>(),
            data.len() as u64
        );
    }

    #[test]
    fn empty_file() {
        assert!(chunk_bytes(&[]).is_empty());
        let (refs, total) =
            chunk_stream(std::io::Cursor::new(Vec::<u8>::new()), |_, _| Ok(())).unwrap();
        assert!(refs.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn one_byte() {
        let refs = chunk_bytes(&[0x5A]);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].len, 1);
        assert_eq!(refs[0].hash, *blake3::hash(&[0x5A]).as_bytes());
    }

    #[test]
    fn max_boundary() {
        let exactly = pseudo_random(CDC_MAX as usize);
        let refs = chunk_bytes(&exactly);
        assert_eq!(
            refs.iter().map(|r| u64::from(r.len)).sum::<u64>(),
            exactly.len() as u64
        );
        for r in &refs {
            assert!(r.len <= CDC_MAX);
        }

        let over = pseudo_random(CDC_MAX as usize + 1);
        let refs = chunk_bytes(&over);
        assert!(refs.len() >= 2, "CDC_MAX+1 bytes must split");
        assert_eq!(
            refs.iter().map(|r| u64::from(r.len)).sum::<u64>(),
            over.len() as u64
        );
        for r in &refs {
            assert!(r.len <= CDC_MAX);
        }
    }

    #[test]
    fn small_edit_preserves_most_chunks() {
        // The delta-sync property in miniature: flip one megabyte in the
        // middle of 8 MiB and most chunk hashes must survive.
        let mut data = pseudo_random(8 * 1024 * 1024);
        let before = chunk_bytes(&data);
        let start = 4 * 1024 * 1024;
        for b in &mut data[start..start + 1024 * 1024] {
            *b ^= 0xFF;
        }
        let after = chunk_bytes(&data);
        let before_set: std::collections::HashSet<[u8; 32]> =
            before.iter().map(|r| r.hash).collect();
        let reused: u64 = after
            .iter()
            .filter(|r| before_set.contains(&r.hash))
            .map(|r| u64::from(r.len))
            .sum();
        assert!(
            reused as f64 > 0.6 * data.len() as f64,
            "only {reused} of {} bytes reused",
            data.len()
        );
    }
}
