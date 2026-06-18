# =============================================================================
#  LMForge - build the desktop UI from this checkout and install it locally (Windows)
#
#  Runs `npm run tauri build` in ui\, then installs the produced NSIS installer
#  via install-ui.ps1 using LMFORGE_UI_LOCAL - the same install path a real user
#  gets, but from current source instead of a release.
#
#  Usage:
#    powershell -File scripts\util\build-ui-local.ps1
#    powershell -File scripts\util\build-ui-local.ps1 -NoDeps   # skip npm ci
#
#  Requires: node/npm, the Rust toolchain, WebView2. Core must be installed + running.
# =============================================================================
param([switch]$NoDeps)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$UiDir    = Join-Path $RepoRoot "ui"

if (-not (Get-Command npm   -EA SilentlyContinue)) { Write-Host "npm not on PATH"   -ForegroundColor Red; exit 1 }
if (-not (Get-Command cargo -EA SilentlyContinue)) { Write-Host "cargo not on PATH" -ForegroundColor Red; exit 1 }

Push-Location $UiDir
try {
    if (-not $NoDeps -or -not (Test-Path "node_modules")) {
        Write-Host "==> npm ci"
        npm ci
        if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
    }
    Write-Host "==> npm run tauri build"
    npm run tauri build
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
} finally { Pop-Location }

# Cargo workspace places bundles under the workspace-root target/; older/standalone
# layouts use ui/src-tauri/target/. Check both, newest NSIS *-setup.exe wins.
$nsisDirs = @(
    (Join-Path $RepoRoot "target\release\bundle\nsis"),
    (Join-Path $UiDir   "src-tauri\target\release\bundle\nsis")
)
$art = $null
foreach ($d in $nsisDirs) {
    $art = Get-ChildItem (Join-Path $d "*-setup.exe") -EA SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($art) { break }
}
if (-not $art) { Write-Host "no UI installer (*-setup.exe) found under: $($nsisDirs -join '; ')" -ForegroundColor Red; exit 1 }
Write-Host "==> built artifact: $($art.FullName)"

Write-Host "==> install-ui.ps1 (local artifact)"
$env:LMFORGE_UI_LOCAL = $art.FullName
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $RepoRoot "scripts\install-ui.ps1")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
