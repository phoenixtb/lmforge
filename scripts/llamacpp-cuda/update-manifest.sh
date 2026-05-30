#!/usr/bin/env bash
# Patch variants-manifest.json after a tarball build or R2 publish.
#
# Usage:
#   scripts/llamacpp-cuda/update-manifest.sh dist/llamacpp/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz
#   # or with config.env loaded:
#   source scripts/llamacpp-cuda/config.env
#   scripts/llamacpp-cuda/update-manifest.sh dist/llamacpp/*.tar.gz
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MANIFEST="$ROOT/data/engines/llamacpp/variants-manifest.json"
CONFIG="$ROOT/scripts/llamacpp-cuda/config.env"
if [[ -f "$CONFIG" ]]; then
  # shellcheck disable=SC1090
  source "$CONFIG"
fi

command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <tarball> [tarball ...]" >&2
  exit 2
fi

CDN_BASE="${LMFORGE_ENGINE_CDN_BASE:-}"
if [[ -z "$CDN_BASE" ]]; then
  echo "Set LMFORGE_ENGINE_CDN_BASE in config.env or environment" >&2
  exit 1
fi
CDN_BASE="${CDN_BASE%/}"

for tarball in "$@"; do
  [[ -f "$tarball" ]] || { echo "missing: $tarball" >&2; exit 1; }
  base="$(basename "$tarball" .tar.gz)"
  # lmforge-llamacpp-b9351-cuda12-linux-x64
  if [[ ! "$base" =~ ^lmforge-llamacpp-([^-]+)-(cuda12|cuda13)-linux-x64$ ]]; then
    echo "unexpected tarball name: $base" >&2
    exit 2
  fi
  tag="${BASH_REMATCH[1]}"
  variant="${BASH_REMATCH[2]}"
  sha="$(sha256sum "$tarball" | awk '{print $1}')"
  object_key="llamacpp/${tag}/${base}.tar.gz"

  echo "  $variant → sha256=${sha:0:16}… key=$object_key"

  tmp="$(mktemp)"
  jq \
    --arg cdn "$CDN_BASE" \
    --arg tag "$tag" \
    --arg vid "$variant" \
    --arg sha "$sha" \
    --arg key "$object_key" \
    '
    .cdn_base = $cdn
    | .llamacpp_tag = $tag
    | .storage = ((.storage // {}) + {provider: "r2", bucket: "lmforge-engine-assets"})
    | .variants = [.variants[] |
        if .id == $vid then
          .sha256 = $sha
          | .object_key = $key
          | del(.url)
        else . end]
    ' "$MANIFEST" > "$tmp"
  mv "$tmp" "$MANIFEST"
done

echo "Updated $MANIFEST"
echo "Next: cargo build --release && tag lmforge release when ready."
