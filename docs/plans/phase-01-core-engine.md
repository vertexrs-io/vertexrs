[← Phase Index](main.md)

## Phase 1 — Core Local Engine

**Goal:** A working single-machine DAG with vectorised execution and incremental updates.  
**Success metric:** Comparable throughput (Memory and Time) to Polars on simple transformation pipelines. Memory usage should be efficient and not grow unbounded with updates.

### 1.1 Chunked Column Storage  ✅ COMPLETE

- [x] Define `AlignedChunk<T>` — fixed size (256 elements), 64-byte aligned
  - Backed by `ScalarBuffer<T>` (Arrow allocator, 64-byte aligned per Arrow spec §2.3)
  - `is_full()` / `is_empty()` / `values()` helpers; panics on `src.len() > CHUNK_SIZE`
  - `to_arrow_array()` for `T: ArrowBacked` — zero-copy (Arc buffer clone)
- [x] Define `ChunkedColumn<T>` — `Vec<AlignedChunk<T>>` with length tracking
  - `from_slice(&[T])` splits evenly at `CHUNK_SIZE`; `push_chunk`, `iter_chunks`, `get`
  - `to_arrow_arrays()` for `T: ArrowBacked` — one `PrimitiveArray` per chunk
- [x] Integrate `jemalloc` as global allocator (opt-in `--features jemalloc`)
  - `tikv-jemallocator = "0.6"` in `[workspace.dependencies]`
  - `#[global_allocator]` guard in `lib.rs` behind `#[cfg(feature = "jemalloc")]`
- [x] Implement Arrow interop — zero-copy where layouts match (Arc clone)
- [ ] Write chunk-level benchmarks against raw `Vec<f32>` — deferred (needs `criterion`)

### 1.2 Dirty Bitmap Tracking  ✅ COMPLETE

- [x] Add `RoaringBitmap dirty` field to `ChunkedColumn` (chunk indices, not row indices)
  - `roaring = "0.10"` in `[workspace.dependencies]`; overflow impossible (needs > 16B rows for u32 overflow)
- [x] Implement `mark_dirty(row_range: Range<usize>)` — converts row range to chunk index range via `/ CHUNK_SIZE`, inserts with `insert_range(first..=last)`; no-op on empty range
- [x] Implement `dirty_chunks() -> impl Iterator<Item=(usize, &AlignedChunk<T>)>` — iterates bitmap, skips out-of-bounds indices
- [x] `mark_all_dirty()`, `clear_dirty()`, `is_dirty()` helpers
- [x] Unit tests: append delta (rows 512..768 → chunk 2 only), mutation delta (rows 100..300 → chunks 0 and 1), empty-range no-op, clear removes all flags

### 1.3 DAG Topology  ✅ COMPLETE

- [x] Define `NodeId(u32)` — dense opaque index; `index() -> usize` helper
- [x] Define `IndexMapping` enum: `Pointwise`, `LocalWindow { half }`, `Scatter`, `Reshape`
  - `map_range(Range<usize>) -> Range<usize>`: Pointwise=identity, LocalWindow=±half, Scatter/Reshape=`0..usize::MAX`
  - `is_blocking()` → true for Scatter and Reshape
- [x] Define `NodeDescriptor` — `id`, `name`, `inputs: Vec<NodeId>`, `mapping: IndexMapping`
- [x] Build `Graph` struct — `nodes: Vec<NodeDescriptor>`, `consumers: Vec<Vec<NodeId>>`
  - `add_node(name, inputs, mapping) -> NodeId` — panics on unknown input ids
  - `consumers_of(id)` — reverse edge accessor
- [x] `topological_order() -> Vec<NodeId>` — Kahn's BFS algorithm; panics on cycle
- [x] `propagate_dirty(source, row_range) -> HashMap<NodeId, Range<usize>>` — BFS; uses latest dirty range per node (not stale queue snapshot); merges ranges on diamond joins; blocking nodes receive `0..usize::MAX`
- [x] Tests: mapping variants, topo order (single/chain/diamond), dirty propagation (chain/window/blocking/5-node-mixed/diamond-merge/unrelated-nodes)

### 1.4 Kernel Trait  ✅ COMPLETE

- [x] Define `Kernel<T: ArrowNativeType>` trait — `execute_chunk(&[&[T]], chunk_idx) -> Vec<T>` + `contract() -> ChunkContract`; `Send + Sync`
- [x] Define `ChunkContract` enum: `ElementIndependent`, `FixedSize(usize)`, `BoundaryDependent`
- [x] `BinaryKernel<T, F>` — element-wise binary op via closure; asserts 2 inputs + matching lengths
- [x] `UnaryKernel<T, F>` — element-wise unary op via closure; asserts 1 input
- [x] `CondKernel<T>` — if/else over 3 inputs (mask, then, else); nonzero mask = true
- [x] Factory functions: `add`, `sub`, `mul`, `div`, `rem`, `cond` — all `impl Kernel<T>` with explicit `Add/Sub/Mul/Div/Rem` bounds (arrow-buffer 54 dropped arithmetic supertraits from `ArrowNativeType`)
- [x] `is_null_mask(validity, one, zero)` — free fn producing `Vec<f64>` from a `NullBuffer`
- [x] `propagate_nulls(&[Option<&NullBuffer>]) -> Option<NullBuffer>` — ANDs validity bitmaps; `None` if all valid; correct Arrow null semantics (valid only when valid in all inputs)
- [ ] Benchmarks — deferred (needs `criterion`)

### 1.5 Single-Threaded Executor ✅ COMPLETE

- [x] Implement topological chunk-ordered traversal
- [x] Execute kernels in topo order, clearing dirty bits per chunk
- [x] Clear dirty bits as chunks complete
- [x] Wire up `WarningCollector` — capped (1000), drained per cycle
- [x] Integration test: 3-node DAG (A→B=A×2→C=A+B) on 512 synthetic f64 values

**Implementation notes:**
- `Executor<T: ArrowNativeType + PartialEq>` in `executor.rs`
- Kernels stored as `Arc<dyn Kernel<T>>` to avoid borrow conflicts with `&mut self`
- `RefCell<ChunkedColumn<T>>` per node for interior-mutability borrow split
- `mark_source_dirty(source, row_range)` propagates via `graph.propagate_dirty`
- Input values cloned to `Vec<T>` before kernel call; output written back via `replace_chunk`
- `column.rs` additions: `replace_chunk(idx, chunk)`, `clear_dirty_chunk(idx)`
- `dag.rs` additions: `Graph::len()`, `Graph::is_empty()`

### 1.6 NA and Error Handling ✅ COMPLETE

- [x] Define `FailureMode`: `Soft` (set NA + warn) and `Hard` (halt + return error)
- [x] Implement debug-mode purity checker — run UDF twice, assert identical output
- [x] Implement NA threshold config — warn if NA fraction exceeds threshold in any node
- [x] Write tests: soft failure propagates NA, hard failure returns error immediately

**Implementation notes:**
- `FailureMode` (`Soft` default, `Hard`) on `Executor` struct
- `ExecutionError { node, chunk_idx, message }` returned on hard failure
- `NaConfig { warn_threshold: f64 }` (default 0.5)
- Kernel panics caught via `std::panic::catch_unwind` + `AssertUnwindSafe`
- Soft failure: fills output with `T::default()`, sets chunk null bitmap to all-null
- `null_chunks: Vec<Vec<Option<NullBuffer>>>` — per-node, per-chunk null tracking
- `propagate_nulls` used for valid-data null inheritance from inputs
- Purity check: kernel run twice; output `Vec<T>` compared element-wise (requires `T: PartialEq`)
- 9 executor tests: integration, idempotent run, soft/hard failure, NA threshold, purity, null propagation
- **Total tests after 1.5+1.6: 78 unit + 4 doctests**

