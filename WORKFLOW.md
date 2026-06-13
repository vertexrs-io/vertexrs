# Developer Workflow

This project uses a label-driven pipeline where GitHub Actions and Claude Code agents handle the implementation and review stages, and the Architect design stage is run locally and interactively by you. Your role as a developer is to steer the agents at defined checkpoints, and to run the Architect session yourself for non-trivial issues.

For the full technical detail (mermaid diagram, workflow files, agent descriptions) see [AGENTS.md](AGENTS.md).

---

## The pipeline at a glance

```
Plan  →  Queue  →  Schedule  →  Design   →  Implement  →  Code review  →  Merge
        (you)    (automatic)   (you, local)  (agent)       (you)          (you)
```

1. **Plan** — use the Planner agent in VS Code to work through a planning session. It writes the phase files under `docs/plans/` and creates the GitHub issues automatically.
2. **Queue** — review the issues the Planner created. When you're happy with one, add the `awaiting-agent` label. That's your approval for it to enter the pipeline.
3. **Schedule** — `implementer-queue.yml` automatically promotes the oldest `awaiting-agent` issue to `ready` when a concurrency slot is free. You don't touch this.
4. **Design** (non-trivial issues only) — when an issue carries `ready` without `trivial`, run the Architect agent **locally and interactively** in VS Code against that issue. It creates a `feat/<issue-number>-<slug>` branch from `main`, commits design docs to `docs/design/` (and `docs/adr/` if needed), opens a **draft PR** to `main`, and posts a design summary on the issue. Iterate live with the Architect until you're satisfied, then remove `ready` and set `design-approved` yourself, in the same session. Trivial issues (`ready` + `trivial`) skip this step entirely.
5. **Implement** — `implementer.yml` fires on `design-approved` (non-trivial) or on `ready` + `trivial` (trivial). The Implementer picks up the `feat/*` branch (created by the Architect, or by itself for trivial issues), writes the code, runs CI, and converts the draft PR to ready for review.
6. **Code review** — review the PR normally. Request changes if needed; `pr-response.yml` re-invokes the Implementer to address them. Approve and merge when done.

---

## Label reference

These are the only labels you set manually. Everything else is automated.

| Label | You set it when… | What happens next |
|---|---|---|
| `awaiting-agent` | You've reviewed the issue and approved it for the pipeline | Scheduler promotes it to `ready` when a slot is free |
| `design-approved` | You're happy with the Architect's design (non-trivial issues; you run the Architect locally first) | Implementer is triggered automatically |

These labels are set by agents or automation — do not set them yourself:

| Label | Set by | Meaning |
|---|---|---|
| `queued` | Planner | Issue created; not yet reviewed by you |
| `trivial` | Planner | Small change; bypasses the Architect stage |
| `ready` | `implementer-queue.yml` | Slot granted. For trivial issues, triggers the Implementer directly. For non-trivial issues, signals that you should run the Architect locally |

---

## Running the Architect locally

For any `ready`, non-trivial issue, open a session with the Architect agent in VS Code (same as the Planner) and work through the design live:

- The Architect reads the issue, ADRs, and existing code, and proposes a design
- You react, ask for changes, and refine — there's no fixed number of rounds
- When you're both happy, the Architect commits the design docs to `feat/<issue-number>-<slug>`, opens a draft PR to `main`, and posts a summary comment on the issue
- **You** then remove `ready` and set `design-approved` on the issue, in the same session, to hand off to the Implementer

---

## Branch naming

| Branch | Created by | Purpose |
|---|---|---|
| `feat/<issue-number>-<slug>` | Architect (non-trivial) or Implementer (trivial) | Feature branch; design docs + code; PR targets `main` |
| `plan/<description>` | Planner | Planning session; PR targets `planning` |

Never commit directly to `main` or `planning`.

---

## Trivial vs non-trivial issues

The Planner classifies issues at creation time. A **trivial** issue has the `trivial` label and skips the Architect stage entirely — the Implementer creates the branch and implements directly. Use trivial for: typo fixes, doc updates, single-function changes with no API impact.

---

## Concurrency limit

Only one issue moves through the pipeline at a time by default (controlled by the `IMPLEMENTER_CONCURRENCY_LIMIT` repository variable). A slot is occupied from the moment an issue receives `ready` (or `design-approved`) until its `feat/*` PR is closed. If you have multiple `awaiting-agent` issues, they queue automatically — oldest first.
