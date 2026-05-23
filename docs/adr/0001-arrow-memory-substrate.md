# ADR-0001: Apache Arrow as the Memory Substrate

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-05-23 |
| **Supersedes** | — |

## Context

The columnar execution path needs a memory layout that is cache-friendly for SIMD operations, supports null/validity semantics without a separate null-value sentinel, enables zero-copy interop with the broader data ecosystem (Polars, DataFusion, Python via PyArrow), and has a well-audited Rust implementation.

## Decision

Use Apache Arrow as the sole memory substrate for the columnar path. All chunked storage is backed by `ScalarBuffer<T>` from `arrow-buffer`, which guarantees 64-byte alignment per the Arrow spec §2.3. Validity (null) information is carried in Arrow `NullBuffer` bitmaps. Zero-copy interop is achieved via `Arc` buffer clones in `to_arrow_array()`.

## Alternatives considered

- **Custom `Vec<T>` with manual alignment** — no ecosystem interop; would need to re-implement validity bitmaps and alignment guarantees ourselves.
- **ndarray** — good SIMD story but no native null semantics and limited ecosystem interop; not designed for columnar record batches.
- **Raw `*mut u8` allocations** — maximum control but entirely unsafe; reimplements what Arrow already provides with audit and testing.

## Consequences

**Positive:** zero-copy interop with Polars, DataFusion, and PyArrow; validity bitmaps come for free; 64-byte alignment guaranteed; Arrow ecosystem handles serialisation (IPC, Parquet, Flight).

**Negative / trade-offs:** dependency on `arrow-buffer` / `arrow-array` / `arrow-schema`; minor overhead from `Arc` reference counting on buffer clones; Arrow spec changes can cause minor churn.
