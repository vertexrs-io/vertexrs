---
applyTo: "vertexrs/src/**/*.rs"
---

# vertexrs Crate — Doc Example Rules

All doc examples must be compilable, runnable Rust. No exceptions.

- ` ```rust ` only — ` ```text `, ` ```ignore `, and ` ```rust,ignore ` are forbidden
- Every example must pass `cargo test --doc`
- `vertexrs-macro` is a dependency of this crate, so all macros (`pipeline!`, `node!`, etc.) are available via `use vertexrs::{pipeline, node, ...}`
- There is no circular-dependency excuse here — fix broken examples, do not suppress them
