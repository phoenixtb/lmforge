# =============================================================================
# LMForge UI - Windows PowerShell Uninstaller
# Removes the desktop app only. Daemon, service, and models are NOT affected.
#
# Usage:
#   Download uninstall-ui.ps1 from the latest GitHub release, then run:
#     powershell -ExecutionPolicy Bypass -File uninstall-ui.ps1
#
# Skip confirmation: pass -Yes  (or set $env:LMFORGE_YES = "1").
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

function Stop-LmforgeUiProcesses {
    Get-Process -Name "lmforge-ui","lmforge" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($_.Path -like "*\LMForge\*" -or $_.Path -like "*\lmforge-ui*") } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process -Name "lmforge-ui" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    # Tray / child processes can outlive the main window briefly; give them a
    # moment and re-check below (Stop-Process above already covers the kill).
    $deadline = (Get-Date).AddSeconds(12)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-Process -Name "lmforge-ui" -ErrorAction SilentlyContinue)) {
            return $true
        }
        Start-Sleep -Milliseconds 400
    }
    return -not (Get-Process -Name "lmforge-ui" -ErrorAction SilentlyContinue)
}

function Remove-LmforgeUiDirectory {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return $true }
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            Info "Removed $Path"
            return $true
        } catch {
            if ($attempt -lt 3) {
                Warn "Could not remove $Path (attempt $attempt): $($_.Exception.Message)"
                Stop-LmforgeUiProcesses | Out-Null
                Start-Sleep -Seconds 2
            } else {
                Warn "Could not remove $Path : $($_.Exception.Message)"
                Warn "Quit LMForge from the system tray, then re-run uninstall-ui.ps1"
                return $false
            }
        }
    }
    return $false
}

# --- Quit running app ---
Section "Quitting LMForge..."
if (Stop-LmforgeUiProcesses) {
    Info "App process stopped"
} else {
    Warn "lmforge-ui.exe is still running (check system tray)"
}

# --- Run NSIS uninstaller if registered ---
Section "Removing app..."

function Find-LMForgeUiUninstallEntry {
    # Look up the NSIS uninstall registration by its known product key instead
    # of enumerating the entire Uninstall hive (a wildcard scan over every
    # installed product trips AV heuristics). Tauri/NSIS registers the key
    # under the product name / bundle identifier.
    $base = "Software\Microsoft\Windows\CurrentVersion\Uninstall"
    $names = @("LMForge", "com.lmforge.app")
    $hives = @("HKCU:", "HKLM:", "HKLM:\Software\WOW6432Node")
    foreach ($hive in $hives) {
        foreach ($name in $names) {
            $key = if ($hive -like "*WOW6432Node*") {
                "$hive\Microsoft\Windows\CurrentVersion\Uninstall\$name"
            } else {
                "$hive\$base\$name"
            }
            $entry = Get-ItemProperty -Path $key -ErrorAction SilentlyContinue
            if ($entry -and $entry.UninstallString) { return $entry }
        }
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
    Stop-LmforgeUiProcesses | Out-Null
    if (Remove-LmforgeUiDirectory -Path $InstallDir) {
        $Removed = $true
    }
} elseif (-not $Removed -and -not (Test-Path $AppExe)) {
    Warn "LMForge UI not found - may already be uninstalled"
}

# --- Remove app data (Tauri identifier: com.lmforge.app) ---
$AppDataDir = "$env:APPDATA\com.lmforge.app"
if (Test-Path $AppDataDir) {
    try {
        Remove-Item -LiteralPath $AppDataDir -Recurse -Force -ErrorAction Stop
        Info "Removed $AppDataDir"
    } catch {
        Warn "Could not remove $AppDataDir : $($_.Exception.Message)"
    }
}

Write-Host ""
Success "LMForge UI uninstalled."
Write-Host ""
Write-Host "  The daemon is still running. To also remove Core, run" -ForegroundColor White
Write-Host "  uninstall-core.ps1 from the latest release:" -ForegroundColor White
Write-Host "    https://github.com/$Repo/releases/latest" -ForegroundColor White
Write-Host ""
Write-Host "  To reinstall the UI, run install-ui.ps1 from the same release." -ForegroundColor White
Write-Host ""

if (Test-Path $AppExe) { exit 1 }
exit 0
