# Developer Workflow

This project uses a label-driven pipeline where GitHub Actions and Copilot agents handle the design and implementation stages. Your role as a developer is to steer the agents at defined checkpoints — not to run them manually.

For the full technical detail (mermaid diagram, workflow files, agent descriptions) see [AGENTS.md](AGENTS.md).

---

## The pipeline at a glance

```
Plan  →  Queue  →  Schedule  →  Design  ⇄  Review  →  Implement  →  Code review  →  Merge
        (you)    (automatic)   (agent)    (you)      (agent)       (you)           (you)
```

1. **Plan** — use the Planner agent in VS Code to work through a planning session. It writes the phase files under `docs/plans/` and creates the GitHub issues automatically.
2. **Queue** — review the issues the Planner created. When you're happy with one, add the `awaiting-agent` label. That's your approval for it to enter the pipeline.
3. **Schedule** — `implementer-queue.yml` automatically promotes the oldest `awaiting-agent` issue to `ready` when a concurrency slot is free. You don't touch this.
4. **Design** — `architect.yml` fires on `ready` (non-trivial issues only). The Architect creates a `feat/*` branch, commits design docs to `docs/design/`, and opens a **draft PR** to `main`. It then sets `awaiting-design-approval` on the issue.
5. **Design review** — read the draft PR and/or the design summary comment on the issue. Leave feedback as a PR review or an issue comment. The Architect will revise and re-commit automatically (`architect-response.yml` handles PR reviews; `architect-comment-receive.yml` + `architect-comment-run.yml` handle issue comments). Repeat until you're satisfied, then set `design-approved` on the issue.
6. **Implement** — `implementer.yml` fires on `design-approved` (or on `ready` for trivial issues). The Implementer picks up the `feat/*` branch, writes the code, runs CI, and converts the draft PR to ready for review.
7. **Code review** — review the PR normally. Request changes if needed; `pr-response.yml` re-invokes the Implementer to address them. Approve and merge when done.

---

## Label reference

These are the only labels you set manually. Everything else is automated.

| Label | You set it when… | What happens next |
|---|---|---|
| `awaiting-agent` | You've reviewed the issue and approved it for the pipeline | Scheduler promotes it to `ready` when a slot is free |
| `design-approved` | You're happy with the Architect's design | Implementer is triggered automatically |

These labels are set by agents or automation — do not set them yourself:

| Label | Set by | Meaning |
|---|---|---|
| `queued` | Planner | Issue created; not yet reviewed by you |
| `trivial` | Planner | Small change; bypasses the Architect stage |
| `ready` | `implementer-queue.yml` | Slot granted; triggers Architect or Implementer |
| `awaiting-design-approval` | Architect | Design is ready for your review |

---

## Giving design feedback

While an issue has `awaiting-design-approval`, the Architect is listening. To request changes:

- **On the draft PR** — leave a review (any type, including just a comment). The Architect will update the design docs, re-commit, and reply summarising what changed.
- **On the issue** — post a comment. Same effect.

You can iterate as many times as you need. When you're satisfied, set `design-approved` on the issue to hand off to the Implementer.

---

## Branch naming

| Branch | Created by | Purpose |
|---|---|---|
| `feat/<issue-number>-<slug>` | Architect (or Implementer for trivial) | Feature branch; design docs + code; PR targets `main` |
| `plan/<description>` | Planner | Planning session; PR targets `planning` |

Never commit directly to `main` or `planning`.

---

## Trivial vs non-trivial issues

The Planner classifies issues at creation time. A **trivial** issue has the `trivial` label and skips the Architect stage entirely — the Implementer creates the branch and implements directly. Use trivial for: typo fixes, doc updates, single-function changes with no API impact.

---

## Concurrency limit

Only one issue moves through the pipeline at a time by default (controlled by the `IMPLEMENTER_CONCURRENCY_LIMIT` repository variable). A slot is occupied from the moment an issue receives `ready` until its `feat/*` PR is closed. If you have multiple `awaiting-agent` issues, they queue automatically — oldest first.
