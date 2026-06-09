# Smoke-test all Windows install/uninstall scripts locally.
#
#   .\scripts\util\test-windows-scripts.ps1           # syntax only (fast)
#   .\scripts\util\test-windows-scripts.ps1 -Invoke   # run a guided local iex sequence
#
# Individual script (same as GitHub irm | iex):
#   .\scripts\util\run-ps1-like-github.ps1 install-core
#   .\scripts\util\run-ps1-like-github.ps1 uninstall-ui -Yes

param(
    [switch]$Invoke
)

$ErrorActionPreference = "Stop"
$utilDir = $PSScriptRoot
$runner = Join-Path $utilDir "run-ps1-like-github.ps1"
$scripts = @("install-core", "install-ui", "uninstall-ui", "uninstall-core", "install-root")

Write-Host "  Windows script checks" -ForegroundColor Cyan
Write-Host ""

$fail = 0
foreach ($name in $scripts) {
    & $runner $name -CheckSyntaxOnly
    if ($LASTEXITCODE -ne 0) { $fail++ }
}

& (Join-Path $utilDir "test-ps1-syntax.ps1")
if ($LASTEXITCODE -ne 0) { $fail++ }

$launcher = Join-Path $env:USERPROFILE ".lmforge\daemon-task.cmd"
if (Test-Path -LiteralPath $launcher) {
    Write-Host "OK   daemon-task.cmd present at $launcher"
} else {
    Write-Host "SKIP daemon-task.cmd (created by lmforge service install)"
}

if ($fail -gt 0) {
    Write-Host ""
    Write-Host "  $fail check(s) failed" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "  All syntax checks passed" -ForegroundColor Green

if (-not $Invoke) {
    Write-Host ""
    Write-Host "  Run like GitHub (from repo root):" -ForegroundColor White
    Write-Host "    .\scripts\util\run-ps1-like-github.ps1 install-core"
    Write-Host "    .\scripts\util\run-ps1-like-github.ps1 install-ui"
    Write-Host "    .\scripts\util\run-ps1-like-github.ps1 uninstall-ui -Yes"
    Write-Host "    `$env:LMFORGE_PURGE='1'; .\scripts\util\run-ps1-like-github.ps1 uninstall-core -Yes"
    exit 0
}

Write-Host ""
Write-Host "  -Invoke runs install/uninstall against your live machine." -ForegroundColor Yellow
Write-Host "  Quit lmforge-ui from the tray before uninstall-ui." -ForegroundColor Yellow
Write-Host ""
$go = Read-Host "  Continue with live install-core? [y/N]"
if ($go -notmatch '^[Yy]$') { exit 0 }
& $runner install-core
