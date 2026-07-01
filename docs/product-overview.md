# LMForge — Run AI Models on Your Own Machine

**LMForge turns any laptop, workstation, or server into a private AI server.** Download a model, and within minutes you have a fast, OpenAI-compatible AI endpoint running entirely on your own hardware — no cloud, no per-token bills, no data leaving your device.

> One install. One click to download a model. A local API that your existing tools already know how to talk to.

---

## Why teams choose LMForge

| | Cloud AI APIs | **LMForge (local)** |
|---|---|---|
| **Data privacy** | Prompts leave your network | Everything stays on your machine |
| **Cost** | Pay per token, forever | Pay once for hardware; runs free |
| **Offline** | Needs internet | Works fully offline |
| **Vendor lock-in** | Tied to one provider | Open models, swap any time |
| **Compliance** | Third-party data processing | Full control, on-premises |
| **Setup** | API keys, billing | One command, done |

**Who it's for:**
- **Engineering managers** who want to give their teams AI without sending source code or customer data to a third party.
- **Businesses** with privacy, compliance, or cost concerns about cloud AI.
- **Developers & analysts** who want a local, drop-in replacement for OpenAI/Ollama endpoints.
- **Anyone with a capable laptop or GPU** who wants to run modern open models (chat, vision, embeddings, code) locally.

---

## Supported platforms

LMForge is fully cross-platform and picks the best inference engine for your hardware automatically.

| Platform | Hardware | What runs under the hood |
|---|---|---|
| **Windows 10/11** | NVIDIA GPU | llama.cpp with CUDA (auto-selects the right CUDA build for your driver) |
| **Windows 10/11** | AMD / Intel GPU, or CPU-only | llama.cpp with Vulkan (one build covers all GPUs) |
| **macOS** | Apple Silicon (M1–M4) | MLX — Apple's native, unified-memory engine |
| **Linux** | NVIDIA / AMD / Intel GPU, or CPU | llama.cpp (Vulkan for broad GPU support; CUDA for peak NVIDIA performance) |

Advanced engines (vLLM, TabbyAPI/ExLlamaV3) are available as opt-in installs for high-throughput NVIDIA setups on Linux/WSL2. You never have to think about any of this — LMForge detects your GPU, VRAM, and drivers and chooses for you.

---

## Install in one command

LMForge has two parts: **Core** (the background engine + command line) and the optional **desktop app** (a friendly UI). Install Core first.

**Windows (PowerShell):**
```powershell
irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.ps1 | iex
```

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.sh | bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
```

The installer sets everything up, starts the engine, and registers it to **start automatically when you log in** (HKCU autostart on Windows, `launchd` on macOS, `systemd` on Linux). The AI server lives at `http://localhost:11430`.

**Test it:** open the LMForge app, or run `lmforge status` in a terminal. You should see the engine reported as **ready**.

---

## Features

Each feature below includes a one-line "what it is", why it matters, and a quick way to try it.

### 1. Desktop app + command line

**What:** A clean desktop application (Overview, Model Library, Observability, Settings) plus a full `lmforge` command-line tool.

**Why it matters:** Non-technical users get a point-and-click experience; power users and scripts get the CLI. Both talk to the same local engine.

**Try it:**
- Open the app — the **Overview** shows engine health and the currently loaded model.
- Or run `lmforge status`, `lmforge list`, `lmforge pull <model>`.

---

### 2. One-click model download (curated catalog)

**What:** A built-in **Model Library** of recommended, ready-to-run models (chat, vision, embeddings, rerank, code) with simple shortcuts like `qwen3:8b:4bit`. You can also paste any Hugging Face repo.

**Why it matters:** No hunting for the right file format or quantization. LMForge resolves the correct files for your engine and downloads them.

**Try it:**
- In the app: **Models → Library**, pick a model, click **Pull**.
- Or CLI: `lmforge pull qwen3-8b`.

---

### 3. Live download progress — everywhere

**What:** A single, always-visible download indicator. It appears as a thin progress line under the page header whenever *anything* is downloading — a manual pull or a background migration — and follows you across every screen.

**Why it matters:** You always know what's happening and how far along it is, without hunting for a status panel. It's unobtrusive by design (a slim line, not a pop-up).

**Try it:** start any model download and switch between screens — the progress line stays visible and updates in real time. Hover the small label to see the exact file and byte count.

---

### 4. Drop-in OpenAI & Ollama compatible API

**What:** LMForge exposes the same API shapes your tools already use — OpenAI (`/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank`, `/v1/models`) and Ollama (`/api/chat`, `/api/generate`, `/api/tags`).

**Why it matters:** Point your existing app, IDE plugin, or script at `http://localhost:11430` instead of the cloud — usually a one-line change. No rewrites.

**Try it:**
```bash
curl http://localhost:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen3-8b","messages":[{"role":"user","content":"Hello!"}]}'
```

---

### 5. Multi-model orchestration

**What:** LMForge can manage several models, load them on demand, keep recently used ones warm (`keep_alive`), and auto-load your favorites at startup.

**Why it matters:** Switch between a chat model, an embedding model, and a code model seamlessly — the engine handles loading/unloading and memory for you.

**Try it:** pull two models, then send requests naming each one — LMForge loads the right model automatically and reports them under `lmforge status`.

---

### 6. Vision, embeddings, reranking, and code

**What:** Beyond chat, LMForge supports vision models (image understanding), text embeddings (for search/RAG), rerankers (for better retrieval), and code models.

**Why it matters:** One local server covers the building blocks for real applications — search, document Q&A, assistants — not just chatbots.

**Try it:** filter the Model Library by role (**chat / embed / rerank / vision / code**) and pull one of each to see capabilities tagged on every model card.

---

### 7. Reasoning ("thinking") models

**What:** First-class support for reasoning models (Qwen3, DeepSeek-R1, Phi-4-reasoning). Turn thinking on or off per request, cap how long the model reasons with a token budget, and stream the reasoning live — kept separate from the final answer.

**Why it matters:** You get visible chain-of-thought for hard problems without it bleeding into the answer, plus a budget knob to trade reasoning depth for speed — the same controls the big cloud APIs expose, running entirely on your machine.

**Try it:** add `"think": true` (optionally `"thinking_budget": 4096`) to a `/v1/chat/completions` request, or use the bundled Playground's thinking profile.

> **Small note — blank replies on reasoning models.** Reasoning tokens share the answer's token budget. Some models always reason and need room to finish; with too small a `max_tokens` they can spend it all thinking and return an empty answer. LMForge auto-raises the budget floor for these models to prevent that — the same headroom approach OpenAI and Anthropic use. A deliberately tiny limit can still truncate, so prefer a larger `max_tokens` (or a non-reasoning model) when you want short replies.

---

### 8. Hardware-aware, zero-config engine selection

**What:** On first run, LMForge profiles your CPU, GPU, VRAM, and drivers, then selects and installs the optimal engine and build (CUDA vs Vulkan vs MLX) — including choosing the correct CUDA runtime for your NVIDIA driver.

**Why it matters:** You get the fastest setup your machine can support without knowing anything about CUDA versions, compute capability, or quantization formats.

**Try it:** the app's hardware panel shows what was detected; `lmforge status` shows the active engine and version.

---

### 9. Flexible storage management

**What:** Choose where models are stored. Move your model library to another drive at any time (e.g., from `C:` to a roomy `D:` drive), with three clear options for existing models:
- **Adopt** — point at an existing folder of models.
- **Delete** — clear the old location.
- **Delete & re-download** — clean up and automatically re-fetch every model into the new location.

**Why it matters:** Model files are large. Teams need them on the right disk, and moving them shouldn't be a manual file-copy chore.

**How "Delete & re-download" works (and why it's smooth):** the re-download runs **in the background after the app restarts** — the app comes back instantly and stays fully usable while a progress banner tracks each model. It survives restarts (resumes where it left off), continues past any single failure, and offers a one-click **Retry** for anything that didn't complete.

**Try it:** **Settings → Storage**, change the models directory, choose **Delete & re-download**, and restart. Watch the global progress indicator re-download your library in the background.

---

### 10. Reliable background service + instant restart

**What:** LMForge runs as a lightweight background service that starts at login and stays out of your way (no console windows, no clutter). Restarting the engine from the app is fast and reliable on every platform.

**Why it matters:** It "just works" after a reboot, and configuration changes apply cleanly without leaving you staring at a "stopped" screen.

**Try it:** **Settings → restart the daemon**, or reboot your machine and confirm the engine is ready again automatically.

---

### 11. Built-in observability

**What:** An **Observability** dashboard with request metrics (throughput, latency, error rate), live log streaming, and a Prometheus-compatible `/metrics` endpoint.

**Why it matters:** Teams can monitor usage and troubleshoot without extra tooling, and plug LMForge into existing monitoring stacks.

**Try it:** open **Observability** in the app while sending a few requests, or scrape `http://localhost:11430/metrics`.

---

### 12. Secure by default

**What:** LMForge binds to `localhost` only by default. To share it on a network, you explicitly enable access with an API key and/or trusted network ranges (CIDR allow-lists).

**Why it matters:** Your AI server isn't exposed to the internet by accident. Opening it up is a deliberate, controlled choice.

**Try it:** by default, only your machine can reach the API. Network sharing is configured in `config.toml` (`api_key`, `trusted_networks`).

---

## A 5-minute evaluation

1. **Install** Core + UI (commands above). The engine auto-starts.
2. **Pull a model** from the Library (e.g., a small 4-bit chat model) and watch the live progress line.
3. **Chat** in the app, or hit the OpenAI-compatible endpoint with `curl`.
4. **Point an existing tool** (IDE assistant, script, internal app) at `http://localhost:11430` — confirm it works with a one-line URL change.
5. **Move your models** to another drive via Settings → Storage → Delete & re-download, and watch the background re-download.
6. **Open Observability** to see live metrics and logs.

---

## Uninstall

LMForge is clean to remove, and removing the UI never touches your models or the daemon.

**Windows (PowerShell):**
```powershell
# UI only (engine keeps running)
irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.ps1 | iex
# Core (stops engine, keeps your models)
irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.ps1 | iex
# Everything, including downloaded models
$env:LMFORGE_PURGE = "1"; irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.ps1 | iex
```

**macOS / Linux:**
```bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.sh | bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash
curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.sh | bash -s -- --purge
```

---

## In one sentence

**LMForge is the easiest way to run modern AI models privately on your own hardware — with a one-command install, a friendly desktop app, and a drop-in OpenAI-compatible API that works the same on Windows, macOS, and Linux.**
