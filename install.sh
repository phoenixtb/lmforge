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
