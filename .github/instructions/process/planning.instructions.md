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
   - Be relevant to the feature — not generic boilerplate like "the code should compile" or "coverage should be maintained"

   Minimum three criteria. A strong AC drives a test; a weak AC drives nothing. If you cannot write a test assertion from an AC, rewrite it.

   **Good AC:** `Given a ChunkedColumn with dirty chunks at [0, 2], when recompute() is called, only chunks 0 and 2 are passed to the kernel; chunk 1 is not recomputed.`
   **Bad AC:** `The dirty-chunk tracking logic should be correct.`

   A good issue with three strong ACs is better than one with ten vague ones.
4. **Affected crates** — `vertexrs`, `vertexrs-macro`, or both
5. **Relevant ADRs** — list any `docs/adr/` records that constrain the design
6. **Out of scope** — what this issue deliberately does not do

## Labels

Every issue created by the Planner must carry **all three** of:

- One primary label: `enhancement`, `bug`, `refactor`, `docs`, `perf`
- One phase label: `phase-1` through `phase-9` (public) or `phase-10`/`phase-11` (internal)
- The lifecycle entry label: `queued`

Apply `trivial` additionally when the trivial criteria below are met.

## Label lifecycle

Every issue moves through this sequence; each transition is gated:

| State | Set by | Means |
|---|---|---|
| `queued` | Planner (at creation) | Issue exists but no agent should act yet |
| `awaiting-agent` | **Human** (manual) | Human has approved the issue for agent execution |
| `ready` | Queue bot (`implementer-queue.yml`) | A slot is free. Trivial issues trigger the Implementer via CI; non-trivial issues need a human to run the Architect locally before setting `design-approved` |
| `design-approved` | **Human** (manual) | Design complete (for non-trivial issues, produced via a local Architect session); Implementer may now build the design |

Notes:
- `awaiting-agent` and `design-approved` are the only transitions that require a human; agents must never apply these
- `ready` is bot-applied so the Implementer workflow can distinguish bot-driven promotion (trivial path) from manual labelling. For non-trivial issues, `ready` simply signals that a human should run the Architect locally
- The label sequence is the source of truth for which agent runs next — workflows gate on both the label and the `sender.type` / `sender.login` of the change

## Trivial vs non-trivial classification

The Planner classifies issues at creation time. Apply the `trivial` label (alongside `queued`) when **all** of the following are true:

- The change is contained within a single crate and a single module
- No new public API, trait, or type is introduced
- No ADR is needed
- The implementation approach is unambiguous from the acceptance criteria alone
- Estimated change is ≤ 50 LOC

If any criterion is not met, do not apply `trivial`. Non-trivial issues require a human to run a local Architect session (against the `ready` issue, on the `feat/<issue-number>-<slug>` branch) before setting `design-approved`; the Implementer must not start until then.

When in doubt, omit `trivial` — the Architect stage has low cost and high value, even run locally.

## Plan maintenance

After creating issues, update the plan: add an inline `<!-- #N -->` annotation at the end of each corresponding checkbox in the relevant `docs/plans/phase-XX-*.md` file. Commit with message `plan: annotate issues for Phase X.Y`.
