# LMForge — macOS Dev Setup (Apple Silicon)

Concise. Reference box: macOS 14+ (Sonoma / Sequoia), Apple Silicon
(M1 / M2 / M3 / M4) with unified memory. Editor: Cursor on the Mac.

> **End-user install?** Use the README's `curl | bash` install — that's
> the supported path. This document covers the dev-clone workflow.

## Why a dev install (not a release)

Iterate locally, restart the daemon after `cargo build`, no release-
pipeline round-trips.

## Engine tiers

Default on Apple Silicon is **`omlx`** (MLX); **`llama.cpp`** is the Metal
fallback on Intel Macs. NVIDIA-only opt-in engines are gated off on Darwin.

Details: [ADR-001](../architecture/ADR-001-engine-tiers.md). Dev scripts:
[DEV_GUIDE](./DEV_GUIDE.md).

---

## 1. System prerequisites

```bash
# Xcode command-line tools (provides clang, git, make):
xcode-select --install   # idempotent; no-op if already installed

# Homebrew (skip if you already have it):
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

LMForge does **not** require system Python. The `default` tier on macOS
uses native engines (oMLX, llama.cpp) and doesn't touch `pip`. The
`uv`-managed venv bootstrap only runs if you ever install an opt-in
engine — which, on Darwin, is never auto-selectable.

## 2. Toolchains

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# Node 20 (Tauri UI)
brew install node@20
brew link --force --overwrite node@20

# Tauri CLI
cargo install tauri-cli --version "^2" --locked
```

`brew install` adds Apple's signed CLT to the cache; subsequent rebuilds
of `lmforge` are fast (~3 s incremental).

## 3. Clone + first build

```bash
git clone https://github.com/phoenixtb/lmforge ~/lmforge
cd ~/lmforge
cargo build                                                  # debug build
ln -sf "$PWD/target/debug/lmforge" ~/.cargo/bin/lmforge      # `lmforge` on PATH
lmforge init                                                 # ~5s; writes ~/.lmforge/hardware.json
```

`lmforge init` on Apple Silicon should print:

```
✓ Engine selected: omlx v<...>  (default tier)
```

If you see `llamacpp` selected instead on Apple Silicon, your
`hardware.json` probe likely missed Metal — re-run with
`rm ~/.lmforge/hardware.json && lmforge init`.

## 4. Run daemon + UI

```bash
# Terminal A — daemon (Ctrl-C to stop)
RUST_LOG=lmforge=info lmforge start --foreground

# Terminal B — desktop UI (hot-reloads on save)
cd ~/lmforge/ui && npm ci && npm run tauri dev
```

> **What you should see in the UI:**
> - **Overview** — top bar shows `omlx` (or `llamacpp` on Intel) as the
>   active engine; *Engine Load Errors* panel absent.
> - **Settings → Engine** — five engine cards. `omlx` and `llamacpp` are
>   marked compatible + installed; `vllm`, `tabbyapi`, `sglang` show
>   incompatibility with "OS/arch/gpu mismatch (Darwin Aarch64 GPU:Apple)"
>   notes and no install chips.

## 5. Smoke test

The default tier serves MLX (`mlx.json`). Pick a small chat shortcut.

```bash
curl -s http://127.0.0.1:11430/lf/status | jq '{overall_status, engine: .engine, last_errors}'
# overall_status: "ready", engine.id: "omlx", last_errors: {}

lmforge pull qwen3.5:4b:4bit
curl -s http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
        "model": "qwen3.5:4b:4bit",
        "messages": [{"role": "user", "content": "Say OK"}],
        "max_tokens": 8
      }'
```

Embeddings (MLX-format Qwen embed):

```bash
lmforge pull qwen3-embed:0.6b:4bit
curl -s http://127.0.0.1:11430/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3-embed:0.6b:4bit","input":"hello world"}'
```

If a load **fails**, both surfaces light up:

- `curl /lf/status | jq .last_errors` returns the entry
  (`message`, `at`, optional `stderr_tail`).
- The UI Overview mounts an **Engine Load Errors** card you can expand.

See [ADR-003](../architecture/ADR-003-last-errors-surface.md) for the
contract.

## 6. Why no opt-in engines on macOS

vLLM, TabbyAPI/ExLlamaV3, and SGLang are NVIDIA-only on the dev path
(CUDA kernels, no MPS backend). The hardware gates in
`src/engine/registry.rs` (`matches_gpu = "nvidia"`) refuse them on
`(Darwin, Apple)`. If you try anyway:

```bash
$ lmforge engine install vllm
Error: Cannot install engine `vllm` — it does not support this hardware
Caused by: OS/arch/gpu mismatch (Darwin Aarch64 GPU:Apple)
```

When MLX-LM gets a unified inference server or vLLM lands the MPS
backend, we'll re-evaluate. The trigger is documented in
[ADR-001 § Re-evaluation triggers](../architecture/ADR-001-engine-tiers.md).

## 7. Rapid iteration loop

```bash
# After any Rust change:
cargo build && lmforge stop && lmforge start

# After UI change: nothing — `npm run tauri dev` hot-reloads
# After Cargo dep change: cargo will refetch on next build
```

Dev tooling: [DEV_GUIDE](./DEV_GUIDE.md) (`./scripts/lmforge.sh`, E2E,
`dev_test.sh`, `dev_logs.sh`). On macOS use `cd ui && npm run tauri dev`
for the UI — no WebKit dep scripts needed.

## 8. Cleaning up

```bash
# 1. Drop opt-in engine venvs (none expected on Darwin, but harmless):
lmforge clean --engines

# 2. Drop the bundled uv (rarely useful on macOS — uv is never invoked
#    for the default-tier path):
rm -rf ~/.lmforge/engines ~/.lmforge/bin

# 3. Drop downloaded model weights (can be many GB):
rm -rf ~/.lmforge/models

# 4. Full nuke:
lmforge service uninstall 2>/dev/null   # remove launchd plist if installed
lmforge stop 2>/dev/null
rm -rf ~/.lmforge

# 5. Remove the dev symlink:
rm -f ~/.cargo/bin/lmforge ~/.local/bin/lmforge
```

The `~/Library/LaunchAgents/dev.lmforge.daemon.plist` (if you ran
`lmforge service install`) is removed by `lmforge service uninstall`.

## 9. Hardware notes

- **Unified memory.** No separate VRAM. `hardware.json` reports
  `unified_mem: true` and `vram_gb` is set to total RAM. MLX loads
  weights directly into the shared pool.
- **Memory budget.** For an N-billion-parameter model at 4-bit MLX:
  expect ~0.5–0.6 N GB resident. An 8 B model ≈ 4–5 GB, 14 B ≈ 8 GB.
  Leave 8 GB headroom for the OS + Cursor + browser.
- **Activation policy.** The Tauri shell sets `ActivationPolicy::Regular`
  on macOS so the menu bar updates when the LMForge window has focus.
  Closing the window hides it to the tray; the daemon keeps running.
- **No GPU passthrough quirks.** On Apple Silicon the GPU is always
  visible — no driver toggles, no VM passthrough surprises.

## 10. Cursor-specific tips

- Open the workspace at `~/lmforge` so MCP/agents see the whole tree.
- Cursor's integrated terminal inherits your shell's env. Set
  `LMFORGE_*` knobs per-session there without persisting them.
- For background daemon runs prefer `launchctl` (or `lmforge service
  install`) over Cursor's terminal so the daemon survives editor restarts.

## 11. When testing is green → cut a release

See [RELEASE.md](./RELEASE.md).

---

## See also

- [DEV_GUIDE](./DEV_GUIDE.md) — mother scripts and E2E
- [ADR-001](../architecture/ADR-001-engine-tiers.md) — engine tier model
- [ADR-002](../architecture/ADR-002-engines-endpoint.md) — `/lf/engines`
- [ADR-003](../architecture/ADR-003-last-errors-surface.md) — `last_errors`
- [INSTALL_LINUX.md](./INSTALL_LINUX.md) — Linux + NVIDIA
- [INSTALL_WINDOWS.md](./INSTALL_WINDOWS.md) — native Windows + WSL2
