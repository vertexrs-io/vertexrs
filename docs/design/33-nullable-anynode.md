# Design: Nullable AnyNode — Validity Bitmap + Null Propagation

| | |
|---|---|
| **Issue** | [#33 — feat(2.11.0): nullable AnyNode — validity bitmap + null propagation](https://github.com/vertexrs-io/vertexrs/issues/33) |
| **Phase** | 2.11.0 |
| **Branch** | `feat/33-nullable-anynode` |
| **Status** | Proposed |

---

## 1. Approach

Issue #14 (per-node failure mode) already added `pub validity: Option<NullBuffer>` to `Node<T>`,
`Node::all_na`, `Node::null_count`, `AnyNode::null_count`, and `Frame::null_count`. The structural
foundation for nullable columns is therefore already in place.

Issue #33 adds the remaining three things:

1. **Public read accessors on `AnyNode`** — `validity() -> Option<&NullBuffer>` and
   `is_nullable() -> bool` (AC1, AC3).
2. **A `with_validity` builder** on `Node<T>` and a `combine_validity` helper so that
   row-mode kernels can propagate input null masks to their output (AC2).
3. **`Frame::get_validity`** — the frame-level accessor for the bitmap (AC4).

The null-propagation convention follows Arrow: kernels compute on all rows regardless of nullity
and the validity bitmap is masked afterwards. A `None` fast-path (all inputs non-nullable)
short-circuits to no bitmap work at all. This is recorded in ADR-0006.

Row-mode is the only macro expansion path touched in this issue. Column mode and BoolNode/StringNode
validity are explicitly deferred.

---

## 2. Reuse Audit

| Existing item | Location | Role in this change |
|---|---|---|
| `pub validity: Option<NullBuffer>` field on `Node<T>` | `vertexrs/src/lib.rs:232` | Reused directly; no structural change to `Node<T>` |
| `Node::all_na` | `vertexrs/src/lib.rs:273` | Reused unchanged (soft-failure path from #14) |
| `Node::null_count` | `vertexrs/src/lib.rs:294` | Reused unchanged |
| `Node::new_with_deps` | `vertexrs/src/lib.rs:242` | Called by macro expansion; `with_validity` chains on its return value |
| `AnyNode::null_count` | `vertexrs/src/lib.rs:551` | Already dispatches to per-variant `null_count()`; `validity()` is a parallel impl |
| `Frame::null_count` | `vertexrs/src/lib.rs:784` | Reused; `get_validity` is a companion accessor, same internal pattern |
| `NullBuffer` / `BooleanBuffer` | `arrow_buffer` (already imported at `vertexrs/src/lib.rs:30`) | `combine_validity` uses `NullBuffer::new` and bitwise AND via `BooleanBuffer` |
| `Frame::get` / `Frame::get_bool` / `Frame::get_str` | `vertexrs/src/lib.rs:715–752` | Pattern for `get_validity`: find-by-name, delegate to `AnyNode::validity()` |
| Single-col row mode code-gen | `vertexrs-macro/src/lib.rs:287–357` | Two sites modified to chain `.with_validity(...)` |
| Multi-col Frame row mode code-gen | `vertexrs-macro/src/lib.rs:228–285` | One site modified; `combine_validity` called to merge column bitmaps |

**New abstractions and justifications:**

| New item | Why existing code is insufficient |
|---|---|
| `Node::combine_validity` (associated fn) | No existing way to compute the intersection of multiple `Option<NullBuffer>` inputs; needed by both macro sites |
| `Node::with_validity` (builder method) | No way to set `validity` after construction; needed so the macro expansion can attach the propagated bitmap to the output node without forking `new_with_deps` |
| `AnyNode::validity()` | `null_count()` only gives a count; callers (including `Frame::get_validity` and downstream code) need the actual bitmap reference |
| `AnyNode::is_nullable()` | Convenience predicate; avoids `validity().is_some()` callsites having to pattern-match |
| `Frame::get_validity` | `Frame::null_count` returns only a count; AC4 requires returning the bitmap itself |

---

## 3. Module and File Changes

### `vertexrs/src/lib.rs` (modified)

- Add `Node::combine_validity` associated function
- Add `Node::with_validity` builder method
- Add `AnyNode::validity() -> Option<&NullBuffer>` method
- Add `AnyNode::is_nullable() -> bool` method
- Add `Frame::get_validity(name: &str) -> Option<&NullBuffer>` method

No new files; no files removed.

### `vertexrs-macro/src/lib.rs` (modified)

Two row-mode code-gen sites updated:

- **Single-col row mode** (~line 342): chain `.with_validity(Node::combine_validity([src.validity.as_ref()]))` on the `Node::new_with_deps(...)` call
- **Multi-col Frame row mode** (~line 273): chain `.with_validity(Node::combine_validity([col_a.validity.as_ref(), col_b.validity.as_ref(), ...]))` — one entry per typed closure argument

BoolNode single-col row mode (~line 327) and all col-mode paths are left unchanged.

---

## 4. Type and Trait Signatures

All items below are additions to `vertexrs/src/lib.rs` unless noted.

### `Node::combine_validity`

```rust
impl<T: ArrowNativeType> Node<T> {
    /// Computes the intersection of zero or more input validity bitmaps.
    ///
    /// Returns `None` (all valid) if every input is `None`.  Otherwise returns
    /// `Some(NullBuffer)` whose null bits are the union of all input null bits
    /// (a position is null in the output iff it is null in any input).
    ///
    /// The fast path — all `None` inputs — returns `None` with no allocation.
    ///
    /// # Panics
    /// Panics if the non-`None` buffers differ in length; this indicates a
    /// programming error (mismatched column lengths, an invariant enforced by
    /// `Frame::append`).
    pub fn combine_validity<'a, I>(iter: I) -> Option<NullBuffer>
    where
        I: IntoIterator<Item = Option<&'a NullBuffer>>,
    {
        let buffers: Vec<&NullBuffer> = iter.into_iter().flatten().collect();
        if buffers.is_empty() {
            return None;
        }
        // Bitwise AND of Boolean (validity) buffers: a bit is 1 iff valid in all inputs.
        // Arrow validity convention: 1 = valid, 0 = null.
        let combined = buffers
            .iter()
            .map(|b| b.inner().clone())          // BooleanBuffer
            .reduce(|a, b| &a & &b)
            .expect("non-empty iterator always has at least one element");
        Some(NullBuffer::new(combined))
    }
}
```

### `Node::with_validity`

```rust
impl<T: ArrowNativeType> Node<T> {
    /// Returns `self` with the validity bitmap replaced.
    ///
    /// `None` makes the node non-nullable (all values valid).
    /// Chained after [`Node::new_with_deps`] by the [`node!`] macro to attach
    /// a propagated null mask to a derived node.
    pub fn with_validity(mut self, validity: Option<NullBuffer>) -> Self {
        self.validity = validity;
        self
    }
}
```

### `AnyNode::validity`

```rust
impl AnyNode {
    /// Returns the Arrow validity bitmap for this column, if it is nullable.
    ///
    /// `None` means all values are valid (no nulls).  `Bool` and `Str` variants
    /// always return `None` in this release (validity deferred).
    pub fn validity(&self) -> Option<&NullBuffer> {
        match self {
            AnyNode::F16(n)  => n.validity.as_ref(),
            AnyNode::F32(n)  => n.validity.as_ref(),
            AnyNode::F64(n)  => n.validity.as_ref(),
            AnyNode::I8(n)   => n.validity.as_ref(),
            AnyNode::I16(n)  => n.validity.as_ref(),
            AnyNode::I32(n)  => n.validity.as_ref(),
            AnyNode::I64(n)  => n.validity.as_ref(),
            AnyNode::U8(n)   => n.validity.as_ref(),
            AnyNode::U16(n)  => n.validity.as_ref(),
            AnyNode::U32(n)  => n.validity.as_ref(),
            AnyNode::U64(n)  => n.validity.as_ref(),
            // BoolNode and StringNode validity deferred to a future issue.
            AnyNode::Bool(_) => None,
            AnyNode::Str(_)  => None,
        }
    }

    /// Returns `true` if this column carries a validity bitmap (has at least
    /// one potentially-null position).
    ///
    /// Equivalent to `self.validity().is_some()`.
    pub fn is_nullable(&self) -> bool {
        self.validity().is_some()
    }
}
```

### `Frame::get_validity`

```rust
impl Frame {
    /// Returns the validity bitmap for column `name`.
    ///
    /// Returns `None` if the column exists but is non-nullable.
    ///
    /// # Panics
    /// Panics if no column with that name exists (matching the behaviour of
    /// [`Frame::get`]).
    pub fn get_validity(&self, name: &str) -> Option<&NullBuffer> {
        let (_, any) = self
            .columns
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("column '{}' not found in Frame", name));
        any.validity()
    }
}
```

---

## 5. Macro Changes

### Site 1 — Single-col row mode, non-bool branch (`vertexrs-macro/src/lib.rs` ~line 342)

Current emitted code:
```rust
let #name = Node::new_with_deps(
    #name_lit,
    &[#recv_lit, #(#extra_dep_lits),*],
    (0..#recv_ident_ts.len())
        .map(|__vtx_i| #map_return_hint { #arg_bind #(#dep_binds)* #body })
        .collect::<Vec<_>>(),
);
```

New emitted code (adds `.with_validity(...)` chain):
```rust
let #name = Node::new_with_deps(
    #name_lit,
    &[#recv_lit, #(#extra_dep_lits),*],
    (0..#recv_ident_ts.len())
        .map(|__vtx_i| #map_return_hint { #arg_bind #(#dep_binds)* #body })
        .collect::<Vec<_>>(),
).with_validity(Node::combine_validity([
    #recv_ident_ts.validity.as_ref(),
    #(#extra_deps.validity.as_ref()),*
]));
```

The `extra_deps` idents are already collected at that site for `extra_dep_lits`; they are reused here.

### Site 2 — Multi-col Frame row mode (`vertexrs-macro/src/lib.rs` ~line 273)

Current emitted code:
```rust
let #name = Node::new_with_deps(
    #name_lit,
    &[#(#col_dep_lits,)* #(#extra_dep_lits),*],
    (0..#recv_ident_ts.len())
        .map(|__vtx_i| { #(#col_binds)* #body })
        .collect::<Vec<_>>(),
);
```

New emitted code:
```rust
let #name = Node::new_with_deps(
    #name_lit,
    &[#(#col_dep_lits,)* #(#extra_dep_lits),*],
    (0..#recv_ident_ts.len())
        .map(|__vtx_i| { #(#col_binds)* #body })
        .collect::<Vec<_>>(),
).with_validity(Node::combine_validity([
    #(#recv_ident_ts.get_any(#col_dep_lits)
        .map(|a| a.validity())
        .flatten()),*
]));
```

**Note on `Frame::get_any`**: the multi-col site needs to reach into the Frame to get each column's
bitmap. This requires a new private helper `Frame::get_any(name: &str) -> Option<&AnyNode>` (or
the equivalent inline search). The Implementer should add this as a private method on `Frame` — it
is not public API. Alternatively, the macro can call `frame.get_validity(col_name)` for each
typed argument directly (since `get_validity` panics on missing, and `Frame::append` already guards
against that). The simpler form is:

```rust
).with_validity(Node::combine_validity([
    #(#recv_ident_ts.get_validity(#col_dep_lits_as_str)),*
]));
```

The Implementer should choose whichever form compiles cleanly given the available `quote!` ident
expansion at that site. Both are equivalent in semantics.

---

## 6. Call Flow

### 6.1 Non-nullable source → derived node (fast path)

```
pipeline!
  └─ node!(out = src.row(|x: f64| x * 2.0))
       [macro expansion]
       ├─ Node::new_with_deps("out", &["src"], values)
       └─ .with_validity(Node::combine_validity([src.validity.as_ref()]))
              combine_validity: all inputs are None → returns None immediately
              with_validity(None): sets out.validity = None
       → out.is_nullable() == false  ✓
```

### 6.2 Nullable source → derived node (propagation path)

```
pipeline!
  └─ node!(out = src.row(|x: f64| x * 2.0))
       [macro expansion]
       ├─ Node::new_with_deps("out", &["src"], values)   // compute on ALL rows
       └─ .with_validity(Node::combine_validity([src.validity.as_ref()]))
              combine_validity: one Some → clones and returns same bitmap
              with_validity(Some(bitmap)): out.validity = Some(bitmap)
       → out.is_nullable() == true
       → out.validity() == src.validity()   ✓
```

### 6.3 Frame::get_validity call

```
frame.get_validity("price")
  └─ self.columns.iter().find(name == "price") → Some((_, any))
       └─ any.validity()                        → AnyNode::F64(n) => n.validity.as_ref()
            → Some(&NullBuffer) or None
```

### 6.4 AnyNode::is_nullable call

```
any_node.is_nullable()
  └─ self.validity().is_some()
       └─ AnyNode::Bool(_) => None → false
          AnyNode::F64(n)  => n.validity.as_ref() → Some(&buf) → true / None → false
```

---

## 7. Executor Path

Pipeline `run()` bodies generated by `pipeline!` do not use the `Executor` struct; they execute as
inline `let` bindings. The validity bitmap is computed synchronously inside each `let` binding
immediately after `new_with_deps`. The `combine_validity` call is `O(k)` in the number of input
columns and `O(n/64)` in the row count (one 64-bit AND per word); it is not on the SIMD hot path.

This issue does not change how dirty-chunk tracking works. ADR-0002 notes that "a null in chunk `k`
of a source must mark chunk `k` of the derived node dirty" — that constraint is satisfied because
null propagation here operates at the full-column level and does not suppress the dirty bit. The
incremental executor (Phase 3+) will need to re-evaluate this when chunk-granular null propagation
is added; that is out of scope here.

---

## 8. ADR Impact

A new ADR is required: **ADR-0006** records the Arrow "compute-all-rows, mask-after" null
propagation convention and the `None` fast-path semantics. See `docs/adr/0006-null-propagation-row-kernels.md`.

---

## 9. Out of Scope

- `BoolNode` and `StringNode` validity fields — deferred.
- Column-mode (`col(|...)`) null propagation — deferred.
- `fill_null` / `drop_nulls` operations — issue 2.11.6.
- Outer join logic — issue 2.11.1.
- Nested null propagation through sub-pipelines.
- Dictionary-encoded / categorical nulls.
- Chunk-granular null propagation for the incremental executor (Phase 3+).

---

## 10. Open Questions

None — all acceptance criteria are unambiguous and the implementation path is clear.

---

## 11. Test Plan

All tests go in the `#[cfg(test)]` module in `vertexrs/src/lib.rs`.

| # | Test name | What is tested | New code paths exercised |
|---|---|---|---|
| i | `combine_validity_all_none_returns_none` | All `None` inputs → `None` output, no allocation | `combine_validity` fast path |
| ii | `combine_validity_one_some_all_valid` | `Some(all-valid)` + `None` → `Some` with `null_count == 0` | `combine_validity` single-buffer path |
| iii | `combine_validity_partial_and_none` | `Some(partial)` + `None` → propagated partial bitmap | `combine_validity` clone path |
| iv | `combine_validity_two_some_partial` | Two `Some(partial)` → AND result | `combine_validity` bitwise-AND path |
| v | `null_propagates_through_row_node_in_pipeline` | End-to-end: nullable source → `node!` → output bitmap matches source | Macro single-col expansion, `with_validity`, `AnyNode::validity`, `Frame::get_validity` |
| vi | `anynode_bool_validity_is_none` | `AnyNode::Bool(_).validity()` → `None`, `is_nullable()` → `false` | `AnyNode::Bool` deferred branch |
| vii | `anynode_str_validity_is_none` | `AnyNode::Str(_).validity()` → `None`, `is_nullable()` → `false` | `AnyNode::Str` deferred branch |
| viii | `with_validity_none_clears_bitmap` | `.with_validity(None)` on a node that was nullable → non-nullable | `with_validity` None branch |

Tests (vi) and (vii) are required to reach the `Bool` and `Str` arms of the `AnyNode::validity`
match and achieve ≥90% line coverage on those branches. Test (viii) verifies the builder contract
for the `None` argument.

The end-to-end test (v) should use a multi-chunk pipeline if the chunked path can be exercised
without the Phase 3 executor; otherwise a single-chunk column is sufficient for correctness
verification of the bitmap contents.
