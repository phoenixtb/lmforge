#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Pre-commit safety net for LMForge catalogs.
#
# Catches four common screw-ups before they hit the repo:
#   1. JSON syntax errors in data/catalogs/*.json
#   2. Drift between data/catalogs/*.json and ~/.lmforge/catalogs/
#      (you edited one, forgot the other)
#   3. Gated HF repos sneaking into safetensors.json or gguf.json
#      (catalog policy: 'no login' on either path)
#   4. Quant-suffix sanity: every chat shortcut in safetensors.json ends in
#      :4bit or :8bit; every chat shortcut in gguf.json ends in :4bit/:6bit/:8bit
#      (or :f16 for embed/rerank)
#
# Use standalone or as a git pre-commit hook. To wire up as a hook:
#   ln -sf ../../scripts/util/pre-commit-check-catalog.sh \
#          .git/hooks/pre-commit
#
# Flags:
#   --no-network   Skip the HF gated-repo probe (faster, offline-safe)
#   --fix-drift    Auto-copy repo → ~/.lmforge to resolve drift (rare; usually
#                  the other direction is the issue — edits in ~/.lmforge that
#                  haven't been ported back to data/catalogs/)
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

NO_NETWORK=0
FIX_DRIFT=0
while (($#)); do
    case "$1" in
        --no-network) NO_NETWORK=1 ;;
        --fix-drift)  FIX_DRIFT=1 ;;
        -h|--help)    sed -n '2,/^# ───*$/p' "$0"; exit 0 ;;
        *)            echo "Unknown flag: $1" >&2; exit 1 ;;
    esac; shift
done

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; BOLD='\033[1m'; NC='\033[0m'
ok()   { echo -e "  ${GREEN}✓${NC} $*"; }
warn() { echo -e "  ${YELLOW}⚠${NC} $*"; }
err()  { echo -e "  ${RED}✗${NC} $*"; }
sec()  { echo -e "\n${BOLD}$*${NC}"; }

FAILS=0
incr_fail() { FAILS=$((FAILS+1)); }

# ── 1. JSON syntax ───────────────────────────────────────────────────────────
sec "1. JSON syntax check"
for cat in "$REPO_ROOT"/data/catalogs/*.json; do
    [[ -f "$cat" ]] || continue
    if jq -e 'type == "object"' "$cat" >/dev/null 2>&1; then
        N=$(jq '[to_entries[] | select(.key | startswith("_comment") | not)] | length' "$cat")
        ok "$(basename "$cat") — $N shortcuts"
    else
        err "$(basename "$cat") — invalid JSON"
        jq . "$cat" 2>&1 | head -3 | sed 's/^/      /'
        incr_fail
    fi
done

# ── 2. Drift between repo and ~/.lmforge ─────────────────────────────────────
sec "2. Drift check (repo ↔ ~/.lmforge/catalogs)"
for cat in "$REPO_ROOT"/data/catalogs/*.json; do
    [[ -f "$cat" ]] || continue
    NAME=$(basename "$cat")
    LIVE="$HOME/.lmforge/catalogs/$NAME"
    if [[ ! -f "$LIVE" ]]; then
        warn "$NAME — no live copy at $LIVE (nothing to drift; harmless)"
        continue
    fi
    if cmp -s "$cat" "$LIVE"; then
        ok "$NAME matches"
    else
        err "$NAME drifted"
        diff -u "$cat" "$LIVE" | head -20 | sed 's/^/      /'
        if (( FIX_DRIFT )); then
            cp "$cat" "$LIVE"
            ok "  fix-drift: copied repo → live"
        else
            echo "        Resolve with: cp \"$cat\" \"$LIVE\"   (repo → live)"
            echo "                  or: cp \"$LIVE\" \"$cat\"   (live → repo)"
            incr_fail
        fi
    fi
done

# ── 3. Gated-repo probe (safetensors + gguf — both must be ungated by policy) ─
sec "3. Gated-repo probe (safetensors + gguf catalogs must stay ungated)"
probe_catalog() {
    local cat_path="$1" cat_name="$2"
    if [[ ! -f "$cat_path" ]]; then
        warn "no $cat_name — nothing to probe"
        return 0
    fi
    local repos total gated_repos=()
    repos=$(jq -r '[.[] | select(type=="string" and (startswith("---") | not))] | unique | .[]' "$cat_path")
    total=$(echo "$repos" | wc -l)
    echo "  $cat_name: probing $total distinct repos..."
    while IFS= read -r repo; do
        [[ -z "$repo" ]] && continue
        body=$(curl -sf --max-time 5 "https://huggingface.co/api/models/${repo}" 2>/dev/null || echo "")
        if [[ -z "$body" ]]; then
            status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "https://huggingface.co/api/models/${repo}")
            if [[ "$status" == "401" || "$status" == "403" ]]; then
                gated_repos+=("$repo  (HTTP $status)")
            fi
        else
            gated=$(echo "$body" | jq -r '.gated // false' 2>/dev/null)
            [[ "$gated" != "false" && "$gated" != "null" ]] && gated_repos+=("$repo  (gated=$gated)")
        fi
    done <<< "$repos"

    if (( ${#gated_repos[@]} == 0 )); then
        ok "$cat_name: $total repos — all public"
    else
        err "$cat_name: ${#gated_repos[@]} gated repo(s):"
        for r in "${gated_repos[@]}"; do echo "      $r"; done
        echo "      Replace with an ungated community mirror (unsloth/*, hugging-quants/*, RedHatAI/*, lmstudio-community/*, bartowski/*) or drop."
        incr_fail
    fi
}

if (( NO_NETWORK )); then
    warn "skipped (--no-network)"
elif ! command -v curl >/dev/null; then
    warn "curl missing — skipping"
else
    probe_catalog "$REPO_ROOT/data/catalogs/safetensors.json" "safetensors.json"
    probe_catalog "$REPO_ROOT/data/catalogs/gguf.json"        "gguf.json"
fi

# ── 4. Quant-suffix sanity (every chat shortcut must declare a bit-width) ────
sec "4. Quant-suffix sanity"
check_quant_suffix() {
    local cat_path="$1" cat_name="$2" suffixes_csv="$3"
    [[ -f "$cat_path" ]] || { warn "$cat_name missing"; return 0; }
    # Build :(a|b|c)$ regex from a CSV of allowed suffixes.
    local suffix_re=":(${suffixes_csv//,/|})$"
    # Inference shortcuts are required to end in a recognised quant suffix.
    # Embed / rerank shortcuts can end at any precision (or none — small models
    # often stay at native precision), so they're exempt from the suffix check.
    # Detection (case-insensitive substring on repo or shortcut):
    #   - "embed"   → embedding model (nomic, snowflake, Qwen3-Embedding, ...)
    #   - "rerank"  → reranker (Qwen3-Reranker, bge-reranker, jina-reranker)
    #   - "bge-"    → BAAI BGE family — every variant is embed or rerank
    local bad
    bad=$(jq -r --arg re "$suffix_re" '
        to_entries[]
        | select(.key | startswith("_comment") | not)
        | select(((.value + " " + .key) | ascii_downcase | test("embed|rerank|bge-")) | not)
        | select(.key | test($re) | not)
        | .key
    ' "$cat_path" 2>/dev/null || true)
    if [[ -z "$bad" ]]; then
        ok "$cat_name: every chat shortcut ends in :{${suffixes_csv//,/|}}"
    else
        err "$cat_name: chat shortcuts without a recognised quant suffix:"
        echo "$bad" | sed 's/^/      /'
        echo "      Recognised: :{$suffixes_csv} (embed/rerank shortcuts exempt by repo name)"
        incr_fail
    fi
}
check_quant_suffix "$REPO_ROOT/data/catalogs/safetensors.json" "safetensors.json" "4bit,8bit"
check_quant_suffix "$REPO_ROOT/data/catalogs/gguf.json"        "gguf.json"        "4bit,6bit,8bit,f16"

# ── Summary ──────────────────────────────────────────────────────────────────
echo ""
if (( FAILS == 0 )); then
    echo -e "${GREEN}  ✓ all checks passed${NC}"
    exit 0
else
    echo -e "${RED}  ✗ $FAILS check(s) failed${NC} — commit blocked"
    exit 1
fi
