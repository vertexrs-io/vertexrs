---
description: "Security review checklist. Apply when writing or reviewing code that touches public APIs, network, or unsafe."
---

# Security Review

## Unsafe code

- Permitted only in performance-critical hot paths where safe alternatives have measurably worse throughput
- Every `unsafe` block **must** be preceded by a `// SAFETY:` comment explaining soundness
- `unsafe` must never cross a public API boundary — wrap in a safe public function
- Prefer `bytemuck` for transmute-like operations over raw pointer casts
- All `unsafe` code must have tests that would catch unsound behaviour (length mismatches, alignment violations)

## Input validation

- Validate all inputs at system boundaries: public API entry points, deserialised data, network messages
- Do not re-validate internal invariants already enforced by the type system
- Never interpolate user-controlled strings into SQL, shell commands, or file paths without sanitisation

## Dependencies

- Run `cargo audit` — CI blocks on any RUSTSEC advisory ≥ Medium
- Every new runtime dependency requires a justification comment in `Cargo.toml`
- Prefer `std` and the Arrow ecosystem; avoid general-purpose alternatives
- Review changelogs before `cargo update` on security-sensitive crates (`ring`, `rustls`, `axum`, `tokio`)

## Network-facing code (Phase 8+)

- All endpoints require authentication except `/health` and `/metrics`
- Use short-lived JWTs (≤ 1h expiry) for API and WebSocket auth
- TLS 1.2+ on all network connections; use `rustls` over OpenSSL
- User pipeline data must never appear in logs, error messages, or telemetry

## Secrets

- Never hardcode secrets; pass via environment variables, Kubernetes Secrets, or Vault
- Secrets must never appear in `PipelineDefinition` JSON or any serialised form

## GitHub Actions / workflow security

These rules apply to any file under `.github/workflows/`. The Security agent must apply them whenever a workflow file is added or modified.

### Triggers and privilege

- `pull_request` from forks runs with read-only `GITHUB_TOKEN` and **no secrets** — generally safe; not a sink for sensitive data
- `pull_request_target`, `issue_comment`, `pull_request_review`, `workflow_run` run with **write tokens and secrets** in the base-repo context — every input must be treated as untrusted until validated
- Never introduce `pull_request_target` without the two-workflow + artifact + API-refetch pattern documented in `architect-comment-receive.yml` / `architect-comment-run.yml`

### Trusting the caller

- Every privileged workflow that ingests user-supplied content must gate on `author_association ∈ {OWNER, MEMBER, COLLABORATOR}`
- Trigger-time gates check the triggering actor; values resolved later (e.g. "latest comment", "all unresolved threads") may be from a different, untrusted actor — re-verify before use
- GitHub usernames interpolated into agent prompts must be regex-validated against `^[A-Za-z0-9-]{1,39}$`

### Checkout safety

- `actions/checkout` `ref:` must be either a known-trusted constant (`main`, `planning`), an immutable SHA from `github.event.pull_request.head.sha`, or omitted (so it resolves to `github.sha`)
- Never pass a branch name resolved from an `issue_comment` or `pull_request_review` payload as a checkout `ref:` — use the two-workflow pattern, capture a SHA via the API, and validate against `^[0-9a-f]{40}$`
- After checkout, switch branches with plain `git fetch` / `git checkout <SHA>` — pin to the SHA, not the branch tip
- Filter PR lookups by `head.repo.full_name == github.repository` to prevent fork branches with colliding names from being selected

### Shell hygiene

- Never use `${{ github.event.* }}` directly inside a `run:` block. Bind to an `env:` entry first; bash never word-splits env vars
- Validate integer-only inputs with `[[ "$VAR" =~ ^[0-9]+$ ]]` before use
- Validate SHAs with `[[ "$SHA" =~ ^[0-9a-f]{40}$ ]]` before use

### Agent prompt content

- Agent prompts must NOT include "read all comments", "read all reviews", or "read all threads" — these ingest content from any thread participant. Pin to a specific comment ID or review ID captured from the trigger event and re-fetched via the API
- The triggering issue's author must be a trusted member before any agent reads its body
- Any login, branch name, or other string interpolated into a prompt must be regex-validated

### Pre-filtering comments at the workflow level

For agents that need broad comment context (e.g. an initial Architect or Implementer reading prior discussion), the agent prompt's compliance is **not a security boundary** — with `--allow-all-tools` the agent can call `gh` itself. The workflow must materialise the trusted set as a file and instruct the agent to use only that file:

1. Add a step that calls `gh api repos/$REPO/issues/$N/comments --paginate --jq '[.[] | select(.user.type == "Bot" or (.author_association | IN("OWNER","MEMBER","COLLABORATOR")))]'` and writes the output to `./trusted-comments.json`
2. The agent prompt must (a) point at the file, (b) explicitly forbid `gh api .../comments`, `gh issue view --comments`, or any equivalent direct fetch
3. The trusted set is: `user.type == "Bot"` OR `author_association ∈ {OWNER, MEMBER, COLLABORATOR}`. `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, `FIRST_TIMER`, `MANNEQUIN`, and `NONE` are excluded
4. Log `kept N of M comments` for auditability

This is defence-in-depth — the agent file rule alone is documentation, not enforcement; the pre-filter file is the actual data the agent sees.

### Artifact handling (workflow_run pattern)

- Only opaque integer lookup keys (issue/comment/PR IDs) may be persisted to an artifact for a privileged consumer
- The privileged consumer must (a) regex-validate the integer, (b) re-fetch the canonical data from the GitHub API using that ID, (c) cross-check the fetched object's referent (e.g. comment's `issue_url`) matches the artifact's other IDs
- Never let an artifact-carried string flow into a checkout `ref:`, a shell command, or an agent prompt
- Extract artifacts into `RUNNER_TEMP`, not the workspace; use `unzip -j` to flatten paths (blocks zip-slip)

### Concurrency

- Any workflow that pushes commits, manages a queue, or merges branches must declare a `concurrency:` block. Racing runs can lose work or exceed configured limits
- Per-PR / per-issue groups for agent workflows (no cancel) so a second trigger waits rather than collides
- Global group for shared-resource workflows (queue manager, branch syncer)
- `cancel-in-progress: true` is appropriate for stateless CI / scan workflows where the latest push supersedes
