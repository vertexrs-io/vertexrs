# ADR-0005: Macro-Defined DAG with Direct Node References

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-05-23 |
| **Supersedes** | — |

## Context

DAG computation frameworks typically require users to explicitly declare edges (`add_edge(a, b)`) or use string-keyed node lookups. This is verbose, error-prone (typos in edge declarations, mismatched types), and prevents the compiler from catching topology errors. VertexRS aims for a declarative syntax where the DAG topology is inferred from how nodes reference each other.

## Decision

Use a proc-macro (`pipeline!`) that parses node definitions where inputs are referenced by name. The macro builds the DAG struct and wires up edges at compile time by analysing which node names appear in each node's kernel expression. The result is a typed Rust struct where each field is a `Node<T>` and the dependency graph is an implicit property of the struct definition, not an explicit adjacency list.

```rust
pipeline! {
    a: Node<f64> = source();
    b: Node<f64> = node!(a * 2.0);
    c: Node<f64> = node!(a + b);   // edges a→c and b→c are inferred
}
```

## Alternatives considered

- **Builder API (`Pipeline::new().add_node(...).add_edge(...)`)**  — more flexible but verbose; edges are declared separately from nodes, making it easy to forget one; no compile-time topology validation.
- **Config file / YAML / JSON graph definition** — accessible to non-Rust users but loses type safety, compile-time checking, and the ability to embed arbitrary Rust computation in nodes.
- **Functional composition (`let c = a.zip(b).map(|(a,b)| a+b)`)**  — natural Rust style but doesn't naturally express DAGs with shared upstream nodes or incremental recompute semantics.

## Consequences

**Positive:** topology errors (unknown node reference, type mismatch) caught at compile time; zero runtime overhead for edge lookup; familiar Rust syntax; single source of truth for both the DAG structure and the computation.

**Negative / trade-offs:** proc-macro complexity is high; macro error messages can be opaque; dynamic DAGs (topology determined at runtime) require a separate API path; the macro must be updated when new node types or execution modes are added.
