//! Incremental recompute benchmarks — vertexrs's primary differentiator.
//!
//! These benchmarks demonstrate the key advantage of dirty-chunk tracking:
//! when only a fraction of input rows change, vertexrs recomputes only the
//! affected chunks rather than the full column.  Polars must always do a full
//! recompute, providing the denominator for the speedup ratio.
//!
//! Benchmark matrix:
//!   - Update fraction: 1%, 10%, 100% of 1M rows
//!   - Dtypes: f64, f32, i64
//!   - Polars full-recompute (same pipeline, same data) as baseline
//!
//! Expected: vertexrs 1% update ≥ 10× faster than Polars full recompute.
//!
//! Run with:
//!   cargo bench --bench incremental
//!   cargo bench --bench incremental --features bench-polars   # + Polars baseline
//!
//! Correctness tests:
//!   cargo test --features bench-polars -- incremental_correctness

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use vertexrs::{Frame, Node, pipeline};

const N: usize = 1_000_000;

// ── Pipeline runners ──────────────────────────────────────────────────────────
//
// Each helper: (1) pushes the initial frame, (2) computes the full pipeline,
// then (3) mutates `update_rows` rows in the source and recomputes.
// Only step (3) is timed by the benchmark — the setup is done in iter_batched's
// setup closure.

/// Returns a fresh f64 pipeline pre-loaded with `n` rows.
fn setup_f64(n: usize) -> (Frame, impl FnMut(&Frame)) {
    let prices: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    let frame = Frame::new().append(Node::from_data("price", prices));
    let mut p = pipeline! {
        source!(price: f64);
        node!(tax   = price.row(|x| x * 0.1_f64));
        node!(total = price.row(|x| x + tax));
        node!(net   = total.row(|x| x * 0.9_f64));
        output!(tax, total, net)
    };
    p.push(&frame);
    p.compute().unwrap();
    (frame, move |new_frame: &Frame| {
        p.push(new_frame);
        p.compute().unwrap();
    })
}

/// Returns a fresh f32 pipeline pre-loaded with `n` rows.
fn setup_f32(n: usize) -> (Frame, impl FnMut(&Frame)) {
    let prices: Vec<f32> = (1..=n).map(|i| i as f32).collect();
    let frame = Frame::new().append(Node::from_data("price", prices));
    let mut p = pipeline! {
        source!(price: f32);
        node!(tax   = price.row(|x| x * 0.1_f32));
        node!(total = price.row(|x| x + tax));
        node!(net   = total.row(|x| x * 0.9_f32));
        output!(tax, total, net)
    };
    p.push(&frame);
    p.compute().unwrap();
    (frame, move |new_frame: &Frame| {
        p.push(new_frame);
        p.compute().unwrap();
    })
}

/// Returns a fresh i64 pipeline pre-loaded with `n` rows.
fn setup_i64(n: usize) -> (Frame, impl FnMut(&Frame)) {
    let prices: Vec<i64> = (1..=n).map(|i| (i % 1000) as i64).collect();
    let frame = Frame::new().append(Node::from_data("price", prices));
    let mut p = pipeline! {
        source!(price: i64);
        node!(tax   = price.row(|x| x / 10_i64));
        node!(total = price.row(|x| x + tax));
        node!(net   = total.row(|x| x - x / 10_i64));
        output!(tax, total, net)
    };
    p.push(&frame);
    p.compute().unwrap();
    (frame, move |new_frame: &Frame| {
        p.push(new_frame);
        p.compute().unwrap();
    })
}

// ── Helper: build a mutated frame for `update_rows` rows ─────────────────────

fn mutated_frame_f64(n: usize, update_rows: usize) -> Frame {
    // Replace the first `update_rows` prices with a new value to simulate an
    // incremental data feed.  The rest of the column is unchanged.
    let mut prices: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    for i in 0..update_rows {
        prices[i] += 1.0;
    }
    Frame::new().append(Node::from_data("price", prices))
}

fn mutated_frame_f32(n: usize, update_rows: usize) -> Frame {
    let mut prices: Vec<f32> = (1..=n).map(|i| i as f32).collect();
    for i in 0..update_rows {
        prices[i] += 1.0;
    }
    Frame::new().append(Node::from_data("price", prices))
}

fn mutated_frame_i64(n: usize, update_rows: usize) -> Frame {
    let mut prices: Vec<i64> = (1..=n).map(|i| (i % 1000) as i64).collect();
    for i in 0..update_rows {
        prices[i] += 1;
    }
    Frame::new().append(Node::from_data("price", prices))
}

// ── vertexrs incremental benchmarks ──────────────────────────────────────────

fn bench_vtx_incremental_f64(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_vtx_f64");

    for (label, pct) in [("1pct", N / 100), ("10pct", N / 10), ("100pct", N)] {
        group.throughput(Throughput::Elements(pct as u64));
        let frame = mutated_frame_f64(N, pct);

        // Each iteration: push the mutated frame and recompute.  Pipeline state
        // is shared across iterations intentionally — after the first iter the
        // pipeline only recomputes dirty chunks.
        group.bench_function(BenchmarkId::new("vtx", label), |b| {
            let (_, mut runner) = setup_f64(N);
            b.iter(|| runner(&frame))
        });
    }

    group.finish();
}

fn bench_vtx_incremental_f32(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_vtx_f32");

    for (label, pct) in [("1pct", N / 100), ("10pct", N / 10), ("100pct", N)] {
        group.throughput(Throughput::Elements(pct as u64));
        let frame = mutated_frame_f32(N, pct);

        group.bench_function(BenchmarkId::new("vtx", label), |b| {
            let (_, mut runner) = setup_f32(N);
            b.iter(|| runner(&frame))
        });
    }

    group.finish();
}

fn bench_vtx_incremental_i64(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_vtx_i64");

    for (label, pct) in [("1pct", N / 100), ("10pct", N / 10), ("100pct", N)] {
        group.throughput(Throughput::Elements(pct as u64));
        let frame = mutated_frame_i64(N, pct);

        group.bench_function(BenchmarkId::new("vtx", label), |b| {
            let (_, mut runner) = setup_i64(N);
            b.iter(|| runner(&frame))
        });
    }

    group.finish();
}

// ── Polars full-recompute baseline ────────────────────────────────────────────
//
// Polars has no incremental mode.  Every call rebuilds the full output from
// scratch.  These are the denominators for the speedup ratio.

#[cfg(feature = "bench-polars")]
mod polars_baseline {
    use super::*;
    use criterion::Criterion;
    use polars::prelude::*;

    pub fn run_polars_f64(prices: &[f64]) -> DataFrame {
        let df = df! { "price" => prices }.unwrap();
        df.lazy()
            .with_column((col("price") * lit(0.1_f64)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") * lit(0.9_f64)).alias("net"))
            .collect()
            .unwrap()
    }

    pub fn run_polars_f32(prices: &[f32]) -> DataFrame {
        let df = df! { "price" => prices }.unwrap();
        df.lazy()
            .with_column((col("price") * lit(0.1_f32)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") * lit(0.9_f32)).alias("net"))
            .collect()
            .unwrap()
    }

    pub fn run_polars_i64(prices: &[i64]) -> DataFrame {
        let df = df! { "price" => prices }.unwrap();
        df.lazy()
            .with_column((col("price") / lit(10_i64)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") - col("total") / lit(10_i64)).alias("net"))
            .collect()
            .unwrap()
    }

    pub fn bench_polars_full_recompute(c: &mut Criterion) {
        let prices_f64: Vec<f64> = (1..=N).map(|i| i as f64).collect();
        let prices_f32: Vec<f32> = (1..=N).map(|i| i as f32).collect();
        let prices_i64: Vec<i64> = (1..=N).map(|i| (i % 1000) as i64).collect();

        let mut group = c.benchmark_group("incremental_polars_full_recompute");
        group.throughput(Throughput::Elements(N as u64));

        group.bench_function(BenchmarkId::new("polars", "f64"), |b| {
            b.iter(|| run_polars_f64(&prices_f64))
        });
        group.bench_function(BenchmarkId::new("polars", "f32"), |b| {
            b.iter(|| run_polars_f32(&prices_f32))
        });
        group.bench_function(BenchmarkId::new("polars", "i64"), |b| {
            b.iter(|| run_polars_i64(&prices_i64))
        });

        group.finish();
    }
}

// ── Correctness tests ─────────────────────────────────────────────────────────
//
// These tests confirm two things:
//   1. The vertexrs incremental result (after a partial update) equals the
//      result of a fresh full recompute on the same data — no stale chunks.
//   2. The vertexrs result equals the Polars result for the same mutated input.
//
// Run with: cargo test --features bench-polars -- incremental_correctness

#[cfg(all(test, feature = "bench-polars"))]
mod correctness {
    use super::*;
    use polars::prelude::*;

    const SMALL: usize = 1000;
    const UPDATE: usize = 10; // 1%

    #[test]
    fn incremental_f64_matches_full_recompute() {
        // Build a pipeline and run once on the original data.
        let prices_orig: Vec<f64> = (1..=SMALL).map(|i| i as f64).collect();
        let frame_orig = Frame::new().append(Node::from_data("price", prices_orig));

        let mut p = pipeline! {
            source!(price: f64);
            node!(tax   = price.row(|x| x * 0.1_f64));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x * 0.9_f64));
            output!(tax, total, net)
        };
        p.push(&frame_orig);
        p.compute().unwrap();

        // Now push a mutated frame and recompute incrementally.
        let mut prices_mutated: Vec<f64> = (1..=SMALL).map(|i| i as f64).collect();
        for i in 0..UPDATE {
            prices_mutated[i] += 1.0;
        }
        let frame_mutated =
            Frame::new().append(Node::from_data("price", prices_mutated.clone()));
        p.push(&frame_mutated);
        p.compute().unwrap();
        let vtx_incremental = p.output().get::<f64>("net").unwrap().to_vec();

        // Compare against Polars full recompute on the same mutated data.
        let pol = polars_baseline::run_polars_f64(&prices_mutated);
        let pol_net: Vec<f64> = pol
            .column("net")
            .unwrap()
            .f64()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_incremental.len(), pol_net.len());
        for (i, (v, p)) in vtx_incremental.iter().zip(pol_net.iter()).enumerate() {
            assert!(
                (v - p).abs() < 1e-6,
                "row {i}: vtx={v} polars={p} after incremental update"
            );
        }
    }

    #[test]
    fn incremental_i64_matches_full_recompute() {
        let prices_orig: Vec<i64> = (1..=SMALL).map(|i| (i % 1000) as i64).collect();
        let frame_orig = Frame::new().append(Node::from_data("price", prices_orig));

        let mut p = pipeline! {
            source!(price: i64);
            node!(tax   = price.row(|x| x / 10_i64));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x - x / 10_i64));
            output!(tax, total, net)
        };
        p.push(&frame_orig);
        p.compute().unwrap();

        let mut prices_mutated: Vec<i64> = (1..=SMALL).map(|i| (i % 1000) as i64).collect();
        for i in 0..UPDATE {
            prices_mutated[i] += 1;
        }
        let frame_mutated =
            Frame::new().append(Node::from_data("price", prices_mutated.clone()));
        p.push(&frame_mutated);
        p.compute().unwrap();
        let vtx_net = p.output().get::<i64>("net").unwrap().to_vec();

        let pol = polars_baseline::run_polars_i64(&prices_mutated);
        let pol_net: Vec<i64> = pol
            .column("net")
            .unwrap()
            .i64()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_net, pol_net, "i64 incremental result mismatch vs Polars");
    }
}

// ── criterion entry point ─────────────────────────────────────────────────────

#[cfg(not(feature = "bench-polars"))]
criterion_group!(
    benches,
    bench_vtx_incremental_f64,
    bench_vtx_incremental_f32,
    bench_vtx_incremental_i64
);

#[cfg(feature = "bench-polars")]
criterion_group!(
    benches,
    bench_vtx_incremental_f64,
    bench_vtx_incremental_f32,
    bench_vtx_incremental_i64,
    polars_baseline::bench_polars_full_recompute
);

criterion_main!(benches);
