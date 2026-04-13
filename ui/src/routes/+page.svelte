<script lang="ts">
  import { onMount, onDestroy } from "svelte";

  let statusMessage = "Unknown";
  let isRunning = false;
  let activeModel = "Waiting...";
  let interval: ReturnType<typeof setInterval>;

  async function fetchStatus() {
    try {
      const response = await fetch("http://localhost:11430/lf/status");
      if (response.ok) {
        const data = await response.json();
        // Since we hit the status endpoint, we can actually parse real daemon stats!
        statusMessage = data.status === "ready" ? "Running" : "Starting";
        isRunning = data.status === "ready";
        activeModel = data.active_model || "None Loaded";
      } else {
        throw new Error("Bad response");
      }
    } catch {
      statusMessage = "Stopped";
      isRunning = false;
      activeModel = "None";
    }
  }

  onMount(() => {
    fetchStatus();
    interval = setInterval(fetchStatus, 1500); 
  });

  onDestroy(() => {
    if (interval) clearInterval(interval);
  });
</script>

<svelte:head>
  <style>
    :root {
      --bg: #1c1c1e;
      --sidebar: #1e1e1e;
      --surface: #2c2c2e;
      --border: rgba(255,255,255,0.08);
      --text: #ffffff;
      --text-dim: #98989d;
      --accent: #0b84ff;
      --accent-dim: rgba(11, 132, 255, 0.15);
      --success: #32d74b;
      --danger: #ff453a;
    }
    body {
      margin: 0;
      padding: 0;
      background: var(--bg);
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      font-size: 13px;
      user-select: none;
      -webkit-font-smoothing: antialiased;
      letter-spacing: -0.1px;
    }
  </style>
</svelte:head>

<main class="app-layout">
  <!-- Desktop App Sidebar Equivalent -->
  <nav class="sidebar">
    <div class="brand">
      <div class="mac-dots">
        <span class="dot close"></span>
        <span class="dot min"></span>
        <span class="dot max"></span>
      </div>
      <h2>LMForge Orchestrator</h2>
    </div>

    <div class="nav-section">
      <h3>Machinery</h3>
      <a href="#" class="nav-item active">
        <svg fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 10V3L4 14h7v7l9-11h-7z" /></svg>
        Overview
      </a>
      <a href="#" class="nav-item">
        <svg fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 11H5m14 0a2 2 0 012 2v6a2 2 0 01-2 2H5a2 2 0 01-2-2v-6a2 2 0 012-2m14 0V9a2 2 0 00-2-2M5 11V9a2 2 0 002-2m0 0V5a2 2 0 012-2h6a2 2 0 012 2v2M7 7h10" /></svg>
        Model Library
      </a>
      <a href="#" class="nav-item">
        <svg fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" /><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" /></svg>
        Preferences
      </a>
    </div>
  </nav>

  <!-- Main Content -->
  <div class="content">
    <header class="content-header">
      <h1>Dashboard</h1>
      <div class="status-badge" class:running={isRunning}>
        <span class="status-dot"></span>
        {statusMessage}
      </div>
    </header>

    <div class="grid">
      <!-- Engine Status Panel -->
      <section class="panel">
        <div class="panel-header">
          <h2>Inference Engine</h2>
          <span class="pill">oMLX macOS Native</span>
        </div>
        
        <table class="properties-table">
          <tbody>
            <tr>
              <td class="key">Active Port</td>
              <td class="value">
                <div class="code-block">11430</div> <span class="dim">(OpenAI REST Proxy)</span>
              </td>
            </tr>
            <tr>
              <td class="key">Primary Model</td>
              <td class="value">
                <span class={isRunning ? 'val-active' : 'val-dim'}>{activeModel}</span>
              </td>
            </tr>
            <tr>
              <td class="key">Engine Auto-Start</td>
              <td class="value">Managed natively by Tauri IPC</td>
            </tr>
          </tbody>
        </table>
      </section>

      <!-- Hardware Panel -->
      <section class="panel narrow">
        <div class="panel-header">
          <h2>Hardware Allocation</h2>
        </div>
        <div class="resource-block">
          <div class="res-head">
            <span>Unified Memory (M3 Profile)</span>
            <span class="memory-value">12.0 / 36.0 GB</span>
          </div>
          <div class="bar-container">
            <div class="bar-fill" style="width: 33%;"></div>
          </div>
          <p class="help-text">System dynamically pages inactive models to SSD cache.</p>
        </div>

        <div class="resource-block mt">
          <div class="res-head">
            <span>Process Priority</span>
            <span class="dim-value">High (Interactive)</span>
          </div>
        </div>
      </section>
    </div>
  </div>
</main>

<style>
  .app-layout {
    display: flex;
    height: 100vh;
    width: 100vw;
  }

  /* Sidebar Design */
  .sidebar {
    width: 240px;
    background: var(--sidebar);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
  }

  .brand {
    padding: 16px;
    -webkit-app-region: drag; /* Makes top area draggable in Tauri */
    display: flex;
    align-items: center;
    gap: 12px;
    height: 48px;
    box-sizing: border-box;
  }

  .mac-dots {
    display: flex;
    gap: 6px;
    margin-right: 8px;
  }

  .dot {
    width: 11px;
    height: 11px;
    border-radius: 50%;
  }

  /* Mock dots for aesthetic natively - Tauri window chrome handles real ones usually if topbar is hidden */
  .dot.close { background: #ff5f56; }
  .dot.min { background: #ffbd2e; }
  .dot.max { background: #27c93f; }

  .brand h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .nav-section {
    padding: 12px;
  }

  .nav-section h3 {
    margin: 0 0 8px 8px;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-dim);
    text-transform: uppercase;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 6px;
    color: var(--text);
    text-decoration: none;
    font-size: 13px;
    font-weight: 500;
    transition: background 0.1s;
  }

  .nav-item svg {
    width: 16px;
    height: 16px;
    color: var(--text-dim);
  }

  .nav-item:hover {
    background: rgba(255,255,255,0.05);
  }

  .nav-item.active {
    background: var(--accent);
    color: white;
  }

  .nav-item.active svg {
    color: white;
  }

  /* Main Content Area */
  .content {
    flex: 1;
    background: var(--bg);
    display: flex;
    flex-direction: column;
  }

  .content-header {
    height: 48px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 24px;
    border-bottom: 1px solid var(--border);
    -webkit-app-region: drag;
  }

  .content-header h1 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .status-badge {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 8px;
    border-radius: 4px;
    background: rgba(255,255,255,0.05);
    font-size: 11px;
    font-weight: 600;
    color: var(--text-dim);
    border: 1px solid var(--border);
  }

  .status-badge.running {
    background: rgba(50, 215, 75, 0.1);
    color: var(--success);
    border-color: rgba(50, 215, 75, 0.2);
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-dim);
  }

  .status-badge.running .status-dot {
    background: var(--success);
    box-shadow: 0 0 6px var(--success);
  }

  /* Dashboard Grid */
  .grid {
    padding: 24px;
    display: flex;
    gap: 16px;
  }

  .panel {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 16px;
    flex: 2;
    box-shadow: 0 2px 12px rgba(0,0,0,0.2);
  }

  .panel.narrow {
    flex: 1;
  }

  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
  }

  .panel h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }

  .pill {
    padding: 2px 6px;
    background: var(--border);
    border-radius: 4px;
    font-size: 10px;
    font-weight: 500;
    color: var(--text-dim);
  }

  /* Table Style details */
  .properties-table {
    width: 100%;
    border-collapse: collapse;
  }

  .properties-table td {
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
  }

  .properties-table tr:last-child td {
    border-bottom: none;
  }

  .key {
    width: 35%;
    color: var(--text-dim);
    font-size: 12px;
  }

  .value {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 500;
  }

  .code-block {
    background: rgba(0,0,0,0.3);
    padding: 2px 6px;
    border-radius: 4px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    border: 1px solid rgba(255,255,255,0.05);
  }

  .dim { color: var(--text-dim); font-size: 12px; font-weight: 400; }
  .val-active { color: var(--accent); }
  .val-dim { color: var(--text-dim); font-style: italic; }

  /* Hardware Resources */
  .resource-block {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .mt { margin-top: 24px; }

  .res-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 12px;
  }

  .memory-value {
    font-family: ui-monospace, SFMono-Regular, monospace;
    font-size: 11px;
    color: var(--text-dim);
  }

  .dim-value {
    font-size: 12px;
    color: var(--text-dim);
  }

  .bar-container {
    height: 6px;
    background: rgba(0,0,0,0.4);
    border-radius: 3px;
    overflow: hidden;
  }

  .bar-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
  }

  .help-text {
    margin: 4px 0 0 0;
    font-size: 11px;
    color: var(--text-dim);
    line-height: 1.4;
  }
</style>
