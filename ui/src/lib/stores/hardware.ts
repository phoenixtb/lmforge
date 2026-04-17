/**
 * hardwareStore — fetched once from GET /lf/hardware on app mount.
 * Hardware doesn't change at runtime, so no polling or SSE needed.
 * Errors are re-thrown so the caller can implement retry logic.
 */

import { writable } from 'svelte/store';
import { getHardware, type HardwareProfile } from '$lib/api';

export const hardwareStore = writable<HardwareProfile | null>(null);
export const hardwareError = writable<string | null>(null);

export async function loadHardware(): Promise<void> {
  try {
    const hw = await getHardware();
    hardwareStore.set(hw);
    hardwareError.set(null);
  } catch (e) {
    hardwareError.set(String(e));
    throw e;   // re-throw so callers can retry
  }
}
