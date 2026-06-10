# End-to-end Windows release smoke test (v0.1.5).
# Uses local scripts (must match tag) + GitHub release binary via LMFORGE_VERSION.
param(
    [string]$Version = "v0.1.5",
    [switch]$SkipUninstall
)

$ErrorActionPreference = "Continue"
$env:LMFORGE_VERSION = $Version
$env:LMFORGE_YES = "1"
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$Runner = Join-Path $PSScriptRoot "run-ps1-like-github.ps1"
$CoreBin = "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe"
$UiExe = "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe"
$Results = New-Object System.Collections.Generic.List[string]

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

Step "release scripts on GitHub" {
    $names = @("install-core.ps1", "install-ui.ps1", "uninstall-core.ps1", "uninstall-ui.ps1")
    foreach ($n in $names) {
        $url = "https://github.com/phoenixtb/lmforge/releases/download/$Version/$n"
        $tmp = Join-Path $env:TEMP "lf-release-$n"
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
        $local = Join-Path $RepoRoot "scripts\$n"
        $relNorm = (Get-Content $tmp -Raw) -replace "`r`n", "`n"
        $locNorm = (Get-Content $local -Raw) -replace "`r`n", "`n"
        if ($relNorm -ne $locNorm) {
            throw "$n content mismatch (release vs repo at $Version)"
        }
    }
}

Step "release binary on GitHub" {
    $url = "https://github.com/phoenixtb/lmforge/releases/download/$Version/lmforge-windows-x86_64.exe"
    $r = Invoke-WebRequest -Uri $url -Method Head -UseBasicParsing
    if ($r.StatusCode -ne 200) { throw "binary HEAD failed" }
    if ([int64]$r.Headers["Content-Length"] -lt 1MB) { throw "binary too small" }
}

if (-not $SkipUninstall) {
    if (Test-Path $UiExe) {
        Get-Process lmforge-ui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
        Start-Sleep 2
        Step "uninstall-ui" { & $Runner uninstall-ui -Yes }
    }
    if (Test-Path $CoreBin) {
        Step "uninstall-core" { & $Runner uninstall-core -Yes }
    }
}

Step "install-core" { & $Runner install-core }
Start-Sleep 3

Step "health" {
    $body = (Invoke-WebRequest "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 15).Content
    if ($body -notmatch '"status"\s*:\s*"ok"') { throw $body }
    Write-Host $body
}

Step "service status" {
    & $CoreBin service status
    $out = & $CoreBin service status 2>&1 | Out-String
    if ($out -notmatch "reachable") { throw $out }
}

Step "autostart run key" {
    $val = Get-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "LMForge" -ErrorAction SilentlyContinue
    if (-not $val) { throw "Run key value 'LMForge' not registered" }
    if ($val.LMForge -notmatch "wscript") { throw "Run key does not use wscript: $($val.LMForge)" }
    Write-Host "Run key: $($val.LMForge)"
    # Legacy scheduled task must be gone (or at least not the autostart path).
    $task = schtasks /Query /TN "LMForge Daemon" 2>&1 | Out-String
    if ($task -notmatch "ERROR") { Write-Host "note: legacy scheduled task still present (harmless)" }
}

Step "daemon-task.vbs" {
    $vbs = Join-Path $env:USERPROFILE ".lmforge\daemon-task.vbs"
    if (-not (Test-Path $vbs)) { throw "missing $vbs" }
    $content = Get-Content $vbs -Raw
    if ($content -notmatch "Wscript\.Shell" -or $content -notmatch " start") {
        throw "bad vbs content"
    }
}

Step "sysinfo endpoint" {
    $json = (Invoke-WebRequest "http://127.0.0.1:11430/lf/sysinfo" -UseBasicParsing -TimeoutSec 15).Content | ConvertFrom-Json
    if ($null -eq $json.cpu_pct) { throw "no cpu_pct" }
    Write-Host "gpu source: $($json.gpu.source)"
}

Step "install-ui" { & $Runner install-ui }
Start-Sleep 2

Step "ui binary" {
    if (-not (Test-Path $UiExe)) { throw "UI not installed at $UiExe" }
}

Step "health after ui" {
    $body = (Invoke-WebRequest "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 10).Content
    if ($body -notmatch '"ok"') { throw $body }
}

if (-not $SkipUninstall) {
    Get-Process lmforge-ui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep 2
    Step "uninstall-ui" { & $Runner uninstall-ui -Yes }
    if (Test-Path $UiExe) { throw "UI dir still exists" }
    Step "uninstall-core" { & $Runner uninstall-core -Yes }
    if (Test-Path $CoreBin) { throw "core binary still exists" }
    try {
        Invoke-WebRequest "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 2 | Out-Null
        throw "daemon still up after uninstall"
    } catch {
        if ($_.Exception.Response) { throw "daemon still up" }
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
