---
description: "How to decompose a plan phase into GitHub issues. Apply when planning work or creating issues."
---

# Planning — Issue Decomposition

## Before creating any issues

Ask the following questions and wait for answers before proceeding:

1. Which phase of the build plan is the current focus? Read `docs/plans/main.md` (the index) to find the phase, then open the relevant `docs/plans/phase-XX-*.md` file. Confirm the most recent incomplete checkbox.
2. Are there already open issues covering this work? Run a search before creating duplicates.
3. Is the scope unambiguous enough to write concrete acceptance criteria? If not, resolve the ambiguity first.
4. Does the work touch both repos (`vertexrs` and `vertexrs-internal`)? If so, raise separate issues per repo.

Do not create a single issue until all four questions are answered.

## Issue sizing

Each issue must be implementable in a single PR:

- Target ≤ 400 LOC changed, excluding generated code and test fixtures
- One concern per issue — do not bundle unrelated changes
- If a phase task is too large, split it into ordered sub-issues; record the dependency in each issue body

## Required issue content

Every issue must include these sections (use the feature issue template):

1. **Phase reference** — link to the relevant checkbox in the appropriate `docs/plans/phase-XX-*.md` file
2. **Summary** — one paragraph; what this implements and why it matters
3. **Acceptance criteria** — behavioral specifications that the Implementer translates directly into tests. The Implementer writes a failing test for each AC *before* writing any implementation. Each criterion must:
   - Describe an observable outcome (return value, state change, error raised, property preserved) — not an implementation detail
   - Be specific enough that a test name and a single assertion can be written from it alone
   - Be verifiable by CI, a test assertion, or unambiguous code review — not by subjective judgment

   Minimum three criteria. A strong AC drives a test; a weak AC drives nothing. If you cannot write a test assertion from an AC, rewrite it.
4. **Affected crates** — `vertexrs`, `vertexrs-macro`, or both
5. **Relevant ADRs** — list any `docs/adr/` records that constrain the design
6. **Out of scope** — what this issue deliberately does not do

## Labels

Apply exactly one primary label: `enhancement`, `bug`, `refactor`, `docs`, `perf`.
Apply one phase label: `phase-1` through `phase-9` (public) or `phase-10`/`phase-11` (internal).

## Trivial vs non-trivial classification

The Planner classifies issues at creation time. Apply the `trivial` label (alongside `queued`) when **all** of the following are true:

- The change is contained within a single crate and a single module
- No new public API, trait, or type is introduced
- No ADR is needed
- The implementation approach is unambiguous from the acceptance criteria alone
- Estimated change is ≤ 50 LOC

If any criterion is not met, do not apply `trivial`. Non-trivial issues automatically trigger the Architect agent (via the `ready` label); the Implementer must not start until the Architect has posted a design and the human has set `design-approved` on the issue.

When in doubt, omit `trivial` — the Architect stage has low cost and high value.

## Plan maintenance

After creating issues, update the plan: add an inline `<!-- #N -->` annotation at the end of each corresponding checkbox in the relevant `docs/plans/phase-XX-*.md` file. Commit with message `plan: annotate issues for Phase X.Y`.
