#!/usr/bin/env bash
# Clean up llama.cpp CUDA build debris. Called by release-variants.sh after a
# successful publish, and runnable standalone (e.g. after a run of an older
# script version that didn't clean up).
#
# Removes:
#   .build/                    llama.cpp source + CUDA build trees (~10 GB/variant)
#   dist/llamacpp/<staging>/   unpacked tarball staging dirs (~1 GB each)
#   dist/llamacpp/*.tar.gz     tarballs from tags OTHER than the keep-tag
# Keeps (on purpose — they make the next rebuild warm):
#   lmforge-ccache-* Docker volumes, the CUDA devel images, and the keep-tag
#   tarballs + .sha256 files.
#
# The build container writes as root, so on Linux hosts these files are
# root-owned. A plain host rm is tried first; anything it can't delete is
# removed through a container (no sudo needed).
#
# Usage:
#   scripts/llamacpp-cuda/cleanup.sh              # keep-tag = engines.toml pin
#   scripts/llamacpp-cuda/cleanup.sh --keep-tag b9861
#   scripts/llamacpp-cuda/cleanup.sh --all        # remove tarballs of ALL tags too
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
KEEP_TAG=""
DELETE_ALL=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep-tag) KEEP_TAG="$2"; shift 2 ;;
    --all)      DELETE_ALL=1; shift ;;
    -h|--help)  sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$KEEP_TAG" && "$DELETE_ALL" -eq 0 ]]; then
  KEEP_TAG="$(awk '
    /^\[\[engine\]\]/ { in_llama = 0 }
    /^id[[:space:]]*=[[:space:]]*"llamacpp"/ { in_llama = 1 }
    in_llama && /^version[[:space:]]*=/ {
      match($0, /"[^"]+"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
  ' "$ROOT/data/engines.toml")"
  [[ -n "$KEEP_TAG" ]] || { echo "✗ could not deduce keep-tag from data/engines.toml — pass --keep-tag or --all" >&2; exit 1; }
fi

# Collect deletion targets.
targets=()
[[ -e "$ROOT/.build" ]] && targets+=("$ROOT/.build")
shopt -s nullglob
for p in "$ROOT/dist/llamacpp/"*; do
  base="$(basename "$p")"
  if [[ "$DELETE_ALL" -eq 0 ]]; then
    case "$base" in
      "lmforge-llamacpp-${KEEP_TAG}-"*.tar.gz|"lmforge-llamacpp-${KEEP_TAG}-"*.tar.gz.sha256)
        continue ;;
    esac
  fi
  targets+=("$p")
done
shopt -u nullglob

if [[ ${#targets[@]} -eq 0 ]]; then
  echo "Nothing to clean."
  exit 0
fi

echo "── Cleanup ──"
if [[ "$DELETE_ALL" -eq 1 ]]; then
  echo "  mode: --all (no tarballs kept)"
else
  echo "  keeping: dist/llamacpp/lmforge-llamacpp-${KEEP_TAG}-*.tar.gz(+.sha256)"
fi
du -shc "${targets[@]}" 2>/dev/null | tail -1 | awk '{print "  reclaiming ~" $1}' || true
for t in "${targets[@]}"; do echo "  rm ${t#"$ROOT"/}"; done

# Host rm first (works on macOS Docker Desktop, where files map to the user).
rm -rf "${targets[@]}" 2>/dev/null || true

# Anything left is root-owned (Linux host) — delete through a container.
leftovers=()
for t in "${targets[@]}"; do [[ -e "$t" ]] && leftovers+=("$t"); done
if [[ ${#leftovers[@]} -gt 0 ]]; then
  command -v docker >/dev/null || {
    echo "✗ ${#leftovers[@]} root-owned path(s) left and docker unavailable — rerun with sudo rm -rf" >&2
    exit 1
  }
  # shellcheck source=scripts/llamacpp-cuda/variants.conf
  source "$ROOT/scripts/llamacpp-cuda/variants.conf"
  echo "  (root-owned files — removing via container)"
  rel=()
  for t in "${leftovers[@]}"; do rel+=("/work/${t#"$ROOT"/}"); done
  docker run --rm -v "$ROOT:/work" "$variant_cuda12_image" rm -rf "${rel[@]}"
fi

echo "Done. Kept ccache volumes + CUDA images for warm rebuilds."
