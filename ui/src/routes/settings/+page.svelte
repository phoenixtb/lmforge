<svelte:head><title>LMForge — Settings</title></svelte:head>

<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { open } from '@tauri-apps/plugin-dialog';
  import { dragOnEmpty } from '$lib/drag';
  import { toast } from '$lib/stores/toasts';

  // ── Catalog directory ───────────────────────────────────────────────────────
  let catalogDir  = $state('');
  let savingCatalog = $state(false);

  async function browseCatalogDir() {
    try {
      const selected = await open({ directory: true, title: 'Select catalog directory' });
      if (selected && typeof selected === 'string') catalogDir = selected;
    } catch { /* cancelled */ }
  }

  async function saveCatalogDir() {
    if (!catalogDir.trim()) return;
    savingCatalog = true;
    try {
      await invoke('set_catalog_dir', { path: catalogDir.trim() });
      toast.success('Catalog directory updated');
    } catch (e) {
      toast.error(`Failed to save: ${e}`);
    } finally {
      savingCatalog = false;
    }
  }

  // Load current value on mount
  import { onMount } from 'svelte';
  onMount(async () => {
    try {
      const cur: string = await invoke('get_catalog_dir');
      if (cur) catalogDir = cur;
    } catch { /* best-effort */ }
  });

  // ── Section helper ──────────────────────────────────────────────────────────
  type SettingSection = { id: string; label: string; icon: string };
  const SECTIONS: SettingSection[] = [
    { id: 'catalog',  label: 'Catalog',  icon: '📂' },
    { id: 'engine',   label: 'Engine',   icon: '⚙' },
    { id: 'about',    label: 'About',    icon: 'ℹ' },
  ];
  let activeSection = $state('catalog');
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

      {#if activeSection === 'catalog'}
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
          <h2 class="section-title">Engine</h2>
          <p class="section-desc">Engine configuration is managed in <code>~/.lmforge/config.toml</code>.</p>

          <div class="coming-soon">
            <span class="cs-icon">⚙</span>
            <div>
              <div class="cs-title">Engine settings UI coming soon</div>
              <div class="cs-body">You can manually edit <code>~/.lmforge/config.toml</code> to configure the engine port, VRAM limits, and model paths.</div>
            </div>
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

  /* Coming soon */
  .coming-soon {
    display: flex; align-items: flex-start; gap: 14px;
    padding: 16px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    color: var(--text-2);
  }
  .cs-icon  { font-size: 24px; flex-shrink: 0; margin-top: 2px; }
  .cs-title { font-size: 13px; font-weight: 600; color: var(--text); margin-bottom: 4px; }
  .cs-body  { font-size: 12px; line-height: 1.6; }
  .cs-body code {
    font-family: var(--font-mono); font-size: 11px;
    background: var(--surface-3); padding: 1px 4px; border-radius: 3px;
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
