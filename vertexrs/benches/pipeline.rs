//! Full-recompute pipeline throughput benchmarks, vertexrs vs Polars.
//!
//! Pipeline: 3-node arithmetic chain on a single source column.
//!   tax      = price * 0.1
//!   total    = price + tax
//!   net      = total * 0.9
//!
//! Benchmarks are parametrized over six dtypes: f64, f32, i64, i32, u64, u32.
//! Polars does not support f16 natively, so f16 is measured as a vertexrs-only
//! group and compared against the f32 group for throughput parity.
//!
//! Run with:
//!   cargo bench --bench pipeline                        # vertexrs only
//!   cargo bench --bench pipeline --features bench-polars  # + Polars comparison
//!
//! Correctness tests (run as part of `cargo test --features bench-polars`):
//!   cargo test --features bench-polars -- pipeline_correctness

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use vertexrs::{Frame, Node, pipeline};

const N: usize = 1_000_000;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Builds a 1M-element source frame for f64.
fn make_frame_f64(n: usize) -> Frame {
    let prices: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    Frame::new().append(Node::from_data("price", prices))
}

/// Builds a 1M-element source frame for f32.
fn make_frame_f32(n: usize) -> Frame {
    let prices: Vec<f32> = (1..=n).map(|i| i as f32).collect();
    Frame::new().append(Node::from_data("price", prices))
}

/// Builds a 1M-element source frame for i64.
fn make_frame_i64(n: usize) -> Frame {
    // Use small values to avoid overflow in the pipeline arithmetic.
    let prices: Vec<i64> = (1..=n).map(|i| (i % 1000) as i64).collect();
    Frame::new().append(Node::from_data("price", prices))
}

/// Builds a 1M-element source frame for i32.
fn make_frame_i32(n: usize) -> Frame {
    let prices: Vec<i32> = (1..=n).map(|i| (i % 1000) as i32).collect();
    Frame::new().append(Node::from_data("price", prices))
}

/// Builds a 1M-element source frame for u64.
fn make_frame_u64(n: usize) -> Frame {
    let prices: Vec<u64> = (1..=n).map(|i| (i % 1000) as u64).collect();
    Frame::new().append(Node::from_data("price", prices))
}

/// Builds a 1M-element source frame for u32.
fn make_frame_u32(n: usize) -> Frame {
    let prices: Vec<u32> = (1..=n).map(|i| (i % 1000) as u32).collect();
    Frame::new().append(Node::from_data("price", prices))
}

// ── vertexrs pipeline macros ──────────────────────────────────────────────────
//
// Each dtype needs its own pipeline! invocation because pipeline! expands to a
// concrete struct; we can't parameterise it over T at runtime.

macro_rules! run_vtx_pipeline {
    ($frame:expr, f64) => {{
        let mut p = pipeline! {
            source!(price: f64);
            node!(tax   = price.row(|x| x * 0.1_f64));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x * 0.9_f64));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
    ($frame:expr, f32) => {{
        let mut p = pipeline! {
            source!(price: f32);
            node!(tax   = price.row(|x| x * 0.1_f32));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x * 0.9_f32));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
    ($frame:expr, i64) => {{
        let mut p = pipeline! {
            source!(price: i64);
            node!(tax   = price.row(|x| x / 10_i64));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x - x / 10_i64));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
    ($frame:expr, i32) => {{
        let mut p = pipeline! {
            source!(price: i32);
            node!(tax   = price.row(|x| x / 10_i32));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x - x / 10_i32));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
    ($frame:expr, u64) => {{
        let mut p = pipeline! {
            source!(price: u64);
            node!(tax   = price.row(|x| x / 10_u64));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x - x / 10_u64));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
    ($frame:expr, u32) => {{
        let mut p = pipeline! {
            source!(price: u32);
            node!(tax   = price.row(|x| x / 10_u32));
            node!(total = price.row(|x| x + tax));
            node!(net   = total.row(|x| x - x / 10_u32));
            output!(tax, total, net)
        };
        p.push($frame);
        p.compute().unwrap();
        p
    }};
}

// ── vertexrs benchmarks ───────────────────────────────────────────────────────

fn bench_vtx_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_3node_vtx");
    group.throughput(Throughput::Elements(N as u64));

    macro_rules! bench_dtype {
        ($label:literal, $dtype:ident, $make:ident) => {
            group.bench_function(BenchmarkId::new($label, N), |b| {
                let frame = $make(N);
                b.iter(|| run_vtx_pipeline!(&frame, $dtype))
            });
        };
    }

    bench_dtype!("f64", f64, make_frame_f64);
    bench_dtype!("f32", f32, make_frame_f32);
    bench_dtype!("i64", i64, make_frame_i64);
    bench_dtype!("i32", i32, make_frame_i32);
    bench_dtype!("u64", u64, make_frame_u64);
    bench_dtype!("u32", u32, make_frame_u32);

    group.finish();
}

// ── Polars benchmarks ─────────────────────────────────────────────────────────

#[cfg(feature = "bench-polars")]
mod polars_benches {
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

    pub fn run_polars_i32(prices: &[i32]) -> DataFrame {
        let df = df! { "price" => prices }.unwrap();
        df.lazy()
            .with_column((col("price") / lit(10_i32)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") - col("total") / lit(10_i32)).alias("net"))
            .collect()
            .unwrap()
    }

    pub fn bench_polars_pipeline(c: &mut Criterion) {
        let prices_f64: Vec<f64> = (1..=N).map(|i| i as f64).collect();
        let prices_f32: Vec<f32> = (1..=N).map(|i| i as f32).collect();
        let prices_i64: Vec<i64> = (1..=N).map(|i| (i % 1000) as i64).collect();
        let prices_i32: Vec<i32> = (1..=N).map(|i| (i % 1000) as i32).collect();

        let mut group = c.benchmark_group("pipeline_3node_polars");
        group.throughput(Throughput::Elements(N as u64));

        group.bench_function(BenchmarkId::new("f64", N), |b| {
            b.iter(|| run_polars_f64(&prices_f64))
        });
        group.bench_function(BenchmarkId::new("f32", N), |b| {
            b.iter(|| run_polars_f32(&prices_f32))
        });
        group.bench_function(BenchmarkId::new("i64", N), |b| {
            b.iter(|| run_polars_i64(&prices_i64))
        });
        group.bench_function(BenchmarkId::new("i32", N), |b| {
            b.iter(|| run_polars_i32(&prices_i32))
        });

        group.finish();
    }
}

// ── Correctness tests ─────────────────────────────────────────────────────────
//
// Run with: cargo test --features bench-polars -- pipeline_correctness
//
// Each test runs both vertexrs and Polars on a small fixed dataset and asserts
// that outputs agree within the tolerance defined in CLAUDE.md:
//   - f32/f64: abs(vtx - polars) < 1e-6
//   - integer types: exact equality

#[cfg(all(test, feature = "bench-polars"))]
mod correctness {
    use super::*;
    use polars::prelude::*;

    const SMALL: usize = 100;

    #[test]
    fn pipeline_correctness_f64() {
        let prices: Vec<f64> = (1..=SMALL).map(|i| i as f64).collect();
        let frame = make_frame_f64(SMALL);
        let vtx = run_vtx_pipeline!(&frame, f64);
        let pol = polars_benches::run_polars_f64(&prices);

        let vtx_net = vtx.output().get::<f64>("net").unwrap().to_vec();
        let pol_net: Vec<f64> = pol
            .column("net")
            .unwrap()
            .f64()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_net.len(), pol_net.len());
        for (v, p) in vtx_net.iter().zip(pol_net.iter()) {
            assert!(
                (v - p).abs() < 1e-6,
                "f64 mismatch: vtx={v} polars={p}"
            );
        }
    }

    #[test]
    fn pipeline_correctness_f32() {
        let prices: Vec<f32> = (1..=SMALL).map(|i| i as f32).collect();
        let frame = make_frame_f32(SMALL);
        let vtx = run_vtx_pipeline!(&frame, f32);
        let pol = polars_benches::run_polars_f32(&prices);

        let vtx_net = vtx.output().get::<f32>("net").unwrap().to_vec();
        let pol_net: Vec<f32> = pol
            .column("net")
            .unwrap()
            .f32()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_net.len(), pol_net.len());
        for (v, p) in vtx_net.iter().zip(pol_net.iter()) {
            assert!(
                (v - p).abs() < 1e-6,
                "f32 mismatch: vtx={v} polars={p}"
            );
        }
    }

    #[test]
    fn pipeline_correctness_i64() {
        let prices: Vec<i64> = (1..=SMALL).map(|i| (i % 1000) as i64).collect();
        let frame = make_frame_i64(SMALL);
        let vtx = run_vtx_pipeline!(&frame, i64);
        let pol = polars_benches::run_polars_i64(&prices);

        let vtx_net = vtx.output().get::<i64>("net").unwrap().to_vec();
        let pol_net: Vec<i64> = pol
            .column("net")
            .unwrap()
            .i64()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_net, pol_net, "i64 pipeline output mismatch");
    }

    #[test]
    fn pipeline_correctness_i32() {
        let prices: Vec<i32> = (1..=SMALL).map(|i| (i % 1000) as i32).collect();
        let frame = make_frame_i32(SMALL);
        let vtx = run_vtx_pipeline!(&frame, i32);
        let pol = polars_benches::run_polars_i32(&prices);

        let vtx_net = vtx.output().get::<i32>("net").unwrap().to_vec();
        let pol_net: Vec<i32> = pol
            .column("net")
            .unwrap()
            .i32()
            .unwrap()
            .into_no_null_iter()
            .collect();

        assert_eq!(vtx_net, pol_net, "i32 pipeline output mismatch");
    }
}

// ── criterion entry point ─────────────────────────────────────────────────────

#[cfg(not(feature = "bench-polars"))]
criterion_group!(benches, bench_vtx_pipeline);

#[cfg(feature = "bench-polars")]
criterion_group!(benches, bench_vtx_pipeline, polars_benches::bench_polars_pipeline);

criterion_main!(benches);
