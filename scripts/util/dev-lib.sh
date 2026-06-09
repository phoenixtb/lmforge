# Shared helpers for scripts/util/dev-*.sh — source only, do not execute.
[[ -n "${_LMFORGE_DEV_LIB_LOADED:-}" ]] && return 0
_LMFORGE_DEV_LIB_LOADED=1

DEV_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEV_REPO_ROOT="$(cd "$DEV_LIB_DIR/../.." && pwd)"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'

dev_info()    { echo -e "${GREEN}  ✓${NC} $*"; }
dev_warn()    { echo -e "${YELLOW}  ⚠${NC} $*"; }
dev_err()     { echo -e "${RED}  ✗${NC} $*" >&2; }
dev_section() { echo -e "\n${BOLD}$*${NC}"; }

dev_is_tty() { [[ -t 0 ]]; }

# prompt_yn VAR default(y|n) "question"
dev_prompt_yn() {
    local __var="$1" __def="$2" __q="$3" __hint=""
    if [[ "$__def" == "y" ]]; then __hint="[Y/n]"; else __hint="[y/N]"; fi
    if (( DEV_NONINTERACTIVE )) || ! dev_is_tty; then
        printf -v "$__var" '%s' "$__def"
        return
    fi
    local __reply
    # Prompt on tty explicitly — read -p + </dev/tty often shows no prompt in IDE terminals.
    if [[ -w /dev/tty ]] 2>/dev/null; then
        printf "  %s %s " "$__q" "$__hint" >/dev/tty
        read -r __reply </dev/tty 2>/dev/null || __reply=""
    else
        printf "  %s %s " "$__q" "$__hint" >&2
        read -r __reply || __reply=""
    fi
    __reply="${__reply:-$__def}"
    case "${__reply,,}" in
        y|yes)  printf -v "$__var" '%s' "y" ;;
        *)      printf -v "$__var" '%s' "n" ;;
    esac
}

dev_size_of() { [[ -e "$1" ]] && du -sh "$1" 2>/dev/null | cut -f1 || echo "—"; }

dev_remove() {
    local path="$1" label="${2:-$1}"
    if [[ -e "$path" || -L "$path" ]]; then
        local s
        s=$(dev_size_of "$path")
        rm -rf "$path"
        dev_info "removed $label ($s)"
    fi
}

# Stop LMForge core: release binary, dev build, engine children, systemd.
dev_stop_core() {
    local data_dir="${1:-${LMFORGE_DATA_DIR:-$HOME/.lmforge}}"

    if command -v lmforge &>/dev/null; then
        lmforge service stop 2>/dev/null || true
        lmforge stop 2>/dev/null || true
    fi
    systemctl --user stop lmforge.service 2>/dev/null || true
    curl -sf -X POST --max-time 2 http://127.0.0.1:11430/lf/shutdown 2>/dev/null || true

    pkill -x lmforge 2>/dev/null || true
    pkill -f 'target/(debug|release)/lmforge' 2>/dev/null || true
    pkill -x llama-server 2>/dev/null || true
    pkill -f 'lmforge.*start' 2>/dev/null || true

    rm -f "$data_dir/lmforge.pid" "$HOME/.lmforge/lmforge.pid" 2>/dev/null || true

    # Engine PID files under data_dir/engines/**/*.pid
    if [[ -d "$data_dir/engines" ]]; then
        find "$data_dir/engines" -name '*.pid' -type f -delete 2>/dev/null || true
    fi
    sleep 1
}

# Stop LMForge UI: Tauri dev, AppImage, vite on ui port.
dev_stop_ui() {
    pkill -x LMForge 2>/dev/null || true
    pkill -f 'lmforge-ui' 2>/dev/null || true
    pkill -f 'tauri dev' 2>/dev/null || true
    pkill -f 'vite.*1420' 2>/dev/null || true
    pkill -f 'node.*vite.*1420' 2>/dev/null || true
    sleep 1
}

dev_audit_line() {
    printf "  %-28s %8s  %s\n" "$1" "$(dev_size_of "$2")" "$2"
}
