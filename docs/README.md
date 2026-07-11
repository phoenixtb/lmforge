# LMForge documentation

| Audience | Start here |
|----------|------------|
| **Users** | Root [`README.md`](../README.md) |
| **Contributors** | [Dev guide](./dev/DEV_GUIDE.md) → platform [install](./dev/) → [Accepted ADRs](#architecture-accepted) |
| **Maintainers** | [Release](./dev/RELEASE.md) · [engineering/](./engineering/) (CDN, engine review) |
| **Historical** | [archive/](./archive/) (stale; not linked as current) |

Proposed ADRs live under [`architecture/proposals/`](./architecture/proposals/) until Accepted.

---

## Contributor (`docs/dev/`)

| Doc | Purpose |
|-----|---------|
| [DEV_GUIDE.md](./dev/DEV_GUIDE.md) | Mother scripts (`lmforge.sh` / `lmforge.ps1`), E2E tiers, env vars |
| [INSTALL_LINUX.md](./dev/INSTALL_LINUX.md) | Linux + NVIDIA dev setup |
| [INSTALL_MACOS.md](./dev/INSTALL_MACOS.md) | Apple Silicon dev setup |
| [INSTALL_WINDOWS.md](./dev/INSTALL_WINDOWS.md) | Native Windows + WSL2 |
| [RELEASE.md](./dev/RELEASE.md) | Tag → draft → publish (maintainer) |

---

## Architecture (Accepted)

| Doc | Status | Topic |
|-----|--------|-------|
| [ARCHITECTURE.md](./architecture/ARCHITECTURE.md) | Living | Runtime diagram (daemon, engines, API) |
| [ADR-001](./architecture/ADR-001-engine-tiers.md) | Accepted | Engine tier model |
| [ADR-002](./architecture/ADR-002-engines-endpoint.md) | Accepted | `/lf/engines` endpoint |
| [ADR-003](./architecture/ADR-003-last-errors-surface.md) | Accepted | `last_errors` failure surface |
| [ADR-004](./architecture/ADR-004-cuda-variant-pipeline.md) | Accepted | CUDA variant pipeline |
| [ADR-005](./architecture/ADR-005-speculative-decoding.md) | Accepted | MTP / speculative decoding |
| [ADR-006](./architecture/ADR-006-engine-residency.md) | Accepted | Engine residency (SharedServer vs ProcessPool) |
| [ADR-007](./architecture/ADR-007-thinking-pipeline.md) | Accepted | Thinking pipeline |
| [ADR-008](./architecture/ADR-008-pool-residency.md) | Accepted | Pool residency + validation matrix |

### Proposals (not Accepted)

| Doc | Status | Topic |
|-----|--------|-------|
| [ADR-009](./architecture/proposals/ADR-009-low-vram-discrete-admission.md) | Proposed | Low-VRAM discrete GPU admission & offload |

---

## Maintainer (`docs/engineering/`)

Ops docs for people who publish engine assets or own the engine roster. Not required for contributing code or using LMForge.

| Doc | Purpose |
|-----|---------|
| [R2-ENGINE-ASSETS.md](./engineering/R2-ENGINE-ASSETS.md) | llama.cpp CUDA tarball CDN (R2) |
| [ANNUAL_ENGINE_REVIEW.md](./engineering/ANNUAL_ENGINE_REVIEW.md) | Yearly upstream re-check checklist |

---

## Also

| Doc | Notes |
|-----|-------|
| [product-overview.md](./product-overview.md) | Product / sales narrative (overlaps root README) |
| [postman/](./postman/) | Postman collection + env for the HTTP API |
| [archive/](./archive/) | Superseded plans, spikes, playbooks |
