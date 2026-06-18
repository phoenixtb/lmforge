# ADR-005: Speculative decoding (MTP-first)

- **Status:** Accepted (2026-05-30)
- **Follows:** [plan-v0.2.0-cuda-mtp](../archive/plan-v0.2.0-cuda-mtp.md) Phase S-1/S-2/S-3

## Context

`llama-server` b9351+ supports `--spec-type draft-mtp` for models with internal
nextn/MTP heads, plus `--spec-type draft-simple` for sibling draft models.
LMForge must enable this safely with telemetry and fallbacks.

## Decision

### Resolution order (`mode=auto`)

1. **MTP** when `capabilities.mtp == true` and VRAM headroom ≥ safety floor.
2. **Draft-model pair** when MTP absent, pair exists in `draft_pairs.toml`,
   draft GGUF installed, VRAM fits, pair not in `draft_pairs_status.json`.
3. **Off** otherwise.

### MTP detection (layered)

1. **GGUF tensor probe wins** when file parses (`nextn.*` / `mtp.*` names).
2. **Catalog hint** only when probe cannot read the file.
3. Standard Unsloth Qwen3.5 quants strip MTP — use `*-MTP-GGUF` repos.

### Launch flags

MTP requires **both** `--spec-type draft-mtp` and `--spec-draft-*` knobs.
Missing `--spec-type` silently disables spec-dec.

### Telemetry

Stderr tee parses `draft acceptance = R (A accepted / G generated)` (b9351) and legacy `draft acceptance rate = …`.
Exposed as `spec_mode` + `spec_stats` on `/lf/status`.

### Crash fallback (S-2.8)

Spec-enabled spawn dying < 5s → one retry with `LMFORGE_SPECULATIVE_MODE=off`.
Draft-model failures also record broken pair in `draft_pairs_status.json`.

### Lossless contract

`mode=off` must not alter non-spec args (seed, temp). Property-tested in
`append_spec_args` unit tests.

## Consequences

- Users need MTP-specific GGUFs for MTP gains; regular quants still work without spec-dec.
- Draft-model auto pairs require both target and draft pulled.
- Greedy byte-identical guarantee holds for `mode=off`; MTP/draft may change timing only.
