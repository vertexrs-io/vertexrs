[← Phase Index](main.md)

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

