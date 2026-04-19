<script lang="ts">
  import { pullModel, fmtBytes, type PullProgress } from '$lib/api';
  import { toast } from '$lib/stores/toasts';
  import { statusStore } from '$lib/stores/status';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import { open } from '@tauri-apps/plugin-dialog';

  // Svelte 5 prop — pre-filled model ID (e.g. from a future entry point)
  let { prefill = '', ondone }: { prefill?: string; ondone?: () => void } = $props();

  let modelIdInput = $state(prefill);
  let pulling       = $state(false);
  let progress      = $state<PullProgress | null>(null);
  let cancelFn      = $state<(() => void) | null>(null);
  let error         = $state<string | null>(null);
  let succeeded     = $state(false);

  // Update input if prefill prop changes (e.g. parent navigates here with a value)
  $effect(() => {
    if (prefill && !pulling) modelIdInput = prefill;
  });

  let pct   = $derived(progress && progress.total_bytes > 0
    ? Math.round((progress.downloaded_bytes / progress.total_bytes) * 100)
    : 0);
  let speed = $derived(progress?.speed_bps ? `${fmtBytes(progress.speed_bps)}/s` : '');

  // Detect whether input looks like a local path or an HF repo
  let inputType = $derived((() => {
    const v = modelIdInput.trim();
    if (!v) return 'empty';
    if (v.startsWith('/') || v.startsWith('~') || v.startsWith('./') || /^[A-Za-z]:\\/.test(v)) return 'local';
    if (v.includes('/')) return 'hf-repo';
    return 'shortcut'; // catalog shortcut like "qwen3:8b:4bit"
  })());

  // ── Engine compatibility ──────────────────────────────────────────────────────
  let requiredFormat = $derived((() => {
    const id = ($statusStore.engine_id ?? '').toLowerCase();
    if (id.includes('mlx'))                          return 'mlx';
    if (id.includes('llama') || id.includes('gguf')) return 'gguf';
    return null;
  })());

  /** Guess the format of the model from the repo/shortcut string */
  let inputFormat = $derived((() => {
    const v = modelIdInput.trim().toLowerCase();
    if (!v || inputType === 'local') return null; // local paths: skip format check
    if (v.includes('mlx-community') || v.includes(':mlx') || v.endsWith('/mlx') || v.includes('-mlx')) return 'mlx';
    if (v.includes('gguf') || v.includes('q4_') || v.includes('q5_') || v.includes('q8_') || v.includes(':gguf')) return 'gguf';
    return null; // can't determine — allow pull without warning
  })());

  let incompatible = $derived(!!requiredFormat && !!inputFormat && inputFormat !== requiredFormat);
  let showIncompatWarning = $state(false);

  async function browsePath() {
    try {
      const selected = await open({ directory: true, title: 'Select model directory' });
      if (selected && typeof selected === 'string') modelIdInput = selected;
    } catch {
      // dialog cancelled or unavailable in web mode
    }
  }

  function startPull() {
    if (!modelIdInput.trim() || pulling) return;
    if (incompatible) {
      showIncompatWarning = true;
      return;
    }
    doPull();
  }

  function doPull() {
    pulling   = true;
    error     = null;
    progress  = null;
    succeeded = false;

    cancelFn = pullModel(
      modelIdInput.trim(),
      (p) => { progress = p; },
      () => {
        pulling   = false;
        progress  = null;
        cancelFn  = null;
        succeeded = true;
        toast.success(`✓ ${modelIdInput.trim()} downloaded`);
        ondone?.();
      },
      (msg) => {
        pulling  = false;
        error    = msg;
        cancelFn = null;
      }
    );
  }

  function cancelPull() {
    cancelFn?.();
    cancelFn  = null;
    pulling   = false;
    progress  = null;
  }

  function reset() {
    modelIdInput = '';
    error        = null;
    succeeded    = false;
    progress     = null;
  }
</script>

<ConfirmDialog
  open={showIncompatWarning}
  title="Format mismatch"
  message={`This looks like a ${inputFormat?.toUpperCase()} model, but your engine requires ${requiredFormat?.toUpperCase()}. Pulling it may fail or not load correctly.`}
  confirmLabel="Pull anyway"
  cancelLabel="Cancel"
  danger={false}
  onconfirm={() => { showIncompatWarning = false; doPull(); }}
  oncancel={() => showIncompatWarning = false}
/>

<div class="add-panel">

  <!-- Header -->
  <div class="ap-header">
    <h2 class="ap-title">Add Custom Model</h2>
    <p class="ap-sub">Pull any model from HuggingFace by repo ID, use a catalog shortcut, or point to a local directory.</p>
  </div>

  <!-- Input card -->
  <div class="ap-card" class:pulling class:succeeded>
    <div class="input-row">
      <div class="input-wrap">
        <input
          id="model-source-input"
          type="text"
          class="model-input"
          placeholder="e.g.  mlx-community/Qwen3-8B-4bit  ·  qwen3:8b:4bit  ·  /path/to/model"
          bind:value={modelIdInput}
          disabled={pulling}
          onkeydown={(e) => e.key === 'Enter' && startPull()}
        />
        {#if inputType !== 'empty' && !pulling}
          <span class="input-type-badge" class:local={inputType === 'local'} class:shortcut={inputType === 'shortcut'}>
            {inputType === 'hf-repo' ? 'HF repo' : inputType === 'local' ? 'local path' : 'shortcut'}
          </span>
        {/if}
      </div>
      <button class="btn btn--ghost btn--sm browse-btn" onclick={browsePath} disabled={pulling} title="Pick a local model directory">
        Browse…
      </button>
    </div>

    <!-- Compat warning -->
    {#if incompatible && !pulling}
      <div class="compat-warn" role="alert">
        ⚠ This looks like a <strong>{inputFormat?.toUpperCase()}</strong> model but your engine requires <strong>{requiredFormat?.toUpperCase()}</strong>.
        You can still pull it, but it may not load correctly.
      </div>
    {/if}

    <!-- Progress -->
    {#if pulling && progress}
      <div class="progress-section" aria-live="polite">
        <div class="progress-file mono">{progress.file}</div>
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
        <div class="progress-meta"><span>Resolving…</span></div>
      </div>
    {:else if succeeded}
      <div class="success-banner" role="status">
        ✓ Model downloaded successfully
        <button class="btn btn--ghost btn--sm" onclick={reset}>Pull another</button>
      </div>
    {/if}

    <!-- Error -->
    {#if error}
      <div class="pull-error" role="alert">
        <span class="error-icon">✕</span>
        <div>
          <strong>Pull failed</strong>
          <div class="error-detail">{error}</div>
        </div>
      </div>
    {/if}

    <!-- Actions -->
    <div class="ap-actions">
      {#if pulling}
        <button class="btn btn--danger" onclick={cancelPull}>Cancel</button>
      {:else}
        <button
          class="btn btn--primary"
          onclick={startPull}
          disabled={!modelIdInput.trim()}
          id="pull-model-btn"
        >
          Pull ↓
        </button>
      {/if}
    </div>
  </div>

  <!-- Help section -->
  <div class="help-grid">
    <div class="help-card">
      <div class="help-icon">🤗</div>
      <div class="help-title">HuggingFace Repo</div>
      <div class="help-desc">Paste a full HF repo path — LMForge resolves and downloads the right files automatically.</div>
      <div class="help-example mono">mlx-community/Qwen3-8B-4bit</div>
    </div>
    <div class="help-card">
      <div class="help-icon">⚡</div>
      <div class="help-title">Catalog Shortcut</div>
      <div class="help-desc">Use a shortcut from the Recommended tab — the shorthand resolves to the correct HF repo and quantization.</div>
      <div class="help-example mono">qwen3:8b:4bit</div>
    </div>
    <div class="help-card">
      <div class="help-icon">📁</div>
      <div class="help-title">Local Directory</div>
      <div class="help-desc">Point to an existing local model directory. Click <em>Browse…</em> to pick one with the file picker.</div>
      <div class="help-example mono">/path/to/my-model</div>
    </div>
  </div>

</div>

<style>
  .add-panel {
    display: flex;
    flex-direction: column;
    gap: 20px;
    max-width: 680px;
  }

  /* Header */
  .ap-header { display: flex; flex-direction: column; gap: 4px; }
  .ap-title { font-size: 15px; font-weight: 600; color: var(--text); margin: 0; }
  .ap-sub   { font-size: 12.5px; color: var(--text-2); margin: 0; line-height: 1.5; }

  /* Card */
  .ap-card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    transition: border-color 140ms ease;
  }
  .ap-card.pulling      { border-color: var(--accent); }
  .ap-card.succeeded    { border-color: var(--success); background: var(--success-dim); }

  /* Compat warning */
  .compat-warn {
    font-size: 11.5px;
    color: var(--warn, #f59e0b);
    background: color-mix(in srgb, var(--warn, #f59e0b) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn, #f59e0b) 35%, transparent);
    border-radius: var(--radius-sm);
    padding: 8px 10px;
    line-height: 1.5;
  }

  /* Input */
  .input-row { display: flex; gap: 8px; align-items: center; }
  .input-wrap { flex: 1; position: relative; display: flex; align-items: center; }
  .model-input {
    flex: 1;
    background: var(--surface-3);
    border: 1px solid var(--border-2);
    border-radius: var(--radius-sm);
    color: var(--text);
    font-family: var(--font-sans);
    font-size: 12.5px;
    padding: 8px 12px;
    padding-right: 80px;
    outline: none;
    transition: border-color 120ms ease;
    user-select: text;
    width: 100%;
  }
  .model-input:focus { border-color: var(--accent); }
  .model-input::placeholder { color: var(--text-3); }
  .model-input:disabled { opacity: 0.6; }

  .input-type-badge {
    position: absolute; right: 10px;
    font-size: 10px; padding: 2px 6px; border-radius: 4px;
    background: var(--surface); border: 1px solid var(--border);
    color: var(--text-3); white-space: nowrap; pointer-events: none;
  }
  .input-type-badge.local    { color: var(--accent-2); border-color: var(--accent); }
  .input-type-badge.shortcut { color: var(--warn);     border-color: var(--warn);   }

  .browse-btn { white-space: nowrap; flex-shrink: 0; }

  /* Progress */
  .progress-section { display: flex; flex-direction: column; gap: 5px; }
  .progress-file {
    font-size: 11px; color: var(--text-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .progress-bar-track {
    height: 6px; background: var(--surface-3); border-radius: 99px; overflow: hidden;
  }
  .progress-bar-fill {
    height: 100%; background: var(--accent); border-radius: 99px;
    transition: width 300ms ease;
  }
  .progress-meta {
    display: flex; justify-content: space-between;
    font-size: 10.5px; color: var(--text-3);
  }

  /* Success banner */
  .success-banner {
    display: flex; align-items: center; justify-content: space-between; gap: 10px;
    font-size: 12.5px; color: var(--success); font-weight: 500;
    background: var(--success-dim); border: 1px solid var(--success);
    border-radius: var(--radius-sm); padding: 8px 12px;
  }

  /* Error */
  .pull-error {
    display: flex; align-items: flex-start; gap: 10px;
    background: var(--danger-dim); border: 1px solid var(--danger);
    border-radius: var(--radius-sm); padding: 10px 12px;
    font-size: 12px; color: var(--danger);
  }
  .error-icon { font-size: 14px; flex-shrink: 0; margin-top: 1px; }
  .error-detail { font-size: 11.5px; margin-top: 2px; opacity: 0.85; word-break: break-all; }

  /* Actions */
  .ap-actions { display: flex; justify-content: flex-end; }

  /* Help grid */
  .help-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 10px;
  }
  .help-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 14px;
    display: flex; flex-direction: column; gap: 6px;
    transition: border-color 120ms ease;
  }
  .help-card:hover { border-color: var(--border-2); }
  .help-icon  { font-size: 20px; }
  .help-title { font-size: 12px; font-weight: 600; color: var(--text); }
  .help-desc  { font-size: 11.5px; color: var(--text-2); line-height: 1.5; flex: 1; }
  .help-example {
    font-size: 10.5px; color: var(--accent-2);
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 4px; padding: 3px 7px;
    word-break: break-all;
  }
</style>
