# Design: Kernel Fusion Pass + Initial Benchmark Baseline (Issue #31)

**Phase:** 2.7  
**Branch:** `feat/31-kernel-fusion-pass`  
**ADR constraints:** ADR-0003 (compile-time kernel fusion), ADR-0004 (type-driven execution — fusion applies to the columnar/SIMD row-mode path only)

---

## Summary

Implement compile-time kernel fusion in the `pipeline!` macro. When the macro detects a linear chain of pure, single-arg, row-mode nodes where each consumer's receiver matches the prior node's name, it emits a single fused loop that computes the entire chain in one pass — no intermediate `Vec` allocations, no per-node loop overhead. The runtime (`Pipeline`, `PipelineImpl`, `Executor`) is untouched. A new `bench_fusion_vs_unfused` benchmark group validates the improvement. A `cargo make bench-smoke` task catches broken benches on PRs.

---

## Phase and ADR constraints

- **ADR-0003** mandates compile-time fusion via the `pipeline!` macro. Runtime fusion is explicitly rejected.
- **ADR-0004** scopes fusion to `T: ArrowNativeType` (columnar/SIMD path). Non-primitive task nodes are never fused.
- The existing `pure = false` flag (implemented in Phase 2.6) already marks nodes as ineligible for incremental skip; we reuse it here as the fusability gating flag.

---

## Reuse audit

| Symbol | Location | Role in this design |
|---|---|---|
| `NodeItemMeta` | `vertexrs-macro/src/lib.rs:440` | The metadata struct for each `node!` item; `pure`, `failure_override`, `receiver_ident`, `name`, `core_expr_tokens` are all read by the fusion helpers |
| `NodeItemMeta::receiver_ident` | `vertexrs-macro/src/lib.rs:446` | `Option<Ident>` for the node's receiver; used to check `consumer.receiver_ident == Some(producer.name)` |
| `NodeItemMeta::pure` | `vertexrs-macro/src/lib.rs:453` | `bool`; `false` breaks a fusion chain |
| `NodeItemMeta::failure_override` | `vertexrs-macro/src/lib.rs:451` | `NodeFailureOverride`; `Soft` or `Hard` breaks a fusion chain |
| `NodeItemMeta::core_expr_tokens` | `vertexrs-macro/src/lib.rs:449` | Token stream passed to `vertexrs::node!(...)` today; re-parsed inside fused emit to extract the closure body |
| `extract_node_call` | `vertexrs-macro/src/lib.rs:43` | Returns `NodeCall { receiver, mode, closure }`; used in `is_row_fusable` to confirm row mode and extract the closure |
| `receiver_ident` | `vertexrs-macro/src/lib.rs:67` | Extracts a bare `Ident` from a receiver `Expr`; used in the chain-link check |
| `OrderedItem` | `vertexrs-macro/src/lib.rs:539` | Enum `Node | Nested | Sub`; only `Node` items participate in fusion |
| `NodeFailureOverride` | `vertexrs-macro/src/lib.rs:430` | Enum `None | Soft | Hard`; only `None` is fusable |
| Codegen loop | `vertexrs-macro/src/lib.rs:758` | `for item in &ordered { ... }` — replaced with the fusion-aware pass |

No new abstractions are introduced in the runtime crate. The four new helpers and one new enum are all in `vertexrs-macro/src/lib.rs` only.

---

## Module and file changes

| File | Change |
|---|---|
| `vertexrs-macro/src/lib.rs` | Add `FusionGroup` enum; add `is_row_fusable`, `chains_from`, `group_fusable`, `emit_fused_chain` helpers; replace the `for item in &ordered` loop body with a fusion-aware pass |
| `vertexrs/benches/pipeline.rs` | Add `bench_fusion_vs_unfused` benchmark group and `#[cfg(test)]` correctness block |
| `Makefile.toml` | Add `[tasks.bench-smoke]` |
| `.github/workflows/ci.yml` | Add a `bench-smoke` step after `cargo make ci` |
| `docs/plans/phase-02-macro-system.md` | Tick 2.7 checkboxes as items complete (Implementer does this) |

No files are deleted or moved.

---

## Type and trait definitions

All new items live in `vertexrs-macro/src/lib.rs`. No public API changes.

```rust
/// Classification of a run of `OrderedItem::Node` elements after the fusion pass.
enum FusionGroup<'a> {
    /// A single node that cannot be fused (non-pure, has failure sigil, col-mode,
    /// multi-arg typed closure, fan-out target, or chain length == 1 after
    /// grouping).  Emitted exactly as today.
    Single(&'a NodeItemMeta),
    /// Two or more consecutive pure row-mode nodes forming a linear chain.
    /// The vec is in declaration order; guaranteed len >= 2.
    Fused(Vec<&'a NodeItemMeta>),
}

/// Returns `true` iff `meta` is eligible to participate in a fusion chain.
///
/// Conditions (all must hold):
/// - `meta.pure == true`
/// - `meta.failure_override` is `NodeFailureOverride::None`
/// - The `core_expr_tokens` parse back to a single-arg, untyped, row-mode call
///   (`receiver.row(|x| body)`) via `extract_node_call`
fn is_row_fusable(meta: &NodeItemMeta) -> bool { ... }

/// Returns the name of the receiver node for `consumer` if it forms a valid
/// linear chain link: `consumer.receiver_ident == Some(&producer.name)`.
///
/// Returns `true` iff the chain link is valid.
fn chains_from(producer: &NodeItemMeta, consumer: &NodeItemMeta) -> bool { ... }

/// Groups `ordered` items into `FusionGroup`s.
///
/// Rules:
/// - Non-`Node` items (`Nested`, `Sub`) always emit as-is (not wrapped in a group).
/// - A `Node` item that is not fusable emits as `FusionGroup::Single`.
/// - A run of fusable nodes where each consecutive pair satisfies `chains_from`
///   and where no node in the run has more than one consumer in `ordered` is
///   emitted as `FusionGroup::Fused` (len >= 2).  A run of length 1 falls back
///   to `FusionGroup::Single`.
/// - Fan-out detection: before building groups, scan `ordered` to count how many
///   times each node name appears as a `receiver_ident`; any node with a count
///   >= 2 is treated as non-fusable for chain-breaking purposes.
fn group_fusable<'a>(ordered: &'a [OrderedItem]) -> Vec<FusionGroup<'a>> { ... }

/// Emits a single fused kernel block for a `FusionGroup::Fused` chain.
///
/// The emitted code:
/// 1. Pre-sizes one `Vec::with_capacity(n)` per node in the chain.
/// 2. Runs a single `for __vtx_i in 0..receiver.len()` loop that computes
///    each node's expression in sequence, accumulating into its vec.
/// 3. Wraps each vec in `Node::new_with_deps(name, deps, data)` so that
///    downstream non-fused nodes referencing any intermediate name still compile.
fn emit_fused_chain(chain: &[&NodeItemMeta]) -> proc_macro2::TokenStream { ... }
```

---

## Call flow

At macro expansion time (build time, not runtime):

1. `pipeline!(...)` calls `parse_macro_input!` → `PipelineDef`.
2. Items are split into `sources`, `ordered`, `output_names` (unchanged from today).
3. **New:** `group_fusable(&ordered)` is called to produce `Vec<FusionGroup>`.
   a. For each `OrderedItem::Node(meta)`, call `is_row_fusable(meta)`.
   b. Build a fan-out count map: `HashMap<String, usize>` from `receiver_ident` → appearance count across all nodes.
   c. Walk `ordered` in declaration order. Accumulate a running chain `current_chain: Vec<&NodeItemMeta>`.
      - If the current node is fusable and `chains_from(prev_in_chain, current)` and fan-out count of `prev_in_chain.name` == 1: extend `current_chain`.
      - Otherwise: flush `current_chain` as a group (Single if len==1, Fused if len>=2), start a new chain.
   d. Flush remaining chain.
   e. Non-`Node` items (`Nested`, `Sub`) force a chain flush and are inserted between groups directly.
4. The existing `for item in &ordered` codegen loop is replaced by a `for group in groups` loop:
   - `FusionGroup::Single(meta)` → emit exactly the existing single-node codegen (unchanged).
   - `FusionGroup::Fused(chain)` → call `emit_fused_chain(&chain)`, push the result into `ord_run`.
5. The rest of the codegen (struct fields, `push_sources`, `output`, etc.) is unchanged.

---

## Fused block code shape

For a chain `[b, c, d]` where:
- `b = price.row(|x| x * 2.0)`
- `c = b.row(|x| x + 1.0)`
- `d = c.row(|x| x * x)`

The fused block emitted by `emit_fused_chain` looks like:

```rust
// Fused chain: b → c → d
let mut __vtx_b_data: Vec<_> = Vec::with_capacity(price.len());
let mut __vtx_c_data: Vec<_> = Vec::with_capacity(price.len());
let mut __vtx_d_data: Vec<_> = Vec::with_capacity(price.len());
for __vtx_i in 0..price.len() {
    let __vtx_b_val = { let x = price.data[__vtx_i]; x * 2.0 };
    let __vtx_c_val = { let x = __vtx_b_val; x + 1.0 };
    let __vtx_d_val = { let x = __vtx_c_val; x * x };
    __vtx_b_data.push(__vtx_b_val);
    __vtx_c_data.push(__vtx_c_val);
    __vtx_d_data.push(__vtx_d_val);
}
// Bind all intermediates so downstream non-fused nodes can reference them.
let b = vertexrs::Node::new_with_deps("b", &["price"], __vtx_b_data);
let c = vertexrs::Node::new_with_deps("c", &["b"],     __vtx_c_data);
let d = vertexrs::Node::new_with_deps("d", &["c"],     __vtx_d_data);
```

Key invariant: every intermediate node (`b`, `c`) is bound in the enclosing scope after the fused block, so any downstream node that references `b` or `c` directly (e.g. a non-fused fan-out consumer) will still find the binding.

The Implementer must re-parse the closure body from `meta.core_expr_tokens` (which holds `name = receiver.row(|arg| body)` tokens) to extract `arg` and `body` for each link. The `extract_node_call` helper already does the structural matching; the closure's single input ident and body expression are extracted from `NodeCall::closure`.

---

## Executor path

The runtime executor (`vertexrs/src/lib.rs: Pipeline`, `PipelineImpl`, `compute()`) is **not touched**. Fusion is purely a macro-expansion transformation. The fused block emits the same `Node::new_with_deps(...)` bindings as the single-node path, so the executor sees no structural difference.

ADR-0004 constraint satisfied: fusion only fires for nodes where `is_row_fusable` returns true, which requires a single-arg untyped row-mode call — the path reserved for `T: ArrowNativeType` columnar nodes.

---

## Benchmark additions (`vertexrs/benches/pipeline.rs`)

### New benchmark group

```rust
fn bench_fusion_vs_unfused(c: &mut Criterion) {
    let mut group = c.benchmark_group("fusion_vs_unfused");
    // N = 256: one AlignedChunk. Chosen to isolate per-element loop overhead
    // vs allocator noise from the 1M-row group.
    const FUSION_N: usize = 256;
    group.throughput(Throughput::Elements(FUSION_N as u64));

    group.bench_function("fused_5node_f64", |b| {
        let frame = make_frame_f64(FUSION_N);
        b.iter(|| {
            let mut p = pipeline! {
                source!(price: f64);
                node!(a = price.row(|x| x * 2.0_f64));
                node!(b = a.row(|x| x + 1.0_f64));
                node!(c = b.row(|x| x - 0.5_f64));
                node!(d = c.row(|x| x * x));
                node!(e = d.row(|x| x / 2.0_f64));
                output!(e)
            };
            p.push(&frame);
            p.compute().unwrap();
            p
        })
    });

    group.bench_function("unfused_5node_f64", |b| {
        let frame = make_frame_f64(FUSION_N);
        b.iter(|| {
            let mut p = pipeline! {
                source!(price: f64);
                node!(a = price.row(|x| x * 2.0_f64));
                // pure = false on the middle node breaks the chain into two
                // independent unfused segments.
                node!(b = a.row(|x| x + 1.0_f64), pure = false);
                node!(c = b.row(|x| x - 0.5_f64));
                node!(d = c.row(|x| x * x));
                node!(e = d.row(|x| x / 2.0_f64));
                output!(e)
            };
            p.push(&frame);
            p.compute().unwrap();
            p
        })
    });

    group.finish();
}
```

Register in the existing `criterion_group!` invocation (both `#[cfg]` branches).

### Correctness block (no `bench-polars` feature needed)

```rust
#[cfg(test)]
mod fusion_correctness {
    use super::*;

    #[test]
    fn fused_and_unfused_agree() {
        const M: usize = 100;
        let frame = make_frame_f64(M);

        // Fused pipeline (all pure, linear chain).
        let mut fused = pipeline! {
            source!(price: f64);
            node!(a = price.row(|x| x * 2.0_f64));
            node!(b = a.row(|x| x + 1.0_f64));
            node!(c = b.row(|x| x - 0.5_f64));
            node!(d = c.row(|x| x * x));
            node!(e = d.row(|x| x / 2.0_f64));
            output!(e)
        };
        fused.push(&frame);
        fused.compute().unwrap();

        // Unfused pipeline (chain broken by pure = false).
        let mut unfused = pipeline! {
            source!(price: f64);
            node!(a = price.row(|x| x * 2.0_f64));
            node!(b = a.row(|x| x + 1.0_f64), pure = false);
            node!(c = b.row(|x| x - 0.5_f64));
            node!(d = c.row(|x| x * x));
            node!(e = d.row(|x| x / 2.0_f64));
            output!(e)
        };
        unfused.push(&frame);
        unfused.compute().unwrap();

        let fused_e   = fused.output().get::<f64>("e").expect("fused e");
        let unfused_e = unfused.output().get::<f64>("e").expect("unfused e");

        assert_eq!(fused_e.len(), unfused_e.len());
        for (f, u) in fused_e.iter().zip(unfused_e.iter()) {
            assert!((f - u).abs() < 1e-9, "fused={f} unfused={u}");
        }
    }
}
```

This test is gated on `#[cfg(test)]` only — no `bench-polars` required — so it runs in `cargo test` without any extra feature flags.

---

## bench-smoke CI integration

### `Makefile.toml` addition

```toml
[tasks.bench-smoke]
description = "Quick benchmark compile+run check (--quick, ~10-20 s). Used in PR CI to catch broken benches."
command = "cargo"
args = ["bench", "--", "--quick"]
```

### `.github/workflows/ci.yml` addition

After the `cargo make ci` step, add:

```yaml
      # Verify benchmarks compile and produce output — fast (~15 s) with --quick.
      # The full benchmark comparison lives in bench.yml (main merges only).
      - run: cargo make bench-smoke
```

This runs on every PR and every push to main (matching the existing `ci.yml` triggers). It does not save or compare baselines — that remains the responsibility of `bench.yml`.

---

## Initial benchmark baseline

No manual `--save-baseline initial` step is needed. The `bench.yml` workflow runs on every merge to `main`. On the first post-merge run after this feature lands, the `bench-baseline-main-*` Actions cache key will be empty; the compare step will be skipped and the save step will write the first baseline entry. This first entry becomes the regression anchor for all subsequent PRs.

The Implementer should create `docs/plans/bench-baseline.md` post-merge, recording the human-readable throughput numbers from that first `bench.yml` run (pipeline group ns/iter and MB/s). This file travels with the codebase for traceability since the binary baseline lives only in the Actions cache.

---

## Edge cases the Implementer must cover with tests

1. **Split chain (soft node in the middle):** chain `[pure, pure, soft!, pure, pure]` → the macro must emit two separate `FusionGroup::Fused` blocks (before and after the soft node), with the soft node between them as a `FusionGroup::Single`. Test: assert the output of each segment matches a hand-computed reference.

2. **Fan-out (one producer, two consumers):** `b = a.row(...)` and `c = a.row(...)` — `a` has a fan-out count of 2, so neither the `a→b` nor the `a→c` link is fusable. Both `b` and `c` are emitted as `FusionGroup::Single`. Test: assert both consumers produce correct results independently.

3. **Intermediate node referenced after a fused block:** chain `[b, c, d]` fused, followed by a non-fused node `e = b.row(...)` that references `b` directly. The fused block must bind `b` in the enclosing scope so `e`'s codegen compiles. Test: pipeline compiles and produces correct results for all four nodes.

These three cases must have dedicated integration tests in `vertexrs/tests/` or as `#[test]` blocks inside the bench file, not just benchmark runs.

---

## What changes where — summary table

| File | What changes |
|---|---|
| `vertexrs-macro/src/lib.rs` | +`FusionGroup` enum; +`is_row_fusable`; +`chains_from`; +`group_fusable`; +`emit_fused_chain`; replace `for item in &ordered` loop at line 758 with `for group in group_fusable(&ordered)` loop |
| `vertexrs/benches/pipeline.rs` | +`bench_fusion_vs_unfused` function; +`fusion_correctness` test module; update both `criterion_group!` invocations to include the new group |
| `Makefile.toml` | +`[tasks.bench-smoke]` |
| `.github/workflows/ci.yml` | +`cargo make bench-smoke` step after `cargo make ci` |
| `docs/plans/phase-02-macro-system.md` | Tick 2.7 checkboxes as items complete |
| `docs/plans/bench-baseline.md` | New file created post-merge with first-run throughput numbers |

---

## ADR impact

No new ADR required. This change implements the decision already recorded in ADR-0003 (compile-time kernel fusion). The constraint from ADR-0004 (type-driven execution) is respected: fusion is gated on `is_row_fusable`, which requires a single-arg untyped row-mode call — the columnar/SIMD path.

---

## Out of scope

- SIMD intrinsics (Phase 3.3)
- Sub-pipeline fusion (cross-pipeline chain collapsing)
- Multi-column Frame row-mode fusion (typed args `|price: f64, qty: i32|` — multi-arg closures are not fusable)
- `col()` mode fusion
- Task-node fusion (non-`ArrowNativeType` output types)
- Runtime fusion
- Saving an explicit `--save-baseline initial` baseline (handled automatically by `bench.yml` on first merge)

---

## Open questions

None. All design decisions were confirmed with the maintainer in the interactive session:
- Fuse at chain length >= 2 (confirmed).
- `bench-smoke` step added to PR CI (confirmed).
- No manual baseline save; `bench.yml` seeds it automatically on first post-merge run (confirmed).
- `docs/plans/bench-baseline.md` created by the Implementer post-merge (confirmed).
