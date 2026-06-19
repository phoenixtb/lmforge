# Release Process

Applies to core + UI releases cut from `main`. The hard rule: **a tag is only
pushed from a clean, pushed, CI-green commit, and a release is only published
after the E2E gate passed on every platform profile it ships to** (Apple
Silicon, Linux CUDA, Linux non-CUDA, Windows CUDA, Windows non-CUDA — see the
[E2E platform matrix](#e2e-platform-matrix)). CI's `e2e.yml` covers the three
OSes; CUDA-specific paths must be smoke-tested on real GPU hardware.

Workflows involved:

| Workflow | Trigger | Purpose |
|---|---|---|
| `ci.yml` | push / PR to main | fmt, clippy (`-D warnings`), `cargo test` on ubuntu/macos/windows |
| `e2e.yml` | push / PR to main | install → health → sysinfo → service → uninstall lifecycle, all 3 OSes, local build |
| `release.yml` | tag push `v*` | build artifacts → **e2e-gate against the exact artifacts** → create **draft** release |

---

## 1. Pre-release

**Toolchain prerequisites** (the e2e harness will abort with these exact hints
if missing): Rust via **rustup** — `curl --proto '=https' --tlsv1.2 -sSf
https://sh.rustup.rs | sh -s -- -y && source "$HOME/.cargo/env"` (Windows:
`winget install Rustlang.Rustup`); Node.js LTS for the UI build (`brew install
node` / `winget install OpenJS.NodeJS.LTS`). Use rustup, not `brew install
rust` — it wires `~/.cargo/bin` into your shell profiles and ships
clippy/rustfmt.

1. **Freeze scope.** No unrelated changes after this point; only release fixes.
2. **Version bump** (all must match):
   - `Cargo.toml` → `[package] version`
   - `ui/package.json` → `version` (then run `npm install` in `ui/` to sync `package-lock.json`)
   - `ui/src-tauri/Cargo.toml` → `version`
   - `ui/src-tauri/tauri.conf.json` → `version`
   - `cargo build` once to refresh `Cargo.lock`
3. **Local checks** (run on any dev box; Linux-cfg lints are caught by CI only):

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets
   ```

4. **Full pre-release E2E** from the current code (see [DEV_GUIDE.md](./DEV_GUIDE.md)).
   The `e2e --source local` cycle is the same everywhere: full clean → build core
   **and UI** → install both → lifecycle → multi-model inference (auto-pulls
   required models) → full purge (incl. models). The harness auto-detects the
   platform, sources `cargo` from `~/.cargo` if it isn't already on PATH, and
   auto-skips capability suites the active engine can't run — so the command is
   identical per OS. **Run it on every platform you ship to** (see the matrix
   below); a green gate on one platform does not cover the others.

   See [§E2E platform matrix](#e2e-platform-matrix) for the exact command and
   what each platform exercises.

   Quick lifecycle-only gate (no model pull, no UI build): add `--no-inference
   --no-ui` (`-NoInference -NoUi`). Use `--no-build` (`-NoBuild`) to reuse an
   existing `target/release` binary.

### E2E platform matrix

The default engine and which inference suites run depend on the platform. The
harness picks the engine and skips unsupported suites automatically — the table
documents what actually gets exercised and the override knobs.

| Platform | Default engine | MTP / spec-dec | Command |
|---|---|---|---|
| **Apple Silicon** (macOS arm64) | oMLX (MLX) | **auto-skipped** (oMLX `spec_mode=Off`) | `./scripts/lmforge.sh e2e --source local` |
| **Linux + NVIDIA/CUDA** | llama.cpp | on | `./scripts/lmforge.sh e2e --source local` |
| **Linux non-CUDA** (CPU / AMD / Intel — Vulkan) | llama.cpp | on (CPU/Vulkan, slow) | `./scripts/lmforge.sh e2e --source local` |
| **Windows + NVIDIA/CUDA** | llama.cpp (CUDA build) | on | `powershell -File scripts\lmforge.ps1 e2e -Source local` |
| **Windows non-CUDA** (CPU / AMD / Intel — Vulkan) | llama.cpp (Vulkan build) | on | `powershell -File scripts\lmforge.ps1 e2e -Source local` |

Notes / overrides:

- **MTP** is a `llama-server` feature, so it only runs on the llama.cpp engine
  (Linux/Windows). On Apple Silicon the harness passes `--skip-mtp` for you and
  does not pull the 4B MTP GGUF. Force it on (e.g. to test a draft-pair) with
  `E2E_WITH_MTP=1`.
- **VLM** and **rerank** self-skip at runtime when the active engine or model
  lacks the capability — no flag needed. Disable explicitly with
  `--skip-vlm` / `--skip-rerank` if you want a faster core-only inference pass.
- **CUDA vs non-CUDA** on the same OS run the identical command; only the
  llama.cpp build that `install-core` resolves differs (CUDA build for NVIDIA,
  Vulkan build otherwise). Verify which engine bound with `lmforge status`.
- **Headless Linux** (no `DISPLAY`/`WAYLAND_DISPLAY`): the UI-launch check
  auto-skips; add `--no-ui` to skip building the UI entirely.
- A failed local `cargo`/UI build now **aborts** the run — it will no longer
  silently download a release binary and report a misleading `install-core PASS`.
- An **engine preflight** runs the active engine binary (`omlx --version` /
  `llama-server --version`) before inference, so a broken engine install fails
  fast with a fix hint instead of an opaque empty `TC-E01` error. Cold-load
  failures now print the actual HTTP status + body (e.g. a 503 spawn error).
5. **Commit the bump, push main, wait for green.** Both `CI` and `E2E`
   workflows must pass **on the exact commit you will tag**. No "it was green
   two commits ago".
6. Working tree clean: `git status` shows nothing, `git log origin/main..main`
   shows nothing.

## 2. Cut the release

```powershell
git tag vX.Y.Z
git push origin vX.Y.Z
```

`release.yml` then runs automatically:

1. Builds core binaries (macos-arm64, linux-x86_64, linux-arm64, windows-x86_64)
   and UI bundles (DMG, AppImage, NSIS exe).
2. **e2e-gate**: runs the full install lifecycle on ubuntu/macos/windows using
   the artifacts that will ship. If any platform fails, **no release is
   created** — fix, delete the tag (`git push origin :refs/tags/vX.Y.Z`,
   `git tag -d vX.Y.Z`), and retag once main is green again. Never reuse a
   tag that was ever published.
3. Creates a **draft** release with all binaries + the 8 install/uninstall
   scripts attached.

## 3. Verify the draft

On the draft release page, check:

- [ ] All assets present: 4 core binaries, 3 UI bundles, 8 scripts
  (`install-core.{sh,ps1}`, `install-ui.{sh,ps1}`, `uninstall-core.{sh,ps1}`,
  `uninstall-ui.{sh,ps1}`).
- [ ] Asset scripts match the tagged commit (they are taken from the tag's
  checkout — a mismatch means the tag was moved; abort).
- [ ] Auto-generated release notes look sane; trim noise.

## 4. Publish — staged

Draft assets are not publicly downloadable, so the public-URL smoke test
happens via a pre-release first:

1. **Publish as pre-release.** `latest/download/...` install URLs still point
   at the previous stable, so users are unaffected.
2. **Smoke-test the published assets** on a real machine (not CI), **per
   platform** — same engine/suite auto-detection as the pre-release matrix
   above, so the only thing that changes between platforms is the launcher
   (`.sh` vs `.ps1`). Two passes: asset verification, then a full
   install→inference run.

   ```bash
   # macOS / Linux (Apple Silicon, Linux CUDA, Linux non-CUDA)
   ./scripts/lmforge.sh e2e --source release:vX.Y.Z --verify-assets --no-inference
   ./scripts/lmforge.sh e2e --source release:vX.Y.Z --keep-install
   ```

   ```powershell
   # Windows (CUDA and non-CUDA)
   powershell -File scripts\lmforge.ps1 e2e -Source release:vX.Y.Z -VerifyAssets -NoInference
   powershell -File scripts\lmforge.ps1 e2e -Source release:vX.Y.Z -KeepInstall
   ```

   MTP auto-skips on Apple Silicon; `--verify-assets` HEAD-checks only the core
   binary + scripts (and the UI asset where one ships for the platform).

3. **Promote**: edit the release, untick "pre-release" → it becomes `latest`
   and the `irm .../latest/download/install-core.ps1 | iex` path goes live.

## 5. Post-release

- [ ] Fresh-install path via public latest URL on at least one machine:
  `irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex`
  (or the `curl | bash` equivalent), then `lmforge service status` and the UI.
- [ ] Upgrade path from the previous release on a machine that still has it
  installed (Windows: verify the legacy "LMForge Daemon" scheduled task is
  gone and the `HKCU\...\Run` `LMForge` value exists after
  `lmforge service install`).
- [ ] Watch the first user-facing issue channels / logs for a day.
- [ ] If broken: mark the release as pre-release again (pulls it out of
  `latest`), fix forward with a new patch tag. Do not delete published
  releases users may have installed from.

## Abort paths

| Situation | Action |
|---|---|
| e2e-gate red on tag | delete tag, fix on main, wait green, retag |
| Draft looks wrong | delete draft + tag, retag |
| Published but broken | re-mark as pre-release, fix forward with vX.Y.Z+1 |
