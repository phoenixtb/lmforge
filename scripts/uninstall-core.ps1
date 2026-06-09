# =============================================================================
# LMForge Core - Windows PowerShell Uninstaller
# Stops the daemon, removes the Scheduled Task, removes the binary and PATH.
# Models and config in ~/.lmforge are kept unless -Purge is passed.
#
# Usage:
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/uninstall-core.ps1 | iex
#
# Skip confirmation:
#   $env:LMFORGE_YES = "1"; irm .../uninstall-core.ps1 | iex
#
# Remove models too:
#   $env:LMFORGE_PURGE = "1"; irm .../uninstall-core.ps1 | iex
# =============================================================================
param(
    [switch]$Purge,
    [switch]$Yes
)

if (-not $Yes -and $env:LMFORGE_YES -match '^(1|true|yes)$') { $Yes = $true }
if (-not $Purge -and $env:LMFORGE_PURGE -match '^(1|true|yes)$') { $Purge = $true }

$ErrorActionPreference = "Stop"

function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Section { param($m) Write-Host ""; Write-Host "  $m" -ForegroundColor White }

$Repo       = "phoenixtb/lmforge"
$Binary     = "lmforge.exe"
$InstallDir = "$env:LOCALAPPDATA\lmforge\bin"
$DataDir    = "$env:USERPROFILE\.lmforge"
$TaskName   = "LMForge Daemon"

Write-Host ""
Write-Host "  LMForge Core - Uninstaller" -ForegroundColor Cyan
if ($Purge) {
    Write-Host "  --purge: ALL data including downloaded models will be deleted." -ForegroundColor Red
} else {
    Write-Host "  Models and config in $DataDir will be kept."
    Write-Host "  Set `$env:LMFORGE_PURGE = '1' to remove everything."
}
Write-Host ""

if (-not $Yes) {
    $confirm = Read-Host "  Continue? [y/N]"
    if ($confirm -notmatch '^[Yy]$') {
        Write-Host "  Aborted."
        exit 0
    }
}

# --- 1. Stop + unregister service via CLI ---
Section "Stopping daemon and removing service..."
$env:PATH = "$InstallDir;$env:PATH"
$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
$CoreBin = if ($LmforgeCmd) { $LmforgeCmd.Source } elseif (Test-Path "$InstallDir\$Binary") { "$InstallDir\$Binary" } else { $null }

if ($CoreBin) {
    & $CoreBin service stop       2>$null | Out-Null
    & $CoreBin service uninstall  2>$null | Out-Null
    Info "Service unregistered via lmforge CLI"
}

# --- 2. Belt-and-suspenders: remove Scheduled Task ---
# Non-fatal: access denied must not abort purge or binary removal.
$taskRemoved = $false
try {
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($task) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction Stop
        $taskRemoved = $true
    }
} catch {
    Warn "Could not unregister scheduled task via PowerShell: $($_.Exception.Message)"
}

if (-not $taskRemoved) {
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $schtasksOut = & schtasks.exe /Delete /TN $TaskName /F 2>&1 | Out-String
    $ErrorActionPreference = $prevEap
    if ($LASTEXITCODE -eq 0) {
        $taskRemoved = $true
    } elseif ($schtasksOut -match 'cannot find|does not exist|not found') {
        $taskRemoved = $true
    } elseif ($schtasksOut -match 'Access is denied|access is denied') {
        Warn "Scheduled task '$TaskName' still registered (access denied)."
        Warn "Run uninstall again from an elevated PowerShell, or: schtasks /Delete /TN `"$TaskName`" /F"
    } else {
        Warn "Could not remove scheduled task '$TaskName' (schtasks exit $LASTEXITCODE)."
    }
}

if ($taskRemoved) {
    Info "Removed scheduled task: $TaskName"
}

# --- 3. Graceful shutdown via API, then force-kill ---
Section "Stopping any running daemon process..."
try {
    Invoke-WebRequest -Uri "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 3 | Out-Null
    Invoke-WebRequest -Uri "http://127.0.0.1:11430/lf/shutdown" -Method POST -UseBasicParsing -TimeoutSec 5 | Out-Null
    Start-Sleep -Seconds 1
    Info "Daemon shutdown via API"
} catch {}

Get-Process -Name "lmforge" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
Info "No lmforge processes running"

# --- 4. Remove binary ---
Section "Removing binary..."
if (Test-Path "$InstallDir\$Binary") {
    Remove-Item "$InstallDir\$Binary" -Force
    Info "Removed $InstallDir\$Binary"
} else {
    Warn "lmforge binary not found at $InstallDir\$Binary"
}

# --- 5. Remove install dir if empty ---
if (Test-Path $InstallDir) {
    $remaining = Get-ChildItem $InstallDir -ErrorAction SilentlyContinue
    if (-not $remaining) {
        Remove-Item $InstallDir -Force -ErrorAction SilentlyContinue
    }
}
if (Test-Path "$env:LOCALAPPDATA\lmforge") {
    $left = Get-ChildItem "$env:LOCALAPPDATA\lmforge" -Recurse -ErrorAction SilentlyContinue
    if (-not $left) {
        Remove-Item "$env:LOCALAPPDATA\lmforge" -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# --- 6. PATH cleanup ---
Section "Cleaning up PATH..."
$UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -like "*$InstallDir*") {
    $newPath = ($UserPath -split ';' | Where-Object { $_ -and $_ -ne $InstallDir }) -join ';'
    [System.Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    Info "Removed $InstallDir from user PATH"
}

# --- 7. PID / socket files ---
Remove-Item "$DataDir\lmforge.pid"  -Force -ErrorAction SilentlyContinue
Remove-Item "$DataDir\lmforge.sock" -Force -ErrorAction SilentlyContinue

# --- 8. Engine installs ---
Section "Removing installed engines..."
if (Test-Path "$DataDir\engines") {
    Remove-Item "$DataDir\engines" -Recurse -Force
    Info "Removed $DataDir\engines"
}
if (Test-Path "$DataDir\bin") {
    Remove-Item "$DataDir\bin" -Recurse -Force
    Info "Removed $DataDir\bin"
}

# --- 9. Data directory ---
Section "Data directory..."
if ($Purge) {
    if (Test-Path $DataDir) {
        try {
            Remove-Item -LiteralPath $DataDir -Recurse -Force -ErrorAction Stop
            Info "Data directory removed"
        } catch {
            Warn "Could not remove $DataDir : $($_.Exception.Message)"
        }
    }
    $uiData = "$env:APPDATA\com.lmforge.app"
    if (Test-Path $uiData) {
        try {
            Remove-Item -LiteralPath $uiData -Recurse -Force -ErrorAction Stop
            Info "Removed $uiData"
        } catch {
            Warn "Could not remove $uiData : $($_.Exception.Message)"
        }
    }
} else {
    Info "Keeping $DataDir (set LMFORGE_PURGE=1 to remove)"
    Write-Host "  Your downloaded models are safe." -ForegroundColor White
}

Write-Host ""
Success "LMForge Core uninstalled."
if (-not $Purge -and (Test-Path "$DataDir\models")) {
    Write-Host ""
    Write-Host "  Models still at: $DataDir\models" -ForegroundColor White
    Write-Host "  To remove everything:" -ForegroundColor White
    Write-Host "    `$env:LMFORGE_PURGE = '1'; irm https://github.com/$Repo/releases/latest/download/uninstall-core.ps1 | iex" -ForegroundColor White
}
Write-Host ""
