# Archive

Historical planning notes, spikes, shipped playbooks, and superseded specs.
**May be wrong** relative to current `main` — use living docs instead:

| Need | Living doc |
|------|------------|
| Dev workflow & E2E | [`../dev/DEV_GUIDE.md`](../dev/DEV_GUIDE.md) |
| Platform setup | [`../dev/INSTALL_*.md`](../dev/) |
| Release process | [`../dev/RELEASE.md`](../dev/RELEASE.md) |
| Runtime architecture | [`../architecture/ARCHITECTURE.md`](../architecture/ARCHITECTURE.md) |
| Accepted decisions | [`../architecture/ADR-*.md`](../architecture/) |
| Proposed decisions | [`../architecture/proposals/`](../architecture/proposals/) |

## Contents

| File | Was | Superseded by |
|------|-----|---------------|
| `THINKING_REFACTOR.md` | Thinking-layer execution playbook | ADR-007 |
| `OMLX_SHARED_SERVER_FINDINGS.md` | oMLX Phase 0 spike | ADR-006 |
| `plan-v0.2.0-cuda-mtp.md` | v0.2.0 execution tracker | ADR-004, ADR-005, shipped code |
| `plan-v0.2.1-vlm-mtp-cuda13.md` | v0.2.1 planning | `multi_model_e2e`, ADRs |
| `test-v0.2.0-post-tarball.md` | One-off tarball checklist | CI + `multi_model_e2e` |
| `engine-revamp-plan.md` | Engine tier revamp phases | ADR-001, install guides |
| `engine-research-sm120.md` | sm_120 / SGLang reproduction | ADR-001 § Context |
| `implementation-plan-omlx-adapter.md` | Early adapter brainstorm | ARCHITECTURE.md |
| `LMForge_SRS-v0.2-draft.md` | Full SRS (2026-03) | ARCHITECTURE.md + ADRs |
| `v0.1-walkthrough.md` | v0.1 usage walkthrough | ARCHITECTURE.md |
