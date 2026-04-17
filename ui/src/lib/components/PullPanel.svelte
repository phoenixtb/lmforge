<script lang="ts">
  import { pullModel, fmtBytes, type PullProgress } from '$lib/api';
  import { toast } from '$lib/stores/toasts';
  import { createEventDispatcher } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';

  const dispatch = createEventDispatcher<{ done: void }>();

  export let prefill = '';   // pre-filled from Discover tab

  let modelIdInput = prefill;
  let pulling      = false;
  let progress: PullProgress | null = null;
  let cancel: (() => void) | null  = null;
  let error: string | null = null;

  $: pct = progress
    ? progress.total_bytes > 0
      ? Math.round((progress.downloaded_bytes / progress.total_bytes) * 100)
      : 0
    : 0;

  $: speed = progress?.speed_bps
    ? `${fmtBytes(progress.speed_bps)}/s`
    : '';

  async function browsePath() {
    try {
      const selected = await open({ directory: true, title: 'Select model directory' });
      if (selected && typeof selected === 'string') {
        modelIdInput = selected;
      }
    } catch {
      // dialog cancelled or unavailable
    }
  }

  function startPull() {
    if (!modelIdInput.trim() || pulling) return;
    pulling = true;
    error   = null;
    progress = null;

    cancel = pullModel(
      modelIdInput.trim(),
      (p) => { progress = p; },
      () => {
        pulling  = false;
        progress = null;
        cancel   = null;
        toast.success(`✓ ${modelIdInput} downloaded successfully`);
        dispatch('done');
      },
      (msg) => {
        pulling = false;
        error   = msg;
        cancel  = null;
      }
    );
  }

  function cancelPull() {
    cancel?.();
    cancel   = null;
    pulling  = false;
    progress = null;
  }
</script>

<div class="pull-panel">
  <h4>Pull a Model</h4>

  <!-- Input row -->
  <div class="input-row">
    <input
      id="model-source-input"
      type="text"
      class="model-input"
      placeholder="HuggingFace repo (e.g. mlx-community/Qwen3-8B-4bit) or local path"
      bind:value={modelIdInput}
      disabled={pulling}
      onkeydown={(e) => e.key === 'Enter' && startPull()}
    />
    <button class="btn btn--ghost btn--sm" onclick={browsePath} disabled={pulling} title="Browse local path">
      Browse…
    </button>
    {#if !pulling}
      <button
        class="btn btn--primary"
        onclick={startPull}
        disabled={!modelIdInput.trim()}
        id="pull-model-btn"
      >
        Pull
      </button>
    {:else}
      <button class="btn btn--danger" onclick={cancelPull}>Cancel</button>
    {/if}
  </div>

  <!-- Progress -->
  {#if pulling && progress}
    <div class="progress-section" aria-live="polite">
      <div class="progress-file mono">{progress.file || 'Preparing…'}</div>
      <div class="progress-bar-track">
        <div
          class="progress-bar-fill"
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
    <div class="progress-section">
      <div class="skeleton" style="height:8px;width:100%;border-radius:4px;"></div>
      <div class="progress-meta"><span>Connecting…</span></div>
    </div>
  {/if}

  <!-- Error -->
  {#if error}
    <div class="pull-error" role="alert">
      <strong>Error:</strong> {error}
    </div>
  {/if}
</div>

<style>
  .pull-panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .input-row {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .model-input {
    flex: 1;
    background: var(--surface-2);
    border: 1px solid var(--border-2);
    border-radius: var(--radius-sm);
    color: var(--text);
    font-family: var(--font-sans);
    font-size: 12.5px;
    padding: 7px 10px;
    outline: none;
    transition: border-color 120ms ease;
    user-select: text;
  }
  .model-input:focus { border-color: var(--accent); }
  .model-input::placeholder { color: var(--text-3); }
  .model-input:disabled { opacity: 0.6; }

  .progress-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .progress-file {
    font-size: 11px;
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .progress-bar-track {
    height: 6px;
    background: var(--surface-3);
    border-radius: 99px;
    overflow: hidden;
  }

  .progress-bar-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 99px;
    transition: width 300ms ease;
  }

  .progress-meta {
    display: flex;
    justify-content: space-between;
    font-size: 10.5px;
    color: var(--text-3);
  }

  .pull-error {
    background: var(--danger-dim);
    border: 1px solid var(--danger);
    border-radius: var(--radius-sm);
    padding: 10px 12px;
    font-size: 12px;
    color: var(--danger);
  }
</style>
