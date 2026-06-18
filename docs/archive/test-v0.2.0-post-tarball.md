# v0.2.0 post-tarball live test plan

**Trigger:** GitHub Actions `Build llama.cpp CUDA variants` completes and publishes fresh `lmforge-llamacpp-b9351-{cuda12,cuda13}-linux-x64.tar.gz` (with NCCL bundled on cuda12).

**Host:** RTX 5060 Ti 16 GB, driver ≥ r590 (cuda13-eligible). Same box used for S-2/S-3 live validation.

**Goal:** Prove both CUDA variants run chat (MTP + non-MTP), embeddings, and VLM end-to-end with no manual library hacks.

---

## 0. Pre-flight (both variants)

```bash
# Install fresh tarballs
lmforge engine install llamacpp --variant cuda12
lmforge engine install llamacpp --variant cuda13

# Sanity — must NOT print "not found" for bundled libs
LD_LIBRARY_PATH=~/.lmforge/engines/llamacpp/variants/cuda12/lib \
  ~/.lmforge/engines/llamacpp/variants/cuda12/llama-server --version
LD_LIBRARY_PATH=~/.lmforge/engines/llamacpp/variants/cuda13/lib \
  ~/.lmforge/engines/llamacpp/variants/cuda13/llama-server --version

lmforge doctor   # cuda12 ACTIVE by default; both variants installed
```

| # | Check | Pass |
|---|--------|------|
| P-1 | `llama-server --version` exits 0 on cuda12 **without** system NCCL | no `libnccl.so.2: cannot open` |
| P-2 | `llama-server --version` exits 0 on cuda13 | |
| P-3 | `ldd …/cuda12/llama-server \| grep not found` is empty with `LD_LIBRARY_PATH=$ORIGIN/lib` | |
| P-4 | Same for cuda13 | |

---

## 1. Matrix overview

Run **§2–§5** twice: `LMFORGE_LLAMACPP_VARIANT=cuda12` then `=cuda13`. Stop daemon between variant runs.

| Category | Model shortcut | Role | Spec expected |
|----------|----------------|------|---------------|
| Chat (non-MTP) | `qwen3.5:4b:4bit` | Chat | `spec_mode=off` |
| Chat (MTP) | `qwen3.5:4b:mtp:4bit` | Chat | `spec_mode=mtp`, `spec_stats` after chat |
| Embed | `qwen3-embed:0.6b:8bit` | Embeddings | N/A (no spec) |
| VLM | `qwen2.5-vl:3b:4bit` | Vision chat | `spec_mode=off` (no MTP tensors) |

Optional stretch (VRAM permitting): `qwen3:8b:4bit` + draft pair `qwen3:0.6b:4bit` → `spec_mode=draft-model`.

---

## 2. Chat — non-MTP (`qwen3.5:4b:4bit`)

```bash
export LMFORGE_LLAMACPP_VARIANT=cuda12   # or cuda13
lmforge stop; lmforge start --model qwen3.5:4b:4bit
# wait for engine port 11431 healthy
curl -s http://127.0.0.1:11430/lf/status | jq '.running_models[0] | {id, spec_mode, spec_stats}'
```

| # | Test | Pass |
|---|------|------|
| C-1 | `spec_mode` is `"off"` | |
| C-2 | Non-stream chat, `max_tokens=64`, `enable_thinking=false` → non-empty content | |
| C-3 | Stream chat → ≥2 SSE chunks + `[DONE]` | |
| C-4 | `spec_stats` absent or null | |
| C-5 | Stderr log has **no** `draft acceptance` lines | |

---

## 3. Chat — MTP (`qwen3.5:4b:mtp:4bit`)

```bash
lmforge stop
export LMFORGE_SPECULATIVE_MODE=auto
lmforge start --model qwen3.5:4b:mtp:4bit
```

| # | Test | Pass |
|---|------|------|
| M-1 | Spawn args include `--spec-type draft-mtp` (daemon log) | |
| M-2 | `spec_mode` is `"mtp"` within 90s of cold load | |
| M-3 | Chat `max_tokens≥128` completes (`finish_reason=length` or `stop`) | |
| M-4 | `/lf/status.spec_stats`: `samples≥1`, `drafted_total>0`, `last_accept_rate∈(0,1]` | |
| M-5 | Stderr log contains `draft acceptance =` (b9351 format) | |
| M-6 | **No S-2.8 fallback** — daemon log must not show `retrying once with spec=off` | |
| M-7 | S-2.9 smoke: same prompt MTP vs `LMFORGE_SPECULATIVE_MODE=off` → identical output (greedy, `temperature=0`) | optional but recommended |

Accept-rate baseline (prior run, for regression eyeball): cuda12 ~84%, cuda13 ~94%. Large drift (>20 pts) warrants investigation, not auto-fail.

---

## 4. Embeddings (`qwen3-embed:0.6b:8bit`)

```bash
lmforge pull qwen3-embed:0.6b:8bit   # if missing
# embed model can co-exist or replace chat — test both single and co-load
curl -s -X POST http://127.0.0.1:11430/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3-embed:0.6b:8bit","input":"hello world"}' | jq '.data[0].embedding | length'
```

| # | Test | Pass |
|---|------|------|
| E-1 | Single input → vector `length > 0` (expect 1024 for Qwen3-Embed-0.6B) | |
| E-2 | Batch `input: ["a","b","c"]` → 3 vectors | |
| E-3 | `/lf/status` shows embed model loaded, `spec_mode` N/A or off | |
| E-4 | **Co-load:** chat model + embed model both in `running_models`; burst 5 embed + 5 chat requests — all 200 | use `tests/multi_model_e2e.sh` |

Repeat E-1–E-4 on **both** cuda12 and cuda13.

---

## 5. VLM (`qwen2.5-vl:3b:4bit`)

```bash
lmforge pull qwen2.5-vl:3b:4bit
lmforge stop; lmforge start --model qwen2.5-vl:3b:4bit
```

| # | Test | Pass |
|---|------|------|
| V-1 | Model index: `capabilities.vision=true`, `mmproj_path` set after pull | |
| V-2 | Spawn uses mmproj sidecar (check daemon log for `mmproj`) | |
| V-3 | Chat with **text-only** prompt works (baseline) | |
| V-4 | Chat with **image** (OpenAI-style `image_url` or LMForge native multimodal payload) → non-empty response | provide a small PNG test asset |
| V-5 | `spec_mode=off` (VL models don't use MTP in this catalog) | |

VRAM note: 3B VLM ~2–3 GB + mmproj; fits 16 GB with default ctx.

---

## 6. Variant-specific regressions

| # | Test | Pass |
|---|------|------|
| R-1 | `LMFORGE_LLAMACPP_VARIANT=cuda13` overrides auto cuda12 selection (`doctor` shows cuda13 ACTIVE) | |
| R-2 | `init` on clean `engines/` auto-installs cuda12 on NVIDIA Linux | |
| R-3 | `lmforge engine list` shows both variants installed | |
| R-4 | UI `/ui` Overview shows spec mode + accept rate on loaded chat slot | manual or Playwright later |

---

## 7. Automated runners (execute after manual smoke passes)

```bash
# Unit + integration + default e2e (chat + embed)
./scripts/util/dev_test.sh --release --with-e2e

# Multi-model co-load burst
LF_BIN=./target/release/lmforge CHAT_MODEL=qwen3.5:4b:4bit \
  EMBED_MODEL=qwen3-embed:0.6b:8bit bash tests/multi_model_e2e.sh

# MTP-specific (add script or env when available)
LMFORGE_LLAMACPP_VARIANT=cuda12 LMFORGE_SPECULATIVE_MODE=auto \
  CHAT_MODEL=qwen3.5:4b:mtp:4bit ./scripts/util/dev_test.sh --release --with-e2e --e2e-model qwen3.5:4b:mtp:4bit
```

---

## 8. Sign-off checklist

- [ ] cuda12 P-1..P-4
- [ ] cuda13 P-1..P-4
- [ ] cuda12: C-*, M-*, E-*, V-*
- [ ] cuda13: C-*, M-*, E-*, V-*
- [ ] R-1..R-4
- [ ] `dev_test.sh --release --with-e2e` green
- [ ] `multi_model_e2e.sh` green on both variants

**Executor:** agent on user notification + tarball SHA256 from release `llamacpp-b9351`.

**Artifacts to capture:** `/lf/status` JSON per model, stderr tail with `draft acceptance`, `lmforge doctor` output, any S-2.8 fallback warnings.
