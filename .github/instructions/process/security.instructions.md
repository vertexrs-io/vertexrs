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
