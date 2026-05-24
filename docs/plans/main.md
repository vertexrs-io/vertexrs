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

## Phase Index

| Phase | Title | Status | File |
|---|---|---|---|
| 1 | Core Local Engine | ✅ Complete | [phase-01-core-engine.md](phase-01-core-engine.md) |
| 2 | The Macro System | 🔄 In Progress | [phase-02-macro-system.md](phase-02-macro-system.md) |
| 2.12 | Process and Task Graph Execution | ⬜ Pending | [phase-02-macro-system.md](phase-02-macro-system.md) |
| 3 | Parallelism | ⬜ Pending | [phase-03-parallelism.md](phase-03-parallelism.md) |
| 4 | Streaming and Live Updates | ⬜ Pending | [phase-04-streaming.md](phase-04-streaming.md) |
| 5 | Partition-Aware Local Execution | ⬜ Pending | [phase-05-partition-aware.md](phase-05-partition-aware.md) |
| 6 | Distributed Execution | ⬜ Pending | [phase-06-distributed.md](phase-06-distributed.md) |
| 7 | Observability and Tooling | ⬜ Pending | [phase-07-observability.md](phase-07-observability.md) |
| 8 | WebAssembly GUI (VertexRS Studio) | ⬜ Pending | [phase-08-wasm-studio.md](phase-08-wasm-studio.md) |
| 9 | Open-Core Repository Split | ⬜ Pending | [phase-09-open-core-split.md](phase-09-open-core-split.md) |

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

