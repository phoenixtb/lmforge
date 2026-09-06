#!/usr/bin/env bash
# Mother script: build → publish → verify → commit the llama.cpp CUDA variant
# tarballs in one command. Wraps the existing single-purpose scripts
# (build-local.sh, publish-r2.sh, update-manifest.sh) — each still works
# standalone; this only sequences them and adds the safety gates.
#
# Pipeline:
#   1. Deduce the llama.cpp tag from data/engines.toml (the registry pin the
#      lmforge binary expects) — the b9861-registry-vs-b9351-tarball skew
#      exists precisely because these were bumped independently. --tag
#      overrides for experimental builds.
#   2. docker pull the Rocky8 CUDA devel images (names come from
#      variants.conf — the single source of truth shared with CI).
#   3. build-local.sh   — compile + tarball into dist/llamacpp/.
#   4. publish-r2.sh    — upload to R2 + patch variants-manifest.json.
#   5. Smoke-test each public CDN URL (HTTP 200).
#   6. Clean up build debris (.build/ source+build trees, dist/ staging dirs,
#      stale-tag tarballs) — ccache volumes and Docker images are kept on
#      purpose so the next rebuild is warm. --no-cleanup preserves everything
#      for debugging. Skipped automatically if any earlier step fails.
#   7. git commit the manifest + push (gated by a confirm unless --yes).
#
# Prerequisites: docker, aws CLI, jq, curl, and a filled-in
# scripts/llamacpp-cuda/config.env (R2 keys + CDN base — see
# docs/engineering/R2-ENGINE-ASSETS.md).
#
# Usage:
#   scripts/llamacpp-cuda/release-variants.sh                  # all variants, tag from engines.toml
#   scripts/llamacpp-cuda/release-variants.sh --variant cuda12
#   scripts/llamacpp-cuda/release-variants.sh --tag b9999      # override pin (experimental build)
#   scripts/llamacpp-cuda/release-variants.sh --yes            # no confirm prompts (CI/cron)
#   scripts/llamacpp-cuda/release-variants.sh --no-push        # commit locally, don't push
#   scripts/llamacpp-cuda/release-variants.sh --no-cleanup     # keep build trees for debugging
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VARIANT="all"
TAG=""
ASSUME_YES=0
NO_PUSH=0
NO_CLEANUP=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --variant)    VARIANT="$2"; shift 2 ;;
    --tag)        TAG="$2"; shift 2 ;;
    --yes)        ASSUME_YES=1; shift ;;
    --no-push)    NO_PUSH=1; shift ;;
    --no-cleanup) NO_CLEANUP=1; shift ;;
    -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

case "$VARIANT" in all|cuda12|cuda13) ;; *)
  echo "variant must be cuda12, cuda13, or all" >&2; exit 2 ;;
esac

# ── Preflight: fail fast before any multi-hour step ──────────────────────────
for cmd in docker aws jq curl git; do
  command -v "$cmd" >/dev/null || { echo "✗ $cmd required" >&2; exit 1; }
done
CONFIG="$ROOT/scripts/llamacpp-cuda/config.env"
[[ -f "$CONFIG" ]] || {
  echo "✗ $CONFIG missing — cp config.example.env config.env and fill in R2 keys" >&2
  exit 1
}
# shellcheck disable=SC1090
source "$CONFIG"
: "${R2_ACCESS_KEY_ID:?Set R2_ACCESS_KEY_ID in config.env}"
: "${LMFORGE_ENGINE_CDN_BASE:?Set LMFORGE_ENGINE_CDN_BASE in config.env}"
if [[ "$LMFORGE_ENGINE_CDN_BASE" == *YOURDOMAIN* ]]; then
  echo "✗ LMFORGE_ENGINE_CDN_BASE still has the YOURDOMAIN placeholder" >&2
  exit 1
fi

# shellcheck source=scripts/llamacpp-cuda/variants.conf
source "$ROOT/scripts/llamacpp-cuda/variants.conf"

# ── Tag deduction ─────────────────────────────────────────────────────────────
# Default: the `version` pin inside the llamacpp [[engine]] block of
# data/engines.toml. That pin is what `lmforge doctor` reports and what the
# Windows prebuilt path already ships, so building any other tag by default
# would reintroduce the version skew.
if [[ -z "$TAG" ]]; then
  TAG="$(awk '
    /^\[\[engine\]\]/ { in_llama = 0 }
    /^id[[:space:]]*=[[:space:]]*"llamacpp"/ { in_llama = 1 }
    in_llama && /^version[[:space:]]*=/ {
      match($0, /"[^"]+"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
  ' "$ROOT/data/engines.toml")"
  [[ -n "$TAG" ]] || { echo "✗ could not deduce llamacpp tag from data/engines.toml — pass --tag" >&2; exit 1; }
  TAG_SOURCE="engines.toml registry pin"
else
  TAG_SOURCE="--tag override"
fi

# ── Plan + confirm ────────────────────────────────────────────────────────────
images=()
[[ "$VARIANT" == "all" || "$VARIANT" == "cuda12" ]] && images+=("$variant_cuda12_image")
[[ "$VARIANT" == "all" || "$VARIANT" == "cuda13" ]] && images+=("$variant_cuda13_image")

echo "── llama.cpp CUDA variant release plan ──"
echo "  tag       : $TAG  ($TAG_SOURCE)"
echo "  variants  : $VARIANT"
for img in "${images[@]}"; do echo "  image     : $img"; done
echo "  cdn       : ${LMFORGE_ENGINE_CDN_BASE%/}"
echo "  git       : commit variants-manifest.json$( ((NO_PUSH)) && echo ' (push skipped)' || echo ' + push' )"
echo "  est. time : ~45–70 min per variant cold (ccache-warm: minutes)"
confirm() {
  ((ASSUME_YES)) && return 0
  read -r -p "$1 [y/N] " reply
  [[ "$reply" == "y" || "$reply" == "Y" ]]
}
confirm "Proceed?" || { echo "aborted"; exit 1; }

# ── 1. Docker images ──────────────────────────────────────────────────────────
for img in "${images[@]}"; do
  echo ""
  echo "── docker pull $img ──"
  docker pull "$img"
done

# ── 2. Build ──────────────────────────────────────────────────────────────────
"$ROOT/scripts/llamacpp-cuda/build-local.sh" --variant "$VARIANT" --tag "$TAG"

# Glob strictly by tag: dist/ may still hold tarballs from previous tags, and
# publishing those would silently re-pin the manifest to the old build.
shopt -s nullglob
tarballs=("$ROOT/dist/llamacpp/lmforge-llamacpp-${TAG}-"*.tar.gz)
shopt -u nullglob
[[ ${#tarballs[@]} -gt 0 ]] || { echo "✗ no tarballs for tag $TAG in dist/llamacpp/" >&2; exit 1; }

# ── 3. Publish to R2 (also patches variants-manifest.json) ───────────────────
"$ROOT/scripts/llamacpp-cuda/publish-r2.sh" "${tarballs[@]}"

# ── 4. CDN smoke test ─────────────────────────────────────────────────────────
echo ""
echo "── CDN smoke test ──"
for tarball in "${tarballs[@]}"; do
  url="${LMFORGE_ENGINE_CDN_BASE%/}/llamacpp/${TAG}/$(basename "$tarball")"
  code="$(curl -fsSL -o /dev/null -w '%{http_code}' --retry 3 --retry-delay 5 "$url" || true)"
  if [[ "$code" == "200" ]]; then
    echo "  ✓ 200 $url"
  else
    echo "  ✗ $code $url" >&2
    echo "    Upload succeeded but the CDN isn't serving it — check custom domain / WAF" >&2
    echo "    (docs/engineering/R2-ENGINE-ASSETS.md §6). Manifest NOT committed." >&2
    exit 1
  fi
done

# ── 5. Cleanup ────────────────────────────────────────────────────────────────
# Reclaims the big build debris (only reached when build/publish/smoke all
# succeeded — failures exit above, leaving everything in place for debugging):
#   .build/llama.cpp-*        source + CUDA build trees (~10 GB per variant)
#   dist/llamacpp/<staging>/  unpacked tarball staging dirs (~1 GB each)
#   dist/llamacpp/*.tar.gz    tarballs from OTHER tags (current tag's are kept)
# Deliberately kept: the lmforge-ccache-* Docker volumes and the CUDA images —
# they turn the next cold ~1 h build into minutes. The build container runs as
# root, so these files are root-owned on Linux hosts; deleting through a
# container avoids needing sudo.
if ((NO_CLEANUP)); then
  echo ""
  echo "── Cleanup skipped (--no-cleanup) ──"
else
  echo ""
  echo "── Cleanup (build trees, staging dirs, stale-tag tarballs) ──"
  docker run --rm -v "$ROOT:/work" -e KEEP_TAG="$TAG" "${images[0]}" bash -c '
    rm -rf /work/.build
    for p in /work/dist/llamacpp/*; do
      [ -e "$p" ] || continue
      case "$(basename "$p")" in
        lmforge-llamacpp-"$KEEP_TAG"-*.tar.gz|lmforge-llamacpp-"$KEEP_TAG"-*.tar.gz.sha256) ;;
        *) echo "  rm $(basename "$p")"; rm -rf "$p" ;;
      esac
    done
  '
  echo "  kept: dist/llamacpp/*${TAG}*.tar.gz(+.sha256), ccache volumes, Docker images"
fi

# ── 6. Commit + push the manifest ─────────────────────────────────────────────
MANIFEST_REL="data/engines/llamacpp/variants-manifest.json"
cd "$ROOT"
if git diff --quiet -- "$MANIFEST_REL"; then
  echo ""
  echo "Manifest unchanged (same sha256s already committed) — nothing to commit."
  exit 0
fi

echo ""
git --no-pager diff --stat -- "$MANIFEST_REL"
confirm "Commit + $( ((NO_PUSH)) && echo 'skip push' || echo 'push' ) the manifest update?" || {
  echo "Manifest left modified in the working tree — commit manually when ready."
  exit 0
}
git add "$MANIFEST_REL"
git commit -m "Publish llamacpp ${TAG} Linux CUDA variants (${VARIANT}) to R2"
if ((NO_PUSH)); then
  echo "Committed locally; push skipped (--no-push)."
else
  git push origin HEAD
fi

echo ""
echo "Done. The manifest is embedded at build time — the new variants ship with"
echo "the next lmforge release (or a local 'cargo build --release')."
