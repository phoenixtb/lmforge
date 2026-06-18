# LMForge — Linux Dev Setup (Ubuntu 24.04 / 26.04 + RTX, Proxmox passthrough)

Concise. Reference box: Ubuntu 24.04 or 26.04, Core Ultra 7 265K, 16 GB RAM,
RTX 5060 Ti 16 GB (consumer Blackwell, `sm_120`, GPU passed through from
Proxmox). Editor: Cursor on the box itself.

> **End-user install?** Use the README's `curl | bash` install — that path
> works on every supported Linux + GPU combo. This document covers the
> dev-clone workflow with hot-rebuild and stdout-noise visibility.

## Why a dev install (not a release)

Iterate locally, restart the daemon after `cargo build`, no release-pipeline
round-trips. **No system Python required** — LMForge ships its own `uv`
(Astral's static toolchain manager, ~24 MB, sha256-verified) at
`~/.lmforge/bin/uv`. Opt-in engines (`vllm`, `tabbyapi`) get their own
isolated venvs under `~/.lmforge/engines/<id>/venv/`.

## Engine tiers

Default on Linux is **`llama.cpp`** (Vulkan, CUDA12, or CUDA13 per driver —
see variant table below). Opt-in: **`vllm`**, **`tabbyapi`**. Experimental
**`sglang`** is refused on consumer Blackwell (`sm_120`).

Full tier model and OS matrix: [ADR-001](../architecture/ADR-001-engine-tiers.md).
Day-to-day dev scripts: [DEV_GUIDE](./DEV_GUIDE.md).

---

## 1. System packages (one-time, ~3 min)

```bash
sudo apt-get update && sudo apt-get install -y \
  build-essential pkg-config libssl-dev curl git \
  libgtk-3-dev libappindicator3-dev librsvg2-dev patchelf libxdo-dev
```

Webkit dev headers (only needed if you build the UI from source — Tauri 2):

| Ubuntu | Package |
|---|---|
| 22.04 | `libwebkit2gtk-4.0-dev` (Tauri 2 may not work; upgrade recommended) |
| **24.04** | `libwebkit2gtk-4.1-dev` |
| 26.04 | `libwebkitgtk-6.0-dev` |

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev   # Ubuntu 24.04
# sudo apt-get install -y libwebkitgtk-6.0-dev  # Ubuntu 26.04
```

### Variant matrix (what `lmforge init` will fetch)

| Your hardware | Variant installed | Notes |
|---|---|---|
| Linux x86_64 + NVIDIA, driver ≥ r570.26 | **cuda12** (manifest tarball) | Custom CUDA 12.8.1 build with bundled cudart/cuBLAS, `sm_86…sm_120`. Default on Blackwell. |
| Linux x86_64 + NVIDIA, driver ≥ r590.44 | **cuda13** (opt-in) | `lmforge engine install llamacpp --variant cuda13` or `LMFORGE_LLAMACPP_VARIANT=cuda13` |
| Linux x86_64 + NVIDIA, driver < r570.26 | **vulkan** (legacy upstream) | Auto-fallback; `lmforge doctor` prints upgrade hint |
| Linux x86_64 + AMD / Intel iGPU | **vulkan** (legacy upstream) | One binary for all non-NVIDIA GPUs |
| Linux x86_64 + no GPU | **cpu** (legacy upstream) | CPU-only build |
| Linux aarch64 + GPU | **vulkan** ARM64 | Jetson / Rockchip |
| Linux aarch64 + no GPU | **cpu** ARM64 | |

CUDA variants live under `~/.lmforge/engines/llamacpp/variants/<id>/` with
bundled libs + RPATH. Vulkan/CPU still use the legacy flat layout until
those entries land in the variants manifest.

Override selection with `LMFORGE_LLAMACPP_VARIANT={cuda12,cuda13,cpu,gpu}`.
Use `lmforge doctor` to see installed variants and which one is **ACTIVE**.

### Speculative decoding (MTP)

On chat models with MTP/nextn tensors in the GGUF, `mode=auto` enables
`--spec-type draft-mtp` on CUDA (primary) or Vulkan (fallback). Telemetry
(`spec_mode`, accept-rate) surfaces on `/lf/status` and the Overview UI.

**Important:** standard Unsloth `Qwen3.5-*-GGUF` quants strip MTP heads.
Use dedicated MTP repos (e.g. `unsloth/Qwen3.5-4B-MTP-GGUF`) — shortcut
`qwen3.5:4b:mtp:4bit`. After pull, verify with:

```bash
lmforge pull qwen3.5:4b:mtp:4bit --refresh   # mtp=Some(true) in output
```

Non-MTP models may still get draft-model speculation via curated pairs
(`data/draft_pairs.toml`, e.g. `qwen3:8b:4bit` + installed `qwen3:0.6b:4bit`).

### CUDA toolkit (only required for `opt-in` engines)

`llama.cpp` (default tier) uses Vulkan on Linux and has **no** `nvcc`
requirement. Only install CUDA if you plan to opt-in to vLLM or
TabbyAPI/ExLlamaV3 — both need `nvcc` to JIT-compile their kernels.

```bash
nvidia-smi                       # confirm RTX visible inside VM (driver-side)

# Skip the next two lines if you only intend to use llama.cpp:
nvcc --version || sudo apt-get install -y nvidia-cuda-toolkit
# If nvcc lives at /usr/local/cuda/bin/ but isn't on PATH:
echo 'export PATH=/usr/local/cuda/bin:$PATH' >> ~/.bashrc && source ~/.bashrc
```

The driver lives in the guest (passthrough), the toolkit is host-side.
`nvidia-smi` working without `nvcc` is the normal initial state.

### System Python is **not** required

LMForge bootstraps its own `uv` on first `lmforge init`. If your system has
no Python ≥ 3.10, `uv` fetches a managed interpreter on demand. The dev
clone never touches system pip.

## 2. Toolchains

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# Node 20 (Tauri UI)
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# Tauri CLI
cargo install tauri-cli --version "^2" --locked
```

## 3. Clone + first build

```bash
git clone https://github.com/phoenixtb/lmforge ~/lmforge
cd ~/lmforge
cargo build                                                  # debug build
ln -sf "$PWD/target/debug/lmforge" ~/.cargo/bin/lmforge      # `lmforge` on PATH
lmforge init                                                 # ~10s on this box
```

`lmforge init` writes `~/.lmforge/hardware.json` (probe result) and seeds
the catalog files. On a Linux + NVIDIA box it should print:

```
✓ Engine selected: llamacpp v<b9351>  (default tier)
```

If you see anything else as the default, your `hardware.json` probably has
a stale schema — run `rm ~/.lmforge/hardware.json && lmforge init` to
re-probe.

## 4. Run daemon + UI

```bash
# Terminal A — daemon (Ctrl-C to stop)
RUST_LOG=lmforge=info lmforge start --foreground

# Terminal B — desktop UI (hot-reloads on save). Or run the wrapper:
scripts/util/dev_ui_ubuntu24.sh
```

The Tauri window and a browser tab on :1420 both talk to the daemon at
`http://127.0.0.1:11430`. Same `~/.lmforge/` data dir for both.

> **What you should see in the UI after step 3:**
> - **Overview** — *Engine Load Errors* panel is absent (nothing failed
>   yet); top bar shows `llamacpp` as the active engine.
> - **Settings → Engine** — five engine cards. `llamacpp` is green
>   (`active` badge); `vllm` / `tabbyapi` show "yes" compatible but
>   "no" installed plus a copy-able `lmforge engine install …` chip;
>   `sglang` is red with "Compute-capability or OS-family gate refused
>   this combo" when running on `sm_120` hardware.

## 5. Smoke test

The default tier serves GGUF (`gguf.json`). Pick a small chat shortcut.

```bash
curl -s http://127.0.0.1:11430/lf/status | jq '{overall_status, engine: .engine, last_errors}'
# overall_status: "ready", engine.id: "llamacpp", last_errors: {}

lmforge pull qwen3:1.7b:4bit
curl -s http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
        "model": "qwen3:1.7b:4bit",
        "messages": [{"role": "user", "content": "Say OK"}],
        "max_tokens": 8,
        "chat_template_kwargs": {"enable_thinking": false}
      }'
```

For embeddings the default catalog includes `qwen3-embed:0.6b:8bit`:

```bash
lmforge pull qwen3-embed:0.6b:8bit
curl -s http://127.0.0.1:11430/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3-embed:0.6b:8bit","input":"hello world"}'
```

If a load **fails**, both surfaces light up:

- `curl /lf/status | jq .last_errors` returns the entry
  (`message`, `at`, optional `stderr_tail`).
- The UI Overview mounts an **Engine Load Errors** card you can expand
  to see the last 32 lines of the engine's stderr inline.

See [ADR-003](../architecture/ADR-003-last-errors-surface.md) for the contract.

## 6. Optional: installing the opt-in engines

Try the default tier (`llama.cpp`) first. Only reach for an opt-in engine
when you have a concrete reason:

| Engine | Pick when |
| --- | --- |
| `vllm` | You need concurrent-request batching (4+ in-flight); building a multi-tenant frontend. Single-stream tok/s is within ~15 % of llama.cpp. |
| `tabbyapi` (ExLlamaV3) | You want fastest **single-stream INT4** on Ada / Blackwell. EXL3 weights live on git branches like `repo@6.0bpw` — see catalog comments. |

Both need `nvcc` on `PATH`. Install:

```bash
lmforge engine install vllm        # ~5 GB, ~5 min on a typical link
lmforge engine install tabbyapi    # ~5 GB, ~5 min
```

Switch at runtime:

```bash
lmforge stop
lmforge start --engine vllm
# or for a one-off command:
lmforge run --engine vllm qwen3:8b:4bit
```

The UI's Settings → Engine page surfaces the same `lmforge engine install`
commands as copy-to-clipboard chips per row. Installs run in your terminal
on purpose — see [ADR-002](../architecture/ADR-002-engines-endpoint.md) for
why we don't trigger them from the GUI.

### PyTorch wheel auto-detection

`torch-backend=auto` is the default — `uv` reads your CUDA driver via
`nvidia-smi` and picks the matching wheel set (`cu130`, `cu128`, …).
On a 5060 Ti with driver 580.95 / CUDA 13.x you'll get `cu130` wheels.

To pin:

```bash
UV_TORCH_BACKEND=cu130 lmforge engine install vllm
```

To force CPU wheels on a no-GPU box:

```bash
UV_TORCH_BACKEND=cpu lmforge engine install vllm
```

## 7. Rapid iteration loop

```bash
# After any Rust change:
cargo build && lmforge stop && lmforge start

# After UI change: nothing — `npm run tauri dev` hot-reloads
# After UI/Cargo deps change: `npm ci` (UI) or wait for cargo to refetch
```

Useful helpers: [DEV_GUIDE](./DEV_GUIDE.md) (mother menu `./scripts/lmforge.sh`,
E2E tiers, `dev_test.sh`, `dev_logs.sh`, UI wrappers).

## 8. Cleaning up

`lmforge init` writes everything under `~/.lmforge/`. Nothing else is
touched on your system — no global pip, no `/opt`, no `/usr/local`.

```bash
# 1. Drop all opt-in engine venvs (default-tier binaries stay):
lmforge clean --engines

# 2. Drop one engine's venv only:
lmforge engine uninstall vllm

# 3. Drop the bundled uv too (forces fresh ~24 MB download next init):
rm -rf ~/.lmforge/engines ~/.lmforge/bin

# 4. Drop downloaded model weights (can be many GB):
rm -rf ~/.lmforge/models

# 5. Full nuke (everything LMForge ever wrote):
lmforge service uninstall 2>/dev/null   # remove systemd-user unit if present
lmforge stop 2>/dev/null
rm -rf ~/.lmforge

# 6. Remove the binary symlink (dev clone):
rm -f ~/.cargo/bin/lmforge ~/.local/bin/lmforge
```

The `nvidia-cuda-toolkit` apt package stays — LMForge never installs it
for you.

## 9. Hardware notes for the reference box

- **16 GB system RAM is tight.** Avoid running two opt-in engines
  concurrently — both vLLM and TabbyAPI peak at 4–6 GB host RAM during
  load before settling on the GPU.
- **`llama.cpp` defaults are good.** The bundled binary respects
  `LMFORGE_LLAMACPP_BIN` if you point it at a custom build. `--cache-ram`
  is wired up; expect ~1 GB host RAM for the KV cache on an 8 B model.
- **vLLM single-GPU is marginal.** The UI's install hint surfaces this:
  on a single-GPU desktop, vLLM tok/s is within ~15 % of `llama.cpp` for
  single-stream chat; the win is concurrent batching.
- **NVFP4 + MoE on `sm_120`** still has an upstream attention-path bug
  under batch>1. Workaround: stay at batch=1 OR use AWQ/GPTQ-4bit quants.
  Standard dense models work fine. (`lmforge engine install vllm` prints
  this caveat too.)
- **GPU passthrough quirks**: if `nvidia-smi` works but `nvcc` doesn't,
  install `nvidia-cuda-toolkit` (toolkit is host-side; driver lives in
  the guest). On this box the driver reports CUDA 13.x and `uv` installs
  `cu130` torch wheels via `--torch-backend=auto`.

## 10. Cursor-specific tips

- Open the workspace at `~/lmforge` so MCP/agents see the whole tree.
- Cursor's integrated terminal inherits your shell's env — useful for
  setting `LMFORGE_*` / `UV_TORCH_BACKEND` knobs per-session without
  persisting them.
- For background daemon runs prefer `tmux` / `systemd-run --user` over
  Cursor's terminal so the daemon survives editor restarts.
- Cursor sandbox redirects writes to `target/` to
  `/tmp/cursor-sandbox-cache/...`. If you build via the agent, copy the
  result back:
  ```bash
  cp /tmp/cursor-sandbox-cache/*/cargo-target/debug/lmforge target/debug/
  ```

## 11. When testing is green → cut a release

See [RELEASE.md](./RELEASE.md).

---

## See also

- [DEV_GUIDE](./DEV_GUIDE.md) — mother scripts and E2E
- [ADR-001](../architecture/ADR-001-engine-tiers.md) — engine tier model
- [ADR-002](../architecture/ADR-002-engines-endpoint.md) — `/lf/engines`
- [ADR-003](../architecture/ADR-003-last-errors-surface.md) — `last_errors`
- [INSTALL_MACOS.md](./INSTALL_MACOS.md) — Apple Silicon
- [INSTALL_WINDOWS.md](./INSTALL_WINDOWS.md) — native Windows + WSL2
