# =============================================================================
# LMForge Core - E2E test (Windows)
# Full lifecycle: install -> health -> sysinfo -> service -> uninstall.
# Uses the real installer/uninstaller scripts from this checkout.
#
# Modes (one required):
#   $env:LMFORGE_LOCAL_BIN = "target\release\lmforge.exe"; .\scripts\util\e2e-core.ps1
#       Test a locally built binary (CI release gate).
#   $env:LMFORGE_VERSION = "v0.1.6"; .\scripts\util\e2e-core.ps1
#       Test a published GitHub release (manual post-release check).
#
# Exit code 0 = all steps passed.
# =============================================================================
$ErrorActionPreference = "Continue"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$Bin      = "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe"
$Api      = "http://127.0.0.1:11430"
$RunKey   = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$Results  = New-Object System.Collections.Generic.List[string]
$env:LMFORGE_YES = "1"

if (-not $env:LMFORGE_LOCAL_BIN -and -not $env:LMFORGE_VERSION) {
    Write-Host "Set LMFORGE_LOCAL_BIN=<path> (local build) or LMFORGE_VERSION=<tag> (release)." -ForegroundColor Red
    exit 2
}
if ($env:LMFORGE_LOCAL_BIN) {
    $env:LMFORGE_LOCAL_BIN = (Resolve-Path $env:LMFORGE_LOCAL_BIN).Path
}

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

function Invoke-LmforgeScript {
    param([string]$Name)
    # Run installer/uninstaller in a child pwsh so their `exit` cannot kill us.
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $RepoRoot "scripts\$Name")
    if ($LASTEXITCODE -ne 0) { throw "$Name exited $LASTEXITCODE" }
}

Step "preclean" {
    if (Test-Path $Bin) { Invoke-LmforgeScript "uninstall-core.ps1" }
    Get-Process lmforge -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

Step "install-core" { Invoke-LmforgeScript "install-core.ps1" }

Step "binary installed" {
    if (-not (Test-Path $Bin)) { throw "missing $Bin" }
    & $Bin --version
}

Step "health" {
    $body = (Invoke-WebRequest "$Api/health" -UseBasicParsing -TimeoutSec 15).Content
    Write-Host $body
    if ($body -notmatch '"ok"') { throw $body }
}

Step "sysinfo" {
    $json = (Invoke-WebRequest "$Api/lf/sysinfo" -UseBasicParsing -TimeoutSec 15).Content | ConvertFrom-Json
    if ($null -eq $json.cpu_pct) { throw "no cpu_pct" }
    Write-Host "sysinfo ok (cpu_pct=$($json.cpu_pct))"
}

Step "service status" {
    $out = & $Bin service status 2>&1 | Out-String
    Write-Host $out
    if ($out -notmatch "reachable") { throw "daemon not reachable per service status" }
}

Step "autostart registered" {
    $val = Get-ItemProperty -Path $RunKey -Name "LMForge" -ErrorAction SilentlyContinue
    if (-not $val) { throw "Run key value 'LMForge' not registered" }
    if ($val.LMForge -notmatch "wscript") { throw "Run key does not use wscript: $($val.LMForge)" }
    Write-Host "Run key: $($val.LMForge)"
    $vbs = Join-Path $env:USERPROFILE ".lmforge\daemon-task.vbs"
    if (-not (Test-Path $vbs)) { throw "missing launcher $vbs" }
}

Step "uninstall-core" { Invoke-LmforgeScript "uninstall-core.ps1" }

Step "binary removed" {
    if (Test-Path $Bin) { throw "$Bin still exists" }
}

Step "daemon down" {
    Start-Sleep 2
    $up = $false
    try {
        Invoke-WebRequest "$Api/health" -UseBasicParsing -TimeoutSec 2 | Out-Null
        $up = $true
    } catch {}
    if ($up) { throw "daemon still reachable after uninstall" }
}

Step "autostart removed" {
    $val = Get-ItemProperty -Path $RunKey -Name "LMForge" -ErrorAction SilentlyContinue
    if ($val) { throw "Run key value still present: $($val.LMForge)" }
    $vbs = Join-Path $env:USERPROFILE ".lmforge\daemon-task.vbs"
    if (Test-Path $vbs) { throw "launcher still present: $vbs" }
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
