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
    gap: 8px;
    padding: 10px 14px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    font-size: 12.5px;
    font-weight: 500;
    box-shadow: var(--shadow);
    pointer-events: all;
    animation: slide-in-up 200ms ease;
    max-width: 340px;
  }

  .toast--success { background: var(--success-dim); border-color: var(--success); color: var(--success); }
  .toast--error   { background: var(--danger-dim);  border-color: var(--danger);  color: var(--danger); }
  .toast--warn    { background: var(--warn-dim);    border-color: var(--warn);    color: var(--warn); }
  .toast--info    { background: var(--accent-dim);  border-color: var(--accent);  color: var(--accent-2); }

  .toast-icon { font-size: 13px; flex-shrink: 0; }
  .toast-msg  { flex: 1; line-height: 1.4; }

  .toast-close {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    opacity: 0.6;
    font-size: 11px;
    padding: 0 2px;
    flex-shrink: 0;
    transition: opacity 100ms;
  }
  .toast-close:hover { opacity: 1; }
</style>
