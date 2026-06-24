# =============================================================================
# LMForge - Windows dev reinstall (build from this repo, install locally)
#
# Windows counterpart of dev-reinstall-core.sh + dev-clean-reinstall-ui.sh.
# One command: build core (and optionally the UI) from the local source tree
# and install both the way a real user would, pointing at the default
# %USERPROFILE%\.lmforge data dir.
#
# Run from anywhere in the repo:
#   powershell -ExecutionPolicy Bypass -File scripts\util\dev-reinstall.ps1
#   ... -SkipUi                 # core only
#   ... -SkipCore               # UI only (core must already be installed)
#   ... -WipeEngines            # re-download the llama.cpp bundle
#   ... -Purge                  # nuke ~/.lmforge first (models included)
#   ... -NoInit -NoStart        # just build + drop the binary in place
#   ... -Debug                  # cargo debug build (faster compile)
#
# Flags:
#   -SkipCore       Don't build/install core
#   -SkipUi         Don't build/install the UI
#   -Debug          cargo debug build instead of release
#   -WipeEngines    Remove ~/.lmforge/engines + bin before install
#   -Purge          Remove the whole ~/.lmforge before install (implies WipeEngines)
#   -NoInit         Skip `lmforge init` (hardware probe + engine bundle)
#   -NoStart        Don't start the daemon after install
#   -KeepNode       Skip `npm install` for the UI (reuse node_modules)
#   -DataDir PATH   Override data dir (default: %USERPROFILE%\.lmforge)
#
# Exit codes: 0 ok | 1 preflight | 2 build | 3 init/install | 4 start/health
# =============================================================================
param(
    [switch]$SkipCore,
    [switch]$SkipUi,
    [switch]$Debug,
    [switch]$WipeEngines,
    [switch]$Purge,
    [switch]$NoInit,
    [switch]$NoStart,
    [switch]$KeepNode,
    [string]$DataDir
)

$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Section { param($m) Write-Host ""; Write-Host "  $m" -ForegroundColor White }
function Die     { param($m, $code = 1) Write-Host "  [x] $m" -ForegroundColor Red; exit $code }

$RepoRoot   = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$Profile    = if ($Debug) { "debug" } else { "release" }
# A custom data dir is pinned into config at install time via `init --data-dir`
# (the data dir is fixed per machine — only the models dir is relocatable later).
$CustomDataDir = [bool]$DataDir
$DataDir    = if ($DataDir) { $DataDir } else { "$env:USERPROFILE\.lmforge" }
$InstallDir = "$env:LOCALAPPDATA\lmforge\bin"
$CoreBin    = "$InstallDir\lmforge.exe"
$UiInstall  = "$env:LOCALAPPDATA\LMForge"
$UiExe      = "$UiInstall\lmforge-ui.exe"
$Api        = "http://127.0.0.1:11430"

# Cargo writes to CARGO_TARGET_DIR when set (e.g. sandboxes), else repo target.
$TargetBase = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $RepoRoot "target" }

Write-Host ""
Write-Host "  LMForge - dev reinstall" -ForegroundColor Cyan
Write-Host "  repo:    $RepoRoot"
Write-Host "  profile: $Profile   data: $DataDir"
Write-Host ""

# --- [0] Preflight -----------------------------------------------------------
Section "[0] Preflight"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Die "cargo not on PATH" 1 }
if (-not $SkipUi -and -not (Get-Command npm -ErrorAction SilentlyContinue)) {
    Die "npm not on PATH (needed for the UI; pass -SkipUi to skip)" 1
}
Info "cargo $(cargo --version)"

# --- [1] Stop anything running ----------------------------------------------
Section "[1] Stopping running processes"
# Always stop the UI (it gets rebuilt/reinstalled or would lock files).
Get-Process lmforge-ui -ErrorAction SilentlyContinue |
    Stop-Process -Force -ErrorAction SilentlyContinue
# Only tear the daemon down when we are rebuilding core; a UI-only run must
# leave the healthy daemon alone.
if (-not $SkipCore) {
    try {
        Invoke-WebRequest "$Api/lf/shutdown" -Method POST -UseBasicParsing -TimeoutSec 4 | Out-Null
    } catch {}
    Get-Process lmforge, llama-server -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item "$DataDir\lmforge.pid" -Force -ErrorAction SilentlyContinue
}
Start-Sleep -Seconds 1
Info "stopped"

# --- [2] Optional state wipe -------------------------------------------------
Section "[2] Preparing $DataDir"
if ($Purge -and (Test-Path $DataDir)) {
    Remove-Item -LiteralPath $DataDir -Recurse -Force -ErrorAction SilentlyContinue
    Info "purged $DataDir"
} elseif ($WipeEngines) {
    Remove-Item "$DataDir\engines" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item "$DataDir\bin"     -Recurse -Force -ErrorAction SilentlyContinue
    Info "removed engines/ and bin/"
} else {
    Info "keeping existing data (use -WipeEngines or -Purge for a clean engine install)"
}

# =============================================================================
# CORE
# =============================================================================
if (-not $SkipCore) {
    Section "[3] cargo build --bin lmforge ($Profile)"
    Push-Location $RepoRoot
    try {
        $buildArgs = @("build", "--bin", "lmforge")
        if (-not $Debug) { $buildArgs += "--release" }
        & cargo @buildArgs
        if ($LASTEXITCODE -ne 0) { Die "cargo build failed" 2 }
    } finally {
        Pop-Location
    }

    $built = Join-Path $TargetBase "$Profile\lmforge.exe"
    if (-not (Test-Path $built)) { Die "built binary not found at $built" 2 }

    Section "[4] Install binary -> $InstallDir"
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item $built $CoreBin -Force
    Info "installed $(& $CoreBin --version)"

    # Put core dir on user PATH (idempotent).
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$InstallDir;$userPath", "User")
        Info "added $InstallDir to user PATH (new shells)"
    }
    $env:PATH = "$InstallDir;$env:PATH"

    if (-not $NoInit) {
        Section "[5] lmforge init (hardware probe + engine bundle)"
        if ($CustomDataDir) { & $CoreBin init --data-dir $DataDir } else { & $CoreBin init }
        if ($LASTEXITCODE -ne 0) { Die "lmforge init failed" 3 }
        Info "init ok"
    } else {
        Info "[5] skipped init (-NoInit)"
    }

    # CRITICAL ordering: start the daemon BEFORE `service install`.
    #
    # `service install` itself starts the daemon when /health is not yet
    # reachable. That daemon is detached but inherits whatever stdout handle the
    # `service install` process had (a pipe or a redirected file), and keeps it
    # open for its whole lifetime — so ANY way of capturing service install's
    # output (`| Out-Null`, `Start-Process -Wait -RedirectStandardOutput`) blocks
    # forever waiting for that handle to close.
    #
    # Fix: bring the daemon up ourselves via a clean detached Start-Process (whose
    # handle we never wait on), confirm /health, THEN run `service install`. With
    # the daemon already reachable it skips the spawn and exits immediately, so
    # capturing its output is safe.
    function Wait-Health {
        param([int]$Seconds = 60)
        foreach ($i in 1..$Seconds) {
            try { Invoke-WebRequest "$Api/health" -UseBasicParsing -TimeoutSec 1 | Out-Null; return $true }
            catch { Start-Sleep -Seconds 1 }
        }
        return $false
    }

    if (-not $NoStart) {
        Section "[6] Start daemon"
        $startArgs = if ($CustomDataDir) { @("start", "--data-dir", $DataDir) } else { @("start") }
        Start-Process -FilePath $CoreBin -ArgumentList $startArgs -WindowStyle Hidden
        if (Wait-Health 60) { Info "daemon healthy at $Api" }
        else { Warn "daemon not healthy in 60s - check $DataDir\logs\daemon.err.log" }

        Section "[7] Register autostart (HKCU Run key)"
        # Daemon is already up, so service install skips its own spawn and exits
        # cleanly -> capturing its output via the pipe is safe (no detached child
        # holding the handle open).
        & $CoreBin service install 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { Warn "service install returned $LASTEXITCODE (retry: lmforge service install)" }
        else { Info "run key registered" }
    } else {
        # -NoStart: skip both start and autostart registration. `service install`
        # force-starts the daemon (and would hang a captured pipe via the
        # inherited handle), which contradicts -NoStart. Register later with:
        #   lmforge service install
        Info "[6-7] skipped start + autostart (-NoStart); binary is installed"
    }
}

# =============================================================================
# UI
# =============================================================================
if (-not $SkipUi) {
    $UiDir = Join-Path $RepoRoot "ui"
    if (-not (Test-Path "$UiDir\package.json")) { Die "no ui\package.json at $UiDir" 1 }

    Section "[8] UI: npm deps"
    Push-Location $UiDir
    try {
        if ($KeepNode -and (Test-Path "node_modules")) {
            Info "reusing node_modules (-KeepNode)"
        } else {
            & npm install
            if ($LASTEXITCODE -ne 0) { Die "npm install failed" 2 }
        }

        Section "[9] UI: npx tauri build"
        & npx tauri build
        if ($LASTEXITCODE -ne 0) { Die "tauri build failed" 3 }
    } finally {
        Pop-Location
    }

    # Tauri always emits release bundles regardless of the core profile above.
    $nsisDir = Join-Path $TargetBase "release\bundle\nsis"
    $setup = Get-ChildItem $nsisDir -Filter "*-setup.exe" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $setup) { Die "NSIS setup not found in $nsisDir" 3 }

    Section "[10] Install UI -> $UiInstall"
    Get-Process lmforge-ui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $UiInstall -Force | Out-Null
    # Silent per-user NSIS install (same flags install-ui.ps1 uses).
    $proc = Start-Process -FilePath $setup.FullName -ArgumentList "/S", "/D=$UiInstall" -Wait -PassThru
    if ($proc.ExitCode -ne 0) { Die "UI installer exited $($proc.ExitCode)" 3 }
    if (-not (Test-Path $UiExe)) { Die "UI install finished but $UiExe missing" 3 }
    Info "installed $UiExe"

    Start-Process $UiExe
    Info "UI launched"
}

Write-Host ""
Success "dev reinstall complete."
if (-not $SkipCore) { Write-Host "  core: $CoreBin" -ForegroundColor White }
if (-not $SkipUi)   { Write-Host "  ui:   $UiExe" -ForegroundColor White }
Write-Host "  api:  $Api" -ForegroundColor White
Write-Host ""
