---
applyTo: "vertexrs-macro/src/**/*.rs"
---

# vertexrs-macro Crate — Doc Example Rules

`vertexrs-macro` is a proc-macro crate. `vertexrs` depends on it, so `vertexrs` **cannot** be added as a dev-dependency here (circular dep). This is the only accepted reason for using ` ```text ` in doc examples.

Rules:
- If an example can be made self-contained without importing `vertexrs` types, use ` ```rust ` and make it runnable
- If an example requires `Node`, `Frame`, or other `vertexrs` types and cannot be made self-contained, use ` ```text ` — but add a comment in the doc explaining why it is not runnable
- ` ```ignore ` and ` ```rust,ignore ` are still forbidden — use ` ```text ` for syntax-only snippets, not `ignore`
- `#[ignore]` on a **failing** test is forbidden — fix it
- `#[ignore]` is acceptable for intentionally slow tests
