# Benchmark Baseline — Initial Numbers

This file documents the throughput numbers from the first `bench.yml` post-merge
run after the kernel fusion pass (Issue #31, Phase 2.7) landed on `main`.

The binary baseline is stored in the GitHub Actions cache under the key
`bench-baseline-main-*`. These numbers provide human-readable traceability
since the Actions cache may be evicted.

## How to read the numbers

- **ns/iter**: median time per pipeline invocation
- **MB/s** (Throughput): rows processed per second × element size

Regression threshold: **> 15%** throughput drop vs this baseline triggers a CI
failure in `bench.yml`.

---

## Results (to be filled after first post-merge bench.yml run)

The `bench.yml` workflow runs automatically on every push to `main`. On the
first post-merge run following this PR, the cache key `bench-baseline-main-*`
will be empty; the comparison step will be skipped and the save step will write
the initial baseline. The maintainer should update this file with the numbers
from that run for long-term traceability.

### Expected groups

- `pipeline_3node_vtx` — parametrised over `f64`, `f32`, `i64`, `i32`, `u64`, `u32`
- `fusion_vs_unfused` — `fused_5node_f64` vs `unfused_5node_f64` at N=256
- `pipeline_column` — column storage throughput
- `pipeline_incremental` — 1%, 10%, 100% incremental update

### Target (pre-SIMD, from docs/plans/phase-02-macro-system.md)

- Full recompute within 5× of Polars on a 3-node pipeline at 1M rows
- `fused_5node_f64` at least 5% faster than `unfused_5node_f64` at N=256
