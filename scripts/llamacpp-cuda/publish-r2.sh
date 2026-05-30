#!/usr/bin/env bash
# Upload llama.cpp CUDA tarballs to Cloudflare R2 and update variants-manifest.json.
#
# Prerequisites:
#   - aws CLI v2 (`apt install awscli` or pip install awscli)
#   - scripts/llamacpp-cuda/config.env with R2 credentials + CDN base
#
# Usage:
#   scripts/llamacpp-cuda/publish-r2.sh dist/llamacpp/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz
#   scripts/llamacpp-cuda/publish-r2.sh dist/llamacpp/*.tar.gz
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CONFIG="$ROOT/scripts/llamacpp-cuda/config.env"
if [[ -f "$CONFIG" ]]; then
  # shellcheck disable=SC1090
  source "$CONFIG"
fi

command -v aws >/dev/null || { echo "aws CLI required (S3-compatible upload to R2)" >&2; exit 1; }

: "${R2_ACCESS_KEY_ID:?Set R2_ACCESS_KEY_ID in config.env}"
: "${R2_SECRET_ACCESS_KEY:?Set R2_SECRET_ACCESS_KEY in config.env}"
: "${R2_BUCKET:?Set R2_BUCKET in config.env}"
: "${R2_ENDPOINT:?Set R2_ENDPOINT in config.env}"
: "${LMFORGE_ENGINE_CDN_BASE:?Set LMFORGE_ENGINE_CDN_BASE in config.env}"

export AWS_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
export AWS_DEFAULT_REGION=auto

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <tarball> [tarball ...]" >&2
  exit 2
fi

for tarball in "$@"; do
  [[ -f "$tarball" ]] || { echo "missing: $tarball" >&2; exit 1; }
  base="$(basename "$tarball" .tar.gz)"
  if [[ ! "$base" =~ ^lmforge-llamacpp-([^-]+)-(cuda12|cuda13)-linux-x64$ ]]; then
    echo "unexpected tarball name: $base" >&2
    exit 2
  fi
  tag="${BASH_REMATCH[1]}"
  object_key="llamacpp/${tag}/${base}.tar.gz"

  echo ""
  echo "Uploading $(basename "$tarball") → s3://${R2_BUCKET}/${object_key}"

  aws s3 cp "$tarball" "s3://${R2_BUCKET}/${object_key}" \
    --endpoint-url "$R2_ENDPOINT" \
    --content-type application/gzip \
    --cache-control "public, max-age=31536000, immutable"

  sha_file="${tarball}.sha256"
  if [[ -f "$sha_file" ]]; then
    aws s3 cp "$sha_file" "s3://${R2_BUCKET}/${object_key}.sha256" \
      --endpoint-url "$R2_ENDPOINT" \
      --content-type text/plain \
      --cache-control "public, max-age=31536000, immutable"
  fi

  public_url="${LMFORGE_ENGINE_CDN_BASE%/}/${object_key}"
  echo "  Public URL (after CDN setup): $public_url"
done

"$ROOT/scripts/llamacpp-cuda/update-manifest.sh" "$@"
echo ""
echo "Smoke-test download (after CDN is live):"
echo "  curl -fsSL -o /dev/null -w '%{http_code}\\n' \"${LMFORGE_ENGINE_CDN_BASE%/}/llamacpp/${tag}/$(basename "$1")\""
