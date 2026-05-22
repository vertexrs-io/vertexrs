//! Benchmarks for low-level column storage: construction, sequential reads,
//! and dirty-bitmap propagation.
//!
//! Run with:
//!   cargo bench --bench column

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use vertexrs::column::{ChunkedColumn, CHUNK_SIZE};

const N: usize = 1_000_000;

// ── from_slice ────────────────────────────────────────────────────────────────

/// Measures the time to split a flat slice into CHUNK_SIZE-aligned chunks and
/// allocate an Arrow-backed buffer for each.  Parametrized over the four most
/// common numeric types.
fn bench_from_slice(c: &mut Criterion) {
    let data_f64: Vec<f64> = (0..N).map(|i| i as f64).collect();
    let data_f32: Vec<f32> = (0..N).map(|i| i as f32).collect();
    let data_i64: Vec<i64> = (0..N).map(|i| i as i64).collect();
    let data_i32: Vec<i32> = (0..N).map(|i| i as i32).collect();
    let data_u64: Vec<u64> = (0..N).map(|i| i as u64).collect();
    let data_u32: Vec<u32> = (0..N).map(|i| i as u32).collect();

    let mut group = c.benchmark_group("column_from_slice");
    group.throughput(Throughput::Elements(N as u64));

    group.bench_with_input(BenchmarkId::new("f64", N), &data_f64, |b, d| {
        b.iter(|| ChunkedColumn::<f64>::from_slice(d))
    });
    group.bench_with_input(BenchmarkId::new("f32", N), &data_f32, |b, d| {
        b.iter(|| ChunkedColumn::<f32>::from_slice(d))
    });
    group.bench_with_input(BenchmarkId::new("i64", N), &data_i64, |b, d| {
        b.iter(|| ChunkedColumn::<i64>::from_slice(d))
    });
    group.bench_with_input(BenchmarkId::new("i32", N), &data_i32, |b, d| {
        b.iter(|| ChunkedColumn::<i32>::from_slice(d))
    });
    group.bench_with_input(BenchmarkId::new("u64", N), &data_u64, |b, d| {
        b.iter(|| ChunkedColumn::<u64>::from_slice(d))
    });
    group.bench_with_input(BenchmarkId::new("u32", N), &data_u32, |b, d| {
        b.iter(|| ChunkedColumn::<u32>::from_slice(d))
    });

    group.finish();
}

// ── Sequential read ───────────────────────────────────────────────────────────

/// Measures sequential read throughput: iterate every chunk and every element,
/// accumulating into a sum that prevents the loop from being optimised away.
fn bench_read_throughput(c: &mut Criterion) {
    let data: Vec<f64> = (0..N).map(|i| i as f64).collect();
    let col = ChunkedColumn::<f64>::from_slice(&data);

    let mut group = c.benchmark_group("column_read_throughput");
    group.throughput(Throughput::Elements(N as u64));

    group.bench_function("f64_1m", |b| {
        b.iter(|| {
            let mut sum = 0.0_f64;
            for chunk in col.iter_chunks() {
                for &v in chunk.values() {
                    sum += v;
                }
            }
            std::hint::black_box(sum)
        })
    });

    group.finish();
}

// ── Dirty-bitmap operations ───────────────────────────────────────────────────

/// Measures the cost of dirty-bitmap operations that drive incremental
/// recomputation: marking a range dirty, and clearing all dirty flags.
fn bench_dirty_propagation(c: &mut Criterion) {
    // Pre-allocate the column outside the timed section; we only measure bitmap
    // operations here, not the alloc cost (that is covered by bench_from_slice).
    let data: Vec<f64> = (0..N).map(|i| i as f64).collect();
    let clean_col = ChunkedColumn::<f64>::from_slice(&data);

    let pct_1 = N / 100;
    let pct_10 = N / 10;
    let chunks_total = (N + CHUNK_SIZE - 1) / CHUNK_SIZE;

    let mut group = c.benchmark_group("column_dirty");
    group.throughput(Throughput::Elements(chunks_total as u64));

    // Mark 1% of rows (10 chunks) dirty then clear.
    group.bench_function("mark_1pct_and_clear", |b| {
        b.iter_batched(
            || clean_col.clone(),
            |mut col| {
                col.mark_dirty(0..pct_1);
                col.clear_dirty();
                col
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Mark 10% of rows dirty then clear.
    group.bench_function("mark_10pct_and_clear", |b| {
        b.iter_batched(
            || clean_col.clone(),
            |mut col| {
                col.mark_dirty(0..pct_10);
                col.clear_dirty();
                col
            },
            criterion::BatchSize::SmallInput,
        )
    });

    // Mark all rows dirty then clear (worst case).
    group.bench_function("mark_all_and_clear", |b| {
        b.iter_batched(
            || clean_col.clone(),
            |mut col| {
                col.mark_dirty(0..N);
                col.clear_dirty();
                col
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_from_slice, bench_read_throughput, bench_dirty_propagation);
criterion_main!(benches);
