# =============================================================================
#  LMForge - shared install/lifecycle primitives for E2E harnesses (dot-source)
#
#  Single source of truth for the install -> health -> service -> uninstall
#  lifecycle that e2e-core.ps1 and the unified e2e.ps1 runner share. The install
#  SOURCE is whatever the caller exported before dot-sourcing:
#     $env:LMFORGE_LOCAL_BIN = <path>   -> install a locally built binary
#     $env:LMFORGE_VERSION   = <tag>    -> install a published GitHub release
#     (neither)                         -> install latest release
# =============================================================================

$E2E_Repo     = "phoenixtb/lmforge"
$E2E_RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$E2E_Api      = "http://127.0.0.1:11430"
$E2E_Bin      = "$env:LOCALAPPDATA\lmforge\bin\lmforge.exe"
$E2E_UiExe    = "$env:LOCALAPPDATA\LMForge\lmforge-ui.exe"
$E2E_RunKey   = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$E2E_Vbs      = Join-Path $env:USERPROFILE ".lmforge\daemon-task.vbs"
$E2E_CoreAsset = "lmforge-windows-x86_64.exe"
$E2E_UiAsset   = "LMForge-UI-windows-x86_64.exe"
$E2E_Results  = New-Object System.Collections.Generic.List[string]
$env:LMFORGE_YES = "1"

function E2eStep {
    param([string]$Name, [scriptblock]$Action)
    Write-Host ""
    Write-Host "=== $Name ===" -ForegroundColor Cyan
    try {
        & $Action
        if ($? -eq $false) { throw "step returned failure" }
        $E2E_Results.Add("PASS  $Name")
        Write-Host "PASS  $Name" -ForegroundColor Green
    } catch {
        $E2E_Results.Add("FAIL  $Name  $($_.Exception.Message)")
        Write-Host "FAIL  $Name  $($_.Exception.Message)" -ForegroundColor Red
    }
}

# Returns $true when every step passed.
function E2eSummary {
    Write-Host ""
    Write-Host "========== SUMMARY ==========" -ForegroundColor White
    $fail = 0
    foreach ($line in $E2E_Results) {
        if ($line.StartsWith("FAIL")) { $fail++; Write-Host $line -ForegroundColor Red }
        else { Write-Host $line -ForegroundColor Green }
    }
    Write-Host ""
    return ($fail -eq 0)
}

# Run installer/uninstaller in a child pwsh so their `exit` cannot kill us.
function Invoke-LmforgeScript {
    param([string]$Name)
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $E2E_RepoRoot "scripts\$Name")
    if ($LASTEXITCODE -ne 0) { throw "$Name exited $LASTEXITCODE" }
}

# ── Release-asset verification (release source only) ─────────────────────────
function E2eReleaseScriptsMatch {
    $tag = $env:LMFORGE_VERSION
    foreach ($n in @("install-core.ps1", "install-ui.ps1", "uninstall-core.ps1", "uninstall-ui.ps1")) {
        $url = "https://github.com/$E2E_Repo/releases/download/$tag/$n"
        $tmp = Join-Path $env:TEMP "lf-release-$n"
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
        $local = Join-Path $E2E_RepoRoot "scripts\$n"
        $relNorm = (Get-Content $tmp -Raw) -replace "`r`n", "`n"
        $locNorm = (Get-Content $local -Raw) -replace "`r`n", "`n"
        if ($relNorm -ne $locNorm) { throw "$n content mismatch (release vs repo at $tag)" }
        Write-Host "$n matches repo"
    }
}

function E2eReleaseCoreBinary {
    $tag = $env:LMFORGE_VERSION
    $url = "https://github.com/$E2E_Repo/releases/download/$tag/$E2E_CoreAsset"
    $r = Invoke-WebRequest -Uri $url -Method Head -UseBasicParsing
    if ($r.StatusCode -ne 200) { throw "binary HEAD failed" }
    $len = [int64]($r.Headers["Content-Length"] | Select-Object -First 1)
    if ($len -lt 1MB) { throw "binary too small ($len bytes)" }
    Write-Host "ok ($len bytes) $url"
}

# ── Build from current source (local install only) ──────────────────────────
# rustup installs cargo under %USERPROFILE%\.cargo\bin; a shell that didn't pick
# up the installer's PATH edit dies with "cargo not recognized". Add it ourselves
# so the harness runs out of the box.
function E2eEnsureCargo {
    if (Get-Command cargo -EA SilentlyContinue) { Write-Host "cargo resolved at $((Get-Command cargo).Source)"; return }
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path $cargoBin) { $env:PATH = "$cargoBin;$env:PATH" }
    if (-not (Get-Command cargo -EA SilentlyContinue)) {
        Write-Host "cargo not found - the Rust toolchain is not installed. Install rustup (Windows):" -ForegroundColor Yellow
        Write-Host "    winget install Rustlang.Rustup        # or download rustup-init.exe from https://rustup.rs"
        Write-Host "    (open a new shell afterwards so PATH picks up %USERPROFILE%\.cargo\bin)"
        throw "cargo not found - install rustup, then re-run"
    }
    Write-Host "cargo resolved at $((Get-Command cargo).Source)"
}

function E2eBuildLocal {
    E2eEnsureCargo
    Push-Location $E2E_RepoRoot
    try {
        & cargo build --release --bin lmforge
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally { Pop-Location }
    $b = Join-Path $E2E_RepoRoot "target\release\lmforge.exe"
    if (-not (Test-Path $b)) { throw "build produced no binary at $b" }
    $env:LMFORGE_LOCAL_BIN = $b
    Write-Host "built $((& $b --version 2>&1 | Out-String).Trim())"
}

# ── Install lifecycle ────────────────────────────────────────────────────────
# Light preclean: remove any prior install but KEEP data/models (CI gate uses
# this). Uninstallers run unconditionally; without LMFORGE_PURGE they leave
# models intact while still clearing the binary/autostart/PATH/engine leftovers.
function E2ePreclean {
    $env:LMFORGE_YES = "1"
    E2eKillEngines
    try { Invoke-LmforgeScript "uninstall-ui.ps1" } catch {}
    try { Invoke-LmforgeScript "uninstall-core.ps1" } catch {}
    E2eKillEngines
}

# Stop the daemon + ALL engine subprocesses (llama-server, …). Orphaned engine
# children survive a crashed/aborted run and keep VRAM + DLL handles; if left
# running they exhaust the GPU and a dying daemon can hold port 11430, breaking
# the next install's daemon start.
function E2eKillEngines {
    Get-Process -Name "lmforge", "lmforge-ui", "llama-server" -EA SilentlyContinue |
        Stop-Process -Force -EA SilentlyContinue
    try {
        $root = Join-Path $env:USERPROFILE ".lmforge"
        Get-CimInstance Win32_Process -EA SilentlyContinue |
            Where-Object { $_.ExecutablePath -and $_.ExecutablePath -like "$root\*" } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -EA SilentlyContinue }
    } catch {}
    Start-Sleep -Seconds 1
}

# Full clean slate: stop everything, remove any install AND all data. The
# uninstallers run unconditionally (NOT gated on the binary/UI existing) — they
# are safe when nothing is installed and are exactly what clears binary-absent
# leftovers (autostart Run key, user PATH entry, engines dir, app data).
function E2eFullClean {
    $env:LMFORGE_YES = "1"
    E2eKillEngines
    try { Invoke-LmforgeScript "uninstall-ui.ps1" } catch {}
    $env:LMFORGE_PURGE = "1"
    try { Invoke-LmforgeScript "uninstall-core.ps1" } catch {}
    Remove-Item Env:\LMFORGE_PURGE -EA SilentlyContinue
    E2eKillEngines
    $data = Join-Path $env:USERPROFILE ".lmforge"
    if (Test-Path $data) {
        for ($i = 0; $i -lt 5; $i++) {
            Remove-Item -LiteralPath $data -Recurse -Force -EA SilentlyContinue
            if (-not (Test-Path $data)) { break }
            Start-Sleep -Milliseconds 600
        }
    }
    if (Test-Path "$data\models") { Write-Host "  [!] $data\models still present (locked?)" -ForegroundColor Yellow }
    Write-Host "clean slate - install + data removed"
}

function E2eInstallCore { Invoke-LmforgeScript "install-core.ps1" }

function E2eBinaryInstalled {
    if (-not (Test-Path $E2E_Bin)) { throw "missing $E2E_Bin" }
    & $E2E_Bin --version
}

function E2eCoreVersionMatches {
    if (-not (Test-Path $E2E_Bin)) { throw "missing $E2E_Bin" }
    $v = (& $E2E_Bin --version 2>&1 | Out-String).Trim()
    Write-Host $v
    if (-not $env:LMFORGE_VERSION) { return }
    $want = $env:LMFORGE_VERSION.TrimStart("v")
    if ($v -notmatch [regex]::Escape($want)) { throw "expected $want" }
}

function E2eHealthOk {
    $body = (Invoke-WebRequest "$E2E_Api/health" -UseBasicParsing -TimeoutSec 20).Content
    Write-Host $body
    if ($body -notmatch '"status"\s*:\s*"ok"') { throw $body }
}

function E2eSysinfoOk {
    $json = (Invoke-WebRequest "$E2E_Api/lf/sysinfo" -UseBasicParsing -TimeoutSec 15).Content | ConvertFrom-Json
    if ($null -eq $json.cpu_pct) { throw "no cpu_pct" }
    Write-Host "sysinfo ok (cpu_pct=$($json.cpu_pct))"
}

function E2eServiceStatusOk {
    $out = & $E2E_Bin service status 2>&1 | Out-String
    Write-Host $out
    if ($out -notmatch "reachable") { throw "daemon not reachable per service status" }
}

function E2eAutostartRegistered {
    $val = Get-ItemProperty -Path $E2E_RunKey -Name "LMForge" -ErrorAction SilentlyContinue
    if (-not $val) { throw "Run key value 'LMForge' not registered" }
    if ($val.LMForge -notmatch "wscript") { throw "Run key does not use wscript: $($val.LMForge)" }
    Write-Host "Run key: $($val.LMForge)"
    if (-not (Test-Path $E2E_Vbs)) { throw "missing launcher $E2E_Vbs" }
}

# ── UI install lifecycle ─────────────────────────────────────────────────────
function E2eInstallUi { Invoke-LmforgeScript "install-ui.ps1" }

# Build the UI from current source and install it (local pre-release path).
function E2eInstallUiLocal {
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $E2E_RepoRoot "scripts\util\build-ui-local.ps1")
    if ($LASTEXITCODE -ne 0) { throw "build-ui-local.ps1 exited $LASTEXITCODE" }
}

function E2eUiInstalled {
    if (-not (Test-Path $E2E_UiExe)) { throw "UI not installed at $E2E_UiExe" }
    Write-Host "UI present: $E2E_UiExe"
}

# ── Teardown ─────────────────────────────────────────────────────────────────
function E2eUninstallUi {
    Get-Process lmforge-ui -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep 2
    Invoke-LmforgeScript "uninstall-ui.ps1"
    if (Test-Path $E2E_UiExe) { throw "$E2E_UiExe still exists" }
    Write-Host "UI removed"
}

# Honours $env:E2E_PURGE = "1" to also delete %USERPROFILE%\.lmforge (models + config).
function E2eUninstallCore {
    if ($env:E2E_PURGE -eq "1") { $env:LMFORGE_PURGE = "1" }
    Invoke-LmforgeScript "uninstall-core.ps1"
    Remove-Item Env:\LMFORGE_PURGE -EA SilentlyContinue
}

function E2eBinaryRemoved {
    if (Test-Path $E2E_Bin) { throw "$E2E_Bin still exists" }
    Write-Host "binary removed"
}

function E2eDataRemoved {
    $models = Join-Path $env:USERPROFILE ".lmforge\models"
    if (Test-Path $models) { throw "models still present at $models" }
    Write-Host "data/models removed"
}

function E2eDaemonDown {
    Start-Sleep 2
    $up = $false
    try { Invoke-WebRequest "$E2E_Api/health" -UseBasicParsing -TimeoutSec 2 | Out-Null; $up = $true } catch {}
    if ($up) { throw "daemon still reachable after uninstall" }
    Write-Host "daemon down"
}

function E2eAutostartRemoved {
    $val = Get-ItemProperty -Path $E2E_RunKey -Name "LMForge" -ErrorAction SilentlyContinue
    if ($val) { throw "Run key value still present: $($val.LMForge)" }
    if (Test-Path $E2E_Vbs) { throw "launcher still present: $E2E_Vbs" }
    Write-Host "autostart artifacts removed"
}

# ── Engine preflight ─────────────────────────────────────────────────────────
# Run the ACTIVE engine's binary directly (not via the daemon) so a broken
# install fails fast with remediation guidance instead of an opaque 503 deep in
# TC-E01 (e.g. a half-extracted llama-server.exe missing its CUDA DLLs).
function E2eEnginePreflight {
    $engine = $null
    try {
        $engine = ((Invoke-RestMethod "$E2E_Api/lf/engines" -TimeoutSec 5).engines |
            Where-Object { $_.active } | Select-Object -First 1).id
    } catch {}
    if (-not $engine) { throw "could not read active engine from $E2E_Api/lf/engines (daemon up?)" }
    Write-Host "active engine: $engine"

    $bin = $null
    switch ($engine) {
        "llamacpp" {
            $cmd = Get-Command llama-server -EA SilentlyContinue
            if ($cmd) { $bin = $cmd.Source }
            else {
                # Search the whole engines tree so both the variant-aware layout
                # (engines\llamacpp\variants\<id>\) and the legacy flat layout
                # (engines\llama-server.exe) are covered.
                $root = Join-Path $env:USERPROFILE ".lmforge\engines"
                $bin = Get-ChildItem $root -Recurse -Filter "llama-server.exe" -EA SilentlyContinue |
                    Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
            }
            if (-not $bin) { throw "llama-server.exe not found - reinstall: lmforge engine install llamacpp" }
        }
        default { Write-Host "no preflight defined for engine '$engine' - skipped"; return }
    }

    $out = (& $bin --version 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        Write-Host "engine binary BROKEN: $bin" -ForegroundColor Red
        Write-Host $out.Trim()
        throw "engine binary failed to run - fix: lmforge engine install llamacpp"
    }
    Write-Host "engine binary OK: $bin"
    Write-Host (($out -split "`n")[0].Trim())
}

# ── Inference (delegates to the shared multi-model suite) ────────────────────
function E2eInference {
    $env:SKIP_START = "1"; $env:SKIP_BUILD = "1"; $env:LF_BIN = $E2E_Bin
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $E2E_RepoRoot "tests\multi_model_e2e.ps1")
    if ($LASTEXITCODE -ne 0) { throw "multi_model_e2e.ps1 exited $LASTEXITCODE" }
}
