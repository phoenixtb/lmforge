# =============================================================================
#  LMForge - unified E2E runner (Windows)
#
#  One runner for every install source. Composes the shared lifecycle
#  (scripts\lib\e2e-lifecycle.ps1) with optional UI install, asset verification,
#  and multi-model inference.
#
#  Full pre-release cycle (default for -Source local):
#    clean (keep models) -> build from current code -> install locally (core + UI) ->
#    install lifecycle -> multi-model inference -> uninstall (keep models),
#    unless -KeepInstall. Models preserved by default; -Purge wipes them.
#
#  Usage:
#    scripts\util\e2e.ps1 -Source local
#    scripts\util\e2e.ps1 -Source release:v0.1.5 -KeepInstall
#    scripts\util\e2e.ps1 -Source release:v0.1.5 -VerifyAssets -NoInference   # release smoke
#
#  Teardown removes binary/service/UI/autostart but KEEPS ~\.lmforge\models
#  unless -Purge. -KeepInstall skips teardown entirely.
# =============================================================================
param(
    [string]$Source = "",
    [switch]$Inference,
    [switch]$NoInference,
    [switch]$Thinking,
    [switch]$NoThinking,
    [switch]$WithUi,
    [switch]$NoUi,
    [switch]$VerifyAssets,
    [switch]$NoBuild,
    [switch]$KeepInstall,
    [switch]$Purge
)

$ErrorActionPreference = "Continue"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path

# ── Resolve install source ───────────────────────────────────────────────────
if (-not $Source) {
    if     ($env:LMFORGE_LOCAL_BIN) { $Source = "local" }
    elseif ($env:LMFORGE_VERSION)   { $Source = "release:$($env:LMFORGE_VERSION)" }
    else                            { $Source = "release:latest" }
}

$Kind = ""
if ($Source -eq "local") {
    $Kind = "local"
    if ($NoBuild) {
        $bin = $env:LMFORGE_LOCAL_BIN
        if (-not $bin) {
            $rel = Join-Path $RepoRoot "target\release\lmforge.exe"
            $dbg = Join-Path $RepoRoot "target\debug\lmforge.exe"
            if     (Test-Path $rel) { $bin = $rel }
            elseif (Test-Path $dbg) { $bin = $dbg }
            else { Write-Host "-NoBuild set but no binary found. Build: cargo build --release --bin lmforge" -ForegroundColor Red; exit 1 }
        }
        $env:LMFORGE_LOCAL_BIN = (Resolve-Path $bin).Path
    }
    Remove-Item Env:\LMFORGE_VERSION -ErrorAction SilentlyContinue
}
elseif ($Source -eq "release" -or $Source -eq "release:latest" -or $Source -eq "latest") {
    $Kind = "release"
    Remove-Item Env:\LMFORGE_LOCAL_BIN -ErrorAction SilentlyContinue
    Remove-Item Env:\LMFORGE_VERSION   -ErrorAction SilentlyContinue
}
elseif ($Source -like "release:*") {
    $Kind = "release"
    $env:LMFORGE_VERSION = $Source.Substring("release:".Length)
    Remove-Item Env:\LMFORGE_LOCAL_BIN -ErrorAction SilentlyContinue
}
else {
    Write-Host "Bad -Source: $Source (want local|release[:TAG])" -ForegroundColor Red; exit 1
}

# ── Flag defaults ────────────────────────────────────────────────────────────
# UI default on. -Source local builds the UI from this checkout (tauri build) and
# installs it; -Source release installs the published UI artifact.
$RunInference = -not $NoInference
$RunThinking = -not $NoThinking
$RunUi = if ($NoUi) { $false } else { $true }

. (Join-Path $PSScriptRoot "..\lib\e2e-lifecycle.ps1")

Write-Host "LMForge E2E - source=$Source ui=$RunUi inference=$RunInference thinking=$RunThinking verify=$VerifyAssets keep=$KeepInstall purge=$Purge"

# ── Asset verification (release only) ────────────────────────────────────────
if ($VerifyAssets) {
    if ($Kind -eq "release" -and $env:LMFORGE_VERSION) {
        E2eStep "release scripts match repo" { E2eReleaseScriptsMatch }
        E2eStep "release core binary"        { E2eReleaseCoreBinary }
    } else {
        Write-Host "  -VerifyAssets needs -Source release:<tag> (skipped)"
    }
}

# ── Clean slate (any prior install: GitHub script, dev build, …) ────────
# Default preserves ~\.lmforge\models so local iteration doesn't re-download;
# -Purge opts into a full wipe (models + config).
if ($Purge) {
    E2eStep "full clean (incl. models)" { E2eFullClean }
} else {
    E2eStep "clean (keep models)"       { E2ePreclean }
}

# ── Build from current source (local only) ───────────────────────────────────
if ($Kind -eq "local" -and -not $NoBuild) {
    E2eStep "build (cargo release)" { E2eBuildLocal }
}

# A failed local build must NOT silently fall through to install-core (which
# would download a *release* binary and report a misleading install-core PASS
# for a -Source local run). Abort loudly instead.
if ($Kind -eq "local" -and (-not $env:LMFORGE_LOCAL_BIN -or -not (Test-Path $env:LMFORGE_LOCAL_BIN))) {
    Write-Host ""
    Write-Host "  local build unavailable (LMFORGE_LOCAL_BIN unset/missing)." -ForegroundColor Red
    Write-Host "  Refusing to install a release binary under -Source local. Aborting." -ForegroundColor Red
    E2eSummary | Out-Null
    exit 1
}

# ── Install + lifecycle ──────────────────────────────────────────────────────
E2eStep "install-core"         { E2eInstallCore }
Start-Sleep 3
E2eStep "binary installed"     { E2eBinaryInstalled }
if ($Kind -eq "release") { E2eStep "core version matches tag" { E2eCoreVersionMatches } }
E2eStep "health"               { E2eHealthOk }
E2eStep "sysinfo"              { E2eSysinfoOk }
E2eStep "service status"       { E2eServiceStatusOk }
E2eStep "autostart registered" { E2eAutostartRegistered }

if ($RunUi) {
    if ($Kind -eq "local") {
        E2eStep "build+install UI (local)" { E2eInstallUiLocal }
    } else {
        E2eStep "install-ui" { E2eInstallUi }
    }
    Start-Sleep 2
    E2eStep "ui installed" { E2eUiInstalled }
    E2eStep "health after ui" { E2eHealthOk }
}

# ── Inference ────────────────────────────────────────────────────────────────
if ($RunInference) {
    E2eStep "engine preflight"      { E2eEnginePreflight }
    E2eStep "multi-model inference" { E2eInference }
    if ($RunThinking) { E2eStep "thinking gate" { E2eThinking } }
}

# ── Teardown (removes install; models kept unless -Purge) ──────────────────
if (-not $KeepInstall) {
    if ($RunUi) { E2eStep "uninstall-ui" { E2eUninstallUi } }
    if ($Purge) {
        $env:E2E_PURGE = "1"
        E2eStep "uninstall-core (purge)" { E2eUninstallCore }
    } else {
        $env:E2E_PURGE = "0"
        E2eStep "uninstall-core (keep models)" { E2eUninstallCore }
    }
    E2eStep "binary removed"     { E2eBinaryRemoved }
    E2eStep "daemon down"        { E2eDaemonDown }
    E2eStep "autostart removed"  { E2eAutostartRemoved }
    if ($Purge) { E2eStep "data/models removed" { E2eDataRemoved } }
    Remove-Item Env:\E2E_PURGE -EA SilentlyContinue
} else {
    Write-Host ""
    Write-Host "  -KeepInstall: leaving core (+UI) + models in place."
}

if (E2eSummary) { exit 0 } else { exit 1 }
