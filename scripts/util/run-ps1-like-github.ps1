# Run a repo install/uninstall script the same way GitHub does: pipe content into iex.
#
# GitHub:
#   irm https://github.com/phoenixtb/lmforge/releases/latest/download/install-core.ps1 | iex
#
# Local (identical execution model):
#   .\scripts\util\run-ps1-like-github.ps1 install-core
#   .\scripts\util\run-ps1-like-github.ps1 uninstall-ui -Yes
#   .\scripts\util\run-ps1-like-github.ps1 uninstall-core -Yes -Purge
#
# Optional env (same as GitHub):
#   $env:LMFORGE_VERSION = "v0.1.5"
#   $env:LMFORGE_YES = "1"
#   $env:LMFORGE_PURGE = "1"

param(
    [Parameter(Position = 0, Mandatory = $true)]
    [ValidateSet("install-core", "install-ui", "uninstall-core", "uninstall-ui")]
    [string]$Script,

    [switch]$Yes,
    [switch]$Purge,
    [switch]$CheckSyntaxOnly
)

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$scriptPath = Join-Path $repoRoot "scripts\$Script.ps1"

if (-not (Test-Path -LiteralPath $scriptPath)) {
    Write-Error "Script not found: $scriptPath"
}

# Syntax check (same parser CI uses)
$parseErrors = $null
[void][System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$null, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) {
    Write-Host "SYNTAX FAIL $scriptPath" -ForegroundColor Red
    $parseErrors | ForEach-Object { Write-Host $_.ToString() }
    exit 1
}

if ($CheckSyntaxOnly) {
    Write-Host "OK   $Script.ps1 syntax"
    exit 0
}

if ($Yes) { $env:LMFORGE_YES = "1" }
if ($Purge) { $env:LMFORGE_PURGE = "1" }

Write-Host ""
Write-Host "  Local iex run (same as irm | iex)" -ForegroundColor Cyan
Write-Host "  Script : $scriptPath"
Write-Host "  LMFORGE_VERSION : $(if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { '(latest)' })"
Write-Host "  LMFORGE_YES     : $(if ($env:LMFORGE_YES) { $env:LMFORGE_YES } else { '(prompt)' })"
Write-Host "  LMFORGE_PURGE   : $(if ($env:LMFORGE_PURGE) { $env:LMFORGE_PURGE } else { '(no)' })"
Write-Host ""

$content = Get-Content -LiteralPath $scriptPath -Raw
Invoke-Expression $content
