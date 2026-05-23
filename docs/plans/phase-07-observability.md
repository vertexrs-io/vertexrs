[← Phase Index](main.md)

## Phase 7 — Observability and Tooling

**Goal:** Make the engine debuggable, profileable, and production-ready.

### 7.1 Query Explain

- [ ] `graph.explain(node)` — print logical plan, physical plan, fusion groups
- [ ] Show access pattern per node, execution strategy, estimated cost
- [ ] Highlight partition pruning decisions
- [ ] Warn on expensive patterns: impure nodes, key-column group-by updates, full-dirty blocking nodes

### 7.2 Profiling

- [ ] Per-node timing — time spent in kernel execution per compute cycle
- [ ] Dirty chunk statistics — what fraction of chunks were recomputed vs cached
- [ ] SIMD utilisation report — autovectorised vs scalar fallback per kernel
- [ ] Warning report — NA counts, suppressed warnings, purity violations

### 7.3 Tracing

- [ ] `graph.trace(row, node)` — show input values and output for a single row
- [ ] Replay a delta sequence for debugging incremental update bugs
- [ ] Time-travel query — compute the DAG as of a previous epoch

### 7.4 Python Bindings

- [ ] PyO3 bindings for `node!` macro equivalent in Python
- [ ] Arrow-native data exchange — zero copy via PyArrow
- [ ] Expose `graph.push`, `graph.compute`, sink callbacks to Python

