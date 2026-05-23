[← Phase Index](main.md)

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

