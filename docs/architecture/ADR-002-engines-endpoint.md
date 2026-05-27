# ADR-002: `/lf/engines` HTTP contract + UI tier-switcher

- **Status:** Accepted (2026-05-27)
- **Follows:** [ADR-001 — Engine tier model](./ADR-001-engine-tiers.md)
- **Stakeholders:** core, UI, docs

## Context

ADR-001 introduced a three-tier engine model (`default` / `opt-in` /
`experimental`) and a hardware-aware selector that respects compute-cap
ranges and OS family. The CLI surfaces that via `lmforge engine list |
install | uninstall | status`. With Phase 6 the same information has to be
reachable from the desktop UI so users can see at a glance:

- Which engines are installed.
- Which engines this host is **compatible with** (and *why not*, when it
  isn't).
- Which engine is currently **active** in the running daemon.
- The exact CLI command to install or switch to a given engine.

Two options were considered for sourcing that data:

1. **Extend `/lf/status`** with a `registry` block.
2. **Add a dedicated `/lf/engines` endpoint.**

Option 1 was rejected: `/lf/status` is on the hot path (Tauri polls it every
2 s, SSE subscribers receive a copy on every model-load tick). The registry
is static for the daemon's lifetime — embedding it would 4–5× the snapshot
payload for no benefit. Worse, mixing per-request state (`overall_status`,
`running_models`) with per-process state (`engines[]`) makes the schema
harder to evolve cleanly.

## Decision

Adopt **option 2**. The daemon exposes a dedicated `GET /lf/engines`
endpoint whose response is the **JSON twin of `lmforge engine list`**:
same engines, same verdicts, same tier strings. The UI never re-derives
compatibility — it consumes the verdict the daemon already computed.

A second decision: the UI is **read-only**. Engine installation runs in
the user's terminal, not behind a GUI button. Reasoning is documented in
the *UX posture* section below.

### Endpoint contract

```http
GET /lf/engines
```

```jsonc
{
  "engines": [
    {
      "id": "llamacpp",
      "name": "llama.cpp",
      "version": "b9351",
      "tier": "default",
      "install_method": "binary",
      "model_format": "gguf",
      "matches_gpu": "any",
      "min_compute_cap": null,
      "max_compute_cap": null,
      "min_vram_gb": 0.0,
      "supported_os_families": ["linux", "darwin", "windows-native", "windows-wsl2"],
      "supports_embeddings": true,
      "supports_reranking": false,
      "installed": true,
      "compatible": true,
      "incompatible_reason": null,
      "active": true
    },
    {
      "id": "vllm",
      "name": "vLLM",
      "version": "0.21.0",
      "tier": "opt-in",
      "installed": true,
      "compatible": true,
      "active": false
      // ...
    },
    {
      "id": "sglang",
      "name": "SGLang",
      "version": "0.5.10.post1",
      "tier": "experimental",
      "installed": false,
      "compatible": false,
      "incompatible_reason": "Compute-capability or OS-family gate refused this combo",
      "active": false
      // ...
    }
  ],
  "active_engine_id": "llamacpp",
  "has_hardware_profile": true
}
```

#### Field semantics

| Field | Type | Notes |
| --- | --- | --- |
| `id` | `string` | Stable identifier, matches `engines.toml`. |
| `name` | `string` | Human label (`"llama.cpp"`, `"TabbyAPI (ExLlamaV3)"`). |
| `version` | `string` | Pinned upstream version (e.g. `"b9351"`, `"0.21.0"`). |
| `tier` | `"default" \| "opt-in" \| "experimental" \| "default*"` | **Identical** to what `lmforge engine list` prints. Wire badge colours off this string. |
| `install_method` | `"binary" \| "pip" \| "brew" \| string` | The UI uses this to decide whether to show a venv path. |
| `model_format` | `string` | `"gguf"`, `"mlx"`, `"safetensors"`, `"exl3"`. |
| `matches_gpu` | `string` | `"any"`, `"nvidia"`, `"apple"`, `"amd"`. |
| `min_compute_cap`, `max_compute_cap` | `string \| null` | NVIDIA gate window, e.g. `"7.5"` / `"10.3"`. Both `null` ⇒ no gate. |
| `min_vram_gb` | `number \| null` | Soft hint, not enforced server-side. |
| `supported_os_families` | `string[]` | Subset of `{linux, darwin, windows-native, windows-wsl2}`. |
| `supports_embeddings`, `supports_reranking` | `boolean` | Drives capability chips in the UI. |
| `installed` | `boolean` | True iff the venv (`pip`) or staged binary (`binary`) is present at `~/.lmforge/engines/<id>/`. |
| `compatible` | `boolean \| null` | `null` ⇒ no `hardware.json` (user hasn't run `lmforge init`). Otherwise the **same** verdict `lmforge engine status <id>` would print. |
| `incompatible_reason` | `string \| null` | Populated only when `compatible == false`. Mirrors the human reason from the gate matcher. |
| `active` | `boolean` | The engine the running daemon is currently using (`engine_state.engine_id == id`). |
| `active_engine_id` | `string` | Top-level convenience field. |
| `has_hardware_profile` | `boolean` | When `false`, `compatible` will be `null` for every row and the UI suppresses install hints. |

#### Compatibility-verdict rule

The endpoint **does not duplicate** the gate logic. It calls into the same
`pub(crate)` helpers used by the CLI:

- `cli::engine::install_state(engine, data_dir)` — venv / binary presence.
- `cli::engine::compatibility(engine, profile)` — runs `v1_matches` then
  `v2_matches` from `engine::registry`, returning `(ok, reason)`.
- `cli::engine::tier_label(tier)` — single source of tier strings.

A regression that diverges the CLI and the UI verdict would require
breaking these helpers in two callers at once. Tests in `cli/engine.rs`
plus the `dev_test.sh` shape check guard against that.

## UX posture: read-only UI, terminal installs

The Settings → Engine page **shows** the matrix and lets users **copy** the
right CLI command. It does **not** trigger an install when a row's button
is clicked.

Three reasons:

1. **Install size and duration.** An opt-in tier install is ~5 GB of
   wheels + venv and takes 3–8 minutes on a typical link. Hiding that
   behind a sync GUI button either freezes the window or forces us to
   build a streaming-output subsystem in Phase 6. The streaming subsystem
   is correctly Phase 7+ work; not gating Phase 6 on it lets us ship the
   visibility surface today.
2. **Failure debugging.** When `uv pip install vllm` fails because the
   system is missing `ninja`, the right place to see that is the terminal
   you launched it from. A GUI button that swallows stdout and prints
   "Install failed" is strictly worse for users.
3. **Trust posture.** A user is comfortable running `lmforge engine
   install vllm` after reading the line in the UI. They are *less*
   comfortable with the desktop app silently downloading 5 GB on their
   behalf. Showing the exact command is a more honest UX.

When the streaming-output subsystem lands in Phase 7+, this ADR will be
superseded for the install flow specifically. The endpoint contract above
stays.

### Per-row CLI hints

The UI shows a copy-to-clipboard chip with the suggested command, chosen
by tier + state:

| State | Suggested command |
| --- | --- |
| Compatible, not installed, `opt-in` | `lmforge engine install <id>` |
| Compatible, not installed, `experimental` | `lmforge engine install <id> --yes-experimental` |
| Installed, not active | `lmforge start --engine <id>` |
| Active, or `compatible == false` | *(no chip — nothing to do)* |
| `has_hardware_profile == false` | *(no chip — user must run `lmforge init` first)* |

### Refresh semantics

The UI fetches `/lf/engines` once on mount and again whenever the user
switches **into** the Engine section. There is no live SSE feed; the
registry only changes when a user runs `lmforge engine install` from a
terminal, and the polite cost of a manual *Refresh* button is far cheaper
than wiring a fifth SSE stream for a screen most users open once.

## Consequences

### Positive

- **Single source of truth.** The CLI helper functions back both the
  terminal and the GUI verdicts. Drift becomes a build error rather than
  a silent UX bug.
- **Zero hot-path cost.** Daemon's `/lf/status` payload size unchanged.
  Polling `/lf/engines` once-per-section-view is < 5 ms.
- **Scriptable.** External tooling (e.g. DocIntel, CI smoke checks) can
  query the engine roster as structured JSON without parsing the CLI.

### Negative / costs

- **Two HTTP endpoints for related-but-different data** (`/lf/status` for
  per-tick state, `/lf/engines` for per-process registry). Documented
  here so future contributors know the split is intentional.
- **Read-only UI is initially surprising.** A short hint string ("Run in
  your terminal — installs ~5 GB venv + wheels.") is shown below every
  command chip to set expectations. Revisit when streaming-output lands.

### Neutral

- **No new auth surface.** `/lf/engines` reuses the same auth-bypass
  layer as `/lf/status` (loopback + `trusted_networks`).

## Implementation pointers

- Handler: `pub async fn engines` in `src/server/native.rs`.
- Route wiring: `src/server/mod.rs` (`.route("/lf/engines", get(native::engines))`).
- Shared helpers: `cli::engine::{install_state, compatibility, tier_label}` —
  all `pub(crate)` (annotated with "exposed for HTTP" rustdoc comments).
- TS types: `EngineInfo`, `EnginesResponse`, `EngineTier` in
  `ui/src/lib/api.ts`.
- UI: `ui/src/routes/settings/+page.svelte` Engine section (replaces the
  previous "coming soon" placeholder).
- Shape-regression guard: `scripts/util/dev_test.sh` Phase 1 step 2b.

## References

- [ADR-001 — Engine tier model](./ADR-001-engine-tiers.md) — where tiers
  and platform gates are defined.
- `src/cli/engine.rs` — CLI implementation that this endpoint mirrors.
- `data/engines.toml` — engine roster + per-engine gate fields.
