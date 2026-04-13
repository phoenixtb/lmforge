# LMForge — Software Requirements Specification

**Version:** 0.2 Draft  
**Date:** 2026-03-27  
**Status:** Ready for implementation  
**Intended readers:** Claude Code (primary), human developers (secondary)

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [System Overview](#2-system-overview)
3. [Hardware Detection & Engine Routing](#3-hardware-detection--engine-routing)
4. [Engine Management](#4-engine-management)
5. [API Surface](#5-api-surface)
6. [Model Management](#6-model-management)
7. [DocIntel Integration Requirements](#7-docintel-integration-requirements)
8. [Installation & Distribution](#8-installation--distribution)
9. [SDK Specification](#9-sdk-specification)
10. [Tauri UI (v0.2)](#10-tauri-ui-v02)
11. [Non-Functional Requirements](#11-non-functional-requirements)
12. [Logging & Observability](#12-logging--observability)
13. [Upgrade & Migration](#13-upgrade--migration)
14. [Resource Limits](#14-resource-limits)
15. [Testing Strategy](#15-testing-strategy)
16. [Phased Delivery](#16-phased-delivery)
17. [Open Questions & Deferred Decisions](#17-open-questions--deferred-decisions)

---

## 1. Introduction

### 1.1 Purpose

This document specifies the functional and non-functional requirements for **LMForge** — a hardware-aware local LLM orchestrator that automatically selects, installs, and supervises the optimal inference engine for the host machine. It is the authoritative reference for the initial implementation sprint and is intended to be handed directly to an AI coding agent (Claude Code) for construction.

### 1.2 Project Vision

Developers building AI-powered applications — RAG pipelines, agentic systems, document intelligence tools — spend disproportionate time configuring local inference infrastructure. The correct engine (`oMLX`, `llama.cpp`, `SGLang`) varies by hardware, and every project rediscovers this from scratch.

LMForge solves this once. It probes the hardware, routes to the right engine, pulls the right model format from Hugging Face, and exposes a unified OpenAI-compatible API — so any consuming application simply points at `http://localhost:11430/v1` and works, regardless of the host machine.

### 1.3 Scope

LMForge is scoped to the following concerns:

- Hardware detection and engine routing
- Download and lifecycle management of engine binaries and environments (no source compilation)
- Model download from Hugging Face Hub (engine-format-aware) and custom sources
- OpenAI-compatible and Ollama-compatible API proxy
- Lightweight system-tray application (Tauri v2 + Svelte) for model management
- CLI for headless / CI environments
- Python and Dart SDK wrappers

**Out of scope:** model training, model format conversion, model hosting, Ollama model format support.

### 1.4 Known Consumers

The following first-party projects will integrate LMForge from v0.1:

| Project | Integration Type | Key Requirement |
|---|---|---|
| **DocIntel** | OpenAI-compatible REST via `LLM_ENGINE_URL` env var | Chat completions (streaming + non-streaming), embeddings, thinking mode, `/v1/models` with capabilities |
| **CodeForge** | Python SDK / HTTP REST | Agentic prefix caching (SGLang RadixAttention on NVIDIA), oMLX on Apple Silicon |

---

## 2. System Overview

### 2.1 Component Map

LMForge is a **single compiled Rust binary** containing the following logical components:

| Component | Responsibility | Implementation |
|---|---|---|
| `hardware_probe` | Detect OS, CPU arch, GPU vendor, VRAM | `sysinfo` + `nvml-wrapper` crates |
| `engine_registry` | Map hardware profiles to engines; loads embedded defaults and user overrides | Embedded default + runtime `~/.lmforge/engines.toml` overlay |
| `engine_installer` | Download pre-built engine binaries or install pip-managed environments | `reqwest` + SHA256 verification |
| `engine_manager` | Spawn, supervise, and restart engine child processes | `tokio::process` |
| `api_server` | OpenAI + Ollama-compatible proxy | `axum` + `hyper`, SSE streaming |
| `model_manager` | Model resolution, HF download, local index | HF Hub API + `models.json` index |
| `ui` *(optional)* | Tauri shell with Svelte frontend, system tray | Tauri v2, compiled as feature flag |

### 2.2 Architecture Diagram

```
lmforge (single Rust binary)
│
├── hardware_probe
│     └── sysinfo crate → GPU vendor, VRAM, arch, OS
│
├── engine_registry
│     ├── embedded default engines.toml (compile-time)
│     └── ~/.lmforge/engines.toml (runtime override / extension)
│         maps {hardware profile} → {engine + install method + start args}
│
├── engine_installer
│     └── downloads pre-built engine binaries from GitHub Releases
│         installs pip-managed engines in isolated venvs
│         no source compilation, no cargo
│
├── engine_manager
│     └── spawns engine as child process
│         restarts on crash, streams logs
│         tracks per-component health (chat engine, embedding sidecar)
│
├── api_server (axum)
│     ├── /v1/...   → OpenAI-compatible
│     ├── /api/...  → Ollama-compatible (drop-in replacement)
│     └── /lf/...   → LMForge native (health, model management)
│
└── ui (Tauri v2 — optional feature flag)
      └── Svelte model browser + system tray
```

### 2.3 Engine Process Architecture

```
LMForge api_server (:11430)
  │
  ├── /v1/chat/completions  ──▶  primary engine (oMLX / SGLang / llama.cpp)
  │
  └── /v1/embeddings        ──▶  primary engine (if it supports embeddings natively)
                                  OR embedding sidecar (:11432)
                                     └── infinity-emb or mlx-embeddings
                                         (spawned and supervised by engine_manager)
```

> **Note on oMLX:** oMLX natively supports `/v1/embeddings` via its built-in `EmbeddingEngine`. When oMLX is the active engine, no separate embedding sidecar is required — both chat and embedding endpoints are served by the same oMLX process. The embedding sidecar architecture is only activated for engines that lack native embedding support (e.g., llama.cpp).

### 2.4 Data Directory Layout

All runtime state lives under `~/.lmforge/`:

```
~/.lmforge/
  engines/              ← downloaded engine binaries / managed environments
    omlx/               (pip-managed isolated venv or Homebrew-managed)
    llama-server
    sglang/             (pip-managed isolated venv)
  models/               ← downloaded model weights
    qwen2.5-coder-7b/
    nomic-embed-text/
  models.json           ← flat index: name, path, format, engine, size, sha256, schema_version
  engines.toml          ← user engine overrides / custom engine registrations
  config.toml           ← user-level config (port, default model, log level, resource limits)
  logs/                 ← structured log directory
    lmforge.log      ← rotating log file (JSON structured)
    engine-stdout.log   ← engine process stdout capture
    engine-stderr.log   ← engine process stderr capture
```

### 2.5 Project-Level Config

Any consuming project may place a `lmforge.yaml` at its root to override defaults:

```yaml
model:
  id: qwen2.5-coder-7b
  quantization: q4_k_m
  context_length: 32768

engine:
  prefer_radix_cache: true   # hint: this is an agentic workload
  min_vram_for_gpu: 6        # GB; below this, use CPU offload mode

server:
  port: 11430
  openai_compatible: true
  ollama_compatible: true

resources:
  max_gpu_memory_fraction: 0.75   # cap GPU memory usage (see §14)
```

---

## 3. Hardware Detection & Engine Routing

### 3.1 Hardware Probe Output

The `hardware_probe` module runs at startup and produces a `HardwareProfile` struct:

```rust
HardwareProfile {
    os:           Darwin | Linux | Windows,
    arch:         Aarch64 | X86_64,
    is_tegra:     bool,          // Jetson / edge device signature
    gpu_vendor:   Nvidia | Apple | Amd | None,
    vram_gb:      f32,           // 0.0 if no discrete GPU
    unified_mem:  bool,          // true on Apple Silicon
    total_ram_gb: f32,
}
```

### 3.2 VRAM Detection Heuristics

VRAM detection accuracy varies across platforms. The following heuristics are applied:

| Platform | Detection Method | Heuristic |
|---|---|---|
| **Apple Silicon** (unified memory) | `sysinfo` total system RAM | Report 75% of total unified memory as "available VRAM" (remaining 25% reserved for OS and non-ML workloads). Configurable via `resources.max_gpu_memory_fraction` in config. |
| **NVIDIA** (discrete GPU) | `nvml-wrapper` crate (`nvmlDeviceGetMemoryInfo`) | Report free VRAM; subtract 512 MB safety margin for driver/desktop overhead. Multi-GPU: use the GPU with the highest free VRAM for routing decisions. |
| **AMD** (discrete GPU) | Best-effort via `sysinfo` or `rocm-smi` CLI probe | If ROCm is installed and detectable, report VRAM. Otherwise, fall back to `gpu_vendor = None` (routes to llama.cpp CPU). |
| **No GPU / undetectable** | N/A | `vram_gb = 0.0`, routes to llama.cpp CPU fallback. |

### 3.3 Engine Routing Decision Tree

Routing is evaluated top-to-bottom; **first match wins**.

| Priority | Condition | Selected Engine | Rationale |
|---|---|---|---|
| 1 | `os = Darwin` AND `arch = Aarch64` | **oMLX** | Native Metal; continuous batching, tiered KV cache, built-in embedding support. Best TTFT on Apple Silicon |
| 2 | `gpu_vendor = Nvidia` AND `vram_gb > 24` | **SGLang** | RadixAttention; optimal agentic prefix caching |
| 3 | `gpu_vendor = Nvidia` AND `vram_gb` 6–24 GB | **llama.cpp (CUDA)** | Consumer GPU sweet spot with CUDA offload |
| 4 | `is_tegra = true` | **llama.cpp (TensorRT)** | Edge/Jetson; MLC LLM deferred to v0.4 |
| 5 | Fallback (any) | **llama.cpp (CPU/partial GPU)** | GGUF Q4_K_M; bulletproof, no OOM risk |

The routing table is encoded in `engines.toml`. A default version is embedded in the binary at compile time, but users can extend or override it by placing a custom `engines.toml` at `~/.lmforge/engines.toml`. This allows power users to register custom engines without rebuilding the binary. User entries are merged with built-in defaults; user entries with the same `id` override the built-in entry.

### 3.4 VRAM-Aware Quantization Selection

When `llama.cpp` is selected, the installer automatically picks the GGUF quantization tier:

| Available VRAM | Recommended Quant | Quality |
|---|---|---|
| ≥ 24 GB | `Q8_0` | Near-lossless |
| 12–24 GB | `Q6_K` | High quality |
| 6–12 GB | `Q4_K_M` | Sweet spot (default) |
| < 6 GB / CPU only | `Q4_0` | Smallest, fastest on CPU |

---

## 4. Engine Management

### 4.1 Engine Installer

The `engine_installer` provisions each engine using the method best suited to its distribution model. **There is no source compilation in LMForge.**

| Engine | Install Method | Pinned Version | Source | Notes |
|---|---|---|---|---|
| oMLX | Homebrew (`brew install omlx`) or pip in isolated venv | **v0.2.22** | `github.com/jundot/omlx` (7.1k ★) | Python-based (FastAPI). Requires macOS 15.0+ (Sequoia), Python 3.10+, Apple Silicon. Homebrew preferred for auto-updates and crash recovery via `brew services`. |
| llama.cpp | Download pre-built `llama-server` binary | **b8558** | `github.com/ggml-org/llama.cpp/releases/tag/b8558` | Single static binary; no runtime dependencies. Asset naming: `llama-b8558-bin-{platform}.tar.gz` |
| SGLang | `pip install sglang[all]==0.5.9` in isolated venv | **0.5.9** | PyPI | Heavy CUDA/PyTorch dependencies. Pre-flight check required (see §4.1.1). Venv at `~/.lmforge/engines/sglang/`. |

All binary downloads are verified with SHA256 checksums published in the release manifest. Partial downloads resume via HTTP Range requests.

#### 4.1.2 Version Pinning Policy

All engine dependencies are pinned to specific known-good versions in the embedded `engines.toml`. This ensures reproducible behaviour across installations. The pinned versions as of this release:

| Dependency | Pinned Version | Release Date | Update Cadence |
|---|---|---|---|
| oMLX | `v0.2.22` | 2026-03 | Updated with each LMForge minor release after testing |
| llama.cpp | `b8558` | 2026-03-27 | Updated with each LMForge minor release after testing |
| SGLang | `0.5.9` | 2026-01 | Updated with each LMForge minor release after testing |

**Policy:** Engine versions are updated only when a new LMForge release explicitly bumps them, after integration testing. Users may override the pinned version in `~/.lmforge/engines.toml` at their own risk.

#### 4.1.1 SGLang Pre-Flight Checks

Before attempting SGLang installation, the installer runs the following validation and reports clear errors if any check fails:

| Check | Validation | Error on Failure |
|---|---|---|
| CUDA toolkit | `nvcc --version` reports CUDA ≥ 11.8 | "SGLang requires CUDA ≥ 11.8. Found: {version}. Install from https://developer.nvidia.com/cuda-downloads" |
| Python version | Python ≥ 3.10 available in PATH | "SGLang requires Python ≥ 3.10. Found: {version}" |
| Available disk space | ≥ 15 GB free in `~/.lmforge/engines/` | "SGLang requires ~15 GB for PyTorch + dependencies. Available: {free_gb} GB" |
| NVIDIA driver | `nvidia-smi` returns successfully | "NVIDIA driver not found. Install from https://www.nvidia.com/drivers" |

### 4.2 oMLX Engine Details

oMLX ([github.com/jundot/omlx](https://github.com/jundot/omlx)) is a Python-based LLM inference server purpose-built for Apple Silicon. It provides:

- **Continuous batching** via `mlx-lm`'s `BatchGenerator`
- **Tiered KV cache** — hot tier in RAM, cold tier on SSD (safetensors format). Past context survives across requests and even server restarts.
- **Multi-model serving** — LLMs, VLMs, embedding models, and rerankers within a single process, with LRU eviction, model pinning, and per-model TTL
- **Built-in `/v1/embeddings`** — no separate sidecar required on Apple Silicon
- **OpenAI + Anthropic API compatibility** — `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`
- **Tool calling / function calling** with auto-detection for major model families
- **Admin dashboard** at `/admin` for model management, chat, benchmark, and per-model settings
- **MCP (Model Context Protocol)** support

**Install methods for LMForge:**

1. **Homebrew (preferred):** `brew tap jundot/omlx && brew install omlx` — enables `brew services start omlx` for crash recovery and auto-restart.
2. **Pip in isolated venv:** `pip install -e .` from cloned repo at `~/.lmforge/engines/omlx/`. Used as fallback if Homebrew is unavailable.

**Default port:** oMLX defaults to `:8000`. LMForge configures it to use `:11431` (internal) and proxies through `:11430`.

### 4.3 Engine Manager Behaviour

`engine_manager` spawns each engine as a child process and maintains its lifecycle:

- **Health-check loop:** polls `/health` or `/v1/models` every 5 seconds
- **Auto-restart:** on crash, restarts with exponential back-off (1s → 2s → 4s → max 30s); halts after 3 consecutive failures and reports `error` status
- **Per-component health tracking:** tracks health of each managed process independently (chat engine, embedding sidecar if active). The aggregate status is `ready` only when all required components are healthy.
- **Log forwarding:** streams engine stdout/stderr to dedicated log files under `~/.lmforge/logs/` (see §12)
- **Graceful shutdown:** forwards `SIGTERM` / `Ctrl-C` to child process
- **Status exposure:** engine state (`starting | ready | degraded | error`) available at `GET /lf/status`
  - `degraded`: primary chat engine is healthy but embedding sidecar has failed (only applicable when sidecar architecture is active)

### 4.4 Model Switching Behaviour

LMForge serves **one chat model and one embedding model** at a time. When a user requests a model switch (via CLI, API, or UI):

1. **Queued requests** are drained (max 30s timeout).
2. **Engine process** is stopped gracefully (`SIGTERM`, then `SIGKILL` after 10s).
3. **New model** is loaded by restarting the engine process with the new model path.
4. **API returns 503** (`Service Unavailable`) with `Retry-After: 5` header during the transition.
5. **`/lf/status`** reports `starting` during the switch.

> **Note on oMLX:** oMLX natively supports multi-model serving with LRU eviction and model pinning. When oMLX is the active engine, model switches leverage oMLX's native hot-swap capability instead of restarting the process. The `model` field in the API request determines which model oMLX loads/serves.

### 4.5 Engine Registry Schema (`engines.toml`)

```toml
# --- Built-in default (embedded at compile time) ---
# Users can override or extend in ~/.lmforge/engines.toml
# All versions pinned to tested releases. Override at your own risk.

[[engine]]
id              = "omlx"
version         = "0.2.22"          # pinned version
matches         = { os = "macos", arch = "aarch64" }
install_method  = "brew"            # "brew" | "pip" | "binary"
brew_tap        = "jundot/omlx"
brew_formula    = "omlx"
pip_fallback    = "omlx==0.2.22"    # pinned pip version
model_format    = "mlx"
hf_org          = "mlx-community"
start_cmd       = "omlx"
start_args      = ["serve", "--model-dir", "{model_dir}", "--port", "{port}"]
health_endpoint = "/v1/models"
supports_embeddings = true          # no sidecar needed

[[engine]]
id              = "sglang"
version         = "0.5.9"           # pinned version
matches         = { os = "linux", gpu_vendor = "nvidia", vram_gb = ">24" }
install_method  = "pip"
pip_package     = "sglang[all]==0.5.9"  # pinned
preflight       = ["nvcc", "nvidia-smi", "python3"]  # required binaries
min_disk_gb     = 15
model_format    = "safetensors"
hf_org          = "{original}"   # use model's own org, not a conversion community
supports_embeddings = false

[[engine]]
id              = "llamacpp"
version         = "b8558"           # pinned version
matches         = { fallback = true }
install_method  = "binary"
binary          = "llama-server"
release_url     = "https://github.com/ggml-org/llama.cpp/releases/download/b8558"
asset_pattern   = "llama-b8558-bin-{platform}.tar.gz"  # platform: macos-arm64, ubuntu-x64, etc.
model_format    = "gguf"
hf_org          = "bartowski"
start_args      = ["--port", "{port}", "--model", "{model_path}", "-ngl", "{gpu_layers}"]
health_endpoint = "/health"
supports_embeddings = false
```

---

## 5. API Surface

### 5.1 OpenAI-Compatible Layer (`/v1/...`)

All consuming applications (DocIntel, CodeForge) use this surface exclusively.

#### 5.1.1 `POST /v1/chat/completions`

Supports both non-streaming and SSE streaming modes.

**Non-streaming** (DocIntel summariser, query expansion):
```json
{
  "model": "qwen3-8b",
  "messages": [{ "role": "user", "content": "..." }],
  "temperature": 0.1,
  "max_tokens": 4096
}
```

**Streaming** (DocIntel user-facing RAG response):
```json
{
  "model": "qwen3-8b",
  "messages": [...],
  "stream": true,
  "temperature": 0.1,
  "max_tokens": 4096,
  "num_ctx": 16384
}
```

SSE stream must emit `data: {"choices":[{"delta":{"content":"..."}}]}` chunks and terminate with `data: [DONE]`.

**Field handling:**
- Required: `model`, `messages`
- Optional passthrough: `temperature`, `max_tokens`, `stream`, `num_ctx`, `num_predict`
- Unknown extra fields: forwarded to engine if supported; silently dropped otherwise
- `api_key`: any non-empty string accepted; no key also accepted (Haystack compatibility)

**Thinking mode** (for models that support extended reasoning):
- **Preferred:** emit `reasoning_content` as a separate delta field alongside `content`
- **Fallback:** pass through inline `<think>...</think>` tags inside `content` — DocIntel parses both formats

#### 5.1.2 `POST /v1/completions`

Non-chat text completion endpoint. Required for Ollama `/api/generate` compatibility mapping.

```json
// Request
{
  "model": "qwen3-8b",
  "prompt": "Complete this sentence: The quick brown",
  "max_tokens": 100,
  "temperature": 0.7,
  "stream": false
}

// Response
{
  "choices": [{ "text": " fox jumps over the lazy dog.", "index": 0, "finish_reason": "stop" }],
  "model": "qwen3-8b",
  "usage": { "prompt_tokens": 8, "completion_tokens": 9, "total_tokens": 17 }
}
```

#### 5.1.3 `POST /v1/embeddings`

Required for DocIntel (rag-service and ingestion-service) and any RAG consumer.

```json
// Request
{
  "model": "nomic-embed-text",
  "input": "string or array of strings"
}

// Response
{
  "data": [{ "embedding": [0.01, -0.23, ...], "index": 0 }],
  "model": "nomic-embed-text"
}
```

> **Routing:** When oMLX is the active engine, this endpoint is served directly by oMLX (which has built-in embedding support). For other engines (llama.cpp, SGLang), this endpoint is proxied to an embedding sidecar process (see §7.3).

#### 5.1.4 `GET /v1/models`

Returns locally installed models with capability metadata. Used by admin UIs for model pickers and thinking-mode toggles.

```json
{
  "data": [
    { "id": "qwen3-8b",         "capabilities": { "thinking": true,  "embeddings": false } },
    { "id": "nomic-embed-text", "capabilities": { "thinking": false, "embeddings": true  } }
  ]
}
```

### 5.2 Ollama-Compatible Layer (`/api/...`)

Mirrors the Ollama REST API for drop-in compatibility with tools like Continue.dev and Open WebUI.

| Ollama Endpoint | Maps To |
|---|---|
| `POST /api/chat` | `POST /v1/chat/completions` |
| `POST /api/generate` | `POST /v1/completions` |
| `GET /api/tags` | `GET /v1/models` |
| `POST /api/pull` | `lmforge pull <model>` (triggers actual model download) |

### 5.3 LMForge Native Layer (`/lf/...`)

Internal API for the Tauri UI and SDK health checks:

| Endpoint | Purpose |
|---|---|
| `GET /lf/status` | Per-component engine status (chat engine, embedding sidecar), active model, TTFT metrics |
| `GET /lf/hardware` | Full `HardwareProfile` as JSON |
| `POST /lf/model/pull` | Trigger model download; responds with SSE progress stream |
| `GET /lf/model/list` | `models.json` as JSON |
| `DELETE /lf/model/:name` | Remove model from index and disk |
| `POST /lf/model/switch` | Switch active model (see §4.4 for behaviour) |

### 5.4 Health & Readiness

Consuming apps should poll one of these endpoints during startup:

- `GET /health` → `200 OK` when engine is ready
- `GET /v1/models` → `200 OK` (same readiness semantics)

**Default port:** `11430`  
DocIntel defaults to `http://host.docker.internal:11430` — existing Ollama users only need a port change from `11434`.

---

## 6. Model Management

### 6.1 Model Resolution Strategy

When a user requests a model by logical name (e.g., `qwen2.5-coder-7b`), `model_manager` resolves it to the correct Hugging Face repository and file based on the active engine:

| Active Engine | Format | Primary HF Org | Example Repo |
|---|---|---|---|
| oMLX | MLX weights | `mlx-community` | `mlx-community/Qwen2.5-Coder-7B-Instruct-4bit` |
| llama.cpp | GGUF | `bartowski` | `bartowski/Qwen2.5-Coder-7B-Instruct-GGUF` |
| SGLang | safetensors | *(original model org)* | `Qwen/Qwen2.5-Coder-7B-Instruct` |

### 6.2 Download Sources

`lmforge pull` accepts four input modes, auto-detected by pattern:

```bash
# Mode 1 — logical model name (engine-format resolved automatically)
lmforge pull qwen2.5-coder-7b

# Mode 2 — explicit HF repo (tool picks correct file within the repo)
lmforge pull bartowski/Qwen2.5-Coder-7B-Instruct-GGUF

# Mode 3 — direct URL (any host, any format)
lmforge pull https://example.com/mymodel.gguf

# Mode 4 — local filesystem path
lmforge pull /Users/titas/models/mymodel.gguf
```

### 6.3 Model Index (`models.json`)

A flat JSON file at `~/.lmforge/models.json` tracks all installed models. No database dependency.

```json
{
  "schema_version": 1,
  "models": [
    {
      "id": "qwen2.5-coder-7b",
      "path": "~/.lmforge/models/qwen2.5-coder-7b/",
      "format": "mlx",
      "engine": "omlx",
      "size_gb": 4.2,
      "sha256": "a3f...",
      "capabilities": { "thinking": false, "embeddings": false },
      "added_at": "2025-03-01T10:00:00Z"
    },
    {
      "id": "nomic-embed-text",
      "format": "mlx",
      "engine": "omlx",
      "capabilities": { "thinking": false, "embeddings": true },
      "embed_dim": 768
    }
  ]
}
```

> **Migration note:** The `schema_version` field enables forward-compatible schema evolution. See §13 for migration mechanics.

### 6.4 Curated Model Catalog (`curated_models.toml`)

A default catalog is embedded in the binary at compile time. At startup, LMForge checks for updates from a GitHub-hosted `curated_models.toml` (fetched from the repo's `main` branch with a 24-hour cache TTL). If the fetch fails (offline, network error), the embedded default is used silently.

This hybrid approach ensures new models appear without requiring a LMForge binary update, while remaining fully functional offline.

```toml
[[model]]
name        = "qwen2.5-coder-7b"
description = "Best for coding · 128k context"
tags        = ["coding", "recommended"]

  [[model.engine_source]]
  engine           = "omlx"
  hf_repo          = "mlx-community/Qwen2.5-Coder-7B-Instruct-4bit"

  [[model.engine_source]]
  engine           = "llamacpp"
  hf_repo          = "bartowski/Qwen2.5-Coder-7B-Instruct-GGUF"
  hf_file_pattern  = "Q4_K_M"   # selects correct quant from multi-file repo

  [[model.engine_source]]
  engine           = "sglang"
  hf_repo          = "Qwen/Qwen2.5-Coder-7B-Instruct"
```

---

## 7. DocIntel Integration Requirements

> This section captures the specific API contracts LMForge must satisfy for DocIntel. Requirements are derived directly from `llm_engine.md` (DocIntel integration spec).

### 7.1 Environment Variable Contract

DocIntel configures its LLM connection via environment variables. LMForge must be reachable at the URL and serve the endpoints expected by each variable:

| DocIntel Env Var | Purpose | LMForge Default |
|---|---|---|
| `LLM_ENGINE_URL` | Base URL for all `/v1/*` calls | `http://localhost:11430` |
| `LLM_CHAT_MODEL` | Chat / generation model name | `qwen3-8b` |
| `LLM_EMBED_MODEL` | Embedding model name | `nomic-embed-text` |
| `LLM_CTX` | Context window for standard inference | `16384` |
| `LLM_THINKING_CTX` | Context window for thinking mode | `32768` |
| `LLM_EXPANSION_MODEL` | Query expansion / summariser model | `qwen3-8b` |

### 7.2 Haystack Component Compatibility

DocIntel replaces Ollama Haystack components with OpenAI-compatible equivalents. LMForge must:

- Accept any non-empty `api_key` value (or no key) — Haystack sets `"no-key"` by default
- Serve the required endpoints as listed below

| Removed Component | Replaced By | Required Endpoint |
|---|---|---|
| `OllamaChatGenerator` | `OpenAIChatGenerator(api_base_url=LLM_ENGINE_URL)` | `POST /v1/chat/completions` |
| `OllamaTextEmbedder` | `OpenAITextEmbedder(api_base_url=LLM_ENGINE_URL)` | `POST /v1/embeddings` |
| `OllamaDocumentEmbedder` | `OpenAIDocumentEmbedder(api_base_url=LLM_ENGINE_URL)` | `POST /v1/embeddings` |

### 7.3 Embedding Architecture

The embedding endpoint architecture varies by active engine:

**When oMLX is active (Apple Silicon):**

oMLX natively supports embedding models via its built-in `EmbeddingEngine`. Both chat and embedding requests are served by the same oMLX process. No sidecar is needed.

```
LMForge api_server (:11430)
  │
  ├── /v1/chat/completions  ──▶  oMLX server (:11431)
  │                                └── BatchedEngine (chat model)
  └── /v1/embeddings        ──▶  oMLX server (:11431)
                                   └── EmbeddingEngine (embed model)
```

**When llama.cpp or SGLang is active (non-Apple Silicon):**

These engines do not natively expose `/v1/embeddings`. LMForge proxies this endpoint to a supervised sidecar process:

```
LMForge api_server (:11430)
  │
  ├── /v1/chat/completions  ──▶  llama.cpp / SGLang (:11431)
  │
  └── /v1/embeddings        ──▶  embedding sidecar (:11432)
                                   └── infinity-emb (CUDA/CPU)
```

The embedding sidecar is spawned and supervised by `engine_manager` alongside the primary engine. From DocIntel's perspective, both endpoints are served at the same `LLM_ENGINE_URL` — routing is internal to LMForge.

**Embedding dimension constraint:** DocIntel's current embedding model (`nomic-embed-text`) produces **768-dimensional vectors**. Changing the embedding model requires recreating the Qdrant collection and re-indexing all documents. LMForge must validate that the model in `models.json` for the embed role matches the configured `embed_dim` before starting, and warn clearly on mismatch.

### 7.4 Thinking Mode

DocIntel has a per-tenant thinking mode toggle. LMForge must:

1. Expose `thinking: true/false` in `GET /v1/models` capabilities per model
2. When thinking is active, emit `reasoning_content` as a separate delta field in the SSE stream (**preferred format**)
3. Fallback: pass through inline `<think>...</think>` tags inside `content` — DocIntel parses this format already

---

## 8. Installation & Distribution

### 8.1 GitHub Release Artifacts

Each release publishes pre-built binaries via GitHub Actions CI:

| Artifact | Target Platform |
|---|---|
| `lmforge-aarch64-apple-darwin` | macOS Apple Silicon |
| `lmforge-x86_64-unknown-linux-gnu` | Linux x86_64 (NVIDIA workstation / server) |
| `lmforge-x86_64-pc-windows-msvc` | Windows x86_64 |

### 8.2 One-Line Install

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/you/lmforge/main/install.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/you/lmforge/main/install.ps1 | iex
```

The install script: detects OS/arch → downloads matching binary from GitHub Releases → places it in `/usr/local/bin` (or `%APPDATA%\lmforge` on Windows) → runs `lmforge init` to probe hardware and download the default engine binary.

### 8.3 Package Managers (post v0.1)

- Homebrew tap: `brew install yourname/tap/lmforge`
- Cargo: `cargo install lmforge` (builds from source, optional)

### 8.4 Project-Level Auto-Bootstrap

When a consuming project installs the Python or Dart SDK, the SDK checks at startup whether the `lmforge` binary is present. If absent, it downloads the correct platform binary from GitHub Releases and caches it under the project's `.lmforge/` directory. No global install required.

```python
from lmforge import InferenceEngine

engine = InferenceEngine(model='qwen2.5-coder-7b')
engine.start()   # downloads binary if needed, probes hardware, spawns engine
client = engine.openai_client()  # returns openai.OpenAI pointed at local server
```

---

## 9. SDK Specification

### 9.1 Python SDK

**Package:** `lmforge-py` · distributed via PyPI

```python
from lmforge import InferenceEngine, EmbeddingEngine

# Full lifecycle management
engine = InferenceEngine(
    model='qwen2.5-coder-7b',
    port=11430,
    auto_download=True
)
engine.start()

# Returns standard openai.OpenAI client — zero learning curve
client = engine.openai_client()
response = client.chat.completions.create(
    model='qwen2.5-coder-7b',
    messages=[{'role': 'user', 'content': 'Hello'}]
)

engine.stop()
```

The Python SDK ships the correct platform binary as a package resource. It manages the `lmforge` process as a subprocess — no Rust or system install required by the consuming project.

### 9.2 Dart / Flutter SDK

**Package:** `lmforge` · distributed via pub.dev

```dart
import 'package:lmforge/lmforge.dart';

final engine = LMForgeEngine(
  model: 'qwen2.5-coder-7b',
  port: 11430,
);
await engine.start();

final response = await engine.chat([
  ChatMessage(role: 'user', content: 'Hello'),
]);

await engine.stop();
```

---

## 10. Tauri UI (v0.2)

> The UI is deferred to v0.2. v0.1 ships CLI-only.

### 10.1 Technology

- **Shell:** Tauri v2 (Rust backend)
- **Frontend:** Svelte
- **Rendering:** OS native webview — no bundled Chromium
- **Memory footprint:** ~10 MB baseline (vs ~150 MB Electron)
- **Distribution:** compiled as a feature flag and embedded in the LMForge binary

### 10.2 System Tray

The primary UI surface is a system tray icon. The Tauri window is only instantiated when the user clicks the tray — not on startup.

```
[Tray icon]  →  right-click menu
  ├── Active Model: Qwen2.5-Coder 7B  ✓
  ├── Engine: oMLX  |  TTFT: 180ms
  ├── Open Model Browser
  ├── Stop Engine
  └── Quit
```

### 10.3 Model Browser (Svelte Window)

Three panels:

1. **Installed** — lists `models.json` entries; allows deletion and active model switch
2. **Discover** — queries HF Hub API live, filtered to active engine format; shows recommended models with size and VRAM fit indicator
3. **Add Custom** — accepts HF model ID, direct URL, or local path; shows live download progress via `/lf/model/pull` SSE stream

### 10.4 Engine Status Header

Always-visible bar: active engine · hardware summary · current model · last TTFT · health indicator (green / amber / red)

- **Green:** all components healthy (`ready`)
- **Amber:** partial degradation — e.g., chat engine is healthy but embedding sidecar failed (`degraded`)
- **Red:** engine has failed and exceeded restart attempts (`error`)

---

## 11. Non-Functional Requirements

### 11.1 Performance Targets

| Metric | Target |
|---|---|
| Orchestrator idle memory (no UI) | < 15 MB RSS |
| Orchestrator idle memory (with tray) | < 25 MB RSS |
| Time from `lmforge start` to `/health 200` | < 3 seconds (engine already downloaded) |
| Proxy latency overhead vs direct engine call | < 5 ms |
| `models.json` load time | < 50 ms for up to 100 models |

### 11.2 Reliability

- Engine crash → auto-restart within 2 seconds; exponential back-off after 3 consecutive failures
- Download interruption → resume from byte offset; SHA256 verified before model is marked available
- Port conflict on startup → clear error message with suggested alternative port

### 11.3 Security

- Binds to `127.0.0.1` only by default — not network-exposed
- Any non-empty `api_key` accepted; no key also accepted (OpenAI SDK / Haystack compatibility)
- **Network bind warning:** If `bind_address` is changed to `0.0.0.0` (or any non-loopback address) and no real API key is configured, LMForge must: (a) emit a prominent `WARN` log message on startup, and (b) print a CLI warning: `⚠ Server is network-exposed without authentication. Set api_key in config.toml for production use.`
- **API key enforcement (non-loopback):** When bound to a non-loopback address, an explicit `api_key` should be set in `config.toml`. If set, requests without a matching `Authorization: Bearer <key>` header receive `401 Unauthorized`. This does not affect loopback (`127.0.0.1`) binds.
- Engine binary checksums verified against GitHub-published SHA256 manifests before execution

### 11.4 Portability

- Single compiled binary — no runtime dependencies (no Python, no Node.js, no JVM)
- **Engine-specific exceptions:**
  - oMLX: requires Python 3.10+ and macOS 15.0+ (installed via Homebrew or isolated venv)
  - SGLang: installed in isolated venv at `~/.lmforge/engines/sglang/`
- Full uninstall: `rm -rf ~/.lmforge/` + binary removal — no system-level side effects

---

## 12. Logging & Observability

### 12.1 Log Architecture

LMForge uses structured JSON logging for machine-parseable output, with a human-readable fallback for CLI display.

```
~/.lmforge/logs/
  lmforge.log        ← main orchestrator log (JSON, rotating)
  engine-stdout.log     ← raw engine process stdout
  engine-stderr.log     ← raw engine process stderr
```

### 12.2 Log Format

Each line in `lmforge.log` is a JSON object:

```json
{
  "ts": "2026-03-27T15:14:43.123Z",
  "level": "INFO",
  "component": "engine_manager",
  "msg": "Engine started successfully",
  "engine": "omlx",
  "model": "qwen2.5-coder-7b",
  "pid": 12345,
  "startup_ms": 1823
}
```

### 12.3 Log Levels

| Level | Usage |
|---|---|
| `ERROR` | Unrecoverable failures: engine failed to start after retries, model file corrupted, port conflict |
| `WARN` | Recoverable issues: engine crash (before retry), network-exposed without auth, VRAM detection fallback |
| `INFO` | Normal lifecycle events: engine started, model loaded, model switched, download completed |
| `DEBUG` | Verbose tracing: HTTP request/response details, routing decisions, health-check results |
| `TRACE` | Internal diagnostics: SSE frame forwarding, byte-level download progress |

Default level: `INFO`. Configurable via `config.toml` (`log_level = "debug"`) or CLI flag (`--log-level debug`).

### 12.4 Log Rotation

- `lmforge.log`: rotates at 50 MB; keeps last 5 files (250 MB max)
- `engine-stdout.log` / `engine-stderr.log`: rotates at 20 MB; keeps last 3 files

### 12.5 CLI Log Commands

```bash
# Tail the main log (human-readable format)
lmforge logs --follow

# Tail with component filter
lmforge logs --follow --component engine_manager

# Show last N lines
lmforge logs --tail 100

# Show engine output specifically
lmforge logs --engine --follow

# JSON output (for piping to jq, etc.)
lmforge logs --json --follow
```

### 12.6 Metrics Exposure

`GET /lf/status` returns key operational metrics alongside engine status:

```json
{
  "engine": "omlx",
  "status": "ready",
  "model": "qwen2.5-coder-7b",
  "components": {
    "chat": { "status": "ready", "pid": 12345 },
    "embeddings": { "status": "ready", "source": "native" }
  },
  "metrics": {
    "uptime_seconds": 3600,
    "requests_total": 142,
    "last_ttft_ms": 180,
    "avg_ttft_ms": 210,
    "engine_restarts": 0
  }
}
```

---

## 13. Upgrade & Migration

### 13.1 Schema Versioning

All persistent data files include a `schema_version` field:

| File | Current Version | Version Field |
|---|---|---|
| `models.json` | `1` | `schema_version` (top-level) |
| `config.toml` | `1` | `schema_version` (top-level) |
| `engines.toml` (user override) | `1` | `schema_version` (top-level) |

### 13.2 Migration Mechanics

On startup, LMForge checks each data file's `schema_version`:

1. **Version matches current:** load normally.
2. **Version is older:** run migration functions in sequence (v1→v2, v2→v3, etc.). Back up the original file as `<filename>.bak.<old_version>` before migrating.
3. **Version is newer (downgrade):** refuse to start with a clear error: `"models.json schema_version 3 is newer than this binary supports (max: 2). Please upgrade LMForge."`
4. **Version field missing:** treat as v0 (pre-versioning); migrate from v0 to current.

### 13.3 Binary Upgrade Flow

```bash
# Self-update (checks GitHub Releases for newer version)
lmforge update

# Check for updates without applying
lmforge update --check

# Force specific version
lmforge update --version 0.2.1
```

On update:
1. Download new binary to a temp file.
2. Verify SHA256 checksum.
3. Replace current binary atomically (`rename`).
4. Print any migration notes (e.g., "config.toml schema updated from v1 → v2").

### 13.4 Engine Upgrade

Engine binaries are versioned independently. `engines.toml` may specify minimum engine versions. On startup, if the installed engine version is below the minimum:

```
INFO  Engine llama-server is v1.2.0, minimum required is v1.3.0. Upgrading...
```

The upgrade is automatic for binary engines (re-download from release URL). For pip-managed engines (oMLX, SGLang), `pip install --upgrade` is run in the isolated venv.

---

## 14. Resource Limits

### 14.1 GPU Memory Limits

Users can cap GPU memory usage to leave headroom for other GPU workloads:

```toml
# config.toml
[resources]
# Fraction of available VRAM/unified memory to use (0.0 – 1.0)
max_gpu_memory_fraction = 0.75   # default: 0.75 on Apple Silicon, 0.90 on NVIDIA

# Absolute VRAM cap (overrides fraction if set)
max_gpu_memory_gb = 24           # optional; useful for multi-GPU or shared workstations
```

These limits affect:
- **Engine routing:** VRAM-based routing decisions use the capped value, not raw hardware VRAM
- **Quantization selection:** Q-level selection for llama.cpp uses the capped VRAM
- **oMLX:** passed as `--max-process-memory` to oMLX's process memory enforcer
- **SGLang:** passed as `--mem-fraction-static` argument
- **llama.cpp:** adjusts `-ngl` (GPU layers) to stay within the cap

### 14.2 System Memory Limits

```toml
# config.toml
[resources]
# Maximum RAM for engine processes (excludes LMForge orchestrator itself)
max_system_memory_gb = 32        # optional; defaults to no limit
```

### 14.3 Disk Space Management

```toml
# config.toml
[resources]
# Warn when remaining disk space drops below this threshold
min_free_disk_gb = 10            # default: 10

# Maximum total model storage
max_model_storage_gb = 100       # optional; defaults to no limit
```

When `min_free_disk_gb` is breached during a model download, the download is paused and the user is warned:

```
WARN  Disk space low: 8.2 GB remaining (threshold: 10 GB). Download paused.
      Free space by running: lmforge model prune
```

### 14.4 Concurrent Request Limits

```toml
# config.toml
[resources]
# Maximum concurrent inference requests (queued beyond this)
max_concurrent_requests = 4      # default: 4
request_queue_size = 32          # default: 32; requests beyond this get 429
```

---

## 15. Testing Strategy

### 15.1 Unit Tests

| Component | Test Focus | Framework |
|---|---|---|
| `hardware_probe` | Mock `sysinfo` / `nvml` output; verify `HardwareProfile` construction for each platform variant | `#[cfg(test)]` + mockall |
| `engine_registry` | Verify routing decision tree: given a `HardwareProfile`, assert correct engine selection. Test user override merging. | `#[cfg(test)]` |
| `model_manager` | Model name resolution → correct HF repo/file for each engine. `models.json` serialization/deserialization. Schema migration functions. | `#[cfg(test)]` |
| `api_server` | Request/response translation: OpenAI ↔ engine format. SSE streaming correctness. Field forwarding/dropping. | `axum::test` + `tower::ServiceExt` |

### 15.2 Integration Tests

| Test | Scope | Method |
|---|---|---|
| **Engine lifecycle** | `engine_manager` spawns a mock engine binary (a simple HTTP server), verifies health-check, kill/restart, exponential back-off | Rust integration test with a real child process |
| **API proxy round-trip** | Full request through `api_server` → mock engine → response back. Validates streaming SSE, non-streaming JSON, error propagation | Rust integration test with `reqwest` client |
| **Model download (mocked)** | `model_manager` downloads a small fixture file from a local HTTP server; verifies SHA256, resume, `models.json` update | Rust integration test with `wiremock` |
| **Config loading** | Verify `config.toml` + `lmforge.yaml` + CLI flag precedence. Test schema migration from v0 to current. | Rust integration test |

### 15.3 End-to-End Tests

Run as part of CI on macOS (Apple Silicon) and Linux (NVIDIA if available):

1. `lmforge init` → verify hardware probe output
2. `lmforge pull qwen2.5-coder-0.5b` → verify model download (use a small model for CI speed)
3. `lmforge start` → verify `/health` returns 200 within 30 seconds
4. Send a chat completion request → verify response format
5. Send an embedding request → verify response format and dimension
6. `lmforge stop` → verify clean shutdown

### 15.4 CI Configuration

```yaml
# GitHub Actions matrix
strategy:
  matrix:
    include:
      - os: macos-latest       # Apple Silicon runner
        engine: omlx
      - os: ubuntu-latest      # CPU-only fallback
        engine: llamacpp
      # NVIDIA runner (if available via self-hosted)
      # - os: self-hosted-nvidia
      #   engine: sglang
```

### 15.5 Performance Regression Tests

Track key metrics across versions (run in CI on a consistent runner):

| Metric | Method | Alert Threshold |
|---|---|---|
| Proxy latency overhead | Benchmark `/v1/chat/completions` with 1-token response via mock engine | > 10 ms (2× target) |
| `models.json` load time | Load a fixture with 100 model entries | > 100 ms (2× target) |
| Binary startup time | Time from process start to `/health` ready (mock engine) | > 5 seconds |

---

## 16. Phased Delivery

| Phase | Scope | Exit Criteria |
|---|---|---|
| **v0.1 — Core** | `hardware_probe` · `engine_registry` · oMLX spawning via Homebrew/pip (Mac) · `axum` `/v1` proxy · `lmforge pull` CLI · `llama.cpp` fallback · GitHub CI release · structured logging · schema versioning | DocIntel connects to LMForge on Apple Silicon; chat and embeddings work end-to-end |
| **v0.1.1 — Linux** | SGLang path (NVIDIA > 24 GB) with pre-flight checks · llama.cpp CUDA path · embedding sidecar (infinity-emb) · Linux release artifact | CodeForge runs on NVIDIA Linux with RadixAttention enabled |
| **v0.2 — UI** | Tauri v2 shell · Svelte model browser · system tray · `/lf/*` native API · per-component health indicators | Developer can browse, download, and switch models without touching the CLI |
| **v0.3 — SDK** | `lmforge-py` on PyPI · `lmforge` on pub.dev · auto-bootstrap · resource limit configuration | DocIntel CI installs LMForge via Python SDK with zero manual steps |
| **v0.4 — Windows** | Windows release artifact · PowerShell installer · llama.cpp Windows CUDA · network-bind security hardening | Full parity on Windows developer machines |

---

## 17. Open Questions & Deferred Decisions

| # | Question | Decision Gate |
|---|---|---|
| 1 | **Embedding sidecar for non-oMLX engines:** `infinity-emb` (broader model support, already in DocIntel's docker-compose) vs another lightweight embedding server for CUDA? | v0.1.1 implementation spike |
| 2 | **Ollama `/api/pull` behaviour:** should `POST /api/pull` trigger `lmforge pull` (enabling Open WebUI model management through LMForge), or be a no-op? | v0.2 planning |
| 3 | **Jetson / Edge:** llama.cpp TensorRT flag requires TensorRT SDK on device. Auto-detectable and installable, or documented manual prerequisite? | v0.4 planning |
| 4 | **Windows + high-end NVIDIA:** SGLang's Windows support is limited. Fall back to llama.cpp CUDA, or offer WSL2 path? | v0.4 planning |
| 5 | ~~**Naming:**~~ **RESOLVED.** Name is **LMForge** (`lmforge` for binary, crate, CLI, paths, SDKs). Repo directory already matches. | ✅ Decided |
| 6 | **AMD / ROCm support:** Currently falls through to llama.cpp CPU. Add explicit ROCm path for AMD GPUs with `llama.cpp` ROCm build? Requires testing on AMD hardware. | v0.3+ |
| 7 | ~~**oMLX version pinning:**~~ **RESOLVED.** All engines pinned: oMLX `v0.2.22`, llama.cpp `b8558`, SGLang `0.5.9`. Updated with each LMForge release after integration testing. Users can override in `~/.lmforge/engines.toml`. | ✅ Decided |
