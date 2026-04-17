/**
 * sysInfoStore — live CPU + real system memory, polled every 2 s.
 *
 * Backed by GET /lf/sysinfo which uses sysinfo::used_memory() — the true
 * system-wide memory pressure (all processes), not just our model estimates.
 *
 * Call startSysInfoPolling() once on app mount; stopSysInfoPolling() on destroy.
 */

import { writable, get } from 'svelte/store';
import { getSysInfo, type SysStats } from '$lib/api';

export const sysInfoStore = writable<SysStats | null>(null);
export const sysInfoError = writable<string | null>(null);

const POLL_INTERVAL_MS = 2000;

let timer: ReturnType<typeof setInterval> | null = null;

async function poll(): Promise<void> {
  try {
    const stats = await getSysInfo();
    sysInfoStore.set(stats);
    sysInfoError.set(null);
  } catch (e) {
    sysInfoError.set(String(e));
  }
}

export function startSysInfoPolling(): void {
  if (timer !== null) return; // already running
  poll(); // immediate first fetch
  timer = setInterval(poll, POLL_INTERVAL_MS);
}

export function stopSysInfoPolling(): void {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}
