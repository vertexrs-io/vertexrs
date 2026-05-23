# Agent Instructions

## Development workflow

The full cycle for any piece of work is:

```
Planner → ScrumMaster → (Architect) → Implementer → Reviewer → Human approves PR → Merge
```

The Architect step is **conditional** — only required when an issue needs a new ADR or touches more than one crate's public API. Simple issues skip it.

Each stage has a dedicated agent mode (`.github/agents/`):

| Stage | Agent | What it does |
|---|---|---|
| Plan | `Planner` | Collaborative thinking partner — works with the human through Q&A to define and refine `main.md`; never touches GitHub |
| Backlog | `ScrumMaster` | Converts a completed plan into well-formed GitHub issues; only this agent creates issues |
| Design *(conditional)* | `Architect` | Produces a technical design and/or ADR draft for complex issues; posts to the issue for human sign-off before implementation starts |
| Implement | `Implementer` | Picks up one issue, asks clarifying questions, writes code, runs CI, opens a PR |
| Review | `Reviewer` | Reads the PR diff, checks against instructions and ADRs, posts comments — never touches code |
| Approve | **Human** | Final approval and merge; agents never merge |

**Role boundaries are strict.** Only the Planner edits the build plan. Only the ScrumMaster creates GitHub issues. Only humans approve and merge PRs.

## Finding the current phase

The active build plan lives in `docs/plans/main.md`. Always read this first to find the current phase (most recent incomplete section) before planning or implementing anything.

## Architectural decisions

Core design decisions are recorded in `docs/adr/`. Read the relevant ADR(s) before implementing any feature — these record *why* things are the way they are and constrain the acceptable solution space.

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

## Before every commit

Run the full local CI gate and confirm it passes:

```bash
cargo make ci
```

This runs, in order: `check` → `fmt` → `lint` → `test` → `coverage` → `audit`.

Do **not** commit if any step fails. Fix the failure first, then re-run `cargo make ci` in full before committing.
