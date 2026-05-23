//! Performance-ratio tests: vertexrs pipeline vs Polars full-recompute.
//!
//! These tests assert that vertexrs does not regress badly *relative* to Polars.
//! Both sides are timed in the same process on the same machine, so the ratio is
//! machine-independent and will not flake due to CI hardware differences.
//!
//! Threshold tiers:
//!   - debug builds (`cargo test`):            ≤ 50×  catches catastrophic regressions
//!     (O(n²) loops, accidental full-clone-per-element, etc.)
//!   - release builds (`cargo test --release`): ≤ 8×  enforces the pre-SIMD budget
//!     from CLAUDE.md (plan target: ≤ 5×) with 60% headroom for measurement noise
//!
//! When vertexrs genuinely improves past a milestone (e.g. SIMD lands and the
//! ratio drops to ~1.5×), tighten the release cap to match the updated plan target.
//!
//! Run (debug, loose gate):
//!   cargo test --features bench-polars -- pipeline_ratio
//!
//! Run (release, enforcing gate):
//!   cargo test --release --features bench-polars -- pipeline_ratio

#[cfg(feature = "bench-polars")]
mod pipeline_ratio {
    use std::time::Instant;
    use vertexrs::{Frame, Node, pipeline};

    // ── Threshold ─────────────────────────────────────────────────────────────

    /// How many times slower than Polars vertexrs is allowed to be.
    #[cfg(debug_assertions)]
    const RATIO_CAP: f64 = 50.0;
    #[cfg(not(debug_assertions))]
    const RATIO_CAP: f64 = 8.0;

    /// N for the ratio tests — smaller than criterion's 1M to keep the suite fast.
    const N: usize = 100_000;
    /// Number of timing iterations.  Using minimum discards OS-scheduling noise.
    const ITERS: u32 = 20;

    // ── Timer ─────────────────────────────────────────────────────────────────

    /// Runs `f` once as a warm-up, then `iters` times, returning the minimum
    /// elapsed time in nanoseconds.
    fn time_min_ns(mut f: impl FnMut(), iters: u32) -> u128 {
        f(); // warm-up
        (0..iters)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_nanos()
            })
            .min()
            .unwrap()
    }

    // ── Frame builders ────────────────────────────────────────────────────────

    fn make_frame_f64(n: usize) -> Frame {
        Frame::new().append(Node::from_data(
            "price",
            (1..=n).map(|i| i as f64).collect::<Vec<_>>(),
        ))
    }
    fn make_frame_f32(n: usize) -> Frame {
        Frame::new().append(Node::from_data(
            "price",
            (1..=n).map(|i| i as f32).collect::<Vec<_>>(),
        ))
    }
    fn make_frame_i64(n: usize) -> Frame {
        Frame::new().append(Node::from_data(
            "price",
            (1..=n).map(|i| (i % 1000) as i64).collect::<Vec<_>>(),
        ))
    }
    fn make_frame_i32(n: usize) -> Frame {
        Frame::new().append(Node::from_data(
            "price",
            (1..=n).map(|i| (i % 1000) as i32).collect::<Vec<_>>(),
        ))
    }

    // ── vertexrs pipeline runners ─────────────────────────────────────────────

    macro_rules! run_vtx {
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
        }};
    }

    // ── Polars runners ────────────────────────────────────────────────────────

    use polars::prelude::*;

    fn polars_f64(prices: &[f64]) {
        df! { "price" => prices }
            .unwrap()
            .lazy()
            .with_column((col("price") * lit(0.1_f64)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") * lit(0.9_f64)).alias("net"))
            .collect()
            .unwrap();
    }
    fn polars_f32(prices: &[f32]) {
        df! { "price" => prices }
            .unwrap()
            .lazy()
            .with_column((col("price") * lit(0.1_f32)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") * lit(0.9_f32)).alias("net"))
            .collect()
            .unwrap();
    }
    fn polars_i64(prices: &[i64]) {
        df! { "price" => prices }
            .unwrap()
            .lazy()
            .with_column((col("price") / lit(10_i64)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") - col("total") / lit(10_i64)).alias("net"))
            .collect()
            .unwrap();
    }
    fn polars_i32(prices: &[i32]) {
        df! { "price" => prices }
            .unwrap()
            .lazy()
            .with_column((col("price") / lit(10_i32)).alias("tax"))
            .with_column((col("price") + col("tax")).alias("total"))
            .with_column((col("total") - col("total") / lit(10_i32)).alias("net"))
            .collect()
            .unwrap();
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn pipeline_ratio_f64() {
        let frame = make_frame_f64(N);
        let prices: Vec<f64> = (1..=N).map(|i| i as f64).collect();

        let vtx_ns = time_min_ns(|| run_vtx!(&frame, f64), ITERS);
        let pol_ns = time_min_ns(|| polars_f64(&prices), ITERS);

        let ratio = vtx_ns as f64 / pol_ns as f64;
        assert!(
            ratio <= RATIO_CAP,
            "f64 pipeline: vertexrs is {ratio:.2}× Polars — exceeds cap {RATIO_CAP}× \
             (vtx={vtx_ns}ns polars={pol_ns}ns per {N} rows)"
        );
    }

    #[test]
    fn pipeline_ratio_f32() {
        let frame = make_frame_f32(N);
        let prices: Vec<f32> = (1..=N).map(|i| i as f32).collect();

        let vtx_ns = time_min_ns(|| run_vtx!(&frame, f32), ITERS);
        let pol_ns = time_min_ns(|| polars_f32(&prices), ITERS);

        let ratio = vtx_ns as f64 / pol_ns as f64;
        assert!(
            ratio <= RATIO_CAP,
            "f32 pipeline: vertexrs is {ratio:.2}× Polars — exceeds cap {RATIO_CAP}× \
             (vtx={vtx_ns}ns polars={pol_ns}ns per {N} rows)"
        );
    }

    #[test]
    fn pipeline_ratio_i64() {
        let frame = make_frame_i64(N);
        let prices: Vec<i64> = (1..=N).map(|i| (i % 1000) as i64).collect();

        let vtx_ns = time_min_ns(|| run_vtx!(&frame, i64), ITERS);
        let pol_ns = time_min_ns(|| polars_i64(&prices), ITERS);

        let ratio = vtx_ns as f64 / pol_ns as f64;
        assert!(
            ratio <= RATIO_CAP,
            "i64 pipeline: vertexrs is {ratio:.2}× Polars — exceeds cap {RATIO_CAP}× \
             (vtx={vtx_ns}ns polars={pol_ns}ns per {N} rows)"
        );
    }

    #[test]
    fn pipeline_ratio_i32() {
        let frame = make_frame_i32(N);
        let prices: Vec<i32> = (1..=N).map(|i| (i % 1000) as i32).collect();

        let vtx_ns = time_min_ns(|| run_vtx!(&frame, i32), ITERS);
        let pol_ns = time_min_ns(|| polars_i32(&prices), ITERS);

        let ratio = vtx_ns as f64 / pol_ns as f64;
        assert!(
            ratio <= RATIO_CAP,
            "i32 pipeline: vertexrs is {ratio:.2}× Polars — exceeds cap {RATIO_CAP}× \
             (vtx={vtx_ns}ns polars={pol_ns}ns per {N} rows)"
        );
    }
}
