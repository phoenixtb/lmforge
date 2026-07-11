# =============================================================================
# LMForge Core - Windows PowerShell Installer
# Downloads the pre-built binary from GitHub Releases, installs it to the
# current user's local bin directory, adds it to PATH, runs init, and
# registers the system service.
#
# Usage (run in PowerShell as your user):
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
#
# Environment variables:
#   LMFORGE_VERSION     Pin a specific version, e.g. "v0.3.1" (default: latest)
#   LMFORGE_DATA_DIR    Install LMForge's data root (engines, logs, model index)
#                       at a custom path instead of %USERPROFILE%\.lmforge. This
#                       is pinned into config at install time; the data dir is
#                       NOT relocatable later from the UI (only the models dir is).
#   LMFORGE_LOCAL_BIN   Path to a locally built lmforge.exe - skips the GitHub
#                       download. Used by the E2E harness/CI; not for end users.
# =============================================================================
$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
# throw, never `exit`: under `irm | iex` there is no script scope, so `exit`
# kills the user's whole terminal session. throw stops the script either way
# and still yields exit code 1 when run via `powershell -File`.
function Err     { param($m) Write-Host "  [x] $m" -ForegroundColor Red; throw "install-core failed: $m" }

function Test-LmforgeHealth {
    param([int]$TimeoutSec = 3)
    try {
        Invoke-WebRequest -Uri "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec $TimeoutSec | Out-Null
        return $true
    } catch {
        return $false
    }
}

function Wait-LmforgeHealth {
    param([int]$TimeoutSec = 120)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (Test-LmforgeHealth -TimeoutSec 2) { return $true }
        Start-Sleep -Seconds 2
    }
    return $false
}

function Ensure-LmforgeDaemon {
    param([string]$Binary)
    if (Test-LmforgeHealth -TimeoutSec 3) {
        Info "Daemon is running at http://127.0.0.1:11430"
        return
    }
    Warn "Daemon not reachable yet. Starting engine..."
    & $Binary start
    if (Wait-LmforgeHealth -TimeoutSec 120) {
        Success "Daemon is running at http://127.0.0.1:11430"
    } else {
        Warn "Daemon still not reachable after 120s."
        Warn "Check: lmforge service status"
        Warn "Log:  $env:USERPROFILE\.lmforge\logs\daemon.out.log"
        Warn "Debug: lmforge start --foreground"
    }
}

# Stop the daemon/service so the (locked) .exe can be overwritten on reinstall.
function Stop-LmforgeForInstall {
    param([string]$Binary)
    try { if (Test-Path $Binary) { & $Binary service stop 2>$null; & $Binary stop 2>$null } } catch {}
    try {
        Invoke-WebRequest -Uri "http://127.0.0.1:11430/lf/shutdown" -Method Post -TimeoutSec 3 -UseBasicParsing | Out-Null
    } catch {}
    Start-Sleep -Seconds 1
    # Only the daemon process ("lmforge"), not the UI ("lmforge-ui").
    Get-Process -Name "lmforge" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
}

# Place $Source at $Target, surviving a locked or AV-blocked existing file.
# A plain `Copy-Item -Force` onto a quarantined/blocked exe throws
# UnauthorizedAccessException even when writing a NEW file to the same folder
# would succeed. So: stop processes, try delete, fall back to rename-aside
# (rename usually works when overwrite/delete is denied), then copy fresh.
function Install-LmforgeBinary {
    param([string]$Source, [string]$Target)
    $Dir = Split-Path $Target -Parent
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null

    # Sweep rename-aside leftovers from previous runs (best-effort).
    Get-ChildItem "$Dir\*.exe.old*" -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue

    if (Test-Path $Target) {
        Get-Process -Name "lmforge" -ErrorAction SilentlyContinue |
            Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
        try {
            Remove-Item $Target -Force -ErrorAction Stop
        } catch {
            $Aside = "$Target.old.$PID"
            try {
                Move-Item $Target $Aside -Force -ErrorAction Stop
                Warn "Old binary was locked (likely AV) - moved aside to $Aside"
            } catch {
                Warn "Cannot delete or rename the existing binary at $Target"
                Warn "Your antivirus has locked it. To fix:"
                Warn "  1. Open your AV (Bitdefender: Protection > Antivirus > Settings > Manage Exceptions;"
                Warn "     Windows Security: Virus & threat protection > Protection history > Restore/Allow)"
                Warn "  2. Restore or allow lmforge.exe, and add an exclusion for the folder:"
                Warn "     $Dir"
                Warn "  3. Re-run this installer."
                Err "Existing binary is locked by antivirus: $Target"
            }
        }
    }

    try {
        Copy-Item $Source $Target -Force -ErrorAction Stop
    } catch {
        Warn "Could not write $Target ($($_.Exception.Message))."
        Warn "Your antivirus is blocking writes to this folder. Add an exclusion for:"
        Warn "  $Dir"
        Warn "then re-run this installer."
        Err "Antivirus blocked binary install"
    }
}

Write-Host ""
Write-Host "  LMForge Core - Installer" -ForegroundColor Cyan
Write-Host ""

$Repo       = "phoenixtb/lmforge"
$Binary     = "lmforge.exe"
$AssetName  = "lmforge-windows-x86_64.exe"
$InstallDir = "$env:LOCALAPPDATA\lmforge\bin"
$Version    = if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { "latest" }

Write-Host "  Repo   : https://github.com/$Repo"
Write-Host "  Version: $Version"
Write-Host "  Install: $InstallDir\$Binary"
Write-Host ""

# --- Idempotency check ---
# A local build (LMFORGE_LOCAL_BIN) or LMFORGE_UPGRADE=1 always overwrites - an
# explicit dev/upgrade action. The early-exit only guards plain release installs.
$env:PATH = "$InstallDir;$env:PATH"
$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
$AlreadyInstalled = ($LmforgeCmd -or (Test-Path "$InstallDir\$Binary"))
$IsLocal   = [bool]$env:LMFORGE_LOCAL_BIN
$IsUpgrade = ($env:LMFORGE_UPGRADE -eq "1")
if ($AlreadyInstalled -and -not $IsLocal -and -not $IsUpgrade) {
    $CoreBin = if ($LmforgeCmd) { $LmforgeCmd.Source } else { "$InstallDir\$Binary" }
    # The probe must not abort the install: if the binary exists but cannot run
    # (AV quarantine block -> "Access is denied"), treat it as a broken install
    # and fall through to a fresh download over it instead of dying here.
    $CoreVerRaw = $null
    try { $CoreVerRaw = & $CoreBin --version 2>$null } catch {
        Warn "Existing binary at $CoreBin cannot run ($($_.Exception.Message))."
        Warn "Likely antivirus quarantine - reinstalling over it."
        Warn "If this repeats: Windows Security > Protection history > restore/allow lmforge.exe,"
        Warn "then consider an exclusion for $InstallDir"
        $AlreadyInstalled = $false
    }
}
if ($AlreadyInstalled -and -not $IsLocal -and -not $IsUpgrade) {
    $CoreVerMatch = [regex]::Match("$CoreVerRaw", '(\d+\.\d+\.\d+)')
    $CoreVer = if ($CoreVerMatch.Success) { $CoreVerMatch.Groups[1].Value } else { "unknown" }
    Warn "lmforge $CoreVer is already installed at $CoreBin"
    if (-not (Test-LmforgeHealth -TimeoutSec 3)) {
        Warn "Daemon is not running - repairing service and starting engine..."
        & $CoreBin service install
        Ensure-LmforgeDaemon $CoreBin
    } else {
        Info "Daemon is running at http://127.0.0.1:11430"
    }
    Warn "Use 'lmforge service status' to check the daemon."
    Warn "To upgrade in place: `$env:LMFORGE_UPGRADE = '1'; irm https://github.com/$Repo/releases/latest/download/install-core.ps1 | iex"
    Warn "To reinstall:"
    Warn "  irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex"
    # `return`, not `exit`: safe under both `irm | iex` and `powershell -File`.
    return
}
if ($AlreadyInstalled -and ($IsLocal -or $IsUpgrade)) {
    $CoreBin = if ($LmforgeCmd) { $LmforgeCmd.Source } else { "$InstallDir\$Binary" }
    if ($IsLocal) { Info "Local build - reinstalling over existing install..." }
    else          { Info "Upgrading existing install..." }
    Stop-LmforgeForInstall $CoreBin
}

# --- Resolve download URL ---
if ($env:LMFORGE_LOCAL_BIN) {
    if (-not (Test-Path $env:LMFORGE_LOCAL_BIN)) {
        Err "LMFORGE_LOCAL_BIN set but not found: $env:LMFORGE_LOCAL_BIN"
    }
    Info "Using local binary: $env:LMFORGE_LOCAL_BIN"
    Install-LmforgeBinary $env:LMFORGE_LOCAL_BIN "$InstallDir\$Binary"
    $Version = "local"
} else {
if ($Version -eq "latest") {
    Info "Fetching latest release..."
    try {
        $ApiUrl  = "https://api.github.com/repos/$Repo/releases/latest"
        $Headers = @{ "User-Agent" = "lmforge-installer" }
        $Release = Invoke-RestMethod -Uri $ApiUrl -Headers $Headers
        $Version = $Release.tag_name
        Info "Latest release: $Version"
    } catch {
        Err "Could not fetch latest release from GitHub."
    }
}

$DownloadUrl = "https://github.com/$Repo/releases/download/$Version/$AssetName"
$TmpExe      = "$env:TEMP\lmforge-download.exe"

Info "Downloading $AssetName..."
try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpExe -UseBasicParsing
} catch {
    Err "Download failed from $DownloadUrl`n  Check https://github.com/$Repo/releases for available versions."
}

Install-LmforgeBinary $TmpExe "$InstallDir\$Binary"
Remove-Item $TmpExe -ErrorAction SilentlyContinue
}
Success "Binary installed to $InstallDir\$Binary"

# --- Post-install: data directories ---
# Honor a custom data root (LMFORGE_DATA_DIR). When set, it is passed to
# `lmforge init --data-dir` below, which pins it into config.toml so every later
# `lmforge start` (manual, autostart) resolves the same directory.
$DataDir = if ($env:LMFORGE_DATA_DIR) { $env:LMFORGE_DATA_DIR } else { "$env:USERPROFILE\.lmforge" }
Info "Creating LMForge data directories at $DataDir ..."
@("models", "engines", "logs") | ForEach-Object {
    New-Item -ItemType Directory -Path "$DataDir\$_" -Force | Out-Null
}

# --- PATH update ---
# Non-fatal: AV behavior-blocking can deny registry env writes from a piped
# script. The session PATH (set above) still works for the rest of this run.
try {
    $UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [System.Environment]::SetEnvironmentVariable(
            "PATH",
            "$UserPath;$InstallDir",
            "User"
        )
        $env:PATH += ";$InstallDir"
        Success "Added $InstallDir to your user PATH (restart terminal to take effect)"
    }
} catch {
    Warn "Could not persist $InstallDir to user PATH ($($_.Exception.Message))."
    Warn "Add it manually: Settings > System > About > Advanced system settings > Environment Variables"
}

# --- Init + Service install ---
Info "Running lmforge init..."
if ($env:LMFORGE_DATA_DIR) {
    & "$InstallDir\$Binary" init --data-dir $env:LMFORGE_DATA_DIR
} else {
    & "$InstallDir\$Binary" init
}

Info "Registering auto-start at logon (HKCU Run key)..."
$ServiceRegistered = $true
& "$InstallDir\$Binary" service install
if ($LASTEXITCODE -ne 0) {
    $ServiceRegistered = $false
    Warn "Service registration failed (exit $LASTEXITCODE). Daemon will still start now; retry: lmforge service install"
}

Ensure-LmforgeDaemon "$InstallDir\$Binary"

Write-Host ""
Success "LMForge $Version installed successfully!"
Write-Host ""
if ($ServiceRegistered) {
    Write-Host "  The daemon is running and starts automatically at logon." -ForegroundColor White
} else {
    Write-Host "  The daemon is running now. Auto-start at logon was NOT registered." -ForegroundColor Yellow
    Write-Host "  Retry: lmforge service install" -ForegroundColor Yellow
}
Write-Host "  API:  http://127.0.0.1:11430" -ForegroundColor White
Write-Host ""
Write-Host "  Next steps:" -ForegroundColor White
Write-Host "    lmforge pull qwen3-8b        # download your first model"
Write-Host "    lmforge run qwen3-8b         # interactive chat"
Write-Host "    lmforge status               # show engine + model status"
Write-Host "    lmforge service status       # show service health"
Write-Host ""
Write-Host "  Install the desktop UI:" -ForegroundColor White
Write-Host "    irm https://github.com/$Repo/releases/latest/download/install-ui.ps1 | iex"
Write-Host ""
Write-Host "  Uninstall:" -ForegroundColor White
Write-Host "    UI only:  irm https://github.com/$Repo/releases/latest/download/uninstall-ui.ps1 | iex"
Write-Host "    Core:     irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex"
Write-Host "    Purge:    `$env:LMFORGE_PURGE = '1'; irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex"
Write-Host ""
