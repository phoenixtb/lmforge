/**
 * metricsStore — observability digest from GET /lf/metrics, polled every 5 s.
 *
 * Polling is opt-in via startMetricsPolling() so routes that don't need
 * metrics (Models, Settings) don't pay the network cost. Stop on destroy
 * to keep the daemon's request rate clean.
 */

import { writable } from 'svelte/store';
import { getMetricsDigest, type MetricsDigest } from '$lib/api';

const EMPTY: MetricsDigest = {
  endpoints: {},
  requests_total: 0,
  errors_total: 0,
  error_rate: 0,
  active_models: 0,
  model_loads: {},
  image_inputs: { accepted: 0, rejected: 0, data_url: 0 },
  auth_rejections: 0,
  uptime_secs: 0,
  recorder_unavailable: false,
};

export const metricsStore = writable<MetricsDigest>(EMPTY);
export const metricsError = writable<string | null>(null);

const POLL_INTERVAL_MS = 5000;

let timer: ReturnType<typeof setInterval> | null = null;
let refCount = 0;

async function poll(): Promise<void> {
  try {
    const d = await getMetricsDigest();
    metricsStore.set(d);
    metricsError.set(null);
  } catch (e) {
    metricsError.set(String(e));
  }
}

/**
 * Reference-counted polling: the first subscriber starts the timer, the
 * last unsubscribe stops it. Safe to call from multiple components.
 */
export function startMetricsPolling(): void {
  refCount += 1;
  if (timer !== null) return;
  poll();
  timer = setInterval(poll, POLL_INTERVAL_MS);
}

export function stopMetricsPolling(): void {
  refCount = Math.max(0, refCount - 1);
  if (refCount > 0) return;
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}
