/**
 * dismissedErrors — client-side dismissal for Engine Load Errors.
 *
 * The daemon's `last_errors` map is re-pushed on every 2 s /lf/status poll and
 * has no dismiss endpoint (ADR-003: dismissal is a UI concern). We track
 * dismissed *occurrences* keyed by `${model_id}@${at}` so:
 *   - dismissing hides that specific failure even though the daemon keeps
 *     reporting it,
 *   - a NEW failure for the same model (new `at`) re-appears automatically.
 *
 * Persisted in sessionStorage so a reload within the session keeps dismissals
 * but a fresh session starts clean (matching the daemon's own per-session
 * error lifetime).
 */

import { get, writable } from 'svelte/store';

const KEY = 'lmforge.dismissedLoadErrors';

/** Stable key for one failure occurrence. */
export function errorKey(modelId: string, at: string): string {
  return `${modelId}@${at}`;
}

function load(): Set<string> {
  if (typeof sessionStorage === 'undefined') return new Set();
  try {
    const raw = sessionStorage.getItem(KEY);
    if (!raw) return new Set();
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? new Set(arr.filter((x) => typeof x === 'string')) : new Set();
  } catch {
    return new Set();
  }
}

function persist(s: Set<string>): void {
  if (typeof sessionStorage === 'undefined') return;
  try {
    sessionStorage.setItem(KEY, JSON.stringify([...s]));
  } catch {
    /* sessionStorage full / unavailable — dismissal is best-effort */
  }
}

export const dismissedErrors = writable<Set<string>>(load());

/** Dismiss a single failure occurrence. */
export function dismissError(key: string): void {
  dismissedErrors.update((s) => {
    if (s.has(key)) return s;
    const next = new Set(s);
    next.add(key);
    persist(next);
    return next;
  });
}

/**
 * Drop dismissed keys that no longer correspond to a live error, so the set
 * cannot grow unbounded as models churn. Call with the keys currently present
 * in `last_errors`.
 */
export function pruneDismissed(liveKeys: Set<string>): void {
  // Read-compute-then-conditionally-set (NOT `update`): a Svelte store notifies
  // subscribers on every `set`/`update` call even when the value is unchanged.
  // `pruneDismissed` is called from an `$effect` that transitively depends on
  // this store, so an unconditional notify here would retrigger that effect
  // forever (`effect_update_depth_exceeded`) and freeze the component. Only
  // write when something actually changed.
  const s = get(dismissedErrors);
  const next = new Set<string>();
  let changed = false;
  for (const k of s) {
    if (liveKeys.has(k)) next.add(k);
    else changed = true;
  }
  if (changed) {
    persist(next);
    dismissedErrors.set(next);
  }
}
