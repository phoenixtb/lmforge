<div align="center">

<img src="docs/assets/logo.png" width="96" alt="LMForge logo" />

# LMForge

**Hardware-aware LLM inference orchestrator — from edge to server**

[![Release](https://img.shields.io/github/v/release/phoenixtb/lmforge?include_prereleases&style=flat-square&color=6ee7b7)](https://github.com/phoenixtb/lmforge/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/phoenixtb/lmforge/ci.yml?style=flat-square&label=CI)](https://github.com/phoenixtb/lmforge/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=flat-square)](#supported-platforms)

Run multiple LLMs simultaneously on your local hardware.  
LMForge is a persistent daemon that manages model loading, VRAM allocation, and engine selection automatically — then exposes a single OpenAI-compatible REST API to every app on your machine.

[**Install**](#install) · [**Quick Start**](#quick-start) · [**Verify Installation**](#verify-installation) · [**CLI Reference**](#cli-reference) · [**REST API**](#rest-api) · [**Model Catalog**](#model-catalog) · [**Developer Guide**](#developer-guide) · [**Contributing**](#contributing)

</div>

---

## What is LMForge?

LMForge is a **local AI infrastructure layer**. It sits between your hardware and the tools that consume AI (code editors, agents, custom scripts, REST clients), managing the messy reality of running LLMs locally:

- Which engine to use for your hardware (Apple MLX, SGLang, llama.cpp)?
- How much VRAM is available, and which models fit right now?
- What happens when you want a chat model *and* an embedding model loaded at once?
- How do you keep everything running when apps open and close?

LMForge answers all of this transparently. Consumer apps see a simple OpenAI-compatible API; LMForge handles the rest.

### Architecture

```
┌─────────────────────────────────────────────────────┐
│  LMForge Core (daemon)                              │
│  • Runs as a system service — always available      │
│  • Starts at login, survives app closures           │
│  • REST API at http://127.0.0.1:11430               │
│  • Manages model lifecycle, VRAM, engine selection  │
└────────────────────┬────────────────────────────────┘
                     │ HTTP  (OpenAI-compatible API)
       ┌─────────────┼──────────────┐
       │             │              │
  LMForge UI    Your IDE       Any Client
  (Tauri app)  (Copilot,    (curl, Python,
               Continue)     LangChain…)
```

This is the **Docker model**: the engine is a service, the UI is just a client. Closing the desktop app never stops your models.

---

## Features

- **Multi-model orchestration** — chat, embeddings, vision, and rerank models can be resident together; per-model `keep_alive`, VRAM admission, and LRU eviction
- **Hardware-aware engine selection** — automatically picks the best engine / build:
  - 🍎 **Apple Silicon** → [oMLX](https://github.com/jundot/omlx) — OpenAI-compatible server on Metal via MLX
  - 🖥️ **NVIDIA GPU (Linux)** → [SGLang](https://github.com/sgl-project/sglang) (8 GB+ VRAM) or [llama.cpp](https://github.com/ggerganov/llama.cpp) CUDA (`cuda12` / opt-in `cuda13`)
  - 🪟 **NVIDIA GPU (Windows)** → llama.cpp CUDA prebuilts — SGLang is Linux-only upstream; use WSL2 if you need it
  - 🎮 **AMD / Intel GPU** → llama.cpp Vulkan
  - 💻 **CPU / any hardware** → llama.cpp — universal fallback
- **Engine tiers** — `default` / `opt-in` / `experimental`; `lmforge doctor` shows installed variants and which is active
- **OpenAI-compatible API** — `/v1/chat/completions`, `/v1/embeddings`, `/v1/models`, `/v1/rerank`
- **Ollama-compatible API** — `/api/chat`, `/api/generate`, `/api/tags` for tools that expect Ollama
- **Thinking / reasoning** — two-call `thinking_budget` workflow, live reasoning deltas, chat vs thinking sampling profiles; dedicated `:thinking` / `:reasoning` catalog models stay locked on (`native_reasoning`)
- **MTP speculative decoding** — GGUF models with MTP heads get llama.cpp draft-MTP when VRAM headroom allows (catalog `:mtp` shortcuts)
- **Capability detection** — pull (and startup self-heal) records `chat` / `vision` / `embeddings` / `thinking` / `native_reasoning` / `mtp` / … without a forced re-download after detector fixes
- **Model catalog** — shortcuts `family:size:quant[:variant]` resolve to the right HuggingFace repo and format for your hardware
- **Desktop UI** — optional Tauri app: Model Library, **Playground** (think toggle + Advanced sampling), Observability, Settings (incl. relocatable model storage)
- **System service on all platforms** — launchd (macOS), systemd user unit (Linux), HKCU Run key (Windows) — `lmforge service install`, no admin required
- **Real-time telemetry** — live GPU/CPU/memory metrics, Prometheus `/metrics`, request latency/counts, engine `last_errors`
- **Secure by default** — localhost bind; optional API key + `trusted_networks` for LAN
- **Idempotent CLI** — `lmforge start` is safe to call from any script; no-ops if already running

---

## Supported Platforms

| Platform | Architecture | Engine | Core | Desktop UI |
|---|---|---|---|---|
| macOS 13+ | Apple Silicon (arm64) | oMLX (Metal/MLX) | ✅ | ✅ DMG |
| Ubuntu / Debian | x86_64 | SGLang (NVIDIA, 8 GB+) / llama.cpp | ✅ | ✅ .deb (AppImage fallback) |
| Fedora / RHEL / SUSE | x86_64 | SGLang (NVIDIA, 8 GB+) / llama.cpp | ✅ | ✅ .rpm (AppImage fallback) |
| Ubuntu 22.04 / 24.04 / 26.04 | arm64 | llama.cpp | ✅ | 🔜 Planned |
| Windows 10/11 | x86_64 | llama.cpp (CPU + NVIDIA CUDA) | ✅ | ✅ NSIS installer |
| Windows 10/11 + WSL2 | x86_64 | SGLang (NVIDIA via CUDA-on-WSL) | ✅ (inside WSL) | run via Linux build |

> **macOS Intel (x86_64)** binaries are available but not currently published via the CI release pipeline. Build from source with `cargo build --release --target x86_64-apple-darwin`.

> **SGLang is Linux-only upstream.** On Windows the engine selector picks `llama.cpp` even on NVIDIA hardware. To run SGLang on a Windows host, install WSL2 + Ubuntu, install the NVIDIA Windows driver (CUDA-on-WSL is included automatically — do **not** install a Linux NVIDIA driver inside WSL), then install the Linux LMForge build *inside* WSL. The Windows-side LMForge can keep running llama.cpp; the two are independent.

> **Windows 10 users** must install the Edge WebView2 Runtime before launching the desktop UI (preinstalled on Windows 11). Get it from <https://developer.microsoft.com/microsoft-edge/webview2/>.

> **Windows Firewall**: when LMForge first binds to a non-loopback address (e.g. `0.0.0.0` for LAN access), Windows will pop a Defender Firewall dialog asking to allow `lmforge.exe` on Private/Public networks. Allow it on **Private** networks only unless you intentionally want WAN exposure.

> **Windows + NVIDIA — recommended one-time setting**: in NVIDIA Control Panel → Manage 3D Settings, set **CUDA - Sysmem Fallback Policy** to **Prefer No Sysmem Fallback**. The default policy silently pages VRAM into system RAM under memory pressure, which slows inference 4–6x and can corrupt engine output on some driver/GPU combinations. Details in [docs/dev/INSTALL_WINDOWS.md](docs/dev/INSTALL_WINDOWS.md#-known-issue-wddm-sysmem-fallback-nvidia--set-the-driver-policy).

> **Ubuntu 26.04 — building from source**: `libwebkit2gtk-4.1-dev` was removed in 26.04. Use `libwebkitgtk-6.0-dev` instead when installing Tauri build dependencies manually. The pre-built AppImage and core binary (released binaries built on Ubuntu 22.04) run on 26.04 without modification.

---

## Install

### Core (daemon + CLI)

The daemon runs as a system service. Install once; it starts automatically on every login.

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

**Windows:**
```powershell
irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
```

Or download `lmforge-windows-x86_64.exe` from [Releases](https://github.com/phoenixtb/lmforge/releases/latest), place it on your `PATH`, then run `lmforge init` and `lmforge service install`.

What the install script does on macOS/Linux:
1. Downloads the pre-built `lmforge` binary for your platform/arch (~5 MB)
2. Installs to `/usr/local/bin/lmforge`
3. Runs `lmforge init` — probes hardware, pulls the matching inference engine:
   - **macOS** → oMLX (MLX on Metal) via Homebrew
   - **Linux NVIDIA (driver ≥ r570)** → custom **cuda12** tarball (~1 GB, bundled
     CUDA runtime + llama.cpp). Opt-in **cuda13** via
     `lmforge engine install llamacpp --variant cuda13`.
   - **Linux NVIDIA (below r570) / AMD / Intel** → Vulkan upstream build
   - **Linux / Windows, no GPU** → CPU build
   - **Windows NVIDIA** → upstream CUDA prebuilts
4. Registers a system service (`launchd` on macOS, `systemd --user` on Linux)
5. Starts the daemon immediately

Override variant: `LMFORGE_LLAMACPP_VARIANT={cuda12,cuda13,cpu,gpu}` before
`lmforge init`. Run `lmforge doctor` to see installed variants and which is active.

To pin a specific version:
```bash
LMFORGE_VERSION=v0.1.2 curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

### Desktop UI (optional)

Install the core first, then:

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
```

**Windows:**
```powershell
irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.ps1 | iex
```

WebView2 is required on Windows 10 (pre-installed on Windows 11). The installer downloads it automatically when internet is available.

The UI is a pure client — closing it never affects the daemon or running models.

### Uninstall

**macOS / Linux:**
```bash
# Remove the desktop UI only (daemon keeps running)
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.sh | bash

# Remove Core (stops daemon; keeps your downloaded models)
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash

# Remove everything including models
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash -s -- --purge
```

**Windows:**
```powershell
# Remove UI only (daemon keeps running)
irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.ps1 | iex

# Remove Core (stops daemon; keeps models)
irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.ps1 | iex

# Remove everything including models
$env:LMFORGE_PURGE = "1"; irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.ps1 | iex
```

---

## Quick Start

Model shortcuts follow the format **`family:size:quant`** (and optionally **`:variant`**).  
The same shortcut resolves to the correct format for your hardware — MLX on Apple Silicon, GGUF everywhere else.

```bash
# 1. Install core
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash

# 2. Pull a model using its catalog shortcut
lmforge pull qwen3:8b:4bit

# 3. Interactive chat via CLI
lmforge run qwen3:8b:4bit

# 4. Or hit the API directly
curl http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3:8b:4bit",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'

# 5. Thinking model with budget cap (Qwen3 / DeepSeek-R1 style)
curl http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3.5:4b:4bit",
    "messages": [{"role": "user", "content": "Prove the Pythagorean theorem."}],
    "think": true,
    "thinking_budget": 4096,
    "stream_reasoning_deltas": true,
    "stream": true
  }'

# 6. Embeddings
curl http://127.0.0.1:11430/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model": "nomic-embed-text:v1.5", "input": "Hello world"}'
```

---

## Verify Installation

After `lmforge start`, run these smoke tests to confirm the daemon is healthy and models respond correctly.

```bash
# ── 1. Health / status ───────────────────────────────────────────────────────
curl -s http://127.0.0.1:11430/health
curl -s http://127.0.0.1:11430/lf/status | jq '{overall_status, engine, running_models}'

# ── 2. Pull the recommended inference + embedding models ─────────────────────
lmforge pull qwen3:8b:4bit           # chat / reasoning (~4.5 GB)
lmforge pull qwen3-embed:0.6b:8bit   # embeddings (~0.6 GB)

# ── 3. Chat completion (non-streaming) ───────────────────────────────────────
curl -s http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3:8b:4bit",
    "messages": [{"role": "user", "content": "Say: OK"}],
    "max_tokens": 16
  }' | jq '.choices[0].message.content'
# Expected: "OK" (or similar short acknowledgement)

# ── 4. Chat completion (streaming) ───────────────────────────────────────────
curl -sN http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3:8b:4bit",
    "messages": [{"role": "user", "content": "Count to 5."}],
    "stream": true
  }'
# Expected: a series of data: {"choices":[{"delta":{"content":"..."}}]} lines

# ── 5. Embeddings ─────────────────────────────────────────────────────────────
curl -s http://127.0.0.1:11430/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3-embed:0.6b:8bit",
    "input": "The quick brown fox"
  }' | jq '.data[0].embedding | length'
# Expected: 1024 (embedding dimension for the 0.6B model)

# ── 6. List loaded models ─────────────────────────────────────────────────────
curl -s http://127.0.0.1:11430/v1/models | jq '[.data[] | {id, capabilities}]'
```

> If you set an API key (`api_key` in `config.toml` or `LMFORGE_API_KEY` env), add `-H "Authorization: Bearer <your-key>"` to every request above.

### Confirm the engine that was selected

```bash
curl -s http://127.0.0.1:11430/lf/status | jq '.engine'
# Expected on Linux + NVIDIA ≥ 8 GB VRAM: { "id": "sglang", "version": "..." }
# Expected on Apple Silicon:              { "id": "omlx", "version": "..." }
# Anything else (incl. Windows + NVIDIA): { "id": "llamacpp", "version": "..." }
```

If you see `llamacpp` on a Linux machine with ≥ 8 GB NVIDIA VRAM, **restart the daemon — the engine is re-selected on every `start`**, so this is enough; SGLang will auto-install on first launch:

```bash
lmforge stop
lmforge start   # logs will show "Engine not installed, running installer..." for SGLang
```

---

## CLI Reference

```
lmforge <command> [options]
```

| Command | Description |
|---|---|
| `lmforge init` | Probe hardware, select engine, install if needed |
| `lmforge start` | Start the daemon (idempotent — safe to call if already running) |
| `lmforge stop` | Gracefully stop the daemon |
| `lmforge status` | Show engine status, loaded models, and VRAM usage |
| `lmforge pull <model>` | Download a model (catalog shortcut, HF repo, or local path) |
| `lmforge run <model>` | Interactive REPL with a model |
| `lmforge catalog` | List all model shortcuts |
| `lmforge catalog --search <keyword>` | Search the catalog |
| `lmforge models list` | List installed models with sizes and capabilities |
| `lmforge models remove <name>` | Remove a model from disk |
| `lmforge models unload` | Unload active model from VRAM (keeps files) |
| `lmforge logs` | View daemon logs |
| `lmforge logs -f` | Tail logs continuously |
| `lmforge clean` | Audit disk usage (orphans, logs, HF cache) |
| `lmforge clean --all --yes` | Clean everything without prompts |
| `lmforge service install` | Register as a system service (auto-start on login) |
| `lmforge service uninstall` | Remove the system service |
| `lmforge service start` | Start the registered service |
| `lmforge service stop` | Stop the registered service |
| `lmforge service status` | Show service registration and daemon reachability |

### Service Management

LMForge registers a native service on every platform — no admin/root required:

| Platform | Mechanism | Config location |
|---|---|---|
| macOS | launchd user agent | `~/Library/LaunchAgents/com.lmforge.daemon.plist` |
| Linux | systemd user unit | `~/.config/systemd/user/lmforge.service` |
| Windows | HKCU Run key (At Logon) | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` → `LMForge` |

---

## REST API

LMForge exposes two API surfaces on `http://127.0.0.1:11430`:

### OpenAI-compatible (`/v1/*`)

Drop-in replacement for any tool built on the OpenAI SDK:

```bash
export OPENAI_API_BASE=http://127.0.0.1:11430/v1
export OPENAI_API_KEY=none    # no key required
```

| Endpoint | Method | Description |
|---|---|---|
| `/v1/models` | GET | List available models |
| `/v1/chat/completions` | POST | Chat completion (streaming + non-streaming) |
| `/v1/completions` | POST | Text completion |
| `/v1/embeddings` | POST | Generate embeddings (batched, auto-chunked) |
| `/v1/rerank` | POST | Rerank documents |

### Ollama-compatible (`/api/*`)

For tools that target Ollama (Open WebUI, Continue, etc.):

| Endpoint | Method | Description |
|---|---|---|
| `/api/chat` | POST | Chat generation |
| `/api/generate` | POST | Text generation |
| `/api/tags` | GET | List installed models |

### LMForge-native (`/lf/*`)

| Endpoint | Method | Description |
|---|---|---|
| `/health` | GET | Health check |
| `/lf/status` | GET | Engine state snapshot (includes `last_errors`) |
| `/lf/status/stream` | GET | SSE stream of state changes |
| `/lf/hardware` | GET | Detected hardware (GPU, CPU, VRAM, engine) |
| `/lf/engines` | GET | Engine registry (tiers, variants, active engine) |
| `/lf/sysinfo` | GET | Live system metrics (GPU%, VRAM, CPU, RAM) |
| `/lf/model/list` | GET | Installed models with sizes and capabilities |
| `/lf/model/pull` | POST | Download a model (SSE progress stream) |
| `/lf/model/switch` | POST | Load a model into VRAM |
| `/lf/model/unload` | POST | Unload a model from VRAM |
| `/lf/model/delete/{name}` | DELETE | Remove a model from disk |
| `/lf/config` | GET | Current runtime configuration |
| `/lf/shutdown` | POST | Graceful daemon shutdown |

---

## Integrating LMForge (client-side reference)

Quick reference for downstream services (DocIntel and similar) that consume
LMForge over HTTP. Everything below works against an unmodified daemon
running at `http://127.0.0.1:11430`.

### Auth model in two sentences

The daemon binds loopback by default and ships with `trusted_networks`
covering loopback + RFC1918 (`10/8`, `172.16/12`, `192.168/16`) — any client
on those networks reaches the API without a token. Set `api_key` in
`~/.lmforge/config.toml` to require `Authorization: Bearer <key>` for any
source outside `trusted_networks`; `/health` and `/metrics` are always
auth-bypassed for liveness probes and Prometheus.

### Capability discovery

Before sending a request, ask which models can do what:

```bash
curl -s http://127.0.0.1:11430/v1/models | jq '.data[] | {id, capabilities}'
# → {"id":"qwen3:8b:4bit","capabilities":{"chat":true,"vision":false,...}}
# → {"id":"qwen2.5-vl:7b:4bit","capabilities":{"chat":true,"vision":true,...}}

curl -s http://127.0.0.1:11430/v1/models/qwen2.5-vl:7b:4bit | jq
# Single-model lookup with full metadata (size_bytes, format, mmproj_path, ...)
```

Capability bits: `chat`, `embeddings`, `reranking`, `thinking`, `native_reasoning`,
`vision`, `mtp`, `stop_tokens`, `embedding_dims` (auto-detected on first
`/v1/embeddings` call), `mmproj_path` (GGUF VLMs only). Detector fixes self-heal
on daemon startup when `caps_detector_version` advances — no re-pull required.

### Vision (multimodal) requests

Use OpenAI's content-block format on `/v1/chat/completions`:

```json
{
  "model": "qwen2.5-vl:7b:4bit",
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "Describe this page."},
      {"type": "image_url", "image_url": {"url": "https://example.com/page.jpg"}}
    ]
  }]
}
```

Behaviour to integrate against:

- **Remote `http(s)://` URLs** are fetched server-side with a real
  `User-Agent`, capped at 20 MB (override `LMFORGE_IMAGE_MAX_BYTES`), and
  rewritten as inline `data:` URLs before reaching the engine. Hosts that
  block empty UAs (Wikimedia, several CDNs) no longer cause silent
  hallucinations — fetch failures return **400 `image_fetch_failed`**;
  oversized payloads return **413 `image_too_large`**.
- **`data:image/...;base64,...` URLs** pass through untouched.
- **Anthropic `{"type":"image","source":{...}}`** and OpenAI Responses
  **`{"type":"input_image","image_url":"..."}`** aliases are recognised by
  the capability gate.
- Sending an image to a non-vision model returns **400
  `vision_not_supported`** before the engine spins up.
- Ollama clients (`/api/chat`) can keep using the legacy
  `images: ["<base64>"]` field per message — LMForge translates them.

### Embeddings

```bash
curl -sS http://127.0.0.1:11430/v1/embeddings \
  -H 'content-type: application/json' \
  -d '{"model":"qwen3-embed:0.6b:8bit","input":["doc one","doc two"]}'
```

- Inputs over `embed_batch_size` (default 32) are auto-chunked across
  multiple engine calls; `usage.prompt_tokens` and `data[].index` are
  re-merged transparently.
- `capabilities.embedding_dims` is `null` until the first successful call
  observes it, then it's persisted to `models.json`.
- Sending a non-embedding model returns **400** with a clear suggestion.

### Reranking — Linux + NVIDIA caveat

`/v1/rerank` works fine on macOS (oMLX) and on llama.cpp builds. **On Linux
+ NVIDIA + SGLang it returns 501** because SGLang v0.5.10's cross-encoder
support is experimental and disabled in `engines.toml`. Workarounds: (a)
pull a GGUF reranker and run a second LMForge instance on a different port
pinned to llama.cpp via `engines.toml`, or (b) use an LLM-as-reranker via
`/v1/chat/completions`. Multi-engine routing is on the roadmap.

### Concurrency, queueing, retries

- `[resources] max_concurrent_requests` caps inflight requests (default 4).
  Excess waits up to `request_queue_size × 100 ms` for a permit, then
  returns **503 `concurrency_limit`** with `Retry-After: 1`. Clients should
  honour `Retry-After`.
- `[resources] max_request_body_mb` caps the HTTP request body (default
  **32 MB**, env override `LMFORGE_MAX_BODY_MB`). Sized for VLM payloads
  with inline base64 images — typical 300 DPI A4 page renders fit
  comfortably; remote URLs are still bounded separately by
  `LMFORGE_IMAGE_MAX_BYTES`. Bodies above the cap return **413**. Lower it
  on hostile networks to shrink DoS surface.
- `keep_alive` (default `5m`) determines TTL after which idle models unload
  from VRAM. Pass `"keep_alive": "0"` in a request to evict immediately, or
  `"keep_alive": "1h"` to override per call.

### Cold-load latency & warming models from your app

First call to an unloaded model pays the load cost (≈3–60 s depending on
model + engine). LMForge intentionally does **not** ship with any models
pre-loaded — choosing which models to warm is the consumer's decision, not
the daemon's.

**Recommended pattern (consumer-side warm-up).** During your service's
startup, call `POST /lf/model/switch` for each model you depend on. This
keeps the model list versioned with your app, not with the operator's
LMForge install:

```bash
for m in qwen3.5:4b:4bit qwen2.5-vl:7b:4bit qwen3-embed:0.6b:8bit; do
  curl -sS -X POST http://127.0.0.1:11430/lf/model/switch \
    -H 'content-type: application/json' \
    -d "{\"model\":\"$m\"}"
done
```

The endpoint is idempotent (no-op if the model is already resident), returns
immediately, and load progress streams on `GET /lf/status/stream` (SSE) for
UI feedback.

**Operator-side pre-warm (optional).** When LMForge is run as a shared,
multi-tenant daemon and the operator wants a fixed warm set independent of
any consumer, `[orchestrator] auto_load = [...]` cold-loads serially at
startup with logged progress. Leave it empty for the standard single-app
deployment.

### Errors clients should handle

| Status | `code` | Meaning |
|---|---|---|
| 400 | `vision_not_supported` | Image sent to a non-vision model. Pick a `vision:true` model. |
| 400 | `image_fetch_failed` | Remote image URL returned non-2xx or DNS/timeout. Use a direct asset URL or inline as `data:`. |
| 401 | `missing_or_invalid_api_key` | Outside `trusted_networks` and no/wrong `Authorization: Bearer`. |
| 413 | `image_too_large` | Image > `LMFORGE_IMAGE_MAX_BYTES`. Resize or raise the cap. |
| 413 | (axum default body) | Request body > `max_request_body_mb` (default 32 MB). Compress images, reduce DPI, or raise via `LMFORGE_MAX_BODY_MB`. |
| 503 | `concurrency_limit` | At capacity — back off (`Retry-After: 1`) and retry. |
| 503 | (no code) | Engine starting / model loading. /health distinguishes the two. |
| 504 | (server) | Inference exceeded the 120 s wall-clock guard (thinking-mode runaway). |

### Observability for client-side dashboards

Scrape `GET /metrics` (Prometheus text format). Useful series:

- `lmforge_requests_total{endpoint,status}` — request rate and error rate per route.
- `lmforge_request_duration_seconds_bucket{endpoint}` — latency histogram.
- `lmforge_model_loads_total{model,result}` + `lmforge_model_load_duration_seconds` — cold-load behaviour.
- `lmforge_active_models` — gauge of models currently in VRAM.
- `lmforge_image_inputs_total{result=accepted|rejected|data_url}` — VLM traffic mix.
- `lmforge_auth_rejections_total` — credential failures.

### What changed in v0.2.x (vs v0.1.x)

DocIntel-relevant changes since the previous release:

| Area | Change |
|---|---|
| VLM | `qwen2.5-vl:3b:4bit` and `qwen2.5-vl:7b:4bit` shortcuts (MLX + GGUF + safetensors). `image_url`, `input_image`, and Anthropic `image` content blocks. Capability gate: 400 on image-to-non-vision-model. |
| Image preflight | Remote URLs fetched server-side with proper UA, rewritten as `data:` URLs, 4xx on failure (no more silent hallucinations on Wikimedia/CDN 403s). |
| Auth | `Authorization: Bearer` + `trusted_networks` CIDR allowlist (RFC1918 by default). `unsafe_disable_auth` for dev. `LMFORGE_REFUSE_UNSAFE_BIND=1` refuses startup on misconfig. |
| Concurrency | `max_concurrent_requests` enforced via tower semaphore → 503 with `Retry-After: 1` on overflow. |
| Observability | `GET /metrics` (Prometheus); `GET /v1/models/{id}`. |
| Logging | Per-model engine logs `engine-<sanitized_id>.{stdout,stderr}.log` with size-based rotation (env-tunable). `lmforge clean --logs --max-mb N`. |
| Warming | Consumers warm via `POST /lf/model/switch` on startup. Operators can set `[orchestrator] auto_load = [...]` for shared multi-tenant deployments (serial, logged). |
| Ollama | `/api/chat` streaming now emits proper NDJSON with `done`/`done_reason`/`total_duration`/`thinking` fields (was leaking raw OpenAI SSE before). |
| Downloads | sha256 verification against HuggingFace `X-Linked-Etag` for LFS files; corrupt downloads auto-deleted. |
| Container | CPU/llama.cpp `Dockerfile` ships from this repo. |

---

### Thinking Models

For models with reasoning capability (Qwen3 Thinking, DeepSeek-R1 distillations,
Phi-4 Reasoning, etc.), LMForge implements a **two-call thinking-budget workflow**
that caps how long the model reasons before it answers. It runs across engines
(oMLX, llama.cpp, SGLang).

**Catalog hint:** shortcuts with `:thinking` or `:reasoning` set
`capabilities.native_reasoning = true`. Thinking stays **on** for those models
(Playground shows a locked gray toggle). Toggleable thinking applies to models
that support optional reasoning without being dedicated reasoning builds.

#### How it works

1. **Call 1 — Thinking phase:** The model generates reasoning tokens (`<think>…</think>`)
   up to `thinking_budget` tokens. Reasoning deltas stream live to the client if
   `stream_reasoning_deltas: true`.
2. **Call 2 — Answer phase:** Once the budget is exhausted, LMForge appends the
   accumulated reasoning as a closed `<think>…</think>` assistant turn and sends a second
   request with `enable_thinking: false`. The answer streams directly to the client
   token-by-token. A `call2_prefill` status event marks the switch for UI feedback.

#### Request parameters

| Parameter | Type | Description |
|---|---|---|
| `think` | bool | Enable thinking mode. Required for the budget path. |
| `thinking_budget` | int | Max reasoning tokens (Call 1 cap). Triggers the two-call path when set. |
| `stream_reasoning_deltas` | bool | Forward live reasoning tokens to the client during Call 1. |
| `stream` | bool | Must be `true` for the two-call path. |

#### Example

```bash
curl http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3.5:4b:4bit",
    "messages": [{"role": "user", "content": "Explain quantum entanglement."}],
    "think": true,
    "thinking_budget": 4096,
    "stream_reasoning_deltas": true,
    "stream": true
  }'
```

The response stream contains three phases:

```
# Phase 1 — live reasoning tokens (delta.reasoning_content)
data: {"choices":[{"delta":{"reasoning_content":"Let me think..."}}]}

# Between phases — call2_prefill status event (ignored by standard clients)
data: {"choices":[{"delta":{}}],"lmforge":{"status":"call2_prefill","reasoning_len":12453}}

# Phase 2 — answer tokens (delta.content)
data: {"choices":[{"delta":{"content":"Quantum entanglement is..."}}]}
```

The `lmforge.status = "call2_prefill"` event marks the start of the KV-cache prefill
gap (typically 5–15 seconds for 4B models with a 4096-token budget). Clients can use
this to show a "Generating answer…" indicator. Standard OpenAI clients ignore the
`lmforge` extension field — it is fully backward-compatible.

> **Note:** The two-call orchestrator runs on the thinking engines that respond to
> `enable_thinking` — oMLX (native `reasoning_content`) and llama.cpp / SGLang
> (inline `<think>` tags, which the daemon splits into reasoning vs. answer). It
> guarantees an answer phase even when the reasoning budget is exhausted, so these
> models can't return a blank answer (the prior single-call llama.cpp path could
> burn `max_tokens` inside `<think>` and stream nothing).
>
> **Blank-reply caveat — native-reasoning models.** Models that *always* reason and
> ignore `enable_thinking` (`phi4:reasoning`, `qwen3:Nb:thinking`, DeepSeek-R1
> distills) stay on a single call — the orchestrator's `enable_thinking:false`
> toggle is a no-op for them. With a small `max_tokens` they can spend the whole
> budget thinking and emit no answer (`finish_reason:length`, empty content) — the
> same way OpenAI's o-series and Anthropic's extended-thinking do. LMForge mitigates
> this by flooring `max_tokens` to **4096** for these models (industry-standard
> headroom; never lowers a larger value). A deliberately tiny `max_tokens` can still
> truncate — that's inherent to reasoning models, not a recoverable error.

#### Sampling (avoid reasoning loops)

LMForge is **client-owns-sampling**: it never overrides a value you send. For
`think:true` requests that arrive with *no* sampling, it seeds the thinking
profile below (absent fields only) so reasoning stays bounded out of the box;
sending your own values always wins. With only `temperature` + `max_tokens` and
no defaults, Qwen3-class models loop on reasoning (`… Wait. Wait. Wait. …`), burn
the whole `thinking_budget`, and the answer echoes the thinking. The profile:

| Param | Chat | Thinking |
|---|:---:|:---:|
| `temperature` | `0.7` | `0.6` (≥ 0.6 required) |
| `top_p` | `0.95` | `0.95` |
| `top_k` | `20` | `20` |
| `repetition_penalty` | `1.1` | `1.2` (loop breaker) |
| `presence_penalty` | `0.0` | `0.3` |

```bash
curl -sN http://127.0.0.1:11430/v1/chat/completions -H "Content-Type: application/json" -d '{
  "model": "qwen3.5:4b:4bit", "stream": true, "think": true, "thinking_budget": 2048,
  "messages": [{"role":"user","content":"A bat and ball cost $1.10; the bat is $1 more than the ball. Cost of the ball?"}],
  "temperature": 0.6, "top_p": 0.95, "top_k": 20, "repetition_penalty": 1.2, "presence_penalty": 0.3
}'
```

The bundled Playground UI and Postman collection ship these profiles by default.
Sampling fixes the engine-default loop but won't make a tiny model reason: prefer
**≥ 4B** models for `think:true` and use `think:false` for chat (guidance, not
enforced — LMForge runs any model). Full rationale, the cross-platform benchmark,
and tuning notes: [docs/dev/DEV_GUIDE.md → Sampling & thinking](docs/dev/DEV_GUIDE.md#sampling--thinking).

### Speculative decoding (MTP)

GGUF models with Multi-Token Prediction heads (catalog shortcuts containing
`:mtp`, or tensors detected on pull) advertise `capabilities.mtp = true`. On
llama.cpp, LMForge enables `--spec-type draft-mtp` when VRAM headroom allows.
Clients keep using the same `/v1/chat/completions` API — no extra request fields.

See [ADR-005](docs/architecture/ADR-005-speculative-decoding.md) for the
admission rules and probe precedence.

---

## Model Catalog

LMForge includes a curated catalog of shortcut names that resolve to the right HuggingFace repo and quantisation format for your hardware automatically.

Shortcuts follow the pattern **`family:size:quant`** (with an optional **`:variant`** suffix for special builds):

```
qwen3:8b:4bit               # Qwen3 8B, 4-bit quantization
gemma3:4b:4bit              # Gemma 3 4B, 4-bit
nomic-embed-text:v1.5       # Nomic embedding, version 1.5
qwen3-reranker:0.6b:q4      # Qwen3 Reranker 0.6B, Q4 quant
llama4:17b:4bit:scout       # Llama 4 Scout variant
```

```bash
lmforge catalog                    # list all shortcuts
lmforge catalog --search qwen      # search by name/family
```

### Chat / Instruction Models

| Shortcut | macOS (MLX) | Linux / Windows (GGUF) |
|---|---|---|
| `qwen3:8b:4bit` | `mlx-community/Qwen3-8B-4bit` | `bartowski/Qwen3-8B-GGUF` |
| `qwen3.5:2b:4bit` | `mlx-community/Qwen3.5-2B-4bit` | `Qwen/Qwen3.5-2B-GGUF` |
| `qwen3.5:4b:4bit` | `mlx-community/Qwen3.5-4B-4bit` | `Qwen/Qwen3.5-4B-GGUF` |
| `qwen3.5:9b:4bit` | `mlx-community/Qwen3.5-9B-OptiQ-4bit` | `bartowski/Qwen3.5-9B-OptiQ-GGUF` |
| `gemma3:1b:4bit` | `mlx-community/gemma-3-1b-it-4bit` | `bartowski/gemma-3-1b-it-GGUF` |
| `gemma3:4b:4bit` | `mlx-community/gemma-3-4b-it-4bit` | `bartowski/gemma-3-4b-it-GGUF` |
| `gemma3:12b:4bit` | `mlx-community/gemma-3-12b-it-4bit` | `bartowski/gemma-3-12b-it-GGUF` |
| `gemma4:e4b:4bit` | `mlx-community/gemma-4-e4b-it-4bit` | `bartowski/gemma-4-e4b-it-GGUF` |
| `llama3.1:8b:4bit` | `mlx-community/Meta-Llama-3.1-8B-Instruct-4bit` | `bartowski/Meta-Llama-3.1-8B-Instruct-GGUF` |
| `llama4:17b:4bit:scout` | `mlx-community/Llama-4-Scout-17B-16E-Instruct-4bit` | `bartowski/Llama-4-Scout-17B-16E-Instruct-GGUF` |
| `phi4:4b:4bit` | `mlx-community/Phi-4-mini-instruct-4bit` | `bartowski/Phi-4-mini-instruct-GGUF` |
| `deepseek_r1:8b:4bit:distill-qwen` | `mlx-community/DeepSeek-R1-Distill-Qwen-8B-4bit` | `unsloth/DeepSeek-R1-Distill-Qwen-8B-GGUF` |

### Embedding Models

| Shortcut | macOS (MLX) | Linux / Windows (GGUF) |
|---|---|---|
| `nomic-embed-text:v1.5` | `mlx-community/nomic-embed-text-v1.5-mlx` | `nomic-ai/nomic-embed-text-v1.5-GGUF` |
| `nomic-modernbert-embed:4bit` | `mlx-community/nomicai-modernbert-embed-base-4bit` | — |
| `nomic-modernbert-embed:f16` | — | `nomic-ai/nomic-modernbert-embed-base-GGUF` |
| `snowflake-arctic-embed-l:v2:4bit` | `mlx-community/snowflake-arctic-embed-l-v2.0-4bit` | — |
| `qwen3-embed:0.6b:4bit` | `mlx-community/Qwen3-Embedding-0.6B-4bit-DWQ` | `Qwen/Qwen3-Embedding-0.6B-GGUF` |
| `qwen3-embed:4b:4bit` | `mlx-community/Qwen3-Embedding-4B-4bit-DWQ` | `Qwen/Qwen3-Embedding-4B-GGUF` |
| `bge-m3:f16` | — | `gpustack/bge-m3-GGUF` |

### Vision-Language Models (VLMs)

| Shortcut | macOS (MLX) | Linux/Windows (GGUF + mmproj) | Linux (safetensors / SGLang) |
|---|---|---|---|
| `qwen2.5-vl:3b:4bit` | `mlx-community/Qwen2.5-VL-3B-Instruct-4bit` | `bartowski/Qwen2.5-VL-3B-Instruct-GGUF` | — |
| `qwen2.5-vl:7b:4bit` | `mlx-community/Qwen2.5-VL-7B-Instruct-4bit` | `bartowski/Qwen2.5-VL-7B-Instruct-GGUF` | — |
| `qwen2.5-vl:7b`      | — | — | `Qwen/Qwen2.5-VL-7B-Instruct` |

GGUF VLM entries automatically pull the multimodal projector (`mmproj-*.gguf`)
alongside the main weights.

### Re-ranking Models (llama.cpp only)

| Shortcut | GGUF Repo |
|---|---|
| `bge-reranker-v2-m3:f16` | `gpustack/bge-reranker-v2-m3-GGUF` |
| `bge-reranker:large:f16` | `gpustack/bge-reranker-large-GGUF` |
| `jina-reranker-v2:f16` | `gpustack/jina-reranker-v2-base-multilingual-GGUF` |
| `qwen3-reranker:0.6b:q4` | `Qwen/Qwen3-Reranker-0.6B-GGUF` |
| `qwen3-reranker:4b:q4` | `Qwen/Qwen3-Reranker-4B-GGUF` |

> **Re-ranking** requires llama.cpp with `--reranking`. The `/v1/rerank` endpoint returns 501 on oMLX and SGLang. See "Re-ranking on Linux + NVIDIA" under [Configuration](#configuration) for workarounds.

You can also pull any HuggingFace repo directly by its full path:

```bash
lmforge pull mlx-community/Qwen3-8B-4bit
lmforge pull bartowski/Qwen3-8B-GGUF
```

---

## Configuration

Global config file: `~/.lmforge/config.toml`

```toml
schema_version = 2

port          = 11430
bind_address  = "127.0.0.1"
log_level     = "info"

# Auth: requests from these CIDRs bypass api_key entirely.
# Defaults cover loopback + RFC1918 private LAN, so a fresh install binding
# 0.0.0.0 works on any home/office network without a token. External requests
# (outside trusted_networks) still need `Authorization: Bearer <api_key>` when
# api_key is set, or are rejected with 401 when it isn't.
trusted_networks = [
    "127.0.0.0/8", "::1/128",
    "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
    "fc00::/7", "fe80::/10",
]
# api_key = "..."             # optional Bearer token for external clients
# unsafe_disable_auth = false # dev escape hatch — opens the daemon completely

[resources]
max_gpu_memory_fraction  = 0.75   # fraction of VRAM available to LMForge
max_concurrent_requests  = 4      # in-flight cap; excess requests get 503
request_queue_size       = 32     # how long to wait for a permit (~100ms each)
max_request_body_mb      = 32     # HTTP body cap; 413 above. Raise for high-DPI VLM payloads.
min_free_disk_gb         = 10

[orchestrator]
keep_alive        = "5m"          # unload idle models after this duration
max_loaded_models = 0             # 0 = unlimited (bounded by VRAM)
embed_batch_size  = 32            # max inputs per engine call for /v1/embeddings

# Operator pre-warm (optional). Leave empty for single-consumer setups —
# consumers should warm their own models via POST /lf/model/switch on
# startup so the model set lives with the app, not with the daemon.
# Use this only when running LMForge as a shared multi-tenant service.
# Models cold-load serially at daemon startup; order matters when VRAM is
# tight (load larger models first).
# auto_load = []
```

### Environment knobs

| Variable | Default | Purpose |
|---|---|---|
| `LMFORGE_BIND` | unset | Override `bind_address` from the env (CLI `--bind` still wins). Useful in containers. |
| `LMFORGE_API_KEY` | unset | Override `api_key` from the env. Takes precedence over `config.toml` when both are set. |
| `LMFORGE_UI_DIR` | container: `/usr/local/share/lmforge/ui`; native: repo `ui/build` if present | Path to the SvelteKit static bundle served at `/ui`. Unset and missing → route is disabled. |
| `LMFORGE_REFUSE_UNSAFE_BIND` | `0` | Refuse startup when bind is non-loopback and no `api_key`/`trusted_networks` are configured. |
| `LMFORGE_IMAGE_MAX_BYTES` | `20971520` (20 MB) | Per-image cap for the chat preflight. |
| `LMFORGE_MAX_BODY_MB` | `32` | HTTP request body cap in MB (overrides `max_request_body_mb`). Floored at 1 MB. |
| `LMFORGE_ENGINE_LOG_MAX_MB` | `50` | Rotate per-model engine logs above this size. |
| `LMFORGE_ENGINE_LOG_KEEP` | `3` | How many rotated copies to retain per stream. |
| `LMFORGE_SGLANG_MEM_FRACTION` | `0.5` | SGLang `--mem-fraction-static` (raise to `0.85` for single-slot deployments). |
| `LMFORGE_LLAMACPP_NGL` | auto | Force `-ngl <N>` for `llama-server` (0..=99). Default is computed from free VRAM and model size. Set to `0` to disable GPU offload entirely; set to `99` to force full offload. |
| `LMFORGE_LLAMACPP_CTX` | auto | Force `--ctx-size <N>` for VLM (mmproj) loads. Default scales 1024 → 8192 with post-load free VRAM. Values below 512 are ignored. |
| `HF_TOKEN` / `HUGGING_FACE_HUB_TOKEN` | unset | Used by the downloader for gated repos. |

### Observability

- `GET /health` — bypasses auth; reports daemon and engine status.
- `GET /metrics` — Prometheus exposition (`text/plain; version=0.0.4`); also auth-bypassed. Counters and histograms include `lmforge_requests_total{endpoint,status}`, `lmforge_request_duration_seconds`, `lmforge_model_loads_total{model,result}`, `lmforge_model_load_duration_seconds`, `lmforge_active_models`, `lmforge_image_inputs_total{result}`, and `lmforge_auth_rejections_total`.
- `GET /lf/metrics` — JSON digest of the same data, shaped for dashboards (per-endpoint p50/p95/p99, error rate, model-load history, image preflight mix). Stable schema; safe for client UIs.
- `GET /lf/logs/list` — discover available log streams (daemon + per-model engines).
- `GET /lf/logs/tail?component=<id>&stream=stdout|stderr|main&lines=200` — last N lines as plain text. Bounded at 5000 lines / 2 MB.
- `GET /lf/logs/stream?component=<id>&stream=…` — SSE follow that emits each new appended line. Detects rotation and resumes from the new file.
- `GET /ui/` — embedded SvelteKit dashboard. Serves an Observability page with live KPIs, per-endpoint latency table, and an in-browser log tail. Auth-bypassed (static assets only); `/lf/*` and `/v1/*` calls from the page still go through the same Bearer/CIDR rules.
- `~/.lmforge/logs/engine-<model>.{stdout,stderr}.log` — one file per model id (sanitized: `:` and `/` → `_`); rotated when files exceed `LMFORGE_ENGINE_LOG_MAX_MB`. Older copies are `.1`, `.2`, … and pruned beyond `LMFORGE_ENGINE_LOG_KEEP`.
- `lmforge clean --logs --max-mb 100` — deletes oldest log files until total size ≤ 100 MB. Without `--max-mb` the legacy behaviour (truncate-all) still applies.

### Image preflight

Chat requests with `image_url.url` pointing at remote `http(s)://` images are
fetched server-side (real `User-Agent`, 15 s timeout) and rewritten as inline
`data:` URLs before reaching the engine. This stops engines from silently
failing on hosts that block empty UAs (Wikimedia, several CDNs). Failures
return a 400 with `code:image_fetch_failed`; oversized payloads return 413.

### Container image

The repo ships a CPU/llama.cpp `Dockerfile` (multi-stage; debian-slim runtime
with `llama-server` baked in **plus the SvelteKit dashboard built into the
image and served at `/ui`**). It does **not** include oMLX (Apple-only) or
SGLang (CUDA-only) — see below for status on a CUDA variant.

#### Build & run

```bash
docker build -t lmforge:cpu .

# Bind a host volume for persistent state (models, logs, config).
docker volume create lmforge-data

docker run -d --name lmforge \
  -p 11430:11430 \
  -v lmforge-data:/root/.lmforge \
  lmforge:cpu
```

`/root/.lmforge` holds everything the daemon needs across restarts:

| Path | Contents |
|---|---|
| `/root/.lmforge/config.toml` | Daemon config (auto-created with defaults if absent) |
| `/root/.lmforge/models/` | Downloaded model weights (multi-GB; size the volume accordingly) |
| `/root/.lmforge/models.json` | Model index + capability cache |
| `/root/.lmforge/engines/` | Engine PID files used by `startup_cleanup` |
| `/root/.lmforge/logs/` | Daemon + per-model engine logs (rotated) |

The dashboard itself ships inside the image at
`/usr/local/share/lmforge/ui` and is mounted at `http://<host>:11430/ui/`
when the daemon starts. Override the path with `LMFORGE_UI_DIR=/elsewhere`
or unset it to disable the route entirely.

#### Configuration

Two ways to configure the containerised daemon — environment variables for
the common knobs, or a mounted `config.toml` for everything else.

**Env vars (preferred for orchestrators):**

```bash
docker run -d --name lmforge \
  -p 11430:11430 \
  -v lmforge-data:/root/.lmforge \
  -e LMFORGE_API_KEY="<bearer-token>" \
  -e LMFORGE_REFUSE_UNSAFE_BIND=1 \
  -e LMFORGE_MAX_BODY_MB=64 \
  -e LMFORGE_ENGINE_LOG_MAX_MB=100 \
  -e LMFORGE_ENGINE_LOG_KEEP=5 \
  lmforge:cpu
```

See [Environment knobs](#environment-knobs) for the full list.

**Mounted config file:**

```bash
docker run -d --name lmforge \
  -p 11430:11430 \
  -v lmforge-data:/root/.lmforge \
  -v "$PWD/config.toml:/root/.lmforge/config.toml:ro" \
  lmforge:cpu
```

#### Pulling models inside the container

Models live on the volume and persist across container restarts:

```bash
docker exec lmforge lmforge pull qwen3.5:4b:4bit
docker exec lmforge lmforge pull qwen3-embed:0.6b:8bit
docker exec lmforge lmforge catalog --search vl
```

Or do it from a HuggingFace path directly:

```bash
docker exec lmforge lmforge pull bartowski/Qwen3-8B-GGUF
```

#### docker-compose example

```yaml
services:
  lmforge:
    image: lmforge:cpu
    build: .
    ports:
      - "11430:11430"
    volumes:
      - lmforge-data:/root/.lmforge
    environment:
      LMFORGE_API_KEY: ${LMFORGE_API_KEY:-}
      LMFORGE_REFUSE_UNSAFE_BIND: "1"
      LMFORGE_MAX_BODY_MB: "64"
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://127.0.0.1:11430/health"]
      interval: 15s
      timeout: 3s
      start_period: 10s
      retries: 3

volumes:
  lmforge-data:
```

#### LAN exposure & auth

The image binds `0.0.0.0:11430` so it's reachable from outside the
container. The default `trusted_networks` (loopback + RFC1918) means any
client on the same docker network or LAN reaches the API without a token.
For anything beyond a single-host home setup:

- Set `LMFORGE_API_KEY` (or `api_key` in `config.toml`) — non-trusted
  sources then need `Authorization: Bearer <key>`.
- Set `LMFORGE_REFUSE_UNSAFE_BIND=1` to refuse startup if no key/CIDR is
  configured. Treat this as the default for production.

#### Resource sizing

- **CPU only.** This image deliberately ships llama.cpp without GPU
  acceleration. Expect 1–10 tok/s on small models (1–4 B); use it for
  ops, dev environments, CI smoke tests, or low-throughput workloads.
- **RAM.** Budget ≈1.5× the model file size (e.g. `qwen3.5:4b:4bit` ~2.4
  GB on disk needs ~4 GB resident). Container memory limit must clear
  this plus daemon overhead (~150 MB).
- **Disk.** Catalog VLMs and embedding pairs hit 10–30 GB on the volume.
  Size the volume accordingly.

#### CUDA / SGLang variant

Not yet shipped. Tracking item — for now NVIDIA users should run LMForge
natively on the host (the SGLang adapter starts SGLang as a subprocess and
needs CUDA libraries on the host).

#### Healthcheck

The image declares a 15 s healthcheck against `GET /health`. `docker ps`
reflects `healthy` once the daemon is up; orchestrators (k8s, Nomad,
docker-compose `depends_on: condition: service_healthy`) should rely on
this rather than container start.

### Vision-language models (VLMs)

LMForge serves VLMs through the same `/v1/chat/completions` endpoint as text
models. Send images using OpenAI's content-block format:

```json
{
  "model": "qwen2.5-vl:7b:4bit",
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "Describe this image."},
      {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,<...>"}}
    ]
  }]
}
```

Ollama clients (`/api/chat`) can use the legacy `images: ["<base64>"]` field per
message — LMForge translates them to OpenAI image blocks before forwarding.
Vision-incapable models receive a 400 with `vision_not_supported` instead of
silently confusing the engine.

Curated VLM shortcuts:

| Shortcut                | Engine        | Backbone                     |
|-------------------------|---------------|------------------------------|
| `qwen2.5-vl:3b:4bit`    | oMLX / llama.cpp | Qwen 2.5-VL 3B (Q4_K_M + mmproj-f16) |
| `qwen2.5-vl:7b:4bit`    | oMLX / llama.cpp | Qwen 2.5-VL 7B (Q4_K_M + mmproj-f16) |
| `qwen2.5-vl:7b`         | SGLang (safetensors) | Qwen 2.5-VL 7B FP16 |

GGUF VLMs ship with a multimodal projector sidecar (`mmproj-*.gguf`); LMForge
downloads it alongside the main weights and passes `--mmproj` to llama-server
automatically. SGLang VLMs use `--chat-template` derived from `model_type`.

### Re-ranking on Linux + NVIDIA (SGLang gap)

SGLang v0.5.10's cross-encoder reranker support is experimental and
intentionally disabled in `engines.toml`. On Linux + NVIDIA hosts, `/v1/rerank`
returns 501 because the daemon currently routes all models through one engine.
**Workarounds**: (a) use an LLM-as-reranker via `/v1/chat/completions`, or
(b) run a second LMForge instance on a different port pinned to llama.cpp via
`engines.toml` user override. Multi-engine routing (one engine per model) is
tracked for the next iteration.

Per-directory overrides via `lmforge.yaml`:

```yaml
default_chat_model: qwen3:8b:4bit
default_embed_model: nomic-embed-text:v1.5
```

CLI flags override config:

```bash
lmforge start --port 8080 --log-level debug
```

---

## Building from Source

**Requirements:** Rust 1.78+, Node.js 20+ (UI only)

The quick-start below works on every supported OS. For platform-specific
notes (CUDA toolkit when needed, WSL2 vs native Windows, Apple Silicon
quirks, opt-in engine installs, cleanup procedures) see
[`docs/README.md`](docs/README.md) — install guides, Accepted ADRs, and
maintainer docs. Product narrative: [`docs/product-overview.md`](docs/product-overview.md).
Proposed (not Accepted) ADRs live under `docs/architecture/proposals/`.

```bash
git clone https://github.com/phoenixtb/lmforge
cd lmforge

# Build the CLI/daemon
cargo build --release
./target/release/lmforge init

# Build the desktop UI
cd ui && npm install && npm run tauri build
```

**Run tests:**
```bash
cargo test --lib                        # unit tests (no engine required)
cargo test -- --ignored --nocapture     # live catalog verification (network)
bash tests/multi_model_e2e.sh          # E2E multi-model test suite
```

---

## Developer Guide

This section covers everything you need to develop, debug, and fully clean up a local LMForge installation.

### Prerequisites

| Tool | Min version | Purpose |
|---|---|---|
| Rust | 1.78 | Core daemon + CLI |
| Node.js | 20 | Desktop UI (SvelteKit) |
| npm | 10 | UI package manager |
| Tauri CLI | 2.x | Desktop UI bundler |
| Homebrew | any | macOS: oMLX engine install |

```bash
# Install Tauri CLI (once)
cargo install tauri-cli --version "^2" --locked
```

---

### Local Development Setup

#### 1 — Clone and build the daemon

```bash
git clone https://github.com/phoenixtb/lmforge
cd lmforge

# Debug build (fast compile, verbose logging)
cargo build

# Release build (optimized)
cargo build --release
```

The compiled binary is at `target/debug/lmforge` or `target/release/lmforge`.

#### 2 — Initialise hardware detection

```bash
# Detect hardware, install the right engine, write ~/.lmforge/config.toml
./target/debug/lmforge init
```

This is idempotent — safe to re-run after engine upgrades.

#### 3 — Run the daemon in the foreground (preferred for debugging)

```bash
# Runs in the foreground; Ctrl-C to stop
# Logs go to stdout at whatever level is set in config
./target/debug/lmforge start
cargo run -- start                          # equivalent shorthand

# Override log level without editing config
RUST_LOG=debug ./target/debug/lmforge start
RUST_LOG=debug cargo run -- start           # equivalent shorthand

# Use a different port (e.g. to avoid clashing with an installed daemon)
./target/debug/lmforge start --port 11431
cargo run -- start --port 11431             # equivalent shorthand
```

#### 4 — Quick smoke-tests while the daemon is running

```bash
curl http://127.0.0.1:11430/health
curl http://127.0.0.1:11430/lf/status | jq .
curl http://127.0.0.1:11430/lf/hardware | jq .
```

---

#### 5 — Debug streaming instrumentation (thinking-budget proxy)

The two-call thinking-budget proxy (`proxy_stream_with_thinking_budget`) emits a
`tracing::info!` log line for **every single SSE event** it yields to the client
during Call 2. This is the definitive tool for diagnosing whether bulk-dumping
originates inside LMForge or upstream in the inference engine.

**Important — PATH visibility for external tools (e.g. DocIntel)**

`cargo run` builds and runs the binary at `target/debug/lmforge`, but it does **not**
put it on your `PATH`. Tools like DocIntel check for the `lmforge` binary via `which lmforge`
and will report "binary not found" if it can't find it there.

**Option A — Install to PATH (recommended for integration testing):**

```bash
# Builds an optimised-enough debug binary and installs it to ~/.cargo/bin
cargo install --path . --force

# Run init (safe to re-run — idempotent; sets up config + engine)
lmforge init

# Start with instrumentation
RUST_LOG=lmforge=info lmforge start
```

**Option B — Symlink for fast iteration (avoids reinstalling after every change):**

```bash
# One-time: symlink the debug binary into ~/.cargo/bin
ln -sf $(pwd)/target/debug/lmforge ~/.cargo/bin/lmforge

# After each code change: just rebuild (the symlink updates automatically)
cargo build

# Then start with instrumentation
RUST_LOG=lmforge=info lmforge start
```

With the symlink in place, `cargo build` + `lmforge start` is all you need between iterations.

**While the daemon is running, trigger a thinking-budget request** (e.g. from
DocIntel or with curl). Then watch the logs for lines like:

```
INFO lmforge::server::proxy: call2 SSE event yielded call2_event_n=1 payload_bytes=42
INFO lmforge::server::proxy: call2 SSE event yielded call2_event_n=2 payload_bytes=38
...
INFO lmforge::server::proxy: Call-2 stream complete (received [DONE]) call2_total_events=148
```

**Interpreting the output:**

| Observation | Meaning |
|---|---|
| Many events (100+), small `payload_bytes` (< 100) | LMForge is streaming token-by-token — dump is upstream or downstream |
| Single event with large `payload_bytes` (> 500) | Engine is batching; dump is inside `mlx_lm.server` |
| No `call2 SSE event` lines appear | Budget not being exhausted; check `Call-1 accumulation complete` log for `finish_reason` |

**Filter just the Call 2 diagnostics:**

```bash
RUST_LOG=lmforge=info ./target/debug/lmforge start 2>&1 | grep -E "call2|Call-1|Call-2"
RUST_LOG=lmforge=info cargo run -- start 2>&1 | grep -E "call2|Call-1|Call-2"  # equivalent
```

**If you only want the per-event byte count summary:**

```bash
RUST_LOG=lmforge=info ./target/debug/lmforge start 2>&1 | \
  grep "call2 SSE event yielded" | \
  awk -F'payload_bytes=' '{print "Event " NR ": " $2 " bytes"}'
```

---

### Desktop UI Development

The UI is a SvelteKit app wrapped in Tauri. There are two ways to run it:

#### Hot-reload dev server (recommended)

```bash
# Terminal 1 — daemon must be running first
./target/debug/lmforge start

# Terminal 2 — Tauri dev mode (hot-reloads on every file save)
cd ui
npm install          # first time only
npm run tauri dev
```

The Tauri shell connects to the Vite dev server on `http://localhost:1420`. Svelte component changes auto-reload without restarting the daemon.

#### Vite-only (browser, no Tauri APIs)

```bash
cd ui
npm run dev
# Opens http://localhost:1420 in your browser
```

Note: Tauri-specific features (system tray, dialog picker, native events) are unavailable in the browser — use `npm run tauri dev` for full fidelity.

#### Build the production bundle

```bash
cd ui
npm run tauri build
# Outputs: ui/src-tauri/target/release/bundle/
```

---

### Useful `cargo` Invocations

```bash
# Check for compile errors without producing a binary (very fast)
cargo check

# Lint (treat warnings as errors — same as CI)
cargo clippy -- -D warnings

# Format
cargo fmt

# Run only unit tests (no engine required)
cargo test --lib

# Run integration tests (daemon is started and stopped by the harness)
cargo test --test integration

# Run the Rust multi-model orchestration test suite
cargo test --test multi_model

# Run tests with output (don't suppress println!)
cargo test -- --nocapture

# Watch mode (requires cargo-watch)
cargo watch -x check
cargo watch -x 'test --lib'

# Install the current build globally (replaces any previous install)
cargo install --path . --force

# Verify every catalog entry is freely downloadable (hits HuggingFace live)
cargo test -- --ignored --nocapture
```

---

### Testing

#### Unit tests

Live alongside the source in `src/**/*.rs`:

```bash
cargo test --lib
```

#### Integration tests (Rust harness)

`tests/integration.rs` and `tests/multi_model.rs` start a real daemon, run requests, and validate responses. They require the engine to be installed (`lmforge init` must have run).

```bash
cargo test --test integration
cargo test --test multi_model
```

To run a single test by name:

```bash
cargo test --test integration -- embed_roundtrip --nocapture
```

#### End-to-end shell tests

```bash
# Single model smoke test
bash tests/e2e.sh

# Full multi-model orchestration suite (generates JSON + Markdown reports)
bash tests/multi_model_e2e.sh
# Reports saved to: tests/integration/reports/
```

#### Catalog verification (live HuggingFace check)

Ensures every shortcut in `gguf.json` and `mlx.json` resolves to a real,
freely downloadable file — no HF token required:

```bash
# Checks all entries against the live HF CDN (HTTP HEAD, no auth)
# ✓ 200 = confirmed free   ✗ 401/404 = must be removed from catalog
cargo test -p lmforge -- --ignored --nocapture
```

Run this whenever you add or change a catalog entry.

#### Testing with a custom catalog

```bash
# Point to a local catalog directory for testing new shortcuts
./target/debug/lmforge start --catalog-dir ./data/catalogs
```

---

### Environment Variables

| Variable | Default | Effect |
|---|---|---|
| `RUST_LOG` | `info` | Log verbosity: `error`, `warn`, `info`, `debug`, `trace` |
| `LMFORGE_CONFIG` | `~/.lmforge/config.toml` | Override config file path |
| `LMFORGE_DATA_DIR` | `~/.lmforge` | Override the data directory (engines, logs, `models.json`). Also settable via `--data-dir` or `data_dir` in config.toml. |
| `LMFORGE_MODELS_DIR` | `{data_dir}/models` | Override the model **weights** directory only. Also settable via `--models-dir` or `models_dir` in config.toml. Point this at a shared volume to reuse one weights library across machines. |
| `LMFORGE_PORT` | `11430` | Override the API port |

Precedence for the storage dirs: CLI flag > env var > config.toml > default.
Changing them via the UI / `POST /lf/config` is persisted but takes effect on the next daemon restart.

---

### Runtime Data Layout

Everything LMForge writes at runtime lives in `~/.lmforge/`:

```
~/.lmforge/
├── config.toml          # User configuration (editable)
├── hardware.json        # Cached hardware probe result
├── models.json          # Index of all downloaded models + metadata
├── lmforge.pid          # PID of the running daemon (deleted on clean stop)
├── models/              # Downloaded model weights (one dir per model)
│   ├── qwen3.5-4b-4bit/
│   └── …
├── engines/             # Installed engine binaries managed by LMForge
│   └── (oMLX, llama.cpp, etc.)
├── bin/                 # LMForge helper binaries (e.g. GPU probe)
│   └── lmforge-gpu-probe-aarch64-apple-darwin
└── logs/                # Daemon log files
    └── lmforge.log
```

`models/` can be relocated independently of the data root via `models_dir`
(`LMFORGE_MODELS_DIR` / `--models-dir` / config). The rest of the layout stays
under `data_dir`.

#### Sharing a weights library across VMs (virtio-fs)

Keep `data_dir` local per machine (engines, venvs, logs, index) and share only
the weights volume:

```
Host:     /srv/lmforge-models        (virtio-fs export)
Linux VM: LMFORGE_MODELS_DIR=/mnt/lmforge-models
Windows VM: LMFORGE_MODELS_DIR=D:\lmforge-models
```

The index (`models.json`) stores per-model paths **relative to `models_dir`**
(schema v2), so the same physical volume works across OSes with different mount
points. On a freshly pointed VM, run `lmforge models scan` to (re)build the
index from the weights already present on the volume.

---

### Full Cleanup (Development Reset)

The commands below progressively remove LMForge artifacts. **Models are large** — remove them explicitly only when you intend to.

#### Stop the daemon

```bash
# Graceful stop (if running via service)
lmforge service stop

# Or kill the foreground process with Ctrl-C

# Or kill by PID
kill $(cat ~/.lmforge/lmforge.pid 2>/dev/null) 2>/dev/null
```

#### Remove the system service (if registered)

```bash
# macOS
launchctl unload ~/Library/LaunchAgents/com.lmforge.daemon.plist 2>/dev/null
rm -f ~/Library/LaunchAgents/com.lmforge.daemon.plist

# Linux
systemctl --user stop lmforge 2>/dev/null
systemctl --user disable lmforge 2>/dev/null
rm -f ~/.config/systemd/user/lmforge.service
systemctl --user daemon-reload
```

#### Remove the installed binary

```bash
# Installed via install-core.sh
rm -f /usr/local/bin/lmforge

# Or if installed via cargo install
cargo uninstall lmforge
```

#### Remove the desktop UI

```bash
# macOS — drag LMForge.app from /Applications to Trash, or:
rm -rf /Applications/LMForge.app

# Linux AppImage
rm -f ~/Applications/LMForge*.AppImage
```

#### Remove runtime data (config, logs, model index, engine binaries)

```bash
# Keeps downloaded models — just cleans state files and engines
rm -f ~/.lmforge/config.toml
rm -f ~/.lmforge/hardware.json
rm -f ~/.lmforge/models.json
rm -f ~/.lmforge/lmforge.pid
rm -rf ~/.lmforge/engines/
rm -rf ~/.lmforge/bin/
rm -rf ~/.lmforge/logs/
```

#### Remove downloaded models

```bash
# ⚠ This deletes all model weights — they must be re-downloaded
rm -rf ~/.lmforge/models/

# Or remove one specific model
rm -rf ~/.lmforge/models/qwen3.5-4b-4bit/
```

#### Purge everything

```bash
# Complete wipe — removes all models, config, engines, logs
rm -rf ~/.lmforge/
```

#### Clean Cargo build artefacts

The repo has **two independent Cargo workspaces**:

| Manifest | Output | Built by |
|---|---|---|
| `Cargo.toml` (root) | `target/` | `cargo build` for the daemon + CLI |
| `ui/src-tauri/Cargo.toml` | `ui/src-tauri/target/` | `npm run tauri build` for the desktop app |

`cargo clean` only touches the workspace it is invoked from, so cleaning the
root does **not** delete the Tauri build (which contains a `LMForge.app`
that Spotlight will keep indexing until removed).

```bash
# Clean only the daemon/CLI workspace
cargo clean

# Clean only the Tauri desktop app workspace
cargo clean --manifest-path ui/src-tauri/Cargo.toml

# Clean only the UI node_modules and dist
rm -rf ui/node_modules ui/.svelte-kit ui/build

# Full clean in one shot (daemon + Tauri + node)
cargo clean \
  && cargo clean --manifest-path ui/src-tauri/Cargo.toml \
  && rm -rf ui/node_modules ui/.svelte-kit ui/build
```

> **Tip — keep Spotlight quiet:** if `LMForge.app` keeps appearing in
> Spotlight after you uninstall the released app, it's probably a leftover
> Tauri build under `ui/src-tauri/target/.../bundle/macos/`. Either run the
> full clean above, or exclude the project folder from Spotlight via
> *System Settings → Siri & Spotlight → Spotlight Privacy*.

#### Quick dev reset (no model loss)

Stop daemon → remove state files → rebuild → reinit:

```bash
kill $(cat ~/.lmforge/lmforge.pid 2>/dev/null) 2>/dev/null || true && \
rm -f ~/.lmforge/models.json ~/.lmforge/hardware.json ~/.lmforge/lmforge.pid && \
cargo build && \
./target/debug/lmforge init && \
./target/debug/lmforge start
```

Or, rebuild and install globally in one shot:

```bash
cargo install --path . --force && lmforge init && lmforge start
```

And, uninstall lmforge from previous local install and install again (not touching downloaded models):

```bash
cargo uninstall lmforge && \
cargo install --path . --force && \
lmforge init && \
lmforge start
```

---

## Data & Privacy

LMForge runs **entirely on your machine**. There is no telemetry, no analytics, no cloud sync. Models are downloaded directly from HuggingFace to `~/.lmforge/models/` and inference runs locally. The API binds to `127.0.0.1` by default and is never exposed to the network.

---

## Contributing

Contributions are welcome. Please:

1. **Open an issue first** for significant changes — discuss the approach before coding
2. Fork the repo and create a feature branch from `main`
3. Write tests for new functionality (unit tests in `src/`, integration tests in `tests/`)
4. Ensure `cargo fmt`, `cargo clippy`, and `cargo test` all pass
5. Submit a PR with a clear description of what changed and why

### Project Structure

```
lmforge/
├── src/
│   ├── cli/           # CLI subcommands (start, stop, pull, service, …)
│   ├── config/        # Configuration loading and merging
│   ├── engine/        # Engine adapters (oMLX, llama.cpp, SGLang) + manager
│   │   └── adapters/
│   ├── hardware/      # GPU/CPU/memory probing
│   ├── model/         # Model index, resolver, catalog
│   └── server/        # Axum HTTP server, OpenAI/Ollama/LF route handlers
├── data/
│   └── catalogs/      # mlx.json, gguf.json — shortcut → HF repo mappings
├── ui/
│   ├── src/           # SvelteKit frontend
│   └── src-tauri/     # Tauri shell (HTTP client — no daemon code)
├── docs/              # Product overview, ADRs, install / release guides
├── tests/             # Integration test suite (Rust + shell)
├── scripts/           # Install / uninstall scripts
└── .github/workflows/ # CI (ci.yml) and release (release.yml) pipelines
```

Docs index: [`docs/README.md`](docs/README.md).

### Opening Issues

- **Bug reports**: include `lmforge status`, `lmforge logs --tail 50`, and your OS/hardware
- **Feature requests**: describe the use case, not just the solution
- **Model compatibility**: include the model repo URL and the error from `lmforge logs`

---

## License

MIT — see [LICENSE](LICENSE).

---

<div align="center">

<img src="docs/assets/logo.png" width="48" alt="LMForge" />

Made for developers who want local AI to work like infrastructure —  
always on, always fast, never in the way.

**[phoenixtb/lmforge](https://github.com/phoenixtb/lmforge)**

</div>
