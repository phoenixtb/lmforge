# Release Process

Applies to core + UI releases cut from `main`. The hard rule: **a tag is only
pushed from a clean, pushed, CI-green commit, and a release is only published
after the E2E gate passed on all three platforms.**

Workflows involved:

| Workflow | Trigger | Purpose |
|---|---|---|
| `ci.yml` | push / PR to main | fmt, clippy (`-D warnings`), `cargo test` on ubuntu/macos/windows |
| `e2e.yml` | push / PR to main | install → health → sysinfo → service → uninstall lifecycle, all 3 OSes, local build |
| `release.yml` | tag push `v*` | build artifacts → **e2e-gate against the exact artifacts** → create **draft** release |

---

## 1. Pre-release

1. **Freeze scope.** No unrelated changes after this point; only release fixes.
2. **Version bump** (all must match):
   - `Cargo.toml` → `[package] version`
   - `ui/package.json` → `version` (then run `npm install` in `ui/` to sync `package-lock.json`)
   - `ui/src-tauri/Cargo.toml` → `version`
   - `ui/src-tauri/tauri.conf.json` → `version`
   - `cargo build` once to refresh `Cargo.lock`
3. **Local checks** (Windows dev box; Linux-cfg lints are caught by CI only):

   ```powershell
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets
   ```

4. **Local E2E** against the release build:

   ```powershell
   cargo build --release --bin lmforge
   $env:LMFORGE_LOCAL_BIN = "target\release\lmforge.exe"
   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\util\e2e-core.ps1
   ```

   (macOS/Linux box, if available: `LMFORGE_LOCAL_BIN=target/release/lmforge ./scripts/util/e2e-core.sh`)
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
2. **Smoke-test the published assets** on a real machine (not CI):

   ```powershell
   # Windows — full release-asset + install + UI flow
   powershell -File scripts\util\test-release-windows.ps1 -Version vX.Y.Z
   ```

   ```bash
   # macOS / Linux
   LMFORGE_VERSION=vX.Y.Z ./scripts/util/e2e-core.sh
   ```

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
