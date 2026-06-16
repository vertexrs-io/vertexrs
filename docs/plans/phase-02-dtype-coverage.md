[← Phase 2 Plan](phase-02-macro-system.md)

# Phase 2.11 — Dtype-Coverage Matrix

`AnyNode` supports 11 primitive types: `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`, `f16`, `f32`, `f64`.

This document records the accepted dtype set for each 2.11 sub-issue. No sub-issue may begin implementation until this document is merged.

**Correctness tolerance (applies everywhere):**
- `f32`/`f64`: `abs(vtx − polars) < 1e-6`
- Integer types: exact equality
- `f16`: widen both sides to `f32` before comparing with the `f32` tolerance

---

## Summary Table

| Sub-issue | Issue | Title | Accepted dtypes | Excluded |
|---|---|---|---|---|
| D1 | #41 | Joins — equality (inner, left, right, outer, semi, anti) | All 11 for value cols; `i8`–`u64` for key cols | `f16`/`f32`/`f64` as join keys |
| D2 | #45 | Joins — asof, cross, non-equi | All 11 for value cols; `i32`, `i64`, `f32`, `f64` for asof key | `f16` as asof key |
| E  | #34 | Group-By Aggregations | `i32`, `i64`, `u32`, `u64`, `f32`, `f64` + small ints for correctness | `f16` |
| F  | #42 | Window Functions | `f32`, `f64`, `i32`, `i64`; `u32`/`u64` correctness-only | `f16`, `i8`, `i16`, `u8`, `u16` |
| G  | #35 | Basic Scalar Operations | All 11 (per-op exclusions noted below) | Floats from bitwise ops |
| H  | #36 | Reshaping and Concatenation | All 11 | None |
| I  | #43 | Missing Data | All 11 | None |
| J  | #37 | Type Casting | All 11 (all source→target pairs) | None |
| K  | #38 | String Operations | `Utf8`/`StringNode` input only; `u32`/`bool` outputs | All 11 numeric types as input |
| L  | #39 | Time-Series Operations | `f32`, `f64`, `i32`, `i64` for value cols; `i64` for timestamp | `f16`, `u8`, `u16`, `i8`, `i16` |

---

## D1 — Joins: Equality Joins (#41)

Inner, left, right, full outer, semi, anti.

**Value columns (data gathered during join):** All 11 AnyNode types. The join gather operation is type-generic — it reindexes buffers regardless of element type.

**Join key columns:** `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64` (integer types only).
- `f32`/`f64`/`f16` keys excluded: floating-point equality is semantically unsafe for join keys (`NaN != NaN`; two observationally equal values may differ in the last bit). Any user needing a float key must cast to an integer representation first.

**Benchmarks parametrised over:** `f32`, `f64`, `i32`, `i64` (value columns; key column is always `i32`).

**Correctness tests:** Each join type tested with `i32` key and `f64` value columns, compared to Polars within tolerance.

---

## D2 — Joins: Asof, Cross, Non-Equi (#45)

**Cross join:** No key column; value columns: all 11 AnyNode types.

**Asof join key column:** `i32`, `i64`, `u32`, `u64`, `f32`, `f64` (ordered types that support `PartialOrd`).
- `f16` excluded from asof keys: limited hardware ordering support; asof keys are typically `i64` timestamps or `f64` prices.
- Value columns: all 11 AnyNode types.

**Non-equi / predicate join:** User-supplied closure receives row values; all 11 AnyNode types are valid operands in the predicate. No dtype exclusions.

**Benchmarks:** `f32`, `f64`, `i32`, `i64` (consistent with D1).

---

## E — Group-By Aggregations (#34)

**Accepted value-column types:** `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `f32`, `f64` (10 of 11).

**Excluded:** `f16` — rolling sum/mean in f16 accumulates precision error that grows with group size; Polars itself does not support f16 group-by natively, making correctness comparison impractical.

**Benchmarks parametrised over:** `f32`, `f64`, `i32`, `i64`. Smaller integer types (`i8`, `i16`, `u8`, `u16`) are covered by correctness tests but not benchmarked separately.

**Std/Var:** Implemented for `f32`/`f64` natively; for integer types the implementation widens to `f64` for the intermediate accumulation, then returns `f64` output.

---

## F — Window Functions (#42)

**Accepted value-column types:** `f32`, `f64`, `i32`, `i64`, `u32`, `u64` (6 of 11).

**Excluded:**
- `f16`: precision loss in rolling accumulations; `f16` hardware support varies; widening-then-narrowing adds implementation cost with minimal user value.
- `i8`, `i16`, `u8`, `u16`: impractical for window function output (overflow risk in rolling sum; rarely used in practice for windowed analytics).

**Rolling mean:** Returns `f64` for integer input types to avoid precision loss; `f32` for `f32` input, `f64` for `f64` input.

**Benchmarks parametrised over:** `f32`, `f64`. `i32`/`i64`/`u32`/`u64` covered by correctness tests.

---

## G — Basic Scalar Operations (#35)

**Default accepted types:** All 11 AnyNode types, with the per-operation exclusions below.

| Operation | Accepted | Excluded | Rationale |
|---|---|---|---|
| `+`, `-`, `*`, `/`, `%` | All 11 | — | All numeric types support these |
| `**` (pow) | `f32`, `f64`, `i32`, `i64`, `u32`, `u64`, `f16` | `i8`, `i16`, `u8`, `u16` | Small integer pow overflows trivially; implement for the six standard types + f16 |
| Comparisons (`>`, `<`, `==`, etc.) | All 11 | — | All types are `PartialOrd` |
| Bitwise (`&`, `\|`, `^`, `~`) | `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64` | `f16`, `f32`, `f64` | Bitwise ops are undefined for float bit patterns in the context of data semantics |
| `when/then/otherwise` | All 11 | — | Type-generic ternary; no dtype constraint |
| `n_unique`, `unique`, `value_counts` | All 11 | — | Operates on equality; all types support `Eq` (or bitwise equality for floats, consistent with Polars) |
| `approx_n_unique` | All 11 | — | HyperLogLog operates on hash values; type-generic |

**Benchmarks parametrised over:** `f32`, `f64`, `i32`, `i64`.

---

## H — Reshaping and Concatenation (#36)

**Accepted types:** All 11 AnyNode types.

- `vstack` and `hstack` are purely mechanical — they append buffers and update metadata without inspecting element values. No dtype exclusions.
- `pivot` and `unpivot` value columns: all 11 AnyNode types. The index/column key in pivot is typically a string or integer column (handled by the column type, not constrained by AnyNode numeric types).
- Aggregation inside `pivot` follows the constraints of the aggregation function used (see E above for group-by dtype constraints).

**Benchmarks:** `f32`, `f64`, `i32`, `i64` for pivot; vstack/hstack are dtype-agnostic so benchmark with `f64` only.

---

## I — Missing Data (#43)

**Accepted types:** All 11 AnyNode types.

Fill and drop operations act on the validity bitmap and the raw buffer uniformly. The fill value for `fill_null_literal` must match the column type, but there is no dtype exclusion — all 11 types support the operation.

**Benchmarks parametrised over:** `f32`, `f64`, `i32`, `i64`.

---

## J — Type Casting (#37)

**Accepted types:** All 11 AnyNode types as both source and target.

The purpose of this sub-issue is cross-type conversion. Arrow's safe cast matrix covers all numeric widening and narrowing paths. Overflow in narrowing casts saturates (not UB), documented as the defined behaviour.

**Accepted cast pairs (non-exhaustive):**
- All numeric → all numeric (widening and narrowing)
- Integer ↔ float (widening: no precision loss possible; narrowing: truncation)
- `f16` ↔ `f32` ↔ `f64` (widening and narrowing via `half` crate)
- `bool` ↔ integer (0/1 encoding)

**Benchmarks:** Representative cross-type pairs: `f32 → f64`, `i32 → f64`, `f64 → i32`, `f32 → f16`, `f16 → f32`.

---

## K — String Operations (#38)

**Input type:** `Utf8`/`StringNode` only (Arrow `StringArray` layout). The 11 AnyNode numeric types are not valid inputs to string operations.

**Output types produced by each operation:**
- `str_len` → `u32` (byte length)
- `str_contains`, `str_starts_with`, `str_ends_with` → `BoolNode`
- `str_to_uppercase`, `str_to_lowercase`, `str_replace`, `str_slice` → `StringNode`

**Dtype parametrisation for benchmarks:** Not applicable. String operation benchmarks use string column inputs only; no numeric dtype axis.

**Rationale for exclusion of all 11 numeric types:** String operations are semantically inapplicable to numeric data. Applying `str_contains` to an `f64` column has no meaningful interpretation.

---

## L — Time-Series Operations (#39)

**Timestamp key column type:** Always `i64` (microseconds since Unix epoch; mirrors Arrow `TimestampMicrosecond`). No other type is accepted for the timestamp axis.

**Value column types for rolling and resample operations:** `f32`, `f64`, `i32`, `i64` (4 of 11).

**Excluded:**
- `f16`: rolling accumulations in f16 lose precision rapidly; sensor/financial time-series data is always stored as `f32`/`f64` or integer types.
- `u8`, `u16`, `u32`, `u64`: unsigned types are uncommon for time-series values (counts or indices, not measurements); `u32`/`u64` may be added in a follow-up if needed.
- `i8`, `i16`: overflow risk in rolling sums over typical time-series window sizes.

**Benchmarks parametrised over:** `f64` (primary — financial tick data); `f32` (secondary).
