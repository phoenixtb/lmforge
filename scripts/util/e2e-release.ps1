# =============================================================================
# LMForge — Release E2E (Windows)
# Install core + UI from a published GitHub release, pull predefined models,
# run multi-model E2E inference tests, optionally uninstall afterward.
#
# Usage:
#   .\scripts\util\e2e-release.ps1
#   .\scripts\util\e2e-release.ps1 -Version v0.1.5
#   .\scripts\util\e2e-release.ps1 -KeepInstall          # leave install in place
#   -Full is a legacy no-op (all suites on by default in multi_model_e2e.ps1)
#   .\scripts\util\e2e-release.ps1 -SkipCleanup          # skip preclean only
#   .\scripts\util\e2e-release.ps1 -Purge                # uninstall with data purge
#
# Exit 0 = all steps passed.
# =============================================================================
param(
    [string]$Version = $(if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { "latest" }),
    [switch]$Full,
    [switch]$KeepInstall,
    [switch]$SkipCleanup,
    [switch]$Purge
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$Runner   = Join-Path $PSScriptRoot "run-ps1-like-github.ps1"
$CoreBin  = "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe"
$UiExe    = "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe"
$Api      = "http://127.0.0.1:11430"
$MmScript = Join-Path $RepoRoot "tests\multi_model_e2e.ps1"
$Results  = New-Object System.Collections.Generic.List[string]

$env:LMFORGE_YES = "1"
if ($Version -ne "latest") { $env:LMFORGE_VERSION = $Version }

$env:N_REQUESTS  = if ($env:N_REQUESTS)  { $env:N_REQUESTS }  else { "5" }

function Step {
    param([string]$Name, [scriptblock]$Action)
    Write-Host ""
    Write-Host "=== $Name ===" -ForegroundColor Cyan
    try {
        & $Action
        if ($? -eq $false) { throw "step returned failure" }
        $Results.Add("PASS  $Name")
        Write-Host "PASS  $Name" -ForegroundColor Green
    } catch {
        $Results.Add("FAIL  $Name  $($_.Exception.Message)")
        Write-Host "FAIL  $Name  $($_.Exception.Message)" -ForegroundColor Red
    }
}

function Invoke-Runner([string]$Script, [switch]$Yes, [switch]$PurgeFlag) {
    # Do not assign to $args — shadows PowerShell's automatic parameter array.
    $runnerArgs = @{ Script = $Script }
    if ($Yes) { $runnerArgs.Yes = $true }
    if ($PurgeFlag) { $runnerArgs.Purge = $true }
    & $Runner @runnerArgs
    # Inner scripts may leave a stale $LASTEXITCODE from Start-Process; runner exits 0 on success.
    if ($LASTEXITCODE -gt 0) { throw "$Script failed (exit $LASTEXITCODE)" }
}

Write-Host ""
Write-Host "  LMForge Release E2E (Windows)" -ForegroundColor Cyan
Write-Host "  version: $Version   keep: $($KeepInstall.IsPresent)"
Write-Host "  burst=$($env:N_REQUESTS)  (models: scripts/lib/e2e-defaults.ps1)"
Write-Host ""

if (-not $SkipCleanup) {
    Step "preclean" {
        if (Test-Path $UiExe) {
            Get-Process lmforge-ui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
            Start-Sleep 2
            Invoke-Runner uninstall-ui -Yes
        }
        if (Test-Path $CoreBin) {
            Invoke-Runner uninstall-core -Yes
        }
        Get-Process lmforge -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
}

Step "install-core" { Invoke-Runner install-core }
Start-Sleep 3

Step "core health" {
    $body = (Invoke-WebRequest "$Api/health" -UseBasicParsing -TimeoutSec 20).Content
    if ($body -notmatch '"status"\s*:\s*"ok"') { throw $body }
    Write-Host $body
}

Step "install-ui" { Invoke-Runner install-ui }
Start-Sleep 2

Step "ui binary" {
    if (-not (Test-Path $UiExe)) { throw "UI not at $UiExe" }
    Write-Host $UiExe
}

Step "multi-model e2e" {
    $env:SKIP_START = "1"
    $env:SKIP_BUILD = "1"
    $env:LF_BIN = $CoreBin
    $mmArgs = @("-File", $MmScript)
    & powershell -NoProfile -ExecutionPolicy Bypass @mmArgs
    if ($LASTEXITCODE -ne 0) { throw "multi_model_e2e.ps1 exited $LASTEXITCODE" }
}

if (-not $KeepInstall) {
    Get-Process lmforge-ui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep 2
    Step "uninstall-ui" { Invoke-Runner uninstall-ui -Yes }
    if ($Purge) { $env:LMFORGE_PURGE = "1" }
    Step "uninstall-core" { Invoke-Runner uninstall-core -Yes -PurgeFlag:($Purge.IsPresent) }
    Step "daemon down" {
        Start-Sleep 2
        try {
            Invoke-WebRequest "$Api/health" -UseBasicParsing -TimeoutSec 2 | Out-Null
            throw "daemon still reachable"
        } catch {
            if ($_.Exception.Response) { throw "daemon still reachable" }
        }
    }
}

Write-Host ""
Write-Host "========== SUMMARY ==========" -ForegroundColor White
$fail = 0
foreach ($line in $Results) {
    if ($line.StartsWith("FAIL")) { $fail++; Write-Host $line -ForegroundColor Red }
    else { Write-Host $line -ForegroundColor Green }
}
Write-Host ""
if ($fail -gt 0) { exit 1 }
exit 0
