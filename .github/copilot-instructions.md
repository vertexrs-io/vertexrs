# VertexRS — Project Conventions

## Project Overview

VertexRS is a general-purpose DAG computation engine that redefines how users write data flows and process graphs using a declarative macro format. It targets high-speed execution both locally (multi-core, SIMD) and in the cloud (vendor-agnostic execution backends). Its key differentiator is **incremental recomputation via dirty-chunk tracking** — only the chunks of data that have changed are recomputed, giving superlinear speedups over full-recompute engines like Polars for partial updates.

Although the execution backend is columnar (Arrow-backed chunks), VertexRS is not limited to data analytics. The `pipeline!` macro builds a typed DAG whose nodes can represent **any computation** — data transformations, business-logic process steps, ML inference stages, or arbitrary task graphs. Heavy node types (structs, enums, non-primitive values) automatically route to the task/rayon executor rather than the SIMD path, so the same macro syntax covers both vectorised data pipelines and general process orchestration.

**Target domains — data pipelines:**
- **Data analytics** — exploratory and production pipelines over large datasets; a live, reactive alternative to dbt-style batch transformation graphs
- **Machine learning pipelines** — feature engineering and preprocessing graphs where features update as new training data arrives; only affected features recompute
- **Real-time financial analysis** — market data ticks, instrument pricing, Greeks calculations, P&L attribution; sub-millisecond latency on a 50-instrument DAG
- **Risk management** — portfolio VaR, margin calculations, real-time stress testing; incremental recompute avoids the cost of full-recompute batch risk systems
- **IoT and sensor data** — continuous streams from large device fleets where only a small fraction of sensors change per cycle; dirty-chunk tracking eliminates redundant work
- **ETL and data transformation** — structured replacement for batch transformation graphs; pipelines that stay live and react to upstream changes rather than running on a schedule
- **Scientific simulation** — agent-based models, climate models, financial simulations where parameters change incrementally and full recompute at each step is prohibitively expensive

**Target domains — process / task graphs:**
- **Business-logic orchestration** — multi-step approval flows, pricing engines, and rule graphs where downstream steps only re-run when their direct inputs change
- **Build and CI pipelines** — incremental task graphs where unchanged inputs skip recomputation, analogous to Make/Bazel but expressed in pure Rust macros
- **Event-driven workflows** — reactive process graphs triggered by partial state changes; only the affected subgraph re-executes
- **Agent and simulation loops** — tick-driven graphs where each agent's state node depends on neighbours; only dirty agents recompute each cycle

Core design principles (see `docs/plans/` for full detail):
- Nodes reference each other directly — the `pipeline!` macro builds the DAG, no manual edge declaration
- Types drive execution strategy — scalar primitives → columnar/SIMD, heavy types → task/rayon
- Arrow as the memory substrate — interop, validity bitmaps, 64-byte aligned buffers
- Dirty chunks, not dirty nodes — incremental recomputation at chunk granularity
- Kernel fusion — pointwise chains fuse into single-pass kernels at compile time
- Soft/hard/isolated failure propagation via Arrow validity bitmaps

---

## Crate Structure

```
vertexrs/           # main library — engine, types, pipeline runtime, tests
vertexrs-macro/     # proc-macro crate — node!, pipeline!, sub! macros
```

All dependency versions and features live in the **workspace `Cargo.toml`** at the root. Crate-level `Cargo.toml` files reference workspace deps with `{ workspace = true }` — never pin a version in a crate-level file.

Key abstractions:
- `AlignedChunk<T>` — fixed 256-element, 64-byte aligned block backed by `ScalarBuffer<T>`
- `ChunkedColumn<T>` — `Vec<AlignedChunk<T>>` with a `RoaringBitmap` dirty index
- `Frame` — collection of type-erased `AnyNode` columns
- `Node<T>` / `AnyNode` — typed and erased column handles
- `Pipeline` / `PipelineImpl` — runtime pipeline wrapper and generated impl trait
- `pipeline! { }` — declarative macro that builds a typed DAG struct
- `node! { }` — macro to declare a compute kernel
- `sub! { }` — macro to compose an external pipeline as a sub-pipeline

---

## Build Plan

The build plan is split by phase under `docs/plans/`. `docs/plans/main.md` is the index (phase list and status); each phase has its own file (`docs/plans/phase-01-core-engine.md`, etc.). The enterprise/commercial roadmap lives in `vertexrs-internal/.copilot/strategy/plan.md` (private repo). Before starting any non-trivial work, check the index and the relevant phase file to confirm the next step aligns with the current phase.

Any agent or developer implementing a feature must:
1. Check the plan for the relevant phase
2. Update the plan checkboxes as tasks complete
3. Never skip ahead of a phase without explicitly noting it as a deliberate deviation

---

## Development Workflow

- All work happens on **feature branches**; no direct commits to `main`
- Every branch requires a **PR with at least one review** before merging
- Ambiguities in requirements must be resolved before starting implementation — ask clarifying questions
- All tests and benchmarks must pass before a PR is merged
- CI runs on every PR via **GitHub Actions**; task orchestration uses **cargo make**

Common commands:
```bash
cargo check                          # fast type-check
cargo test                           # all unit + integration tests
cargo test -- --include-ignored      # include slow/ignored tests
cargo bench                          # run criterion benchmarks
cargo bench --save-baseline main     # save a named baseline
cargo clippy -- -D warnings          # fail on any lint warning
cargo fmt --check                    # verify formatting (CI); use `cargo fmt` locally
```

---

## Testing Policy

Every PR must include appropriate test coverage for the changed code:

| Test type | When required |
|---|---|
| **Unit tests** | All non-trivial functions; place in a `#[cfg(test)]` module in the same file |
| **Integration tests** | New pipeline behaviours, macro-generated code paths; place in `vertexrs/src/lib.rs` test module |
| **Doctests** | All public API items with an example worth showing |
| **Benchmarks** | Every critical code path and any code on the hot recompute path; use `criterion` |

Rules:
- **Line coverage must be ≥ 90%** across the `vertexrs` and `vertexrs-macro` crates. Measure with `cargo llvm-cov --all-features --lib` (requires `cargo-llvm-cov`). PRs that drop coverage below 90% must include a written justification.
- Tests must be deterministic and not depend on timing
- Use exact expected values; avoid `unwrap()` in test assertions — use `assert_eq!` / `assert!(matches!(...))` clearly
- Benchmarks that have a Polars equivalent **must** include a `#[test]` correctness assertion comparing outputs within tolerance (`abs(vtx − polars) < 1e-6` for f32/f64; exact equality for integer types; f16 widened to f32 before comparison)

---

## Benchmarking Policy

Benchmarks live in `vertexrs/benches/` and are written using `criterion`. See `docs/plans/phase-02-macro-system.md` for the full benchmark plan.

Key rules:
- Benchmark new code paths on the recompute hot path before merging
- Every benchmark file with a Polars counterpart must assert correctness — a faster but wrong result is not acceptable
- Save a baseline on every merge to `main`: `cargo bench --save-baseline main`
- Regressions > 15% on throughput benchmarks must be explained and justified in the PR
- Cross-dtype benchmarks are parametrized over `f32`, `f64`, `i32`, `i64`, `u32`, `u64`, `f16` — no single type should be > 2× slower than the fastest for the same logical pipeline

---

## Error Handling

Use the right mechanism for the situation:

| Situation | Mechanism |
|---|---|
| Programmer invariant violated (should never happen) | `panic!` / `assert!` / `unreachable!` |
| Recoverable operation that can fail | `Result<T, E>` with a descriptive error type |
| Domain-specific pipeline errors | Custom error type (e.g. `PipelineError`) |
| Missing optional value | `Option<T>` — do not use `Result` for absence |
| Node kernel panic in Isolate mode | Caught and stored in `Pipeline::isolated_errors` |

Never silence errors with `let _ = ...` unless the intent is explicitly documented with a comment.  
Never `unwrap()` in library code — only in tests and examples.

---

## Unsafe Code

- Unsafe is permitted **only in performance-critical hot paths** where safe alternatives have measurably worse throughput
- Every `unsafe` block must be preceded by a `// SAFETY:` comment explaining why it is sound
- Unsafe must not cross public API boundaries — wrap in a safe public function
- Prefer `bytemuck` for transmute-like operations over raw pointer casts

---

## Dependencies

- **Minimise dependencies** — every new crate addition must be justified with a comment in `Cargo.toml`
- **Prefer the Arrow ecosystem** (`arrow-buffer`, `arrow-array`, `arrow-schema`) and `std` over general-purpose alternatives
- `proc-macro2`, `quote`, `syn` are acceptable in `vertexrs-macro` only
- Dev-dependencies (criterion, polars, dhat) are acceptable and do not need the same justification as runtime deps
- Feature-gate heavy dev-deps where possible (e.g. `bench-polars` feature flag for Polars benchmarks)
- Never add a runtime dependency for something achievable with a 20-line safe Rust function

---

## Code Style

- Follow the [Rust Style Guide](https://doc.rust-lang.org/style-guide/); enforced by `rustfmt`
- Run `cargo fmt` before every commit; CI runs `cargo fmt --check`
- Run `cargo clippy -- -D warnings`; CI fails on any lint warning
- All public functions, types, traits, and modules must have `///` doc comments
- Non-obvious logic must have inline `//` comments explaining *why*, not *what*
- Keep functions short and single-purpose; prefer pure functions over side-effecting ones
- Use `#[must_use]` on any `Result` or value-returning function where ignoring the return is likely a bug

---

## CI (GitHub Actions + cargo make)

CI runs on every PR and every merge to `main`:

```
check    → cargo check
fmt      → cargo fmt --check
lint     → cargo clippy -- -D warnings
test     → cargo test
audit    → cargo audit  (blocks on RUSTSEC advisory ≥ Medium)
bench    → cargo bench (main merges only: compare new vs previous main baseline; fail on > 15% regression)
```

`cargo make` is used for local task orchestration. The `Makefile.toml` should mirror the CI steps so local and CI behaviour are identical.

---

## Instruction & Agent Files

- Instruction files (`.instructions.md`), agent files (`.agent.md`), and skill files (`SKILL.md`) should each be **≤ 2 000 tokens**
- If a file grows beyond that, split it: extract the overflow into a focused sub-file and link it from the parent file
- Keep each file tightly scoped to one topic — this makes token budgets easier to respect and makes individual files easier to reason about

### File locations

| Path | Scope |
|---|---|
| `.github/instructions/lang/rust.instructions.md` | All `.rs` files |
| `.github/instructions/modules/vertexrs.instructions.md` | `vertexrs/src/**/*.rs` |
| `.github/instructions/modules/vertexrs-macro.instructions.md` | `vertexrs-macro/src/**/*.rs` |
| `.github/instructions/process/planning.instructions.md` | Creating GitHub issues |
| `.github/instructions/process/testing.instructions.md` | Writing and reviewing tests |
| `.github/instructions/process/benchmarking.instructions.md` | Writing and reviewing benchmarks |
| `.github/instructions/process/security.instructions.md` | Security-sensitive code |
| `.github/instructions/process/pr-review.instructions.md` | Reviewing PRs |
| `.github/agents/planner.agent.md` | Planner agent mode |
| `.github/agents/scrum-master.agent.md` | ScrumMaster agent mode |
| `.github/agents/architect.agent.md` | Architect agent mode |
| `.github/agents/implementer.agent.md` | Implementer agent mode |
| `.github/agents/reviewer.agent.md` | Reviewer agent mode |

## Architectural Decision Records

Core design decisions are recorded in `docs/adr/`. **Always read the relevant ADR(s) before implementing any feature** — they record *why* things are the way they are and constrain the acceptable solution space.

| ADR | Decision |
|---|---|
| [0001](../docs/adr/0001-arrow-memory-substrate.md) | Apache Arrow as the memory substrate |
| [0002](../docs/adr/0002-dirty-chunk-incremental-recompute.md) | Dirty-chunk incremental recomputation |
| [0003](../docs/adr/0003-kernel-fusion.md) | Compile-time kernel fusion |
| [0004](../docs/adr/0004-type-driven-execution-strategy.md) | Type-driven execution strategy selection |
| [0005](../docs/adr/0005-macro-defined-dag.md) | Macro-defined DAG with direct node references |

New ADRs go in `docs/adr/` using the template at `docs/adr/template.md`. Record a new ADR for any decision that is non-obvious, constrains future implementation, or was reached after considering alternatives.

**ADRs are immutable.** Once accepted, never edit the body of an existing ADR. If a decision changes, create a new superseding ADR, set the old ADR's status to `Superseded by ADR-XXXX`, and link forward to the new record.

## docs/ structure

All project content (documentation, plans, design artefacts) lives in `docs/`. `.github/` contains only GitHub platform config and Copilot tooling.

| Path | Purpose |
|---|---|
| `docs/plans/main.md` | Phase index — phase list, status, and links to phase files. Owned by the Planner agent. |
| `docs/plans/phase-XX-*.md` | Per-phase detail files — checkboxes, tasks, issue annotations. Owned by the Planner agent. |
| `docs/adr/` | Permanent architectural decision records. Read before implementing anything non-trivial. |
| `docs/design/` | Issue-scoped design documents produced by the Architect agent. Obsolete once the issue is implemented, kept for traceability. |


