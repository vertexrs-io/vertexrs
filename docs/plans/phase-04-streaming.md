[← Phase Index](main.md)

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

