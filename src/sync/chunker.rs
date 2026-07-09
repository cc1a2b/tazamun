//! Content-defined chunking (FastCDC 2020) with BLAKE3 chunk hashes.
//!
//! Invariant: the cut function is pure and deterministic — identical bytes
//! always produce the identical `Vec<ChunkRef>`, regardless of whether they
//! arrive as a slice or as a stream, and regardless of how many hashing
//! threads run. Cut-point scanning is inherently sequential (a rolling
//! computation); per-chunk BLAKE3 hashing and buffer copies run on a rayon
//! pool in order-preserving batches, bounded by a fixed streaming window so
//! memory stays flat no matter the file size.

use std::io::Read;
use std::sync::OnceLock;

use rayon::prelude::*;

use crate::consts::{CDC_AVG, CDC_MAX, CDC_MIN};
use crate::proto::ChunkRef;

/// Streaming window: a cut starting at `offset` is final once `CDC_MAX`
/// lookahead bytes exist past it (the scanner never looks further), so any
/// window ≥ 2×`CDC_MAX` yields cut points byte-identical to scanning the whole
/// input as one slice. 8 MiB keeps the per-pass parallel hash batches large
/// while bounding peak memory.
const STREAM_WINDOW: usize = 8 * 1024 * 1024;

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

/// One window's hashed chunks, in file order.
type HashedBatch = Vec<(ChunkRef, Vec<u8>)>;
/// Completed batches waiting for their turn in the ordered emit.
type PendingBatches = std::collections::BTreeMap<u64, HashedBatch>;
/// Borrowed sink used by the ordered-emit helper.
type DynSink<'a> = &'a mut dyn FnMut(&ChunkRef, Vec<u8>) -> Result<(), ChunkError>;

/// Emits every batch whose sequence number is next in line.
fn emit_ready(
    pending: &mut PendingBatches,
    seq_next_emit: &mut u64,
    refs: &mut Vec<ChunkRef>,
    total: &mut u64,
    sink: DynSink<'_>,
) -> Result<(), ChunkError> {
    while let Some(batch) = pending.remove(seq_next_emit) {
        *seq_next_emit += 1;
        for (r, data) in batch {
            *total += u64::from(r.len);
            refs.push(r);
            sink(&r, data)?;
        }
    }
    Ok(())
}

/// The shared hashing pool, overridable with `TAZAMUN_THREADS`.
///
/// Default: `min(available cores, 4)`. Measured on the 64 MiB bench, BLAKE3
/// (~4.7 GiB/s per thread) saturates the 2.8 GiB/s sequential cut scan with
/// 1–2 threads; beyond 4 the extra hashers only steal cycles from the scan
/// thread and the numbers get *worse* (26.1 ms at ≤4 threads vs 31.7 ms at
/// 16). `None` means pool construction failed (e.g. OS thread limits) and
/// callers fall back to sequential hashing — results are identical either way.
fn hash_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::env::var("TAZAMUN_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
                    .min(4)
            });
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("tazamun-hash-{i}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// Hashes (and, when `copy` is set, clones out) the given cut ranges of
/// `buf`, preserving input order. Parallel when the pool is available.
fn hash_batch(buf: &[u8], cuts: &[(usize, usize)], copy: bool) -> Vec<(ChunkRef, Vec<u8>)> {
    let one = |&(offset, len): &(usize, usize)| {
        let slice = &buf[offset..offset + len];
        let data = if copy { slice.to_vec() } else { Vec::new() };
        (chunk_ref(slice), data)
    };
    match hash_pool() {
        Some(pool) => pool.install(|| cuts.par_iter().map(one).collect()),
        None => cuts.iter().map(one).collect(),
    }
}

/// Pure, deterministic cut function over an in-memory slice.
///
/// An empty input produces zero chunks.
pub fn chunk_bytes(data: &[u8]) -> Vec<ChunkRef> {
    if data.is_empty() {
        return Vec::new();
    }
    let cuts: Vec<(usize, usize)> =
        fastcdc::v2020::FastCDC::new(data, CDC_MIN as usize, CDC_AVG as usize, CDC_MAX as usize)
            .map(|c| (c.offset, c.length))
            .collect();
    hash_batch(data, &cuts, false)
        .into_iter()
        .map(|(r, _)| r)
        .collect()
}

/// Streams a reader through FastCDC, invoking `sink` with each chunk's bytes
/// and returning the full chunk list plus the total size.
///
/// The cuts are identical to [`chunk_bytes`] over the same content.
pub fn chunk_stream<R: Read>(
    reader: R,
    sink: impl FnMut(&ChunkRef, Vec<u8>) -> Result<(), ChunkError>,
) -> Result<(Vec<ChunkRef>, u64), ChunkError> {
    chunk_stream_windowed(reader, STREAM_WINDOW, sink)
}

/// Fast path for regular files: a reader thread fills window buffers ahead of
/// the scan, the calling thread runs only the (inherently sequential) cut
/// scan plus in-order emission, and hash+copy batches execute on the rayon
/// pool. Cut points and hashes are byte-identical to [`chunk_bytes`] and
/// [`chunk_stream`]; only the wall-clock changes. Memory is bounded by a
/// handful of recycled window buffers regardless of file size.
pub fn chunk_file(
    path: &std::path::Path,
    mut sink: impl FnMut(&ChunkRef, Vec<u8>) -> Result<(), ChunkError>,
) -> Result<(Vec<ChunkRef>, u64), ChunkError> {
    use std::sync::mpsc;

    // Segment read per window; CARRY reserves room for the unfinalized tail
    // (< 2×CDC_MAX) carried into the next window.
    const SEG: usize = 4 * 1024 * 1024;
    const CARRY: usize = 2 * (CDC_MAX as usize);
    const BUFS: usize = 3;

    struct Filled {
        buf: Vec<u8>,
        /// Bytes the reader placed at `CARRY..CARRY + len`.
        len: usize,
        eof: bool,
    }

    let file = std::fs::File::open(path)?;

    // recycle: scanner/hasher → reader · filled: reader → scanner.
    let (recycle_tx, recycle_rx) = mpsc::sync_channel::<Vec<u8>>(BUFS);
    let (filled_tx, filled_rx) = mpsc::sync_channel::<std::io::Result<Filled>>(BUFS);
    for _ in 0..BUFS {
        // The reader only ever touches CARRY..; the scanner initializes the
        // carry region before use.
        let _ = recycle_tx.send(vec![0u8; CARRY + SEG]);
    }

    let reader_thread = std::thread::spawn(move || {
        use std::io::Read;
        let mut file = file;
        loop {
            let Ok(mut buf) = recycle_rx.recv() else {
                return; // consumer hung up: done or aborted
            };
            let mut len = 0usize;
            let mut eof = false;
            while CARRY + len < buf.len() {
                match file.read(&mut buf[CARRY + len..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => len += n,
                    Err(e) => {
                        let _ = filled_tx.send(Err(e));
                        return;
                    }
                }
            }
            if filled_tx.send(Ok(Filled { buf, len, eof })).is_err() {
                return;
            }
            if eof {
                return;
            }
        }
    });

    // Hash results come back tagged with a sequence number for ordered emit.
    // Window buffers are recycled straight from the hash tasks so the reader
    // never waits on the scanner's emit loop (no three-way deadlock).
    let (done_tx, done_rx) = mpsc::channel::<(u64, HashedBatch)>();

    let mut refs: Vec<ChunkRef> = Vec::new();
    let mut total: u64 = 0;
    let mut carry: Vec<u8> = Vec::new(); // unfinalized tail, < CARRY bytes
    let mut seq_next_dispatch: u64 = 0;
    let mut seq_next_emit: u64 = 0;
    let mut pending: PendingBatches = PendingBatches::new();

    let result: Result<(), ChunkError> = (|| {
        loop {
            let filled = match filled_rx.recv() {
                Ok(Ok(f)) => f,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    // Reader gone without EOF marker: treat as clean EOF of
                    // an empty tail (happens only for zero-length files after
                    // the first segment).
                    break;
                }
            };
            let mut buf = filled.buf;
            let start = CARRY - carry.len();
            buf[start..CARRY].copy_from_slice(&carry);
            let view_len = CARRY + filled.len;
            let view = &buf[start..view_len];
            let eof = filled.eof;

            let mut cuts: Vec<(usize, usize)> = Vec::new(); // absolute in `buf`
            let mut consumed_abs = start;
            for c in fastcdc::v2020::FastCDC::new(
                view,
                CDC_MIN as usize,
                CDC_AVG as usize,
                CDC_MAX as usize,
            ) {
                if eof || c.offset + CDC_MAX as usize <= view.len() {
                    cuts.push((start + c.offset, c.length));
                    consumed_abs = start + c.offset + c.length;
                } else {
                    break;
                }
            }
            carry.clear();
            carry.extend_from_slice(&buf[consumed_abs..view_len]);

            if cuts.is_empty() {
                // Zero-length file (or logic regression): recycle and finish.
                if eof && view.is_empty() {
                    break;
                }
                return Err(ChunkError::Io(std::io::Error::other(
                    "chunk window produced no finalized cut",
                )));
            }

            let seq = seq_next_dispatch;
            seq_next_dispatch += 1;
            let done = done_tx.clone();
            let recycle = recycle_tx.clone();
            match hash_pool() {
                Some(pool) => pool.spawn(move || {
                    let batch: Vec<(ChunkRef, Vec<u8>)> = cuts
                        .par_iter()
                        .map(|&(o, l)| {
                            let s = &buf[o..o + l];
                            (chunk_ref(s), s.to_vec())
                        })
                        .collect();
                    let _ = recycle.send(buf);
                    let _ = done.send((seq, batch));
                }),
                None => {
                    let batch: Vec<(ChunkRef, Vec<u8>)> = cuts
                        .iter()
                        .map(|&(o, l)| {
                            let s = &buf[o..o + l];
                            (chunk_ref(s), s.to_vec())
                        })
                        .collect();
                    let _ = recycle.send(buf);
                    let _ = done_tx.send((seq, batch));
                }
            }

            // Opportunistically emit whatever has completed while the reader
            // and hash pool keep running ahead.
            while let Ok((s, batch)) = done_rx.try_recv() {
                pending.insert(s, batch);
            }
            emit_ready(
                &mut pending,
                &mut seq_next_emit,
                &mut refs,
                &mut total,
                &mut sink,
            )?;

            if eof {
                break;
            }
        }

        // Drain the outstanding batches in sequence order.
        while seq_next_emit < seq_next_dispatch {
            match done_rx.recv() {
                Ok((s, batch)) => {
                    pending.insert(s, batch);
                    emit_ready(
                        &mut pending,
                        &mut seq_next_emit,
                        &mut refs,
                        &mut total,
                        &mut sink,
                    )?;
                }
                Err(_) => {
                    return Err(ChunkError::Io(std::io::Error::other(
                        "hash stage ended before all batches were emitted",
                    )));
                }
            }
        }
        Ok(())
    })();

    // Tear down: closing the recycle channel stops the reader.
    drop(recycle_tx);
    drop(done_tx);
    let _ = reader_thread.join();
    result.map(|()| (refs, total))
}

/// Windowed implementation: fill → scan finalized cuts (sequential) →
/// hash+copy the batch in parallel (order preserved) → emit → slide the
/// remainder to the front and refill.
fn chunk_stream_windowed<R: Read>(
    mut reader: R,
    window: usize,
    mut sink: impl FnMut(&ChunkRef, Vec<u8>) -> Result<(), ChunkError>,
) -> Result<(Vec<ChunkRef>, u64), ChunkError> {
    let mut buf = vec![0u8; window.max(2 * CDC_MAX as usize)];
    let mut filled = 0usize;
    let mut eof = false;
    let mut refs: Vec<ChunkRef> = Vec::new();
    let mut total: u64 = 0;

    loop {
        while filled < buf.len() && !eof {
            let n = reader.read(&mut buf[filled..])?;
            if n == 0 {
                eof = true;
            } else {
                filled += n;
            }
        }
        if filled == 0 {
            break;
        }

        let mut cuts: Vec<(usize, usize)> = Vec::new();
        let mut consumed = 0usize;
        for c in fastcdc::v2020::FastCDC::new(
            &buf[..filled],
            CDC_MIN as usize,
            CDC_AVG as usize,
            CDC_MAX as usize,
        ) {
            if eof || c.offset + CDC_MAX as usize <= filled {
                cuts.push((c.offset, c.length));
                consumed = c.offset + c.length;
            } else {
                break;
            }
        }
        if cuts.is_empty() {
            // Unreachable with the ≥2×CDC_MAX buffer: a full buffer always
            // finalizes the first cut and EOF finalizes everything. Fail
            // loudly rather than loop forever if that invariant regresses.
            return Err(ChunkError::Io(std::io::Error::other(
                "chunk window produced no finalized cut",
            )));
        }

        for (r, data) in hash_batch(&buf[..filled], &cuts, true) {
            total += u64::from(r.len);
            refs.push(r);
            sink(&r, data)?;
        }

        buf.copy_within(consumed..filled, 0);
        filled -= consumed;
        if eof && filled == 0 {
            break;
        }
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
    fn windowed_stream_matches_slice_across_boundaries() {
        // A window barely above the 2×CDC_MAX floor forces many refills, so
        // cut finalization at window edges is exercised heavily.
        let data = pseudo_random(5 * 1024 * 1024);
        let whole = chunk_bytes(&data);
        let small_window = 2 * CDC_MAX as usize + 4096;
        let mut sunk = Vec::new();
        let (refs, total) =
            chunk_stream_windowed(std::io::Cursor::new(&data), small_window, |r, bytes| {
                sunk.push((*r, bytes));
                Ok(())
            })
            .unwrap();
        assert_eq!(refs, whole);
        assert_eq!(total, data.len() as u64);
        // The sink received the exact bytes, in order.
        let mut rebuilt = Vec::with_capacity(data.len());
        for (r, bytes) in sunk {
            assert_eq!(bytes.len() as u32, r.len);
            rebuilt.extend_from_slice(&bytes);
        }
        assert_eq!(rebuilt, data);
    }

    #[test]
    fn chunk_file_matches_slice_and_stream() {
        // The overlapped file pipeline must produce byte-identical refs and
        // sink bytes across many windows (17 MiB > 4 SEGs).
        let data = pseudo_random(17 * 1024 * 1024 + 12345);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, &data).unwrap();

        let whole = chunk_bytes(&data);
        let mut rebuilt = Vec::with_capacity(data.len());
        let (refs, total) = chunk_file(&path, |r, bytes| {
            assert_eq!(bytes.len() as u32, r.len);
            rebuilt.extend_from_slice(&bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(refs, whole);
        assert_eq!(total, data.len() as u64);
        assert_eq!(rebuilt, data);

        // Empty file: zero chunks, zero bytes.
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        let (refs, total) = chunk_file(&empty, |_, _| Ok(())).unwrap();
        assert!(refs.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn short_reads_do_not_change_cuts() {
        // A reader that trickles 7 bytes at a time exercises the top-up loop.
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(out.len()).min(7);
                out[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let data = pseudo_random(700 * 1024);
        let (a, _) = chunk_stream(Trickle(&data), |_, _| Ok(())).unwrap();
        assert_eq!(a, chunk_bytes(&data));
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
