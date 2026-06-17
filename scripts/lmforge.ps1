# ==============================================================================
# LMForge CLI — interactive dev / test / release menu (Windows)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 test-multi
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 release-e2e -Full
# ==============================================================================
param(
    [Parameter(Position = 0)]
    [string]$Action = "",
    [string]$Version = $(if ($env:LMFORGE_VERSION) { $env:LMFORGE_VERSION } else { "latest" }),
    [switch]$Full,
    [switch]$KeepInstall
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Util     = Join-Path $RepoRoot "scripts\util"

$Menu = @(
    @{ Key = "status";            Label = "Status             Snapshot binaries, daemon, disk" },
    @{ Key = "dev-reinstall";     Label = "Dev reinstall      Build + install core + UI from repo" },
    @{ Key = "test-dev";          Label = "Test: dev matrix   API + inference (dev_test.sh)" },
    @{ Key = "test-multi";        Label = "Test: multi-model  Chat+embed co-load E2E" },
    @{ Key = "test-e2e-core";     Label = "Test: install E2E  Core install lifecycle (local bin)" },
    @{ Key = "test-release";      Label = "Test: release      Release smoke (no model pull)" },
    @{ Key = "release-e2e";       Label = "Release E2E        Install release + models + inference" },
    @{ Key = "cleanup-ui";        Label = "Uninstall UI       uninstall-ui.ps1" },
    @{ Key = "cleanup-core";      Label = "Uninstall core     uninstall-core.ps1" },
    @{ Key = "quit";              Label = "Quit" }
)

function Invoke-Action([string]$Key) {
    switch ($Key) {
        "status" {
            if (Test-Path (Join-Path $Util "dev_status.sh")) {
                bash (Join-Path $Util "dev_status.sh")
            } else {
                Write-Host "  core: $(Test-Path "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe")"
                Write-Host "  ui:   $(Test-Path "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe")"
                try {
                    $h = Invoke-WebRequest "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 3
                    Write-Host "  daemon: $($h.Content)"
                } catch { Write-Host "  daemon: down" }
            }
        }
        "dev-reinstall" {
            & (Join-Path $Util "dev-reinstall.ps1")
        }
        "test-dev" {
            bash (Join-Path $Util "dev_test.sh") --yes
        }
        "test-multi" {
            $args = @("-File", (Join-Path $RepoRoot "tests\multi_model_e2e.ps1"))
            if ($Full) { $args += "-Full" }
            & powershell -NoProfile -ExecutionPolicy Bypass @args
        }
        "test-e2e-core" {
            $bin = Join-Path $RepoRoot "target\release\lmforge.exe"
            if (-not (Test-Path $bin)) { throw "Build first: cargo build --release --bin lmforge" }
            $env:LMFORGE_LOCAL_BIN = $bin
            & (Join-Path $Util "e2e-core.ps1")
        }
        "test-release" {
            & (Join-Path $Util "test-release-windows.ps1") -Version $Version
        }
        "release-e2e" {
            $args = @{ Version = $Version }
            if ($Full) { $args.Full = $true }
            if ($KeepInstall) { $args.KeepInstall = $true }
            & (Join-Path $Util "e2e-release.ps1") @args
        }
        "cleanup-ui" {
            & (Join-Path $Util "run-ps1-like-github.ps1") uninstall-ui -Yes
        }
        "cleanup-core" {
            & (Join-Path $Util "run-ps1-like-github.ps1") uninstall-core -Yes
        }
        "quit" { exit 0 }
        default { throw "Unknown action: $Key" }
    }
}

if ($Action) {
    Invoke-Action $Action
    exit $LASTEXITCODE
}

Write-Host ""
Write-Host "  LMForge CLI (Windows)" -ForegroundColor White
Write-Host "  repo: $RepoRoot"
Write-Host ""

for ($i = 0; $i -lt $Menu.Count; $i++) {
    Write-Host ("  {0,2}) {1}" -f ($i + 1), $Menu[$i].Label)
}
Write-Host ""
$choice = Read-Host "  Choice [1-$($Menu.Count)]"
if ($choice -notmatch '^\d+$' -or [int]$choice -lt 1 -or [int]$choice -gt $Menu.Count) {
    Write-Host "  Aborted."
    exit 0
}

$key = $Menu[[int]$choice - 1].Key
Write-Host ""
Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor White
Write-Host ""

Invoke-Action $key
