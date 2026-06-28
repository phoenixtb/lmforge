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

### Fast local build + install (keeps models)

For the day-to-day inner loop: rebuild from this checkout and swap the installed
artifact **without touching `~/.lmforge/models`**. None of these wipe data — only
`clean --purge`, `dev-up --wipe-models`, and `e2e` (full clean+purge) do.

| Goal | Command | What it does |
|------|---------|--------------|
| **Core only** (fastest) | `./scripts/lmforge.sh install --source local` | incremental `cargo build --release` → copy binary → restart service. Models/UI untouched. |
| **UI only** (fastest) | `./scripts/util/build-ui-local.sh --no-deps` | reuse `node_modules` (skip `npm ci`) → Tauri build → install via `install-ui`. Models/core untouched. |
| **Both** | `./scripts/lmforge.sh install --source local && ./scripts/util/build-ui-local.sh --no-deps` | core then UI, both incremental. |

```bash
# Core only — fastest path, models preserved
./scripts/lmforge.sh install --source local

# UI only — reuse node_modules (drop --no-deps after a package.json change)
./scripts/util/build-ui-local.sh --no-deps

# Both
./scripts/lmforge.sh install --source local && ./scripts/util/build-ui-local.sh --no-deps
```

```powershell
# Core only
powershell -File scripts\lmforge.ps1 install -Source local
# UI only (reuse node_modules)
powershell -File scripts\util\build-ui-local.ps1 -NoDeps
# Both
powershell -File scripts\lmforge.ps1 install -Source local; powershell -File scripts\util\build-ui-local.ps1 -NoDeps
```

Notes for max speed:
- `install --source local` builds **core only** and uses an **incremental** cargo
  build (no `cargo clean`). First build is cold; subsequent ones are seconds.
- Avoid `dev-up` for a quick loop — it runs `cargo clean` and wipes `~/.lmforge/engines`
  by default. If you do use it, `dev-up --no-cargo-clean --keep-engines` keeps it fast
  (Linux/llama.cpp only).
- Avoid `e2e --source local` for iterating — it does a **full clean + purge** (wipes
  models). Use it only for the pre-release gate.
- Even tighter core loop (no service reinstall): `cargo build --release --bin lmforge`
  then restart the daemon pointing at `target/release/lmforge`.

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

---

## Sampling & thinking

### The problem: reasoning loops eat the budget

LMForge follows a **client-owns-sampling** contract — the daemon never overrides
a sampling value the client sends. It does, however, **seed anti-loop defaults
for thinking requests that arrive with no sampling at all** (`think:true` with no
`temperature`/`top_p`/`top_k`/penalty): absent fields are filled from the thinking
profile below, present fields are left untouched. That keeps the OpenAI/Ollama
surfaces predictable while stopping a thin client (only `temperature` +
`max_tokens`) from inheriting the engine defaults under which Qwen3-class
reasoning models degenerate badly.

Symptom: ask a reasoning model a "hard" question with `think:true` and a low
temperature, and the reasoning stream collapses into a repeating tail —

```
… so the ball is 0.05. Wait. Wait. Wait. Wait. Wait. Wait. …
```

The loop never terminates on its own, so it **fills the entire `thinking_budget`**
(default 2048 tokens). The two-call thinking orchestrator then hits the
budget-exhausted branch, feeds that looping `<think>…</think>` block into call-2
as a prefill, and call-2 dutifully echoes it — which is why the *thinking* and the
*answer* come back looking identical. Both symptoms (the loop **and** the
duplicated answer) have one root cause: **missing repetition/nucleus controls**,
not a bug in the orchestrator.

> The two-call orchestrator runs on **all** thinking engines now — oMLX (native
> `reasoning_content`) and llama.cpp / SGLang (inline `<think>` tags, split out by
> the daemon). This guarantees an answer phase even when the reasoning budget is
> exhausted, so the engine can no longer return a *blank* answer (which used to
> happen on llama.cpp: a runaway `<think>` would burn `max_tokens` and stream
> nothing). If a model still produces no answer, the Playground surfaces a
> "used its entire thinking budget before answering" hint instead of an empty bubble.

### The fix: send the thinking sampling profile

The cure is the standard Qwen3 thinking profile — nucleus + top-k + a repetition
penalty to break the loop. These are the knobs and what they do:

| Param | Role | Chat default | Thinking default |
|-------|------|:------------:|:----------------:|
| `temperature` | Randomness. ≥ 0.6 is required for Qwen3 thinking (lower → deterministic loops). | `0.7` | `0.6` |
| `top_p` | Nucleus cutoff (cumulative prob). | `0.95` | `0.95` |
| `top_k` | Keep only the top-k tokens (`0` = off). | `20` | `20` |
| `repetition_penalty` | **Primary loop-breaker** — `>1` penalizes already-seen tokens. | `1.1` | `1.2` |
| `presence_penalty` | Secondary anti-repeat; discourages re-using any present token. | `0.0` | `0.3` |
| `thinking_budget` | Reasoning-phase token cap for the two-call orchestrator. Only sent when `think:true`. | — | `2048` |

These mirror docintel's `LLM_THINKING_*` defaults (`config/defaults.env`), which is
the reference implementation that streams clean reasoning on the same oMLX engine.

> oMLX note: the daemon derives a `repetition_penalty` from `presence`/`frequency`
> penalties when the engine doesn't accept them directly, but a client-supplied
> `repetition_penalty` always wins. Sending `1.2` explicitly is the reliable path.

### curl — thinking (loop-free)

```bash
curl -sN http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "qwen3.5:4b:6bit",
    "messages": [{"role":"user","content":"A bat and ball cost $1.10. The bat costs $1 more than the ball. How much is the ball? Reason step by step."}],
    "stream": true,
    "think": true,
    "stream_reasoning_deltas": true,
    "thinking_budget": 2048,
    "temperature": 0.6,
    "top_p": 0.95,
    "top_k": 20,
    "repetition_penalty": 1.2,
    "presence_penalty": 0.3,
    "max_tokens": 1024
  }'
```

Reasoning arrives as `delta.reasoning_content`, the answer as `delta.content`.
With the profile above the reasoning terminates in a few hundred tokens, so the
budget isn't exhausted, call-2 never runs, and thinking ≠ answer.

### curl — non-thinking

Omit `think` (or send `think:false`) and the chat profile is enough. No
`thinking_budget`, no orchestrator — a single pass-through call.

```bash
curl -sN http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "qwen3.5:4b:6bit",
    "messages": [{"role":"user","content":"Give me one fun fact about octopuses."}],
    "stream": true,
    "temperature": 0.7,
    "top_p": 0.95,
    "top_k": 20,
    "repetition_penalty": 1.1,
    "max_tokens": 256
  }'
```

### Model guidance (not a hard rule)

Sampling fixes the *engine default* loop; it does not make a tiny model a strong
reasoner. Cross-platform benchmarks (`tests/bench/think_bench.py`, run on
oMLX/metal, llama.cpp/CUDA and llama.cpp/Vulkan) show:

- **≥ 4B reasoning models** (`qwen3.5:4b:6bit`, `qwen3:4b:thinking:4bit`,
  `qwen3:8b:4bit`) are reliable in thinking mode with the profile above.
- **Tiny quantized models** (`qwen3.5:2b:4bit`, `qwen3:1.7b:4bit`) still loop on
  multi-step problems even with the full anti-loop profile — they lack the
  stability to terminate a reasoning chain, and on a trivial prompt (e.g.
  "how are you") they may fabricate a problem to reason about.

This is **guidance, not enforcement** — LMForge runs whatever model you point it
at. For conversational turns, prefer `think:false` (or omit it); reserve
`think:true` for ≥ 4B models when you actually want step-by-step reasoning. To
reproduce/extend the matrix on your own hardware, see
[`tests/bench/think_bench.py`](../../tests/bench/think_bench.py) — the result dir
is auto-fingerprinted per machine (`<ts>__<os>-<arch>-<accel>`) and `report.md`
scores blank answers (budget exhausted, no content) as failures.

### Playground & Postman

- **Playground** (`ui` → *Interact → Playground*) bakes both profiles in. The
  **think** toggle snaps every value to the thinking profile (and back); the
  **sampling** button opens an Advanced row to override `top_p` / `top_k` /
  `rep_pen` / `pres_pen` and a *reset* back to the active profile.
- **Postman** — see [`docs/postman/`](../postman/). The thinking requests carry
  the profile above so they run loop-free out of the box; tune via the collection
  variables.
