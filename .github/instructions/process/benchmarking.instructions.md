---
description: "Benchmarking standards and regression policy. Apply when writing or reviewing benchmarks."
---

# Benchmarking Standards

## Framework

All benchmarks use `criterion`. Benchmark files live in `vertexrs/benches/`.

## Required benchmarks

Benchmark before merging any code on the hot recompute path:

- New kernel types
- Changes to `Executor` traversal logic
- Changes to dirty-bitmap propagation
- Changes to `AlignedChunk` or `ChunkedColumn`

## Cross-dtype coverage

Parametrize benchmarks over: `f32`, `f64`, `i32`, `i64`, `u32`, `u64`, `f16`.
No single dtype should be more than 2× slower than the fastest for the same logical pipeline.

## Polars counterparts

Every benchmark file with a Polars equivalent must:

1. Include a correctness `#[test]` asserting outputs match within tolerance
2. Name the Polars benchmark group `polars_*` so comparisons are obvious in reports

## Regression policy

- Save a baseline on every merge to `main`: `cargo bench --save-baseline main`
- Regressions > 15% on throughput benchmarks must be explained and justified in the PR
- A faster-but-wrong result is never acceptable — correctness assertions are non-negotiable

## Baseline comparison

```bash
cargo bench --baseline main   # compare current vs saved main baseline
```
