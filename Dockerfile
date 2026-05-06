# syntax=docker/dockerfile:1.7
#
# LMForge — CPU/llama.cpp container image.
#
# This image ships:
#   - the lmforge orchestrator binary (built from this repo)
#   - llama-server (binary build) for chat / embed / rerank inference
#
# It does NOT ship oMLX (Apple-only) or SGLang (CUDA-only). For NVIDIA hosts
# build the SGLang image separately with `Dockerfile.cuda` (planned).
#
# Build:   docker build -t lmforge:cpu .
# Run:     docker run --rm -p 11430:11430 -v lmforge-data:/root/.lmforge lmforge:cpu
# Health:  curl http://localhost:11430/health

# ─────────────────────────────────────────────────────────────────────────────
# Stage 1a — build the SvelteKit dashboard (static)
# ─────────────────────────────────────────────────────────────────────────────
FROM node:22-bookworm-slim AS ui-builder
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
# adapter-static writes the bundle to /ui/build
RUN npm run build

# ─────────────────────────────────────────────────────────────────────────────
# Stage 1 — build the lmforge binary
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1.83-slim-bookworm AS builder

# Build deps for tokio/openssl/sha2; pkg-config + libssl for reqwest's TLS path
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libssl-dev \
    git \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Copy manifests first so dep layer caches independently of source edits.
COPY Cargo.toml Cargo.lock ./
COPY ui/src-tauri/Cargo.toml ui/src-tauri/Cargo.toml

# Cargo workspace requires the member to exist; create a placeholder lib.rs
# so we can compile dependencies without the full UI source tree.
RUN mkdir -p src ui/src-tauri/src \
 && echo 'fn main() {}' > src/main.rs \
 && echo '' > ui/src-tauri/src/lib.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin lmforge \
 && rm -rf src

# Now bring in the real source and build the actual binary.
COPY src ./src
COPY data ./data
COPY tests ./tests
COPY README.md ./

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --bin lmforge \
 && cp target/release/lmforge /usr/local/bin/lmforge

# ─────────────────────────────────────────────────────────────────────────────
# Stage 2 — runtime
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

# Runtime deps:
#   - ca-certificates: HuggingFace HTTPS pulls + image preflight
#   - libgomp1: llama.cpp OpenMP runtime
#   - lsof, procps: startup_cleanup() and orphan kill paths in start.rs
#   - curl: image baseline probe / health checks from inside the container
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libgomp1 \
    libssl3 \
    lsof \
    procps \
 && rm -rf /var/lib/apt/lists/*

# llama.cpp prebuilt binary (CPU build). Pinned to a known-good release tag.
# Override at build time: --build-arg LLAMA_CPP_VERSION=b4500
ARG LLAMA_CPP_VERSION=b4503
RUN curl -fsSL -o /tmp/llamacpp.zip \
    "https://github.com/ggerganov/llama.cpp/releases/download/${LLAMA_CPP_VERSION}/llama-${LLAMA_CPP_VERSION}-bin-ubuntu-x64.zip" \
 && apt-get update && apt-get install -y --no-install-recommends unzip \
 && unzip -j /tmp/llamacpp.zip -d /usr/local/bin/ "build/bin/llama-server" \
 && chmod +x /usr/local/bin/llama-server \
 && apt-get purge -y --auto-remove unzip \
 && rm -rf /var/lib/apt/lists/* /tmp/llamacpp.zip

COPY --from=builder /usr/local/bin/lmforge /usr/local/bin/lmforge

# Dashboard static bundle. Mounted at /ui by the daemon when LMFORGE_UI_DIR
# resolves to a directory containing index.html. Path matches the env var
# below — change both together.
COPY --from=ui-builder /ui/build /usr/local/share/lmforge/ui
ENV LMFORGE_UI_DIR=/usr/local/share/lmforge/ui

# Persistent state mount point. Models, logs, hardware probe, models.json,
# and the `engines/` PID files all live here. Host should bind a volume.
VOLUME ["/root/.lmforge"]

# 11430: API server (default). Override with `--bind` / `--port` if needed.
EXPOSE 11430

# Bind 0.0.0.0 so requests from outside the container reach the daemon.
# Auth defaults still apply: trusted_networks covers RFC1918 + loopback,
# so adjacent docker network traffic is allowed without a token. Set
# `LMFORGE_REFUSE_UNSAFE_BIND=1` to refuse startup when no api_key/CIDR is
# configured (recommended for production deployments).
ENV LMFORGE_BIND=0.0.0.0

HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:11430/health || exit 1

ENTRYPOINT ["lmforge"]
CMD ["start", "--bind", "0.0.0.0", "--port", "11430"]
