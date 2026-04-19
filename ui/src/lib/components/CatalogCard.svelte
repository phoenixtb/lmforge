<script lang="ts">
  import { pullModel, fmtBytes, type CatalogEntry, type PullProgress } from '$lib/api';
  import { toast } from '$lib/stores/toasts';
  import { statusStore } from '$lib/stores/status';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';

  export let entry: CatalogEntry;
  /** Set of installed model IDs / HF repos — for "Already installed" indicator */
  export let installedIds: Set<string> = new Set();
  /** Called after a successful pull so the parent can refresh the installed list */
  export let onPulled: () => void = () => {};
  /** When true (Discover tab), show a link to open the model page on HuggingFace */
  export let showHfLink = false;

  // ── Pull state ──────────────────────────────────────────────────────────────
  let pulling    = false;
  let progress: PullProgress | null = null;
  let cancelFn: (() => void) | null = null;
  let pullError: string | null = null;

  $: installed = installedIds.has(entry.shortcut) || installedIds.has(entry.hf_repo);

  $: pct = progress && progress.total_bytes > 0
    ? Math.round((progress.downloaded_bytes / progress.total_bytes) * 100)
    : 0;
  $: speed = progress?.speed_bps ? `${fmtBytes(progress.speed_bps)}/s` : '';

  /**
   * Real size in bytes from HuggingFace:
   *   undefined = batch fetch still in flight → show loading skeleton
   *   null      = HF returned no size data   → show nothing
   *   number    = real bytes                 → show formatted size
   */
  export let sizeBytes: number | null | undefined = undefined;
  $: sizeLabel = (sizeBytes != null && sizeBytes > 0) ? fmtBytes(sizeBytes) : null;

  // ── Engine compatibility ──────────────────────────────────────────────────────
  $: requiredFormat = (() => {
    const id = ($statusStore.engine_id ?? '').toLowerCase();
    if (id.includes('mlx'))                          return 'mlx';
    if (id.includes('llama') || id.includes('gguf')) return 'gguf';
    return null; // unknown engine — no restriction
  })();
  $: incompatible = !!requiredFormat && !!entry.format && entry.format !== requiredFormat;
  let showIncompatWarning = false;

  // ── Role → badge colour ──────────────────────────────────────────────────────
  const ROLE_CLS: Record<string, string> = {
    chat:   'badge--green',
    embed:  'badge--blue',
    rerank: 'badge--amber',
    vision: 'badge--blue',
    code:   'badge--grey',
  };
  const FORMAT_CLS: Record<string, string> = {
    mlx:  'badge--purple',
    gguf: 'badge--grey',
  };

  function startPull() {
    if (pulling || installed) return;
    if (incompatible) {
      showIncompatWarning = true; // ask user to confirm before pulling wrong format
      return;
    }
    doPull();
  }

  function doPull() {
    pulling   = true;
    pullError = null;
    progress  = null;

    cancelFn = pullModel(
      entry.shortcut,
      (p) => { progress = p; },
      () => {
        pulling  = false;
        progress = null;
        cancelFn = null;
        toast.success(`✓ ${entry.shortcut} downloaded`);
        onPulled();
      },
      (msg) => {
        pulling  = false;
        pullError = msg;
        cancelFn  = null;
      }
    );
  }

  function cancelPull() {
    cancelFn?.();
    cancelFn = null;
    pulling  = false;
    progress = null;
  }
</script>

<ConfirmDialog
  open={showIncompatWarning}
  title="Format mismatch"
  message={`This model is ${entry.format.toUpperCase()} format, but your engine requires ${(requiredFormat ?? '').toUpperCase()}. Pulling it may fail or produce errors at load time.`}
  confirmLabel="Pull anyway"
  cancelLabel="Cancel"
  danger={false}
  onconfirm={() => { showIncompatWarning = false; doPull(); }}
  oncancel={() => showIncompatWarning = false}
/>

<div class="cat-card" class:pulling class:installed class:incompatible>
  <!-- ── Header ── -->
  <div class="cat-top">
    <div class="cat-id-row">
      <span class="cat-shortcut mono">{entry.shortcut}</span>
      {#if installed}
        <span class="installed-badge">✓ installed</span>
      {/if}
    </div>
    <div class="cat-badges">
      <span class="badge {ROLE_CLS[entry.role] ?? 'badge--grey'}">{entry.role}</span>
      <span class="badge {FORMAT_CLS[entry.format] ?? 'badge--grey'}">{entry.format.toUpperCase()}</span>
    </div>
  </div>

  <!-- ── Tags ── -->
  <div class="cat-tags">
    {#each entry.tags as tag}
      <span class="tag-chip">{tag}</span>
    {/each}
  </div>

  <!-- ── Repo + size ── -->
  <div class="cat-meta-row">
    <div class="cat-repo mono" title={entry.hf_repo}>{entry.hf_repo}</div>
    {#if sizeLabel}
      <span class="cat-size">{sizeLabel}</span>
    {:else if sizeBytes === undefined}
      <!-- Batch fetch still in flight — pulsing placeholder -->
      <span class="size-loading"></span>
    {/if}
  </div>

  <!-- ── Pull progress ── -->
  {#if pulling && progress}
    <div class="pull-progress" aria-live="polite">
      <div class="progress-file mono">{progress.file || 'Preparing…'}</div>
      <div class="progress-track">
        <div
          class="progress-fill"
          style="width:{pct}%"
          role="progressbar"
          aria-valuenow={pct}
          aria-valuemin={0}
          aria-valuemax={100}
        ></div>
      </div>
      <div class="progress-meta">
        <span>{pct}%</span>
        <span>{fmtBytes(progress.downloaded_bytes)} / {fmtBytes(progress.total_bytes)}</span>
        {#if speed}<span>{speed}</span>{/if}
      </div>
    </div>
  {:else if pulling}
    <div class="pull-progress">
      <div class="skeleton" style="height:6px;width:100%;border-radius:4px;"></div>
      <div class="progress-meta"><span>Connecting…</span></div>
    </div>
  {/if}

  <!-- ── Error ── -->
  {#if pullError}
    <div class="pull-error" role="alert">{pullError}</div>
  {/if}

  <!-- ── Compat warning ── -->
  {#if incompatible && !pulling && !installed}
    <div class="compat-warn" role="alert">
      ⚠ {entry.format.toUpperCase()} — your engine needs {(requiredFormat ?? '').toUpperCase()}
    </div>
  {/if}

  <!-- ── Actions ── -->
  <div class="cat-actions">
    {#if showHfLink}
      <a
        href="https://huggingface.co/{entry.hf_repo}"
        target="_blank"
        rel="noopener noreferrer"
        class="hf-action-link"
        title="Open on HuggingFace"
      >HF ↗</a>
    {/if}
    {#if pulling}
      <button class="btn btn--danger btn--sm" onclick={cancelPull}>Cancel</button>
    {:else if installed}
      <span class="btn btn--ghost btn--sm disabled" aria-disabled="true">Installed</span>
    {:else}
      <button class="btn btn--primary btn--sm" onclick={startPull} id="pull-{entry.shortcut}">
        Pull ↓
      </button>
    {/if}
  </div>
</div>

<style>
  .cat-card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 13px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    transition: border-color 140ms ease, box-shadow 140ms ease, transform 140ms ease;
    animation: fade-in 180ms ease;
  }
  .cat-card:hover { border-color: var(--border-2); transform: translateY(-1px); box-shadow: var(--shadow-sm); }
  .cat-card.installed   { border-color: var(--success); background: var(--success-dim); }
  .cat-card.pulling     { border-color: var(--accent); }
  .cat-card.incompatible { border-color: var(--warn, #f59e0b); }

  /* Compat warning */
  .compat-warn {
    font-size: 10.5px;
    color: var(--warn, #f59e0b);
    background: color-mix(in srgb, var(--warn, #f59e0b) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn, #f59e0b) 35%, transparent);
    border-radius: var(--radius-sm);
    padding: 4px 8px;
  }

  /* Header */
  .cat-top { display: flex; justify-content: space-between; align-items: flex-start; gap: 8px; }
  .cat-id-row { display: flex; align-items: center; gap: 6px; flex: 1; min-width: 0; }
  .cat-shortcut {
    font-size: 12.5px; font-weight: 600; color: var(--text);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .installed-badge {
    font-size: 10px; color: var(--success); background: var(--success-dim);
    padding: 1px 6px; border-radius: 99px; flex-shrink: 0; white-space: nowrap;
  }
  .cat-badges { display: flex; gap: 4px; flex-shrink: 0; }

  /* Tags */
  .cat-tags { display: flex; flex-wrap: wrap; gap: 4px; }
  .tag-chip {
    padding: 2px 8px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: 99px;
    font-size: 10.5px;
    color: var(--text-2);
    font-family: var(--font-mono);
  }

  /* Repo + size row */
  .cat-meta-row {
    display: flex; align-items: center; gap: 8px; min-width: 0;
  }
  .cat-repo {
    font-size: 10.5px; color: var(--text-3);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    flex: 1; min-width: 0;
  }
  .cat-size {
    font-size: 10.5px; color: var(--text-2); font-weight: 500;
    background: var(--surface-3); border-radius: 4px;
    padding: 1px 6px; white-space: nowrap; flex-shrink: 0;
  }
  .size-loading {
    display: inline-block; width: 36px; height: 12px;
    background: var(--surface-3); border-radius: 4px; flex-shrink: 0;
    animation: pulse 1.4s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { opacity: 0.4; }
    50%       { opacity: 1;   }
  }

  /* Progress */
  .pull-progress { display: flex; flex-direction: column; gap: 5px; }
  .progress-file { font-size: 10.5px; color: var(--text-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .progress-track {
    height: 5px; background: var(--surface-3); border-radius: 99px; overflow: hidden;
  }
  .progress-fill {
    height: 100%; background: var(--accent); border-radius: 99px;
    transition: width 300ms ease;
  }
  .progress-meta {
    display: flex; justify-content: space-between;
    font-size: 10px; color: var(--text-3);
  }

  /* Error */
  .pull-error {
    font-size: 11px; color: var(--danger);
    background: var(--danger-dim); border: 1px solid var(--danger);
    border-radius: var(--radius-sm); padding: 6px 10px;
  }

  /* Actions */
  .cat-actions { margin-top: auto; display: flex; align-items: center; justify-content: flex-end; gap: 8px; }
  .disabled { opacity: 0.5; cursor: default; pointer-events: none; }

  .hf-action-link {
    font-size: 11px; color: var(--text-3);
    text-decoration: none; padding: 3px 8px;
    border: 1px solid var(--border); border-radius: var(--radius-sm);
    transition: all 110ms ease;
    white-space: nowrap;
  }
  .hf-action-link:hover { color: var(--accent-2); border-color: var(--accent); }

  /* badge--purple for MLX */
  :global(.badge--purple) {
    background: rgba(139, 92, 246, 0.15);
    color: #a78bfa;
    border-color: rgba(139, 92, 246, 0.3);
  }
</style>
