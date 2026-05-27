# LMForge — Linux Dev Setup (Ubuntu 24.04 / 26.04 + RTX, Proxmox passthrough)

Concise. Reference box: Ubuntu 24.04 or 26.04, Core Ultra 7 265K, 16 GB RAM,
RTX 5060 Ti 16 GB (GPU passed through from Proxmox). Editor: Cursor on the box
itself.

## Why a dev install (not a release)

Iterate locally, restart the daemon after `cargo build`, no release-pipeline
round trips. Mamba stays clean: LMForge manages its **own** SGLang venv
under `~/.lmforge/engines/sglang/venv/` via a bundled `uv` binary
(`~/.lmforge/bin/uv`, sha256-verified, ~24 MB, downloaded once on first
`lmforge init`). No `python3-venv` / `ensurepip` / system pip required.

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

Pick the one for your release:
```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev   # Ubuntu 24.04
# sudo apt-get install -y libwebkitgtk-6.0-dev  # Ubuntu 26.04
```

CUDA toolkit — required by SGLang preflight (`nvcc`):
```bash
nvidia-smi                       # confirm RTX visible inside VM (driver-side)
nvcc --version || sudo apt-get install -y nvidia-cuda-toolkit
# If nvcc is at /usr/local/cuda/bin/ but not on PATH:
echo 'export PATH=/usr/local/cuda/bin:$PATH' >> ~/.bashrc && source ~/.bashrc
```

System Python is **not** required. LMForge ships its own `uv` (Astral's
static Python toolchain manager) on first `lmforge init`. If your system has
no Python ≥ 3.10, uv will fetch a managed interpreter on demand.

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
cargo build                                # debug build
ln -sf "$PWD/target/debug/lmforge" ~/.cargo/bin/lmforge   # `lmforge` on PATH
lmforge init                               # uv venv → SGLang install (~2 min)
```

`lmforge init` should print `Engine selected: sglang` (8 GB VRAM threshold).
If it picks `llamacpp`, check `nvidia-smi` works and re-run.

`torch-backend=auto` is the default — uv detects your CUDA driver via
`nvidia-smi` and picks the matching wheel set (`cu130`, `cu129`, `cu128`, …).
To pin: `UV_TORCH_BACKEND=cu130 lmforge init`. To force CPU on a no-GPU box:
`UV_TORCH_BACKEND=cpu lmforge init`.

## 4. Run daemon + UI

```bash
# Terminal A — daemon (Ctrl-C to stop)
RUST_LOG=lmforge=info lmforge start

# Terminal B — desktop UI (hot-reloads on save)
cd ~/lmforge/ui && npm ci && npm run tauri dev
```

The Tauri window and a browser tab on :1420 both talk to the daemon at
`http://127.0.0.1:11430`. Same `~/.lmforge/` data dir for both.

## 5. Smoke test

```bash
curl -s http://127.0.0.1:11430/lf/status | jq '{overall_status, engine, running_models}'
# overall_status: "ready", engine.id: "sglang", running_models: []

# Linux+SGLang uses the safetensors catalog. Use catalog-resident shortcuts:
lmforge pull qwen3:1.7b
curl -s http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3:1.7b","messages":[{"role":"user","content":"Say OK"}],"max_tokens":8}'
```

If the first model load fails with `ModuleNotFoundError: sglang`, the venv
wasn't picked up. Confirm `~/.lmforge/engines/sglang/venv/bin/python3`
exists; if not, re-run `lmforge init`.

## 6. Rapid iteration loop

```bash
# After any Rust change:
cargo build && lmforge stop && lmforge start

# After UI change: nothing — tauri dev hot-reloads
# After UI/Cargo deps change: `npm ci` (UI) or wait for cargo to refetch
```

## 7. Cleaning up

`lmforge init` creates everything under `~/.lmforge/`. Nothing else is
touched on your system — no global pip, no /opt, no /usr/local. Cleanup
options, from light to nuclear:

```bash
# 1. Drop just the SGLang venv (and any other engine pip installs):
rm -rf ~/.lmforge/engines              # re-run `lmforge init` to recreate

# 2. Also drop the bundled uv (forces fresh ~24 MB download next init):
rm -rf ~/.lmforge/engines ~/.lmforge/bin

# 3. Drop downloaded models (HF weights live here, can be many GB):
rm -rf ~/.lmforge/models

# 4. Full nuke (everything LMForge ever wrote):
lmforge service uninstall 2>/dev/null   # remove systemd-user unit if present
lmforge stop 2>/dev/null                # stop running daemon
rm -rf ~/.lmforge

# 5. Remove the binary itself (dev symlink + any release install):
rm -f ~/.cargo/bin/lmforge ~/.local/bin/lmforge
```

There is no system-wide footprint. The `nvidia-cuda-toolkit` apt package
(if installed) stays — LMForge never installs it for you, only suggests it.

## 8. Hardware notes for this box

- **16 GB system RAM is tight.** SGLang stages weights through CPU RAM
  on load. An 8B AWQ model peaks ~6 GB during load. Avoid running two
  models concurrently until weights are on the GPU; once loaded, host
  RAM drops back to ~1–2 GB per slot.
- **Default `LMFORGE_SGLANG_MEM_FRACTION=0.5`** reserves 8 GB of the
  16 GB VRAM for KV cache. For a single-slot deployment bump to `0.8`
  in `~/.bashrc` or before `lmforge start`:
  ```bash
  export LMFORGE_SGLANG_MEM_FRACTION=0.8
  ```
- **GPU passthrough quirks**: if `nvidia-smi` works but `nvcc` doesn't,
  install `nvidia-cuda-toolkit` (toolkit is host-side; driver lives in
  the guest). On this box the driver reports CUDA 13.2 and uv installs
  cu130 torch wheels via `--torch-backend=auto`.

## 9. Cursor-specific tips

- Open the workspace at `~/lmforge` so MCP/agents see the whole tree.
- Cursor's integrated terminal inherits your shell's env — useful for
  setting `LMFORGE_*` / `UV_TORCH_BACKEND` knobs per-session without
  persisting them.
- For background runs of the daemon, prefer `tmux`/`systemd-run --user`
  over Cursor's terminal so it survives across editor restarts.
- Cursor sandbox redirects writes to `target/` to
  `/tmp/cursor-sandbox-cache/...`. If you build via the agent, copy the
  result back: `cp /tmp/cursor-sandbox-cache/*/cargo-target/debug/lmforge target/debug/`.

## 10. When testing is green → cut a release

```bash
git checkout -b release/0.2.x
# bump versions in Cargo.toml, ui/package.json, ui/src-tauri/{Cargo.toml,tauri.conf.json}
git tag -a v0.2.x -m "..." && git push origin v0.2.x
```

Release workflow at `.github/workflows/release.yml` builds DMG/AppImage/MSI.
