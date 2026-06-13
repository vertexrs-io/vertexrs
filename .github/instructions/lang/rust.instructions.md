---
applyTo: "**/*.rs"
paths:
  - "**/*.rs"
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

## Code Style

- Follow the [Rust Style Guide](https://doc.rust-lang.org/style-guide/); enforced by `rustfmt`
- Run `cargo fmt` before every commit; CI runs `cargo fmt --check`
- Run `cargo clippy -- -D warnings`; CI fails on any lint warning
- All public functions, types, traits, and modules must have `///` doc comments
- Non-obvious logic must have inline `//` comments explaining *why*, not *what*
- Keep functions short and single-purpose; prefer pure functions over side-effecting ones
- Use `#[must_use]` on any `Result` or value-returning function where ignoring the return is likely a bug
