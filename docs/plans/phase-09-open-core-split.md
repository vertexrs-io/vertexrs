[← Phase Index](main.md)

## Phase 9 — Open-Core Repository Split

**Goal:** Cleanly separate the open-source core from the commercial enterprise layer before any enterprise features are built, so the boundary is intentional and stable rather than retrofitted.

**Rationale:** Adding enterprise features to the same repo and then trying to extract them later always produces a messy fork. Do the split first, while the codebase is small and the API surface is clear.

### 9.1 Freeze the Public API Boundary

- [ ] Audit every public type and trait in `vertexrs-core`, `vertexrs-dag`, `vertexrs-exec`, `vertexrs-macro` — these form the stable API that enterprise plugins compile against
- [ ] Define a `vertexrs-plugin` trait crate: `ExecutorPlugin`, `SourceConnector`, `SinkConnector`, `AuthProvider` — extension points the enterprise layer hooks into without forking the core
- [ ] Add semver stability guarantees (`#[non_exhaustive]` on all public enums, `#[must_use]` audit)
- [ ] Document the plugin API in `vertexrs-plugin/README.md` with a worked example connector

### 9.2 Repo Topology

- [ ] **`vertexrs` (public, MIT)** — `vertexrs-core`, `vertexrs-dag`, `vertexrs-exec`, `vertexrs-macro`, `vertexrs-stream`, `vertexrs-dist`, `vertexrs-py`, `vertexrs-plugin`, `vertexrs-server`, `vertexrs-studio`
- [ ] **`vertexrs-enterprise` (private)** — imports `vertexrs` as a dependency; adds enterprise-only crates listed in Phase 10
- [ ] CI for the public repo must not depend on anything in the private repo
- [ ] Publish `vertexrs` and `vertexrs-plugin` to crates.io; enterprise crates are never published

### 9.3 Licensing

- [ ] `vertexrs` (core + macro + stream + dist + py + server) — **MIT**
- [ ] `vertexrs-studio` (WASM GUI) — **BSL 1.1**, converting to Apache 2.0 after 4 years; standard open-core protection while the product matures
- [ ] `vertexrs-enterprise` — **proprietary**, commercial license required
- [ ] Add `LICENSE`, `LICENSE-BSL`, and `COMMERCIAL.md` files explaining the split clearly
- [ ] Contributor License Agreement (CLA) required for contributions to Studio and enterprise crates

> **Enterprise and hosted cloud roadmap (Phases 10–11):** See `vertexrs-internal/.copilot/strategy/plan.md`.

