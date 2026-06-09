# =============================================================================
# LMForge UI - Windows PowerShell Uninstaller
# Removes the desktop app only. Daemon, service, and models are NOT affected.
#
# Usage:
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-ui.ps1 | iex
#
# Skip confirmation:
#   $env:LMFORGE_YES = "1"; irm .../uninstall-ui.ps1 | iex
# =============================================================================
param(
    [switch]$Yes
)

if (-not $Yes -and $env:LMFORGE_YES -match '^(1|true|yes)$') {
    $Yes = $true
}

$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Section { param($m) Write-Host ""; Write-Host "  $m" -ForegroundColor White }

$Repo       = "phoenixtb/lmforge"
$InstallDir = "$env:LOCALAPPDATA\LMForge"
$AppExe     = "$InstallDir\lmforge-ui.exe"

Write-Host ""
Write-Host "  LMForge UI - Uninstaller" -ForegroundColor Cyan
Write-Host "  Removes the desktop app only."
Write-Host "  Daemon service and models are NOT affected."
Write-Host ""

if (-not $Yes) {
    $confirm = Read-Host "  Continue? [y/N]"
    if ($confirm -notmatch '^[Yy]$') {
        Write-Host "  Aborted."
        exit 0
    }
}

# --- Quit running app ---
Section "Quitting LMForge..."
Get-Process -Name "lmforge-ui" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
Info "App process stopped"

# --- Run NSIS uninstaller if registered ---
Section "Removing app..."

function Find-LMForgeUiUninstallEntry {
    $roots = @(
        "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*"
    )
    foreach ($root in $roots) {
        $entry = Get-ItemProperty $root -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -eq "LMForge" -and $_.UninstallString } |
            Select-Object -First 1
        if ($entry) { return $entry }
    }
    return $null
}

$UninstallEntry = Find-LMForgeUiUninstallEntry

$Removed = $false
if ($UninstallEntry) {
    $UninstallCmd = $UninstallEntry.UninstallString.Trim('"')
    if (Test-Path $UninstallCmd) {
        Info "Running uninstaller: $UninstallCmd"
        $Proc = Start-Process -FilePath $UninstallCmd -ArgumentList "/S" -Wait -PassThru
        if ($Proc.ExitCode -eq 0) {
            Info "Uninstaller completed"
            $Removed = $true
        } else {
            Warn "Uninstaller exited with code $($Proc.ExitCode)"
        }
    }
}

# --- Fallback: remove known install dir ---
if (Test-Path $InstallDir) {
    Remove-Item -Recurse -Force $InstallDir
    Info "Removed $InstallDir"
    $Removed = $true
} elseif (-not $Removed -and -not (Test-Path $AppExe)) {
    Warn "LMForge UI not found - may already be uninstalled"
}

# --- Remove app data (Tauri identifier: com.lmforge.app) ---
$AppDataDir = "$env:APPDATA\com.lmforge.app"
if (Test-Path $AppDataDir) {
    Remove-Item -Recurse -Force $AppDataDir
    Info "Removed $AppDataDir"
}

Write-Host ""
Success "LMForge UI uninstalled."
Write-Host ""
Write-Host "  The daemon is still running. To also remove Core:" -ForegroundColor White
Write-Host "    irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex" -ForegroundColor White
Write-Host ""
Write-Host "  To reinstall the UI:" -ForegroundColor White
Write-Host "    irm https://github.com/$Repo/releases/latest/download/install-ui.ps1 | iex" -ForegroundColor White
Write-Host ""
