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

[**Install**](#install) · [**Quick Start**](#quick-start) · [**CLI Reference**](#cli-reference) · [**REST API**](#rest-api) · [**Contributing**](#contributing)

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

- **Multi-model orchestration** — run inference *and* embedding models simultaneously, each independently managed with its own keep-alive lifecycle
- **Hardware-aware engine selection** — automatically picks the best engine:
  - 🍎 **Apple Silicon** → [oMLX](https://github.com/jundot/omlx) — OpenAI-compatible server, runs natively on Metal via MLX
  - 🖥️ **NVIDIA GPU (Linux/Windows)** → [SGLang](https://github.com/sgl-project/sglang) — CUDA, high-concurrency
  - 💻 **CPU / any hardware** → [llama.cpp](https://github.com/ggerganov/llama.cpp) — universal cross-platform fallback
- **VRAM-aware LRU eviction** — loads models up to detected VRAM budget; evicts least-recently-used when full
- **OpenAI-compatible API** — `/v1/chat/completions`, `/v1/embeddings`, `/v1/models`, `/v1/rerank`
- **Ollama-compatible API** — `/api/chat`, `/api/generate`, `/api/tags` for tools that expect Ollama
- **Model catalog** — curated shortcut names resolving to the right HuggingFace repo and format for your hardware
- **System service on all platforms** — launchd (macOS), systemd user unit (Linux), Windows Scheduled Task — all via `lmforge service install`, no admin required
- **Real-time telemetry** — live GPU/CPU/memory metrics, per-model RSS, request latency, request counts
- **Desktop UI** — optional Tauri app with model browser, hardware panel, and live metrics dashboard
- **Idempotent CLI** — `lmforge start` is safe to call from any script; no-ops if already running

---

## Supported Platforms

| Platform | Architecture | Engine | Core | Desktop UI |
|---|---|---|---|---|
| macOS 13+ | Apple Silicon (arm64) | oMLX (Metal/MLX) | ✅ | ✅ DMG |
| Ubuntu 22.04+ | x86_64 | SGLang / llama.cpp | ✅ | ✅ AppImage |
| Ubuntu 22.04+ | arm64 | llama.cpp | ✅ | 🔜 Planned |
| Windows 11 | x86_64 | llama.cpp / SGLang | ✅ | ✅ NSIS installer |

> **macOS Intel (x86_64)** binaries are available but not currently published via the CI release pipeline. Build from source with `cargo build --release --target x86_64-apple-darwin`.

---

## Install

### Core (daemon + CLI)

The daemon runs as a system service. Install once; it starts automatically on every login.

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

**Windows** — download `lmforge-windows-x86_64.exe` from [Releases](https://github.com/phoenixtb/lmforge/releases/latest), place it on your `PATH`, then:
```powershell
lmforge init                   # detect hardware, install engine
lmforge service install        # register Scheduled Task (auto-starts at logon)
```

What the install script does on macOS/Linux:
1. Downloads the pre-built binary for your platform/arch
2. Installs to `/usr/local/bin/lmforge`
3. Runs `lmforge init` — detects hardware, selects and installs the right engine
4. Registers a system service (`launchd` on macOS, `systemd --user` on Linux)
5. Starts the daemon immediately

To pin a specific version:
```bash
LMFORGE_VERSION=v0.1.0 curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
```

### Desktop UI (optional)

Install the core first, then:

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
```

**Windows:** Download `LMForge-UI-windows-x86_64.exe` from [Releases](https://github.com/phoenixtb/lmforge/releases/latest) and run the installer. WebView2 (pre-installed on Windows 11) is the only requirement.

The UI is a pure client — closing it never affects the daemon or running models.

### Uninstall

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
lmforge service uninstall      # remove Scheduled Task
# then delete lmforge.exe from PATH
```

---

## Quick Start

```bash
# 1. Install core
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash

# 2. Pull a model (shortcut resolves to the right format for your hardware)
lmforge pull qwen3-4b

# 3. Interactive chat via CLI
lmforge run qwen3-4b

# 4. Or hit the API directly
curl http://127.0.0.1:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen3-4b",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'

# 5. Embeddings
curl http://127.0.0.1:11430/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model": "qwen3-embedding", "input": "Hello world"}'
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
| `lmforge pull <model>` | Download a model (shortcut name, HF repo, or URL) |
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
| Windows | Scheduled Task (At Logon) | Task Scheduler → `LMForge Daemon` |

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
| `/lf/status` | GET | Engine state snapshot |
| `/lf/status/stream` | GET | SSE stream of state changes |
| `/lf/hardware` | GET | Detected hardware (GPU, CPU, VRAM, engine) |
| `/lf/sysinfo` | GET | Live system metrics (GPU%, VRAM, CPU, RAM) |
| `/lf/model/list` | GET | Installed models with sizes and capabilities |
| `/lf/model/pull` | POST | Download a model (SSE progress stream) |
| `/lf/model/switch` | POST | Load a model into VRAM |
| `/lf/model/unload` | POST | Unload a model from VRAM |
| `/lf/model/delete/{name}` | DELETE | Remove a model from disk |
| `/lf/config` | GET | Current runtime configuration |
| `/lf/shutdown` | POST | Graceful daemon shutdown |

---

## Model Catalog

LMForge includes a curated catalog of shortcut names that resolve to the right HuggingFace repo and quantisation format for your hardware automatically:

```bash
lmforge catalog                    # list all shortcuts
lmforge catalog --search qwen      # search by name/family
```

| Shortcut | Resolves to (macOS/MLX) | Resolves to (Linux/Windows GGUF) |
|---|---|---|
| `qwen3-4b` | `mlx-community/Qwen3-4B-4bit` | `Qwen/Qwen3-4B-GGUF` |
| `qwen3-8b` | `mlx-community/Qwen3-8B-4bit` | `Qwen/Qwen3-8B-GGUF` |
| `qwen3-14b` | `mlx-community/Qwen3-14B-4bit` | `Qwen/Qwen3-14B-GGUF` |
| `qwen3-embedding` | `mlx-community/Qwen3-Embedding-0.6B-4bit` | `Qwen/Qwen3-Embedding-GGUF` |
| `llama3.1-8b` | `mlx-community/Meta-Llama-3.1-8B-Instruct-4bit` | `bartowski/Meta-Llama-3.1-8B-Instruct-GGUF` |

You can also pull any HuggingFace repo directly:

```bash
lmforge pull mlx-community/Qwen3-4B-4bit
lmforge pull Qwen/Qwen3-4B-GGUF
```

---

## Configuration

Global config file: `~/.lmforge/config.toml`

```toml
schema_version = 2

port          = 11430
bind_address  = "127.0.0.1"
log_level     = "info"

[resources]
max_gpu_memory_fraction  = 0.75   # fraction of VRAM available to LMForge
max_concurrent_requests  = 4
request_queue_size       = 32
min_free_disk_gb         = 10

[orchestrator]
keep_alive        = "5m"          # unload idle models after this duration
max_loaded_models = 0             # 0 = unlimited (bounded by VRAM)
embed_batch_size  = 32            # max inputs per engine call for /v1/embeddings
```

Per-directory overrides via `lmforge.yaml`:

```yaml
default_chat_model: qwen3-8b
default_embed_model: qwen3-embedding
```

CLI flags override config:

```bash
lmforge start --port 8080 --log-level debug
```

---

## Building from Source

**Requirements:** Rust 1.78+, Node.js 20+ (UI only)

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
cargo test                              # unit + mock integration tests
bash tests/multi_model_e2e.sh          # E2E multi-model test suite
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
├── ui/
│   ├── src/           # SvelteKit frontend
│   └── src-tauri/     # Tauri shell (HTTP client — no daemon code)
├── tests/             # Integration test suite (Rust + shell)
├── scripts/           # Install / uninstall scripts
└── .github/workflows/ # CI (ci.yml) and release (release.yml) pipelines
```

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
