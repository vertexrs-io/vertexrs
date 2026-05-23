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
3. **Acceptance criteria** — testable bullet points checkable by CI or code review; minimum three criteria
4. **Affected crates** — `vertexrs`, `vertexrs-macro`, or both
5. **Relevant ADRs** — list any `docs/adr/` records that constrain the design
6. **Out of scope** — what this issue deliberately does not do

## Labels

Apply exactly one primary label: `enhancement`, `bug`, `refactor`, `docs`, `perf`.
Apply one phase label: `phase-1` through `phase-9` (public) or `phase-10`/`phase-11` (internal).

## Design step trigger

After an issue is created, decide whether it needs a design pass before implementation. The Architect agent is required when **any** of the following apply:

- The issue would require a new ADR (a non-obvious technical decision that constrains future work)
- Changes touch more than one crate's public API
- The implementation approach is not obvious from the acceptance criteria alone

Mark issues that require design with the label `needs-design`. The Implementer must not start until the Architect has posted a design and the human has approved it.

Simple, self-contained issues skip the design step and go directly to the Implementer.

## Plan maintenance

After creating issues, update the plan: add an issue reference next to each corresponding checkbox in `main.md`.
