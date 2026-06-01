<svelte:head><title>LMForge — Settings</title></svelte:head>

<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { open } from '@tauri-apps/plugin-dialog';
  import { onMount } from 'svelte';
  import { dragOnEmpty } from '$lib/drag';
  import { toast } from '$lib/stores/toasts';
  import { getEngines, getConfig, postConfig, applyStorage, type EngineInfo, type LfConfig } from '$lib/api';

  // ── Daemon config (storage dirs) ────────────────────────────────────────────
  let cfg = $state<LfConfig & { restart_required?: boolean }>({});
  let catalogDir = $state('');
  let dataDir    = $state('');
  let modelsDir  = $state('');
  let savingCatalog = $state(false);

  /** Persisted restart_required — set after storage apply or config save. */
  let restartNeeded = $state(false);

  // ── Storage change flow ──────────────────────────────────────────────────────
  /** New path values being edited (before the user hits Apply). */
  let pendingModelsDir = $state('');
  let pendingDataDir   = $state('');

  type ModelsAction = 'adopt' | 'delete' | 'repull';
  type DataAction   = 'relocate' | 'keep';
  let modelsAction = $state<ModelsAction>('adopt');
  let dataAction   = $state<DataAction>('keep');
  let copyLogs     = $state(false);

  /** Whether any of the storage paths were edited by the user. */
  let storageEdited = $derived(
    pendingModelsDir !== modelsDir || pendingDataDir !== dataDir
  );
  let applyingStorage = $state(false);

  /** Models that would be permanently lost during repull (no hf_repo). */
  let wouldLose = $state<string[]>([]);
  /** Show a confirmation dialog for lose-warning models. */
  let showLossConfirm = $state(false);
  let lossConfirmed = $state(false);

  /** Blocking restart modal — shown after a storage apply. */
  let showRestartModal = $state(false);
  /** Whether the user acknowledged "I'll restart later". */
  let restartAcknowledged = $state(false);

  // ── Daemon / Service section ─────────────────────────────────────────────────
  type ServiceStatus = { installed: boolean; running: boolean; output: string; error?: string };
  let serviceStatus = $state<ServiceStatus | null>(null);
  let loadingServiceStatus = $state(false);
  let restartingDaemon  = $state(false);
  let restartingService = $state(false);

  async function loadServiceStatus() {
    loadingServiceStatus = true;
    try {
      serviceStatus = await invoke<ServiceStatus>('get_service_status');
    } catch { serviceStatus = null; }
    finally { loadingServiceStatus = false; }
  }

  async function doRestartDaemon() {
    restartingDaemon = true;
    try {
      await invoke('restart_engine');
      await loadConfig();
      await loadEngines();
      restartNeeded = false;
      showRestartModal = false;
      toast.success('Daemon restarted');
    } catch (e) {
      toast.error(`Restart failed — run "lmforge stop && lmforge start" manually (${e})`);
    } finally {
      restartingDaemon = false;
    }
  }

  async function doRestartService() {
    restartingService = true;
    try {
      await invoke('restart_service');
      await loadConfig();
      await loadEngines();
      await loadServiceStatus();
      restartNeeded = false;
      showRestartModal = false;
      toast.success('Service restarted');
    } catch (e) {
      toast.error(`Service restart failed (${e})`);
    } finally {
      restartingService = false;
    }
  }

  function ackRestart() {
    restartAcknowledged = true;
    showRestartModal = false;
  }

  async function applyStorageChange() {
    // Guard: if repull and would-lose models aren't acknowledged, prompt first.
    if (modelsAction === 'repull' && wouldLose.length > 0 && !lossConfirmed) {
      showLossConfirm = true;
      return;
    }

    applyingStorage = true;
    try {
      const mdChanged = pendingModelsDir !== modelsDir;
      const ddChanged = pendingDataDir !== dataDir;
      // Clearing a previously-custom dir means "reset to default".
      const resetModels = mdChanged && pendingModelsDir.trim() === '';
      const resetData   = ddChanged && pendingDataDir.trim()   === '';
      const res = await applyStorage({
        models_dir: mdChanged && !resetModels ? pendingModelsDir.trim() : undefined,
        data_dir:   ddChanged && !resetData   ? pendingDataDir.trim()   : undefined,
        reset_models_dir: resetModels,
        reset_data_dir:   resetData,
        models_action: modelsAction,
        data_action:   dataAction,
        copy_logs: copyLogs,
        exclude_from_repull: lossConfirmed ? wouldLose : [],
      });

      if (res.restart_required) {
        restartNeeded = true;
        // Apply the new paths to the live state.
        modelsDir = pendingModelsDir;
        dataDir   = pendingDataDir;
        // Reset editing state.
        modelsAction = 'adopt';
        dataAction   = 'keep';
        lossConfirmed = false;
        wouldLose = [];
        showRestartModal = true;
      }
      toast.success('Storage change applied — restart to activate');
    } catch (err: unknown) {
      // 422: server returned would_lose list — show confirmation dialog.
      const e = err as { status?: number; body?: { would_lose?: string[] } };
      if (e?.status === 422 && e?.body?.would_lose?.length) {
        wouldLose = e.body.would_lose;
        showLossConfirm = true;
      } else {
        toast.error(`Apply failed: ${err}`);
      }
    } finally {
      applyingStorage = false;
    }
  }

  async function loadConfig() {
    try {
      cfg = await getConfig() as LfConfig & { restart_required?: boolean };
      catalogDir = (cfg.catalogs_dir as string) ?? '';
      dataDir    = (cfg.data_dir    as string) ?? '';
      modelsDir  = (cfg.models_dir  as string) ?? '';
      pendingModelsDir = modelsDir;
      pendingDataDir   = dataDir;
      restartNeeded = !!cfg.restart_required;
    } catch (e) {
      toast.error(`Failed to load config: ${e}`);
    }
  }

  async function saveCatalogDir() {
    savingCatalog = true;
    try {
      const next: LfConfig = { ...cfg, catalogs_dir: catalogDir.trim() || null };
      const res = await postConfig(next);
      cfg = { ...next };
      if (res.restart_required) restartNeeded = true;
      toast.success('Catalog directory updated');
    } catch (e) {
      toast.error(`Failed to save: ${e}`);
    } finally {
      savingCatalog = false;
    }
  }

  // ── Engine roster ────────────────────────────────────────────────────────────
  let engines = $state<EngineInfo[]>([]);
  let activeEngineId = $state('');
  let hasHardware    = $state(true);
  let engineLoading  = $state(false);
  let engineError    = $state<string | null>(null);

  async function loadEngines() {
    engineLoading = true;
    engineError = null;
    try {
      const r = await getEngines();
      engines = r.engines;
      activeEngineId = r.active_engine_id;
      hasHardware = r.has_hardware_profile;
    } catch (e) {
      engineError = String(e);
    } finally {
      engineLoading = false;
    }
  }

  async function copyCmd(cmd: string) {
    try {
      await navigator.clipboard.writeText(cmd);
      toast.success('Copied');
    } catch {
      toast.error('Copy failed — select and copy manually');
    }
  }

  async function browseDir(title: string): Promise<string | null> {
    try {
      const selected = await open({ directory: true, title });
      if (selected && typeof selected === 'string') return selected;
    } catch { /* cancelled or unavailable in web mode */ }
    return null;
  }

  async function browseCatalogDir() {
    const p = await browseDir('Select catalog directory');
    if (p) catalogDir = p;
  }

  onMount(async () => {
    await loadConfig();
    await loadEngines();
    await loadServiceStatus();
  });

  let lastFetchedFor = $state<string | null>(null);
  $effect(() => {
    if (activeSection === 'engine' && lastFetchedFor !== 'engine') {
      lastFetchedFor = 'engine';
      void loadEngines();
    } else if (activeSection === 'daemon' && lastFetchedFor !== 'daemon') {
      lastFetchedFor = 'daemon';
      void loadServiceStatus();
    } else if (activeSection !== 'engine' && activeSection !== 'daemon') {
      lastFetchedFor = null;
    }
  });

  type SettingSection = { id: string; label: string; icon: string };
  const SECTIONS: SettingSection[] = [
    { id: 'storage', label: 'Storage',       icon: '💾' },
    { id: 'catalog', label: 'Catalog',        icon: '📂' },
    { id: 'engine',  label: 'Engine',         icon: '⚙' },
    { id: 'daemon',  label: 'Daemon',         icon: '⚡' },
    { id: 'about',   label: 'About',          icon: 'ℹ' },
  ];
  let activeSection = $state('storage');
</script>

<div class="page">

  <!-- Toolbar -->
  <div class="toolbar" data-tauri-drag-region onpointerdown={dragOnEmpty}>
    <h1>Settings</h1>
  </div>

  <div class="settings-body">

    <!-- Left nav -->
    <nav class="settings-nav" aria-label="Settings sections">
      {#each SECTIONS as s}
        <button
          class="snav-item"
          class:active={activeSection === s.id}
          onclick={() => activeSection = s.id}
        >
          <span class="snav-icon">{s.icon}</span>
          <span class="snav-label">{s.label}</span>
        </button>
      {/each}
    </nav>

    <!-- Content pane -->
    <div class="settings-content">

      {#if activeSection === 'storage'}
        <!-- ── Storage ── -->
        <section class="settings-section">
          <h2 class="section-title">Storage Locations</h2>
          <p class="section-desc">
            Control where LMForge keeps model weights and its data root. Point
            <strong>Models</strong> at a shared volume (e.g. a virtio-fs mount) to reuse one
            weights library across VMs. A daemon restart is required for changes to take effect.
          </p>

          {#if restartNeeded && !showRestartModal}
            <div class="alert alert--warn restart-banner">
              <span class="restart-banner-text">
                Storage location changed. Restart the daemon to activate the new directories.
              </span>
              <button class="btn btn--primary btn--sm" onclick={() => showRestartModal = true}>
                Restart…
              </button>
            </div>
          {/if}

          <!-- Models directory -->
          <div class="setting-row">
            <div class="setting-label-group">
              <label class="setting-label" for="models-dir-input">Models directory</label>
              <span class="setting-hint">
                Where model weights are downloaded and loaded from.
                Leave blank for the default <code>&lbrace;data_dir&rbrace;/models</code>.
              </span>
            </div>
            <div class="setting-control">
              <div class="path-input-row">
                <input
                  id="models-dir-input"
                  type="text"
                  class="path-input"
                  placeholder="(default: data_dir/models)"
                  bind:value={pendingModelsDir}
                />
                <button class="btn btn--ghost btn--sm" onclick={async () => {
                  const p = await browseDir('Select models directory');
                  if (p) pendingModelsDir = p;
                }}>Browse…</button>
                {#if modelsDir && pendingModelsDir !== ''}
                  <button
                    class="btn btn--ghost btn--sm"
                    title="Clear to the built-in default (data_dir/models)"
                    onclick={() => pendingModelsDir = ''}
                  >Reset to default</button>
                {/if}
              </div>
              {#if pendingModelsDir !== modelsDir}
                <div class="option-group">
                  <div class="option-group-label">What to do with models in the current directory?</div>
                  <label class="option-radio">
                    <input type="radio" bind:group={modelsAction} value="adopt" />
                    <span class="option-title">Adopt in place</span>
                    <span class="option-desc">Keep existing files where they are; scan new directory on restart. Best for virtio-fs or already-populated volumes.</span>
                  </label>
                  <label class="option-radio">
                    <input type="radio" bind:group={modelsAction} value="delete" />
                    <span class="option-title">Delete from current directory</span>
                    <span class="option-desc">Remove model files from the old directory now. New directory starts empty; pull models again after restart.</span>
                  </label>
                  <label class="option-radio">
                    <input type="radio" bind:group={modelsAction} value="repull" />
                    <span class="option-title">Delete &amp; re-download</span>
                    <span class="option-desc">Remove files from old directory, then automatically re-download all models into the new directory on restart. Models without a HuggingFace repo cannot be re-downloaded.</span>
                  </label>
                </div>
              {/if}
            </div>
          </div>

          <!-- Data directory -->
          <div class="setting-row">
            <div class="setting-label-group">
              <label class="setting-label" for="data-dir-input">Data directory</label>
              <span class="setting-hint">
                Engines, logs, and the model index (<code>models.json</code>) live here.
                Keep this local per machine. Leave blank for the default <code>~/.lmforge</code>.
              </span>
            </div>
            <div class="setting-control">
              <div class="path-input-row">
                <input
                  id="data-dir-input"
                  type="text"
                  class="path-input"
                  placeholder="(default ~/.lmforge)"
                  bind:value={pendingDataDir}
                />
                <button class="btn btn--ghost btn--sm" onclick={async () => {
                  const p = await browseDir('Select data directory');
                  if (p) pendingDataDir = p;
                }}>Browse…</button>
                {#if dataDir && pendingDataDir !== ''}
                  <button
                    class="btn btn--ghost btn--sm"
                    title="Clear to the built-in default (~/.lmforge)"
                    onclick={() => pendingDataDir = ''}
                  >Reset to default</button>
                {/if}
              </div>
              {#if pendingDataDir !== dataDir}
                <div class="option-group">
                  <div class="option-group-label">How to handle the current data directory?</div>
                  <label class="option-radio">
                    <input type="radio" bind:group={dataAction} value="keep" />
                    <span class="option-title">Keep existing data</span>
                    <span class="option-desc">Old data stays in place; new directory starts fresh. Engines will be re-installed on next init.</span>
                  </label>
                  <label class="option-radio">
                    <input type="radio" bind:group={dataAction} value="relocate" />
                    <span class="option-title">Copy model index &amp; hardware profile</span>
                    <span class="option-desc">Copies <code>models.json</code> and <code>hardware.json</code> to the new location. Engines are NOT moved (venvs are not portable) — run <code>lmforge init</code> after restart to reinstall them.</span>
                  </label>
                  {#if dataAction === 'relocate'}
                    <label class="option-checkbox" style="margin-left:20px">
                      <input type="checkbox" bind:checked={copyLogs} />
                      <span>Also copy logs/</span>
                    </label>
                  {/if}
                </div>
              {/if}
            </div>
          </div>

          <!-- Section-level apply bar — applies whichever dir(s) changed. -->
          {#if storageEdited}
            <div class="storage-apply-bar">
              <span class="storage-apply-summary">
                Pending:
                {#if pendingModelsDir !== modelsDir}<span class="chip">Models dir</span>{/if}
                {#if pendingDataDir !== dataDir}<span class="chip">Data dir</span>{/if}
              </span>
              <div class="storage-apply-actions">
                <button
                  class="btn btn--ghost btn--sm"
                  onclick={() => { pendingModelsDir = modelsDir; pendingDataDir = dataDir; modelsAction = 'adopt'; dataAction = 'keep'; }}
                >
                  Cancel
                </button>
                <button
                  class="btn btn--primary btn--sm"
                  onclick={applyStorageChange}
                  disabled={applyingStorage}
                >
                  {applyingStorage ? 'Applying…' : 'Apply change'}
                </button>
              </div>
            </div>
          {/if}
        </section>

        <!-- Loss confirmation dialog -->
        {#if showLossConfirm}
          <div class="modal-overlay" role="dialog" aria-modal="true">
            <div class="modal">
              <h3 class="modal-title">Some models cannot be re-downloaded</h3>
              <p class="modal-body">
                The following models have no HuggingFace repo recorded and <strong>will be permanently deleted</strong>:
              </p>
              <ul class="lose-list">
                {#each wouldLose as id}
                  <li><code>{id}</code></li>
                {/each}
              </ul>
              <p class="modal-body">Confirm to accept data loss, or cancel and choose a different action.</p>
              <div class="modal-actions">
                <button class="btn btn--ghost btn--sm" onclick={() => { showLossConfirm = false; wouldLose = []; }}>
                  Cancel
                </button>
                <button class="btn btn--danger btn--sm" onclick={() => {
                  lossConfirmed = true;
                  showLossConfirm = false;
                  void applyStorageChange();
                }}>
                  Delete anyway
                </button>
              </div>
            </div>
          </div>
        {/if}

        <!-- Blocking restart modal -->
        {#if showRestartModal}
          <div class="modal-overlay" role="dialog" aria-modal="true">
            <div class="modal">
              <h3 class="modal-title">Restart required</h3>
              <p class="modal-body">
                Storage directories have been updated. A daemon restart is required to activate the new paths.
                Until restarted, the daemon continues using the old directories.
              </p>
              <div class="modal-actions modal-actions--col">
                <button
                  class="btn btn--primary btn--sm"
                  onclick={doRestartDaemon}
                  disabled={restartingDaemon || restartingService}
                >
                  {restartingDaemon ? 'Restarting…' : 'Restart daemon'}
                </button>
                {#if serviceStatus?.installed}
                  <button
                    class="btn btn--primary btn--sm"
                    onclick={doRestartService}
                    disabled={restartingDaemon || restartingService}
                  >
                    {restartingService ? 'Restarting…' : 'Restart service'}
                  </button>
                {/if}
                <button class="btn btn--ghost btn--sm" onclick={ackRestart}>
                  I'll restart later
                </button>
              </div>
            </div>
          </div>
        {/if}

      {:else if activeSection === 'catalog'}
        <!-- ── Catalog ── -->
        <section class="settings-section">
          <h2 class="section-title">Model Catalog</h2>
          <p class="section-desc">
            LMForge ships with a built-in recommended catalog. You can point to a custom directory
            of <code>.json</code> catalog files to override or extend it.
          </p>

          <div class="setting-row">
            <div class="setting-label-group">
              <label class="setting-label" for="catalog-dir-input">Catalog directory</label>
              <span class="setting-hint">
                Path to a folder containing <code>mlx.json</code> / <code>gguf.json</code> catalog files.
                Leave blank to use the built-in catalog.
              </span>
            </div>
            <div class="setting-control">
              <div class="path-input-row">
                <input
                  id="catalog-dir-input"
                  type="text"
                  class="path-input"
                  placeholder="(default built-in)"
                  bind:value={catalogDir}
                />
                <button class="btn btn--ghost btn--sm" onclick={browseCatalogDir}>
                  Browse…
                </button>
              </div>
              <div class="setting-actions">
                {#if catalogDir.trim()}
                  <button
                    class="btn btn--ghost btn--sm"
                    onclick={() => { catalogDir = ''; saveCatalogDir(); }}
                  >
                    Reset to default
                  </button>
                {/if}
                <button
                  class="btn btn--primary btn--sm"
                  onclick={saveCatalogDir}
                  disabled={savingCatalog}
                >
                  {savingCatalog ? 'Saving…' : 'Save'}
                </button>
              </div>
            </div>
          </div>

          <div class="info-card">
            <div class="info-card-title">Catalog file format</div>
            <p class="info-card-body">
              Each catalog file is a JSON object mapping shortcut keys to HuggingFace repo IDs, e.g.:
            </p>
            <pre class="info-code">{`{
  "qwen3:8b:4bit": "mlx-community/Qwen3-8B-4bit",
  "gemma3:4b:4bit": "mlx-community/gemma-3-4b-it-4bit"
}`}</pre>
          </div>
        </section>

      {:else if activeSection === 'engine'}
        <!-- ── Engine ── -->
        <section class="settings-section">
          <div class="row-spread">
            <h2 class="section-title">Inference Engines</h2>
            <button class="btn btn--ghost btn--sm" onclick={loadEngines} disabled={engineLoading}>
              {engineLoading ? 'Refreshing…' : 'Refresh'}
            </button>
          </div>
          <p class="section-desc">
            LMForge ships with a tiered engine roster. <strong>Default</strong> engines are auto-installed by
            <code>lmforge init</code>. <strong>Opt-in</strong> engines (vLLM, ExLlamaV3) cost 5+ GB of disk and a
            <code>uv</code>-managed Python venv — install only what you need.
            <strong>Experimental</strong> engines are never auto-selected.
          </p>

          {#if !hasHardware}
            <div class="alert alert--warn">
              No hardware profile found. Run <code>lmforge init</code> to populate it; compatibility verdicts will appear afterwards.
            </div>
          {/if}

          {#if engineError}
            <div class="alert alert--error">Failed to load engines: {engineError}</div>
          {/if}

          {#if engines.length > 0}
            <div class="engine-grid">
              {#each engines as e (e.id)}
                <article class="engine-card" class:engine-card--active={e.active}>
                  <header class="ec-head">
                    <div class="ec-id">
                      <span class="ec-name">{e.name}</span>
                      <code class="ec-key">{e.id}</code>
                    </div>
                    <div class="ec-tags">
                      <span class="tier-badge tier-badge--{e.tier.replace('*','star').replace('-','')}">
                        {e.tier}
                      </span>
                      {#if e.active}
                        <span class="badge badge--active">active</span>
                      {/if}
                    </div>
                  </header>

                  <dl class="ec-grid">
                    <dt>Version</dt>      <dd>{e.version}</dd>
                    <dt>Format</dt>       <dd>{e.model_format}</dd>
                    <dt>Install</dt>      <dd>{e.install_method}</dd>
                    <dt>GPU</dt>          <dd>{e.matches_gpu}</dd>
                    {#if e.min_compute_cap}
                      <dt>Compute cap</dt>
                      <dd>
                        ≥ {e.min_compute_cap}{#if e.max_compute_cap} &nbsp;·&nbsp; ≤ {e.max_compute_cap}{/if}
                      </dd>
                    {/if}
                    {#if e.min_vram_gb && e.min_vram_gb > 0}
                      <dt>Min VRAM</dt>   <dd>{e.min_vram_gb} GB</dd>
                    {/if}
                    {#if e.supported_os_families && e.supported_os_families.length}
                      <dt>OS</dt>         <dd>{e.supported_os_families.join(', ')}</dd>
                    {/if}
                    <dt>Capabilities</dt>
                    <dd>
                      <span class="cap" class:cap--on={true}>chat</span>
                      <span class="cap" class:cap--on={e.supports_embeddings}>embed</span>
                      <span class="cap" class:cap--on={e.supports_reranking}>rerank</span>
                    </dd>
                  </dl>

                  <div class="ec-state">
                    {#if e.id === 'llamacpp'}
                      <p class="ec-variant-hint">
                        Linux NVIDIA: <code>lmforge init</code> installs <strong>cuda12</strong> by default.
                        Opt-in: <code>lmforge engine install llamacpp --variant cuda13</code>.
                        Active variant: <code>lmforge doctor</code>.
                      </p>
                    {/if}
                    <div class="state-pair">
                      <span class="state-key">Installed</span>
                      <span class="state-val">
                        {#if e.installed}<span class="dot dot--ok"></span>yes
                        {:else}<span class="dot dot--off"></span>no{/if}
                      </span>
                    </div>
                    <div class="state-pair">
                      <span class="state-key">Compatible</span>
                      <span class="state-val">
                        {#if e.compatible === null}<span class="dot dot--off"></span>unknown
                        {:else if e.compatible}<span class="dot dot--ok"></span>yes
                        {:else}<span class="dot dot--err"></span>no{/if}
                      </span>
                    </div>
                  </div>

                  {#if e.incompatible_reason}
                    <div class="ec-note ec-note--err">{e.incompatible_reason}</div>
                  {/if}

                  {#if !e.installed && e.compatible && e.tier === 'opt-in'}
                    {@const cmd = `lmforge engine install ${e.id}`}
                    <div class="ec-cmd">
                      <code class="ec-cmd-text">{cmd}</code>
                      <button class="btn btn--ghost btn--xs" onclick={() => copyCmd(cmd)}>Copy</button>
                    </div>
                    <div class="ec-hint">Run in your terminal — installs ~5 GB venv + wheels.</div>
                  {:else if !e.installed && e.compatible && e.tier === 'experimental'}
                    {@const cmd = `lmforge engine install ${e.id} --yes-experimental`}
                    <div class="ec-cmd">
                      <code class="ec-cmd-text">{cmd}</code>
                      <button class="btn btn--ghost btn--xs" onclick={() => copyCmd(cmd)}>Copy</button>
                    </div>
                    <div class="ec-hint">Experimental — may fail at runtime on this hardware.</div>
                  {:else if e.installed && !e.active}
                    {@const cmd = `lmforge start --engine ${e.id}`}
                    <div class="ec-cmd">
                      <code class="ec-cmd-text">{cmd}</code>
                      <button class="btn btn--ghost btn--xs" onclick={() => copyCmd(cmd)}>Copy</button>
                    </div>
                  {/if}
                </article>
              {/each}
            </div>
          {:else if !engineLoading && !engineError}
            <div class="empty">No engines registered.</div>
          {/if}
        </section>

      {:else if activeSection === 'daemon'}
        <!-- ── Daemon & Service ── -->
        <section class="settings-section">
          <div class="row-spread">
            <h2 class="section-title">Daemon &amp; Service</h2>
            <button class="btn btn--ghost btn--sm" onclick={loadServiceStatus} disabled={loadingServiceStatus}>
              {loadingServiceStatus ? 'Refreshing…' : 'Refresh'}
            </button>
          </div>
          <p class="section-desc">
            Control the LMForge daemon process and, if installed, the system service.
            Restarting picks up config changes that require a restart (storage dirs, port, etc.).
          </p>

          {#if restartNeeded}
            <div class="alert alert--warn">
              Pending config change — restart the daemon to activate.
            </div>
          {/if}

          {#if serviceStatus !== null}
            <div class="setting-row">
              <div class="setting-label-group">
                <span class="setting-label">System service</span>
              </div>
              <div class="service-status-grid">
                <div class="state-pair">
                  <span class="state-key">Installed</span>
                  <span class="state-val">
                    {#if serviceStatus.installed}
                      <span class="dot dot--ok"></span>yes
                    {:else}
                      <span class="dot dot--off"></span>no
                    {/if}
                  </span>
                </div>
                {#if serviceStatus.installed}
                  <div class="state-pair">
                    <span class="state-key">Running</span>
                    <span class="state-val">
                      {#if serviceStatus.running}
                        <span class="dot dot--ok"></span>active
                      {:else}
                        <span class="dot dot--off"></span>inactive
                      {/if}
                    </span>
                  </div>
                {/if}
              </div>
            </div>
          {/if}

          <div class="daemon-actions">
            <button
              class="btn btn--primary btn--sm"
              onclick={doRestartDaemon}
              disabled={restartingDaemon || restartingService}
            >
              {restartingDaemon ? 'Restarting…' : 'Restart daemon'}
            </button>
            {#if serviceStatus?.installed}
              <button
                class="btn btn--primary btn--sm"
                onclick={doRestartService}
                disabled={restartingDaemon || restartingService}
              >
                {restartingService ? 'Restarting…' : 'Restart service'}
              </button>
            {/if}
          </div>

          <div class="info-card">
            <div class="info-card-title">CLI commands</div>
            <p class="info-card-body">You can also manage the daemon from the terminal:</p>
            <pre class="info-code">{`lmforge stop && lmforge start   # restart foreground daemon
lmforge service restart         # restart via service manager
lmforge service status          # show installation status`}</pre>
          </div>
        </section>

      {:else if activeSection === 'about'}
        <!-- ── About ── -->
        <section class="settings-section">
          <h2 class="section-title">About LMForge</h2>
          <div class="about-card">
            <img src="/lmforge-logo.png" alt="LMForge" class="about-logo" />
            <div class="about-name">LMForge</div>
            <div class="about-tagline">Local AI model engine &amp; management UI</div>
            <div class="about-links">
              <a href="https://github.com/phoenixtb/lmforge" target="_blank" rel="noopener" class="about-link">GitHub →</a>
            </div>
          </div>
        </section>
      {/if}

    </div>
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

  /* Toolbar */
  .toolbar {
    height: var(--toolbar-h);
    display: flex; align-items: center;
    padding: 0 20px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .toolbar h1 { font-size: 14px; font-weight: 600; color: var(--text); }

  /* Body split */
  .settings-body {
    display: flex;
    flex: 1;
    overflow: hidden;
  }

  /* Left nav */
  .settings-nav {
    width: 160px;
    flex-shrink: 0;
    border-right: 1px solid var(--border);
    padding: 12px 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .snav-item {
    display: flex; align-items: center; gap: 8px;
    padding: 7px 10px; border: none; border-radius: var(--radius-sm);
    background: none; color: var(--text-2);
    font-size: 12.5px; font-family: var(--font-sans);
    cursor: pointer;
    transition: background 100ms ease, color 100ms ease;
    text-align: left;
  }
  .snav-item:hover { background: rgba(255,255,255,0.05); color: var(--text); }
  .snav-item.active { background: var(--accent-dim); color: var(--accent-2); font-weight: 500; }
  .snav-icon  { font-size: 14px; flex-shrink: 0; }
  .snav-label { flex: 1; }

  /* Content pane */
  .settings-content {
    flex: 1;
    overflow-y: auto;
    padding: 24px 28px;
    max-width: 680px;
  }

  .settings-section { display: flex; flex-direction: column; gap: 20px; }
  .section-title { font-size: 15px; font-weight: 600; color: var(--text); margin: 0; }
  .section-desc  {
    font-size: 12.5px; color: var(--text-2); line-height: 1.6; margin: 0;
  }
  .section-desc code {
    font-family: var(--font-mono); font-size: 11.5px;
    background: var(--surface-3); padding: 1px 5px; border-radius: 3px;
  }

  /* Setting row */
  .setting-row {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .setting-label-group { display: flex; flex-direction: column; gap: 3px; }
  .setting-label {
    font-size: 12.5px; font-weight: 600; color: var(--text);
  }
  .setting-hint {
    font-size: 11.5px; color: var(--text-2); line-height: 1.5;
  }
  .setting-hint code {
    font-family: var(--font-mono); font-size: 11px;
    background: var(--surface-3); padding: 1px 4px; border-radius: 3px;
  }
  .setting-control { display: flex; flex-direction: column; gap: 8px; }
  .path-input-row  { display: flex; gap: 8px; align-items: center; }
  .path-input {
    flex: 1;
    background: var(--surface-3); border: 1px solid var(--border-2);
    border-radius: var(--radius-sm); color: var(--text);
    font-family: var(--font-mono); font-size: 11.5px;
    padding: 7px 10px; outline: none;
    transition: border-color 120ms ease; user-select: text;
  }
  .path-input:focus { border-color: var(--accent); }
  .path-input::placeholder { color: var(--text-3); font-family: var(--font-sans); font-style: italic; }
  .setting-actions {
    display: flex; justify-content: flex-end; gap: 8px;
  }

  /* Info card */
  .info-card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 14px 16px;
    display: flex; flex-direction: column; gap: 8px;
  }
  .info-card-title { font-size: 12px; font-weight: 600; color: var(--text); }
  .info-card-body  { font-size: 12px; color: var(--text-2); margin: 0; line-height: 1.5; }
  .info-code {
    font-family: var(--font-mono); font-size: 11px; color: var(--text-2);
    background: var(--surface-3); border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 10px 12px; overflow-x: auto;
    white-space: pre;
  }

  /* Engine roster */
  .row-spread {
    display: flex; align-items: center; justify-content: space-between; gap: 12px;
  }
  .engine-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: 14px;
  }
  .engine-card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 14px 16px 12px;
    display: flex; flex-direction: column; gap: 10px;
    transition: border-color 120ms ease;
  }
  .engine-card--active {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent-dim) inset;
  }
  .ec-head {
    display: flex; align-items: flex-start; justify-content: space-between; gap: 10px;
  }
  .ec-id { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .ec-name {
    font-size: 13px; font-weight: 600; color: var(--text);
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .ec-key {
    font-family: var(--font-mono); font-size: 11px; color: var(--text-3);
  }
  .ec-tags { display: flex; gap: 6px; flex-shrink: 0; }

  .tier-badge {
    font-size: 10.5px; font-weight: 600; line-height: 1;
    padding: 3px 7px; border-radius: 999px;
    text-transform: uppercase; letter-spacing: 0.4px;
  }
  .tier-badge--default { background: rgba(70, 200, 120, 0.16); color: #6ee7a4; }
  .tier-badge--optin   { background: rgba(100, 170, 255, 0.18); color: #8ab8ff; }
  .tier-badge--experimental { background: rgba(240, 175, 80, 0.18); color: #f4c071; }
  .tier-badge--defaultstar  { background: rgba(150, 150, 150, 0.18); color: var(--text-2); }

  .badge--active {
    font-size: 10px; font-weight: 600;
    padding: 3px 7px; border-radius: 999px;
    background: var(--accent-dim); color: var(--accent-2);
    text-transform: uppercase; letter-spacing: 0.4px;
  }

  .ec-grid {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 12px;
    font-size: 11.5px;
    margin: 0;
  }
  .ec-grid dt { color: var(--text-3); font-weight: 500; }
  .ec-grid dd { color: var(--text); margin: 0; font-family: var(--font-mono); word-break: break-all; }

  .cap {
    display: inline-block;
    font-family: var(--font-mono); font-size: 10.5px;
    padding: 2px 6px; margin-right: 4px;
    border-radius: 4px;
    background: var(--surface-3); color: var(--text-3);
    border: 1px solid var(--border);
  }
  .cap--on { color: #6ee7a4; border-color: rgba(70, 200, 120, 0.4); }

  .ec-state {
    display: flex; flex-wrap: wrap; gap: 8px 18px;
    padding-top: 6px;
    border-top: 1px dashed var(--border);
  }
  .ec-variant-hint {
    flex: 1 1 100%;
    margin: 0;
    font-size: 11px; line-height: 1.45; color: var(--text-2);
  }
  .ec-variant-hint code { font-size: 10.5px; }
  .state-pair { display: flex; align-items: center; gap: 6px; }
  .state-key  { font-size: 11px; color: var(--text-3); }
  .state-val  { font-size: 11.5px; color: var(--text); display: inline-flex; align-items: center; gap: 5px; }

  .dot { width: 7px; height: 7px; border-radius: 50%; display: inline-block; }
  .dot--ok  { background: #4ade80; box-shadow: 0 0 4px rgba(74, 222, 128, 0.6); }
  .dot--off { background: var(--text-3); }
  .dot--err { background: #f87171; box-shadow: 0 0 4px rgba(248, 113, 113, 0.6); }

  .ec-note {
    font-size: 11px; line-height: 1.5;
    padding: 7px 10px;
    border-radius: var(--radius-sm);
  }
  .ec-note--err {
    background: rgba(248, 113, 113, 0.08);
    border: 1px solid rgba(248, 113, 113, 0.25);
    color: #fcaaaa;
  }

  .ec-cmd {
    display: flex; align-items: center; gap: 8px;
    padding: 6px 8px 6px 10px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .ec-cmd-text {
    flex: 1; min-width: 0;
    font-family: var(--font-mono); font-size: 11.5px; color: var(--text);
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
    user-select: text;
  }
  .ec-hint { font-size: 10.5px; color: var(--text-3); margin-top: -4px; }

  .alert {
    padding: 10px 12px; border-radius: var(--radius-sm);
    font-size: 12px; line-height: 1.55;
  }
  .alert code {
    font-family: var(--font-mono); font-size: 11px;
    padding: 1px 5px; border-radius: 3px;
    background: rgba(255,255,255,0.05);
  }
  .alert--warn  { background: rgba(240, 175, 80, 0.10); border: 1px solid rgba(240, 175, 80, 0.30); color: #f4c071; }
  .restart-banner {
    display: flex; align-items: center; gap: 12px; justify-content: space-between;
  }
  .restart-banner-text { flex: 1; }
  .restart-banner .btn { flex-shrink: 0; }
  .alert--error { background: rgba(248, 113, 113, 0.10); border: 1px solid rgba(248, 113, 113, 0.30); color: #fcaaaa; }
  .empty {
    padding: 24px; text-align: center; color: var(--text-3); font-size: 12.5px;
    background: var(--surface-2); border: 1px dashed var(--border); border-radius: var(--radius-lg);
  }

  /* btn--xs for inline copy buttons */
  :global(.btn--xs) {
    font-size: 10.5px !important;
    padding: 3px 8px !important;
    line-height: 1.2 !important;
  }

  /* Section-level apply bar for storage changes */
  .storage-apply-bar {
    position: sticky;
    bottom: 0;
    margin-top: 16px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    background: var(--surface-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: 0 -2px 12px rgba(0, 0, 0, 0.25);
  }
  .storage-apply-summary {
    font-size: 12px;
    color: var(--text-2);
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .chip {
    font-size: 11px;
    font-weight: 600;
    color: var(--accent);
    background: rgba(124, 92, 245, 0.14);
    border: 1px solid rgba(124, 92, 245, 0.35);
    padding: 1px 8px;
    border-radius: 999px;
  }
  .storage-apply-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }

  /* Option radios for storage action selection */
  .option-group {
    display: flex; flex-direction: column; gap: 6px;
    padding: 10px 12px;
    background: var(--surface-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .option-group-label {
    font-size: 11px; font-weight: 600; color: var(--text-3);
    text-transform: uppercase; letter-spacing: 0.4px; margin-bottom: 4px;
  }
  .option-radio {
    display: grid;
    grid-template-columns: 16px 1fr;
    grid-template-rows: auto auto;
    column-gap: 8px; row-gap: 1px;
    cursor: pointer;
    padding: 6px 8px;
    border-radius: var(--radius-sm);
    transition: background 100ms ease;
  }
  .option-radio:hover { background: rgba(255,255,255,0.04); }
  .option-radio input[type="radio"] {
    grid-row: 1; grid-column: 1;
    margin-top: 3px; accent-color: var(--accent);
  }
  .option-title {
    grid-row: 1; grid-column: 2;
    font-size: 12px; font-weight: 600; color: var(--text);
  }
  .option-desc {
    grid-row: 2; grid-column: 2;
    font-size: 11px; color: var(--text-2); line-height: 1.4;
  }
  .option-desc code {
    font-family: var(--font-mono); font-size: 10.5px;
    background: var(--surface-3); padding: 1px 4px; border-radius: 3px;
  }
  .option-checkbox {
    display: flex; align-items: center; gap: 8px;
    font-size: 11.5px; color: var(--text); cursor: pointer;
    padding: 4px 8px;
  }
  .option-checkbox input { accent-color: var(--accent); }

  /* Blocking modal */
  .modal-overlay {
    position: fixed; inset: 0;
    background: rgba(0,0,0,0.6);
    display: flex; align-items: center; justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 24px;
    max-width: 420px; width: 90%;
    display: flex; flex-direction: column; gap: 14px;
  }
  .modal-title {
    font-size: 14px; font-weight: 700; color: var(--text); margin: 0;
  }
  .modal-body {
    font-size: 12.5px; color: var(--text-2); line-height: 1.55; margin: 0;
  }
  .lose-list {
    margin: 0; padding-left: 16px;
    font-size: 12px; color: var(--text);
    display: flex; flex-direction: column; gap: 4px;
  }
  .lose-list code {
    font-family: var(--font-mono); font-size: 11.5px;
    background: var(--surface-3); padding: 1px 5px; border-radius: 3px;
  }
  .modal-actions {
    display: flex; gap: 8px; justify-content: flex-end;
  }
  .modal-actions--col {
    flex-direction: column; align-items: stretch;
  }
  .btn--danger {
    background: rgba(248, 113, 113, 0.15);
    color: #f87171;
    border: 1px solid rgba(248, 113, 113, 0.3);
  }
  .btn--danger:hover { background: rgba(248, 113, 113, 0.25); }

  /* Daemon & Service section */
  .service-status-grid {
    display: flex; gap: 20px; flex-wrap: wrap;
    padding: 6px 0;
  }
  .daemon-actions {
    display: flex; gap: 10px; flex-wrap: wrap;
  }

  /* About */
  .about-card {
    display: flex; flex-direction: column; align-items: center; gap: 8px;
    padding: 32px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    text-align: center;
  }
  .about-logo {
    width: 80px; height: 80px;
    border-radius: 18px; object-fit: contain;
    filter: drop-shadow(0 0 16px rgba(90, 160, 255, 0.5));
  }
  .about-name    { font-size: 18px; font-weight: 700; color: var(--text); }
  .about-tagline { font-size: 12.5px; color: var(--text-2); }
  .about-links   { display: flex; gap: 12px; margin-top: 8px; }
  .about-link    { font-size: 12px; color: var(--accent-2); text-decoration: none; }
  .about-link:hover { text-decoration: underline; }
</style>
