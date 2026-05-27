# LMForge — Windows Dev Setup (native + WSL2)

Concise. Reference box: Windows 11 23H2+, x86_64, NVIDIA RTX GPU (Turing
`sm_75` or newer), 16 GB+ RAM. Editor: Cursor on Windows (or inside WSL2).

> **End-user install?** See the README's Windows section — download
> `lmforge-windows-x86_64.exe` from Releases and run `lmforge init`. This
> document covers the dev-clone workflow.

## Windows native vs WSL2 — pick first

LMForge supports both targets but the **opt-in engine matrix differs**.
Always pick the path that matches what you intend to test:

| Path | Tier on this host | Best for |
| --- | --- | --- |
| **Windows native** | `default: llamacpp` + `opt-in: tabbyapi` (EXL3 single-stream). **`vllm` is NOT offered** — upstream wheels are Linux/WSL2-only. SGLang is refused on `sm_120`. | UI testing, single-stream chat, EXL3 INT4 throughput. |
| **WSL2 (Ubuntu inside)** | `default: llamacpp` + `opt-in: vllm` + `opt-in: tabbyapi`. Same matrix as native Linux. | vLLM concurrent batching, multi-engine smoke tests. |

The hardware probe sets `is_wsl: true` and `os_family: "windows-wsl2"`
under WSL2; native Windows reports `os_family: "windows-native"`.
`lmforge engine list` (and the UI's Settings → Engine page) prints the
verdict so you never have to guess.

Full per-platform matrix is in
[ADR-001 § OS / hardware support matrix](architecture/ADR-001-engine-tiers.md).

---

# Path A — Windows native (PowerShell)

## A.1 System prerequisites

Open an **Administrator PowerShell**:

```powershell
# Visual Studio Build Tools (MSVC + Windows SDK — required for cargo build)
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# Git, curl
winget install --id Git.Git -e
winget install --id curl.curl -e   # Win11 already ships curl; skip if present

# WebView2 runtime (Tauri UI). Win11 ships it preinstalled; idempotent on Win10:
winget install --id Microsoft.EdgeWebView2Runtime -e

# Node 20 (Tauri UI)
winget install --id OpenJS.NodeJS.LTS -e
```

Restart PowerShell so the new tools land on `PATH`.

> No system Python is required. LMForge bootstraps its own `uv` for any
> opt-in engine. The default-tier `llama.cpp` path is pure binary.

## A.2 NVIDIA driver + CUDA toolkit (only for opt-in engines)

`llama.cpp` (default) and TabbyAPI/ExLlamaV3 (opt-in) only need an NVIDIA
driver — they don't link CUDA toolkit. Install the toolkit only if you
plan to opt-in to **vLLM under WSL2** (Path B below). For Path A:

```powershell
nvidia-smi   # must succeed and show your RTX
```

If `nvidia-smi` is missing, install the latest Game Ready or Studio driver
from nvidia.com — that ships `nvidia-smi.exe`.

## A.3 Toolchains

```powershell
# Rust — pick the MSVC ABI host triple
Invoke-WebRequest -Uri https://win.rustup.rs/x86_64 -OutFile rustup-init.exe
.\rustup-init.exe -y --default-host x86_64-pc-windows-msvc
$env:Path += ";$env:USERPROFILE\.cargo\bin"

# Tauri CLI
cargo install tauri-cli --version "^2" --locked
```

## A.4 Clone + first build

```powershell
cd $HOME
git clone https://github.com/phoenixtb/lmforge
cd lmforge
cargo build                                                    # debug
# Put the dev binary on PATH:
New-Item -ItemType SymbolicLink -Path "$env:USERPROFILE\.cargo\bin\lmforge.exe" `
         -Target "$PWD\target\debug\lmforge.exe" -Force
lmforge init
```

Expected:

```
✓ Engine selected: llamacpp v<b9351>  (default tier)
```

If `lmforge init` complains that it can't write to `C:\Users\<you>\.lmforge\`,
make sure your user has write access to that path — LMForge never asks
for elevated privileges.

## A.5 Run daemon + UI

```powershell
# Terminal A — daemon (Ctrl-C to stop)
$env:RUST_LOG="lmforge=info"
lmforge start --foreground

# Terminal B — desktop UI (hot-reloads on save)
cd ui
npm ci
npm run tauri dev
```

> **What you should see in the UI on Windows native:**
> - **Overview** — `llamacpp` is the active engine.
> - **Settings → Engine** — five cards. `llamacpp` ✓ active; `tabbyapi`
>   ✓ compatible with an `lmforge engine install tabbyapi` chip;
>   **`vllm` is incompatible** with reason "Compute-capability or
>   OS-family gate refused this combo" (vLLM gates require Linux or
>   WSL2); `omlx` incompatible (`Darwin`-only); `sglang` incompatible.

## A.6 Smoke test (native Windows)

```powershell
curl http://127.0.0.1:11430/lf/status | jq '{overall_status, engine: .engine, last_errors}'

lmforge pull qwen3:1.7b:4bit
curl http://127.0.0.1:11430/v1/chat/completions `
  -H "Content-Type: application/json" `
  -d '{\"model\":\"qwen3:1.7b:4bit\",\"messages\":[{\"role\":\"user\",\"content\":\"Say OK\"}],\"max_tokens\":8,\"chat_template_kwargs\":{\"enable_thinking\":false}}'
```

If a load **fails**, both surfaces light up the same way as Linux —
`/lf/status.last_errors` is populated and the UI Overview mounts the
Engine Load Errors panel. See
[ADR-003](architecture/ADR-003-last-errors-surface.md).

## A.7 Optional: TabbyAPI (EXL3) on native Windows

TabbyAPI **does** support native Windows when CUDA + an MSVC toolchain
are present. Install pre-reqs from
[ExLlamaV3 prerequisites](https://github.com/turboderp-org/exllamav3) and:

```powershell
lmforge engine install tabbyapi    # ~5 GB, requires nvcc on PATH
lmforge stop
lmforge start --engine tabbyapi
```

EXL3 models live on Hugging Face git **branches** (e.g.
`turboderp/Qwen3-8B-exl3@6.0bpw`). The `repo@revision` syntax is wired
through both `lmforge pull` and the catalog resolver — see
[INSTALL_LINUX_DEV.md § 6](./INSTALL_LINUX_DEV.md#6-optional-installing-the-opt-in-engines)
for usage examples. The Windows-native experience is identical.

> **Want vLLM?** Switch to Path B (WSL2). Upstream vLLM has no native
> Windows wheels.

---

# Path B — WSL2 (Ubuntu inside)

If you need vLLM, want the same dev workflow as your Linux teammates, or
hit upstream Windows quirks — flip to WSL2. This sub-path is a thin
wrapper around the [Linux dev guide](./INSTALL_LINUX_DEV.md).

## B.1 Enable WSL2 (one-time)

In an Admin PowerShell:

```powershell
wsl --install -d Ubuntu-24.04
wsl --set-default-version 2
wsl --update
```

Reboot when prompted. Then `wsl` to drop into Ubuntu.

## B.2 GPU passthrough (NVIDIA)

WSL2 ↔ NVIDIA driver passthrough is **driver-side** — install the latest
Game Ready / Studio driver on **Windows**. Inside WSL2:

```bash
nvidia-smi   # must succeed; shares the Windows-side driver
```

If `nvidia-smi` is absent inside WSL2, your Windows driver predates the
CUDA-in-WSL release. Update it.

## B.3 Continue with the Linux dev guide

From here onward the WSL2 path is identical to native Linux — `uv`-managed
venvs, opt-in `vllm` / `tabbyapi`, `cu130` torch wheels via auto-
detection. Follow [`INSTALL_LINUX_DEV.md`](./INSTALL_LINUX_DEV.md) starting
at **§ 1 — System packages**. The Ubuntu 24.04 paths apply unchanged.

When the hardware probe reports `os_family: windows-wsl2`, the engine
registry unlocks both `vllm` and `tabbyapi`. The UI's Settings → Engine
page then shows `vllm` as compatible.

## B.4 UI display from WSL2

Two options:

1. **Run the Tauri UI on Windows side**, point it at the WSL2 daemon. The
   daemon is `localhost:11430` from Windows because WSL2 ports forward
   transparently in recent Windows builds. Cleanest dev loop.
2. **Run the UI inside WSL2** via WSLg. `npm run tauri dev` works under
   WSLg without extra config on Win11 23H2+, but UI build times are a bit
   slower than native.

---

# Cleaning up

## Native Windows

```powershell
# 1. Drop opt-in engine venvs:
lmforge clean --engines

# 2. Drop downloaded model weights:
Remove-Item -Recurse -Force "$env:USERPROFILE\.lmforge\models"

# 3. Stop + remove service:
lmforge service uninstall   # removes the Scheduled Task
lmforge stop

# 4. Full nuke:
Remove-Item -Recurse -Force "$env:USERPROFILE\.lmforge"

# 5. Remove the dev symlink:
Remove-Item "$env:USERPROFILE\.cargo\bin\lmforge.exe"
```

## WSL2

Use the cleanup section of [`INSTALL_LINUX_DEV.md § 8`](./INSTALL_LINUX_DEV.md#8-cleaning-up)
inside WSL2 — paths and commands are identical.

---

# Hardware + driver notes

- **Native Windows + RTX 50-series (`sm_120`)**: `llama.cpp` works
  out-of-the-box because the bundled binary ships `sm_120` cubins.
  TabbyAPI works via JIT-compiled kernels (`cu128`+ wheels).
- **`sm_120` + vLLM**: only via WSL2. The hardware gate
  (`compute_cap = (12, 0)` × `os_family = "windows-native"`) refuses the
  install with a clear error.
- **NVFP4 + MoE on `sm_120` under WSL2 vLLM**: same upstream
  attention-path bug as documented in
  [INSTALL_LINUX_DEV.md § 9](./INSTALL_LINUX_DEV.md#9-hardware-notes-for-the-reference-box).
- **WSL2 GPU memory**: `nvidia-smi` inside WSL2 reports the same VRAM
  the Windows-side driver sees. No double-counting.

# Cursor-specific tips

- For native Windows dev, open the workspace at `C:\Users\<you>\lmforge`.
- For WSL2 dev, install the Cursor + WSL extension and open the folder
  remotely (`\\wsl$\Ubuntu-24.04\home\<you>\lmforge`). All toolchains live
  inside WSL2 — Cursor's terminal shells into the right env automatically.
- Background daemon runs: prefer `lmforge service install` (Scheduled
  Task on Windows, systemd-user inside WSL2) so the daemon survives
  Cursor restarts.

---

## See also

- [ADR-001](architecture/ADR-001-engine-tiers.md) — engine tier model
  and full per-platform support matrix.
- [ADR-002](architecture/ADR-002-engines-endpoint.md) — `/lf/engines`
  endpoint + UI tier-switcher contract.
- [ADR-003](architecture/ADR-003-last-errors-surface.md) —
  `last_errors` / `stderr_tail` failure surface contract.
- [`INSTALL_LINUX_DEV.md`](./INSTALL_LINUX_DEV.md) — native Linux
  workflow (also the canonical WSL2 reference).
- [`INSTALL_MACOS_DEV.md`](./INSTALL_MACOS_DEV.md) — Apple Silicon
  variant.
