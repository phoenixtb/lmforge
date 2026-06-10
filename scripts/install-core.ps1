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
#   LMFORGE_LOCAL_BIN   Path to a locally built lmforge.exe — skips the GitHub
#                       download. Used by the E2E harness/CI; not for end users.
# =============================================================================
$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Err     { param($m) Write-Host "  [x] $m" -ForegroundColor Red; exit 1 }

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
$env:PATH = "$InstallDir;$env:PATH"
$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
if ($LmforgeCmd -or (Test-Path "$InstallDir\$Binary")) {
    $CoreBin = if ($LmforgeCmd) { $LmforgeCmd.Source } else { "$InstallDir\$Binary" }
    $CoreVerRaw = & $CoreBin --version 2>$null
    $CoreVerMatch = [regex]::Match($CoreVerRaw, '(\d+\.\d+\.\d+)')
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
    Warn "To reinstall:"
    Warn "  irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex"
    exit 0
}

# --- Resolve download URL ---
if ($env:LMFORGE_LOCAL_BIN) {
    if (-not (Test-Path $env:LMFORGE_LOCAL_BIN)) {
        Err "LMFORGE_LOCAL_BIN set but not found: $env:LMFORGE_LOCAL_BIN"
    }
    Info "Using local binary: $env:LMFORGE_LOCAL_BIN"
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item $env:LMFORGE_LOCAL_BIN "$InstallDir\$Binary" -Force
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

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item $TmpExe "$InstallDir\$Binary" -Force
Remove-Item $TmpExe -ErrorAction SilentlyContinue
}
Success "Binary installed to $InstallDir\$Binary"

# --- Post-install: data directories ---
Info "Creating LMForge data directories..."
$DataDir = "$env:USERPROFILE\.lmforge"
@("models", "engines", "logs") | ForEach-Object {
    New-Item -ItemType Directory -Path "$DataDir\$_" -Force | Out-Null
}

# --- PATH update ---
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

# --- Init + Service install ---
Info "Running lmforge init..."
& "$InstallDir\$Binary" init

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
