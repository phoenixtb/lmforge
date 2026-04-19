<script lang="ts">
  import { onMount } from 'svelte';
  import { listModels, getCatalog, fetchHfSizesBatch, type ModelEntry, type CatalogEntry } from '$lib/api';
  import { statusStore } from '$lib/stores/status';
  import { hardwareStore } from '$lib/stores/hardware';
  import { toast } from '$lib/stores/toasts';
  import { dragOnEmpty } from '$lib/drag';
  import ModelCard from '$lib/components/ModelCard.svelte';
  import CatalogCard from '$lib/components/CatalogCard.svelte';
  import PullPanel from '$lib/components/PullPanel.svelte';

  // ── Tabs ───────────────────────────────────────────────────────────────────
  type Tab = 'installed' | 'recommended' | 'discover' | 'add';
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

  // ── Recommended (platform-filtered catalog) ────────────────────────────────
  let catalogEntries: CatalogEntry[] = [];
  let loadingCatalog = false;
  let catalogError: string | null = null;
  let catalogFetched = false;

  // Derive the right format for this machine from the hardware profile.
  // macOS uses MLX (Apple Silicon); Linux + Windows use GGUF.
  $: platformFormat = (() => {
    const os = ($hardwareStore?.os ?? '').toLowerCase();
    if (os.includes('mac') || os.includes('darwin')) return 'mlx';
    return 'gguf';
  })();

  // Role filter: '' = all, 'chat', 'embed', 'rerank', 'vision', 'code'
  let catalogRole = '';

  $: filteredCatalog = catalogEntries.filter((e) => {
    if (catalogRole && e.role !== catalogRole) return false;
    return true;
  });

  // Installed IDs + HF repos for "Already installed" state across both tabs
  $: installedIds = new Set<string>([
    ...models.map((m) => m.id),
    ...models.filter((m) => m.hf_repo).map((m) => m.hf_repo as string),
  ]);

  // Real HF sizes: populated in the background after catalog loads
  // key = hf_repo, value = bytes
  let catalogSizes: Record<string, number> = {};
  let catalogSizesReady = false;  // false = fetch still in flight

  async function fetchCatalog() {
    if (catalogFetched) return; // lazy — fetch only on first visit
    loadingCatalog = true;
    catalogError   = null;
    try {
      const res = await getCatalog(platformFormat);
      catalogEntries = res.entries;
      catalogFetched = true;
      // Fire background size fetch — cards update reactively as data arrives
      fetchHfSizesBatch(res.entries.map((e) => e.hf_repo)).then((sizes) => {
        catalogSizes = sizes;
        catalogSizesReady = true;
      });
    } catch (e) {
      catalogError = String(e);
      catalogSizesReady = true; // avoid perpetual loading skeleton on error
    } finally {
      loadingCatalog = false;
    }
  }

  function onTabChange(tab: Tab) {
    activeTab = tab;
    if (tab === 'recommended') fetchCatalog();
  }

  function onCatalogPulled() {
    fetchModels(); // refresh so "installed" badges update everywhere
  }

  // ── Discover ───────────────────────────────────────────────────────────────
  let searchQuery    = '';
  let searchResults: CatalogEntry[] = []; // reuse CatalogEntry shape
  let searching      = false;
  let searchError: string | null = null;
  let searchTimer: ReturnType<typeof setTimeout>;

  // The HF API model shape (internal, before conversion)
  interface HfApiModel {
    id: string;
    downloads: number;
    lastModified: string;
    tags?: string[];
    gated?: boolean;
  }

  // Engine Format hint for HF search filter badge tooltip
  $: engineFormat = (() => {
    const id = $statusStore.engine_id.toLowerCase();
    if (id.includes('mlx'))                      return 'mlx';
    if (id.includes('llama') || id.includes('gguf')) return 'gguf';
    return platformFormat; // fallback to hardware-detected
  })();

  /** Infer role from HF tags/model ID — mirrors server-side infer_role() */
  function inferRole(modelId: string, tags: string[]): string {
    const s = [modelId, ...tags].join(' ').toLowerCase();
    if (s.includes('rerank') || s.includes('reranker')) return 'rerank';
    if (s.includes('vl-embed') || s.includes('vl_embed')) return 'vision';
    if (s.includes('embed') || s.includes('embedding'))   return 'embed';
    if (s.includes('vision') || s.includes('-vl') || s.includes('vision-language')) return 'vision';
    if (s.includes('code') || s.includes('coder')) return 'code';
    return 'chat';
  }

  // Discover sizes: populated from HF siblings data returned in the search response
  let discoverSizes: Record<string, number> = {};
  let discoverSizesReady = false;

  /** Convert a raw HF API model into a CatalogEntry so CatalogCard can be reused */
  function hfToEntry(m: HfApiModel): CatalogEntry {
    const safeTags = m.tags ?? [];

    // Tags: split model id on '/' and '-' for meaningful chips, supplement with HF tags
    const idParts = m.id.split('/').pop()?.split(/[-_]/) ?? [];
    const tags = [...new Set([...idParts, ...safeTags.slice(0, 4)])].slice(0, 6);

    return {
      shortcut: m.id,
      hf_repo:  m.id,
      format:   engineFormat,
      tags,
      role:     inferRole(m.id, safeTags),
    };
  }

  function onSearchInput() {
    clearTimeout(searchTimer);
    if (searchQuery.trim().length < 2) { searchResults = []; return; }
    searchTimer = setTimeout(doSearch, 350);
  }

  async function doSearch() {
    if (!searchQuery.trim()) return;
    searching = true; searchError = null;
    discoverSizes = {};
    discoverSizesReady = false;
    try {
      const filter = engineFormat ? `&filter=${engineFormat}` : '';
      const url = `https://huggingface.co/api/models?search=${encodeURIComponent(searchQuery)}&limit=20&sort=downloads${filter}`;
      const data: HfApiModel[] = await (await fetch(url)).json();
      searchResults = data.map(hfToEntry);
      // Batch-fetch real sizes from individual model API (search API doesn't expose usedStorage)
      fetchHfSizesBatch(searchResults.map((e) => e.hf_repo)).then((sizes) => {
        discoverSizes = sizes;
        discoverSizesReady = true;
      });
    } catch (e) {
      searchError = String(e);
      discoverSizesReady = true;
    } finally {
      searching = false;
    }
  }

  // ── Add ────────────────────────────────────────────────────────────────────
  let pullPrefill = '';
  function onPullDone() { fetchModels(); activeTab = 'installed'; }

  const TABS: { id: Tab; label: string }[] = [
    { id: 'installed',   label: 'Installed'   },
    { id: 'recommended', label: 'Recommended' },
    { id: 'discover',    label: 'Discover'    },
    { id: 'add',         label: 'Add Custom'  },
  ];

  const ROLE_FILTERS = ['chat', 'embed', 'rerank', 'vision', 'code'];
</script>

<svelte:head><title>LMForge — Model Library</title></svelte:head>

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
        onclick={() => onTabChange(t.id)}
      >
        {t.label}
        {#if t.id === 'installed' && models.length > 0}
          <span class="tab-count">{models.length}</span>
        {/if}
        {#if t.id === 'recommended' && catalogEntries.length > 0}
          <span class="tab-count">{catalogEntries.length}</span>
        {/if}
        {#if t.id === 'discover' && searchResults.length > 0}
          <span class="tab-count">{searchResults.length}</span>
        {/if}
      </button>
    {/each}
  </div>

  <!-- Tab panels -->
  <div class="tab-body">

    <!-- ── Installed ── -->
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
          <p>Browse <strong>Recommended</strong> for curated models, or search <strong>Discover</strong> for anything on HuggingFace.</p>
          <button class="btn btn--primary" onclick={() => onTabChange('recommended')}>
            Browse Recommended →
          </button>
        </div>
      {:else}
        <div class="model-grid" role="list">
          {#each models as model (model.id)}
            <ModelCard {model} onDeleted={onModelDeleted} />
          {/each}
        </div>
      {/if}

    <!-- ── Recommended ── -->
    {:else if activeTab === 'recommended'}
      {#if loadingCatalog}
        <div class="loading-grid">
          {#each Array(6) as _}
            <div class="skeleton" style="height:130px;border-radius:var(--radius-lg);"></div>
          {/each}
        </div>

      {:else if catalogError}
        <div class="tab-error" role="alert">
          Failed to load recommended models: {catalogError}
          <button class="btn btn--ghost btn--sm" onclick={() => { catalogFetched = false; fetchCatalog(); }} style="margin-top:10px;">Retry</button>
        </div>

      {:else}
        <!-- Role filter + count -->
        <div class="filter-bar">
          <div class="filter-group">
            <span class="filter-label">Role</span>
            <button class="filter-pill" class:active={catalogRole === ''} onclick={() => catalogRole = ''}>All</button>
            {#each ROLE_FILTERS as r}
              <button class="filter-pill" class:active={catalogRole === r} onclick={() => catalogRole = r}>{r}</button>
            {/each}
          </div>
          <span class="filter-count">{filteredCatalog.length} models · {platformFormat.toUpperCase()}</span>
        </div>

        {#if filteredCatalog.length === 0}
          <div class="empty-full">
            <div class="es-icon">🔍</div>
            <h3>No matches</h3>
            <p>Try changing the role filter.</p>
          </div>
        {:else}
          <div class="model-grid" role="list">
            {#each filteredCatalog as entry (entry.shortcut + entry.format)}
              <CatalogCard
                {entry}
                {installedIds}
                onPulled={onCatalogPulled}
                sizeBytes={catalogSizesReady ? (catalogSizes[entry.hf_repo] ?? null) : undefined}
              />
            {/each}
          </div>
        {/if}
      {/if}

    <!-- ── Discover ── -->
    {:else if activeTab === 'discover'}
      <div class="discover-panel">

        <!-- Search bar -->
        <div class="search-header">
          <div class="search-row">
            <input
              id="discover-search" type="search"
              class="search-input"
              placeholder="Search HuggingFace (e.g. Qwen3, Llama, Mistral, nomic-embed)…"
              bind:value={searchQuery}
              oninput={onSearchInput}
              onkeydown={(e) => e.key === 'Enter' && doSearch()}
            />
            <button
              class="btn btn--primary"
              onclick={doSearch}
              disabled={searching || searchQuery.trim().length < 2}
            >
              {searching ? '…' : 'Search'}
            </button>
          </div>

          <!-- Info bar: engine filter hint + HF tooltip -->
          <div class="search-info">
            {#if engineFormat}
              <span class="info-chip">
                <span class="info-dot"></span>
                Filtering for <strong>{engineFormat.toUpperCase()}</strong> models compatible with your engine
              </span>
            {/if}
            <a
              href="https://huggingface.co/models"
              target="_blank"
              rel="noopener noreferrer"
              class="hf-link"
              title="Browse all models on HuggingFace"
            >
              <span class="hf-logo">🤗</span> HuggingFace
            </a>
          </div>
        </div>

        <!-- Results -->
        {#if searchError}
          <div class="tab-error" role="alert">{searchError}</div>
        {:else if searching}
          <div class="loading-grid">
            {#each Array(6) as _}
              <div class="skeleton" style="height:130px;border-radius:var(--radius-lg);"></div>
            {/each}
          </div>
        {:else if searchResults.length > 0}
          <div class="search-results-header">
            <span class="filter-count">{searchResults.length} results for "<strong>{searchQuery}</strong>"</span>
          </div>
          <div class="model-grid" role="list">
            {#each searchResults as entry (entry.hf_repo)}
              <CatalogCard
                {entry}
                {installedIds}
                onPulled={onCatalogPulled}
                showHfLink={true}
                sizeBytes={discoverSizesReady ? (discoverSizes[entry.hf_repo] ?? null) : undefined}
              />
            {/each}
          </div>
        {:else if searchQuery.trim().length >= 2}
          <div class="empty-full">
            <div class="es-icon">🔍</div>
            <h3>No results</h3>
            <p>No {engineFormat.toUpperCase()} models found for "<strong>{searchQuery}</strong>".<br/>Try a broader term or <a href="https://huggingface.co/models" target="_blank" rel="noopener">browse HuggingFace directly</a>.</p>
          </div>
        {:else}
          <!-- Landing state — no search yet -->
          <div class="discover-landing">
            <div class="landing-icon">🤗</div>
            <h3>Search HuggingFace</h3>
            <p>Find any model by name, family, or task. Results are pre-filtered for <strong>{engineFormat.toUpperCase()}</strong> models compatible with your engine.</p>
            <div class="suggest-chips">
              {#each ['Qwen3', 'Llama', 'Gemma', 'Mistral', 'nomic-embed', 'bge-reranker'] as term}
                <button
                  class="suggest-chip"
                  onclick={() => { searchQuery = term; doSearch(); }}
                >{term}</button>
              {/each}
            </div>
          </div>
        {/if}
      </div>

    <!-- ── Add Custom ── -->
    {:else}
      <PullPanel prefill={pullPrefill} ondone={onPullDone} />
    {/if}

  </div><!-- /tab-body -->
</div><!-- /page -->

<style>
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

  /* ── Filter bar (shared: Recommended + Discover results) ─────────────────── */
  .filter-bar {
    display: flex; align-items: center; gap: 16px; flex-wrap: wrap;
    margin-bottom: 16px;
    padding: 10px 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .filter-group { display: flex; align-items: center; gap: 5px; }
  .filter-label { font-size: 10.5px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.5px; margin-right: 2px; }
  .filter-pill {
    padding: 3px 10px; background: none;
    border: 1px solid var(--border); border-radius: 99px;
    color: var(--text-3); font-size: 11.5px; cursor: pointer;
    transition: all 110ms ease;
  }
  .filter-pill:hover { border-color: var(--border-2); color: var(--text-2); }
  .filter-pill.active {
    background: var(--accent); border-color: var(--accent);
    color: var(--bg); font-weight: 600;
  }
  .filter-count { margin-left: auto; font-size: 11px; color: var(--text-3); }

  /* Grids */
  .model-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 12px;
  }
  .loading-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 12px;
  }

  /* Error */
  .tab-error {
    background: var(--danger-dim); border: 1px solid var(--danger);
    border-radius: var(--radius-lg); padding: 14px 16px;
    color: var(--danger); font-size: 12.5px;
    display: flex; flex-direction: column;
  }

  /* Empty */
  .empty-full {
    display: flex; flex-direction: column; align-items: center;
    justify-content: center; min-height: 200px; gap: 12px;
    color: var(--text-2); text-align: center;
  }
  .es-icon { font-size: 40px; }
  .empty-full h3 { font-size: 15px; color: var(--text); }
  .empty-full p  { font-size: 13px; color: var(--text-2); max-width: 300px; line-height: 1.5; }
  .empty-full a  { color: var(--accent-2); }

  /* ── Discover ─────────────────────────────────────────────────────────────── */
  .discover-panel { display: flex; flex-direction: column; gap: 16px; }

  .search-header {
    display: flex; flex-direction: column; gap: 8px;
  }
  .search-row { display: flex; gap: 8px; align-items: center; }
  .search-input {
    flex: 1;
    background: var(--surface-2); border: 1px solid var(--border-2);
    border-radius: var(--radius-sm); color: var(--text);
    font-family: var(--font-sans); font-size: 12.5px;
    padding: 8px 12px; outline: none;
    transition: border-color 110ms ease; user-select: text;
  }
  .search-input:focus { border-color: var(--accent); }
  .search-input::placeholder { color: var(--text-3); }

  /* Engine info bar */
  .search-info {
    display: flex; align-items: center; gap: 12px;
    padding: 6px 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .info-chip {
    display: flex; align-items: center; gap: 6px;
    font-size: 11.5px; color: var(--text-2); flex: 1;
  }
  .info-dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: var(--accent); flex-shrink: 0;
    box-shadow: 0 0 4px var(--accent);
  }
  .hf-link {
    display: flex; align-items: center; gap: 4px;
    font-size: 11.5px; color: var(--text-3);
    text-decoration: none; white-space: nowrap;
    transition: color 110ms ease;
  }
  .hf-link:hover { color: var(--accent-2); }
  .hf-logo { font-size: 13px; }

  /* Search results header */
  .search-results-header {
    display: flex; justify-content: flex-end; margin-bottom: 4px;
  }

  /* ── Landing state ─────────────────────────────────────────────────────────── */
  .discover-landing {
    display: flex; flex-direction: column; align-items: center;
    justify-content: center; min-height: 280px; gap: 14px; text-align: center;
  }
  .landing-icon { font-size: 48px; }
  .discover-landing h3 { font-size: 16px; font-weight: 600; color: var(--text); }
  .discover-landing p  { font-size: 13px; color: var(--text-2); max-width: 340px; line-height: 1.6; }

  .suggest-chips { display: flex; flex-wrap: wrap; gap: 8px; justify-content: center; margin-top: 4px; }
  .suggest-chip {
    padding: 5px 14px;
    background: var(--surface-2); border: 1px solid var(--border);
    border-radius: 99px; color: var(--text-2);
    font-size: 12px; font-family: var(--font-mono); cursor: pointer;
    transition: all 120ms ease;
  }
  .suggest-chip:hover {
    border-color: var(--accent); color: var(--accent-2);
    background: var(--surface-3);
    transform: translateY(-1px);
  }

  /* Add custom */


</style>
