# ADR-0004: Type-Driven Execution Strategy Selection

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-05-23 |
| **Supersedes** | — |

## Context

VertexRS targets two distinct workload classes: (1) high-throughput columnar data pipelines over scalar primitives, best served by SIMD/vectorised execution; and (2) general-purpose task/process graphs over arbitrary Rust types (structs, enums, domain objects), best served by a thread-pool executor. Users should not have to choose the execution mode or annotate nodes — the engine should determine the path automatically.

## Decision

Use the node's type to select the execution strategy at compile time. Nodes whose type `T` implements `ArrowNativeType` (i.e. `f32`, `f64`, `i32`, `i64`, `u32`, `u64`, `f16`, `i8`, `u8`, `i16`, `u16`) route to the columnar/SIMD path. All other types route to the task/rayon path. This selection happens inside the `pipeline!` macro via a trait bound check; no runtime dispatch or user annotation is required.

## Alternatives considered

- **Explicit `#[columnar]` / `#[task]` annotations** — more control for the user but violates the "declarative, no boilerplate" design goal; easy to annotate incorrectly.
- **Runtime type inspection** — possible via `TypeId` but prevents compile-time optimisation (fusion, SIMD codegen); adds an avoidable dispatch cost per chunk.
- **Single unified executor** — treat all types the same; loses SIMD gains for scalar types entirely.

## Consequences

**Positive:** zero user-facing complexity — the same `pipeline!` / `node!` syntax works for both workload types; scalar primitives get SIMD without annotation; struct-typed nodes get parallelism without annotation.

**Negative / trade-offs:** the boundary is `ArrowNativeType`, which is an Arrow crate concern leaking into the user-facing type model; composite types wrapping primitives (e.g. a newtype `struct Price(f64)`) route to the task path even if they would benefit from SIMD — users must unwrap to primitive or implement `ArrowNativeType`.
