---
applyTo: "**/*.rs"
---

# Rust Code Conventions

## Doc Examples

All doc examples must be compilable, runnable Rust:

- Use ` ```rust ` fences — never ` ```rust,ignore ` or bare ` ```ignore `
- Every example must compile and pass as a doctest (`cargo test --doc`)
- If an example fails to compile, **fix the example** — do not mark it `ignore`

See crate-specific instruction files for any narrow exceptions.

## Tests

- Never mark a test `#[ignore]` because it is **failing** — fix it or delete it
- `#[ignore]` is acceptable for tests that are intentionally slow; run them with `cargo test -- --include-ignored` or `cargo make test-slow`
- Slow tests that don't require unit-test proximity may be moved to `vertexrs/tests/` instead
