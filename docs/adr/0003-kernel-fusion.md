# ADR-0003: Compile-Time Kernel Fusion

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-05-23 |
| **Supersedes** | — |

## Context

Pointwise chains (e.g. `c = a * 2`, `d = c + 1`, `e = d * d`) are extremely common in data pipelines. Executing each as a separate kernel requires three passes over the data, three sets of temporary allocations, and three kernel dispatch overheads. For columnar workloads at the scale of millions of rows, this dominates over the actual compute cost.

## Decision

Fuse pointwise kernel chains into a single-pass kernel at compile time via the `pipeline!` macro. The macro detects chains of `ElementIndependent` kernels with no branching consumers and emits a single closure that computes the full chain in one pass. The `ChunkContract::ElementIndependent` variant on `Kernel<T>` is the marker that makes a kernel eligible for fusion.

## Alternatives considered

- **Runtime fusion** — can fuse based on actual DAG topology at startup, but requires an interpreter loop and cannot leverage Rust's zero-cost abstraction for the fused closure; harder to optimise for SIMD.
- **No fusion** — simplest implementation; acceptable for small pipelines or proof-of-concept but leaves significant throughput on the table for production workloads.
- **LLVM-level fusion via LTO** — possible in theory but unreliable across crate boundaries and opaque to the developer.

## Consequences

**Positive:** single memory pass for pointwise chains; no intermediate allocations; Rust's optimiser can autovectorise the fused closure; throughput approaches raw SIMD loop performance.

**Negative / trade-offs:** fusion is limited to chains of `ElementIndependent` kernels; `BoundaryDependent` and blocking nodes break the chain; macro complexity increases; harder to debug intermediate values in a fused chain.
