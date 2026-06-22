#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  LMForge UI — Install Script
#  Downloads the LMForge desktop app from GitHub Releases and installs it.
#  Requires LMForge Core to be installed first.
#
#  Usage:
#    curl -fsSL https://github.com/phoenixtb/lmforge/releases/latest/download/install-ui.sh | bash
#
#  Environment variables:
#    LMFORGE_VERSION   Pin a specific version, e.g. "v0.3.1" (default: latest)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="phoenixtb/lmforge"
LMFORGE_RELEASE="${LMFORGE_VERSION:-latest}"
MIN_CORE_VERSION="0.1.0"

# macOS — install to user Applications (no sudo required)
APP_NAME="LMForge"
APP_BUNDLE="${HOME}/Applications/${APP_NAME}.app"

# ── Colours ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }
section() { echo -e "\n${BOLD}$*${NC}"; }

# shellcheck source=banner.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/banner.sh" 2>/dev/null || true
if ! declare -F print_lmforge_banner &>/dev/null; then
    print_lmforge_banner() {
        echo ""
        echo "  ██╗     ███╗   ███╗███████╗ ██████╗ ██████╗  ██████╗ ███████╗"
        echo "  ██║     ████╗ ████║██╔════╝██╔═══██╗██╔══██╗██╔════╝ ██╔════╝"
        echo "  ██║     ██╔████╔██║█████╗  ██║   ██║██████╔╝██║  ███╗█████╗  "
        echo "  ██║     ██║╚██╔╝██║██╔══╝  ██║   ██║██╔══██╗██║   ██║██╔══╝  "
        echo "  ███████╗██║ ╚═╝ ██║██║     ╚██████╔╝██║  ██║╚██████╔╝███████╗"
        echo "  ╚══════╝╚═╝     ╚═╝╚═╝      ╚═════╝ ╚═╝  ╚═╝ ╚═════╝ ╚══════╝"
        echo ""
        echo "  ${1:-Hardware-aware LLM inference orchestrator}"
        echo ""
    }
fi

# Install hicolor icons + .desktop entry for the AppImage (matches Tauri bundle).
install_linux_ui_launcher() {
    local appimage_path="$1"
    local desktop_dir="${HOME}/.local/share/applications"
    local icon_theme="${HOME}/.local/share/icons/hicolor"
    local extract_dir icon_src size name

    extract_dir=$(mktemp -d)
    (
        cd "$extract_dir"
        "$appimage_path" --appimage-extract usr/share/icons >/dev/null 2>&1 \
            || "$appimage_path" --appimage-extract >/dev/null 2>&1
    )

    if [[ -d "$extract_dir/squashfs-root/usr/share/icons/hicolor" ]]; then
        while IFS= read -r -d '' icon_src; do
            size=$(basename "$(dirname "$(dirname "$icon_src")")")
            name=$(basename "$icon_src")
            mkdir -p "$icon_theme/$size/apps"
            cp "$icon_src" "$icon_theme/$size/apps/$name"
        done < <(find "$extract_dir/squashfs-root/usr/share/icons/hicolor" -path '*/apps/*.png' -print0)
        if command -v gtk-update-icon-cache &>/dev/null; then
            gtk-update-icon-cache -f -t "$icon_theme" 2>/dev/null || true
        fi
        info "Icons installed to $icon_theme"
    else
        warn "Could not extract icons from AppImage; launcher may show a generic icon."
    fi
    rm -rf "$extract_dir"

    # Without libfuse2 the AppImage can't self-mount, so the launcher must run
    # it in extract-and-run mode for the desktop entry to work.
    local exec_line="$appimage_path %u"
    if ! ldconfig -p 2>/dev/null | grep -q "libfuse.so.2"; then
        exec_line="env APPIMAGE_EXTRACT_AND_RUN=1 $appimage_path %u"
    fi

    mkdir -p "$desktop_dir"
    cat > "$desktop_dir/lmforge.desktop" <<EOF
[Desktop Entry]
Name=LMForge
Comment=LMForge — Local LLM Orchestrator UI
Exec=$exec_line
Icon=lmforge-ui
StartupWMClass=lmforge-ui
Terminal=false
Type=Application
Categories=Development;AI;
EOF
    update-desktop-database "$desktop_dir" 2>/dev/null || true
    info "Desktop entry created"
}

# ── Detect platform ───────────────────────────────────────────────────────────
OS=$(uname -s)
ARCH=$(uname -m)

# Native Linux package facts (must match what Tauri's deb/rpm bundles install).
LINUX_PKG_NAME="lm-forge"                          # rpm/deb package name
LINUX_NATIVE_BIN="/usr/bin/lmforge-ui"             # binary installed by deb/rpm
LINUX_NATIVE_DESKTOP="/usr/share/applications/LMForge.desktop"

# Root vs sudo. Native package install/removal needs root; user-local AppImage
# does not. Resolved once so every code path uses the same escalation.
if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then SUDO=""; else SUDO="sudo"; fi

# Choose the Linux UI distribution format for this host:
#   rpm  → Fedora/RHEL/SUSE families (dnf/zypper resolve webkit deps natively)
#   deb  → Debian/Ubuntu families    (apt resolves webkit deps natively)
#   appimage → portable fallback for everything else (or when the native package
#              manager is missing). AppImage needs libfuse2 at runtime, which is
#              why native packages are preferred where available.
detect_linux_pkg_kind() {
    local id="" like=""
    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        id="${ID:-}"; like="${ID_LIKE:-}"
    fi
    case "$id" in
        fedora|rhel|centos|rocky|almalinux|ol|amzn|opensuse*|suse|sles)
            command -v dnf &>/dev/null || command -v zypper &>/dev/null || command -v rpm &>/dev/null \
                && { echo rpm; return; } ;;
        ubuntu|debian|pop|linuxmint|elementary|zorin|kali|raspbian|neon)
            command -v apt-get &>/dev/null || command -v dpkg &>/dev/null \
                && { echo deb; return; } ;;
    esac
    case " $like " in
        *rhel*|*fedora*|*suse*)   command -v dnf &>/dev/null   && { echo rpm; return; } ;;
        *debian*|*ubuntu*)        command -v apt-get &>/dev/null && { echo deb; return; } ;;
    esac
    echo appimage
}

LINUX_PKG_KIND=""
[[ "$OS" == "Linux" ]] && LINUX_PKG_KIND=$(detect_linux_pkg_kind)

detect_ui_asset() {
    case "$OS" in
        Darwin)
            case "$ARCH" in
                arm64)   echo "LMForge-UI-macos-arm64.dmg" ;;
                x86_64)  echo "LMForge-UI-macos-x86_64.dmg" ;;
                *)        error "Unsupported macOS arch: $ARCH" ;;
            esac ;;
        Linux)
            case "$ARCH" in
                x86_64)
                    case "$LINUX_PKG_KIND" in
                        rpm) echo "LMForge-UI-linux-x86_64.rpm" ;;
                        deb) echo "LMForge-UI-linux-x86_64.deb" ;;
                        *)   echo "LMForge-UI-linux-x86_64.AppImage" ;;
                    esac ;;
                *)        error "Unsupported Linux arch: $ARCH. Only x86_64 is supported currently." ;;
            esac ;;
        *)
            error "Unsupported OS: $OS. For Windows, run:\n  irm https://github.com/$REPO/releases/latest/download/install-ui.ps1 | iex" ;;
    esac
}

resolve_url() {
    local asset="$1"
    if [[ "$LMFORGE_RELEASE" == "latest" ]]; then
        echo "https://github.com/${REPO}/releases/latest/download/${asset}"
    else
        echo "https://github.com/${REPO}/releases/download/${LMFORGE_RELEASE}/${asset}"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
print_lmforge_banner "LMForge UI — Installer"
echo    "  Repo   : https://github.com/$REPO"
echo    "  Version: $LMFORGE_RELEASE"
echo    "  OS/Arch: $OS/$ARCH"
echo ""

# ── Idempotency check ─────────────────────────────────────────────────────────
if [[ "$OS" == "Darwin" && -d "$APP_BUNDLE" ]]; then
    warn "LMForge.app already installed at $APP_BUNDLE"
    warn "To update, uninstall first: curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-ui.sh | bash"
    warn "Opening existing app..."
    open "$APP_BUNDLE" 2>/dev/null || true
    exit 0
fi

# ── Prerequisite: Core must be installed ─────────────────────────────────────
section "Checking LMForge Core..."

# Augment PATH with every location install-core.sh might have used,
# so this check works even in a fresh bash subprocess (curl | bash)
# where ~/.zshrc / ~/.bashrc has not been sourced.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"

if ! command -v lmforge &>/dev/null; then
    error "LMForge Core not found. Install it first:\n  curl -fsSL https://github.com/$REPO/releases/latest/download/install-core.sh | bash"
fi

CORE_VER=$(lmforge --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "0.0.0")
info "Core version: $CORE_VER"

# Semver check (major.minor)
CORE_MAJOR=$(echo "$CORE_VER"     | cut -d. -f1)
CORE_MINOR=$(echo "$CORE_VER"     | cut -d. -f2)
MIN_MAJOR=$(echo "$MIN_CORE_VERSION"  | cut -d. -f1)
MIN_MINOR=$(echo "$MIN_CORE_VERSION"  | cut -d. -f2)

if (( CORE_MAJOR < MIN_MAJOR )) || (( CORE_MAJOR == MIN_MAJOR && CORE_MINOR < MIN_MINOR )); then
    error "Core $CORE_VER is too old. UI requires >= $MIN_CORE_VERSION\nUpdate: curl -fsSL https://github.com/$REPO/releases/latest/download/install-core.sh | bash"
fi
info "Core $CORE_VER >= $MIN_CORE_VERSION (compatible)"

# Check daemon is running (it should be — installed by install-core.sh)
if curl -sf --max-time 3 http://127.0.0.1:11430/health >/dev/null 2>&1; then
    info "Daemon is running"
else
    warn "Daemon not currently running. Starting it now..."
    lmforge start || true
    sleep 2
fi

# ── Linux UI runtime deps (AppImage fallback only) ───────────────────────────
# Native deb/rpm packages declare webkit2gtk/appindicator as dependencies, so
# the system package manager resolves them automatically — this manual block is
# ONLY needed for the portable AppImage path, which bundles most libraries but
# cannot relocate the host's webkit2gtk HTML renderer.
# Package names differ by distro AND distro version (Tauri 2 needs webkit 4.1+,
# the 4.0 series was deprecated). We probe `/etc/os-release` and install
# conservatively, with explicit confirmation. Unknown distros: print
# instructions and continue (the AppImage may still launch if libs are
# pre-installed, or fail with a clear message).
if [[ "$OS" == "Linux" && "$LINUX_PKG_KIND" == "appimage" ]]; then
    section "Checking UI runtime dependencies..."

    # Default — overridden per distro below.
    LINUX_DEPS=()
    PKG_INSTALL_CMD=""

    DISTRO_ID=""
    DISTRO_VER=""
    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        DISTRO_ID="${ID:-}"
        DISTRO_VER="${VERSION_ID:-}"
    fi

    # Choose webkit + tray package names per distro/version.
    case "$DISTRO_ID" in
        ubuntu|debian|pop|linuxmint)
            # Map to webkit major version.
            # Ubuntu 22.04 = jammy (4.0, EOL for Tauri 2),
            # Ubuntu 24.04 = noble (4.1), Ubuntu 26.04 = ? (6.0),
            # Debian 12 = bookworm (4.1), Debian 13 = trixie (6.0/4.1).
            WEBKIT_PKG=""
            # First, query apt-cache to see what's actually available — this
            # is the most reliable signal across distro forks (Pop!_OS, Mint).
            if apt-cache show libwebkitgtk-6.0-0 &>/dev/null; then
                WEBKIT_PKG="libwebkitgtk-6.0-0"
            elif apt-cache show libwebkit2gtk-4.1-0 &>/dev/null; then
                WEBKIT_PKG="libwebkit2gtk-4.1-0"
            elif apt-cache show libwebkit2gtk-4.0-37 &>/dev/null; then
                # Ubuntu 22.04 fallback — Tauri 2 nominally needs 4.1, but
                # the AppImage *may* run if upstream still links 4.0.
                WEBKIT_PKG="libwebkit2gtk-4.0-37"
                warn "Detected older Ubuntu/Debian (${DISTRO_VER}). Tauri 2 prefers webkit 4.1+; upgrading distro recommended."
            fi
            LINUX_DEPS=("libayatana-appindicator3-1")
            [[ -n "$WEBKIT_PKG" ]] && LINUX_DEPS+=("$WEBKIT_PKG")
            PKG_INSTALL_CMD="sudo apt-get install -y"
            ;;
        fedora|rhel|centos|rocky|almalinux)
            # Fedora 39+ ships webkit2gtk4.1; older RHEL/Rocky needs EPEL.
            if command -v dnf &>/dev/null; then
                if dnf list available webkitgtk6.0 &>/dev/null 2>&1; then
                    LINUX_DEPS=("webkitgtk6.0" "libayatana-appindicator-gtk3")
                elif dnf list available webkit2gtk4.1 &>/dev/null 2>&1; then
                    LINUX_DEPS=("webkit2gtk4.1" "libayatana-appindicator-gtk3")
                else
                    warn "Could not find webkit2gtk4.1+ in dnf repos. You may need EPEL or COPR."
                fi
                PKG_INSTALL_CMD="sudo dnf install -y"
            fi
            ;;
        arch|manjaro|endeavouros)
            # Arch keeps both 4.1 and 6.0 in the official repos.
            if pacman -Si webkitgtk-6.0 &>/dev/null; then
                LINUX_DEPS=("webkitgtk-6.0" "libayatana-appindicator")
            else
                LINUX_DEPS=("webkit2gtk-4.1" "libayatana-appindicator")
            fi
            PKG_INSTALL_CMD="sudo pacman -S --noconfirm"
            ;;
        opensuse*|suse|sles)
            # openSUSE uses underscores in versioned suffixes.
            if zypper se -x libwebkitgtk-6_0-0 &>/dev/null; then
                LINUX_DEPS=("libwebkitgtk-6_0-0" "libayatana-appindicator3-1")
            else
                LINUX_DEPS=("libwebkit2gtk-4_1-0" "libayatana-appindicator3-1")
            fi
            PKG_INSTALL_CMD="sudo zypper install -y"
            ;;
        *)
            warn "Unknown distro '$DISTRO_ID' — cannot auto-install UI runtime deps."
            echo ""
            echo "  Manually install (whichever your package manager exposes):"
            echo "    • webkit2gtk-4.1 OR webkitgtk-6.0  (HTML renderer)"
            echo "    • libayatana-appindicator3        (system tray)"
            echo ""
            echo "  The AppImage may still launch if these are already present."
            ;;
    esac

    # Filter to only the missing packages and prompt before installing.
    if [[ ${#LINUX_DEPS[@]} -gt 0 && -n "$PKG_INSTALL_CMD" ]]; then
        MISSING_DEPS=()
        for dep in "${LINUX_DEPS[@]}"; do
            case "$DISTRO_ID" in
                ubuntu|debian|pop|linuxmint)
                    dpkg -s "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                fedora|rhel|centos|rocky|almalinux)
                    rpm -q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                arch|manjaro|endeavouros)
                    pacman -Q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
                opensuse*|suse|sles)
                    rpm -q "$dep" &>/dev/null 2>&1 || MISSING_DEPS+=("$dep")
                    ;;
            esac
        done

        if [[ ${#MISSING_DEPS[@]} -eq 0 ]]; then
            info "All UI runtime deps already present."
        else
            warn "Missing UI runtime deps (${DISTRO_ID} ${DISTRO_VER}):"
            for d in "${MISSING_DEPS[@]}"; do echo "    • $d"; done
            echo ""
            echo "  Install command:"
            echo "    $PKG_INSTALL_CMD ${MISSING_DEPS[*]}"
            echo ""

            if [ -t 0 ]; then
                printf "  Run this now (requires sudo)? [Y/n] "
                read -r REPLY </dev/tty
                REPLY="${REPLY:-Y}"
                if [[ "$REPLY" == "y" || "$REPLY" == "Y" ]]; then
                    if eval "$PKG_INSTALL_CMD ${MISSING_DEPS[*]}"; then
                        info "UI runtime deps installed."
                    else
                        warn "Install failed. The AppImage will still download but may fail to launch."
                    fi
                else
                    warn "Skipped. Install the deps manually before launching the UI."
                fi
            else
                warn "Non-interactive shell — skipping auto-install. Run manually:"
                echo "    $PKG_INSTALL_CMD ${MISSING_DEPS[*]}"
            fi
        fi
    fi
fi

# ── Obtain artifact (local build or GitHub download) ──────────────────────────
# Figure out the file suffix up front: dnf/apt/dpkg identify a package by its
# `.rpm`/`.deb` extension, so the temp file must carry it. A local artifact also
# overrides the detected kind (a hand-built .rpm installs as rpm regardless of
# what the host probe guessed).
artifact_suffix() {  # <filename>
    case "$1" in
        *.rpm)      echo ".rpm" ;;
        *.deb)      echo ".deb" ;;
        *.AppImage) echo ".AppImage" ;;
        *.dmg)      echo ".dmg" ;;
        *)          echo "" ;;
    esac
}

ART_SUFFIX=""
if [[ -n "${LMFORGE_UI_LOCAL:-}" ]]; then
    ART_SUFFIX=$(artifact_suffix "$LMFORGE_UI_LOCAL")
    if [[ "$OS" == "Linux" ]]; then
        case "$ART_SUFFIX" in
            .rpm)      LINUX_PKG_KIND="rpm" ;;
            .deb)      LINUX_PKG_KIND="deb" ;;
            .AppImage) LINUX_PKG_KIND="appimage" ;;
        esac
    fi
else
    ASSET=$(detect_ui_asset)
    ART_SUFFIX=$(artifact_suffix "$ASSET")
fi

TMP_FILE=$(mktemp "/tmp/lmforge-ui-XXXXXX")
if [[ -n "$ART_SUFFIX" ]]; then
    mv "$TMP_FILE" "$TMP_FILE$ART_SUFFIX"
    TMP_FILE="$TMP_FILE$ART_SUFFIX"
fi
trap 'rm -f "$TMP_FILE"' EXIT

if [[ -n "${LMFORGE_UI_LOCAL:-}" ]]; then
    # Dev/E2E path: install a locally built artifact (.dmg / .deb / .rpm /
    # .AppImage), skip the GitHub download. Mirrors LMFORGE_LOCAL_BIN in install-core.sh.
    section "Using local LMForge UI artifact..."
    [[ -f "$LMFORGE_UI_LOCAL" ]] || error "LMFORGE_UI_LOCAL not found: $LMFORGE_UI_LOCAL"
    cp "$LMFORGE_UI_LOCAL" "$TMP_FILE"
    info "Local artifact: $LMFORGE_UI_LOCAL"
else
    section "Downloading LMForge UI..."
    URL=$(resolve_url "$ASSET")
    echo    "  Asset:  $ASSET"
    echo    "  URL:    $URL"
    echo ""
    if ! curl -fSL --progress-bar "$URL" -o "$TMP_FILE"; then
        error "Download failed from $URL\n  Check https://github.com/$REPO/releases for available versions."
    fi
    info "Downloaded $ASSET"
fi

# ── Install macOS DMG ─────────────────────────────────────────────────────────
if [[ "$OS" == "Darwin" ]]; then
    section "Installing LMForge.app..."

    # Ensure ~/Applications exists (no sudo required)
    mkdir -p "${HOME}/Applications"

    # Mount DMG
    MOUNT_POINT=$(mktemp -d "/tmp/lmforge-dmg-XXXXXX")
    trap 'hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true; rm -f "$TMP_FILE"' EXIT
    hdiutil attach "$TMP_FILE" -mountpoint "$MOUNT_POINT" -nobrowse -quiet

    # Copy .app to ~/Applications
    APP_SRC=$(find "$MOUNT_POINT" -name "*.app" -maxdepth 1 | head -1)
    [[ -z "$APP_SRC" ]] && error "No .app found in DMG"

    if [[ -d "$APP_BUNDLE" ]]; then
        rm -rf "$APP_BUNDLE"
    fi
    cp -R "$APP_SRC" "$APP_BUNDLE"
    info "Installed: $APP_BUNDLE"

    # Detach DMG
    hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true

    # Remove quarantine (so macOS doesn't block first open)
    xattr -dr com.apple.quarantine "$APP_BUNDLE" 2>/dev/null || true
    info "Quarantine cleared"

    # Launch
    section "Launching LMForge..."
    open "$APP_BUNDLE"
    info "LMForge opened"
fi

# ── Install Linux (native rpm/deb preferred, AppImage fallback) ───────────────
install_linux_native_pkg() {
    local f="$1" kind="$2"
    case "$kind" in
        rpm)
            if command -v dnf &>/dev/null; then
                $SUDO dnf install -y "$f"
            elif command -v zypper &>/dev/null; then
                $SUDO zypper --non-interactive install --allow-unsigned-rpm "$f"
            else
                # Last resort: rpm can't resolve deps, but the package declares
                # them so the user gets a clear missing-dependency error.
                $SUDO rpm -Uvh --replacepkgs "$f"
            fi ;;
        deb)
            if command -v apt-get &>/dev/null; then
                # apt-get install on a local .deb resolves declared deps.
                $SUDO apt-get install -y "$f" \
                    || { $SUDO dpkg -i "$f" || true; $SUDO apt-get -f install -y; }
            else
                $SUDO dpkg -i "$f" || true
            fi ;;
    esac
}

if [[ "$OS" == "Linux" && ( "$LINUX_PKG_KIND" == "rpm" || "$LINUX_PKG_KIND" == "deb" ) ]]; then
    section "Installing LMForge ($LINUX_PKG_KIND)..."
    install_linux_native_pkg "$TMP_FILE" "$LINUX_PKG_KIND"
    [[ -x "$LINUX_NATIVE_BIN" ]] || error "Install reported success but $LINUX_NATIVE_BIN is missing."
    info "Installed: $LINUX_NATIVE_BIN"
    # Native packages ship the .desktop + icons under /usr/share already.
    command -v update-desktop-database &>/dev/null && $SUDO update-desktop-database &>/dev/null || true
    echo ""
    echo    "  Launch: $LINUX_NATIVE_BIN"
    echo    "  Or find 'LMForge' in your app launcher"

elif [[ "$OS" == "Linux" ]]; then
    section "Installing LMForge AppImage..."

    APPIMAGE_DIR="${HOME}/.local/bin"
    mkdir -p "$APPIMAGE_DIR"
    APPIMAGE_PATH="$APPIMAGE_DIR/LMForge"

    chmod +x "$TMP_FILE"
    cp "$TMP_FILE" "$APPIMAGE_PATH"
    info "Installed: $APPIMAGE_PATH"

    # AppImage type-2 self-mounts via libfuse2. Modern distros (e.g. Fedora)
    # ship only fuse3, so when libfuse.so.2 is absent we launch in
    # extract-and-run mode (no FUSE needed) — slightly slower startup, but it
    # actually runs. This is the documented FUSE-less fallback.
    if ldconfig -p 2>/dev/null | grep -q "libfuse.so.2"; then
        APPIMAGE_FUSE_OK=1
    else
        APPIMAGE_FUSE_OK=0
        warn "libfuse2 not found — launching the AppImage in extract-and-run mode."
        warn "For native desktop integration, prefer the .rpm/.deb package for your distro."
    fi

    install_linux_ui_launcher "$APPIMAGE_PATH"

    echo ""
    if [[ "$APPIMAGE_FUSE_OK" -eq 1 ]]; then
        echo    "  Launch: $APPIMAGE_PATH"
    else
        echo    "  Launch: APPIMAGE_EXTRACT_AND_RUN=1 $APPIMAGE_PATH"
    fi
    echo    "  Or find 'LMForge' in your app launcher"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}${GREEN}  ✓ LMForge UI installed successfully!${NC}"
echo ""
if [[ "$OS" == "Darwin" ]]; then
    echo    "  App:     $APP_BUNDLE"
    echo    "  Open:    open -a LMForge  (also available in Spotlight / Launchpad)"
fi
echo    ""
echo    "  The UI connects to the daemon at http://127.0.0.1:11430"
echo    "  Closing the UI window does NOT stop the daemon or your models."
echo ""
if [[ "$OS" == "Darwin" || "$OS" == "Linux" ]]; then
    echo    "  Uninstall UI only:"
    echo    "    curl -fsSL https://github.com/$REPO/releases/latest/download/uninstall-ui.sh | bash"
elif [[ "$OS" == "MINGW"* || "$OS" == "MSYS"* || "$OS" == "CYGWIN"* ]]; then
    echo    "  Uninstall UI only:"
    echo    "    irm https://github.com/$REPO/releases/latest/download/uninstall-ui.ps1 | iex"
fi
echo ""
