//! Criterion benchmarks for the publish-side chunk+hash path.
//!
//! Synthetic input files are generated into a temp dir from a fixed seed at
//! bench startup (never committed, never held in memory). Sizes: 4 MiB and
//! 64 MiB always; 512 MiB only under `cargo bench` (skipped in `--test`
//! validation mode so `cargo test --all-targets` stays fast) and skippable
//! entirely with TAZAMUN_BENCH_SKIP_LARGE=1.

use std::io::{BufReader, Write};
use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const MIB: usize = 1024 * 1024;

/// Deterministic xorshift byte stream, written to disk in 8 MiB slabs so the
/// generator itself never holds a large buffer.
fn write_seeded_file(path: &PathBuf, size: usize, seed: u64) {
    let mut x = seed | 1;
    let file = std::fs::File::create(path).expect("create bench file");
    let mut w = std::io::BufWriter::new(file);
    let mut written = 0usize;
    let mut slab = Vec::with_capacity(8 * MIB);
    while written < size {
        slab.clear();
        let want = (size - written).min(8 * MIB);
        while slab.len() < want {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            slab.extend_from_slice(&x.to_le_bytes());
        }
        slab.truncate(want);
        w.write_all(&slab).expect("write bench file");
        written += want;
    }
    w.flush().expect("flush bench file");
}

/// True when the binary is being run by `cargo test` (criterion validation
/// mode); the 512 MiB case is bench-only.
fn in_test_mode() -> bool {
    std::env::args().any(|a| a == "--test")
}

fn bench_chunk_stream(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("bench tempdir");
    let mut sizes: Vec<usize> = vec![4 * MIB, 64 * MIB];
    if !in_test_mode() && std::env::var_os("TAZAMUN_BENCH_SKIP_LARGE").is_none() {
        sizes.push(512 * MIB);
    }

    let mut group = c.benchmark_group("chunk_stream");
    for size in sizes {
        let label = format!("{}MiB", size / MIB);
        let path = dir.path().join(&label);
        write_seeded_file(&path, size, 0x00C0_FFEE_D15E_A5E5);
        group.throughput(Throughput::Bytes(size as u64));
        // Fewer samples for the big cases: each iteration reads the whole file.
        group.sample_size(if size > 4 * MIB { 10 } else { 30 });
        // publish_local's actual path: the overlapped file pipeline.
        group.bench_function(&label, |b| {
            b.iter(|| {
                let (refs, total) =
                    tazamun::sync::chunker::chunk_file(&path, |_, _| Ok(())).expect("chunk");
                assert_eq!(total, size as u64);
                std::hint::black_box(refs)
            });
        });
    }
    group.finish();
}

/// Cut-only scan (no hashing): the inherently sequential part of the pipeline,
/// i.e. the theoretical ceiling for parallel-hash speedups.
fn bench_cut_only(c: &mut Criterion) {
    use fastcdc::v2020::StreamCDC;
    let dir = tempfile::tempdir().expect("bench tempdir");
    let size = 64 * MIB;
    let path = dir.path().join("cut64MiB");
    write_seeded_file(&path, size, 0x00C0_FFEE_D15E_A5E5);

    let mut group = c.benchmark_group("cut_only");
    group.throughput(Throughput::Bytes(size as u64));
    group.sample_size(10);
    group.bench_function("64MiB", |b| {
        b.iter(|| {
            let file = std::fs::File::open(&path).expect("open bench file");
            let mut n = 0usize;
            let mut bytes = 0u64;
            for chunk in StreamCDC::new(
                BufReader::new(file),
                tazamun::consts::CDC_MIN as usize,
                tazamun::consts::CDC_AVG as usize,
                tazamun::consts::CDC_MAX as usize,
            ) {
                let chunk = chunk.expect("cut");
                n += 1;
                bytes += chunk.data.len() as u64;
            }
            assert_eq!(bytes, size as u64);
            std::hint::black_box(n)
        });
    });
    group.finish();
}

/// The absolute sequential floor: a pure FastCDC slice scan over in-memory
/// data — no I/O, no hashing, no chunk copies. Parallel-hash speedups can
/// never push the pipeline below this line (Amdahl).
fn bench_scan_only_slice(c: &mut Criterion) {
    let size = 64 * MIB;
    let mut x: u64 = 0x00C0_FFEE_D15E_A5E5 | 1;
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        data.extend_from_slice(&x.to_le_bytes());
    }
    data.truncate(size);

    let mut group = c.benchmark_group("scan_only_slice");
    group.throughput(Throughput::Bytes(size as u64));
    group.sample_size(20);
    group.bench_function("64MiB", |b| {
        b.iter(|| {
            let n = fastcdc::v2020::FastCDC::new(
                &data,
                tazamun::consts::CDC_MIN as usize,
                tazamun::consts::CDC_AVG as usize,
                tazamun::consts::CDC_MAX as usize,
            )
            .count();
            std::hint::black_box(n)
        });
    });
    group.finish();
}

/// Alternative under evaluation: sequential per-chunk hashing but with
/// blake3's internal rayon parallelism (`update_rayon`) inside each chunk.
fn bench_blake3_update_rayon(c: &mut Criterion) {
    use fastcdc::v2020::StreamCDC;
    let dir = tempfile::tempdir().expect("bench tempdir");
    let size = 64 * MIB;
    let path = dir.path().join("b3rayon64MiB");
    write_seeded_file(&path, size, 0x00C0_FFEE_D15E_A5E5);

    let mut group = c.benchmark_group("blake3_update_rayon_per_chunk");
    group.throughput(Throughput::Bytes(size as u64));
    group.sample_size(10);
    group.bench_function("64MiB", |b| {
        b.iter(|| {
            let file = std::fs::File::open(&path).expect("open bench file");
            let mut refs = Vec::new();
            for chunk in StreamCDC::new(
                BufReader::new(file),
                tazamun::consts::CDC_MIN as usize,
                tazamun::consts::CDC_AVG as usize,
                tazamun::consts::CDC_MAX as usize,
            ) {
                let chunk = chunk.expect("cut");
                let mut hasher = blake3::Hasher::new();
                hasher.update_rayon(&chunk.data);
                refs.push((*hasher.finalize().as_bytes(), chunk.data.len() as u32));
            }
            std::hint::black_box(refs)
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_chunk_stream,
    bench_cut_only,
    bench_scan_only_slice,
    bench_blake3_update_rayon
);
criterion_main!(benches);
