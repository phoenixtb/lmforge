# Shared interactive menu helpers for scripts/lmforge.sh — source only.
[[ -n "${_LMFORGE_MENU_LIB_LOADED:-}" ]] && return 0
_LMFORGE_MENU_LIB_LOADED=1

MENU_GREEN='\033[0;32m'; MENU_RED='\033[0;31m'; MENU_YELLOW='\033[1;33m'
MENU_CYAN='\033[0;36m'; MENU_BOLD='\033[1m'; MENU_DIM='\033[2m'; MENU_NC='\033[0m'

menu_is_tty() { [[ -t 0 ]]; }

# pick_from_list TITLE OPTS_ARRAY_NAME LABELS_ARRAY_NAME [preselect_index]
# Sets global PICK_RESULT to the selected option key.
pick_from_list() {
    local title="$1"
    local opts_name="$2"
    local lbls_name="$3"
    local preselect="${4:-0}"

    local cur=$preselect n
    eval "n=\${#${opts_name}[@]}"

    local _pick_saved_tty
    _pick_saved_tty=$(stty -g 2>/dev/null)

    _draw_pick() {
        local row=$1 i _lbl
        printf "\033[%s;0H" "$row"
        printf "\033[2K"
        echo -e "  ${MENU_BOLD}${title}${MENU_NC}"
        echo ""
        for (( i = 0; i < n; i++ )); do
            eval "_lbl=\${${lbls_name}[$i]}"
            printf "\033[2K"
            if [[ $i -eq $cur ]]; then
                printf "  ${MENU_CYAN}▸ ${MENU_BOLD}%s${MENU_NC}\n" "$_lbl"
            else
                printf "    %s\n" "$_lbl"
            fi
        done
        printf "\033[2K"
        printf "\n  ${MENU_DIM}↑↓ navigate • enter select • q quit${MENU_NC}"
    }

    local start_row=8
    printf "\033[?25l"
    stty -echo -icanon min 1 time 0 2>/dev/null

    _draw_pick "$start_row"

    local key seq
    while IFS= read -r -n1 -s key; do
        if [[ "$key" == $'\x1b' ]]; then
            IFS= read -r -n2 -s -t 1 seq || true
            case "$seq" in
                '[A'|'OA') [[ $cur -gt 0 ]] && cur=$(( cur - 1 )) ;;
                '[B'|'OB') [[ $cur -lt $((n - 1)) ]] && cur=$(( cur + 1 )) ;;
            esac
        elif [[ "$key" == '' ]]; then
            break
        elif [[ "$key" == 'q' || "$key" == 'Q' ]]; then
            PICK_RESULT="__quit__"
            stty "$_pick_saved_tty" 2>/dev/null
            printf "\033[?25h"
            return 0
        fi
        _draw_pick "$start_row"
    done

    stty "$_pick_saved_tty" 2>/dev/null
    printf "\033[?25h"
    printf "\033[%s;0H" $((start_row + n + 3))

    eval "PICK_RESULT=\${${opts_name}[$cur]}"
}

# Simple numbered fallback when not a TTY.
pick_from_list_fallback() {
    local title="$1" opts_name="$2" lbls_name="$3"
    local i _lbl
    echo ""
    echo -e "  ${MENU_BOLD}${title}${MENU_NC}"
    echo ""
    eval "local n=\${#${opts_name}[@]}"
    for (( i = 0; i < n; i++ )); do
        eval "_lbl=\${${lbls_name}[$i]}"
        printf "    %2d) %s\n" "$((i + 1))" "$_lbl"
    done
    echo ""
    local choice
    read -r -p "  Choice [1-$n]: " choice
    if [[ "$choice" =~ ^[0-9]+$ ]] && (( choice >= 1 && choice <= n )); then
        eval "PICK_RESULT=\${${opts_name}[$((choice - 1))]}"
    else
        PICK_RESULT="__quit__"
    fi
}

menu_pick() {
    local title="$1" opts_name="$2" lbls_name="$3" pre="${4:-0}"
    if menu_is_tty; then
        pick_from_list "$title" "$opts_name" "$lbls_name" "$pre"
    else
        pick_from_list_fallback "$title" "$opts_name" "$lbls_name"
    fi
}
