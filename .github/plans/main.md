# VertexRS — Build Plan

> A fast, incremental DAG computation engine in Rust.  
> Arrow-backed columnar core. General-purpose task graph surface. Macro-defined. Local to distributed.

---

## Vision

VertexRS is a general-purpose computation graph engine where nodes are defined via macros, execution propagates incrementally through dirty-chunk tracking, and the execution strategy is chosen automatically by type. Scalar primitive nodes execute on the vectorised/SIMD columnar path; heavy or struct-typed nodes execute on the task/rayon path. The same `pipeline!` macro syntax therefore covers both high-throughput data pipelines and arbitrary process/task graphs — users never need to choose between the two modes.

It targets Polars-comparable throughput for columnar workloads, with sub-millisecond incremental update latency on partial-change graphs, and a path to distributed execution and live streaming data.

---

## Core Design Principles

- **Nodes reference each other directly** — the macro builds the DAG, no manual edge declaration
- **Types drive execution strategy** — scalar = columnar/SIMD path; struct/heavy = task/rayon path
- **One macro, two execution modes** — data pipelines and process/task graphs share the same `pipeline!` / `node!` syntax; the engine selects the path at compile time from the node's type
- **Arrow as the memory substrate** — interop, validity bitmaps, aligned buffers (columnar path)
- **Dirty chunks not dirty nodes** — incremental recomputation at chunk granularity
- **Kernel fusion** — pointwise chains fuse into single-pass kernels at compile time
- **Soft/hard failure via NA** — Arrow validity bitmaps propagate nulls, warnings collected per cycle

---

## Security Policy

Security is a first-class concern at every phase, not a late-stage retrofit. The principles below apply unconditionally from Phase 1 onwards; additional phase-specific requirements are noted in the relevant phase sections.

### Memory Safety

- Rust's type system and borrow checker eliminate the entire class of memory-safety bugs (use-after-free, buffer overflows, data races) by default. This is a deliberate language choice and must be preserved.
- `unsafe` blocks are permitted only in performance-critical hot paths where safe alternatives have measurably worse throughput. Every `unsafe` block **must** be preceded by a `// SAFETY:` comment; no exceptions.
- `unsafe` must never cross a public API boundary. Wrap all unsafe in safe public functions.
- Prefer `bytemuck` for transmute-like operations over raw pointer casts.
- All `unsafe` code must be covered by tests that would catch unsound behaviour (e.g. length mismatches, alignment violations).

### Supply Chain Security

- Run `cargo audit` on every CI build; fail the build on any `RUSTSEC` advisory at severity ≥ Medium.
- Pin indirect dependencies with `Cargo.lock` committed to the repository.
- Minimise the dependency tree — every new crate addition must be justified in `Cargo.toml`. Prefer `std` and the Arrow ecosystem over general-purpose alternatives.
- Dev-dependencies (criterion, polars, dhat) are acceptable without the same justification but are still audited.
- Review dependency changelogs before `cargo update` on security-sensitive crates (`ring`, `rustls`, `axum`, `tokio`).

### Input Validation

- Validate all inputs at system boundaries (public API entry points, deserialized data, network messages). Do not validate internal invariants that Rust's type system already enforces.
- Deserialized `PipelineDefinition` (Phase 10.1+) must be validated against a strict schema before execution — reject unknown fields, enforce type bounds, reject circular references.
- Macro-generated code operates on trusted compile-time input; runtime interpreter paths (Phase 10.1) are untrusted and must validate before execution.
- Never interpolate user-controlled strings into SQL, shell commands, or file paths without sanitisation.

### Authentication and Authorisation

- All network-facing APIs (Phase 8 WebSocket bridge, Phase 10 API gateway, Phase 11 user portal) must require authentication before accepting any request. No unauthenticated endpoints except `/health` and `/metrics`.
- Use short-lived tokens (JWT with ≤1h expiry) for API and WebSocket authentication; refresh via a dedicated token endpoint.
- Apply the principle of least privilege: API keys are scoped to the minimum required permissions; worker nodes have no access to billing or account data.
- RBAC must be enforced server-side; never trust the client to enforce its own access restrictions.

### Data Security

- User pipeline data (column values, pipeline definitions) must never appear in server logs, error messages, or telemetry payloads.
- Secrets (database credentials, API keys, Kafka passwords) must be passed via environment variables, Kubernetes Secrets, or Vault — never hardcoded, never in `PipelineDefinition` JSON.
- Encryption in transit: TLS 1.2+ on all network connections (WebSocket, Arrow Flight, HTTP). Use `rustls` over OpenSSL for pure-Rust TLS.
- Encryption at rest: pipeline definitions and audit logs stored server-side must be encrypted (AES-256). Compute data in memory is not encrypted at rest (acceptable; mitigated by tenant isolation).

### OWASP Top 10 (web-facing components)

Applies to the Studio WASM frontend (Phase 8), WebSocket bridge (Phase 8.1), user portal (Phase 11.3), and any HTTP APIs:

| # | Risk | Mitigation |
|---|---|---|
| A01 | Broken Access Control | Server-side RBAC; tenant isolation at network-policy level |
| A02 | Cryptographic Failures | TLS everywhere; AES-256 at rest; no home-grown crypto |
| A03 | Injection | Parameterised queries only; no string interpolation into SQL/shell; `PipelineDefinition` schema validation |
| A04 | Insecure Design | Threat model written before Phase 8 network code; data plane / control plane separation (Phase 11) |
| A05 | Security Misconfiguration | Helm chart hardened by default; no debug endpoints in production images; `cargo clippy` catches common misconfigs |
| A06 | Vulnerable Components | `cargo audit` in CI; dependency justification policy |
| A07 | Auth Failures | Short-lived JWTs; OIDC for SSO; no password storage (delegate to identity provider) |
| A08 | Software Integrity | `Cargo.lock` committed; verify checksums on binary releases |
| A09 | Logging / Monitoring | Audit log for all mutations (Phase 10.4); no user data in logs; anomaly alerting (Phase 11.6) |
| A10 | SSRF | `RemoteTarget` whitelist (Phase 4.6); connector URLs validated against an allowlist |

### Security Testing

- `cargo audit` — runs in CI on every PR; blocks merge on medium+ advisories.
- Fuzzing — `cargo-fuzz` targets for: `PipelineDefinition` deserialisation, macro input parsing, WebSocket message parsing. Run locally before each major release.
- Penetration test — engage an external firm before each public product launch (Phase 8 Studio, Phase 11 cloud tier).
- Dependency review — automated via `cargo audit`; manual review of changelogs on major version bumps of security-sensitive crates.

### Vulnerability Disclosure

- Maintain a `SECURITY.md` in the public repo with a responsible disclosure contact (private email or GitHub private advisory).
- Target acknowledgement within 48 hours, patch within 14 days for critical issues.
- CVE filing for vulnerabilities with a CVSS score ≥ 7.0.

---

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

---

## Phase 2 — The Macro System

**Goal:** Users define nodes with natural Rust expressions; the macro builds the DAG and generates kernels.  
**Success metric:** A 10-node pricing pipeline defined in ~15 lines of macro code.

### 2.0 Multi-Column Frame Support  ✅ COMPLETE

**Goal:** Allow a single pipeline to hold heterogeneous typed columns and address them by name from the `node!` macro.  
**Success metric:** A `Frame` of mixed types (e.g. `f64` price + `i32` quantity) can be combined in a single `node!` row expression.

- [x] Define `AnyNode` enum wrapping all 6 native types (`F32`, `F64`, `I32`, `I64`, `U32`, `U64`)
  - `From<Node<T>>` impl for each `T`; `name()`, `len()`, `is_empty()`, `data_type()` methods
- [x] Define `Frame` struct — ordered `Vec<(String, AnyNode)>` (avoids extra dep; O(n) lookup acceptable for typical column counts)
  - `new() -> Self`
  - `append(node: impl Into<AnyNode>) -> Self` — panics on duplicate name or length mismatch (fail-fast)
  - `get::<T: ArrowBacked>(&self, name: &str) -> Option<&[T]>` — typed downcast via sealed `private::Extract` trait
  - `len() -> usize`, `is_empty() -> bool`, `column_names() -> impl Iterator<Item=&str>`, `column_count() -> usize`
- [x] Update `node!` macro — multi-column mode activates when closure args carry explicit type annotations
  - Replaced `closure_arg_ident` with `closure_typed_args` → `Vec<(Ident, Option<Type>)>`
  - **Row mode typed args** `source.row(|price: f64, qty: i32| price * qty as f64)`:
    generates `let price = source.get::<f64>("price").expect("...")[__vtx_i];` per arg;
    typed arg names become deps automatically; body still scanned for additional bare deps
  - **Col mode typed arg** `source.col(|price: f64| price.sort())`:
    generates `let price = ColRef { data: source.get::<f64>("price").expect("...") };`
  - Untyped single arg → existing `recv.data[__vtx_i]` path unchanged (fully backward compatible)
  - `BodyDepCollector.excluded` changed from `&str` to `Vec<String>` to support multiple exclusions
- [x] Unit tests in `vertexrs-macro`: `closure_typed_args` (untyped/single-typed/multi-typed/mixed), `BodyDepCollector` (bare ident detection, multi-exclude, multi-segment path filtering)
- [x] Integration tests in `vertexrs`:
  - `Frame::append` / `get::<T>` roundtrip; wrong-type `get` returns `None`; missing column returns `None`
  - Duplicate column name panics; length mismatch panics
  - `node!(revenue = frame.row(|price: f64, qty: i64| price * qty as f64))` computes correctly + correct deps
  - `node!(sorted = frame.col(|price: f64| price.sort()))` computes correctly + correct deps
  - All existing single-column `node!` tests continue to pass

**Implementation notes:**
- `AnyNode` defined after `Node<T>` in `lib.rs`; `Frame` defined after `AnyNode`
- Downcast uses sealed `private::Extract` trait (supertrait of `ArrowBacked`); `impl_arrow_backed!` macro updated to include `$variant:ident` arm and now emits `Extract` + `Sealed` + `ArrowBacked` impls together; macro invocation moved after `AnyNode` definition to satisfy forward-reference ordering
- `Frame::get<T>` calls `T::try_extract(any)` — zero unsafe, no `TypeId`, no `transmute`
- **Total tests after 2.0: 93 unit + 7 macro-crate unit + 5 doctests = 105 total**
- **Type set expanded (post 2.0):** `f16`, `i8`, `i16`, `u8`, `u16` added alongside original 6; `half = "2"` added to workspace deps; `impl_node_rhs_ops!` and `AnyNode` extended to cover all 11 types


### 2.1 `node!` Macro — Basic Expression Parsing ✅ COMPLETE

**Syntax:** `node!(name = receiver.row(|x| expr))` and `node!(name = receiver.col(|c| expr))`

- [x] Parse `name = receiver.row(|arg| body)` and `name = receiver.col(|arg| body)`
- [x] Row mode: generates `(0..recv.len()).map(|__vtx_i| { let arg = recv.data[__vtx_i]; /* dep shadows */ body }).collect::<Vec<_>>()` wrapped in `Node::new_with_deps`
- [x] Col mode: calls `recv.col(|arg| body)` directly at runtime via `Node::col` method
- [x] Auto-collects dependency names from closure body (skips closure arg, call-position idents, nested closures)
- [x] Col mode now also scans closure body for dep identifiers — extra nodes referenced directly (not inside nested closures) are added to the `deps` array; receiver and self-reference filtered to avoid duplicates
- [x] `Node<T>` type: `name`, `deps: &'static [&'static str]`, `data: Vec<T>`
- [x] `ColRef<T>`: `sort()`, `filter()`, arithmetic ops between two `ColRef`s or `ColRef` and scalar
- [x] `impl_node_rhs_ops!` for rust-analyzer compat (`T op Node<T>` panicking impls for f32/f64/i32/i64/u32/u64)
- [x] Support for nodes with mixed-type inputs: row mode binds each dep as `dep.data[__vtx_i]` using its own native type; Rust's type inference unifies the expression (e.g. `Node<f64>` + `Node<i32>` dep works). `bool` is not `ArrowNativeType`; use `i32`/`u8` for flag columns.
- [x] 10 passing tests: 3 direct API, 2 arrow interop, 5 macro (row scalar, row two-nodes, col sort, col filter, row-after-col), + 2 new (row mixed-types, col dep capture)

**Known limitation:** Non-node local variables captured in row closure bodies will cause compile errors
(e.g. `let rate = 0.2; node!(x = a.row(|v| v * rate))` — macro generates `let rate = rate.data[__vtx_i]` which fails).
Workaround: inline literals, or wrap constants as `Node::from_data("rate", vec![0.2; n])`.

### 2.2 Pipeline Macro — Implicit DAG Construction ✅ COMPLETE

**Goal:** Users never declare a `Graph` — the `pipeline!` macro builds one transparently.  A pipeline is a `Frame → Frame` computation unit: it declares typed source columns, defines derived nodes, and exposes a subset of those nodes as its output `Frame`.  Sub-pipelines are first-class — they appear as a single opaque node in the parent's DAG.

**Syntax:**
```rust
let pipeline = pipeline! {
    source!(price: f64);
    source!(qty:   i32);
    node!(tax   = price.row(|x| x * 0.2));
    node!(total = price.row(|x| x + tax));
    output!(tax, total)
};
pipeline.push(Frame::new()
    .append(Node::from_data("price", prices))
    .append(Node::from_data("qty",   qtys)));
pipeline.compute()?;
let out: &Frame = pipeline.output();
let totals = out.get::<f64>("total").unwrap();
```

**`pipeline!` is self-similar — nested `pipeline!` declares an embedded sub-pipeline:**
```rust
let root = pipeline! {
    source!(price: f64);

    pipeline!(pricing {
        settings { failure: Isolate }
        source!(price: f64);           // receives same Frame as parent
        node!(tax   = price.row(|x| x * 0.2));
        node!(total = price.row(|x| x + tax));
        output!(tax, total)            // public boundary; rest is private
    });

    // `pricing` is a Frame node to everything downstream; internals are invisible
    node!(alert = pricing.row(|total: f64| total > 1000.0_f64));
    output!(alert)
};
```

**`sub!` embeds an externally-defined `Pipeline` as an opaque node:**
```rust
sub!(normaliser() => norm: f64);   // runs normaliser; binds `norm` as Node<f64>
```

- [x] `source!(name: T)` — declares a typed source column; no deps, no kernel
- [ ] `source!(name: T <- frame.col)` — **DEFERRED**: requires `Frame` as a first-class source type; blocked on `source!(market: Frame)` concept not yet designed
- [x] `output!(node, ...)` — declares which nodes are exported as the pipeline's output `Frame`; all other nodes are private
- [x] `pipeline!` macro collects all declarations in order, builds `PipelineImpl` struct:
  - Assigns a struct field to each `source!`, `node!`, nested `pipeline!`, and `sub!` declaration
  - Resolves deps by name; compile-error on unknown dep
  - Nested `pipeline!(name { ... })` compiles the inner block recursively into a `Pipeline`, then registers it as a single atomic `Frame` node in the parent whose `run()` calls the inner `Pipeline`'s `compute()` and binds its output `Frame` as a local
  - `sub!(expr => out: T, ...)` injects a pre-built external `Pipeline` and extracts named typed columns from its output `Frame`
  - Returns a `Pipeline` struct
- [x] `Pipeline` API:
  - `push(frame: &Frame)` — writes source columns from a `Frame`; also pushes to nested/sub pipelines
  - `compute() -> Result<(), PipelineError>` — executes all nodes in declaration order; drains isolated errors after run
  - `output() -> &Frame` — borrows the output `Frame` declared by `output!(...)`
  - `drain_warnings() -> Vec<String>` — surfaces warnings
  - `errors() -> &[PipelineError]` — errors from `Isolate`-mode nested pipeline failures (not propagated from `compute()`)
- [x] `settings { failure: Mode }` — per-nested-pipeline execution policy:
  - `failure_mode: FailureMode` — `Soft` (default, records warning + empty Frame), `Hard` (propagates error), `Isolate` (records error + empty Frame, parent continues)
  - `na_threshold` and `purity_check` — **DEFERRED** to Phase 2.5 (require NA infrastructure)
- [x] Encapsulation rules (apply at every nesting level):
  - Internal nodes are **not** addressable from the parent — only `output!` columns are visible
  - A nested pipeline failure in `Isolate` mode produces an empty Frame downstream; error recorded in `Pipeline::errors()`
  - A nested pipeline failure in `Hard` mode propagates as `PipelineError` from `compute()`
  - A nested pipeline failure in `Soft` mode produces an empty Frame downstream; warning recorded
- [x] Heterogeneous columns: `pipeline!` uses `Frame` at all boundaries; no homogeneous constraint
- [x] Tests:
  - 3-node pipeline matches expected output (push + compute + output round-trip)
  - Multi-source, multi-derived-node pipeline
  - `output!` only exposes declared columns; undeclared intermediate nodes absent
  - Missing source returns `PipelineError::MissingSource`
  - Push, compute, push new data, recompute — correct second result
  - Nested pipeline output is visible as a `Frame` node to downstream nodes; internal nodes absent from nested's output
  - Nested pipeline `Isolate` failure: parent continues, `errors()` records 1 error
  - `sub!` basic: single sub, output column feeds outer node
  - `sub!` multiple outputs: two output columns both consumed downstream
  - `sub!` missing output column at runtime → `MissingSource` error
  - Two independent `sub!` calls in one pipeline

**Implementation notes:**
- `PipelineImpl` sealed trait: `push_sources`, `run`, `output`, `drain_warnings`, `drain_isolated_errors`
- `Pipeline { inner: Box<dyn PipelineImpl>, isolated_errors: Vec<PipelineError> }` — outer struct
- `Pipeline::compute()` clears `isolated_errors`, calls `inner.run()`, then drains `inner.drain_isolated_errors()`
- Generated struct always has `__isolated_errors: Vec<PipelineError>` and `__warnings: Vec<String>`
- Ordered items (node!, nested pipeline!, sub!) pre-processed in a single loop with explicit `sub_idx` counter to avoid multiple-mutable-borrow issues in iterator closures
- `FailureModeKind` is a macro-crate compile-time enum (not the runtime `FailureMode`); `try_parse_settings()` uses `fork()` to peek for `settings { }` without consuming
- `PipelineError` derives `Clone` to allow storage in `Vec<PipelineError>`
- **Total tests after 2.2.0: 105 unit + 7 macro-crate unit + 5 doctests = 117 total**

### 2.3 `sub!` — Cross-Module Sub-Pipeline Composition ✅ COMPLETE

Implemented as part of 2.2.0 above. All planned tests pass. See 2.2.0 implementation notes.

### 2.4 Benchmarking Infrastructure

**Goal:** Establish a continuous benchmark suite that tracks throughput and memory for every phase going forward.  Polars is the comparison baseline for equivalent batch computations; vertexrs's incremental update behaviour is its primary differentiator and must be measured explicitly.

**Why now:** Phases 1 and 2.2 built the core engine.  Without a benchmark baseline locked in before the optimisation phases (SIMD, fusion, parallelism), there is no way to measure improvement or detect regressions.

**Toolchain:**
- `criterion = "0.5"` — statistical benchmarking with confidence intervals; `[[bench]]` sections in `vertexrs/Cargo.toml`
- `polars-lazy` + `polars-ops` as dev-dependencies (feature-gated behind `bench-polars` to avoid bloating regular builds)
- `dhat` (optional) — heap allocation profiling; enables per-benchmark peak-bytes reporting
- `jemalloc` stats already feature-flagged; enable in bench profile for allocator-level telemetry
- `critcmp` — compare saved baselines across commits

**Correctness contract:** Every benchmark file that has a Polars counterpart **must** include a `#[test]` block (or a one-shot pre-bench assertion) that runs both computations on a small fixed dataset and asserts the outputs are equal, within floating-point tolerance where applicable (`abs(vtx - polars) < 1e-6` for f32/f64; exact equality for integer types; `f16` values compared after widening to f32).  A benchmark that produces wrong answers faster than Polars is not a win.

**Benchmark files (`vertexrs/benches/`):**

`column.rs` — low-level column storage
- `column_from_slice_1m` — `ChunkedColumn::from_slice` on 1M f64 values
- `column_read_throughput` — sequential read of a 1M-element column
- `column_dirty_propagation` — mark + clear dirty bitmap on 1M rows

`pipeline.rs` — full pipeline throughput vs Polars
- `vtx_pipeline_3node_1m` — 3-node pricing pipeline (price, tax, total) on 1M rows; full recompute
- `polars_pipeline_3node_1m` — equivalent Polars `LazyFrame` plan for direct comparison
- `vtx_pipeline_5node_1m` — 5-node pipeline; multi-source (f64 + i32)
- `polars_pipeline_5node_1m` — Polars equivalent
- Both pipeline benchmarks are **parametrized over dtypes**: `f32`, `f64`, `i32`, `i64`, `u32`, `u64`, `f16`; results tracked per-type so regressions in one type don't hide under another
- Polars does not support `f16` natively — f16 benchmarks are vertexrs-only, measuring raw throughput relative to `f32`

`incremental.rs` — incremental recompute; vertexrs's key differentiator
- `vtx_incremental_1pct` — update 1% of 1M rows in a 3-node pipeline, recompute
- `vtx_incremental_10pct` — update 10% of 1M rows
- `vtx_incremental_100pct` — full re-push (equivalent to full recompute baseline)
- `polars_full_recompute` — Polars must always do full recompute; provides the denominator
- Incremental benchmarks also parametrized over `f64`, `i64`, `f32` (the three most common finance dtypes)
- Expected: vertexrs 1% update ≥ 10× faster than Polars full recompute at 1M rows

**Memory measurement approach:**
- Use `criterion`'s `Throughput::Bytes` to report MB/s alongside ns/iter
- Use `jemalloc` epoch-flush + `mallctl` stats to capture peak bytes per bench group
- Target: vertexrs peak RSS ≤ 2× Polars for full recompute (acceptable before SIMD; closure data layout not yet optimised)

**Regression gating:**
- `cargo bench --save-baseline main` on every merge to main
- CI runs `cargo bench` + `critcmp main` — fail if any benchmark regresses > 15%
- Throughput benchmarks (MB/s) used for regression; latency benchmarks (ns) for profiling only

**Success criteria — tiered by phase:**

*Pre-SIMD (now through Phase 2.10):*
- [ ] Full recompute: within 5× of Polars on a 3-node pipeline at 1M rows across all comparable dtypes (`f32`, `f64`, `i32`, `i64`, `u32`, `u64`)
- [ ] Incremental 1% update: ≥ 10× faster than Polars full recompute at 1M rows
- [ ] Memory: vertexrs peak ≤ 2× Polars for equivalent computation

*Post-SIMD (Phase 3.3+):*
- [ ] Full recompute: parity with Polars (within 1.2×) on `f32`, `f64`, `i32`, `i64` at 1M rows
- [ ] `f16` pipeline: within 1.5× of equivalent `f32` pipeline (SIMD throughput should be ~equal, overhead tracked)
- [ ] `u8`/`u16` pipelines: within 1.5× of `u32` pipeline at same row count
- [ ] Incremental 1% update: ≥ 20× faster than Polars full recompute (SIMD makes full recompute faster, so the bar rises)
- [ ] Memory: vertexrs peak ≤ 1.2× Polars for equivalent computation

*Continuous (all phases):*
- [ ] No benchmark regresses > 15% between commits without an explicit explanation
- [ ] Cross-dtype spread: no single dtype should be > 2× slower than the fastest dtype for the same logical pipeline (catches per-type codegen regressions)

**Implementation checklist:**
- [x] Add `criterion` and `polars` to `[workspace.dependencies]` in workspace `Cargo.toml`; `polars` is optional behind the `bench-polars` feature in `vertexrs/Cargo.toml`
- [x] Configure `[[bench]]` entries in `vertexrs/Cargo.toml` for `column`, `pipeline`, `incremental`
- [x] Write `benches/column.rs` — `column_from_slice` (6 dtypes), `column_read_throughput`, `column_dirty_propagation` (1%, 10%, 100%)
- [x] Write `benches/pipeline.rs` — vertexrs 3-node pipeline at 1M rows parametrized over `f32`, `f64`, `i32`, `i64`, `u32`, `u64`; Polars comparison group behind `bench-polars`; correctness `#[test]` blocks for f64, f32, i64, i32
- [x] Write `benches/incremental.rs` — 1% / 10% / 100% incremental update, dtypes `f64`, `f32`, `i64`; Polars full-recompute baseline behind `bench-polars`; correctness tests for f64 and i64
- [x] Correctness tolerance rules applied: `abs(vtx − polars) < 1e-6` for f32/f64; exact equality for integer types
- [ ] Save initial baseline: `cargo bench --save-baseline initial`
- [ ] Document results in `.claude/plans/bench-baseline.md`

**Future benchmark expansion (do after each major feature phase):**
- Group-by aggregation pipelines (sum, mean, count) — the most common real-world workload; Polars has highly optimised hash-group-by for comparison
- Multi-source joins / merge pipelines
- String column pipelines (once variable-length types land in 2.2.X)
- Fan-out DAGs: one source node feeding multiple independent downstream chains
- Wider pipelines: 10-node and 20-node chains to measure per-node overhead scaling
- Mixed-dtype pipelines: f64 source feeding both f32 and i64 derived nodes
- Multi-threaded scaling benchmarks: run the same pipeline with 1/2/4/8 threads (via rayon thread-pool size) and assert near-linear scaling; Polars comparison is reasonable here since it also uses rayon internally, though direct comparison is less meaningful than the scaling curve itself
- Tighten `RATIO_CAP` in `tests/pipeline_perf.rs` to 1.2× (post-SIMD target) once SIMD lands

**Note:** Incremental benchmarks currently show 1pct ≈ 10pct ≈ 100pct (all ~12ms f64). This is expected — the pipeline executor does a full recompute today. Dirty-chunk-based partial recomputation is wired into `ChunkedColumn` but not yet into the pipeline executor. The speedup will appear automatically when the executor is updated in a future phase.

### 2.5 Variable-Length and Non-Primitive Column Types

**Goal:** Extend `Frame` and `AnyNode` to support Arrow types that cannot be stored in `ScalarBuffer<T>` — strings, binary, booleans, and lists.

**Design constraint:** `Node<T: ArrowNativeType>` is locked to fixed-size primitives by the `ScalarBuffer<T>` backend.  Variable-length types require a parallel column variant backed by their respective Arrow array types.

**Tier 2a — `bool` column:**
- [x] Arrow packs booleans as bits (`BooleanArray`); `bool` is not `ArrowNativeType`
- [x] New `BoolNode` struct backed by `arrow_array::BooleanArray`
- [x] `AnyNode::Bool(BoolNode)` variant; `Frame::get_bool(name) -> Option<&BoolNode>`
- [x] `node!` macro support: `source.row(|x| -> bool { x > 0.0 })` produces a `BoolNode` when closure has explicit `-> bool` return type
- [ ] `bool` columns usable as masks in `CondKernel` (Phase 1.4) and `filter` col ops

**Tier 2b — `String` / `LargeString` column:**
- [x] New `StringNode` struct backed by `arrow_array::StringArray` (Utf8)
- [x] `AnyNode::Str(StringNode)` variant; `Frame::get_str(name) -> Option<&StringNode>`
- [ ] `node!` macro: `source.row(|s: &str| s.len() as i64)` — string-to-numeric projection
- [x] String columns are read-only input sources in Phase 2.5; string-output nodes deferred

**Tier 2c — `Binary` column:**
- Binary column support deferred until needed

**Tier 2d — `List<T>` column:**
- List column support deferred until needed

**Architectural approach:**
- [x] `AnyNode` extended with `Bool(BoolNode)` and `Str(StringNode)` variants; `From` impls added
- [x] `ArrowBacked` trait stays fixed-size-only; `Frame::get_bool` / `Frame::get_str` are separate accessors

**Ordering:** Tier 2a (`bool`) first (needed for filter masks); Tier 2b (`String`) second (most common user request); Tier 2c/2d deferred until needed.

### 2.6 Failure Mode Syntax

- [ ] `node!(x = expr?)` → soft failure, NA on error
- [ ] `node!(x = expr!)` → hard failure, halt on error
- [ ] `node!(x = expr, pure = false)` → impure node, always fully dirty
- [ ] Default: pure = true, failure = soft

### 2.7 Kernel Fusion Pass

- [ ] Walk DAG and identify fusable chains (pointwise + preserves length + pure)
- [ ] Collapse chains into single fused kernel
- [ ] Emit fused kernel — all ops in one loop, data stays in registers
- [ ] Benchmark: fused vs unfused on a 5-op chain

### 2.8 Struct Node Projection

- [ ] `#[derive(NodeOutput)]` macro for struct outputs
- [ ] Downstream field access (`bs.delta`) generates free projection node
- [ ] No recomputation — projection is a field access on cached struct column

### 2.9 Stateful Nodes

- [ ] `#[stateful]` annotation — state lives alongside node, persists between updates
- [ ] `state.get::<T>()` / `state.set(value)` API inside node body
- [ ] `#[recompute_when(dep)]` — node only dirty when named dependency changes
- [ ] Test: EMA node accumulates correctly across 100 update cycles

### 2.10 Optimizing

- [ ] Benchmark graph against Polars. Identify bottlenecks and optimise critical paths (e.g. chunk allocation, dirty tracking, kernel execution). Performance should be a core focus from day one, with benchmarks driving design decisions. Results should be faster or comparable to Polars on equivalent pipelines.

---

### 2.11 Polars Feature Parity

**Goal:** Implement all major Polars DataFrame operations as first-class VertexRS operations and verify correctness and throughput against Polars.  
**Success metric:** Every item below has (a) a correctness test that compares VertexRS output to the equivalent Polars output within tolerance (`abs(vtx − polars) < 1e-6` for `f32`/`f64`; exact equality for integer types; `f16` widened to `f32` before comparison), and (b) a Criterion benchmark comparing throughput; and where applicable a third benchmark demonstrating the incremental recompute advantage over Polars full-recompute.

**Correctness tolerance convention (applies to every item in this phase):**
- `f32`/`f64`: `abs(vtx − polars) < 1e-6`
- integer types (`i32`, `i64`, `u32`, `u64`, `i8`, `i16`, `u8`, `u16`): exact equality
- `f16`: widen both sides to `f32` before comparing with the `f32` tolerance above
- All correctness tests use the Polars dev-dependency gated behind the `bench-polars` feature flag

---

#### 2.11.1 Joins

VertexRS has no join implementation. Joins require a gather index (`u32` column), null/validity support for outer joins, and dirty propagation through index remapping.

**Planned API:** `Frame::join(&self, right: &Frame, on: &str, how: JoinHow) -> Frame` — a standalone function rather than macro-level syntax (macro join syntax is deferred to a later phase).

**Join types to implement:**

- [ ] **Inner join** — `JoinHow::Inner` — rows present in both left and right on the key; no nulls produced
- [ ] **Left join** — `JoinHow::Left` — all left rows; unmatched right rows produce nulls
- [ ] **Right join** — `JoinHow::Right` — all right rows; unmatched left rows produce nulls
- [ ] **Full outer join** — `JoinHow::Full` — all rows from both sides; unmatched rows produce nulls; optional `coalesce` to merge key columns
- [ ] **Semi join** — `JoinHow::Semi` — left rows that have at least one match in right; no right columns emitted
- [ ] **Anti join** — `JoinHow::Anti` — left rows that have no match in right; no right columns emitted
- [ ] **Cross join** — `JoinHow::Cross` — Cartesian product; no key required; produces `|left| × |right|` rows
- [ ] **Asof join** — `JoinHow::Asof { strategy: AsofStrategy, tolerance: Option<T>, by: Option<&str> }` — match each left row to the nearest right row on a sorted key; `AsofStrategy::Backward` (default, ≤) and `AsofStrategy::Forward` (≥); `tolerance` limits how far a match can be; `by` adds an exact pre-filter group key; left and right key columns must be pre-sorted (panic if not)
- [ ] **Non-equi / predicate join** — `Frame::join_where(right, predicate_fn)` — evaluate a user-supplied closure over each left/right row pair; produces all matching pairs; analogous to Polars `join_where`

**Null/validity support required for outer joins:**
- [ ] Add `Option<arrow_buffer::BooleanBuffer>` validity bitmap field to `AnyNode` variants (or introduce a separate `NullableAnyNode` wrapper) — only required for outer join output columns; inner/semi/anti join output is always non-null
- [ ] Downstream nodes that receive nullable columns must propagate the validity bitmap (null-in → null-out); this is the Arrow null semantics model

**Correctness tests (one per join type):**
- [ ] Each test constructs a left `Frame` and right `Frame` with a shared key column, performs the join in VertexRS and in Polars, and asserts row counts and all value columns match within tolerance
- [ ] Asof join test includes a case with `tolerance` and a case with `by` grouping
- [ ] Non-equi join test uses a numeric predicate (e.g. `left_price > right_strike`)

**Throughput benchmarks (parametrised over `f32`, `f64`, `i32`, `i64`):**
- [ ] Inner join: 1M left × 1M right rows (low collision key), VertexRS vs Polars — throughput in rows/s
- [ ] Left join: 500K left × 1M right rows (some unmatched left rows)
- [ ] Asof join: 1M ticks × 10K reference rates (financial time-series pattern)
- [ ] Cross join: 10K × 10K rows (100M output rows)

**Incremental benchmark:**
- [ ] Mutate 1% of the left key column, re-run the join; measure VertexRS incremental time vs Polars full-recompute time — demonstrate the dirty-chunk advantage

---

#### 2.11.2 Group-By Aggregations

**Planned API:** `Frame::group_by(keys: &[&str]).agg(exprs: &[AggExpr]) -> Frame`

**Aggregation functions to implement:**

- [ ] `AggExpr::Sum` — sum of column per group
- [ ] `AggExpr::Mean` — mean per group
- [ ] `AggExpr::Min` / `AggExpr::Max`
- [ ] `AggExpr::Count` / `AggExpr::Len` — row count per group (aliased)
- [ ] `AggExpr::First` / `AggExpr::Last` — first/last row value per group (insertion order)
- [ ] `AggExpr::Std` / `AggExpr::Var` — sample standard deviation / variance per group (ddof = 1)
- [ ] Multi-key group-by — `keys: &[&str]` with two or more key columns; composite key hashing
- [ ] Filtered aggregation — `AggExpr::Filter { inner: Box<AggExpr>, predicate: Predicate }` — aggregate only rows matching predicate within each group (e.g. count where value > 0)
- [ ] Sort-within-group — `AggExpr::SortedFirst { sort_by: &str, descending: bool }` — sort group rows by `sort_by` and take first; equivalent to Polars `.sort_by(col).first()` inside `.agg()`

**Incremental group-by (Phase 3.4 is the full implementation; this phase specifies the correctness and benchmark requirements):**
- [ ] Correctness: appending new rows to a source column and calling `group_by` produces the same result as a full Polars `group_by` on all rows
- [ ] Correctness: mutating a key column row (group membership change) produces the correct output after recompute

**Correctness tests:**
- [ ] Single-key, all aggregation functions: `f32`, `f64`, `i32`, `i64` — compare to Polars `group_by().agg()` within tolerance
- [ ] Multi-key (2 keys) sum and mean
- [ ] Filtered aggregation: count rows > 0 per group
- [ ] Sort-within-group first

**Throughput benchmarks:**
- [ ] 1M rows, 100 groups, `sum` and `mean`: VertexRS vs Polars — `f32`, `f64`, `i32`, `i64`
- [ ] 1M rows, 10K groups (high-cardinality): same aggregations
- [ ] Incremental: add 1% new rows; VertexRS incremental vs Polars full-recompute

---

#### 2.11.3 Window Functions

Window functions compute an expression per group and map the result back to the original row positions (unlike group-by, which reduces to one row per group).

**Planned API:** `node!(out = col.over("group_col", AggExpr::Mean))` or equivalent col-mode syntax.

**Mapping strategies (mirroring Polars `mapping_strategy`):**

- [ ] `MappingStrategy::GroupToRows` (default) — aggregate result broadcast back to all rows in the group; preserves original row order
- [ ] `MappingStrategy::Explode` — groups are contiguous in the output (faster, changes row order)
- [ ] `MappingStrategy::Join` — aggregate result collected into a `List` column per row (one list value per row containing the group's aggregated value)

**Operations to implement over groups:**

- [ ] Scalar broadcast: `mean`, `sum`, `min`, `max` per group mapped back to every row in that group
- [ ] Multi-column grouping: `over(&["col1", "col2"])` — composite key
- [ ] Running window (rolling): `rolling_mean(window: usize)`, `rolling_sum(window: usize)`, `rolling_min`, `rolling_max` — computed over the preceding `window` rows within each group (requires sorted input within group)

**Correctness tests:**
- [ ] `mean().over("type")` on 100K rows, 10 groups: VertexRS output matches Polars `mean().over("type")` within tolerance, all mapping strategies
- [ ] Multi-column group `over(&["a","b"])`: compare to Polars
- [ ] Rolling mean (window = 5, 20): compare to Polars `rolling_mean`

**Throughput benchmarks:**
- [ ] 1M rows, 10 groups, `mean().over(...)`: VertexRS vs Polars — `f32`, `f64`
- [ ] 1M rows, 1K groups (medium cardinality)
- [ ] Rolling mean over 1M rows: VertexRS vs Polars

---

#### 2.11.4 Basic Scalar Operations

Most of these are already partially implemented via `node!` col/row modes. This item audits completeness and adds any missing operations.

**Arithmetic:**
- [ ] `+`, `-`, `*`, `/` — all numeric types; confirmed implemented
- [ ] `**` (power / `pow`) — `f32`, `f64`; integer power via repeated multiplication
- [ ] `%` (remainder / mod) — integer and float
- [ ] Broadcast: scalar literal op column (e.g. `col * 0.2`)

**Comparisons:**
- [ ] `>`, `>=`, `<`, `<=`, `==`, `!=` — all numeric types; output is `bool` / `u8` (1/0) column

**Boolean / bitwise:**
- [ ] `&` (and), `|` (or), `~` (not), `^` (xor) — on bool/u8 columns and integer types

**Conditional:**
- [ ] `when(predicate_col).then(value_col).otherwise(default_col)` — element-wise ternary; output type matches `then`/`otherwise` columns; equivalent to Polars `when().then().otherwise()`; chainable (multiple when/then branches before `otherwise`)

**Unique / count operations:**
- [ ] `n_unique(col)` — count of distinct values in a column (scalar output)
- [ ] `approx_n_unique(col)` — HyperLogLog++ cardinality estimate (scalar output); acceptable relative error ≤ 2% at 1M rows
- [ ] `value_counts(col) -> Frame` — returns a two-column `Frame` (value, count), sorted by count descending
- [ ] `unique(col)` — de-duplicated column (order not guaranteed)
- [ ] `unique_counts(col)` — de-duplicated column with a parallel count column

**Correctness tests:**
- [ ] `pow`, `%`: compare to Polars for `f32`, `f64`, `i32`, `i64`
- [ ] All comparison operators: compare to Polars on 100K rows
- [ ] `when/then/otherwise`: two-branch and three-branch; compare to Polars
- [ ] `value_counts`: compare to Polars (sort both outputs before comparing to handle order differences)
- [ ] `approx_n_unique`: result within 2% of `n_unique` for 1M rows with 100K distinct values

**Throughput benchmarks:**
- [ ] `pow` and `%` on 1M elements: VertexRS vs Polars — `f32`, `f64`, `i32`, `i64`
- [ ] `when/then/otherwise` on 1M rows: VertexRS vs Polars
- [ ] `value_counts` on 1M rows, 1K distinct values: VertexRS vs Polars

---

#### 2.11.5 Reshaping and Concatenation

**Concatenation:**
- [ ] `Frame::vstack(other: &Frame) -> Frame` — vertical stack (append rows); column names and types must match (panic on mismatch)
- [ ] `Frame::hstack(other: &Frame) -> Frame` — horizontal stack (append columns); row counts must match (panic on mismatch); column names must be distinct (panic on duplicate)

**Pivot:**
- [ ] `Frame::pivot(index: &str, columns: &str, values: &str, agg: AggExpr) -> Frame` — reshape from long to wide; one row per unique `index` value, one output column per unique `columns` value, cells aggregated by `agg` (default `AggExpr::First`)

**Unpivot (melt):**
- [ ] `Frame::unpivot(id_vars: &[&str], value_vars: &[&str]) -> Frame` — reshape from wide to long; produces `id_vars` columns + a `variable` string column + a `value` column; equivalent to Polars `unpivot` / `melt`

**Correctness tests:**
- [ ] `vstack`: stack two 50K-row `Frame`s, compare concatenated output to Polars `vstack`
- [ ] `hstack`: combine two disjoint-column `Frame`s, compare to Polars `hstack`
- [ ] `pivot`: long-to-wide on a 100K-row `Frame` with 10 group keys and 5 column categories; compare to Polars `pivot`
- [ ] `unpivot`: wide-to-long on a 10-column `Frame`; compare to Polars `unpivot`

**Throughput benchmarks:**
- [ ] `vstack` 1M rows: VertexRS vs Polars
- [ ] `pivot` 1M rows, 100 categories: VertexRS vs Polars

---

#### 2.11.6 Missing Data

**Operations:**
- [ ] `fill_null_literal(col, value: T) -> Node<T>` — replace nulls with a constant literal
- [ ] `fill_null_forward(col) -> Node<T>` — fill nulls with the preceding non-null value (forward fill / LOCF); nulls at the start of the column remain null
- [ ] `fill_null_backward(col) -> Node<T>` — fill nulls with the following non-null value (backward fill); nulls at the end of the column remain null
- [ ] `drop_nulls(frame: &Frame) -> Frame` — drop all rows that contain at least one null in any column

**Validity bitmap prerequisite:** All of the above require the nullable `AnyNode` variant introduced in 2.9.1. These items are therefore blocked on completing the validity bitmap work.

**Correctness tests:**
- [ ] `fill_null_literal`: 10% null column, fill with 0.0; compare to Polars `fill_null(0.0)`
- [ ] `fill_null_forward`: 10% null column; compare to Polars `forward_fill()`
- [ ] `fill_null_backward`: compare to Polars `backward_fill()`
- [ ] `drop_nulls`: multi-column frame with nulls in different columns; compare to Polars `drop_nulls()`

**Throughput benchmarks:**
- [ ] `fill_null_forward` on 1M rows, 10% nulls: VertexRS vs Polars
- [ ] `drop_nulls` on 1M rows, 10% nulls: VertexRS vs Polars

---

#### 2.11.7 Type Casting

- [ ] `cast::<T>(col) -> Node<T>` — numeric type coercion; supported casts mirror Arrow safe casts: all numeric → all numeric widening/narrowing, integer ↔ float, bool ↔ integer
- [ ] Overflow behaviour: narrowing casts that overflow saturate (not UB); document this as the defined behaviour
- [ ] `f16` ↔ `f32` ↔ `f64` widening and narrowing

**Correctness tests:**
- [ ] `i32 → f64`, `f64 → i32` (with truncation), `u32 → i64`, `f32 → f16`: compare to Polars `cast()` within tolerance

**Throughput benchmarks:**
- [ ] `f32 → f64` and `i32 → f64` on 1M elements: VertexRS vs Polars

---

#### 2.11.8 String Operations

String columns are required for joins on string keys, group-by on string keys, and `value_counts` on categorical data.

- [ ] Add `AnyNode::Utf8(ScalarBuffer<u8> data, ScalarBuffer<u32> offsets)` — variable-length string column backed by Arrow `StringArray` layout
- [ ] `str_len(col) -> Node<u32>` — byte length per string
- [ ] `str_contains(col, pattern: &str) -> Node<bool>` — element-wise substring test
- [ ] `str_starts_with` / `str_ends_with`
- [ ] `str_to_uppercase` / `str_to_lowercase`
- [ ] `str_replace(col, from: &str, to: &str)` — replace first occurrence per element
- [ ] `str_slice(col, offset: i64, length: Option<u64>)` — substring by byte offset/length
- [ ] Support string columns as group-by keys in 2.9.2 and join keys in 2.9.1

**Correctness tests:**
- [ ] Each string operation on a 100K-string column: compare to Polars `str` namespace equivalent

**Throughput benchmarks:**
- [ ] `str_contains` on 1M strings: VertexRS vs Polars

---

#### 2.11.9 Time-Series Operations

- [ ] Add `AnyNode::Timestamp(ScalarBuffer<i64>)` — microseconds since Unix epoch; mirrors Arrow `TimestampMicrosecond`
- [ ] `rolling_mean_time(col, window_duration: Duration)` — rolling mean over a fixed time window (requires sorted timestamp column)
- [ ] `rolling_sum_time`, `rolling_min_time`, `rolling_max_time`
- [ ] `resample(ts_col, rule: ResampleRule, agg: AggExpr)` — group by fixed time buckets (second, minute, hour, day) and aggregate; equivalent to Polars `group_by_dynamic`
- [ ] Time range filter: `filter_time_range(ts_col, from: Timestamp, to: Timestamp)` — keep only rows within the range

**Correctness tests:**
- [ ] Rolling mean (window = 5 minutes) on 1M tick dataset: compare to Polars `rolling_mean` with a `by="ts"` option
- [ ] Resample to 1-minute OHLC (open = first, high = max, low = min, close = last): compare to Polars `group_by_dynamic`

**Throughput benchmarks:**
- [ ] Rolling mean (5-min window) on 1M ticks: VertexRS vs Polars
- [ ] Incremental: append 1% new ticks; VertexRS incremental recompute vs Polars full rolling_mean — target ≥ 10× speedup on incremental path

---

#### 2.11.10 Benchmark Summary Requirements

Every benchmark in this phase must:

1. Live in `vertexrs/benches/` in a file named `polars_parity_<feature>.rs` (e.g. `polars_parity_joins.rs`, `polars_parity_groupby.rs`)
2. Be parametrised over at least `f32`, `f64`, `i32`, `i64` where the operation is type-generic
3. Include a `#[test]` correctness assertion (not just a benchmark) using the tolerance convention defined above
4. Save a Criterion baseline on every merge to `main`: `cargo bench --save-baseline main`
5. Flag any regression > 15% throughput vs the saved baseline as a CI failure
6. Include a VertexRS incremental benchmark (1% mutation) alongside the full-recompute benchmark wherever the operation sits on the recompute hot path (joins, group-by, window functions, time-series rolling)

The Polars dev-dependency must be gated behind the `bench-polars` feature flag in `vertexrs/Cargo.toml` so Polars is never compiled into the release library.

---

## Phase 2.12 — Process and Task Graph Execution

**Goal:** Make non-columnar, struct-typed nodes a first-class citizen of the `pipeline!` macro so VertexRS can express general-purpose process graphs — business-logic orchestration, approval flows, pricing engines, build-like dependency graphs — with the same incremental recomputation guarantees already in place for columnar data.

**Motivation:** The dirty-chunk incremental model and the DAG macro syntax are not inherently data-specific. A node that produces a `RiskReport` struct or a `Vec<Order>` benefits from the same "only recompute when inputs change" semantics as a node that produces a `ChunkedColumn<f64>`. This phase delivers on that promise by implementing the task/rayon execution path for heavy (non-`ArrowNativeType`) types, completing the type-driven dispatch described in Phase 2.2.

**Success metric:** A 10-node process graph (mixing struct-output nodes and columnar nodes) can be declared in ~20 lines of `pipeline!` macro code, executes incrementally, and routes struct nodes to rayon while columnar nodes stay on the SIMD path — all transparently, with no user-facing mode selection.

---

### 2.12.1 Type-Driven Dispatch

**Design:** Whether a node uses the columnar/SIMD path or the task/rayon path is determined entirely by its output type. No user annotation is needed for default dispatch.

| Output type | Execution path | Dirty granularity | Notes |
|---|---|---|---|
| `T: ArrowNativeType` (primitive) | Columnar / SIMD | Chunk (256 elements) | existing path |
| `Option<T: ArrowNativeType>` | Columnar with validity bitmap | Chunk | existing path |
| `bool` | Columnar (`BooleanArray`) | Chunk | Phase 2.5 Tier 2a |
| `String` / `&str` | Columnar (`StringArray`) | Chunk | Phase 2.5 Tier 2b |
| `Vec<T: Send + Sync>` | Task — collection node | Item | each item is an independent `Vec<T>` |
| Any other `T: Send + Sync + 'static` (struct, enum, …) | Task / rayon | Item (one value per "row") | plain structs without `#[derive(NodeOutput)]` |

**Struct dispatch rule:** A plain struct without `#[derive(NodeOutput)]` routes to the task path (this phase). A struct with `#[derive(NodeOutput)]` routes to the struct-of-arrays columnar path (Phase 2.8 — an optimisation, not required for correctness).

**`#[task(cost = ...)]` annotation** is an optional scheduler hint used by Phase 3.1 to choose rayon chunk sizing. The macro parses and attaches it to the node descriptor; the executor ignores it until Phase 3.1 is implemented.

- [ ] Extend `AnyNode` with a `Task(TaskNode)` variant
- [ ] Define `TaskNode` — holds output items as `Vec<Arc<dyn Any + Send + Sync>>` with a parallel `RoaringBitmap` dirty index (item granularity)
- [ ] `node!` macro: when the closure return type is neither a recognised columnar type nor `Option<columnar>`, emit a task-path kernel wrapper instead of the columnar path
- [ ] `Vec<T>` output: treated as a task collection node — each "row" produces one `Vec<T>`; stored as `Vec<Arc<Vec<T>>>` in `TaskNode`
- [ ] Parse `#[task(cost = Microseconds | Milliseconds | Seconds)]` attribute on `node!`; attach to `NodeDescriptor` as `Option<CostHint>`; executor ignores until Phase 3.1
- [ ] Task node dirty propagation: a dirty item in a task node marks the corresponding item dirty in all downstream nodes
- [ ] Typed accessor: `pipeline.get_task::<T>(name) -> Option<&[Arc<T>]>` — downcasts `Arc<dyn Any>` to `Arc<T>`; returns `None` if name unknown or type mismatch
- [ ] Compile-time error if a task node output type does not implement `Send + Sync`
- [ ] Unit tests: task node creation, dirty propagation through task → columnar edge and columnar → task edge, `Vec<T>` collection node round-trip

### 2.12.2 Process Graph User API

**Example — instrument pricing pipeline mixing struct and columnar nodes:**

```rust
#[derive(Clone)]
struct Greeks { delta: f64, gamma: f64, vega: f64 }

let pipeline = pipeline! {
    source!(spot:       f64);
    source!(vol:        f64);
    source!(strike:     f64);

    // struct-output node → task path; dirty per instrument
    node!(greeks = spot.row(|s: f64, v: f64, k: f64| {
        Greeks { delta: bs_delta(s, v, k), gamma: bs_gamma(s, v, k), vega: bs_vega(s, v, k) }
    }));

    // projection nodes — columnar, derived from the struct field
    node!(delta = greeks.row(|g: Greeks| g.delta));
    node!(gamma = greeks.row(|g: Greeks| g.gamma));

    // downstream columnar node — only reruns for dirty instruments
    node!(pnl = delta.row(|d: f64, s: f64| d * s));

    output!(delta, gamma, pnl)
};
```

- [ ] `node!` row mode: when a typed arg resolves to a task node, generate `let arg = pipeline.get_task::<T>(name).unwrap()[__vtx_i].as_ref().clone();`
- [ ] `node!` struct-field projection shorthand: `node!(delta = greeks.field(|g: Greeks| g.delta))` — zero-cost projection that avoids a full clone of the struct; generates a columnar node whose kernel accesses `greeks[i].delta`
- [ ] `output!` can include both columnar and task nodes; `Frame::get` for columnar, `pipeline.get_task::<T>` for task
- [ ] Tests:
  - Greeks pipeline above: push 1000 instruments, compute, verify `delta` output matches a direct Black–Scholes calculation to `1e-6`
  - Mixed graph: task node depending on a columnar node; columnar node depending on a task node
  - Struct projection node: verify it produces the same values as accessing the field manually

### 2.12.3 Incremental Correctness for Task Nodes

The primary value of task nodes is that only the items whose inputs changed are recomputed. This section specifies the correctness requirements.

- [ ] Push 1000 instruments, compute; mutate 10 instruments; recompute — assert exactly 10 `greeks` items were recomputed (verified via a recompute counter hooked into the kernel)
- [ ] Diamond join with a task node: two columnar sources feed one task node; mutate source A for 5 instruments; verify only those 5 instruments' task outputs are dirty after propagation
- [ ] Task → columnar propagation: dirty items in a task node mark the corresponding chunks dirty in downstream columnar nodes; verify via dirty bitmap inspection after partial update
- [ ] Columnar → task propagation: a dirty chunk in a columnar source marks all items within that chunk's row range dirty in downstream task nodes

### 2.12.4 Benchmarks

Unlike the columnar benchmarks (which compare to Polars), process graph benchmarks compare to hand-written Rust code that always recomputes everything — the baseline that VertexRS replaces.

- [ ] `bench_greeks_full` — compute Greeks for 10,000 instruments from scratch; VertexRS vs direct loop
- [ ] `bench_greeks_1pct_incremental` — update 1% of spot prices; VertexRS incremental vs direct full recompute; target ≥ 10× speedup
- [ ] `bench_greeks_10pct_incremental` — update 10% of spot prices; same comparison
- [ ] `bench_approval_flow` — a 5-step linear process graph (validate → enrich → score → approve → notify) over 10,000 items; incremental update of 1% of inputs
- [ ] All benchmarks include a `#[test]` correctness assertion (output matches direct calculation to `1e-6` for f64 fields)

---

## Phase 3 — Parallelism

**Goal:** Full multi-core utilisation on a single machine.  
**Success metric:** Near-linear scaling to available cores on independent subgraphs.

### 3.1 Rayon Integration for Task Nodes

- [ ] Detect task nodes in scheduler
- [ ] Execute dirty task rows via `rayon::par_iter`
- [ ] Implement `CostHint`-based chunk sizing:
  - `Microseconds` → chunk size 64
  - `Milliseconds` → chunk size 1
  - `Seconds` → dedicated thread
- [ ] Benchmark: 10,000 instrument pricing across 8 cores

### 3.2 Rayon for Independent Columnar Subgraphs

- [ ] Detect independent subgraphs in DAG (no shared dependencies)
- [ ] Execute independent subgraphs in parallel via Rayon
- [ ] Ensure dirty bitmap updates are thread-safe (atomic or per-partition)

### 3.3 SIMD Kernels

- [ ] Enable `target-cpu=native` in release profile
- [ ] Verify autovectorisation of fixed-chunk kernels via assembly inspection
- [ ] Implement `portable_simd` versions of hot kernels: arithmetic, comparison, null check
- [ ] Implement runtime CPU feature dispatch via `multiversion` crate
- [ ] Benchmark: scalar vs autovectorised vs explicit SIMD on 256-element chunks

### 3.4 Group-By Execution

- [ ] Implement `GroupBy` struct — `HashMap<KeyValue, Vec<usize>>` + `row_to_group` inverse
- [ ] Implement incremental append: hash new rows, mark only affected groups dirty
- [ ] Implement incremental mutation: remove from old group, add to new group, mark both dirty
- [ ] Implement aggregate recompute — only dirty groups, gather-and-aggregate per group
- [ ] Implement gather strategies: scatter (small groups), sorted indices (medium), contiguous copy (large)
- [ ] Warn user if group-by key column is updated (expensive full rebuild)

### 3.5 Local GPU Execution

**Goal:** Offload columnar kernels to a local GPU when one is available, giving GPU-grade throughput on large dirty batches without changing the pipeline definition.  The user annotates a node (or lets the scheduler decide) and execution transparently routes dirty chunks through a GPU kernel rather than a CPU SIMD kernel.  Analogous to what Polars GPU Engine (RAPIDS cuDF backend) does but integrated natively into the dirty-chunk incremental model — only dirty chunks are transferred, not the whole column.

**Execution model:**
- `GpuExecutionStrategy` variant added to the type-driven dispatch introduced in Phase 2.2 — complements `Columnar` and `Task`
- `#[gpu]` node annotation opts a node into GPU execution; the scheduler falls back to CPU if no GPU is detected at runtime
- Feature-gated: `--features gpu-local`; zero overhead and no GPU dependency when the feature is absent
- Dirty chunks only: the same `RoaringBitmap` dirty index drives which chunks are uploaded — large unchanged regions never cross the PCIe bus
- On unified-memory hardware (Apple Silicon, NVIDIA Grace-Hopper) use zero-copy buffer sharing where the allocator supports it

**Backend abstraction (`GpuBackend` trait):**
- `MockGpuBackend` — executes all kernels on CPU using the same interface as the real backends; the default when no GPU hardware is detected, and the backend used in all unit tests and CI; allows the entire Phase 3.5 API to be developed, tested, and shipped before any GPU hardware is available
- `wgpu` — cross-platform (Vulkan, Metal, DX12, WebGPU); primary portable backend; WGSL compute shaders for arithmetic kernels; also supports software Vulkan adapters (Mesa `lavapipe`, Google `SwiftShader`) — wgpu tests run on GPU-less CI machines via this path
- `cudarc` — CUDA-specific backend for NVIDIA hardware; provides access to cuBLAS, cuDNN, and custom CUDA kernels for maximum throughput; only compiled when `--features gpu-cuda` is set; CUDA tests are skipped in CI unless a CUDA device is present (`#[cfg_attr(not(feature = "gpu-cuda"), ignore)]`)
- `metal` — Apple Metal / Metal Performance Shaders (MPS) for M-series; activated automatically when targeting macOS and no CUDA device is found; every Mac (Intel or Apple Silicon) has a Metal-capable GPU, so local macOS development always exercises real GPU code via this backend
- Runtime device enumeration on startup: prefer CUDA > Metal > wgpu-hardware > wgpu-software > MockGpuBackend

**Developing without a GPU:**
The design is intentionally hardware-agnostic at all stages:
1. `MockGpuBackend` runs on any CPU machine — all traits, dispatch logic, dirty-chunk upload/download flow, null handling, and pipeline integration can be written and tested without a GPU
2. `wgpu` with a software adapter (enabled by setting `WGPU_BACKEND=gl` or installing `lavapipe` on Linux) runs actual WGSL shaders on the CPU via the driver stack — useful for catching shader bugs before hardware is available
3. On macOS, Metal is always present (even on Intel Macs with Intel Iris / UHD graphics); Metal backend development requires no discrete GPU
4. CUDA-specific optimisations (`cudarc`, cuBLAS kernels) are isolated behind the `gpu-cuda` feature and can be developed last, on cloud GPU instances (GitHub Actions with GPU runners, or a spot `g4dn.xlarge`)
5. CI runs `MockGpuBackend` and wgpu-software tests on standard runners; CUDA tests are opt-in only

**Phase checklist:**
- [ ] Define `GpuBackend` trait — `upload(&[T]) -> GpuBuffer`, `download(GpuBuffer) -> Vec<T>`, `execute_kernel(GpuBuffer, KernelDescriptor) -> GpuBuffer`
- [ ] Implement `MockGpuBackend` — CPU-backed implementation of `GpuBackend`; identical interface, no GPU dependency; used for all unit tests, CI, and development without hardware
- [ ] Implement `wgpu` backend — arithmetic kernels (add, sub, mul, div) as WGSL compute shaders; 256-element workgroup size matching `AlignedChunk`; software adapter support for GPU-less CI
- [ ] Implement `cudarc` backend (NVIDIA) — wraps PTX kernels for the same arithmetic ops; cuBLAS `sgemm`/`dgemm` for matrix multiply nodes; gated behind `--features gpu-cuda`
- [ ] Implement Metal backend (Apple) — MPS-based arithmetic and matrix ops; activate when `cfg(target_os = "macos")` and CUDA absent
- [ ] Integrate dirty-chunk-aware upload: only chunks in `dirty_chunks()` are transferred; clean chunks stay in CPU `AlignedChunk`
- [ ] Define `ExecutionTarget` enum used by the scheduler to route each node:
  - `Cpu` — default; SIMD columnar or rayon task as per Phase 2.2 / Phase 3.1
  - `GpuLocal` — local GPU via `GpuBackend`; maps to Phase 3.5 dispatch
  - `GpuRemote { target: String }` — remote GPU worker over Arrow Flight; maps to Phase 6.5 dispatch
- [ ] `#[gpu]` / `#[gpu(local)]` annotation on `node!` macro — sets `ExecutionTarget::GpuLocal`; these two forms are identical; scheduler validates the local backend is present at pipeline init and falls back to `Cpu` if not
- [ ] `#[gpu(remote = "target_name")]` annotation on `node!` macro — sets `ExecutionTarget::GpuRemote { target }`; `target` must match a `RemoteGpuTarget` registered with the pipeline; compile-error if `target` string is absent
- [ ] Scheduler routing check at pipeline init: iterate all nodes, assert each `GpuLocal` node has a live local backend and each `GpuRemote` node has a registered `RemoteGpuTarget`; emit a single descriptive error naming every misconfigured node
- [ ] Auto-promotion heuristic: for nodes with a batch size ≥ configurable threshold (default 1M elements), promote to `GpuLocal` automatically when a local backend is available; never auto-promote to `GpuRemote`
- [ ] GPU buffer pool — reuse allocated GPU buffers across cycles to avoid repeated allocations on the critical path
- [ ] Null bitmap handling: upload Arrow validity bitmaps alongside data; GPU kernels respect null propagation semantics
- [ ] Benchmarks: GPU vs CPU-SIMD for arithmetic kernels at 256K, 1M, 10M rows on `f32` and `f64`; measure PCIe transfer cost separately from compute time
- [ ] Correctness tests: GPU kernel output matches CPU kernel output within `1e-6` tolerance for `f32`/`f64`; exact equality for integer types; all correctness tests run against `MockGpuBackend` so they pass in CI without hardware

---

## Phase 4 — Streaming and Live Updates

**Goal:** Push deltas into source nodes and have results propagate automatically to sinks.  
**Success metric:** Sub-millisecond latency for a 50-instrument market data tick through a 10-node pricing DAG.

### 4.1 Delta Model

- [ ] Define `Delta` enum: `Append { count }`, `Mutate { range }`, `Replace`, `Watermark { up_to }`
- [ ] Implement `graph.push(node, delta)` — marks dirty, does not compute
- [ ] Implement `graph.compute(node)` — recomputes only dirty chunks on critical path
- [ ] Batch multiple source updates before triggering compute
- [ ] Implement coalescing window — accumulate deltas for a configurable duration (default 1ms) before flushing; prevents per-tick compute cycles on high-frequency sources (market data, IoT); configurable to 0 for immediate dispatch when latency > throughput

> **Transport design note (relevant when deltas arrive over the network in Phase 4.5 and 6.x):**  
> Separate two planes to avoid conflating small control messages with bulk data:  
> - *Notification/control plane* — tiny fixed-size messages ("source X has delta at epoch 42", backpressure signals, heartbeats); raw gRPC with a small protobuf message is the right fit here — low overhead, typed, works well for sub-1KB payloads  
> - *Data plane* — Arrow IPC column batches; use Arrow Flight's `do_put` / `do_get` as a **persistent streaming RPC**, not a one-call-per-batch pattern — opening a new HTTP/2 stream per delta destroys throughput for small batches; a long-lived stream amortises connection overhead across many updates  
> Both planes sit on HTTP/2 (gRPC/Flight); see Phase 6.1 for the QUIC upgrade path when TCP head-of-line blocking becomes a latency concern.

### 4.2 Sink Nodes

- [ ] Implement `sink<Table>` — materialised queryable view, updated incrementally
- [ ] Implement `on_change` callback — fires when sink has new clean chunks
- [ ] Implement `serve(port)` — expose sink as HTTP endpoint

### 4.3 Backpressure

- [ ] Define `PushResult`: `Accepted`, `Backpressure { queue_depth }`, `Dropped`
- [ ] Implement bounded delta queue per source node
- [ ] Implement backpressure policy config: `DropOldest`, `DropNewest`, `Block`

### 4.4 Watermarks and Windows

- [ ] Implement `Watermark` delta — signals all events before timestamp T have arrived
- [ ] Implement `tumbling_window(duration, agg)` col op — closes windows on watermark
- [ ] Implement `sliding_window(size, step, agg)` col op
- [ ] Track window state between updates
- [ ] Test: 5-minute tumbling window sum over streaming price data

### 4.5 Source Connectors

- [ ] `source<T>` — static load once, manual update
- [ ] `stream<T>` — Kafka/WebSocket delta push
- [ ] `poll<T>` — database polling at interval
- [ ] Arrow IPC and Parquet file sources

### 4.6 Remote Pushdown

**Goal:** Offload computation to an external data system (Postgres, BigQuery, DuckDB, etc.) so that intermediate columns never leave the remote machine.  Only declared output columns are materialised locally.

**Syntax:**
```rust
let target = BigQuery::new(conn_string);  // or RemoteTarget::Local for tests

let p = pipeline! {
    remote!(target) {
        source!(price: f64,  table: "trading.prices");
        source!(vol:   f64,  table: "trading.prices");
        node!(tax   = price.row(|x| x * 0.2));
        node!(total = price.row(|x| x + tax));
        node!(vwap  = price.row(|x| x * vol));   // intermediate — stays remote
        output!(total)                             // only `total` crosses the wire
    }
    node!(report = total.row(|x| x * 1.1));       // runs locally
};
```

- [ ] Define `RemoteTarget` trait — implemented per backend; erased at runtime via `Box<dyn RemoteTarget>`
  - Initial backends: `DuckDb`, `Postgres`; `BigQuery` deferred (requires gRPC)
  - `RemoteTarget::Local` — executes locally, no network; useful for tests without changing pipeline code
- [ ] Macro-level closure-to-query translation (proc-macro AST pattern matching):
  - Translatable subset: arithmetic (`+ - * /`), comparison (`< > == !=`), `if/else` → `CASE WHEN`, casts
  - Each `node!` inside `remote!` lowers to a named CTE: `WITH tax AS (SELECT price * 0.2 AS tax FROM ...)`
  - Consecutive remote nodes on the same target batch into a single multi-CTE query — no per-node round trips
  - Unrecognised closure patterns are a **compile error** with the unsupported construct named explicitly
- [ ] `output!(col, ...)` inside `remote!` — declares which columns are materialised locally
  - Default (no `output!`): terminal nodes only (those with no downstream deps inside the block)
  - Non-output columns exist only as CTEs in the generated query; they never touch the local machine
- [ ] Type mapping table: `f32→REAL`, `f64→DOUBLE PRECISION`, `i32→INTEGER`, `i64→BIGINT`, `u32→BIGINT`, `u64→NUMERIC`
- [ ] Schema declaration at compile time via `source!(name: T, table: "schema.table")` — no runtime introspection
- [ ] `pipeline!` executor detects `remote!` blocks and submits the batched query via `RemoteTarget`; result `Frame` is fed as a `Frame` node into the parent DAG
- [ ] Tests:
  - DuckDB round-trip: remote two-node chain produces same result as local equivalent
  - Multi-CTE batching: two consecutive remote nodes emit exactly one query
  - `output!` materialisation: only declared columns arrive locally
  - `RemoteTarget::Local` used in tests — same pipeline code runs without a database

---

## Phase 5 — Partition-Aware Local Execution

**Goal:** Scale beyond RAM on a single machine using partitioned execution and object store.  
**Success metric:** Process a 100GB dataset on a 16GB machine without OOM.

### 5.1 Partition Model

- [ ] Define `Partition` — a subset of a node's chunks, assigned to a location
- [ ] Define `PartitionLocation`: `Memory`, `LocalDisk`, `Remote { uri }`
- [ ] Implement LRU partition cache — evict cold partitions to disk
- [ ] Implement partition stats: `min`, `max`, `null_count`, `bloom_filter`

### 5.2 Partition Pruning

- [ ] At query time, evaluate filter predicates against partition stats
- [ ] Skip partitions where stats prove no matching rows
- [ ] Log pruning decisions for query explain output

### 5.3 Object Store Integration

- [ ] Implement Parquet partition file reader/writer
- [ ] Implement S3/GCS/R2 partition storage backend
- [ ] Implement Delta Lake / Iceberg partition file format support
- [ ] Implement epoch-based dirty detection from file modification timestamps

---

## Phase 6 — Distributed Execution

**Goal:** Spread a DAG across multiple machines with Arrow Flight transport.  
**Success metric:** Linear throughput scaling to 10 machines on a partitioned dataset.

### 6.1 Arrow Flight Transport

> **Protocol design note:**  
> Arrow Flight runs on gRPC (HTTP/2). Two important constraints:
> 1. **Use persistent streaming RPCs** — `do_put` and `do_get` are bidirectional streaming calls; open one long-lived stream per source-worker pair and push many `FlightData` messages over it. Never open a new gRPC call per dirty-chunk batch — the HTTP/2 connection setup cost will dominate for small payloads.
> 2. **TCP head-of-line blocking** — HTTP/2 multiplexes logical streams over a single TCP connection. A single lost packet stalls every stream on that connection until retransmitted. For the latency targets in Phase 4 (sub-millisecond ticks), this is acceptable on a LAN. For WAN deployments or the financial real-time use case, **QUIC** (`quinn` crate, RFC 9000) eliminates head-of-line blocking at the transport layer. Arrow Flight over QUIC is an active area in the upstream Arrow ecosystem; plan to adopt it when it stabilises.  
> Epoch gossip (Phase 6.2) uses the gRPC notification plane (small protobuf messages); actual partition data uses Arrow Flight streaming — keep these on separate gRPC services so gossip latency is not affected by large in-flight data transfers.

- [ ] Implement Arrow Flight server per node — serves partition chunks on request; use persistent bidirectional `do_put` streams, not per-batch unary calls
- [ ] Implement partition fetch client — pulls dirty chunks from remote nodes
- [ ] Implement compute-to-data routing — ship compiled kernel descriptor to remote machine
- [ ] Separate gRPC service for notification/control plane (epoch gossip, heartbeats, backpressure signals) — keeps small control messages from being queued behind large in-flight `FlightData` transfers
- [ ] QUIC upgrade path — design the transport abstraction so the underlying connection can be swapped from TCP (HTTP/2) to QUIC without changing the Arrow Flight API surface; implement when `quinn`-based Arrow Flight support stabilises upstream

### 6.2 Distributed Dirty Coordination

- [ ] Implement epoch model — global monotonic counter, each partition tracks computed epoch
- [ ] Implement epoch gossip — lightweight broadcast when source nodes update
- [ ] Partition is dirty if its dependency epoch > its computed epoch

### 6.3 Shuffle Planning

- [ ] Detect shuffle-requiring operations: sort, join, global group-by
- [ ] Implement shuffle cache — keyed by `(left_epoch, right_epoch)`, reuse if unchanged
- [ ] Implement consistent hash ring for task placement — same instrument always same machine
- [ ] Implement broadcast join for small tables

### 6.4 Distributed Scheduler

- [ ] Implement coordinator — owns partition placement map, routes compute requests
- [ ] Implement worker — runs local VertexRS engine on assigned partitions
- [ ] Implement fault tolerance — detect worker failure, reassign partitions
- [ ] Implement partition rebalancing on worker join/leave

### 6.5 Remote GPU Kernel Execution

**Goal:** Submit compute-intensive node kernels to remote GPU workers — physical machines or managed cloud GPU instances — for workloads where local CPU (or even local GPU) is insufficient.  Primary use cases: heavyweight ML feature engineering (matrix factorisation, embedding lookup, gradient computation), large-scale numerical simulation, and real-time model inference in the pipeline DAG.

**Relationship to Phase 3.5:** Phase 3.5 is local GPU dispatch (same machine, GPU-accelerated chunk execution).  Phase 6.5 is *remote* GPU dispatch: the dirty batch is serialised as Arrow IPC, shipped over the network to a GPU worker, computed there, and the result returned as an Arrow batch.  The two phases share the same `GpuBackend` abstraction but differ in where the physical device lives.

**Execution model:**
- `#[remote_gpu]` annotation on a node routes its kernel to a GPU worker pool rather than local execution
- The dirty-chunk batch is serialised as Arrow IPC (zero-copy from `AlignedChunk` memory) and sent to a GPU worker via Arrow Flight
- The GPU worker runs the kernel (CUDA/ROCm/Metal as available on that machine) and returns the result batch over the same Flight channel
- Kernel descriptor is sent ahead of the data on first use and cached on the worker; subsequent calls send data only
- Compatible with the Arrow Flight transport from Phase 6.1 — GPU workers are a specialised worker type in the distributed pool

**Managed GPU backends:**
- **Self-managed GPU cluster** — physical NVIDIA/AMD machines registered as GPU workers in the coordinator; CUDA or ROCm runtime on the worker
- **AWS SageMaker / EC2 GPU** (p3/p4d/p5 instances) — worker image deployed via the Phase 10.3 operator; spot instance support for cost control
- **GCP Vertex AI / A2/A3 instances** — same worker image; managed via the GKE GPU node pool
- **Azure ML / NC/NV-series** — AKS GPU node pool with NVIDIA device plugin
- **Modal** — serverless GPU; useful for burst inference workloads where a permanent GPU worker is too costly; Arrow Flight stub invokes Modal function
- **ONNX Runtime** — for ML inference nodes, the kernel descriptor includes an ONNX model; the GPU worker loads the model once and runs inference per cycle

**Phase checklist:**
- [ ] Define `RemoteGpuTarget` — connection descriptor for a GPU worker (Arrow Flight endpoint, device type, available VRAM); registered with the pipeline by name so the `ExecutionTarget::GpuRemote { target }` tag can resolve it at init time
- [ ] `#[gpu(remote = "target_name")]` annotation on `node!` — sets `ExecutionTarget::GpuRemote`; consistent with the `#[gpu(local)]` form from Phase 3.5 so there is a single unified `#[gpu(...)]` surface across both phases
- [ ] Arrow IPC serialisation of dirty chunks including null bitmaps — reuse Phase 6.1 Flight transport; compression is handled natively by Arrow IPC (no extra library needed):
  - Arrow IPC supports per-buffer compression via `CompressionType::Lz4Frame` (default for remote GPU paths — near-memcpy decompression speed, low hot-path overhead) and `CompressionType::Zstd` (opt-in per `RemoteGpuTarget` for bandwidth-constrained or metered-egress connections)
  - Compression is opt-in; it is off by default in Arrow IPC and must be explicitly enabled on the `IpcWriteOptions` — do not assume it is active
  - Float columns (`f32`/`f64`) from financial/sensor streams are semi-random and compress modestly (10–30%); integer and timestamp columns compress much better; null bitmaps are sparse and compress well regardless
  - Arrow does **not** provide specialised time-series numeric codecs (e.g. Gorilla float compression, Turbo-PFOR integer compression); if per-column compression ratios are insufficient for a specific workload these can be layered as a pre-serialisation transform, but this is not planned for this phase
- [ ] Kernel descriptor protocol — compact representation of the compute kernel sent to the worker on first use; versioned so workers can cache compiled kernels
- [ ] GPU worker binary — runs `GpuBackend` (Phase 3.5) on received batches; returns Arrow IPC result; supports CUDA, ROCm, and Metal backends
- [ ] ONNX Runtime integration — `onnx_inference(model_path, input_cols)` node type; model loaded once per worker lifecycle; batch inference per dirty cycle
- [ ] Result stitching — remote results are written back into `AlignedChunk` slots for downstream local nodes; dirty tracking resumes normally after stitch
- [ ] Fault tolerance — GPU worker failure causes the node to fall back to local CPU execution for the current cycle; worker re-registers asynchronously
- [ ] Cost observability — per-node remote GPU time and data-transfer bytes exposed via Phase 7.2 profiling
- [ ] Tests: ONNX round-trip correctness; Arrow IPC serialise → ship → deserialise → execute → stitch produces same output as local CPU kernel

---

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

---

## Phase 8 — WebAssembly GUI (VertexRS Studio)

**Goal:** A browser-based live dashboard that connects to a running VertexRS engine, visualises the pipeline DAG, displays column distributions and statistics, and allows interactive data exploration — all with live incremental updates as data flows through the graph.

**Rationale:** The core engine is open source; this GUI layer is the productised interface. It runs entirely in the browser via WASM + a lightweight WebSocket server on the engine side, so there is no server infrastructure requirement beyond the process running the pipeline.

### 8.1 Engine-Side WebSocket Server

- [ ] Add `vertexrs-server` crate — thin `tokio` + `axum` WebSocket server
- [ ] Define a wire protocol (JSON or Arrow IPC over WebSocket) for:
  - DAG topology snapshot (nodes, edges, types, execution strategy)
  - Per-cycle delta: which nodes recomputed, dirty chunk counts, timing
  - Column stats snapshot: min/max/mean/null_count per node on demand
  - Distribution sample: reservoir sample of a column (for histogram rendering)
  - Interactive data query: row range or filter expression → row batch response
- [ ] Push incremental updates to connected clients after each compute cycle
- [ ] Gate behind `--features studio` so it has zero overhead when not in use

### 8.2 WASM Frontend (Leptos or Dioxus)

- [ ] Bootstrap `vertexrs-studio` crate compiled to `wasm32-unknown-unknown` via `trunk`
- [ ] DAG canvas — interactive node graph using `petgraph` layout; nodes coloured by execution strategy (columnar/task/fused); edges show data flow direction
- [ ] Live recompute indicator — nodes flash on each cycle when they recomputed; show dirty chunk count badge
- [ ] Column inspector panel — click any node to see:
  - Histogram of current column values (binned client-side from reservoir sample)
  - Min / max / mean / null count
  - Sparkline of a rolling metric (e.g. mean over last N cycles)
- [ ] Interactive data table — paginated view of raw row data for any node; supports sort and simple filter
- [ ] Cycle timeline — horizontal scrubber showing per-cycle recompute time and dirty chunk counts over time; click to time-travel (Phase 7.3 trace integration)
- [ ] Pipeline editor (stretch goal) — drag-and-drop node creation that emits `node!` / `pipeline!` macro source back to the engine

### 8.3 Distribution and Statistics

- [ ] Reservoir sampler in the engine — maintains a fixed-size sample per column, updated incrementally on dirty chunks only
- [ ] Server-sent histogram bins — engine bins the sample server-side and sends compact `(bin_edge, count)` pairs to reduce WASM compute
- [ ] Correlation matrix view — heatmap of pairwise Pearson correlations across all numeric columns in a `Frame`
- [ ] Outlier highlighting — rows > 3σ from mean flagged in the data table

### 8.4 Open Source / Productisation Split

- [ ] `vertexrs` (core engine) — MIT licensed, no GUI dependency
- [ ] `vertexrs-server` (WebSocket bridge) — MIT licensed, part of the open source repo
- [ ] `vertexrs-studio` (WASM GUI) — licence TBD; could be source-available or BSL if commercial value warrants it
- [ ] Studio connects via a documented open protocol so third-party UIs can be built

---

## Phase 9 — Open-Core Repository Split

**Goal:** Cleanly separate the open-source core from the commercial enterprise layer before any enterprise features are built, so the boundary is intentional and stable rather than retrofitted.

**Rationale:** Adding enterprise features to the same repo and then trying to extract them later always produces a messy fork. Do the split first, while the codebase is small and the API surface is clear.

### 9.1 Freeze the Public API Boundary

- [ ] Audit every public type and trait in `vertexrs-core`, `vertexrs-dag`, `vertexrs-exec`, `vertexrs-macro` — these form the stable API that enterprise plugins compile against
- [ ] Define a `vertexrs-plugin` trait crate: `ExecutorPlugin`, `SourceConnector`, `SinkConnector`, `AuthProvider` — extension points the enterprise layer hooks into without forking the core
- [ ] Add semver stability guarantees (`#[non_exhaustive]` on all public enums, `#[must_use]` audit)
- [ ] Document the plugin API in `vertexrs-plugin/README.md` with a worked example connector

### 9.2 Repo Topology

- [ ] **`vertexrs` (public, MIT)** — `vertexrs-core`, `vertexrs-dag`, `vertexrs-exec`, `vertexrs-macro`, `vertexrs-stream`, `vertexrs-dist`, `vertexrs-py`, `vertexrs-plugin`, `vertexrs-server`, `vertexrs-studio`
- [ ] **`vertexrs-enterprise` (private)** — imports `vertexrs` as a dependency; adds enterprise-only crates listed in Phase 10
- [ ] CI for the public repo must not depend on anything in the private repo
- [ ] Publish `vertexrs` and `vertexrs-plugin` to crates.io; enterprise crates are never published

### 9.3 Licensing

- [ ] `vertexrs` (core + macro + stream + dist + py + server) — **MIT**
- [ ] `vertexrs-studio` (WASM GUI) — **BSL 1.1**, converting to Apache 2.0 after 4 years; standard open-core protection while the product matures
- [ ] `vertexrs-enterprise` — **proprietary**, commercial license required
- [ ] Add `LICENSE`, `LICENSE-BSL`, and `COMMERCIAL.md` files explaining the split clearly
- [ ] Contributor License Agreement (CLA) required for contributions to Studio and enterprise crates

> **Enterprise and hosted cloud roadmap (Phases 10–11):** See `vertexrs-internal/.copilot/strategy/plan.md`.

---

## Crate Structure

```
vertexrs/
  vertexrs-core/        # ChunkedColumn, AlignedChunk, dirty bitmaps, Arrow interop
  vertexrs-dag/         # NodeDescriptor, Graph, AccessPattern, dirty propagation
  vertexrs-exec/        # Executor, scheduler, kernel fusion, SIMD kernels
  vertexrs-macro/       # node! macro, type inference, kernel codegen
  vertexrs-stream/      # Delta model, source/sink nodes, watermarks, windows
  vertexrs-dist/        # Arrow Flight transport, distributed scheduler, shuffle
  vertexrs-py/          # PyO3 Python bindings
  vertexrs-server/      # WebSocket server bridge (tokio + axum, --features studio)
  vertexrs-studio/      # WASM GUI (Leptos/Dioxus, compiled via trunk)
  vertexrs/             # Top-level crate, ties everything together
```

---

## Key Dependencies

| Crate | Purpose |
|---|---|
| `arrow-rs` | Columnar memory layout, validity bitmaps, Arrow IPC |
| `roaring` | Compressed dirty bitmaps |
| `rayon` | Work-stealing parallelism for task nodes |
| `tikv-jemallocator` | High-performance allocator |
| `multiversion` | Runtime CPU feature dispatch |
| `arrow-flight` | Distributed chunk transport |
| `object_store` | S3/GCS/R2 partition storage |
| `parquet` | Partition file format |
| `pyo3` | Python bindings |
| `proc-macro2` + `syn` + `quote` | Macro implementation |

---

## Milestone Summary

| Phase | Description | Deliverable |
|---|---|---|
| 1 | Core local engine | Chunked columns, dirty bitmaps, single-threaded executor |
| 2 | Macro system | `node!` macro, type-driven strategy, kernel fusion |
| 3 | Parallelism | Rayon tasks, SIMD kernels, group-by |
| 4 | Streaming | Delta push, sinks, watermarks, windows |
| 5 | Partition-aware | Object store, partition pruning, large data |
| 6 | Distributed | Arrow Flight, epoch coordination, shuffle |
| 7 | Tooling | Explain, profiling, tracing, Python bindings |
| 8 | WASM GUI | Live DAG visualiser, column distributions, interactive data explorer |
| 9 | Open-core split | Plugin API, repo separation, licensing |
| 10–11 | Enterprise + Cloud | See `vertexrs-internal/.copilot/strategy/plan.md` |

---

## What Makes VertexRS Different

| | Polars | DataFusion | Flink | **VertexRS** |
|---|---|---|---|---|
| Incremental updates | ❌ | ❌ | ✅ | ✅ |
| Vectorised SIMD | ✅ | ✅ | ❌ | ✅ |
| General DAG (not just SQL) | ❌ | ❌ | ✅ | ✅ |
| Macro-defined graph | ❌ | ❌ | ❌ | ✅ |
| Chunk-level dirty tracking | ❌ | ❌ | ❌ | ✅ |
| Task + columnar in one graph | ❌ | ❌ | ❌ | ✅ |
| Local → distributed same API | ❌ | partial | ❌ | ✅ |

