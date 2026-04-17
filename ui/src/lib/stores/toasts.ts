/**
 * Toast notification store.
 *
 * Usage:
 *   import { toast } from '$lib/stores/toasts';
 *   toast.success('Model switched!');
 *   toast.error('Failed to delete model');
 */

import { writable } from 'svelte/store';

export type ToastKind = 'success' | 'error' | 'warn' | 'info';

export interface Toast {
  id: string;
  kind: ToastKind;
  message: string;
  /** ms until auto-dismiss (default 4000) */
  duration: number;
}

export const toasts = writable<Toast[]>([]);

function add(kind: ToastKind, message: string, duration = 4000): void {
  const id = crypto.randomUUID();
  toasts.update((ts) => [...ts, { id, kind, message, duration }]);
  setTimeout(() => dismiss(id), duration);
}

export function dismiss(id: string): void {
  toasts.update((ts) => ts.filter((t) => t.id !== id));
}

export const toast = {
  success: (msg: string, dur?: number) => add('success', msg, dur),
  error:   (msg: string, dur?: number) => add('error', msg, dur ?? 6000),
  warn:    (msg: string, dur?: number) => add('warn', msg, dur),
  info:    (msg: string, dur?: number) => add('info', msg, dur),
};
