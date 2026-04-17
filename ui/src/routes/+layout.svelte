<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { invoke } from '@tauri-apps/api/core';
  import '../app.css';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import ToastContainer from '$lib/components/ToastContainer.svelte';
  import { statusStore, daemonOnline, isConnecting } from '$lib/stores/status';
  import { loadHardware } from '$lib/stores/hardware';
  import { startSysInfoPolling, stopSysInfoPolling } from '$lib/stores/sysinfo';
  import { normalizeStatus } from '$lib/api';
  import type { LfStatus } from '$lib/api';

  let unlistenHealth: (() => void) | null = null;
  let unlistenStatus: (() => void) | null = null;
  let hwRetryTimer: ReturnType<typeof setTimeout> | null = null;
  let destroyed = false;
  let starting = false;
  let startError: string | null = null;

  $: online = $daemonOnline;
  $: connecting = $isConnecting;

  async function tryLoadHardware(attempts = 0): Promise<void> {
    try { await loadHardware(); }
    catch {
      if (!destroyed && attempts < 5) {
        const delay = Math.min(1000 * 2 ** attempts, 16000);
        hwRetryTimer = setTimeout(() => tryLoadHardware(attempts + 1), delay);
      }
    }
  }

  async function startEngine() {
    starting = true;
    startError = null;
    try {
      await invoke('start_engine');
    } catch (e: unknown) {
      startError = String(e);
    } finally {
      starting = false;
    }
  }

  onMount(async () => {
    // Listen for daemon reachability changes
    unlistenHealth = await listen<{ online: boolean }>('lf:health', (event) => {
      daemonOnline.set(event.payload.online);
      if (event.payload.online) {
        // Daemon just came online — kick off hardware load and polling
        tryLoadHardware();
        startSysInfoPolling();
      } else {
        stopSysInfoPolling();
      }
    });

    // Listen for engine state updates
    unlistenStatus = await listen<LfStatus>('lf:status', (event) => {
      statusStore.set(normalizeStatus(event.payload as Parameters<typeof normalizeStatus>[0]));
    });
  });

  onDestroy(() => {
    destroyed = true;
    unlistenHealth?.();
    unlistenStatus?.();
    if (hwRetryTimer) clearTimeout(hwRetryTimer);
    stopSysInfoPolling();
  });
</script>

{#if connecting}
  <!-- ── Connecting splash (first 2-4s before first health event) ── -->
  <div class="daemon-screen">
    <div class="daemon-card">
      <div class="lf-logo">⬡</div>
      <h1>LMForge</h1>
      <p class="status-line connecting">Connecting to engine…</p>
      <div class="spinner"></div>
    </div>
  </div>

{:else if online === false}
  <!-- ── Engine offline screen ── -->
  <div class="daemon-screen">
    <div class="daemon-card">
      <div class="lf-logo offline">⬡</div>
      <h1>LMForge</h1>
      <p class="status-line offline">Engine not running</p>
      <p class="hint">
        The LMForge engine is not reachable at <code>localhost:11430</code>.<br>
        Start it below, or run <code>lmforge start</code> in a terminal.
      </p>

      {#if startError}
        <p class="start-error">{startError}</p>
      {/if}

      <button class="start-btn" on:click={startEngine} disabled={starting}>
        {#if starting}
          <span class="btn-spinner"></span> Starting…
        {:else}
          ▶ Start Engine
        {/if}
      </button>

      <p class="hint-small">
        Or install as a service so it starts automatically:<br>
        <code>lmforge service install</code>
      </p>
    </div>
  </div>

{:else}
  <!-- ── Normal app shell ── -->
  <div class="app-shell">
    <Sidebar />
    <div class="content-region">
      <slot />
    </div>
    <ToastContainer />
  </div>
{/if}

<style>
  /* ── App shell (normal state) ─────────────────────────────────── */
  :global(body) { margin: 0; padding: 0; }

  .app-shell {
    display: flex;
    height: 100vh; width: 100vw;
    overflow: hidden;
    background: transparent;
  }
  .content-region {
    flex: 1; display: flex; flex-direction: column;
    overflow: hidden; background: var(--content-bg);
  }

  /* ── Daemon offline / connecting screens ──────────────────────── */
  .daemon-screen {
    display: flex; align-items: center; justify-content: center;
    height: 100vh; width: 100vw;
    background: var(--bg, #0f1117);
  }

  .daemon-card {
    display: flex; flex-direction: column; align-items: center;
    gap: 12px; padding: 40px 48px;
    background: var(--surface, rgba(255,255,255,0.04));
    border: 1px solid var(--border, rgba(255,255,255,0.08));
    border-radius: 16px;
    max-width: 400px; width: 90%;
    text-align: center;
  }

  .lf-logo {
    font-size: 42px; line-height: 1;
    color: var(--accent, #6ee7b7);
    filter: drop-shadow(0 0 18px rgba(110,231,183,0.35));
  }
  .lf-logo.offline { color: var(--text-3, #555); filter: none; }

  .daemon-card h1 {
    margin: 0; font-size: 22px; font-weight: 700;
    color: var(--text, #e8eaf0); letter-spacing: -0.5px;
  }

  .status-line {
    margin: 0; font-size: 13px; font-weight: 500;
  }
  .status-line.connecting { color: var(--text-2, #9ca3af); }
  .status-line.offline    { color: var(--warn, #fbbf24); }

  .hint {
    margin: 0; font-size: 12px; color: var(--text-3, #6b7280);
    line-height: 1.6;
  }
  .hint code, .hint-small code {
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    background: rgba(255,255,255,0.06); padding: 1px 5px;
    border-radius: 4px; font-size: 11px;
  }
  .hint-small {
    margin: 4px 0 0; font-size: 11px;
    color: var(--text-3, #6b7280); line-height: 1.6;
  }

  .start-btn {
    display: flex; align-items: center; justify-content: center; gap: 7px;
    padding: 10px 24px; border-radius: 8px; border: none; cursor: pointer;
    background: var(--accent, #6ee7b7); color: #0f1117;
    font-size: 13px; font-weight: 700; letter-spacing: 0.2px;
    transition: opacity 150ms, transform 150ms;
    min-width: 160px;
  }
  .start-btn:hover:not(:disabled) { opacity: 0.88; transform: translateY(-1px); }
  .start-btn:disabled { opacity: 0.45; cursor: default; }

  .start-error {
    margin: 0; font-size: 11.5px; color: var(--danger, #f87171);
    background: rgba(248,113,113,0.08); padding: 8px 12px;
    border-radius: 6px; border: 1px solid rgba(248,113,113,0.2);
    text-align: left; width: 100%; box-sizing: border-box;
  }

  /* Spinners */
  .spinner {
    width: 20px; height: 20px; border-radius: 50%;
    border: 2px solid rgba(110,231,183,0.2);
    border-top-color: var(--accent, #6ee7b7);
    animation: spin 0.8s linear infinite;
  }
  .btn-spinner {
    width: 12px; height: 12px; border-radius: 50%;
    border: 2px solid rgba(15,17,23,0.3);
    border-top-color: #0f1117;
    animation: spin 0.8s linear infinite;
    display: inline-block;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
