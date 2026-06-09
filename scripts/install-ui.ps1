# =============================================================================
# LMForge UI - Windows PowerShell Installer
# Downloads the NSIS installer from GitHub Releases and installs the desktop app.
# Requires LMForge Core to be installed first.
#
# Usage (run in PowerShell as your user):
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.ps1 | iex
#
# Environment variables:
#   LMFORGE_VERSION   Pin a specific version, e.g. "v0.3.1" (default: latest)
# =============================================================================
$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Err     { param($m) Write-Host "  [x] $m" -ForegroundColor Red; exit 1 }
function Section { param($m) Write-Host ""; Write-Host "  $m" -ForegroundColor White }

$Repo            = "phoenixtb/lmforge"
$Version         = if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { "latest" }
$MinCoreVersion  = [version]"0.1.0"
$AssetName       = "LMForge-UI-windows-x86_64.exe"
$InstallDir      = "$env:LOCALAPPDATA\LMForge"
$AppExe          = "$InstallDir\lmforge-ui.exe"
$CoreInstallDir  = "$env:LOCALAPPDATA\lmforge\bin"
$CoreExe         = "$CoreInstallDir\lmforge.exe"

Write-Host ""
Write-Host "  LMForge UI - Installer" -ForegroundColor Cyan
Write-Host "  Repo   : https://github.com/$Repo"
Write-Host "  Version: $Version"
Write-Host ""

# --- Idempotency: already installed ---
if (Test-Path $AppExe) {
    Warn "LMForge UI already installed at $AppExe"
    Warn "To update, uninstall first:"
    Warn "  irm https://github.com/$Repo/releases/latest/download/uninstall-ui.ps1 | iex"
    Info "Launching existing app..."
    Start-Process $AppExe
    exit 0
}

# --- Prerequisite: Core must be installed ---
Section "Checking LMForge Core..."

# Augment PATH with every location install.ps1 / manual installs might use.
$env:PATH = "$CoreInstallDir;$env:PATH"

$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
if (-not $LmforgeCmd -and -not (Test-Path $CoreExe)) {
    Err @"
LMForge Core not found. Install it first:
  irm https://github.com/$Repo/releases/latest/download/install-core.ps1 | iex
"@
}

$CoreBin = if ($LmforgeCmd) { $LmforgeCmd.Source } else { $CoreExe }
$CoreVerRaw = & $CoreBin --version 2>$null
$CoreVerMatch = [regex]::Match($CoreVerRaw, '(\d+\.\d+\.\d+)')
$CoreVer = if ($CoreVerMatch.Success) { [version]$CoreVerMatch.Groups[1].Value } else { [version]"0.0.0" }
Info "Core version: $CoreVer"

if ($CoreVer -lt $MinCoreVersion) {
    Err @"
Core $CoreVer is too old. UI requires >= $MinCoreVersion
Update: irm https://github.com/$Repo/releases/latest/download/install-core.ps1 | iex
"@
}
Info "Core $CoreVer >= $MinCoreVersion (compatible)"

# Check daemon is running (should be - installed by install-core.ps1 / install.ps1)
try {
    Invoke-WebRequest -Uri "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 3 | Out-Null
    Info "Daemon is running"
} catch {
    Warn "Daemon not currently running. Starting it now..."
    $Started = $false
    try {
        & $CoreBin service start 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { $Started = $true }
    } catch {}
    if (-not $Started) {
        Start-Process -FilePath $CoreBin -ArgumentList "start" -WindowStyle Hidden
    }
    Start-Sleep -Seconds 3
}

# --- WebView2 runtime (required on Windows 10) ---
Section "Checking WebView2 runtime..."

$WebView2Key = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
$HasWebView2 = (Test-Path $WebView2Key) -or
    (Test-Path "$env:ProgramFiles\Microsoft\EdgeWebView\Application") -or
    (Test-Path "${env:ProgramFiles(x86)}\Microsoft\EdgeWebView\Application")

if ($HasWebView2) {
    Info "WebView2 runtime present"
} else {
    $OsBuild = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
    if ($OsBuild -ge 22000) {
        Warn "WebView2 not detected, but Windows 11 usually includes it. Continuing..."
    } else {
        Warn "WebView2 not detected. Windows 10 requires the Edge WebView2 Runtime."
        Warn "Download: https://developer.microsoft.com/microsoft-edge/webview2/"
        Warn "The UI installer may download WebView2 automatically if internet is available."
    }
}

# --- Download ---
Section "Downloading LMForge UI..."

if ($Version -eq "latest") {
    $DownloadUrl = "https://github.com/$Repo/releases/latest/download/$AssetName"
} else {
    $DownloadUrl = "https://github.com/$Repo/releases/download/$Version/$AssetName"
}

Info "Asset: $AssetName"
Info "URL:   $DownloadUrl"

$TmpInstaller = Join-Path $env:TEMP "lmforge-ui-installer.exe"
try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TmpInstaller -UseBasicParsing
} catch {
    Err "Download failed from $DownloadUrl`n  Check https://github.com/$Repo/releases for available versions."
}
Info "Downloaded $AssetName"

# --- Install (silent NSIS - per-user, no admin) ---
Section "Installing LMForge UI..."

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$InstallArgs = "/S", "/D=$InstallDir"
$Proc = Start-Process -FilePath $TmpInstaller -ArgumentList $InstallArgs -Wait -PassThru
Remove-Item $TmpInstaller -ErrorAction SilentlyContinue

if ($Proc.ExitCode -ne 0) {
    Err "Installer exited with code $($Proc.ExitCode). Try running the installer manually from GitHub Releases."
}

if (-not (Test-Path $AppExe)) {
    Err "Install finished but $AppExe was not found. Check GitHub Releases or run the .exe manually."
}
Info "Installed: $AppExe"

# --- Launch ---
Section "Launching LMForge..."
Start-Process $AppExe
Info "LMForge opened"

Write-Host ""
Success "LMForge UI installed successfully!"
Write-Host ""
Write-Host "  App:     $AppExe" -ForegroundColor White
Write-Host "  Start:   Start-Process `"$AppExe`"" -ForegroundColor White
Write-Host ""
Write-Host "  The UI connects to the daemon at http://127.0.0.1:11430" -ForegroundColor White
Write-Host "  Closing the UI window does NOT stop the daemon or your models." -ForegroundColor White
Write-Host ""
Write-Host "  Uninstall UI only:" -ForegroundColor White
Write-Host "    irm https://github.com/$Repo/releases/latest/download/uninstall-ui.ps1 | iex"
Write-Host ""
