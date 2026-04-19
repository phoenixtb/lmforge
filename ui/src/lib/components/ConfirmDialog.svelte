<script lang="ts">
  /**
   * ConfirmDialog — inline modal confirmation dialog.
   *
   * Usage:
   *   <ConfirmDialog
   *     open={showConfirm}
   *     title="Delete model?"
   *     message="This will permanently remove the files from disk."
   *     confirmLabel="Delete"
   *     danger={true}
   *     onconfirm={() => doDelete()}
   *     oncancel={() => showConfirm = false}
   *   />
   */

  let {
    open       = false,
    title      = 'Are you sure?',
    message    = '',
    confirmLabel = 'Confirm',
    cancelLabel  = 'Cancel',
    danger     = false,
    onconfirm,
    oncancel,
  }: {
    open?: boolean;
    title?: string;
    message?: string;
    confirmLabel?: string;
    cancelLabel?: string;
    danger?: boolean;
    onconfirm?: () => void;
    oncancel?: () => void;
  } = $props();

  function handleConfirm() {
    onconfirm?.();
  }
  function handleCancel() {
    oncancel?.();
  }

  function handleBackdrop(e: MouseEvent) {
    if ((e.target as Element).classList.contains('dialog-backdrop')) handleCancel();
  }
  function handleKey(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === 'Escape') handleCancel();
    if (e.key === 'Enter')  handleConfirm();
  }
</script>

<svelte:window onkeydown={handleKey} />

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="dialog-backdrop" onclick={handleBackdrop} role="dialog" aria-modal="true" aria-labelledby="dlg-title">
    <div class="dialog" class:danger>
      <div class="dlg-header">
        {#if danger}<span class="dlg-icon">⚠</span>{/if}
        <h3 id="dlg-title" class="dlg-title">{title}</h3>
      </div>
      {#if message}
        <p class="dlg-message">{message}</p>
      {/if}
      <div class="dlg-actions">
        <button class="btn btn--ghost btn--sm" onclick={handleCancel}>{cancelLabel}</button>
        <button
          class="btn btn--sm"
          class:btn--danger={danger}
          class:btn--primary={!danger}
          onclick={handleConfirm}
          autofocus
        >
          {confirmLabel}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .dialog-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(3px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    animation: fade-in 120ms ease;
  }

  .dialog {
    background: var(--surface-2);
    border: 1px solid var(--border-2);
    border-radius: var(--radius-lg);
    padding: 20px 22px;
    min-width: 300px;
    max-width: 420px;
    width: 90%;
    box-shadow: var(--shadow-lg, 0 20px 60px rgba(0,0,0,.5));
    animation: slide-up 140ms ease;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .dialog.danger { border-color: color-mix(in srgb, var(--danger) 40%, transparent); }

  .dlg-header {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dlg-icon  { font-size: 16px; color: var(--danger); flex-shrink: 0; }
  .dlg-title { font-size: 14px; font-weight: 600; color: var(--text); margin: 0; }

  .dlg-message {
    font-size: 12.5px;
    color: var(--text-2);
    line-height: 1.55;
    margin: 0;
  }

  .dlg-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }

  @keyframes slide-up {
    from { transform: translateY(12px); opacity: 0; }
    to   { transform: translateY(0);    opacity: 1; }
  }
</style>
