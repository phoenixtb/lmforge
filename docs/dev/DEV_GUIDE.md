# LMForge — Dev Guide (scripts & E2E)

Day-to-day tooling for contributors: interactive menus, test tiers, and env
knobs. Platform setup lives in the install guides; release cutting in
[RELEASE.md](./RELEASE.md).

Quick command reference: `scripts/util/cheat-sheet`.

---

## Mother scripts

Interactive menus dispatch to `scripts/util/` and `tests/`. Run from repo root.

| Platform | Interactive | Non-interactive |
|----------|-------------|-----------------|
| macOS / Linux | `./scripts/lmforge.sh` | `./scripts/lmforge.sh <action> [args…]` |
| Windows | `powershell -File scripts\lmforge.ps1` | `powershell -File scripts\lmforge.ps1 <action> [-KeepInstall] [-Version vX.Y.Z]` |

### Action map

| Action | Script / command | Unix | Win |
|--------|------------------|:----:|:---:|
| `status` | `dev_status.sh` (or inline health check) | ✓ | ✓ |
| `dev-reinstall-core` | `dev-reinstall-core.sh` | ✓ | — |
| `dev-reinstall-ui` | `dev-clean-reinstall-ui.sh` | ✓ | — |
| `dev-reinstall` | `dev-reinstall.ps1` (core + UI) | — | ✓ |
| `dev-clean` | `dev_clean.sh` | ✓ | — |
| `dev-logs` | `dev_logs.sh` | ✓ | — |
| `test-unit` | `cargo test --lib` + integration | ✓ | — |
| `test-dev` | `dev_test.sh` | ✓ | ✓ |
| `test-multi` | `tests/multi_model_e2e.{sh,ps1}` | ✓ | ✓ |
| `test-e2e-core` | `e2e-core.{sh,ps1}` (local release bin) | ✓ | ✓ |
| `test-release` | `test-release-unix.sh` / `test-release-windows.ps1` | ✓ | ✓ |
| `release-e2e` | `e2e-release.{sh,ps1}` | ✓ | ✓ |
| `cleanup-core` | `uninstall-core.{sh,ps1}` | ✓ | ✓ |
| `cleanup-ui` | `uninstall-ui.{sh,ps1}` | ✓ | ✓ |

Examples:

```bash
./scripts/lmforge.sh test-multi
./scripts/lmforge.sh test-multi --skip-mtp
./scripts/lmforge.sh release-e2e v0.1.5 --keep-install
LMFORGE_VERSION=v0.1.5 ./scripts/lmforge.sh test-release
```

```powershell
powershell -File scripts\lmforge.ps1 test-multi
powershell -File scripts\lmforge.ps1 release-e2e -Version v0.1.5 -KeepInstall
powershell -File scripts\lmforge.ps1 test-e2e-core
```

---

## Test tiers

| Tier | When to use | Entry |
|------|-------------|-------|
| **Unit / integration** | Rust changes, no GPU | `cargo test` or `lmforge.sh test-unit` |
| **Dev matrix** | API shape, single-model inference | `dev_test.sh` / mother `test-dev` |
| **Multi-model E2E** | Chat+embed co-load, bursts, VLM/rerank/MTP (SKIP if unavailable) | `multi_model_e2e.*` / mother `test-multi` |
| **Install E2E** | Core install lifecycle on local build | `e2e-core.*` / mother `test-e2e-core` |
| **Release smoke** | Published assets, no model pull | `test-release-*` / mother `test-release` |
| **Release E2E** | Full install + models + inference + cleanup | `e2e-release.*` / mother `release-e2e` |

### `dev_test.sh`

Holistic dev runner: cargo (optional) + live daemon API + inference probes.

```bash
scripts/util/dev_test.sh              # interactive
scripts/util/dev_test.sh --yes        # defaults
scripts/util/dev_test.sh --yes --full # + VLM + rerank
scripts/util/dev_test.sh --yes --full --with-mtp
scripts/util/dev_test.sh --e2e-only --yes   # skip cargo; hit running daemon
```

`--full` = VLM + rerank only. MTP is opt-in via `--with-mtp`.

### `multi_model_e2e.{sh,ps1}`

GPU E2E: co-load (chat + embed), burst traffic, capability gates, VLM (text +
picsum remote image, base64), rerank, MTP. **All capability suites run by
default**; unavailable models/engine features → `SKIP` (others continue).
Workloads are paragraph-scale (`E2E_CHAT_MAX_TOKENS=128`, picsum VLM, 5-doc
rerank). Override via `scripts/lib/e2e-defaults.*`.

```bash
bash tests/multi_model_e2e.sh
bash tests/multi_model_e2e.sh --skip-mtp
SKIP_BUILD=1 LF_BIN=target/release/lmforge bash tests/multi_model_e2e.sh
```

```powershell
$env:SKIP_BUILD = "1"
$env:LF_BIN = "target\release\lmforge.exe"
powershell -File tests\multi_model_e2e.ps1
powershell -File tests\multi_model_e2e.ps1 -SkipVlm
```

`--full` / `-Full` is a legacy alias (all suites on — now the default).

`tests/e2e.sh` is a thin wrapper (quick burst smoke, smaller defaults).

---

## Default E2E models

Canonical defaults live in `scripts/lib/e2e-defaults.{sh,ps1}` (sourced by
`e2e-api.*` and test scripts). Override via env:

| Variable | Default |
|----------|---------|
| `CHAT_MODEL` / `E2E_CHAT_MODEL` | `qwen3.5:2b:4bit` |
| `EMBED_MODEL` / `E2E_EMBED_MODEL` | `qwen3-embed:0.6b:8bit` |
| `VLM_MODEL` / `E2E_VLM_MODEL` | `qwen3-vl:2b:4bit` |
| `RERANK_MODEL` / `E2E_RERANK_MODEL` | `qwen3-reranker:0.6b:8bit` |
| `MTP_MODEL` / `E2E_MTP_MODEL` | `qwen3.5:4b:mtp:4bit` |
| `E2E_VLM_IMAGE_URL` | `https://picsum.photos/seed/picsum/200/300` |
| `E2E_CHAT_MAX_TOKENS` | `128` |
| `E2E_MTP_MAX_TOKENS` | `256` |
| `E2E_VLM_IMAGE_MAX_TOKENS` | `192` |

Shared API helpers (health, pull, chat, embed, VLM, rerank, MTP, assertions):
`scripts/lib/e2e-api.{sh,ps1}`.

---

## Common env vars

| Variable | Purpose |
|----------|---------|
| `LF_HOST` | Daemon base URL (default `http://127.0.0.1:11430`) |
| `LF_BIN` / `LMFORGE_LOCAL_BIN` | Path to `lmforge` binary under test |
| `SKIP_BUILD` | Use existing binary instead of `cargo build` |
| `SKIP_PULL` | Skip model downloads (models must already be installed) |
| `SKIP_START` | Assume daemon already running |
| `DO_VLM` / `DO_RERANK` / `DO_MTP` | Default `1`; set `0` or use `--skip-*` to disable a suite |
| `LMFORGE_VERSION` | Release tag for release-oriented scripts |
| `LMFORGE_DATA_DIR` | Override data dir (default `~/.lmforge`) |

Qwen3 chat models need `enable_thinking: false` in API requests; the shared
`e2e-api` layer sets this automatically for `qwen3*` shortcuts.

---

## Release E2E flow

`e2e-release.{sh,ps1}` installs core (+ UI when available) from a GitHub
release, pulls default models, runs `multi_model_e2e` (all suites on by default;
optional probes → `SKIP` when model/engine unavailable), then uninstalls unless
`-KeepInstall` / `--keep-install`.

```bash
scripts/util/e2e-release.sh v0.1.5
scripts/util/e2e-release.sh v0.1.5 --keep-install
LMFORGE_VERSION=v0.1.5 scripts/util/e2e-release.sh v0.1.5 --keep-install
```

```powershell
powershell -File scripts\util\e2e-release.ps1 -Version v0.1.5
powershell -File scripts\util\e2e-release.ps1 -Version v0.1.5 -KeepInstall
```

Platform notes:

| Platform | Caveat |
|----------|--------|
| Linux arm64 | Release has no UI AppImage; `install-ui` step skips (core E2E still runs) |
| macOS (MLX) | VLM / rerank / MTP may `SKIP` if the MLX engine lacks that capability |
| Windows | Full core + UI install path; same default suites as Unix |

`--full` / `-Full` is a legacy no-op (kept for old scripts).

See [RELEASE.md](./RELEASE.md) for tag → draft → publish workflow.

---

## Other util scripts

| Script | Role |
|--------|------|
| `dev_status.sh` | Binaries, daemon, disk snapshot |
| `dev_logs.sh` | Tail engine logs |
| `dev-reinstall-core.sh` / `dev-reinstall.ps1` | Clean build + install |
| `dev-clean-reinstall-ui.sh` | UI node_modules reset + dev launch |
| `dev_ui_*.sh` | Distro-specific WebKit deps for Tauri dev |
| `release_binary_test.sh` | CUDA12/13 release binary matrix |
