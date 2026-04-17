/**
 * Shared drag utility.
 *
 * Usage in Svelte template:
 *   <div onpointerdown={dragOnEmpty}>  ← drag when clicking empty space
 *   <div onpointerdown={dragAlways}>   ← drag on any click (no interactive children)
 *
 * Requires `window:allow-start-dragging` in capabilities/default.json.
 */

import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * Start window drag if the event target IS the element (not a child button/link).
 * Use on toolbars that contain interactive children — clicking a button won't drag.
 */
export async function dragOnEmpty(e: PointerEvent): Promise<void> {
  if (e.button !== 0) return;
  if (e.target !== e.currentTarget) return; // child element — don't drag
  try { await getCurrentWindow().startDragging(); } catch { /* no-op */ }
}

/**
 * Always start drag on left pointer down.
 * Use on drag-only regions with no interactive children (e.g. the tl-zone).
 */
export async function dragAlways(e: PointerEvent): Promise<void> {
  if (e.button !== 0) return;
  try { await getCurrentWindow().startDragging(); } catch { /* no-op */ }
}
