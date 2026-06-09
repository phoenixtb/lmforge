#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# LMForge log viewer with level-aware coloring.
#
# Tails one or more log files with `tail -F` and pipes through sed to color
# the standard tracing levels (ERROR red, WARN yellow, INFO green, DEBUG dim).
# Works on both JSON and pretty `tracing` formats. If `multitail` is installed
# it's used in --all mode for split-pane viewing.
#
# Usage:
#   ./dev_logs.sh                       tail ~/.lmforge/logs/dev.log
#   ./dev_logs.sh --daemon              tail daemon.out.log + daemon.err.log
#   ./dev_logs.sh --init                tail init.log
#   ./dev_logs.sh --test                tail dev_test.log
#   ./dev_logs.sh --all                 tail every log under ~/.lmforge/logs/
#   ./dev_logs.sh --filter ERROR        only show ERROR lines (still colored)
#   ./dev_logs.sh --filter 'lmforge::engine'
#   ./dev_logs.sh --no-color            disable colors (pipe-friendly)
#   ./dev_logs.sh --tail-n 200          start with last N lines (default: 30)
#   ./dev_logs.sh /custom/path/file.log [more.log ...]
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

LOG_DIR="$HOME/.lmforge/logs"
FILES=()
FILTER=""
COLOR=1
TAIL_N=30

# Resolve presets to file paths
add_preset() {
    case "$1" in
        --daemon) FILES+=("$LOG_DIR/daemon.out.log" "$LOG_DIR/daemon.err.log") ;;
        --init)   FILES+=("$LOG_DIR/init.log") ;;
        --test)   FILES+=("$LOG_DIR/dev_test.log") ;;
        --all)
            while IFS= read -r f; do FILES+=("$f"); done \
                < <(find "$LOG_DIR" -maxdepth 1 -type f -name '*.log' 2>/dev/null) ;;
    esac
}

while (($#)); do
    case "$1" in
        --daemon|--init|--test|--all) add_preset "$1" ;;
        --filter)    FILTER="${2:?--filter requires a pattern}"; shift ;;
        --no-color)  COLOR=0 ;;
        --tail-n)    TAIL_N="${2:?--tail-n requires a number}"; shift ;;
        -h|--help)   sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        --*)         echo "Unknown flag: $1" >&2; exit 1 ;;
        *)           FILES+=("$1") ;;
    esac
    shift
done

# Default file if nothing specified
(( ${#FILES[@]} == 0 )) && FILES=("$LOG_DIR/dev.log")

# Verify at least one file exists
EXISTING=()
for f in "${FILES[@]}"; do [[ -f "$f" ]] && EXISTING+=("$f"); done
if (( ${#EXISTING[@]} == 0 )); then
    echo "  ✗ none of the requested log files exist:" >&2
    for f in "${FILES[@]}"; do echo "      $f" >&2; done
    echo ""
    echo "  Tip: run lmforge with stdout teed to a file. Common locations:" >&2
    ls -1 "$LOG_DIR" 2>/dev/null | sed "s|^|      $LOG_DIR/|" >&2
    exit 1
fi

# ── Build the coloring pipeline ──────────────────────────────────────────────
if (( COLOR )); then
    # ANSI escapes consumable by `sed` — keep them as printable octal to avoid
    # quoting hell. \033 = ESC.
    color_pipe() {
        sed -uE \
            -e $'s/(ERROR|FATAL|panic|panicked)/\033[1;31m\\1\033[0m/g' \
            -e $'s/( WARN |WARN: |WARNING)/\033[1;33m\\1\033[0m/g' \
            -e $'s/( INFO )/\033[0;32m\\1\033[0m/g' \
            -e $'s/( DEBUG )/\033[2;37m\\1\033[0m/g' \
            -e $'s/( TRACE )/\033[2;90m\\1\033[0m/g'
    }
else
    color_pipe() { cat; }
fi

if [[ -n "$FILTER" ]]; then
    filter_pipe() { grep --line-buffered -E "$FILTER" || true; }
else
    filter_pipe() { cat; }
fi

# ── Run ──────────────────────────────────────────────────────────────────────
# Prefer multitail when --all is requested and >1 file exists
if [[ "$*" == *"--all"* ]] && command -v multitail >/dev/null && (( ${#EXISTING[@]} > 1 )); then
    echo "  → multitail (split-pane, ${#EXISTING[@]} files)"
    exec multitail "${EXISTING[@]}"
fi

echo "  → tail -F  files=${#EXISTING[@]}  filter=${FILTER:-(none)}  color=$COLOR"
for f in "${EXISTING[@]}"; do echo "     $f"; done
echo ""

# `tail -F` follows rotated/recreated files; --pid not portable everywhere,
# rely on shell exit signal handling. Add a per-file label when >1 file.
if (( ${#EXISTING[@]} == 1 )); then
    tail -n "$TAIL_N" -F "${EXISTING[0]}" | filter_pipe | color_pipe
else
    # `tail -F file1 file2` prefixes ==> file <== headers — pass through.
    tail -n "$TAIL_N" -F "${EXISTING[@]}" | filter_pipe | color_pipe
fi
