# =============================================================================
# LMForge Core - install-lifecycle E2E (Windows)
# Full lifecycle: install -> health -> sysinfo -> service -> autostart -> uninstall.
# No inference, no UI - this is the CI release gate (e2e.yml / release.yml).
#
# Modes (one required):
#   $env:LMFORGE_LOCAL_BIN = "target\release\lmforge.exe"; .\scripts\util\e2e-core.ps1
#       Test a locally built binary (CI release gate).
#   $env:LMFORGE_VERSION = "v0.1.6"; .\scripts\util\e2e-core.ps1
#       Test a published GitHub release.
#
# For inference / UI / asset-verification, use scripts\util\e2e.ps1.
# Exit code 0 = all steps passed.
# =============================================================================
$ErrorActionPreference = "Continue"

if (-not $env:LMFORGE_LOCAL_BIN -and -not $env:LMFORGE_VERSION) {
    Write-Host "Set LMFORGE_LOCAL_BIN=<path> (local build) or LMFORGE_VERSION=<tag> (release)." -ForegroundColor Red
    exit 2
}
if ($env:LMFORGE_LOCAL_BIN) {
    $env:LMFORGE_LOCAL_BIN = (Resolve-Path $env:LMFORGE_LOCAL_BIN).Path
}

. (Join-Path $PSScriptRoot "..\lib\e2e-lifecycle.ps1")

E2eStep "preclean"             { E2ePreclean }
E2eStep "install-core"         { E2eInstallCore }
E2eStep "binary installed"     { E2eBinaryInstalled }
E2eStep "health"               { E2eHealthOk }
E2eStep "sysinfo"              { E2eSysinfoOk }
E2eStep "service status"       { E2eServiceStatusOk }
E2eStep "autostart registered" { E2eAutostartRegistered }
E2eStep "uninstall-core"       { E2eUninstallCore }
E2eStep "binary removed"       { E2eBinaryRemoved }
E2eStep "daemon down"          { E2eDaemonDown }
E2eStep "autostart removed"    { E2eAutostartRemoved }

if (E2eSummary) { exit 0 } else { exit 1 }
