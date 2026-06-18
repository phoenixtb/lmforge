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
| Windows | `powershell -File scripts\lmforge.ps1` | `powershell -File scripts\lmforge.ps1 <action> [-Source …] [flags]` |

The install **source** is a parameter (`--source local` builds + installs from this
checkout; `--source release[:TAG]` installs a published GitHub release), so the same
`install` / `e2e` verbs cover dev, pre-release, and post-release.

### Action map

| Action | Script / command | Unix | Win |
|--------|------------------|:----:|:---:|
| `status` | `dev_status.sh` (or inline health check) | ✓ | ✓ |
| `install` | `install-core` (+ `install-ui`); `--source local\|release[:TAG]` | ✓ | ✓ |
| `e2e` | `util/e2e.{sh,ps1}`; `--source …` + lifecycle/inference/UI/asset flags | ✓ | ✓ |
| `clean` | `uninstall-core` + `uninstall-ui` (`--dev` adds `dev_clean.sh`, `--purge` data) | ✓ | ✓ |
| `dev-up` | `dev-reinstall-core.sh` / `dev-reinstall.ps1` (build + run, debug) | ✓ | ✓ |
| `dev-up-ui` | `dev-clean-reinstall-ui.sh` | ✓ | — |
| `dev-down` | `dev_clean.sh` (audit / wipe build artefacts) | ✓ | — |
| `dev-logs` | `dev_logs.sh` | ✓ | — |
| `test-unit` | `cargo test --lib` + integration | ✓ | — |
| `test-dev` | `dev_test.sh` | ✓ | ✓ |
| `test-multi` | `tests/multi_model_e2e.{sh,ps1}` (against running daemon) | ✓ | ✓ |

> `e2e-core.{sh,ps1}` still exists as the CI install-lifecycle gate (no inference/UI);
> `e2e` is the human-facing superset.

Examples:

```bash
./scripts/lmforge.sh e2e --source local                      # full pre-release cycle
./scripts/lmforge.sh e2e --source release:v0.1.5 --keep-install
./scripts/lmforge.sh e2e --source release:v0.1.5 --verify-assets --no-inference
./scripts/lmforge.sh install --source release:v0.1.5
./scripts/lmforge.sh clean --dev --purge
```

```powershell
powershell -File scripts\lmforge.ps1 e2e -Source local
powershell -File scripts\lmforge.ps1 e2e -Source release:v0.1.5 -KeepInstall
powershell -File scripts\lmforge.ps1 install -Source release:v0.1.5
powershell -File scripts\lmforge.ps1 clean -Dev -Purge
```

---

## Test tiers

| Tier | When to use | Entry |
|------|-------------|-------|
| **Unit / integration** | Rust changes, no GPU | `cargo test` or `lmforge.sh test-unit` |
| **Dev matrix** | API shape, single-model inference | `dev_test.sh` / mother `test-dev` |
| **Multi-model E2E** | Chat+embed co-load, bursts, VLM/rerank/MTP (SKIP if unavailable) | `multi_model_e2e.*` / mother `test-multi` |
| **Install E2E (CI gate)** | Core install lifecycle, no inference/UI | `e2e-core.*` (CI: `e2e.yml` / `release.yml`) |
| **Unified E2E** | Install (any source) + lifecycle + UI + inference + asset verify | `util/e2e.*` / mother `e2e --source …` |

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

`util/e2e.{sh,ps1}` is the one runner for every install source. Each run is a full
cycle: **full clean** (remove any prior install — GitHub script, dev symlink, … —
and `~/.lmforge` data) → install → lifecycle → `multi_model_e2e` (all suites on by
default; optional probes → `SKIP` when model/engine unavailable) → **full purge**
(binary, service, UI, models) — unless `--keep-install` / `-KeepInstall`.

- `--source local` **builds core + UI from this checkout** (`cargo build --release`
  and `npm run tauri build` via `build-ui-local.{sh,ps1}`), then installs both. This
  is the full pre-release cycle. `--no-build` reuses an existing `target/release`
  binary (core only).
- `--source release[:TAG]` installs a published release. `--verify-assets` adds the
  published-asset + scripts-match checks (the old release smoke).
- UI is installed by default on both (`--no-ui` to skip). Local builds the UI from
  source via `build-ui-local.{sh,ps1}` (Tauri build → install via the `LMFORGE_UI_LOCAL`
  path in `install-ui`); release installs the published artifact. UI build needs the
  Tauri toolchain (node, Rust, webkit2gtk on Linux / WebView2 on Windows).

```bash
scripts/util/e2e.sh --source local                                # full pre-release cycle
scripts/util/e2e.sh --source release:v0.1.5 --keep-install
scripts/util/e2e.sh --source release:v0.1.5 --verify-assets --no-inference   # smoke
```

```powershell
powershell -File scripts\util\e2e.ps1 -Source local
powershell -File scripts\util\e2e.ps1 -Source release:v0.1.5 -KeepInstall
powershell -File scripts\util\e2e.ps1 -Source release:v0.1.5 -VerifyAssets -NoInference
```

Platform notes:

| Platform | Caveat |
|----------|--------|
| Linux arm64 | Release has no UI AppImage; `install-ui` step skips (core E2E still runs) |
| macOS (MLX) | VLM / rerank / MTP may `SKIP` if the MLX engine lacks that capability |
| Windows | Full core + UI install path; same default suites as Unix |

See [RELEASE.md](./RELEASE.md) for tag → draft → publish workflow.

---

## Other util scripts

| Script | Role |
|--------|------|
| `dev_status.sh` | Binaries, daemon, disk snapshot |
| `dev_logs.sh` | Tail engine logs |
| `dev-reinstall-core.sh` / `dev-reinstall.ps1` | Clean build + install |
| `build-ui-local.{sh,ps1}` | Build UI from source + install locally (used by `e2e --source local`) |
| `dev-clean-reinstall-ui.sh` | UI node_modules reset + dev launch |
| `dev_ui_*.sh` | Distro-specific WebKit deps for Tauri dev |
| `release_binary_test.sh` | CUDA12/13 release binary matrix |
