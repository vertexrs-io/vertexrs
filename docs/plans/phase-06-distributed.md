[← Phase Index](main.md)

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

