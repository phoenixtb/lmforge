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
| **Windows 10/11** | NVIDIA GPU | llama.cpp with CUDA (right build for your driver) |
| **Windows 10/11** | AMD / Intel GPU, or CPU-only | llama.cpp with Vulkan / CPU |
| **macOS** | Apple Silicon (M1–M4) | oMLX — OpenAI-compatible server on Apple Metal/MLX |
| **Linux** | NVIDIA (strong GPU) | SGLang (high concurrency) or llama.cpp CUDA (cuda12/cuda13 variants) |
| **Linux** | AMD / Intel GPU, or CPU | llama.cpp (Vulkan or CPU) |

Advanced engines (vLLM, TabbyAPI/ExLlamaV3, and similar) are available as **opt-in / experimental** installs for power users. You never have to think about this on day one — LMForge detects your GPU, VRAM, and drivers and chooses a **default** engine for you. Run `lmforge doctor` anytime to see what is installed and active.

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

The installer sets everything up, starts the engine, and registers it to **start automatically when you log in** (user autostart on Windows, `launchd` on macOS, `systemd --user` on Linux). The AI server lives at `http://localhost:11430`.

**Test it:** open the LMForge app, or run `lmforge status` in a terminal. You should see the engine reported as **ready**.

---

## Features

Each feature below includes a one-line "what it is", why it matters, and a quick way to try it.

### 1. Desktop app + command line

**What:** A desktop application (Overview, Model Library, **Playground**, Observability, Settings) plus a full `lmforge` CLI.

**Why it matters:** Non-technical users get point-and-click; power users and scripts get the CLI. Both talk to the same local daemon — closing the app never stops your models.

**Try it:**
- Open the app — **Overview** shows engine health and loaded models.
- Or run `lmforge status`, `lmforge models list`, `lmforge pull <model>`.

---

### 2. Built-in Playground

**What:** An in-app chat surface for trying models without wiring up a client. Includes a thinking toggle (locked gray when the model always reasons), chat vs thinking sampling profiles, and an **Advanced** popover for temperature / top_p / top_k / penalties.

**Why it matters:** Fastest way to validate a pull, compare models, and tune decoding — no curl or third-party UI required.

**Try it:** **Playground** → pick a model → send a message. For dedicated reasoning catalog entries (e.g. `:thinking` / `:reasoning`), the think control stays on and grayed so you cannot accidentally disable native reasoning.

---

### 3. One-click model download (curated catalog)

**What:** A **Model Library** of recommended, ready-to-run models (chat, vision, embeddings, rerank, code, MTP) with shortcuts like `qwen3:8b:4bit`. You can also paste any Hugging Face repo.

**Why it matters:** No hunting for the right file format or quantization. LMForge resolves the correct files for your engine (GGUF vs MLX, etc.) and downloads them.

**Try it:**
- In the app: **Models → Library**, pick a model, click **Pull**.
- Or CLI: `lmforge pull qwen3:8b:4bit`.

---

### 4. Live download progress — everywhere

**What:** A single, always-visible download indicator under the page header for every pull or background migration. It follows you across screens.

**Why it matters:** You always know what is downloading and how far along it is, without hunting for a status panel.

**Try it:** start any model download and switch screens — the progress line stays visible. Hover the label for file and byte count.

---

### 5. Drop-in OpenAI & Ollama compatible API

**What:** Same shapes your tools already use — OpenAI (`/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank`, `/v1/models`) and Ollama (`/api/chat`, `/api/generate`, `/api/tags`), plus LMForge-native ops under `/lf/*` (hardware, engines, status, model list).

**Why it matters:** Point your IDE plugin, agent, or script at `http://localhost:11430` — usually a one-line URL change.

**Try it:**
```bash
curl http://localhost:11430/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"qwen3:8b:4bit","messages":[{"role":"user","content":"Hello!"}]}'
```

---

### 6. Multi-model orchestration (concurrent roles)

**What:** Load and serve several models on demand — e.g. chat + embeddings at once — with per-model `keep_alive`, LRU eviction under VRAM pressure, and auto-load favorites at startup. Requests for the wrong role on an endpoint are rejected cleanly (embed model on chat, chat model on embed).

**Why it matters:** Real apps need more than one model. LMForge handles load/unload and memory so clients only name the model they want.

**Try it:** pull a chat model and an embed model, hit `/v1/chat/completions` and `/v1/embeddings` in parallel, then check `lmforge status`.

---

### 7. Vision, embeddings, reranking, and code

**What:** Beyond chat — vision (image_url / base64), text embeddings (batched), rerankers, and code models. Capabilities are tagged on every model card and exposed via `/v1/models` / `/lf/model/list`.

**Why it matters:** One local server covers the building blocks for search, RAG, document Q&A, and assistants — not just chatbots.

**Try it:** filter the Model Library by role (**chat / embed / rerank / vision / code**) and pull one of each.

---

### 8. Reasoning ("thinking") models

**What:** First-class support for reasoning models (Qwen3 Thinking, DeepSeek-R1 style, Phi-4 Reasoning). Per-request `think` on/off where the model allows it; dedicated `:thinking` / `:reasoning` catalog models stay **locked on**. Cap depth with `thinking_budget`, stream reasoning live, and keep it separate from the final answer. Chat vs thinking sampling profiles ship in the Playground and Postman collection.

**Why it matters:** Visible chain-of-thought for hard problems, with a budget knob to trade depth for speed — cloud-style controls, fully local.

**Try it:** add `"think": true` (optionally `"thinking_budget": 4096`) to `/v1/chat/completions`, or use the Playground thinking profile.

> **Note — blank replies on reasoning models.** Reasoning tokens share the answer budget. Too small a `max_tokens` can spend everything thinking and return an empty answer. LMForge raises a budget floor for these models (same idea as OpenAI/Anthropic). Prefer a larger `max_tokens` (or a non-reasoning model) for short replies.

---

### 9. Speculative decoding (MTP)

**What:** For GGUF models with Multi-Token Prediction heads, LMForge detects MTP capability and enables llama.cpp draft-MTP speculative decoding when VRAM headroom allows — faster token generation without changing your client API.

**Why it matters:** Same model quality, higher tokens/sec on supported catalog entries (e.g. MTP-tagged Qwen3.5 variants).

**Try it:** pull an MTP catalog model (shortcut includes `:mtp`), chat as usual; `capabilities.mtp` on the model list should be true after pull/heal.

---

### 10. Automatic capability detection (and self-heal)

**What:** On pull (and on daemon startup when the detector version advances), LMForge inspects weights/templates and records capabilities: chat, vision, embeddings, reranking, thinking, `native_reasoning`, MTP, stop tokens, embedding dims, etc. Fixes to detection propagate without forcing a re-download.

**Why it matters:** The UI and API stay honest about what each model can do — including after you upgrade LMForge.

**Try it:** `curl -s http://localhost:11430/v1/models | jq '.data[] | {id, capabilities}'` after a pull or after upgrading the daemon.

---

### 11. Hardware-aware, zero-config engine selection

**What:** On first run, LMForge profiles CPU, GPU, VRAM, and drivers, then selects and installs the optimal engine and build (CUDA vs Vulkan vs oMLX; cuda12 vs cuda13 on Linux NVIDIA). Engine tiers keep risky options opt-in. VRAM-aware admission and LRU eviction protect against over-commit; Windows NVIDIA users get guidance for WDDM sysmem-fallback (a driver setting that can silently tank performance).

**Why it matters:** Fastest setup your machine can support without learning CUDA versions or quantization formats.

**Try it:** hardware panel in the app; `lmforge status` / `lmforge doctor` for active engine and variants; `GET /lf/hardware` and `GET /lf/engines`.

---

### 12. Flexible storage management

**What:** Choose where models live. Move the library to another drive anytime (e.g. `C:` → `D:`) with clear options for existing models:
- **Adopt** — point at an existing folder.
- **Delete** — clear the old location.
- **Delete & re-download** — clean up and re-fetch every model into the new location.

**Why it matters:** Model files are large. Teams need them on the right disk without a manual copy chore.

**How "Delete & re-download" works:** re-download runs **in the background after restart** — the app stays usable with a progress banner, survives restarts, continues past single failures, and offers **Retry**.

**Try it:** **Settings → Storage**, change the models directory, choose **Delete & re-download**, restart, watch the global progress indicator.

---

### 13. Reliable background service + instant restart

**What:** Lightweight background daemon that starts at login and stays out of the way. Restart from the app is fast on every platform. Engine failures surface as structured `last_errors` for diagnosis.

**Why it matters:** It works after reboot, and config changes apply without leaving you on a stuck "stopped" screen.

**Try it:** **Settings → restart the daemon**, or reboot and confirm the engine is ready again.

---

### 14. Built-in observability

**What:** **Observability** dashboard with request metrics (throughput, latency, errors), live logs, and a Prometheus-compatible `/metrics` endpoint — plus live GPU/CPU/memory via `/lf/sysinfo`.

**Why it matters:** Monitor and troubleshoot without extra tooling; plug into existing scrapers.

**Try it:** open **Observability** while sending requests, or scrape `http://localhost:11430/metrics`.

---

### 15. Secure by default

**What:** Binds to `localhost` only by default. Network sharing is explicit: API key and/or trusted CIDR ranges in config.

**Why it matters:** Your AI server is not exposed by accident. Opening it up is deliberate.

**Try it:** by default only your machine reaches the API. Network sharing is configured in `config.toml` (`api_key`, `trusted_networks`).

---

## A 5-minute evaluation

1. **Install** Core + UI (commands above). The engine auto-starts.
2. **Pull a model** from the Library (e.g. a small 4-bit chat model) and watch the live progress line.
3. **Chat** in the **Playground**, or hit the OpenAI-compatible endpoint with `curl`.
4. **Try thinking** on a reasoning model (toggle or locked, plus Advanced sampling).
5. **Point an existing tool** at `http://localhost:11430` — confirm a one-line URL change works.
6. **Optional:** pull an embed model and hit `/v1/embeddings` while chat stays loaded; open **Observability**.
7. **Optional:** move models via Settings → Storage → Delete & re-download.

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

**LMForge is the easiest way to run modern AI models privately on your own hardware — one-command install, desktop app with Playground, multi-model orchestration (chat, vision, embeddings, rerank, thinking, MTP), and a drop-in OpenAI-compatible API on Windows, macOS, and Linux.**
