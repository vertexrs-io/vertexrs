---
description: "PR review checklist. Apply when reviewing a pull request."
---

# PR Review Checklist

## Before leaving comments, check

1. Read the linked issue and confirm all acceptance criteria are addressed
2. Read the relevant ADRs in `docs/adr/` — verify the implementation doesn't contradict them
3. Check CI status — all steps (`check`, `fmt`, `lint`, `test`, `coverage`, `audit`) must be green
4. Verify test coverage has not dropped below 90%

## Code correctness

- [ ] Logic matches the acceptance criteria in the issue
- [ ] No `unwrap()` in library code
- [ ] No silent error suppression (`let _ = ...`) without a comment
- [ ] `unsafe` blocks have `// SAFETY:` comments and do not cross public API boundaries
- [ ] New public API items have `///` doc comments with a runnable example

## Architecture alignment

- [ ] Implementation is consistent with the relevant ADRs
- [ ] No new runtime dependencies added without a justification comment in `Cargo.toml`
- [ ] Macro-generated code follows the conventions in `vertexrs-macro.instructions.md`

## Tests

- [ ] Unit tests cover the new logic
- [ ] Integration tests added if the change affects macro-generated code paths
- [ ] Benchmarks added or updated if the change touches the hot recompute path
- [ ] No tests marked `#[ignore]` because they are failing

## Review behaviour

- Post comments on specific lines where issues are found
- Use "Request changes" if any acceptance criterion is unmet or CI is failing
- Use "Approve" only when all checklist items pass
- Never modify the code yourself — only comment
