# Design: Per-Node Failure Mode Syntax — `?`/`!` Sigils and `pure = false`

| | |
|---|---|
| **Issue** | [#14 — feat(phase-2.6): per-node failure mode syntax](https://github.com/vertexrs-io/vertexrs/issues/14) |
| **Phase** | 2.6 |
| **Branch** | `feat/14-per-node-failure-mode-syntax` |
| **Status** | Proposed |

---

## 1. Approach

Per-node failure mode annotations are handled entirely at the `pipeline!` macro code-generation layer.  When the `pipeline!` parser sees a `node!(…)` item it now parses deeper, extracting:

- A **trailing `?` sigil** (parsed by `syn` as `Expr::Try`) → soft failure override
- A **trailing `!` sigil** (left unconsumed after `Expr::parse`) → hard failure override
- A **`, pure = false`** named option (trailing comma + `pure = <bool>`) → impure node annotation

No sigil = no override; the node is emitted as before (no `catch_unwind`).  The generated `run()` method wraps sigil-carrying nodes in `std::panic::catch_unwind`.  This keeps the `node!` standalone macro unchanged and avoids new runtime types.

Nullability is surfaced by adding an `Option<NullBuffer>` validity field to `Node<T>`.  A new `Node::all_na` constructor creates the all-null fallback value returned on soft failure.  A `pure: bool` field stores the purity annotation for future incremental-executor consumption.

**Why this approach over alternatives:**
- *Executor-level per-node modes* — The `Executor` struct is a separate Phase 1 component with a different execution model (chunked, dirty-tracking).  The `pipeline!`-generated `run()` does not use `Executor`; bridging the two would require a larger refactor out of scope for Phase 2.6.
- *Runtime `PipelineSettings` dispatch for every node* — Adds a per-node branch cost and forces panic-catching for all nodes, even those that will never fail.  Sigil-based opt-in keeps the hot path free of overhead for unannotated nodes.
- *New `node_value!` helper macro* — Evaluated but unnecessary.  By placing the `vertexrs::node!(…)` call **inside** the `catch_unwind` closure and returning the bound variable, the existing macro is reused without introducing a second public/hidden proc-macro.

---

## 2. Reuse Audit

| Existing item | Location | Role in this change |
|---|---|---|
| `NodeInput` struct + `Parse` impl | `vertexrs-macro/src/lib.rs:13–25` | Extended to detect sigil; `pure` option parsed afterwards |
| `extract_node_call` | `vertexrs-macro/src/lib.rs:43–63` | Reused to obtain receiver ident for NA-fallback length |
| `OrderedItem::Node(proc_macro2::TokenStream)` | `vertexrs-macro/src/lib.rs:452` | **Changed** to `OrderedItem::Node(Box<NodeItemMeta>)` |
| `pipeline!` `ord_run` code-gen loop | `vertexrs-macro/src/lib.rs:669–676` | Extended with new branches for sigil-carrying nodes |
| `PipelineImpl::run` generated body | `vertexrs-macro/src/lib.rs:806–818` | Uses `self.__warnings`, `self.__isolated_errors` — also used by soft-failure `Err` branch |
| `Node<T>` struct | `vertexrs/src/lib.rs:209–216` | Two new fields: `validity`, `pure` |
| `Node::new_with_deps` | `vertexrs/src/lib.rs:222–228` | Updated to initialise new fields |
| `Node::from_data` | `vertexrs/src/lib.rs:231–237` | Updated to initialise new fields |
| `Node::to_arrow_array` | `vertexrs/src/lib.rs:287–290` | Updated to pass `self.validity.clone()` |
| `PipelineError::KernelPanic` | `vertexrs/src/pipeline.rs:55` | Reused as the `Err` value for hard-failure nodes |
| `FailureModeKind` enum (macro-internal) | `vertexrs-macro/src/lib.rs:421–426` | Pattern for the new `NodeFailureOverride` enum |
| `WarningCollector` / `self.__warnings` | `vertexrs-macro/src/lib.rs:797`, `executor.rs:55` | `self.__warnings` (pipeline struct field) reused for soft-failure warning |

**New abstractions justified:**

| New item | Why existing code is insufficient |
|---|---|
| `NodeItemMeta` struct | `OrderedItem::Node` only stored raw tokens; sigil detection requires a richer parse result |
| `NodeFailureOverride` enum | No per-node failure concept existed; new concept, no existing type to extend |
| `Node::all_na` constructor | No way to construct an all-null `Node<T>` today; `Node::from_data` + manual validity setup is 5 lines repeated everywhere |
| `Node::with_pure` fluent setter | Needed to set `pure = false` after construction without knowing the concrete type at code-gen time (`Node<T>` vs `BoolNode`) |
| `Node::null_count` method | Needed to expose validity information for tests (AC1) |
| `AnyNode::null_count` / `Frame::null_count` | Frame-level test helper for AC1 |
| `pub validity: Option<NullBuffer>` on `Node<T>` | `ScalarBuffer<T>` carries no validity; Arrow-native null representation per ADR-0001 |
| `pub pure: bool` on `Node<T>` / `BoolNode` | No purity annotation existed at node level |

---

## 3. Module and File Changes

### `vertexrs/src/lib.rs` (modified)
- Add `pub validity: Option<arrow_buffer::NullBuffer>` and `pub pure: bool` to `Node<T>` struct definition
- Update `Node::new_with_deps` and `Node::from_data` to initialise `validity: None, pure: true`
- Add `Node::all_na` constructor
- Add `Node::null_count` method
- Add `Node::with_pure` fluent setter
- Update `Node::to_arrow_array` to pass `self.validity.clone()` as the second arg to `PrimitiveArray::new`
- Add `pub pure: bool` to `BoolNode` struct; update `BoolNode::from_data` and `BoolNode::new_with_deps`; add `BoolNode::with_pure`
- Add `AnyNode::null_count` match arm delegating to per-variant `null_count()`
- Add `Frame::null_count(name: &str) -> Option<usize>` for test assertions

### `vertexrs-macro/src/lib.rs` (modified)
- Add `NodeFailureOverride` enum (`None` | `Soft` | `Hard`)
- Add `NodeItemMeta` struct
- Add `parse_node_item_meta` free function
- Change `OrderedItem::Node(proc_macro2::TokenStream)` → `OrderedItem::Node(Box<NodeItemMeta>)`
- Update `PipelineDef::parse` "node" branch to call `parse_node_item_meta`
- Update `pipeline!` `ord_run` code-gen: add per-failure-mode and per-pure-flag code paths

### `vertexrs/src/lib.rs` tests (new `#[cfg(test)]` cases)
AC1–AC5 integration tests added to the existing test module in `vertexrs/src/lib.rs`.

---

## 4. Type and Trait Signatures

### `vertexrs/src/lib.rs`

#### `Node<T>` — two new public fields
```rust
pub struct Node<T: ArrowNativeType> {
    pub name:     &'static str,
    pub deps:     &'static [&'static str],
    pub data:     ScalarBuffer<T>,
    /// Arrow validity bitmap.  `None` means all values are valid (no NA).
    /// `Some(b)` where `b.null_count() == len` means all values are NA.
    pub validity: Option<arrow_buffer::NullBuffer>,
    /// `false` → always-dirty; the incremental executor must recompute this
    /// node even when all upstream inputs are unchanged.
    pub pure:     bool,
}
```

#### `Node::all_na`
```rust
impl<T: ArrowNativeType> Node<T> {
    /// Creates an all-NA node: data is zeroed, every element is marked null.
    ///
    /// Used by the `pipeline!` macro as the soft-failure fallback value.
    pub fn all_na(
        name: &'static str,
        deps: &'static [&'static str],
        len: usize,
    ) -> Self {
        use arrow_buffer::{BooleanBuffer, NullBuffer};
        Self {
            name,
            deps,
            data:     ScalarBuffer::from(vec![T::default(); len]),
            validity: Some(NullBuffer::new(BooleanBuffer::from(vec![false; len]))),
            pure:     true,
        }
    }
}
```

#### `Node::null_count`
```rust
impl<T: ArrowNativeType> Node<T> {
    /// Number of null elements in this column.
    /// Returns `0` when `validity` is `None` (all values valid).
    pub fn null_count(&self) -> usize {
        self.validity.as_ref().map(|v| v.null_count()).unwrap_or(0)
    }
}
```

#### `Node::with_pure`
```rust
impl<T: ArrowNativeType> Node<T> {
    /// Returns `self` with the `pure` flag overridden.
    ///
    /// Used by the `pipeline!` macro to apply `pure = false` annotations
    /// without needing to know the concrete node type at code-generation time.
    pub fn with_pure(mut self, pure: bool) -> Self {
        self.pure = pure;
        self
    }
}
```

#### `Node::to_arrow_array` (updated signature unchanged, body updated)
```rust
pub fn to_arrow_array(&self) -> PrimitiveArray<T::ArrowType> {
    PrimitiveArray::new(self.data.clone(), self.validity.clone())
}
```

#### `BoolNode` — new `pure` field and `with_pure`
```rust
pub struct BoolNode {
    pub name: &'static str,
    pub deps: &'static [&'static str],
    pub data: BooleanArray,
    pub pure: bool,
}

impl BoolNode {
    pub fn with_pure(mut self, pure: bool) -> Self {
        self.pure = pure;
        self
    }
}
```
`BoolNode::from_data` and `BoolNode::new_with_deps` gain `pure: true` in their body.

#### `AnyNode::null_count`
```rust
impl AnyNode {
    /// Number of null elements in this column.
    pub fn null_count(&self) -> usize {
        match self {
            AnyNode::F16(n)  => n.null_count(),
            AnyNode::F32(n)  => n.null_count(),
            AnyNode::F64(n)  => n.null_count(),
            // … all numeric variants …
            AnyNode::Bool(_) => 0,  // BoolNode validity deferred
            AnyNode::Str(_)  => 0,  // StringNode validity deferred
        }
    }
}
```

#### `Frame::null_count`
```rust
impl Frame {
    /// Returns the number of null elements in column `name`, or `None` if the
    /// column does not exist.
    pub fn null_count(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, any)| any.null_count())
    }
}
```

---

### `vertexrs-macro/src/lib.rs`

#### `NodeFailureOverride`
```rust
/// Per-node failure mode override declared via a trailing sigil on a `node!`
/// expression inside `pipeline!`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NodeFailureOverride {
    /// No sigil — behave as before (panic propagates; existing tests unaffected).
    None,
    /// `?` sigil — soft failure: catch panic, write NA, push warning, continue.
    Soft,
    /// `!` sigil — hard failure: catch panic, return `PipelineError::KernelPanic`.
    Hard,
}
```

#### `NodeItemMeta`
```rust
/// Parsed metadata for a `node!(…)` item inside `pipeline!`.
struct NodeItemMeta {
    /// Node name (the LHS of `name = expr`).
    name: Ident,
    /// Primary receiver identifier, used to compute NA fallback length.
    /// `None` only if the receiver expression is not a simple ident (compile
    /// error in `node!` anyway, so the pipeline will fail to compile).
    receiver_ident: Option<Ident>,
    /// Core expression: `node!` tokens without the sigil and without the
    /// `pure` option.  Suitable for passing directly to `vertexrs::node!(…)`.
    core_expr_tokens: proc_macro2::TokenStream,
    /// Per-node failure override.
    failure_override: NodeFailureOverride,
    /// When `false`, the incremental executor must always recompute this node.
    pure: bool,
}
```

#### `parse_node_item_meta`
```rust
/// Parses `name = expr[?|!][, pure = bool]` from the content of a `node!(…)`
/// declaration inside `pipeline!`.
fn parse_node_item_meta(input: ParseStream) -> syn::Result<NodeItemMeta> {
    let name: Ident = input.parse()?;
    input.parse::<Token![=]>()?;

    // Parse the expression.  `syn` will consume `?` as `Expr::Try` but will
    // leave a trailing `!` unconsumed (not a valid postfix operator in Rust).
    let expr: Expr = input.parse()?;

    // Detect sigil.
    let (core_expr, failure_override) = if let Expr::Try(try_expr) = expr {
        (*try_expr.expr, NodeFailureOverride::Soft)
    } else if input.peek(Token![!]) {
        input.parse::<Token![!]>()?;
        // `expr` already excludes the `!`
        (expr, NodeFailureOverride::Hard)
    } else {
        (expr, NodeFailureOverride::None)
    };

    // Detect `, pure = <bool>`.
    let pure = if input.peek(Token![,]) {
        input.parse::<Token![,]>()?;
        let key: Ident = input.parse()?;
        if key != "pure" {
            return Err(syn::Error::new(
                key.span(),
                "expected `pure` key in node options (e.g. `pure = false`)",
            ));
        }
        input.parse::<Token![=]>()?;
        let val: syn::LitBool = input.parse()?;
        val.value
    } else {
        true
    };

    // Extract receiver ident for NA-fallback length.
    let receiver_ident = extract_node_call(&core_expr)
        .and_then(|nc| receiver_ident(nc.receiver).cloned());

    let core_expr_tokens = quote::quote! { #name = #core_expr };

    Ok(NodeItemMeta {
        name,
        receiver_ident,
        core_expr_tokens,
        failure_override,
        pure,
    })
}
```

---

## 5. Call Flow

### 5.1 Parsing (compile time)

```
pipeline! { … node!(tax = price.row(|_x| -> f64 { panic!("oops") })?) … }
  └─ PipelineDef::parse
       └─ "node" branch
            └─ parse_node_item_meta(content)
                 ├─ parse `tax`, `=`
                 ├─ parse Expr → Expr::Try { expr: price.row(…) }
                 ├─ failure_override = Soft
                 ├─ receiver_ident = Some(Ident("price"))
                 ├─ core_expr_tokens = `tax = price.row(|_x| -> f64 { panic!("oops") })`
                 └─ NodeItemMeta { name: tax, receiver_ident: price,
                                   failure_override: Soft, pure: true }
```

### 5.2 Code generation (compile time)

For the `Soft` case, `pipeline!` emits in `run()`:
```rust
let tax = match ::std::panic::catch_unwind(
    ::std::panic::AssertUnwindSafe(|| {
        #[allow(unused_imports)]
        use vertexrs::{Node, BoolNode, ColRef};
        vertexrs::node!(tax = price.row(|_x| -> f64 { panic!("oops") }));
        tax
    })
) {
    ::core::result::Result::Ok(__n) => __n,
    ::core::result::Result::Err(__e) => {
        let __msg = __e.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| __e.downcast_ref::<::std::string::String>().cloned())
            .unwrap_or_else(|| "kernel panicked".to_string());
        self.__warnings.push(::std::format!(
            "node 'tax': kernel failed: {}", __msg
        ));
        vertexrs::Node::all_na("tax", &[], price.len())
    }
};
```

For the `Hard` case:
```rust
let tax = ::std::panic::catch_unwind(
    ::std::panic::AssertUnwindSafe(|| {
        #[allow(unused_imports)]
        use vertexrs::{Node, BoolNode, ColRef};
        vertexrs::node!(tax = price.row(|_x| -> f64 { panic!("oops") }));
        tax
    })
).map_err(|__e| {
    let __msg = __e.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| __e.downcast_ref::<::std::string::String>().cloned())
        .unwrap_or_else(|| "kernel panicked".to_string());
    vertexrs::PipelineError::KernelPanic(
        ::std::format!("node 'tax': {}", __msg)
    )
})?;
```

For `pure = false` (no sigil):
```rust
vertexrs::node!(counter = src.row(|x| x + 1.0));
let counter = counter.with_pure(false);
```

For `pure = false` with `Soft` sigil, both transforms are applied:
```rust
let counter = match ::std::panic::catch_unwind(…) {
    Ok(__n) => __n,
    Err(__e) => { … Node::all_na(…) }
}.with_pure(false);
```

For nodes with **no sigil** (default):
```rust
// Unchanged from today — no catch_unwind:
vertexrs::node!(tax = price.row(|x| x * 0.2));
```

### 5.3 Runtime (per `Pipeline::compute` call)

1. `Pipeline::compute` → `PipelineImpl::run`
2. Source binds established (existing behaviour)
3. Each node executes in declaration order via `ord_run` body
4. **Sigil-carrying node (Soft):**  
   a. `catch_unwind` executes the `node!` expansion  
   b. Panic: warning pushed to `self.__warnings`; `Node::all_na` returned as `tax`  
   c. Execution continues to subsequent nodes (downstream uses the zero-value data with all-null validity)  
   d. No early return → `run()` returns `Ok(())`
5. **Sigil-carrying node (Hard):**  
   a. `catch_unwind` executes the `node!` expansion  
   b. Panic: `PipelineError::KernelPanic` constructed, propagated via `?`  
   c. `run()` returns `Err(PipelineError::KernelPanic(…))` immediately  
   d. `self.__output` is NOT updated — retains its value from the previous successful `run()` call
6. **`pure = false` node:** executes normally; only `.pure` flag on the resulting `Node<T>` is set to `false`
7. At end of `run()`: output frame assembled and stored in `self.__output`; `run()` returns `Ok(())`

---

## 6. Executor Path

Phase 2 pipelines generated by `pipeline!` do **not** use the `Executor` struct from `executor.rs`.  They execute via inline Rust code in the generated `run()` method (simple `let` bindings).  The `Executor`'s `FailureMode` field is unchanged.

The `pure: bool` field on `Node<T>` is metadata stored for **future** use by the incremental `Executor` (Phase 3+).  The current `Executor` does not read it.

---

## 7. ADR Impact

**No new ADR required.**

ADR-0005 already acknowledges: *"the macro must be updated when new node types or execution modes are added"* — this change is an expected evolution within the existing decision.

ADR-0001 (Arrow as memory substrate) is satisfied by using `arrow_buffer::NullBuffer` / `BooleanBuffer` for the validity bitmap in `Node<T>`.

---

## 8. Known Limitations / Out of Scope

- **`BoolNode` soft-failure NA**: `BoolNode` does not gain a `validity` field in this issue.  If a node annotated with `?` returns a `BoolNode` (closure with `-> bool` return type), the soft-failure `Err` branch would emit `Node::all_na(…)` — a type mismatch (unless `BoolNode::all_na` is also added).  To avoid this, the Implementer should add `BoolNode::all_na` returning a `BoolNode` of all-`false` values, and handle the BoolNode case in code-gen via the same `with_pure`-style pattern.  If too complex to unify cleanly, restrict soft/hard sigils to non-bool nodes and emit a compile error for bool nodes with sigils.
- **`StringNode` / other exotic node types**: Not in scope; validity/purity deferred.
- **Failure mode for pipeline-level `settings { failure: … }` dispatching to un-annotated nodes**: This issue only adds per-node overrides for annotated nodes.  The `PipelineSettings::failure_mode` field is not wired to un-annotated nodes; that wiring is a future phase concern.  AC5 confirms this by stating default (no sigil) behaviour is unchanged.
- **Incremental executor skipping `pure = true` nodes**: Deferred to the incremental executor phase.
- **`na_threshold` threshold warnings on NA output**: The existing `PipelineSettings::na_threshold` field is not wired; deferred per plan §2.2.

---

## 9. Open Questions

None — all acceptance criteria are unambiguous and the implementation path is clear.

---

## 10. Test Plan

Each test maps directly to an acceptance criterion.  All tests go in the `#[cfg(test)]` module in `vertexrs/src/lib.rs`.

| Test name | AC | Assertion |
|---|---|---|
| `soft_sigil_returns_ok_and_warns` | AC1 | `compute().is_ok()`; `null_count("tax") == price_len`; `drain_warnings()` contains `"tax"` |
| `hard_sigil_returns_kernel_panic_err` | AC2 | `compute() == Err(KernelPanic(_))`; `output()` retains pre-call value for `total` |
| `node_sigil_overrides_pipeline_hard_setting` | AC3 | `?` node in `settings { failure: Hard }` pipeline → `Ok(())`; warnings non-empty |
| `node_bang_overrides_pipeline_soft_setting` | AC3 | `!` node in `settings { failure: Soft }` pipeline → `Err(KernelPanic(_))` |
| `pure_false_compiles_and_correct_values` | AC4 | Output matches non-annotated pipeline; `output_node.pure == false` (checked via Frame) |
| `default_no_sigil_unchanged` | AC5 | All existing tests pass; a non-failing node without sigil continues to work |

> **Note on AC4 testability**: `Frame::get::<T>` returns `&[T]` with no purity info.  To assert `pure == false`, the test should access the `Node<T>` via an `AnyNode`-level getter or by adding a `Frame::get_node::<T>` accessor returning `Option<&Node<T>>`.  The simplest approach: add `Frame::get_node::<T>(name) -> Option<&Node<T>>` (parallel to `Frame::get`) — this is a useful API addition for any future phase that needs more than raw slice access.
