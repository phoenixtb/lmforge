# =============================================================================
# LMForge Core - Windows PowerShell Uninstaller
# Stops the daemon, removes autostart (Run key + legacy Scheduled Task),
# removes the binary and PATH.
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

# Resilient recursive delete: engine DLLs (e.g. cublas) can be briefly locked by
# a just-exited llama-server child, so retry instead of aborting the uninstall.
function Remove-Tree {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return $true }
    for ($i = 0; $i -lt 5; $i++) {
        try { Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop; return $true }
        catch { Start-Sleep -Milliseconds 600 }
    }
    Warn "Could not fully remove $Path (file in use): $($Error[0].Exception.Message)"
    return $false
}

$Repo       = "phoenixtb/lmforge"
$Binary     = "lmforge.exe"
# Current install location; installs <= v0.1.6 used the hidden AppData path.
# Both are cleaned up.
$InstallDir       = "$env:USERPROFILE\.lmforge\bin"
$LegacyInstallDir = "$env:LOCALAPPDATA\lmforge\bin"
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
        # `return`, not `exit`: `exit` under `irm | iex` closes the terminal.
        return
    }
}

# --- 1. Stop + unregister service via CLI ---
Section "Stopping daemon and removing service..."
$env:PATH = "$InstallDir;$LegacyInstallDir;$env:PATH"
$LmforgeCmd = Get-Command "lmforge" -ErrorAction SilentlyContinue
$CoreBin = $null
if ($LmforgeCmd) { $CoreBin = $LmforgeCmd.Source }
elseif (Test-Path "$InstallDir\$Binary") { $CoreBin = "$InstallDir\$Binary" }
elseif (Test-Path "$LegacyInstallDir\$Binary") { $CoreBin = "$LegacyInstallDir\$Binary" }

if ($CoreBin) {
    & $CoreBin service stop       2>$null | Out-Null
    & $CoreBin service uninstall  2>$null | Out-Null
    Info "Service unregistered via lmforge CLI"
}

# --- 2. Belt-and-suspenders: remove autostart Run key ---
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if (Get-ItemProperty -Path $RunKey -Name "LMForge" -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $RunKey -Name "LMForge" -ErrorAction SilentlyContinue
    Info "Removed autostart Run key"
}
Remove-Item "$DataDir\daemon-task.vbs" -Force -ErrorAction SilentlyContinue
Remove-Item "$DataDir\daemon-task.cmd" -Force -ErrorAction SilentlyContinue

# --- 2b. Legacy cleanup: Scheduled Task from installs <= v0.1.5 ---
# Non-fatal: access denied must not abort purge or binary removal.
$taskRemoved = $false
try {
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($task) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction Stop
        $taskRemoved = $true
    }
} catch {}

if (-not $taskRemoved) {
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $schtasksOut = & schtasks.exe /Delete /TN $TaskName /F 2>&1 | Out-String
    $ErrorActionPreference = $prevEap
    if ($LASTEXITCODE -eq 0) {
        $taskRemoved = $true
    } elseif ($schtasksOut -match 'cannot find|does not exist|not found') {
        # nothing to remove - fresh installs use the Run key, not a task
    } elseif ($schtasksOut -match 'Access is denied|access is denied') {
        Warn "Legacy scheduled task '$TaskName' still registered (access denied)."
        Warn "Remove from an elevated PowerShell: schtasks /Delete /TN `"$TaskName`" /F"
    } else {
        Warn "Could not remove legacy scheduled task '$TaskName' (schtasks exit $LASTEXITCODE)."
    }
}

if ($taskRemoved) {
    Info "Removed legacy scheduled task: $TaskName"
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
# Stop engine subprocesses (e.g. llama-server) that keep DLL handles (cublas, ...)
# open under the engines dir - otherwise a --purge can't delete them on Windows.
Get-Process -Name "llama-server" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
try {
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath -like "$DataDir\engines\*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
} catch {}
Start-Sleep -Seconds 1
Info "No lmforge processes running"

# --- 4. Remove binary (current + legacy locations) ---
# Non-fatal: a quarantined/blocked exe denies delete; the remaining
# cleanup steps must still run.
Section "Removing binary..."
$found = $false
foreach ($dir in @($InstallDir, $LegacyInstallDir)) {
    if (Test-Path "$dir\$Binary") {
        $found = $true
        try {
            Remove-Item "$dir\$Binary" -Force -ErrorAction Stop
            Info "Removed $dir\$Binary"
        } catch {
            Warn "Could not remove $dir\$Binary ($($_.Exception.Message))."
            Warn "Your security software has locked it. Restore/allow it in the"
            Warn "quarantine UI, or delete the folder manually after a reboot: $dir"
        }
    }
    # Rename-aside leftovers from blocked upgrades (lmforge.exe.old.<pid>)
    Get-ChildItem "$dir\*.exe.old*" -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}
if (-not $found) {
    Warn "lmforge binary not found at $InstallDir\$Binary"
}

# --- 5. Remove install dirs if empty ---
foreach ($dir in @($InstallDir, $LegacyInstallDir)) {
    if (Test-Path $dir) {
        $remaining = Get-ChildItem $dir -ErrorAction SilentlyContinue
        if (-not $remaining) {
            Remove-Item $dir -Force -ErrorAction SilentlyContinue
        }
    }
}
if (Test-Path "$env:LOCALAPPDATA\lmforge") {
    $left = Get-ChildItem "$env:LOCALAPPDATA\lmforge" -Recurse -ErrorAction SilentlyContinue
    if (-not $left) {
        Remove-Item "$env:LOCALAPPDATA\lmforge" -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# --- 6. PATH cleanup (current + legacy entries) ---
# Non-fatal: security software can deny registry env writes from a piped
# script. A failed PATH edit must not abort the remaining purge steps.
Section "Cleaning up PATH..."
try {
    $UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    $newPath = ($UserPath -split ';' |
        Where-Object { $_ -and $_ -ne $InstallDir -and $_ -ne $LegacyInstallDir }) -join ';'
    if ($newPath -ne $UserPath) {
        [System.Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
        Info "Removed install dir from user PATH"
    }
} catch {
    Warn "Could not update user PATH ($($_.Exception.Message))."
    Warn "Remove '$InstallDir' from PATH manually: Settings > System > About > Advanced system settings > Environment Variables"
}

# --- 7. PID / socket files ---
Remove-Item "$DataDir\lmforge.pid"  -Force -ErrorAction SilentlyContinue
Remove-Item "$DataDir\lmforge.sock" -Force -ErrorAction SilentlyContinue

# --- 8. Engine installs ---
Section "Removing installed engines..."
if (Test-Path "$DataDir\engines") {
    if (Remove-Tree "$DataDir\engines") { Info "Removed $DataDir\engines" }
}
if (Test-Path "$DataDir\bin") {
    if (Remove-Tree "$DataDir\bin") { Info "Removed $DataDir\bin" }
}

# --- 9. Data directory ---
Section "Data directory..."
if ($Purge) {
    if (Test-Path $DataDir) {
        if (Remove-Tree $DataDir) { Info "Data directory removed" }
    }
    $uiData = "$env:APPDATA\com.lmforge.app"
    if (Test-Path $uiData) {
        if (Remove-Tree $uiData) { Info "Removed $uiData" }
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
