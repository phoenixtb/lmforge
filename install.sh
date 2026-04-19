#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# LMForge — One-Command Installer
# Usage: curl -sSf https://raw.githubusercontent.com/phoenixtb/lmforge/main/install.sh | sh
# =============================================================================

REPO="phoenixtb/lmforge"
BINARY="lmforge"
INSTALL_DIR="${LMFORGE_INSTALL_DIR:-$HOME/.local/bin}"

# ── Print helpers ──────────────────────────────────────────────────────────────
info()    { printf "  \033[34m⚙\033[0m %s\n" "$*"; }
success() { printf "  \033[32m✓\033[0m %s\n" "$*"; }
warn()    { printf "  \033[33m⚠\033[0m %s\n" "$*"; }
err()     { printf "  \033[31m✗\033[0m %s\n" "$*" >&2; exit 1; }

echo ""
echo "  ██╗     ███╗   ███╗███████╗ ██████╗ ██████╗  ██████╗ ███████╗"
echo "  ██║     ████╗ ████║██╔════╝██╔═══██╗██╔══██╗██╔════╝ ██╔════╝"
echo "  ██║     ██╔████╔██║█████╗  ██║   ██║██████╔╝██║  ███╗█████╗  "
echo "  ██║     ██║╚██╔╝██║██╔══╝  ██║   ██║██╔══██╗██║   ██║██╔══╝  "
echo "  ███████╗██║ ╚═╝ ██║██║     ╚██████╔╝██║  ██║╚██████╔╝███████╗"
echo "  ╚══════╝╚═╝     ╚═╝╚═╝      ╚═════╝ ╚═╝  ╚═╝ ╚═════╝ ╚══════╝"
echo ""
echo "  Hardware-aware LLM inference orchestrator"
echo ""

# ── Platform detection ─────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) OS_NAME="apple-darwin" ;;
    Linux)  OS_NAME="unknown-linux-gnu" ;;
    *) err "Unsupported OS: $OS. LMForge supports macOS and Linux." ;;
esac

case "$ARCH" in
    arm64|aarch64) ARCH_NAME="aarch64" ;;
    x86_64|amd64)  ARCH_NAME="x86_64" ;;
    *) err "Unsupported architecture: $ARCH." ;;
esac

TARGET="${ARCH_NAME}-${OS_NAME}"
info "Detected platform: ${OS} / ${ARCH} → ${TARGET}"

# ── Fetch latest release tag from GitHub ──────────────────────────────────────
info "Fetching latest release..."
if command -v curl &>/dev/null; then
    LATEST=$(curl -sSf "https://api.github.com/repos/${REPO}/releases/latest" \
             | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
elif command -v wget &>/dev/null; then
    LATEST=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
             | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
else
    err "Neither curl nor wget found. Please install one and retry."
fi

if [ -z "$LATEST" ]; then
    warn "Could not determine latest release. Falling back to source install."
    INSTALL_MODE="source"
else
    info "Latest release: ${LATEST}"
    INSTALL_MODE="binary"
fi

# ── Try pre-built binary first ─────────────────────────────────────────────────
install_binary() {
    TARBALL="${BINARY}-${TARGET}.tar.gz"
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST}/${TARBALL}"

    info "Downloading ${TARBALL}..."
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT

    if command -v curl &>/dev/null; then
        curl -sSfL "$DOWNLOAD_URL" -o "$TMP_DIR/$TARBALL" || return 1
    else
        wget -qO "$TMP_DIR/$TARBALL" "$DOWNLOAD_URL" || return 1
    fi

    tar -xzf "$TMP_DIR/$TARBALL" -C "$TMP_DIR"

    mkdir -p "$INSTALL_DIR"
    install -m 755 "$TMP_DIR/$BINARY" "$INSTALL_DIR/$BINARY"
    success "Binary installed to ${INSTALL_DIR}/${BINARY}"
}

# ── Fallback: build from source ────────────────────────────────────────────────
install_from_source() {
    if ! command -v cargo &>/dev/null; then
        info "Rust toolchain not found. Installing rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    fi

    # Check if we're already inside the repo
    if [ -f "./Cargo.toml" ] && grep -q 'name = "lmforge"' ./Cargo.toml 2>/dev/null; then
        info "Building from local source..."
        cargo build --release
        install -m 755 ./target/release/"$BINARY" "$INSTALL_DIR/$BINARY"
    else
        info "Cloning repository..."
        TMP_DIR="$(mktemp -d)"
        trap 'rm -rf "$TMP_DIR"' EXIT
        git clone --depth 1 "https://github.com/${REPO}.git" "$TMP_DIR"
        info "Building from source (this may take 1–2 minutes)..."
        cargo build --release --manifest-path "$TMP_DIR/Cargo.toml"
        install -m 755 "$TMP_DIR/target/release/$BINARY" "$INSTALL_DIR/$BINARY"
    fi

    success "Built and installed to ${INSTALL_DIR}/${BINARY}"
}

# ── Install ────────────────────────────────────────────────────────────────────
if [ "$INSTALL_MODE" = "binary" ]; then
    if ! install_binary; then
        warn "Pre-built binary not available for ${TARGET}. Falling back to source build..."
        install_from_source
    fi
else
    install_from_source
fi

# ── Linux: install UI tray dependency ──────────────────────────────────────────
# The LMForge system tray icon requires libayatana-appindicator3-1 on Linux.
# We install it at install-time so the first launch works correctly.
# If the package manager is not recognised we warn but do NOT abort (the CLI
# works fine without the tray; the UI falls back to window-only mode).
if [ "$OS" = "Linux" ]; then
    TRAY_PKG="libayatana-appindicator3-1"
    info "Checking for Linux tray dependency (${TRAY_PKG})..."

    if dpkg -s "$TRAY_PKG" &>/dev/null 2>&1; then
        success "${TRAY_PKG} is already installed."
    elif command -v apt-get &>/dev/null; then
        info "Installing ${TRAY_PKG} via apt-get..."
        if sudo apt-get install -y "$TRAY_PKG" 2>/dev/null; then
            success "${TRAY_PKG} installed."
        else
            warn "Could not install ${TRAY_PKG}. UI system tray will be unavailable — window-only mode."
        fi
    elif command -v dnf &>/dev/null; then
        info "Installing libayatana-appindicator3 via dnf..."
        if sudo dnf install -y libayatana-appindicator3 2>/dev/null; then
            success "libayatana-appindicator3 installed."
        else
            warn "Could not install tray dependency. UI will use window-only mode."
        fi
    elif command -v pacman &>/dev/null; then
        info "Installing libayatana-appindicator via pacman..."
        if sudo pacman -S --noconfirm libayatana-appindicator 2>/dev/null; then
            success "libayatana-appindicator installed."
        else
            warn "Could not install tray dependency. UI will use window-only mode."
        fi
    else
        warn "Unknown package manager. Install '${TRAY_PKG}' manually for system tray support."
        warn "The UI will start in window-only mode until the library is present."
    fi
fi

# ── Linux: GPU probe advisory ──────────────────────────────────────────────────
# LMForge uses nvidia-smi / rocm-smi to display live GPU stats in the UI and
# to make VRAM-aware model scheduling decisions.
#
# These tools ship with GPU driver packages, NOT with LMForge itself.
# We NEVER install GPU drivers automatically — that is too invasive and can
# break an existing driver stack. Instead we:
#   • Detect the GPU vendor via lspci (if available)
#   • Check whether the management CLI is in PATH
#   • If missing, install the lightweight *utils* package (not the full driver)
#     on supported distros, or print clear manual instructions otherwise.
if [ "$OS" = "Linux" ]; then
    echo ""
    info "Checking GPU tooling..."

    # Detect GPU vendor from PCI device list (best-effort — lspci may not exist)
    HAS_NVIDIA=0
    HAS_AMD=0
    if command -v lspci &>/dev/null; then
        lspci_out="$(lspci 2>/dev/null)"
        echo "$lspci_out" | grep -qi "nvidia"                                  && HAS_NVIDIA=1
        echo "$lspci_out" | grep -qi "\(amd\|radeon\|advanced micro devices\)" && HAS_AMD=1
    fi

    # ── NVIDIA ────────────────────────────────────────────────────────────────
    if [ "$HAS_NVIDIA" = "1" ]; then
        info "NVIDIA GPU detected."
        if command -v nvidia-smi &>/dev/null; then
            success "nvidia-smi found — GPU stats will be available."
        else
            warn "NVIDIA GPU detected but nvidia-smi is not in PATH."
            warn "Without it LMForge cannot display GPU utilisation or make VRAM-aware scheduling decisions."
            echo ""
            echo "  LMForge will NOT install GPU drivers automatically."
            echo "  To enable GPU stats, install the NVIDIA utilities package for your distro:"
            echo ""

            INSTALLED_NVIDIA_UTILS=0
            if command -v apt-get &>/dev/null; then
                # Find the highest available nvidia-utils-<ver> for the installed driver.
                NVIDIA_VER="$(apt-cache search nvidia-utils 2>/dev/null \
                    | grep -oP 'nvidia-utils-\K[0-9]+' | sort -rn | head -1)"
                if [ -n "$NVIDIA_VER" ]; then
                    NVIDIA_UTILS_PKG="nvidia-utils-${NVIDIA_VER}"
                    echo "    Install (Ubuntu/Debian):  sudo apt-get install -y ${NVIDIA_UTILS_PKG}"
                    echo ""
                    printf "  Install %s now? [y/N] " "$NVIDIA_UTILS_PKG"
                    read -r REPLY </dev/tty
                    if [ "$REPLY" = "y" ] || [ "$REPLY" = "Y" ]; then
                        if sudo apt-get install -y "$NVIDIA_UTILS_PKG" 2>/dev/null; then
                            success "nvidia-smi installed via ${NVIDIA_UTILS_PKG}."
                            INSTALLED_NVIDIA_UTILS=1
                        else
                            warn "apt-get install failed. Please install manually."
                        fi
                    fi
                else
                    echo "    sudo apt-get install nvidia-utils-<version>"
                    echo "    (replace <version> with your driver series, e.g. 535, 550, 570)"
                fi
            elif command -v dnf &>/dev/null; then
                echo "    Install (Fedora/RHEL):  sudo dnf install -y akmod-nvidia xorg-x11-drv-nvidia-cuda-libs"
                echo "    RPM Fusion guide:       https://rpmfusion.org/Howto/NVIDIA"
            elif command -v pacman &>/dev/null; then
                echo "    Install (Arch):         sudo pacman -S nvidia-utils"
            else
                echo "    See: https://www.nvidia.com/Download/index.aspx"
            fi

            if [ "$INSTALLED_NVIDIA_UTILS" = "0" ]; then
                echo ""
                warn "GPU stats will be unavailable until nvidia-smi is installed."
                warn "You can install it later and restart LMForge — no reinstall needed."
            fi
        fi

    # ── AMD ───────────────────────────────────────────────────────────────────
    elif [ "$HAS_AMD" = "1" ]; then
        info "AMD GPU detected."
        if command -v rocm-smi &>/dev/null; then
            success "rocm-smi found — GPU stats will be available."
        else
            warn "AMD GPU detected but rocm-smi is not in PATH."
            warn "Without it LMForge cannot display GPU utilisation or make VRAM-aware scheduling decisions."
            echo ""
            echo "  To enable GPU stats, install ROCm SMI for your distro:"
            echo ""
            if command -v apt-get &>/dev/null; then
                echo "    sudo apt-get install -y rocm-smi-lib"
            elif command -v dnf &>/dev/null; then
                echo "    sudo dnf install -y rocm-smi"
            elif command -v pacman &>/dev/null; then
                echo "    sudo pacman -S rocm-smi-lib"
            fi
            echo "    Full guide: https://rocm.docs.amd.com/en/latest/deploy/linux/quick_start.html"
            echo ""
            warn "GPU stats will be unavailable until rocm-smi is installed."
            warn "You can install it later and restart LMForge — no reinstall needed."
        fi

    # ── No discrete GPU / lspci unavailable ───────────────────────────────────
    else
        if command -v lspci &>/dev/null; then
            info "No discrete NVIDIA/AMD GPU detected — LMForge will use CPU inference."
        else
            info "lspci not available — cannot auto-detect GPU. Install 'pciutils' for GPU detection."
            info "If you have a GPU, install nvidia-smi or rocm-smi manually to enable GPU stats."
        fi
    fi
fi


# ── Post-install setup ─────────────────────────────────────────────────────────
info "Creating LMForge data directories..."
mkdir -p "$HOME/.lmforge/models"
mkdir -p "$HOME/.lmforge/engines"
mkdir -p "$HOME/.lmforge/logs"

# ── PATH check ────────────────────────────────────────────────────────────────
if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
    echo ""
    warn "${INSTALL_DIR} is not in your PATH."
    echo ""
    echo "  Add this to your shell config (~/.zshrc, ~/.bashrc, etc.):"
    echo ""
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
fi

# ── Done ───────────────────────────────────────────────────────────────────────
echo ""
success "LMForge ${LATEST:-dev} installed successfully!"
echo ""
echo "  Next steps:"
echo "    lmforge init          — detect hardware and create config"
echo "    lmforge pull qwen3-8b — download your first model"
echo "    lmforge start         — start the inference server"
echo "    lmforge run qwen3-8b  — start an interactive chat session"
echo ""
