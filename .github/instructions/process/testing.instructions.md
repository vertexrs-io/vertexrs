---
description: "Testing standards and coverage requirements. Apply when writing or reviewing tests."
---

# Testing Standards

## AC-driven testing

Every test must be traceable to an acceptance criterion in the linked issue. Before writing any test, ask: which AC does this verify?

- **Required:** tests that verify a stated acceptance criterion
- **Allowed:** tests for complex internal invariants not captured by an AC (e.g. memory-safety postconditions in `unsafe` blocks)
- **Not allowed:** tests added purely to reach the 90% coverage threshold with no AC justification

The 90% coverage target is a consequence of thorough AC-driven testing — not a goal to hit with trivial or meaningless assertions. If coverage falls below 90% after writing all AC tests, the ACs were incomplete and should be revisited with the Planner.

## Coverage requirement

Line coverage must be ≥ 90% across `vertexrs` and `vertexrs-macro`. Measure with:

```bash
cargo llvm-cov --all-features --lib
```

PRs that drop coverage below 90% must include written justification in the PR body.

## Test types and placement

| Type | When required | Where |
|---|---|---|
| Unit | All non-trivial functions | `#[cfg(test)]` module in the same file |
| Integration | New pipeline behaviours, macro-generated code paths | `vertexrs/tests/` |
| Doctests | All public API items with a runnable example | Inline in `///` doc comments |
| Benchmarks | Critical paths and hot recompute paths | `vertexrs/benches/` via `criterion` |

## Test rules

- Tests must be deterministic — no timing dependencies, no random seeds without a fixed value
- Use `assert_eq!` or `assert!(matches!(...))` with exact expected values
- Prefer explicit assertions over `unwrap()` when the `Result`/`Option` outcome is the behavior under test. In test setup, fixtures, and benchmark scaffolding, `unwrap()` is acceptable when failure should immediately fail the test; prefer `expect()` when a clearer failure message would help.
- Benchmarks with a Polars equivalent **must** include a `#[test]` correctness assertion: `abs(vtx − polars) < 1e-6` for f32/f64; exact equality for integers; f16 widened to f32 before comparison

## Adding a new benchmark file

When adding a new file under `vertexrs/benches/`, you **must** also add a corresponding `--bench <name>` entry to the `bench-save` task in `Makefile.toml`. Background: `cargo bench` always runs the lib test harness in bench mode; forwarding Criterion args (e.g. `--save-baseline`) to it causes an "Unrecognized option" error and exit 101. The `bench-save` task avoids this by naming each `[[bench]]` target explicitly, so new bench files are not picked up automatically.

## Forbidden patterns

- `#[ignore]` on a failing test — fix it or delete it
- `unwrap()` in library code
- Silencing errors with `let _ = ...` without a documented comment explaining why
