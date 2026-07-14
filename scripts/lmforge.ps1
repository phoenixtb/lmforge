# ==============================================================================
# LMForge CLI — dev / test / release menu (Windows)
#
# Module model (install SOURCE is a parameter, not a separate script):
#   clean    [-Purge] [-Dev]                 uninstall core + UI (+ dev artefacts)
#   install  -Source local|release[:TAG]     build+install local, or install release
#   e2e      -Source local|release[:TAG] [-Inference|-NoInference] [-WithUi|-NoUi]
#                                            [-VerifyAssets] [-KeepInstall] [-Purge]
#   dev-up                                    build+run from repo, debug (dev loop)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 e2e -Source local -Inference
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 e2e -Source release:v0.1.5 -KeepInstall
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 install -Source release:v0.1.5
#   powershell -ExecutionPolicy Bypass -File scripts\lmforge.ps1 clean -Dev -Purge
# ==============================================================================
param(
    [Parameter(Position = 0)]
    [string]$Action = "",
    [string]$Source = "",
    [switch]$Inference,
    [switch]$NoInference,
    [switch]$WithUi,
    [switch]$NoUi,
    [switch]$VerifyAssets,
    [switch]$KeepInstall,
    [switch]$Purge,
    [switch]$Dev
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Util     = Join-Path $RepoRoot "scripts\util"
$UiExe    = "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe"

# Run an installer/uninstaller in a child pwsh so its `exit` can't kill us.
function Invoke-Lf([string]$Name) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $RepoRoot "scripts\$Name")
    if ($LASTEXITCODE -ne 0) { throw "$Name exited $LASTEXITCODE" }
}

$Menu = @(
    @{ Key = "status";     Label = "Status             Snapshot binaries, daemon, disk" },
    @{ Key = "install";    Label = "Install            Install core (-Source local|release[:TAG])" },
    @{ Key = "e2e";        Label = "E2E                Install + lifecycle + inference (-Source …)" },
    @{ Key = "clean";      Label = "Clean              Uninstall core + UI (-Dev -Purge)" },
    @{ Key = "dev-up";     Label = "Dev up             Build + install core + UI from repo" },
    @{ Key = "test-dev";   Label = "Test: dev matrix   API + inference (dev_test.sh)" },
    @{ Key = "test-multi"; Label = "Test: multi-model  Inference suite against running daemon" },
    @{ Key = "quit";       Label = "Quit" }
)

function Invoke-Action([string]$Key) {
    switch ($Key) {
        "status" {
            if (Test-Path (Join-Path $Util "dev_status.sh")) {
                bash (Join-Path $Util "dev_status.sh")
            } else {
                Write-Host "  core: $(Test-Path "$env:USERPROFILE\.lmforge\bin\lmforge.exe")"
                Write-Host "  ui:   $(Test-Path $UiExe)"
                try {
                    $h = Invoke-WebRequest "http://127.0.0.1:11430/health" -UseBasicParsing -TimeoutSec 3
                    Write-Host "  daemon: $($h.Content)"
                } catch { Write-Host "  daemon: down" }
            }
        }
        "install" {
            $src = if ($Source) { $Source } else { "release" }
            if ($src -eq "local") {
                & cargo build --release --bin lmforge
                if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
                $env:LMFORGE_LOCAL_BIN = (Join-Path $RepoRoot "target\release\lmforge.exe")
                Invoke-Lf "install-core.ps1"
                # Build + install the UI from this checkout too (parity with the
                # bash path). Best-effort: a missing Node/Rust toolchain must not
                # fail the core install — warn and continue.
                try {
                    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Util "build-ui-local.ps1")
                    if ($LASTEXITCODE -ne 0) { Write-Host "  UI local build skipped (exit $LASTEXITCODE) — core install unaffected." -ForegroundColor Yellow }
                } catch {
                    Write-Host "  UI local build skipped: $($_.Exception.Message) — core install unaffected." -ForegroundColor Yellow
                }
            } else {
                $tag = $src -replace '^release:?', ''
                if ($tag -and $tag -ne "latest") { $env:LMFORGE_VERSION = $tag }
                Invoke-Lf "install-core.ps1"
                Invoke-Lf "install-ui.ps1"
            }
        }
        "e2e" {
            $a = @{}
            if ($Source)       { $a.Source = $Source }
            if ($Inference)    { $a.Inference = $true }
            if ($NoInference)  { $a.NoInference = $true }
            if ($WithUi)       { $a.WithUi = $true }
            if ($NoUi)         { $a.NoUi = $true }
            if ($VerifyAssets) { $a.VerifyAssets = $true }
            if ($KeepInstall)  { $a.KeepInstall = $true }
            if ($Purge)        { $a.Purge = $true }
            & (Join-Path $Util "e2e.ps1") @a
        }
        "clean" {
            $env:LMFORGE_YES = "1"
            if (Test-Path $UiExe) {
                Get-Process lmforge-ui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
                Start-Sleep 2
                Invoke-Lf "uninstall-ui.ps1"
            }
            if ($Purge) { $env:LMFORGE_PURGE = "1" }
            Invoke-Lf "uninstall-core.ps1"
            if ($Dev -and (Get-Command bash -EA SilentlyContinue)) {
                bash (Join-Path $Util "dev_clean.sh") --all --yes
            }
        }
        "dev-up" {
            & (Join-Path $Util "dev-reinstall.ps1")
        }
        "test-dev" {
            bash (Join-Path $Util "dev_test.sh") --yes
        }
        "test-multi" {
            & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $RepoRoot "tests\multi_model_e2e.ps1")
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
