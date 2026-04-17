<script lang="ts">
  import { onMount } from 'svelte';
  import { listModels, type ModelEntry } from '$lib/api';
  import { statusStore } from '$lib/stores/status';
  import { hardwareStore } from '$lib/stores/hardware';
  import { toast } from '$lib/stores/toasts';
  import { dragOnEmpty } from '$lib/drag';
  import ModelCard from '$lib/components/ModelCard.svelte';
  import PullPanel from '$lib/components/PullPanel.svelte';

  // ── Tabs ───────────────────────────────────────────────────────────────────
  type Tab = 'installed' | 'discover' | 'add';
  let activeTab: Tab = 'installed';

  // ── Installed ──────────────────────────────────────────────────────────────
  let models: ModelEntry[] = [];
  let loadingModels = true;
  let modelsError: string | null = null;

  async function fetchModels() {
    loadingModels = true;
    modelsError   = null;
    try {
      const res = await listModels();
      models = res.models;
    } catch (e) {
      modelsError = String(e);
    } finally {
      loadingModels = false;
    }
  }

  onMount(fetchModels);

  function onModelDeleted(id: string) {
    models = models.filter((m) => m.id !== id);
    toast.success(`Removed ${id}`);
  }

  // ── Discover ───────────────────────────────────────────────────────────────
  let searchQuery = '';
  let searchResults: HfModel[] = [];
  let searching = false;
  let searchError: string | null = null;
  let searchTimer: ReturnType<typeof setTimeout>;

  interface HfModel {
    id: string; author: string; downloads: number;
    lastModified: string; tags: string[]; size_gb?: number;
  }

  $: engineFilter = (() => {
    const id = $statusStore.engine_id.toLowerCase();
    if (id.includes('mlx'))              return 'mlx';
    if (id.includes('llama') || id.includes('gguf')) return 'gguf';
    return '';
  })();

  $: vramGb = $hardwareStore?.vram_gb ?? 0;

  function vramBadge(sizeGb?: number): { label: string; cls: string } {
    if (!sizeGb || !vramGb) return { label: '?',           cls: 'badge--grey'  };
    if (sizeGb <= vramGb * 0.7) return { label: '✓ Fits',     cls: 'badge--green' };
    if (sizeGb <= vramGb)       return { label: '⚠ Marginal', cls: 'badge--amber' };
    return                             { label: '✗ Too large', cls: 'badge--red'   };
  }

  function onSearchInput() {
    clearTimeout(searchTimer);
    if (searchQuery.trim().length < 2) { searchResults = []; return; }
    searchTimer = setTimeout(doSearch, 350);
  }

  async function doSearch() {
    if (!searchQuery.trim()) return;
    searching = true; searchError = null;
    try {
      const filter = engineFilter ? `&filter=${engineFilter}` : '';
      const url = `https://huggingface.co/api/models?search=${encodeURIComponent(searchQuery)}&limit=20&sort=downloads${filter}`;
      const data = await (await fetch(url)).json();
      searchResults = data.map((m: Record<string,unknown>) => ({
        id: m.id, author: (m.id as string).split('/')[0],
        downloads: m.downloads ?? 0, lastModified: m.lastModified ?? '',
        tags: m.tags ?? [], size_gb: undefined,
      }));
    } catch (e) { searchError = String(e); }
    finally     { searching = false; }
  }

  function pullFromDiscover(id: string) { activeTab = 'add'; pullPrefill = id; }

  // ── Add ────────────────────────────────────────────────────────────────────
  let pullPrefill = '';
  function onPullDone() { fetchModels(); activeTab = 'installed'; }

  const TABS: { id: Tab; label: string }[] = [
    { id: 'installed', label: 'Installed' },
    { id: 'discover',  label: 'Discover'  },
    { id: 'add',       label: 'Add Custom'},
  ];
</script>

<svelte:head><title>LMForge — Model Library</title></svelte:head>

<!-- Renders inside .content-region from +layout.svelte — no sidebar here -->
<div class="page">

  <!-- Toolbar -->
  <div class="toolbar" data-tauri-drag-region onpointerdown={dragOnEmpty}>
    <h1>Model Library</h1>
    <div class="toolbar-right">
      {#if !loadingModels}
        <span class="badge badge--grey">{models.length} installed</span>
      {/if}
    </div>
  </div>

  <!-- Tab bar -->
  <div class="tab-bar" role="tablist">
    {#each TABS as t}
      <button
        id="tab-{t.id}" role="tab"
        aria-selected={activeTab === t.id}
        class="tab-btn" class:active={activeTab === t.id}
        onclick={() => (activeTab = t.id)}
      >
        {t.label}
        {#if t.id === 'installed' && models.length > 0}
          <span class="tab-count">{models.length}</span>
        {/if}
      </button>
    {/each}
  </div>

  <!-- Tab panels -->
  <div class="tab-body">

    <!-- Installed -->
    {#if activeTab === 'installed'}
      {#if loadingModels}
        <div class="loading-grid">
          {#each Array(3) as _}
            <div class="skeleton" style="height:100px;border-radius:var(--radius-lg);"></div>
          {/each}
        </div>
      {:else if modelsError}
        <div class="tab-error" role="alert">
          Failed to load models: {modelsError}
          <button class="btn btn--ghost btn--sm" onclick={fetchModels} style="margin-top:10px;">Retry</button>
        </div>
      {:else if models.length === 0}
        <div class="empty-full">
          <div class="es-icon">📦</div>
          <h3>No models installed</h3>
          <p>Pull a model from the <strong>Discover</strong> tab or add a custom one.</p>
          <button class="btn btn--primary" onclick={() => (activeTab = 'discover')}>
            Browse Discover →
          </button>
        </div>
      {:else}
        <div class="model-grid" role="list">
          {#each models as model (model.id)}
            <ModelCard {model} onDeleted={onModelDeleted} />
          {/each}
        </div>
      {/if}

    <!-- Discover -->
    {:else if activeTab === 'discover'}
      <div class="discover-panel">
        <div class="search-row">
          <input
            id="discover-search" type="search"
            class="search-input"
            placeholder="Search HuggingFace (e.g. Qwen3, Llama, Mistral)…"
            bind:value={searchQuery}
            oninput={onSearchInput}
            onkeydown={(e) => e.key === 'Enter' && doSearch()}
          />
          {#if engineFilter}
            <span class="badge badge--blue" title="Pre-filtered for your engine">{engineFilter}</span>
          {/if}
          <button class="btn btn--primary"
            onclick={doSearch}
            disabled={searching || searchQuery.trim().length < 2}>
            {searching ? '…' : 'Search'}
          </button>
        </div>

        {#if searchError}
          <div class="tab-error">{searchError}</div>
        {:else if searching}
          <div class="loading-grid">
            {#each Array(4) as _}
              <div class="skeleton" style="height:80px;border-radius:var(--radius-lg);"></div>
            {/each}
          </div>
        {:else if searchResults.length === 0 && searchQuery.trim().length >= 2}
          <div class="empty-full"><p>No results for "<strong>{searchQuery}</strong>"</p></div>
        {:else}
          <div class="hf-grid">
            {#each searchResults as m}
              {@const vb = vramBadge(m.size_gb)}
              <div class="hf-card">
                <div class="hf-card-top">
                  <span class="hf-id mono">{m.id}</span>
                  <span class="badge {vb.cls}">{vb.label}</span>
                </div>
                <div class="hf-meta">↓ {m.downloads.toLocaleString()} downloads
                  {#if m.tags.length > 0} · {m.tags.slice(0,3).join(', ')}{/if}
                </div>
                <button class="btn btn--ghost btn--sm"
                  style="margin-top:auto;align-self:flex-start;"
                  onclick={() => pullFromDiscover(m.id)}>
                  Pull →
                </button>
              </div>
            {/each}
          </div>
        {/if}
      </div>

    <!-- Add -->
    {:else}
      <div class="add-panel">
        <PullPanel prefill={pullPrefill} on:done={onPullDone} />
      </div>
    {/if}

  </div><!-- /tab-body -->
</div><!-- /page -->

<style>
  /* Renders inside .content-region (flex col, full height) from layout */
  .page { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

  /* Toolbar */
  .toolbar {
    height: var(--toolbar-h);
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 20px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .toolbar h1 { font-size: 14px; font-weight: 600; color: var(--text); }
  .toolbar-right { display: flex; align-items: center; gap: 8px; }

  /* Tab bar */
  .tab-bar {
    display: flex; gap: 2px;
    padding: 10px 20px 0;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .tab-btn {
    display: flex; align-items: center; gap: 6px;
    padding: 7px 14px; background: none; border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-3);
    font-family: var(--font-sans); font-size: 13px; font-weight: 500;
    cursor: pointer; margin-bottom: -1px;
    transition: color 110ms ease, border-color 110ms ease;
  }
  .tab-btn:hover { color: var(--text-2); }
  .tab-btn.active { color: var(--accent-2); border-bottom-color: var(--accent); }
  .tab-count {
    background: var(--surface-3); border-radius: 99px;
    font-size: 10px; padding: 0 5px; color: var(--text-2); line-height: 16px;
  }

  /* Tab body */
  .tab-body { flex: 1; overflow-y: auto; padding: 18px 20px; }

  /* Grids */
  .model-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: 12px;
  }
  .loading-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 12px;
  }

  /* Error */
  .tab-error {
    background: var(--danger-dim); border: 1px solid var(--danger);
    border-radius: var(--radius-lg); padding: 14px 16px;
    color: var(--danger); font-size: 12.5px;
    display: flex; flex-direction: column;
  }

  /* Empty full-panel state */
  .empty-full {
    display: flex; flex-direction: column; align-items: center;
    justify-content: center; min-height: 200px; gap: 12px;
    color: var(--text-2); text-align: center;
  }
  .es-icon { font-size: 40px; }
  .empty-full h3 { font-size: 15px; color: var(--text); }
  .empty-full p  { font-size: 13px; color: var(--text-2); max-width: 280px; }

  /* Discover */
  .discover-panel { display: flex; flex-direction: column; gap: 16px; }
  .search-row     { display: flex; gap: 8px; align-items: center; }
  .search-input {
    flex: 1;
    background: var(--surface-2); border: 1px solid var(--border-2);
    border-radius: var(--radius-sm); color: var(--text);
    font-family: var(--font-sans); font-size: 12.5px;
    padding: 7px 10px; outline: none;
    transition: border-color 110ms ease; user-select: text;
  }
  .search-input:focus { border-color: var(--accent); }
  .search-input::placeholder { color: var(--text-3); }

  .hf-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 10px; }
  .hf-card {
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: var(--radius-lg); padding: 12px;
    display: flex; flex-direction: column; gap: 6px;
    transition: border-color 140ms ease, transform 140ms ease;
    animation: fade-in 180ms ease;
  }
  .hf-card:hover { border-color: var(--border-2); transform: translateY(-1px); }
  .hf-card-top { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
  .hf-id  { font-size: 12px; font-weight: 500; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
  .hf-meta { font-size: 11px; color: var(--text-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  /* Add custom */
  .add-panel { max-width: 640px; }
</style>
