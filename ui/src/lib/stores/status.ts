/**
 * statusStore — reactive Svelte store for EngineState.
 *
 * Transport: Tauri Events (`lf:status` and `lf:health`).
 * The Tauri rust side polls http://127.0.0.1:11430 every 2 s and emits:
 *   - "lf:health"  { online: boolean }     — daemon reachability
 *   - "lf:status"  EngineState snapshot   — full engine state
 *
 * Usage:
 *   import { statusStore, isOnline, isDaemonOnline } from '$lib/stores/status';
 */

import { writable, derived } from 'svelte/store';
import type { LfStatus } from '$lib/api';

/** Initial placeholder while waiting for first event */
const initial: LfStatus = {
  overall_status: 'stopped',
  engine_id: '—',
  engine_version: '—',
  running_models: {},
  metrics: {
    requests_total: 0,
    ttft_avg_ms: 0,
    uptime_secs: 0,
    restart_count: 0,
  },
};

/** The primary status store. Written by the Tauri event listener in +layout.svelte */
export const statusStore = writable<LfStatus>(initial);

/**
 * Whether the daemon process is reachable on :11430.
 * Driven by the `lf:health` Tauri event.
 * `null` = not yet checked (app just launched).
 */
export const daemonOnline = writable<boolean | null>(null);

/** true when the daemon is reachable and in a non-stopped state */
export const isOnline = derived(
  [statusStore, daemonOnline],
  ([$s, $online]) => $online === true && $s.overall_status !== 'stopped'
);

/** true while we haven't yet received the first health event */
export const isConnecting = derived(
  daemonOnline,
  ($online) => $online === null
);

/** The first running model's ID, or null if none loaded */
export const activeModelId = derived(statusStore, ($s) => {
  const slots = Object.values($s.running_models);
  return slots.length > 0 ? slots[0].model_id : null;
});
