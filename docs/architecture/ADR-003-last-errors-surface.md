# ADR-003: `last_errors` / `stderr_tail` — failure-visibility contract

- **Status:** Accepted (2026-05-27)
- **Follows:** [ADR-001 — Engine tier model](./ADR-001-engine-tiers.md), [ADR-002 — `/lf/engines` endpoint](./ADR-002-engines-endpoint.md)
- **Stakeholders:** core, UI, support

## Context

Through Phase 2 the daemon already wrote engine-subprocess stderr to
`~/.lmforge/logs/<model_id>/stderr.log` via `logging::rotation`. That gave
us the data on disk but nothing in the API surfaced it. Failed-load UX was:

> First-token request hangs → 503 after timeout → user opens a terminal,
> runs `lmforge stop`, finds the log dir manually, `less` the right
> stderr file, decides what went wrong.

That breaks on three counts:

1. **The CLI user has no signal beyond the timeout HTTP code.** They
   don't know whether the engine died at `mmap`, in `flash-attn`, or
   talking to CUDA.
2. **Tauri UI users can't drop into a shell at all.** The desktop app
   is the whole interaction.
3. **DocIntel / external clients** querying `/lf/status` see
   `overall_status: "degraded"` with no machine-readable failure context,
   so an automated supervisor can't decide whether to retry or escalate.

Phase 2.3 added `EngineState.last_errors`. Phase 6 wires the UI consumer.
This ADR pins the contract between the two so future refactors don't
silently drop fields the UI depends on.

## Decision

The daemon exposes a **`last_errors: Map<model_id, ModelLoadError>`** map
on `GET /lf/status`. The map is **always present** (it may be `{}`).
Every successful load **clears** the entry for that `model_id`; every
failed load **replaces** it.

`ModelLoadError` carries an RFC3339 timestamp, a short human message, and
an optional `stderr_tail` containing the last N lines of the engine
subprocess's stderr. The UI renders the message inline and the
`stderr_tail` behind a per-card toggle.

### `ModelLoadError` schema

```jsonc
{
  "at": "2026-05-27T16:26:13.737175765+00:00",   // RFC3339 UTC
  "message": "No .gguf file found in model directory: /home/u/.lmforge/models/qwen3:8b:4bit. Pull the model first with: lmforge pull qwen3:8b:4bit",
  "stderr_tail": "ERROR cuda kernel sm_120 missing\nFATAL aborting"
}
```

| Field | Type | Notes |
| --- | --- | --- |
| `at` | RFC3339 UTC string | When the failure was recorded. UI renders relative ("5m ago") + absolute on hover. |
| `message` | `string` | Single human-readable line describing the failure. **Never** an opaque error code. When the daemon can suggest a fix (e.g. "Pull the model first with: …"), the suggestion is appended. |
| `stderr_tail` | `string \| null` | Last bytes of `~/.lmforge/logs/<model_id>/stderr.log`. `null` when the engine never spawned (pre-flight failures) or when no log file exists. |

### Bounds and tuning knobs

Both bounds live in the daemon, **not** the UI:

| Limit | Default | Override | Source |
| --- | --- | --- | --- |
| Max simultaneous tracked failures | **8 models** | *(not user-tunable)* | `MAX_LAST_ERRORS` in `src/engine/manager.rs` |
| `stderr_tail` line count | **32 lines** | `LMFORGE_STDERR_TAIL_LINES` | `DEFAULT_STDERR_TAIL_LINES` in `src/logging/rotation.rs` |
| `stderr_tail` byte cap | **8 KiB** | *(not user-tunable)* | `DEFAULT_STDERR_TAIL_MAX_BYTES` in `src/logging/rotation.rs` |

When the map grows beyond `MAX_LAST_ERRORS`, the oldest entry by `at` is
evicted (FIFO). The byte cap is applied **before** the line cap to ensure
a single malformed engine line cannot blow the snapshot size.

### Eviction / clearing rule

```text
on successful load of <model_id>:    last_errors.remove(<model_id>)
on failed     load of <model_id>:    last_errors.insert(<model_id>, ModelLoadError { ... })
on map.len() > MAX_LAST_ERRORS:      evict the entry with the smallest `at`
on `lmforge stop`:                   last_errors persists as long as the daemon process lives;
                                     on cold start it begins empty again.
```

Successful loads clear because keeping a stale failure next to a now-
working slot is **strictly worse** than silence. Users would interpret
the stale card as "still failing" and re-run diagnosis.

### Capture path (engine subprocess → `last_errors`)

```
engine subprocess
  └── writes stderr → daemon's logging::rotation writer
        └── ~/.lmforge/logs/<model_id>/stderr.log  (rotated, capped)

EngineManager::load(model_id) fails with anyhow::Error
  └── EngineManager::record_load_failure(model_id, err)
        ├── stderr_tail = read_stderr_tail(&logs_dir, model_id)
        │     reads last DEFAULT_STDERR_TAIL_LINES (env-overridable)
        │     within DEFAULT_STDERR_TAIL_MAX_BYTES bytes
        ├── entry = ModelLoadError { at: now(), message: err.to_string(), stderr_tail }
        ├── state.last_errors.insert(model_id, entry)
        └── if len > MAX_LAST_ERRORS: evict oldest by `at`
```

`record_load_failure` is `infallible-by-design`: every I/O error inside
it is swallowed silently. Surfacing diagnostics must **never** be allowed
to fail the actual load path.

### Where the entry **does not** appear

- **Pre-pull failures** (e.g. catalog miss, network error during model
  download). These are returned synchronously to the `/v1/chat/completions`
  caller as HTTP 4xx/5xx and don't touch `last_errors`. The map is for
  *cold-load* failures specifically — once a model is on disk, the next
  failure to materialise the engine slot is what users want to see.
- **Crashes after a successful load.** Today these are surfaced via
  `running_models[<id>].status = "error"` and the engine metrics
  (`restart_count`). They do **not** get a `last_errors` entry. Adding
  that is a candidate for a future ADR; the runtime-failure log lives in
  Observability already.

## Consumer contracts

### CLI

`lmforge status` (when implemented in a follow-up) reads
`last_errors[*]` and prints a one-line summary per failed model. The
`message` field is rendered as-is; `stderr_tail` is **not** printed
unless the user adds `--verbose`.

### UI (Phase 6)

The Overview route mounts an **Engine Load Errors** panel iff
`Object.keys(last_errors).length > 0`. Per-entry rendering:

```
┌─ <model_id>                                        5m ago ─┐
│  No .gguf file found in model directory: …                  │
│  ▸ stderr tail (1.4 KB)                                     │
└──────────────────────────────────────────────────────────────┘
```

Clicking the toggle expands an inline `<pre>` with `white-space: pre`
and a 220 px scrollable max-height. The card itself is the
last-error per `model_id`, so the user can have at most
`MAX_LAST_ERRORS` cards visible.

Status updates arrive via the existing transport (Tauri `lf:status`
event on desktop, SSE `/lf/status/stream` in the browser). No new
transport is added.

### External (DocIntel, supervisors, CI)

`GET /lf/status` returns `last_errors` as a top-level field. Empty
object iff no recent failures. Supervisor agents should treat any non-
empty `last_errors` for the **active** model as an actionable signal;
non-active models with old entries are advisory only.

## Consequences

### Positive

- **Cold-load failures stop being silent.** Both CLI and UI users see
  exactly what the engine said, without needing to know where logs live.
- **Single transport.** Piggy-backing on `/lf/status` means SSE clients,
  Tauri IPC consumers, and HTTP pollers all receive the same data with
  zero new wiring.
- **Bounded memory.** 8 entries × 8 KiB ≈ 64 KiB worst case on the
  daemon side; trivial. The status snapshot stays under ~100 KiB even
  in worst case.
- **Self-clearing.** Map entries vanish when their model loads
  successfully — no manual dismissal UX needed.

### Negative / costs

- **`stderr_tail: null` is ambiguous.** Could mean "engine never spawned"
  *or* "log file missing". The UI documents this by suppressing the
  toggle entirely when `stderr_tail` is null; we accept the small
  ambiguity rather than fingerprinting the cause.
- **No crash-after-load entries.** A model that loads then dies mid-
  request shows up in `running_models[*].status` and metrics, but not
  here. That's a deliberate scope choice (see "Where the entry does not
  appear"); a follow-up ADR would extend the contract if needed.
- **Stderr language is whatever the upstream engine prints.** vLLM
  python tracebacks, SGLang multi-line CUDA errors, and `llama-server`
  one-liners all coexist in the same field. The UI renders them as
  `monospace pre`, accepting the heterogeneity.

### Neutral

- **The byte cap is not configurable.** 8 KiB is enough for every
  engine error we've observed in practice. Bumping it would require a
  code change + a recompile, which we consider a feature: it prevents
  ad-hoc operators from blowing up the snapshot size by accident.

## Test / regression posture

- **Unit tests** in `src/logging/rotation.rs` cover empty / missing /
  multi-line stderr files (`read_stderr_tail_*`).
- **Shape-regression guard** in `scripts/util/dev_test.sh` (Phase 1
  step 2): asserts `.last_errors | type == "object"` is present on
  every successful `/lf/status` fetch.
- **Live smoke** (validated during Phase 6):
  - `curl POST /v1/chat/completions` with a non-existent model name
    returns HTTP 503.
  - `curl /lf/status | jq .last_errors` shows the new entry
    immediately (cold-load path).
  - Re-running with a real model clears the previous entry on success.

## Future work

- **Crash-after-load entries.** Promote runtime crashes from
  `restart_count` only into a sibling `runtime_errors` map keyed by
  `model_id`. New ADR if/when needed.
- **Severity classification.** Adding a coarse `severity:
  "transient" | "user_error" | "engine_bug"` field would let UIs surface
  the difference between "out of disk" and "this hardware can't run
  this model".
- **Streamed stderr.** Once Phase 7 lands a streaming-output subsystem,
  the UI could `follow` a failing model's stderr in real-time rather
  than reading the post-mortem tail. Until then, the post-mortem is
  strictly better than the previous nothing.

## References

- `src/engine/manager.rs` — `ModelLoadError`, `MAX_LAST_ERRORS`,
  `record_load_failure`.
- `src/logging/rotation.rs` — `read_stderr_tail` + bounds.
- `src/server/native.rs::status` — `/lf/status` response builder.
- `ui/src/lib/api.ts` — `ModelLoadError`, `LfStatus.last_errors`,
  `normalizeStatus`.
- `ui/src/routes/+page.svelte` — Engine Load Errors panel.
