/**
 * LMForge API Client — typed wrappers for all /lf/* endpoints.
 *
 * All Svelte components import from here — no raw fetch() anywhere else.
 * Mirrors the Rust structs in src/engine/manager.rs and src/server/native.rs.
 */

const BASE = 'http://localhost:11430';

// ─── Types ───────────────────────────────────────────────────────────────────

export type EngineStatus = 'stopped' | 'starting' | 'ready' | 'degraded' | 'error';

export interface ModelSlot {
  model_id: string;
  port: number;
  status: EngineStatus;
  idle_secs: number;
  vram_est_gb: number;
  role: string;
}

export interface EngineMetrics {
  requests_total: number;
  ttft_avg_ms: number;
  uptime_secs: number;
  restart_count: number;
}

/**
 * Normalised frontend status — always use this shape in stores/components.
 * The daemon returns a slightly different JSON shape; normalizeStatus() maps it.
 */
export interface LfStatus {
  overall_status: EngineStatus;
  engine_id: string;
  engine_version: string;
  running_models: Record<string, ModelSlot>;
  metrics: EngineMetrics;
}

/** Raw shape from GET /lf/status and the SSE stream */
interface RawStatus {
  overall_status: EngineStatus;
  engine?: { id: string; version: string };
  engine_id?: string;
  engine_version?: string;
  running_models: ModelSlot[] | Record<string, ModelSlot>;
  metrics: EngineMetrics;
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

  return { overall_status: raw.overall_status, engine_id, engine_version, running_models, metrics: raw.metrics };
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

export interface PullProgress {
  file: string;
  downloaded_bytes: number;
  total_bytes: number;
  speed_bps: number;
  done: boolean;
  error?: string;
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

/** GET /lf/config — current daemon config */
export const getConfig = (): Promise<unknown> => get('/lf/config');

/** POST /lf/shutdown — graceful daemon shutdown */
export const shutdown = (): Promise<{ status: string }> => post('/lf/shutdown');

// ─── SSE: model pull progress ─────────────────────────────────────────────────

/**
 * POST /lf/model/pull — starts a model download and streams SSE progress.
 * Returns a cancel function that closes the underlying EventSource.
 *
 * Note: model pull uses a POST body so we open a fetch stream manually
 * rather than EventSource (which only supports GET).
 */
export function pullModel(
  modelId: string,
  onProgress: (p: PullProgress) => void,
  onDone: () => void,
  onError: (msg: string) => void
): () => void {
  let cancelled = false;
  const controller = new AbortController();

  (async () => {
    try {
      const res = await fetch(`${BASE}/lf/model/pull`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: modelId }),
        signal: controller.signal,
      });

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
        const lines = buf.split('\n');
        buf = lines.pop() ?? '';

        for (const line of lines) {
          if (line.startsWith('data: ')) {
            try {
              const progress: PullProgress = JSON.parse(line.slice(6));
              onProgress(progress);
              if (progress.done) { onDone(); return; }
              if (progress.error) { onError(progress.error); return; }
            } catch { /* malformed line */ }
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

/** Format seconds → "3h 24m" */
export function fmtUptime(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}
