# Security Policy

## Supported Versions

| Version | Security fixes |
|---------|---------------|
| `main` (unreleased) | ✅ Active |

Once a stable release is published this table will be updated with a support window.

## Reporting a Vulnerability

**Please do not report security vulnerabilities via public GitHub issues.**

Open a [GitHub Security Advisory](https://github.com/your-org/vertexrs/security/advisories/new)
(private, visible only to maintainers) or email **security@vertexrs.io** (placeholder — update
before public launch).

Include as much of the following as possible:

- Type of issue (e.g. buffer overflow, injection, authentication bypass, information disclosure)
- File paths / crate names related to the issue
- Reproduction steps or a minimal proof-of-concept
- Potential impact assessment

## Response Timeline

| Stage | Target |
|---|---|
| Acknowledgement | 48 hours |
| Triage and severity assessment | 5 business days |
| Patch for Critical / High (CVSS ≥ 7.0) | 14 days |
| Patch for Medium (CVSS 4.0 – 6.9) | 60 days |
| CVE filing (CVSS ≥ 7.0) | Alongside patch release |

## Scope

In scope:
- Memory-safety issues in `unsafe` blocks (`vertexrs-core`, `vertexrs-exec`)
- Authentication or authorisation bypasses in the WebSocket bridge or user portal
- Injection vulnerabilities in `PipelineDefinition` deserialisation or the dynamic executor
- Supply chain issues in direct dependencies

Out of scope:
- Denial-of-service via computationally expensive pipelines (by design — users control their own pipelines)
- Issues in development tools (`criterion`, `polars` dev-deps)
- Social engineering

## Disclosure Policy

We follow coordinated disclosure. We will work with the reporter to agree a disclosure date
(typically the patch release date). We will credit reporters in the release notes unless they
prefer to remain anonymous.
