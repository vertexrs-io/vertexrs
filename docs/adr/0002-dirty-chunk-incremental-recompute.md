# ADR-0002: Dirty-Chunk Incremental Recomputation

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-05-23 |
| **Supersedes** | — |

## Context

For live data workloads (market data ticks, IoT sensor streams, partial dataset updates) re-executing the full DAG on every change is prohibitively expensive. A finer invalidation granularity is needed, but per-row tracking would dominate memory and CPU for large columns.

## Decision

Track dirty state at chunk granularity, not row or node granularity. Each `ChunkedColumn<T>` holds a `RoaringBitmap` of dirty chunk indices. When a row range is updated, only the affected chunk indices (computed via `row / CHUNK_SIZE`) are set. The executor traverses only dirty chunks during recomputation and clears bits as chunks complete. Dirty ranges propagate through the DAG via `Graph::propagate_dirty`, which merges ranges on diamond joins and expands to `0..usize::MAX` at blocking nodes (Scatter, Reshape).

## Alternatives considered

- **Dirty-node tracking** — simpler but forces full-column recompute for any change to a node; loses the sub-column locality that makes incremental recompute worthwhile.
- **Per-row bitmaps** — maximum precision but a 256× overhead vs chunk-level bitmaps for 256-element chunks; bitmap storage alone would rival the data.
- **Version vectors / timestamps** — used by systems like Materialize; lower overhead for sparse changes but requires MVCC semantics that conflict with Arrow's in-place buffer model.

## Consequences

**Positive:** sub-millisecond incremental update latency on partial-change graphs; memory overhead is O(chunks), not O(rows); naturally composable with SIMD chunk processing.

**Negative / trade-offs:** chunk size (256 elements) is a tuning parameter that affects granularity vs overhead trade-off; blocking nodes (Scatter, Reshape) always recompute the full column — these must be used sparingly.
