# Agent Instructions

## Before every commit

Run the full local CI gate and confirm it passes:

```bash
cargo make ci
```

This runs, in order: `check` → `fmt` → `lint` → `test` → `coverage` → `audit`.

Do **not** commit if any step fails. Fix the failure first, then re-run `cargo make ci` in full before committing.

## Development workflow

```mermaid
flowchart TD
    H((Human)) -->|refine plan| PL[Planner\nVS Code]
    PL -->|commits to plan/* branch\nopens PR into planning| PR1[PR: plan/* → planning]
    PR1 -->|planning-sync.yml\nmerges main into planning| SYNC[Sync main → planning]
    SYNC --> PR1
    PL -->|creates issues\nlabels queued ± trivial\nannotates plan files| ISSUES[(GitHub Issues\nqueued ± trivial)]

    ISSUES -->|human reviews &\nsets awaiting-agent| AWAIT[awaiting-agent]

    AWAIT -->|implementer-queue.yml\nslot available?| SCHED{slot\nfree?}
    SCHED -->|no — remains awaiting-agent\nuntil next PR close| AWAIT
    SCHED -->|yes & trivial| READY[ready]
    SCHED -->|yes & non-trivial| AWD[awaiting-design]

    READY -->|implementer.yml triggers| IMP[Implementer\nClaude Code CLI]
    AWD -->|human runs Architect\nlocally, VS Code| AR[Architect\nlocal session]
    AR -->|creates feat/* branch\ncommits design docs\nopens draft PR to main\nposts design summary on issue| ADRAFT[draft PR +\ndesign summary]
    ADRAFT -->|human removes awaiting-design\nsets design-approved| DA[design-approved]
    DA -->|implementer.yml triggers| IMP

    IMP -->|picks up feat/* branch\nimplements, runs CI\nconverts draft PR to ready| PR2[PR: feat/* → main]

    PR2 --> REV[Reviewer\nClaude Code CLI]
    REV -->|posts review comments| PR2
    PR2 -->|human submits\nRequest Changes| RC[Review:\nchanges requested]
    RC -->|pr-response.yml\ntriggers on review| IMP2[Implementer\naddresses comments]
    IMP2 -->|pushes fixes\nto feat/ branch| PR2

    PR2 -->|human approves\nand merges| MAIN[(main)]
    MAIN -->|PR closed: slot freed\nimplementer-queue.yml fires| AWAIT

    style H fill:#f0f0f0,stroke:#666
    style MAIN fill:#d4edda,stroke:#28a745
    style ISSUES fill:#fff3cd,stroke:#ffc107
    style AWAIT fill:#fff3cd,stroke:#ffc107
    style READY fill:#cce5ff,stroke:#004085
    style AWD fill:#fff3cd,stroke:#ffc107
    style AR fill:#f0f0f0,stroke:#666
    style ADRAFT fill:#fff3cd,stroke:#ffc107
    style DA fill:#d4edda,stroke:#28a745
```

The Architect is **required for all non-trivial issues**. The Planner classifies each issue at creation time — trivial issues receive a `trivial` label alongside `queued` and bypass the Architect stage entirely.

Each stage has a dedicated agent persona (`.claude/agents/`):

| Stage | Agent | What it does |
|---|---|---|
| Plan + Backlog | `Planner` | Works with the human through Q&A to define and refine the plan; writes plan files; creates GitHub issues labelled `queued` (± `trivial`); annotates plan files with issue numbers; commits to a `plan/*` session branch; opens PR into `planning` |
| Schedule | Workflow | `implementer-queue.yml` — fires on `awaiting-agent` label and on `feat/*` PR close; promotes the oldest `awaiting-agent` issue to `ready` (trivial) or `awaiting-design` (non-trivial) when the slot count is below the limit |
| Design | `Architect` | Run **locally and interactively** by a human (like the Planner) against an `awaiting-design` issue; creates `feat/<issue-number>-<slug>` branch from `main`, commits design docs (`docs/design/`, `docs/adr/` if needed), opens a draft PR to `main`, posts a design summary on the issue. The human iterates live, then removes `awaiting-design` and sets `design-approved` themselves when satisfied |
| Implement | `Implementer` | Triggered by `ready` (trivial) or `design-approved` (non-trivial); picks up the existing `feat/*` branch (or creates it for trivial), implements, runs CI, converts draft PR to ready |
| Review | `Reviewer` | Reads the PR diff, checks against instructions and ADRs, posts comments — never touches code |
| Approve | **Human** | Final approval and merge; agents never merge |

**Role boundaries are strict.** Only the Planner edits the build plan and creates GitHub issues. Only the Scheduler sets the `ready` and `awaiting-design` labels. Only humans approve and merge PRs.

## Issue label pipeline

Issues move through a fixed sequence of labels. No agent or automation skips or reverses a state.

| Label | Set by | Meaning |
|---|---|---|
| `queued` | Planner | Issue created; not yet human-reviewed |
| `trivial` | Planner | Co-applied with `queued`; issue bypasses the Architect stage |
| `awaiting-agent` | Human | Reviewed and approved for implementation; waiting for a concurrency slot |
| `ready` | Scheduler | Slot granted (trivial issue only); triggers Implementer automatically |
| `awaiting-design` | Scheduler | Slot granted (non-trivial issue); signals a human to run the Architect locally — nothing fires automatically |
| `design-approved` | Human | Architect session complete; triggers Implementer |

### Concurrency limit

`implementer-queue.yml` enforces a cap on the number of concurrently in-flight issues. The limit is stored as the `IMPLEMENTER_CONCURRENCY_LIMIT` GitHub Actions repository variable (default: `1`).

A slot is **in use** when an open issue holds the label `ready`, `awaiting-design`, or `design-approved`, or has an associated open `feat/*` PR. The scheduler counts all such slots before promoting the next `awaiting-agent` issue.

When a `feat/*` PR is closed (merged or abandoned) the workflow re-evaluates the slot count and promotes the oldest `awaiting-agent` issue if a slot has freed up.

`pr-response.yml` (addressing review comments on an already-open PR) is exempt from the slot limit — it never opens a new PR.

## Branch structure

| Branch | Purpose |
|---|---|
| `main` | Stable, releasable code. Feature code, design docs, and ADRs all land here via `feat/*` PRs. |
| `planning` | Long-lived. Holds only `docs/plans/` files. Never merged to `main` on a schedule — only at deliberate phase checkpoints. Regularly synced FROM `main` via an automated workflow. |
| `plan/<description>` | Short-lived session branches created from `planning`. Planner works here. Merged into `planning` via PR. Deleted after merge. |
| `feat/<issue-number>-<slug>` | Short-lived implementation branches created from `main`. Architect seeds with design docs; Implementer adds the code. Merged into `main` via PR. |

Human-driven branches outside this pipeline (no associated issue) follow `<type>/<description>` using conventional-commit prefixes — `feat`, `fix`, `chore`, `docs`, `ci`, etc.

## Every PR closes an issue

Every PR opened in this repository must include a `Closes #N` line in its body so the GitHub "Linked Issues" UI ties the PR back to a tracked unit of work. Referencing the issue only in the PR title (e.g. `(#N)`) is **not** sufficient — it does not auto-close.

| PR type | Branch | Linked issue |
|---|---|---|
| Implementation PR (trivial) | `feat/<issue>-<slug>` → `main` | The implementing issue (the one with `ready` + `trivial`) |
| Implementation PR (non-trivial) | `feat/<issue>-<slug>` → `main` | The implementing issue (the one the Architect designed, with `design-approved`) |
| Architect draft PR | `feat/<issue>-<slug>` → `main` | The same issue — the Architect's draft and the Implementer's PR are the **same PR**, started as draft and converted to ready |
| Plan PR | `plan/<slug>` → `planning` | A **tracking issue** filed before the planning session begins |
| Chore PR (workflow, docs, labels) | `chore/<slug>` → `main` | A tracking issue filed before the work begins |

If no existing issue fits, create a tracking issue first, then open the PR. Do not open a PR without one.

## Keeping `planning` in sync with `main`

`.github/workflows/planning-sync.yml` runs on every PR opened against `planning` and merges `main` in automatically. This keeps the plan files up to date with the latest codebase state so the Planner always works with current context.

## Finding the current phase

The build plan is split by phase under `docs/plans/`. The index lives at `docs/plans/main.md` — read it first to see the phase list and current status, then open the relevant phase file (e.g. `docs/plans/phase-02-macro-system.md`) to find the current incomplete section. Always identify the current phase before planning or implementing anything.

## Architectural decisions

Core design decisions are recorded in `docs/adr/`. Read the relevant ADR(s) before implementing any feature — these record *why* things are the way they are and constrain the acceptable solution space.

**ADRs are immutable.** Once accepted, the body of an ADR is never edited. If a decision changes, create a new ADR that supersedes the old one, update the old ADR's status field to `Superseded by ADR-XXXX`, and link forward to the new record.

## Instruction files

| Path | Scope |
|---|---|
| `.github/instructions/lang/rust.instructions.md` | All `.rs` files |
| `.github/instructions/modules/vertexrs.instructions.md` | `vertexrs/src/**/*.rs` |
| `.github/instructions/modules/vertexrs-macro.instructions.md` | `vertexrs-macro/src/**/*.rs` |
| `.github/instructions/process/planning.instructions.md` | Creating issues |
| `.github/instructions/process/testing.instructions.md` | Writing/reviewing tests |
| `.github/instructions/process/benchmarking.instructions.md` | Writing/reviewing benchmarks |
| `.github/instructions/process/security.instructions.md` | Security-sensitive code |
| `.github/instructions/process/pr-review.instructions.md` | Reviewing PRs |

The three path-scoped files above (`lang/rust`, `modules/vertexrs`, `modules/vertexrs-macro`) carry both an `applyTo` key (for GitHub Copilot) and a `paths` key (for Claude Code's [`.claude/rules/`](https://code.claude.com/docs/en/memory#path-specific-rules)) in their frontmatter, and are symlinked into `.claude/rules/` under matching names. Edit the canonical file under `.github/instructions/`, not the symlink — both tools read the same source.

The five `process/*.instructions.md` files each carry a `name` key matching a [Claude Code skill](https://code.claude.com/docs/en/skills) (`planning-rules`, `testing-standards`, `benchmarking-standards`, `security-checklist`, `pr-review-checklist`), and are symlinked into `.claude/skills/<name>/SKILL.md`. Relevant agent personas preload the matching skill via the `skills:` frontmatter field (see `.claude/agents/*.md`). As above, edit the canonical file under `.github/instructions/`, not the symlink.
