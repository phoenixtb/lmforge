#!/usr/bin/env bash
# Legacy quick E2E entry point — delegates to multi_model_e2e.sh.
# Prefer: bash tests/multi_model_e2e.sh [--full]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export N_REQUESTS="${N_REQUESTS:-5}"
exec bash "$SCRIPT_DIR/multi_model_e2e.sh" "$@"
