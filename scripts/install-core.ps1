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

# Returns $true when the daemon is healthy, $false otherwise - the caller
# must not report success on $false.
function Ensure-LmforgeDaemon {
    param([string]$Binary)
    if (Test-LmforgeHealth -TimeoutSec 3) {
        Info "Daemon is running at http://127.0.0.1:11430"
        return $true
    }
    Warn "Daemon not reachable yet. Starting engine..."
    & $Binary start
    if (Wait-LmforgeHealth -TimeoutSec 120) {
        Success "Daemon is running at http://127.0.0.1:11430"
        return $true
    }
    Warn "Daemon still not reachable after 120s."
    return $false
}

# Warn upfront when third-party security software is present. LMForge ships
# unsigned (free OSS project); behavior-based protection in some products
# quarantines unsigned binaries that autostart, spawn engine processes, and
# open localhost sockets - which is exactly what LMForge does by design.
# Detecting it BEFORE download turns a mid-install mystery quarantine into an
# informed choice. Vendor-neutral: reads the Windows Security Center registry.
function Show-SecuritySoftwareNotice {
    param([string]$DataRoot)
    try {
        $avs = Get-CimInstance -Namespace "root/SecurityCenter2" -ClassName AntiVirusProduct -ErrorAction Stop |
            Select-Object -ExpandProperty displayName -Unique |
            Where-Object { $_ -notmatch 'Windows Defender|Microsoft Defender' }
        if ($avs) {
            Warn "Detected security software: $($avs -join ', ')"
            Warn "LMForge is a free open-source project and its binaries are not yet"
            Warn "code-signed, so behavioral protection may quarantine them when the"
            Warn "daemon or UI starts. If that happens (or to avoid it), add an"
            Warn "exclusion in your security software for this folder:"
            Warn "  $DataRoot"
            Write-Host ""
        }
    } catch {}
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
                Warn "Old binary was locked - moved aside to $Aside"
            } catch {
                Warn "Cannot replace the existing file at $Target - it is locked,"
                Warn "most likely quarantined by your security software. To fix:"
                Warn "  1. Open your security software's quarantine / protection history"
                Warn "     and restore or allow lmforge.exe"
                Warn "  2. Add an exclusion for the folder: $Dir"
                Warn "  3. Re-run this installer."
                Err "Existing binary is locked: $Target"
            }
        }
    }

    try {
        Copy-Item $Source $Target -Force -ErrorAction Stop
    } catch {
        Warn "Could not write $Target ($($_.Exception.Message))."
        Warn "Your security software is blocking writes to this folder."
        Warn "Add an exclusion for $Dir and re-run this installer."
        Err "Binary install blocked: $Target"
    }
}

# Pure-ASCII banner: box-drawing/block glyphs (as used by the .sh installer)
# would garble under `irm | iex` on legacy codepages and violate the ASCII-only
# rule for published ps1 scripts.
Write-Host ""
Write-Host '   _     __  __ _____                     ' -ForegroundColor Cyan
Write-Host '  | |   |  \/  |  ___|__  _ __ __ _  ___  ' -ForegroundColor Cyan
Write-Host '  | |   | |\/| | |_ / _ \| ''__/ _` |/ _ \ ' -ForegroundColor Cyan
Write-Host '  | |___| |  | |  _| (_) | | | (_| |  __/ ' -ForegroundColor Cyan
Write-Host '  |_____|_|  |_|_|  \___/|_|  \__, |\___| ' -ForegroundColor Cyan
Write-Host '                              |___/       ' -ForegroundColor Cyan
Write-Host ""
Write-Host "  LMForge Core - Installer" -ForegroundColor Cyan
Write-Host ""

$Repo       = "phoenixtb/lmforge"
$Binary     = "lmforge.exe"
$AssetName  = "lmforge-windows-x86_64.exe"
# Everything LMForge owns lives under ONE visible root: %USERPROFILE%\.lmforge
# (binary in bin\, models, engines, logs, config). One folder to find, one
# folder to exclude in security software, one folder to delete.
# Installs <= v0.1.6 used %LOCALAPPDATA%\lmforge\bin (hidden under AppData);
# those are migrated below.
$InstallDir       = "$env:USERPROFILE\.lmforge\bin"
$LegacyInstallDir = "$env:LOCALAPPDATA\lmforge\bin"
$Version    = if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { "latest" }

Write-Host "  Repo   : https://github.com/$Repo"
Write-Host "  Version: $Version"
Write-Host "  Install: $InstallDir\$Binary"
Write-Host ""

Show-SecuritySoftwareNotice "$env:USERPROFILE\.lmforge"

# --- Legacy location migration ---
# A binary at the old hidden AppData path forces a fresh install into the new
# location; the old copy and its PATH entry are removed after success.
$LegacyPresent = Test-Path "$LegacyInstallDir\$Binary"
if ($LegacyPresent) {
    Info "Found existing install at old location ($LegacyInstallDir) - migrating to $InstallDir"
    Stop-LmforgeForInstall "$LegacyInstallDir\$Binary"
}

# --- Idempotency check ---
# A local build (LMFORGE_LOCAL_BIN) or LMFORGE_UPGRADE=1 always overwrites - an
# explicit dev/upgrade action. The early-exit only guards plain release installs.
$env:PATH = "$InstallDir;$env:PATH"
$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
# A command resolving to the legacy dir does not count as installed - it is
# being migrated.
if ($LmforgeCmd -and $LmforgeCmd.Source -like "$LegacyInstallDir*") { $LmforgeCmd = $null }
$AlreadyInstalled = (-not $LegacyPresent) -and ($LmforgeCmd -or (Test-Path "$InstallDir\$Binary"))
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
        Warn "It was probably quarantined by your security software - reinstalling over it."
        Warn "If this repeats, restore/allow lmforge.exe in your security software's"
        Warn "quarantine and add an exclusion for $InstallDir"
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
        $null = Ensure-LmforgeDaemon $CoreBin
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

# --- Legacy install cleanup (after the new binary is in place) ---
if ($LegacyPresent) {
    try {
        Remove-Item "$LegacyInstallDir\$Binary" -Force -ErrorAction Stop
        # Remove the whole %LOCALAPPDATA%\lmforge tree if nothing else lives there.
        $left = Get-ChildItem "$env:LOCALAPPDATA\lmforge" -Recurse -File -ErrorAction SilentlyContinue
        if (-not $left) { Remove-Item "$env:LOCALAPPDATA\lmforge" -Recurse -Force -ErrorAction SilentlyContinue }
        Info "Removed old binary at $LegacyInstallDir"
    } catch {
        Warn "Could not remove old binary at $LegacyInstallDir\$Binary - delete it manually."
    }
}

# --- PATH update ---
# Non-fatal: security software can deny registry env writes from a piped
# script. The session PATH (set above) still works for the rest of this run.
try {
    $UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    $Entries  = @($UserPath -split ';' | Where-Object { $_ })
    $Changed  = $false
    if ($Entries -contains $LegacyInstallDir) {
        $Entries = @($Entries | Where-Object { $_ -ne $LegacyInstallDir })
        $Changed = $true
    }
    if ($Entries -notcontains $InstallDir) {
        $Entries += $InstallDir
        $Changed = $true
    }
    if ($Changed) {
        [System.Environment]::SetEnvironmentVariable("PATH", ($Entries -join ';'), "User")
        $env:PATH += ";$InstallDir"
        Success "Added $InstallDir to your user PATH (restart terminal to take effect)"
    }
} catch {
    Warn "Could not persist $InstallDir to user PATH ($($_.Exception.Message))."
    Warn "Add it manually: Settings > System > About > Advanced system settings > Environment Variables"
}

# --- Init + Service install ---
# Each step's outcome is tracked so the final summary tells the truth. `init`
# downloads the inference engine from GitHub; a failure here means the daemon
# cannot serve models, so it must not be reported as a working install.
# init runs DIRECTLY on the console (no output capture): piping would garble
# the binary's UTF-8 output into the legacy OEM codepage (mojibake) and hide
# its download progress bars. The binary itself prints a plain-language cause
# for network failures, so only the exit code is needed here.
Info "Running lmforge init..."
if ($env:LMFORGE_DATA_DIR) {
    & "$InstallDir\$Binary" init --data-dir $env:LMFORGE_DATA_DIR
} else {
    & "$InstallDir\$Binary" init
}
$InitOk = ($LASTEXITCODE -eq 0)
if (-not $InitOk) {
    Write-Host ""
    Warn "Engine setup failed - the message above explains the cause."
    Warn "After fixing it, run: lmforge init"
}

Info "Registering auto-start at logon (HKCU Run key)..."
$ServiceRegistered = $true
& "$InstallDir\$Binary" service install
if ($LASTEXITCODE -ne 0) {
    $ServiceRegistered = $false
    Warn "Service registration failed (exit $LASTEXITCODE). Retry later: lmforge service install"
}

# Without a working engine the daemon cannot become healthy - skip the 2 min
# wait and keep the init failure as the headline problem.
$DaemonUp = $false
if ($InitOk) {
    $DaemonUp = Ensure-LmforgeDaemon "$InstallDir\$Binary"
} else {
    Warn "Skipping daemon startup check (init failed)."
}

Write-Host ""
if ($InitOk -and $DaemonUp) {
    Success "LMForge $Version installed successfully!"
    Write-Host ""
    if ($ServiceRegistered) {
        Write-Host "  The daemon is running and starts automatically at logon." -ForegroundColor White
    } else {
        Write-Host "  The daemon is running now. Auto-start at logon was NOT registered." -ForegroundColor Yellow
        Write-Host "  Retry: lmforge service install" -ForegroundColor Yellow
    }
    Write-Host "  API:  http://127.0.0.1:11430" -ForegroundColor White
} else {
    Warn "LMForge $Version installed WITH PROBLEMS - it is not usable yet:"
    Write-Host ""
    Write-Host "    Binary            : OK ($InstallDir\$Binary)" -ForegroundColor White
    if ($InitOk) {
        Write-Host "    Engine (init)     : OK" -ForegroundColor White
    } else {
        Write-Host "    Engine (init)     : FAILED - see [!] notes above" -ForegroundColor Red
    }
    if ($ServiceRegistered) {
        Write-Host "    Autostart         : OK" -ForegroundColor White
    } else {
        Write-Host "    Autostart         : FAILED - retry: lmforge service install" -ForegroundColor Red
    }
    if ($DaemonUp) {
        Write-Host "    Daemon            : running at http://127.0.0.1:11430" -ForegroundColor White
    } elseif ($InitOk) {
        Write-Host "    Daemon            : NOT reachable - log: $env:USERPROFILE\.lmforge\logs\daemon.out.log" -ForegroundColor Red
        Write-Host "                        debug: lmforge start --foreground" -ForegroundColor Red
    } else {
        Write-Host "    Daemon            : not started (needs engine)" -ForegroundColor Red
    }
    Write-Host ""
    Write-Host "  After fixing the issue(s), run:" -ForegroundColor White
    Write-Host "    lmforge init                 # finish engine setup"
    Write-Host "    lmforge start                # start the daemon"
}
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
