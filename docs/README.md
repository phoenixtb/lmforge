# LMForge documentation

| Audience | Start here |
|----------|------------|
| **Contributors** | [Dev guide](./dev/DEV_GUIDE.md) → platform [install](./dev/) → [release](./dev/RELEASE.md) |
| **Architecture** | [ARCHITECTURE.md](./architecture/ARCHITECTURE.md) + [ADRs](./architecture/) |
| **Engine ops** | [R2 assets](./engineering/R2-ENGINE-ASSETS.md) · [Annual review](./engineering/ANNUAL_ENGINE_REVIEW.md) |
| **Product / sales** | [product-overview.md](./product-overview.md) |
| **Historical** | [archive/](./archive/) (may be stale) |

## Dev (`docs/dev/`)

| Doc | Purpose |
|-----|---------|
| [DEV_GUIDE.md](./dev/DEV_GUIDE.md) | Mother scripts (`lmforge.sh` / `lmforge.ps1`), E2E tiers, env vars |
| [INSTALL_LINUX.md](./dev/INSTALL_LINUX.md) | Linux + NVIDIA dev setup |
| [INSTALL_MACOS.md](./dev/INSTALL_MACOS.md) | Apple Silicon dev setup |
| [INSTALL_WINDOWS.md](./dev/INSTALL_WINDOWS.md) | Native Windows + WSL2 |
| [RELEASE.md](./dev/RELEASE.md) | Tag → draft → publish workflow |

## Architecture (`docs/architecture/`)

| Doc | Topic |
|-----|-------|
| [ARCHITECTURE.md](./architecture/ARCHITECTURE.md) | Runtime diagram (daemon, engines, API) |
| [ADR-001](./architecture/ADR-001-engine-tiers.md) | Engine tier model |
| [ADR-002](./architecture/ADR-002-engines-endpoint.md) | `/lf/engines` endpoint |
| [ADR-003](./architecture/ADR-003-last-errors-surface.md) | `last_errors` failure surface |
| [ADR-004](./architecture/ADR-004-cuda-variant-pipeline.md) | CUDA variant pipeline |
| [ADR-005](./architecture/ADR-005-speculative-decoding.md) | MTP / speculative decoding |
| [ADR-006](./architecture/ADR-006-engine-residency.md) | Engine residency (SharedServer vs ProcessPool) |
| [ADR-007](./architecture/ADR-007-thinking-pipeline.md) | Thinking pipeline (adapters, orchestrator, fixes) |

## Engineering (`docs/engineering/`)

| Doc | Purpose |
|-----|---------|
| [R2-ENGINE-ASSETS.md](./engineering/R2-ENGINE-ASSETS.md) | llama.cpp CUDA tarball CDN (R2) |
| [ANNUAL_ENGINE_REVIEW.md](./engineering/ANNUAL_ENGINE_REVIEW.md) | Yearly upstream re-check checklist |
