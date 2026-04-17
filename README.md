<div align="center">

# ⬡ LMForge

**Hardware-aware LLM inference orchestrator — from edge to server**

[![Release](https://img.shields.io/github/v/release/phoenixtb/lmforge?include_prereleases&style=flat-square&color=6ee7b7)](https://github.com/phoenixtb/lmforge/releases)
[![Build](https://img.shields.io/github/actions/workflow/status/phoenixtb/lmforge/release.yml?style=flat-square&label=build)](https://github.com/phoenixtb/lmforge/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey?style=flat-square)](#supported-platforms)

Run multiple LLMs simultaneously on your local hardware. LMForge is a persistent daemon that manages model loading, VRAM allocation, and engine selection automatically — then exposes a single OpenAI-compatible REST API to every app on your machine.

[**Install Core**](#install) · [**Install UI**](#install) · [**API Reference**](#rest-api) · [**Contributing**](#contributing)

</div>

---

## What is LMForge?

LMForge is a **local AI infrastructure layer**. It sits between your hardware and the tools that use AI (code editors, agents, APIs, CLIs), managing the messy reality of running LLMs locally:

- Which engine to use (Apple MLX, llama.cpp, SGLang)?
- How much VRAM do you have, and which models fit?
- What happens when you want two models at once?
- How do you serve chat *and* embeddings from the same process?

LMForge handles all of this transparently. Consumer apps see a simple OpenAI-compatible API; LMForge handles the rest.

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

This is the **Docker model**: the engine is a service, the UI is just a client. Closing the desktop app doesn't stop your models.

---

## Features

- **Multi-model orchestration** — run inference *and* embedding models simultaneously, independently managed
- **Hardware-aware engine selection** — automatically picks the best engine for your hardware:
  - 🍎 **Apple Silicon** → [MLX](https://github.com/ml-explore/mlx) (native Metal, maximum throughput)
  - 🖥️ **NVIDIA GPU (Linux/Windows)** → [SGLang](https://github.com/sgl-project/sglang) (CUDA, high-concurrency)
  - 💻 **CPU / any hardware** → [llama.cpp](https://github.com/ggerganov/llama.cpp) (universal fallback)
- **VRAM-aware LRU eviction** — loads models up to the detected VRAM budget; evicts least-recently-used when full
- **OpenAI-compatible API** — drop-in replacement: `/v1/chat/completions`, `/v1/embeddings`, `/v1/models`, `/v1/rerank`
- **Ollama-compatible API** — `/api/chat`, `/api/generate`, `/api/tags` for tools that expect Ollama
- **Model catalog** — curated shortcut names that resolve to the right HuggingFace repo + format for your hardware
- **System service** — registered as a LaunchAgent (macOS) or systemd unit (Linux), starts at login automatically
- **Real-time telemetry** — live GPU/CPU/memory metrics, per-model RSS, request latency (TTFT), request counts
- **Desktop UI** — optional Tauri app with a model browser, hardware panel, and live metrics dashboard
- **Idempotent CLI** — `lmforge start` is safe to call from any script; no-ops if already running

---

## Supported Platforms

| Platform | Architecture | Engine | Status |
|---|---|---|---|
| macOS 13+ | Apple Silicon (arm64) | MLX | ✅ Primary |
| macOS 13+ | Intel (x86_64) | llama.cpp | ✅ Supported |
| Ubuntu 22.04+ | x86_64 | SGLang / llama.cpp | ✅ Supported |
| Ubuntu 22.04+ | arm64 | llama.cpp | ✅ Supported |
| Windows 11 | x86_64 | llama.cpp | 🔜 Beta |

---

## Install

### Core (daemon + CLI)

The daemon runs as a system service. Install it once; it starts automatically on every login.

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

**Windows:** Download `lmforge-windows-x86_64.exe` from [GitHub Releases](https://github.com/phoenixtb/lmforge/releases/latest).

What the install script does:
1. Downloads the pre-built binary for your platform/arch
2. Installs to `/usr/local/bin/lmforge`
3. Runs `lmforge init` — detects hardware, selects and installs the right engine
4. Registers a system service (`launchd` on macOS, `systemd --user` on Linux)
5. Starts the daemon immediately

To pin a specific version:
```bash
LMFORGE_VERSION=v0.3.0 curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

### Desktop UI (optional)

After installing the core:
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
```

Downloads the `.dmg` (macOS) or `.AppImage` (Linux), installs it, and opens the app. The UI is a pure client — closing it never affects the daemon or running models.

### Uninstall

```bash
# Remove the desktop UI only (daemon keeps running)
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.sh | bash

# Remove Core (stops daemon; keeps your downloaded models)
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash

# Remove everything including models
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash -s -- --purge
```

---

## Quick Start

```bash
# 1. Install
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash

# 2. Pull a model (shortcut name — resolves to the right format for your hardware)
lmforge pull qwen3-4b

# 3. Chat via CLI
lmforge run qwen3-4b

# 4. Or use the API directly
curl http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3-4b",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
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
| `lmforge stop` | Stop the daemon |
| `lmforge status` | Show engine status and loaded models |
| `lmforge pull <model>` | Download a model (shortcut, HF repo, or URL) |
| `lmforge run <model>` | Interactive REPL with a model |
| `lmforge catalog` | List available model shortcuts |
| `lmforge catalog --search <keyword>` | Search the catalog |
| `lmforge models list` | List installed models |
| `lmforge models remove <name>` | Remove a model from disk |
| `lmforge models unload` | Unload active model from VRAM (keeps files) |
| `lmforge logs` | View daemon logs |
| `lmforge logs -f` | Tail logs continuously |
| `lmforge clean` | Audit disk usage (orphans, logs, HF cache) |
| `lmforge clean --all --yes` | Clean everything without prompts |
| `lmforge service install` | Register as a system service (auto-start on login) |
| `lmforge service uninstall` | Remove the system service |
| `lmforge service start/stop/status` | Control the system service |

---

## REST API

LMForge exposes two API surfaces:

### OpenAI-compatible (`/v1/*`)

Drop-in replacement for any tool that uses the OpenAI SDK:

```bash
# Configure any OpenAI client to point here
OPENAI_API_BASE=http://127.0.0.1:11430/v1
OPENAI_API_KEY=none   # no key needed
```

| Endpoint | Method | Description |
|---|---|---|
| `/v1/models` | GET | List available models |
| `/v1/chat/completions` | POST | Chat (streaming + non-streaming) |
| `/v1/completions` | POST | Text completion |
| `/v1/embeddings` | POST | Generate embeddings |
| `/v1/rerank` | POST | Rerank documents |

### Ollama-compatible (`/api/*`)

For tools that target Ollama:

| Endpoint | Method | Description |
|---|---|---|
| `/api/chat` | POST | Chat generation |
| `/api/generate` | POST | Text generation |
| `/api/tags` | GET | List models |

### LMForge-native (`/lf/*`)

| Endpoint | Method | Description |
|---|---|---|
| `/health` | GET | Health check (`{"status":"ok","version":"0.3.0","min_ui_version":"0.3.0"}`) |
| `/lf/status` | GET | Engine state snapshot |
| `/lf/status/stream` | GET | SSE stream of engine state changes |
| `/lf/hardware` | GET | Detected hardware (GPU, CPU, memory, engine) |
| `/lf/sysinfo` | GET | Live system metrics (GPU %, VRAM, CPU, RAM) |
| `/lf/model/list` | GET | Installed models with sizes + capabilities |
| `/lf/model/pull` | POST | Download a model (SSE progress stream) |
| `/lf/model/switch` | POST | Load a model into VRAM |
| `/lf/model/unload` | POST | Unload a model from VRAM |
| `/lf/model/delete/{name}` | DELETE | Remove a model from disk |
| `/lf/config` | GET | Current configuration |
| `/lf/shutdown` | POST | Graceful shutdown |

---

## Model Catalog

LMForge ships with a curated catalog of shortcut names that resolve to the right HuggingFace repo and format for your hardware:

```bash
lmforge catalog              # list all shortcuts
lmforge catalog --search qwen  # search by name
```

Example shortcuts:

| Shortcut | Resolves to (macOS/MLX) | Resolves to (Linux/GGUF) |
|---|---|---|
| `qwen3-4b` | `mlx-community/Qwen3-4B-...` | `Qwen/Qwen3-4B-GGUF` |
| `qwen3-8b` | `mlx-community/Qwen3-8B-...` | `Qwen/Qwen3-8B-GGUF` |
| `qwen3-embedding` | `mlx-community/Qwen3-Embedding-...` | `Qwen/Qwen3-Embedding-GGUF` |
| `llama3.1-8b` | `mlx-community/Meta-Llama-3.1-8B-...` | `bartowski/Meta-Llama-3.1-8B-GGUF` |

You can also pull any HuggingFace repo directly:

```bash
lmforge pull mlx-community/Qwen3-4B-4bit
lmforge pull Qwen/Qwen3-4B-GGUF
```

---

## Configuration

Config file: `~/.lmforge/config.toml`

```toml
[server]
port = 11430
bind = "127.0.0.1"

[engine]
# Overrides auto-detected engine (mlx | llamacpp | sglang)
# engine_override = "mlx"

[models]
# VRAM budget in GB — models exceeding this are not loaded
# vram_budget_gb = 0   # 0 = auto-detect
```

Override values with flags:
```bash
lmforge start --port 8080 --bind 0.0.0.0
```

---

## Building from Source

Requirements: **Rust 1.78+**, **Node.js 20+** (for UI only)

```bash
git clone https://github.com/phoenixtb/lmforge
cd lmforge

# Build the CLI/daemon
cargo build --release
./target/release/lmforge init

# Build the desktop UI
cd ui
npm install
npm run tauri build
```

Run tests:
```bash
cargo test                    # unit tests
cargo test --test integration # integration tests (requires daemon running)
bash tests/multi_model_e2e.sh # E2E multi-model test suite
```

---

## Data & Privacy

LMForge runs **entirely on your machine**. There is no telemetry, no analytics, no cloud sync. Models are downloaded directly from HuggingFace to `~/.lmforge/models/` and inference runs locally. The API binds to `127.0.0.1` by default and is not exposed to the network.

---

## Contributing

Contributions are welcome. Please:

1. **Open an issue first** for significant changes — discuss the approach before coding
2. Fork the repo and create a branch from `main`
3. Write tests for new functionality (unit tests in `src/`, integration tests in `tests/`)
4. Ensure `cargo check` and `cargo clippy` pass
5. Submit a PR with a clear description of what changed and why

### Project Structure

```
lmforge/
├── src/
│   ├── cli/           # CLI subcommands (start, pull, service, …)
│   ├── engine/        # Engine adapters (MLX, llama.cpp, SGLang)
│   │   └── adapters/
│   ├── model/         # Model index, resolver, catalog
│   └── server/        # Axum HTTP server, route handlers
├── ui/
│   ├── src/           # Svelte frontend
│   └── src-tauri/     # Tauri shell (pure HTTP client — no daemon code)
├── tests/             # Integration test suite (Rust)
├── scripts/           # Install / uninstall scripts
└── .github/workflows/ # CI/CD pipeline
```

### Opening Issues

- **Bug reports**: include `lmforge status`, `lmforge logs --tail 50`, and your OS/hardware
- **Feature requests**: describe the use case, not just the solution
- **Model compatibility**: include the model repo URL and the error from `lmforge logs --engine`

---

## License

MIT — see [LICENSE](LICENSE).

---

<div align="center">

Made for developers who want local AI to work like infrastructure — always on, always fast, never in the way.

**[⬡ phoenixtb/lmforge](https://github.com/phoenixtb/lmforge)**

</div>
