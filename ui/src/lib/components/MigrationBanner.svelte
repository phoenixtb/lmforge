<script lang="ts">
  import { migration, activePull } from '$lib/stores/status';
  import { cancelMigration, retryMigration, fmtBytes } from '$lib/api';
  import { toast } from '$lib/stores/toasts';

  let busy = $state(false);
  // Toast a finished migration exactly once (the status snapshot keeps arriving
  // on every 2 s /lf/status poll until the banner is dismissed).
  let toastedDone = $state(false);

  // % for the model currently downloading (from the shared active_pull snapshot —
  // both manual pulls and the migration drive the same snapshot, so this is a
  // global indicator for every download, not just migrations).
  let pct = $derived(
    $activePull && $activePull.total_bytes > 0
      ? Math.round(($activePull.downloaded_bytes / $activePull.total_bytes) * 100)
      : 0
  );
  let indeterminate = $derived(!!$activePull && $activePull.total_bytes === 0 && !$activePull.error);

  // Show the thin top line while a migration is running or any pull is active.
  let showBar = $derived(!!$activePull || (!!$migration && !$migration.done));
  // Models processed so far in a migration (successes + failures).
  let processed = $derived($migration ? $migration.completed + $migration.failed.length : 0);

  let label = $derived((() => {
    const ap = $activePull;
    const m = $migration;
    if (m && !m.done) {
      const who = ap?.model ?? m.current ?? '…';
      return `Re-downloading ${Math.min(processed + 1, m.total)}/${m.total} · ${who}${ap && ap.total_bytes > 0 ? ` ${pct}%` : ''}`;
    }
    if (ap) {
      return `${ap.error ? '✕ ' : ''}${ap.model}${ap.total_bytes > 0 ? ` ${pct}%` : ''}`;
    }
    return '';
  })());

  let tooltip = $derived((() => {
    const ap = $activePull;
    if (!ap) return label;
    const bytes = ap.total_bytes > 0 ? ` (${fmtBytes(ap.downloaded_bytes)} / ${fmtBytes(ap.total_bytes)})` : '';
    return `${ap.model} — ${ap.error ? `✕ ${ap.error}` : ap.file}${bytes}`;
  })());

  $effect(() => {
    const m = $migration;
    if (m?.done && !toastedDone) {
      toastedDone = true;
      if (m.failed.length === 0) {
        toast.success(`✓ Re-downloaded ${m.total} model${m.total === 1 ? '' : 's'}`);
      }
    }
    if (!m) toastedDone = false;
  });

  async function onCancel() {
    busy = true;
    try { await cancelMigration(); } catch (e) { toast.error(String(e)); } finally { busy = false; }
  }

  async function onRetry() {
    busy = true;
    try { await retryMigration(); toastedDone = false; } catch (e) { toast.error(String(e)); } finally { busy = false; }
  }
</script>

<!-- Thin progress line pinned to the bottom edge of the page header. -->
{#if showBar}
  <div class="dl-line" class:err={$activePull?.error} role="status" aria-live="polite">
    <div class="dl-track">
      {#if indeterminate}
        <div class="dl-fill dl-fill--indet"></div>
      {:else}
        <div class="dl-fill" style="width:{pct}%"></div>
      {/if}
    </div>
    {#if label}
      <span class="dl-label mono" title={tooltip}>{label}</span>
    {/if}
  </div>
{/if}

<!-- Compact corner card only when a migration finished with failures (needs actions). -->
{#if $migration?.done && $migration.failed.length > 0}
  {@const m = $migration}
  <div class="dl-card" role="alert">
    <div class="dl-card-body">
      <div class="dl-card-title">{m.failed.length} of {m.total} re-downloads failed</div>
      <div class="dl-card-sub mono" title={m.failed.join(', ')}>{m.failed.join(', ')}</div>
    </div>
    <div class="dl-card-actions">
      <button class="btn btn--primary btn--sm" onclick={onRetry} disabled={busy}>Retry</button>
      <button class="btn btn--ghost btn--sm" onclick={onCancel} disabled={busy}>Dismiss</button>
    </div>
  </div>
{/if}

<style>
  /* Hairline progress bar sitting exactly at the header's bottom border. The
     content-region is the positioning context (position: relative). */
  .dl-line {
    position: absolute;
    top: var(--toolbar-h);
    left: 0;
    right: 0;
    height: 0;
    z-index: 40;
    pointer-events: none;
  }
  .dl-track {
    position: absolute;
    top: -1px;
    left: 0;
    right: 0;
    height: 2px;
    background: color-mix(in srgb, var(--accent, #6ee7b7) 16%, transparent);
    overflow: hidden;
  }
  .dl-fill {
    height: 100%;
    background: var(--accent, #6ee7b7);
    box-shadow: 0 0 6px color-mix(in srgb, var(--accent, #6ee7b7) 60%, transparent);
    transition: width 300ms ease;
  }
  .dl-line.err .dl-fill { background: var(--danger, #f87171); box-shadow: none; }

  /* Indeterminate sweep while resolving / before total bytes are known. */
  .dl-fill--indet {
    width: 35%;
    animation: dl-indet 1.1s ease-in-out infinite;
  }
  @keyframes dl-indet {
    0%   { transform: translateX(-120%); }
    100% { transform: translateX(320%); }
  }

  .dl-label {
    position: absolute;
    top: 5px;
    right: 16px;
    max-width: 60%;
    font-size: 10.5px;
    color: var(--text-3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 0 0 6px 6px;
    border-top: none;
    padding: 2px 8px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: auto;
  }

  /* Compact failure card, bottom-right (above the toast stack region). */
  .dl-card {
    position: fixed;
    bottom: 16px;
    right: 16px;
    z-index: 900;
    display: flex;
    align-items: center;
    gap: 12px;
    max-width: 380px;
    background: var(--surface-2);
    border: 1px solid var(--danger, #f87171);
    border-radius: var(--radius-lg, 12px);
    padding: 10px 12px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
  }
  .dl-card-body { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 3px; }
  .dl-card-title { font-size: 12px; font-weight: 600; color: var(--text); }
  .dl-card-sub {
    font-size: 10.5px; color: var(--text-3);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .dl-card-actions { display: flex; gap: 6px; flex-shrink: 0; }

  .mono { font-family: var(--font-mono, 'JetBrains Mono', monospace); }
</style>
