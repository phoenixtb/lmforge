<script lang="ts">
  import type { ModelEntry } from '$lib/api';
  import { fmtBytes } from '$lib/api';
  import { activeModelId } from '$lib/stores/status';
  import { toast } from '$lib/stores/toasts';
  import { switchModel, deleteModel } from '$lib/api';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';

  export let model: ModelEntry;
  export let onDeleted: (id: string) => void = () => {};

  let switching    = false;
  let deleting     = false;
  let showConfirm  = false;

  $: isActive   = $activeModelId === model.id;
  $: sizeLabel  = model.size_bytes > 0 ? fmtBytes(model.size_bytes) : '—';
  $: addedDate  = model.added_at ? new Date(model.added_at).toLocaleDateString() : '—';

  // Format capabilities into chip list
  $: caps = Object.entries(model.capabilities ?? {})
    .filter(([, v]) => v)
    .map(([k]) => k);

  async function handleSwitch() {
    if (isActive || switching) return;
    switching = true;
    try {
      await switchModel(model.id);
      toast.success(`Switched to ${model.id}`);
    } catch (e) {
      toast.error(`Failed to switch: ${e}`);
    } finally {
      switching = false;
    }
  }

  function requestDelete() {
    if (deleting) return;
    showConfirm = true;
  }

  async function confirmDelete() {
    showConfirm = false;
    deleting = true;
    try {
      await deleteModel(model.id);
      toast.success(`Deleted ${model.id}`);
      onDeleted(model.id);
    } catch (e) {
      toast.error(`Failed to delete: ${e}`);
    } finally {
      deleting = false;
    }
  }
</script>

<ConfirmDialog
  open={showConfirm}
  title="Delete model?"
  message={`"${model.id}" will be removed from the index and its files deleted from disk. This cannot be undone.`}
  confirmLabel="Delete"
  danger={true}
  onconfirm={confirmDelete}
  oncancel={() => showConfirm = false}
/>

<div class="model-card" class:active={isActive} role="listitem">
  <div class="card-top">
    <div class="card-id">
      {#if isActive}
        <span class="dot dot--ready" style="flex-shrink:0;"></span>
      {/if}
      <span class="id-text mono">{model.id}</span>
    </div>
    <div class="card-badges">
      {#if model.format}
        <span class="badge badge--grey">{model.format.toUpperCase()}</span>
      {/if}
      {#if model.engine}
        <span class="badge badge--blue">{model.engine}</span>
      {/if}
    </div>
  </div>

  <div class="card-caps">
    {#each caps as cap}
      <span class="cap-chip">{cap}</span>
    {/each}
    {#if caps.length === 0}
      <span class="cap-chip dim">no capabilities detected</span>
    {/if}
  </div>

  <div class="card-footer">
    <div class="card-meta">
      <span class="meta-item">{sizeLabel}</span>
      <span class="meta-sep">·</span>
      <span class="meta-item">Added {addedDate}</span>
    </div>
    <div class="card-actions">
      <button
        class="btn btn--ghost btn--sm"
        onclick={handleSwitch}
        disabled={isActive || switching}
        title={isActive ? 'Currently active' : 'Set as active model'}
      >
        {switching ? 'Switching…' : isActive ? '✓ Active' : 'Set Active'}
      </button>
      <button
        class="btn btn--danger btn--sm"
        onclick={requestDelete}
        disabled={deleting}
        title="Delete model"
      >
        {deleting ? '…' : '🗑'}
      </button>
    </div>
  </div>
</div>

<style>
  .model-card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    transition: border-color 150ms ease, box-shadow 150ms ease, transform 150ms ease;
    animation: fade-in 180ms ease;
  }
  .model-card:hover {
    border-color: var(--border-2);
    box-shadow: var(--shadow-sm);
    transform: translateY(-1px);
  }
  .model-card.active {
    border-color: var(--success);
    background: var(--success-dim);
  }

  .card-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .card-id {
    display: flex;
    align-items: center;
    gap: 6px;
    overflow: hidden;
    flex: 1;
  }

  .id-text {
    font-size: 12.5px;
    font-weight: 500;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .card-badges {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }

  .card-caps {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .cap-chip {
    padding: 2px 7px;
    background: var(--surface-3);
    border-radius: var(--radius-xs);
    font-size: 10.5px;
    color: var(--text-2);
    text-transform: capitalize;
  }
  .cap-chip.dim { color: var(--text-3); font-style: italic; }

  .card-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-top: 2px;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: 5px;
  }

  .meta-item { font-size: 11px; color: var(--text-3); }
  .meta-sep  { font-size: 10px; color: var(--border-2); }

  .card-actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }
</style>
