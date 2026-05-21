# LMForge — Linux Dev Setup (Ubuntu 26.04 + RTX, Proxmox passthrough)

Concise. Box: Ubuntu 26.04, Core Ultra 7 265K, 16 GB RAM, RTX 5060 Ti 16 GB
(GPU passed through from Proxmox). Editor: Cursor on the box itself.

## Why a dev install (not a release)

Iterate locally, restart the daemon after `cargo build`, no release-pipeline
round trips. Mamba stays clean: LMForge manages its **own** SGLang venv
under `~/.lmforge/engines/sglang/venv/`.

---

## 1. System packages (one-time, ~3 min)

```bash
sudo apt-get update && sudo apt-get install -y \
  build-essential pkg-config libssl-dev curl git \
  libgtk-3-dev libappindicator3-dev librsvg2-dev patchelf libxdo-dev \
  libwebkitgtk-6.0-dev          # 26.04 uses 6.0; older Ubuntu uses libwebkit2gtk-4.1-dev
```

CUDA toolkit (SGLang preflight checks `nvcc`):

```bash
nvcc --version || sudo apt-get install -y nvidia-cuda-toolkit
nvidia-smi                       # confirm RTX 5060 Ti is visible inside the VM
```

System Python 3 is only used **once** to bootstrap the SGLang venv. Mamba
is not required by LMForge — keep using it for DocIntel etc.

```bash
python3 --version                # any 3.10+ is fine; system python is enough
```

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
lmforge init                               # probes HW, pip-installs SGLang into its own venv
```

`lmforge init` should print `Engine selected: sglang` (8 GB VRAM threshold).
If it picks `llamacpp`, check `nvidia-smi` works and re-run.

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
curl -s http://127.0.0.1:11430/lf/status | jq '{state, engine, loaded_models}'
# engine.id must be "sglang"

lmforge pull qwen3:1.7b:4bit               # small, fast — proves SGLang loads
curl -s http://127.0.0.1:11430/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3:1.7b:4bit","messages":[{"role":"user","content":"Say OK"}],"max_tokens":8}'
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

## 7. Hardware notes for this box

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
  the guest). CUDA 13.x driver in your VM matches SGLang 0.5.10's
  PyTorch CUDA build.

## 8. Cursor-specific tips

- Open the workspace at `~/lmforge` so MCP/agents see the whole tree.
- Cursor's integrated terminal inherits your shell's env — useful for
  setting `LMFORGE_*` knobs per-session without persisting them.
- For background runs of the daemon, prefer `tmux`/`systemd-run --user`
  over Cursor's terminal so it survives across editor restarts.

## 9. When testing is green → cut a release

```bash
git checkout -b release/0.2.x
# bump versions in Cargo.toml, ui/package.json, ui/src-tauri/{Cargo.toml,tauri.conf.json}
git tag -a v0.2.x -m "..." && git push origin v0.2.x
```

Release workflow at `.github/workflows/release.yml` builds DMG/AppImage/MSI.
