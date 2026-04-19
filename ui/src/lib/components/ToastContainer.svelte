<script lang="ts">
  import { toasts, dismiss, type Toast } from '$lib/stores/toasts';

  const ICONS: Record<string, string> = {
    success: '✓',
    error:   '✕',
    warn:    '⚠',
    info:    'ℹ',
  };
</script>

{#if $toasts.length > 0}
  <div class="toast-viewport" aria-live="polite" aria-atomic="false">
    {#each $toasts as t (t.id)}
      <div class="toast toast--{t.kind}" role="alert">
        <span class="toast-icon">{ICONS[t.kind]}</span>
        <span class="toast-msg">{t.message}</span>
        <button class="toast-close" onclick={() => dismiss(t.id)} aria-label="Dismiss">✕</button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .toast-viewport {
    position: fixed;
    bottom: 20px;
    right: 20px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 9999;
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    border-radius: var(--radius);
    border: 1px solid rgba(255,255,255,0.08);
    border-left-width: 3px;
    font-size: 12.5px;
    font-weight: 500;
    /* Solid opaque surface — never transparent */
    background: #2c2c32;
    box-shadow: 0 4px 20px rgba(0,0,0,0.55), 0 1px 4px rgba(0,0,0,0.35);
    pointer-events: all;
    animation: slide-in-up 200ms cubic-bezier(0.22, 1, 0.36, 1);
    max-width: 360px;
    min-width: 220px;
    /* Text is always near-white for readability */
    color: rgba(255, 255, 255, 0.90);
  }

  /* Left-border accent + icon color differ per kind */
  .toast--success { border-left-color: var(--success); }
  .toast--success .toast-icon { color: var(--success); }

  .toast--error   { border-left-color: var(--danger); }
  .toast--error   .toast-icon { color: var(--danger); }

  .toast--warn    { border-left-color: var(--warn); }
  .toast--warn    .toast-icon { color: var(--warn); }

  .toast--info    { border-left-color: var(--accent); }
  .toast--info    .toast-icon { color: var(--accent-2); }

  .toast-icon { font-size: 13px; flex-shrink: 0; }
  .toast-msg  { flex: 1; line-height: 1.4; }

  .toast-close {
    background: none;
    border: none;
    color: rgba(255, 255, 255, 0.38);
    cursor: pointer;
    font-size: 11px;
    padding: 0 2px;
    flex-shrink: 0;
    transition: color 100ms;
  }
  .toast-close:hover { color: rgba(255, 255, 255, 0.75); }

  @keyframes slide-in-up {
    from { opacity: 0; transform: translateY(10px) scale(0.97); }
    to   { opacity: 1; transform: translateY(0)    scale(1); }
  }
</style>
