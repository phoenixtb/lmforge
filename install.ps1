# =============================================================================
# LMForge — Windows PowerShell Installer
# Usage (run in PowerShell as your user):
#   irm https://raw.githubusercontent.com/phoenixtb/lmforge/main/install.ps1 | iex
# =============================================================================
$ErrorActionPreference = "Stop"

# ── Helpers ───────────────────────────────────────────────────────────────────
function Info    { param($m) Write-Host "  [*] $m" -ForegroundColor Cyan }
function Success { param($m) Write-Host "  [+] $m" -ForegroundColor Green }
function Warn    { param($m) Write-Host "  [!] $m" -ForegroundColor Yellow }
function Err     { param($m) Write-Host "  [x] $m" -ForegroundColor Red; exit 1 }

Write-Host ""
Write-Host "  LMForge — Hardware-aware LLM inference orchestrator" -ForegroundColor Cyan
Write-Host ""

# ── Config ────────────────────────────────────────────────────────────────────
$Repo      = "phoenixtb/lmforge"
$Binary    = "lmforge.exe"
$Target    = "x86_64-pc-windows-msvc"
$InstallDir = "$env:LOCALAPPDATA\lmforge\bin"

# ── Fetch latest release ──────────────────────────────────────────────────────
Info "Fetching latest release..."
try {
    $ApiUrl  = "https://api.github.com/repos/$Repo/releases/latest"
    $Headers = @{ "User-Agent" = "lmforge-installer" }
    $Release = Invoke-RestMethod -Uri $ApiUrl -Headers $Headers
    $Latest  = $Release.tag_name
    Info "Latest release: $Latest"
} catch {
    Warn "Could not fetch latest release. Will attempt source install."
    $Latest = $null
}

# ── Try pre-built binary ──────────────────────────────────────────────────────
$Installed = $false

if ($Latest) {
    $Tarball    = "lmforge-$Target.tar.gz"
    $DownloadUrl = "https://github.com/$Repo/releases/download/$Latest/$Tarball"
    $TmpDir     = [System.IO.Path]::GetTempPath() + [System.Guid]::NewGuid().ToString()
    New-Item -ItemType Directory -Path $TmpDir | Out-Null

    Info "Downloading $Tarball..."
    try {
        Invoke-WebRequest -Uri $DownloadUrl -OutFile "$TmpDir\$Tarball" -UseBasicParsing

        # Extract (requires tar, available on Windows 10 1803+)
        tar -xzf "$TmpDir\$Tarball" -C $TmpDir

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Copy-Item "$TmpDir\$Binary" "$InstallDir\$Binary" -Force

        Success "Binary installed to $InstallDir\$Binary"
        $Installed = $true
    } catch {
        Warn "Pre-built binary not available. Falling back to source build..."
    } finally {
        Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    }
}

# ── Fallback: build from source ───────────────────────────────────────────────
if (-not $Installed) {
    Info "Checking for Rust toolchain..."

    if (-not (Get-Command "cargo" -ErrorAction SilentlyContinue)) {
        Info "Rust not found. Installing via rustup..."
        $RustupUrl = "https://win.rustup.rs/x86_64"
        $RustupExe = "$env:TEMP\rustup-init.exe"
        Invoke-WebRequest -Uri $RustupUrl -OutFile $RustupExe -UseBasicParsing
        Start-Process -FilePath $RustupExe -ArgumentList "-y", "--no-modify-path" -Wait
        $env:PATH += ";$env:USERPROFILE\.cargo\bin"
    }

    # Clone or use local source
    if (Test-Path ".\Cargo.toml") {
        Info "Building from local source..."
        cargo build --release
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Copy-Item ".\target\release\$Binary" "$InstallDir\$Binary" -Force
    } else {
        $TmpDir = "$env:TEMP\lmforge-src-$(Get-Random)"
        Info "Cloning repository..."
        git clone --depth 1 "https://github.com/$Repo.git" $TmpDir
        Info "Building from source (this may take 2-3 minutes)..."
        Push-Location $TmpDir
        cargo build --release
        Pop-Location
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Copy-Item "$TmpDir\target\release\$Binary" "$InstallDir\$Binary" -Force
        Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
    }

    Success "Built and installed to $InstallDir\$Binary"
    $Installed = $true
}

# ── Post-install: data directories ───────────────────────────────────────────
Info "Creating LMForge data directories..."
$DataDir = "$env:USERPROFILE\.lmforge"
@("models", "engines", "logs") | ForEach-Object {
    New-Item -ItemType Directory -Path "$DataDir\$_" -Force | Out-Null
}

# ── PATH update ───────────────────────────────────────────────────────────────
$UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [System.Environment]::SetEnvironmentVariable(
        "PATH",
        "$UserPath;$InstallDir",
        "User"
    )
    $env:PATH += ";$InstallDir"
    Success "Added $InstallDir to your user PATH (restart terminal to take effect)"
}

# ── Done ──────────────────────────────────────────────────────────────────────
Write-Host ""
Success "LMForge $($Latest ?? 'dev') installed successfully!"
Write-Host ""
Write-Host "  Next steps:" -ForegroundColor White
Write-Host "    lmforge init           # detect hardware and create config"
Write-Host "    lmforge pull qwen3-8b  # download your first model"
Write-Host "    lmforge start          # start the inference server"
Write-Host "    lmforge run qwen3-8b   # start interactive chat"
Write-Host ""
