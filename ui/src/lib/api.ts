/**
 * LMForge API Client — typed wrappers for all /lf/* endpoints.
 *
 * All Svelte components import from here — no raw fetch() anywhere else.
 * Mirrors the Rust structs in src/engine/manager.rs and src/server/native.rs.
 */

const BASE = 'http://localhost:11430';

// ─── Types ───────────────────────────────────────────────────────────────────

export type EngineStatus = 'stopped' | 'starting' | 'ready' | 'degraded' | 'error';

/**
 * Speculative-decoding mode the slot was started with. Mirrors `SpecMode`
 * in src/engine/speculative.rs. `auto` is a config value only — the Rust
 * resolver normalises it to one of the concrete modes before spawn, so
 * runtime slots will only ever see `mtp` / `draft-model` / `off`.
 */
export type SpecMode = 'auto' | 'mtp' | 'draft-model' | 'off';

/**
 * Cumulative speculative-decoding telemetry parsed from `llama-server`
 * stderr. Mirrors `SpecStats` in src/engine/spec_observer.rs. `null` /
 * undefined for slots that haven't served a spec-active request yet, and
 * for non-llamacpp engines.
 */
export interface SpecStats {
  drafted_total: number;
  accepted_total: number;
  samples: number;
  last_accept_rate: number;
  cumulative_accept_rate: number;
}

export interface ModelSlot {
  model_id: string;
  port: number;
  status: EngineStatus;
  idle_secs: number;
  vram_est_gb: number;
  role: string;
  /** What spec-dec mode this slot was spawned with. Defaults to 'off'. */
  spec_mode?: SpecMode;
  /** Cumulative spec-dec stats — undefined until first sample arrives. */
  spec_stats?: SpecStats | null;
}

export interface EngineMetrics {
  requests_total: number;
  ttft_avg_ms: number;
  uptime_secs: number;
  restart_count: number;
}

/**
 * Last failed-load context for a model. Mirrors ModelLoadError in
 * src/engine/manager.rs. Surfaced on Overview when present so users don't
 * have to grep ~/.lmforge/logs/ for a failed `lmforge pull` or cold load.
 */
export interface ModelLoadError {
  at: string;                     // ISO timestamp
  message: string;                // short human-readable failure
  stderr_tail?: string | null;    // last N lines of engine stderr (may be null)
}

/**
 * Normalised frontend status — always use this shape in stores/components.
 * The daemon returns a slightly different JSON shape; normalizeStatus() maps it.
 */
/** In-flight model pull snapshot from GET /lf/status (or null when idle). */
export interface ActivePull {
  model: string;
  file: string;
  downloaded_bytes: number;
  total_bytes: number;
  done: boolean;
  error?: string | null;
}

export interface LfStatus {
  overall_status: EngineStatus;
  engine_id: string;
  engine_version: string;
  running_models: Record<string, ModelSlot>;
  metrics: EngineMetrics;
  /** model_id → last failure context. Empty when every recent load succeeded. */
  last_errors: Record<string, ModelLoadError>;
  /** Currently in-flight model pull, or null. Survives SSE-client disconnects. */
  active_pull?: ActivePull | null;
}

/** Raw shape from GET /lf/status and the SSE stream */
interface RawStatus {
  overall_status: EngineStatus;
  engine?: { id: string; version: string };
  engine_id?: string;
  engine_version?: string;
  running_models: ModelSlot[] | Record<string, ModelSlot>;
  metrics: EngineMetrics;
  last_errors?: Record<string, ModelLoadError> | null;
  active_pull?: ActivePull | null;
}

/** Normalise the raw daemon response to a stable LfStatus shape */
export function normalizeStatus(raw: RawStatus): LfStatus {
  // Flatten nested engine object if present
  const engine_id = raw.engine?.id ?? raw.engine_id ?? '—';
  const engine_version = raw.engine?.version ?? raw.engine_version ?? '—';

  // running_models is an array in some versions, object in others
  let running_models: Record<string, ModelSlot>;
  if (Array.isArray(raw.running_models)) {
    running_models = {};
    for (const slot of raw.running_models) {
      running_models[slot.model_id] = slot;
    }
  } else {
    running_models = raw.running_models as Record<string, ModelSlot>;
  }

  return {
    overall_status: raw.overall_status,
    engine_id,
    engine_version,
    running_models,
    metrics: raw.metrics,
    last_errors: raw.last_errors ?? {},
    active_pull: raw.active_pull ?? null,
  };
}

export interface HardwareProfile {
  os: string;
  arch: string;
  cpu_model: string;
  cpu_cores: number;
  gpu_vendor: string;
  vram_gb: number;
  unified_mem: boolean;
  total_ram_gb: number;
  is_tegra?: boolean;
}

export interface GpuStats {
  util_pct:     number | null;  // 0–100 or null
  mem_used_mb:  number | null;  // Metal-allocated unified memory MiB
  mem_total_mb: number | null;  // Total Metal budget MiB
  source: string;               // "IOAccelerator" | "nvidia-smi" | "unavailable" …
  note: string;                 // Human-readable context
}

/** Measured RSS for one model server child process */
export interface ModelProcMem {
  model_id: string;  // model id or "engine/other"
  rss_mb:   number;  // resident set size in MiB
}

/** Live system telemetry from GET /lf/sysinfo (polled every 2 s) */
export interface SysStats {
  cpu_pct: number;          // system-wide CPU 0–100
  cpu_cores_pct: number[];  // per-logical-core 0–100 (up to 32)
  mem_total_gb: number;     // physical RAM in GiB
  mem_used_gb: number;      // system-wide used (ALL processes)
  mem_avail_gb: number;     // available to new allocations
  mem_pct: number;          // 0–100
  gpu: GpuStats;
  model_procs: ModelProcMem[];  // per-model-server measured RSS
  model_rss_gb: number;         // sum of model_procs in GiB
}

export interface ModelCapabilities {
  chat: boolean;
  embeddings: boolean;
  thinking: boolean;
  rerank: boolean;
  vision: boolean;
  code: boolean;
}

export interface ModelEntry {
  id: string;
  path: string;
  format: string;
  engine: string;
  hf_repo: string | null;
  size_bytes: number;
  capabilities: ModelCapabilities;
  added_at: string;
}

// ─── REST helpers ─────────────────────────────────────────────────────────────

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) throw new Error(`LF API ${path}: ${res.status}`);
  return res.json() as Promise<T>;
}

async function post<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`LF API ${path}: ${res.status}`);
  return res.json() as Promise<T>;
}

async function del<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(`LF API ${path}: ${res.status}`);
  return res.json() as Promise<T>;
}

// ─── Typed endpoint wrappers ──────────────────────────────────────────────────

/** GET /lf/status — one-shot snapshot (use Tauri Event listener for live updates) */
export const getStatus = (): Promise<LfStatus> => get('/lf/status');

/** GET /lf/hardware — hardware profile (fetch once on mount, doesn't change) */
export const getHardware = (): Promise<HardwareProfile> => get('/lf/hardware');

/** GET /lf/sysinfo — live CPU + real memory pressure (polled every 2 s) */
export const getSysInfo = (): Promise<SysStats> => get('/lf/sysinfo');

/** GET /lf/model/list — installed model index */
export const listModels = (): Promise<{ schema_version: number; models: ModelEntry[] }> =>
  get('/lf/model/list');

/** POST /lf/model/switch — hot-swap active model */
export const switchModel = (modelId: string): Promise<{ status: string }> =>
  post('/lf/model/switch', { model: modelId });

/** POST /lf/model/unload — unload all or specific model from VRAM */
export const unloadModel = (modelId?: string): Promise<{ status: string }> =>
  post('/lf/model/unload', modelId ? { model: modelId } : {});

/** DELETE /lf/model/:name — remove model from index and disk */
export const deleteModel = (id: string): Promise<{ status: string }> =>
  del(`/lf/model/delete/${encodeURIComponent(id)}`);

/** Daemon configuration (subset the UI reads/writes; extra fields preserved on round-trip). */
export interface LfConfig {
  catalogs_dir?: string | null;
  /** Data root (engines, logs, models.json). null/absent = default ~/.lmforge. */
  data_dir?: string | null;
  /** Model weights directory. null/absent = {data_dir}/models. */
  models_dir?: string | null;
  [key: string]: unknown;
}

/** GET /lf/config — current daemon config */
export const getConfig = (): Promise<LfConfig> => get('/lf/config');

/** POST /lf/config — persist config. `restart_required` is true when a storage
 *  dir changed (takes effect on next daemon start). */
export const postConfig = (
  cfg: LfConfig
): Promise<{ status: string; restart_required?: boolean }> => post('/lf/config', cfg);

/** POST /lf/shutdown — graceful daemon shutdown */
export const shutdown = (): Promise<{ status: string }> => post('/lf/shutdown');

/** Request body for POST /lf/storage/apply */
export interface StorageApplyRequest {
  /** New models directory (absolute path). Omit to keep current. */
  models_dir?: string | null;
  /** New data directory (absolute path). Omit to keep current. */
  data_dir?: string | null;
  /** Reset models_dir to its built-in default ({data_dir}/models). Wins over models_dir. */
  reset_models_dir?: boolean;
  /** Reset data_dir to its built-in default (~/.lmforge). Wins over data_dir. */
  reset_data_dir?: boolean;
  /** How to handle existing models in the old models_dir. Default: "adopt". */
  models_action?: 'adopt' | 'delete' | 'repull';
  /** How to handle regenerable artifacts in the old data_dir. Default: "keep". */
  data_action?: 'relocate' | 'keep';
  /** Copy logs/ when relocating data_dir. Default: false. */
  copy_logs?: boolean;
  /** Model IDs to skip re-downloading (they will be lost). Only relevant for models_action="repull". */
  exclude_from_repull?: string[];
}

/** Response from POST /lf/storage/apply */
export interface StorageApplyResponse {
  status: string;
  restart_required: boolean;
  /** Model IDs that cannot be re-downloaded (no hf_repo) — only when models_action="repull". */
  would_lose: string[];
}

/**
 * POST /lf/storage/apply — apply a storage directory change.
 * May return 422 with `{ would_lose: [...] }` when models_action="repull" and
 * some models have no hf_repo. Caller should re-submit with those IDs in
 * `exclude_from_repull` after user confirmation.
 */
export async function applyStorage(req: StorageApplyRequest): Promise<StorageApplyResponse> {
  const res = await fetch(`${BASE}/lf/storage/apply`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  });
  const body = await res.json();
  if (!res.ok) throw Object.assign(new Error(body?.error ?? `HTTP ${res.status}`), { status: res.status, body });
  return body as StorageApplyResponse;
}

/** A single entry from the curated model catalog */
export interface CatalogEntry {
  shortcut: string;     // e.g. "qwen3:8b:4bit"
  hf_repo: string;      // e.g. "mlx-community/Qwen3-8B-4bit"
  format: string;       // "mlx" | "gguf" | "safetensors"
  tags: string[];       // ["qwen3", "8b", "4bit"]
  role: string;         // "chat" | "embed" | "rerank" | "vision" | "code"
  /** GGUF-only: exact .gguf filename to download. Used to fetch the size
   *  of just this quant rather than summing every quant in the repo. */
  file?: string | null;
  /** GGUF-only: VLM multimodal projector filename (llama.cpp --mmproj). */
  mmproj?: string | null;
}

export interface CatalogResponse {
  entries: CatalogEntry[];
}

/** GET /lf/catalog[?format=mlx|gguf] — curated model shortcuts */
export const getCatalog = (format?: string): Promise<CatalogResponse> =>
  get(`/lf/catalog${format ? `?format=${encodeURIComponent(format)}` : ''}`);

// ─── Engine registry (Settings → Engine) ──────────────────────────────────────

/** Tier strings match `lmforge engine list` exactly. Wire badges off this string. */
export type EngineTier = 'default' | 'opt-in' | 'experimental' | 'default*';

/**
 * One engine row from GET /lf/engines. Shape mirrors `cli::engine::list`
 * augmented with the daemon's compatibility verdict for THIS host.
 *
 * - `compatible: null` means the hardware profile is missing (user hasn't run
 *   `lmforge init` yet). UI should suppress install actions in that case.
 * - `active: true` for the engine currently selected by the running daemon.
 */
export interface EngineInfo {
  id: string;
  name: string;
  version: string;
  tier: EngineTier;
  install_method: 'binary' | 'pip' | 'brew' | string;
  model_format: string;
  matches_gpu: string;
  min_compute_cap: string | null;
  max_compute_cap: string | null;
  min_vram_gb: number | null;
  supported_os_families: string[];
  supports_embeddings: boolean;
  supports_reranking: boolean;
  installed: boolean;
  compatible: boolean | null;
  incompatible_reason: string | null;
  active: boolean;
}

export interface EnginesResponse {
  engines: EngineInfo[];
  active_engine_id: string;
  has_hardware_profile: boolean;
}

/** GET /lf/engines — full engine roster + per-host compatibility verdict. */
export const getEngines = (): Promise<EnginesResponse> => get('/lf/engines');

// ─── SSE: model pull progress ─────────────────────────────────────────────────

/**
 * The shape the UI uses internally to track pull progress.
 * Mapped from the Rust DownloadProgress enum variants emitted by /lf/model/pull.
 */
export interface PullProgress {
  file: string;
  downloaded_bytes: number;
  total_bytes: number;
  speed_bps: number;
  done: boolean;
  error?: string;
}

/**
 * POST /lf/model/pull — starts a model download and streams SSE progress.
 * Returns a cancel function.
 *
 * The backend emits Rust enum variants over SSE:
 *   {"Started":{"repo":"...","files":3}}
 *   {"FileProgress":{"file":"config.json","downloaded":1024,"total":2048}}
 *   {"FileCompleted":{"file":"config.json"}}
 *   {"Completed":{"repo":"...","total_bytes":123456}}
 *   {"Failed":{"error":"..."}}
 */
export function pullModel(
  modelId: string,
  onProgress: (p: PullProgress) => void,
  onDone: () => void,
  onError: (msg: string) => void
): () => void {
  let cancelled = false;
  const controller = new AbortController();

  // Track per-file speed
  let lastBytes = 0;
  let lastTime = Date.now();
  let currentSpeed = 0;
  let currentTotal = 0;
  let currentFile = '';

  (async () => {
    try {
      const res = await fetch(`${BASE}/lf/model/pull`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: modelId }),
        signal: controller.signal,
      });

      if (!res.ok || !res.body) {
        if (res.status === 409) {
          let busy = '';
          try { busy = (await res.json())?.model ?? ''; } catch { /* ignore */ }
          onError(
            busy
              ? `A download is already in progress (${busy}). Wait for it to finish.`
              : 'A download is already in progress. Wait for it to finish.'
          );
        } else {
          onError(`Server error ${res.status}`);
        }
        return;
      }

      // Emit a synthetic "connecting" progress immediately so the UI shows activity.
      onProgress({ file: 'Resolving model…', downloaded_bytes: 0, total_bytes: 0, speed_bps: 0, done: false });

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done || cancelled) break;

        buf += decoder.decode(value, { stream: true });
        const lines = buf.split('\n');
        buf = lines.pop() ?? '';

        for (const line of lines) {
          if (!line.startsWith('data: ')) continue;
          let payload: Record<string, unknown>;
          try {
            payload = JSON.parse(line.slice(6));
          } catch {
            continue;
          }

          // ── Rust enum variant dispatch ────────────────────────────────────
          if ('Started' in payload) {
            const v = payload['Started'] as { repo: string; files: number };
            onProgress({
              file: `Preparing ${v.files} file${v.files === 1 ? '' : 's'} from ${v.repo}…`,
              downloaded_bytes: 0, total_bytes: 0, speed_bps: 0, done: false
            });

          } else if ('FileProgress' in payload) {
            const v = payload['FileProgress'] as { file: string; downloaded: number; total: number };
            currentFile = v.file;
            currentTotal = v.total;

            // Compute speed
            const now = Date.now();
            const dt = (now - lastTime) / 1000;
            if (dt > 0.1) {
              currentSpeed = Math.max(0, (v.downloaded - lastBytes) / dt);
              lastBytes = v.downloaded;
              lastTime = now;
            }

            onProgress({
              file: v.file,
              downloaded_bytes: v.downloaded,
              total_bytes: v.total,
              speed_bps: currentSpeed,
              done: false,
            });

          } else if ('FileCompleted' in payload) {
            const v = payload['FileCompleted'] as { file: string };
            onProgress({
              file: `✓ ${v.file}`,
              downloaded_bytes: currentTotal,
              total_bytes: currentTotal,
              speed_bps: 0,
              done: false,
            });
            lastBytes = 0; lastTime = Date.now();

          } else if ('Completed' in payload) {
            onDone();
            return;

          } else if ('Failed' in payload) {
            const v = payload['Failed'] as { error: string };
            onError(v.error);
            return;
          }
        }
      }
    } catch (e) {
      if (!cancelled) onError(String(e));
    }
  })();

  return () => {
    cancelled = true;
    controller.abort();
  };
}

/** Format bytes → human-readable string */
export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

/**
 * Fetch HuggingFace repo siblings (filenames + sizes).
 */
async function fetchHfSiblings(
  hfRepo: string,
): Promise<{ rfilename?: string; size?: number }[] | null> {
  try {
    const res = await fetch(`https://huggingface.co/api/models/${hfRepo}?blobs=true`, {
      headers: { Accept: 'application/json' },
    });
    if (!res.ok) return null;
    const data = await res.json();
    return data.siblings ?? [];
  } catch {
    return null;
  }
}

function ggufBasename(path: string): string {
  return path.split('/').pop() ?? path;
}

function isMmprojSidecar(filename: string): boolean {
  return ggufBasename(filename).startsWith('mmproj-');
}

/** Mirrors `gguf_patterns_for_quant` in src/model/resolver.rs. */
function ggufPatternsForQuant(quant: string): string[] {
  switch (quant.toLowerCase()) {
    case '4bit':
    case 'q4':
      return ['UD-Q4_K_XL', 'Q4_K_M', 'Q4_K_S', 'Q4_K'];
    case '5bit':
    case 'q5':
      return ['UD-Q5_K_XL', 'Q5_K_M', 'Q5_K_S', 'Q5_K'];
    case '6bit':
    case 'q6':
      return ['UD-Q6_K_XL', 'Q6_K'];
    case '8bit':
    case 'q8':
      return ['UD-Q8_K_XL', 'Q8_0'];
    case 'f16':
    case 'bf16':
      return ['F16', 'BF16', 'f16', 'bf16'];
    default:
      return [];
  }
}

function matchesGgufPattern(files: string[], pat: string): string[] {
  const patUp = pat.toUpperCase();
  return files.filter((f) => f.toUpperCase().includes(patUp));
}

/** Mirrors `select_gguf_files` in src/model/resolver.rs (size-estimate path). */
function selectGgufFiles(allGguf: string[], quantHint?: string | null): string[] {
  const weights = allGguf.filter((f) => !isMmprojSidecar(f));
  const patterns = quantHint ? ggufPatternsForQuant(quantHint) : ['Q4_K_S', 'Q4_K_M'];
  for (const pat of patterns) {
    const found = matchesGgufPattern(weights, pat);
    if (found.length > 0) return found;
  }
  return weights.length > 0 ? [weights[0]] : [];
}

function isVlmTarget(shortcut: string, repo: string): boolean {
  const h = shortcut.toLowerCase();
  if (h.includes(':vl:') || h.includes('-vl-') || h.includes('vision')) return true;
  const r = repo.toLowerCase();
  return (
    r.includes('-vl-') ||
    r.includes('-vl') ||
    r.includes('vl-instruct') ||
    r.includes('qwen2.5-vl') ||
    r.includes('qwen3-vl') ||
    r.includes('minicpm-v')
  );
}

function selectMmprojSidecar(allGguf: string[]): string | null {
  const mmprojs = allGguf.filter(isMmprojSidecar);
  if (mmprojs.length === 0) return null;
  for (const tag of ['F16', 'BF16', 'F32']) {
    const hit = mmprojs.find((f) => {
      const base = ggufBasename(f).toUpperCase();
      return base.includes(`-${tag}.`) || base.endsWith(`-${tag}`);
    });
    if (hit) return hit;
  }
  return mmprojs[0];
}

function sumSiblingSizes(
  siblings: { rfilename?: string; size?: number }[],
  filenames: string[],
): number {
  let total = 0;
  for (const fname of filenames) {
    const match = siblings.find((s) => s.rfilename === fname);
    if (match?.size && match.size > 0) total += match.size;
  }
  return total;
}

/**
 * Fetch the disk size (in bytes) for a HuggingFace model repo.
 *
 * - When `fileName` is provided (GGUF entries), returns the size of just
 *   that one file. mradermacher / lmstudio-community style repos pack
 *   8+ quant variants per repo; summing all of them inflates the figure
 *   by ~10× and bears no relation to what the user will actually download.
 *
 * - Without `fileName` (MLX / safetensors — whole repo is downloaded),
 *   returns the sum of all `siblings[].size`. `usedStorage` is not used
 *   because it double-counts LFS blobs.
 *
 * Returns null on error or if no size data is available.
 */
export async function fetchHfModelSize(
  hfRepo: string,
  fileName?: string | null,
): Promise<number | null> {
  const siblings = await fetchHfSiblings(hfRepo);
  if (!siblings) return null;
  if (fileName) {
    const match = siblings.find((s) => s.rfilename === fileName);
    return match?.size && match.size > 0 ? match.size : null;
  }
  const total = siblings.reduce((sum, s) => sum + (s.size ?? 0), 0);
  return total > 0 ? total : null;
}

/** Size lookup input. `key` is the map key in the batch result (use catalog shortcut). */
export interface HfSizeQuery {
  key: string;
  repo: string;
  file?: string | null;
  format?: string;
  /** Last `:segment` of the shortcut, e.g. `4bit` / `f16`. */
  quant?: string | null;
  shortcut?: string;
}

/**
 * Batch-fetch HF model sizes.
 * Returns a map of `key` → bytes. Failed lookups are silently omitted.
 * For GGUF catalog entries pass `{ format: 'gguf', quant, shortcut }` so the
 * size reflects the quant-specific `.gguf` (+ mmproj for VLMs), not the whole repo.
 */
export async function fetchHfSizesBatch(
  queries: HfSizeQuery[],
): Promise<Record<string, number>> {
  const siblingsCache = new Map<string, { rfilename?: string; size?: number }[] | null>();

  async function siblingsFor(repo: string) {
    if (!siblingsCache.has(repo)) {
      siblingsCache.set(repo, await fetchHfSiblings(repo));
    }
    return siblingsCache.get(repo) ?? null;
  }

  const map: Record<string, number> = {};

  await Promise.all(
    queries.map(async ({ key, repo, file, format, quant, shortcut }) => {
      const siblings = await siblingsFor(repo);
      if (!siblings) return;

      if (file) {
        const bytes = sumSiblingSizes(siblings, [file]);
        if (bytes > 0) map[key] = bytes;
        return;
      }

      if (format === 'gguf' && quant) {
        const allGguf = siblings
          .map((s) => s.rfilename)
          .filter((f): f is string => !!f && f.endsWith('.gguf'));
        let files = selectGgufFiles(allGguf, quant);
        if (shortcut && isVlmTarget(shortcut, repo)) {
          const mmproj = selectMmprojSidecar(allGguf);
          if (mmproj && !files.includes(mmproj)) files = [...files, mmproj];
        }
        const bytes = sumSiblingSizes(siblings, files);
        if (bytes > 0) map[key] = bytes;
        return;
      }

      const total = siblings.reduce((sum, s) => sum + (s.size ?? 0), 0);
      if (total > 0) map[key] = total;
    }),
  );

  return map;
}

/** Format seconds → "3h 24m" */
export function fmtUptime(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

// ─── Observability: metrics digest + log tail/follow ─────────────────────────

/** Per-endpoint stats from /lf/metrics. Mirrors EndpointStats in metrics_api.rs. */
export interface EndpointStats {
  requests_total: number;
  errors_total: number;
  by_status: Record<string, number>;
  p50_ms: number | null;
  p95_ms: number | null;
  p99_ms: number | null;
}

export interface ModelLoadStats {
  success: number;
  failure: number;
  last_dur_s: number | null;
}

export interface ImageMix {
  accepted: number;
  rejected: number;
  data_url: number;
}

/** Digest of /metrics, parsed into stable JSON for dashboard widgets. */
export interface MetricsDigest {
  endpoints: Record<string, EndpointStats>;
  requests_total: number;
  errors_total: number;
  error_rate: number;
  active_models: number;
  model_loads: Record<string, ModelLoadStats>;
  image_inputs: ImageMix;
  auth_rejections: number;
  uptime_secs: number;
  recorder_unavailable: boolean;
}

/** GET /lf/metrics — JSON digest for the observability dashboard. */
export const getMetricsDigest = (): Promise<MetricsDigest> => get('/lf/metrics');

export interface LogStream {
  stream: 'stdout' | 'stderr' | string;
  size_bytes: number;
  mtime_secs: number;
}

export interface LogComponent {
  component: string;
  component_safe: string;
  streams: LogStream[];
}

export interface LogIndex {
  components: LogComponent[];
}

/** GET /lf/logs/list — discover available log streams. */
export const listLogs = (): Promise<LogIndex> => get('/lf/logs/list');

/** GET /lf/logs/tail — last N lines as plain text. Bounded at 5000 lines / 2 MB. */
export async function tailLog(
  component: string,
  stream: 'stdout' | 'stderr' | 'main' = 'stderr',
  lines = 200
): Promise<string> {
  const params = new URLSearchParams({
    component,
    stream,
    lines: String(lines),
  });
  const res = await fetch(`${BASE}/lf/logs/tail?${params}`);
  if (!res.ok) throw new Error(`tailLog ${component}/${stream}: ${res.status}`);
  return res.text();
}

/**
 * GET /lf/logs/stream — SSE follow. Each new line emits one event with
 * `{ "line": "..." }`. Returns a cancel function.
 */
export function streamLog(
  component: string,
  stream: 'stdout' | 'stderr' | 'main',
  onLine: (line: string) => void,
  onError: (msg: string) => void
): () => void {
  const params = new URLSearchParams({ component, stream });
  const url = `${BASE}/lf/logs/stream?${params}`;
  let cancelled = false;
  const controller = new AbortController();

  (async () => {
    try {
      const res = await fetch(url, { signal: controller.signal });
      if (!res.ok || !res.body) {
        onError(`Server error ${res.status}`);
        return;
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';
      while (true) {
        const { done, value } = await reader.read();
        if (done || cancelled) break;
        buf += decoder.decode(value, { stream: true });
        const events = buf.split('\n\n');
        buf = events.pop() ?? '';
        for (const evt of events) {
          for (const line of evt.split('\n')) {
            if (!line.startsWith('data: ')) continue;
            const payload = line.slice(6);
            if (payload === '{}') continue; // heartbeat
            try {
              const obj = JSON.parse(payload);
              if (typeof obj.line === 'string') onLine(obj.line);
            } catch {
              // ignore malformed events
            }
          }
        }
      }
    } catch (e) {
      if (!cancelled) onError(String(e));
    }
  })();

  return () => {
    cancelled = true;
    controller.abort();
  };
}
