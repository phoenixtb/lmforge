<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { isOnline } from '$lib/stores/status';
  import {
    metricsStore,
    metricsError,
    startMetricsPolling,
    stopMetricsPolling,
  } from '$lib/stores/metrics';
  import { listLogs, tailLog, streamLog, fmtUptime, type LogIndex } from '$lib/api';
  import { dragOnEmpty } from '$lib/drag';

  // ── Metrics ─────────────────────────────────────────────────────────────
  $: m = $metricsStore;
  $: endpointRows = Object.entries(m.endpoints).sort(([a], [b]) => a.localeCompare(b));
  $: chatStats = m.endpoints['/v1/chat/completions'];
  $: chatP95 = chatStats?.p95_ms != null ? `${chatStats.p95_ms.toFixed(0)} ms` : '—';
  $: errorRatePct = (m.error_rate * 100).toFixed(m.error_rate >= 0.01 ? 1 : 2);
  $: errorColor = m.error_rate > 0.05 ? 'var(--danger)' : m.error_rate > 0.01 ? 'var(--warn)' : 'var(--text)';

  // ── Logs ────────────────────────────────────────────────────────────────
  let logIndex: LogIndex = { components: [] };
  let selectedComponent = '';
  let selectedStream: 'stdout' | 'stderr' | 'main' = 'stderr';
  let logText = '';
  let following = false;
  let followCancel: (() => void) | null = null;
  let logScroll: HTMLPreElement | null = null;
  let logError: string | null = null;

  async function refreshLogIndex() {
    try {
      logIndex = await listLogs();
      // Auto-select first non-daemon component on first load
      if (!selectedComponent && logIndex.components.length > 0) {
        const first = logIndex.components.find(c => c.component !== 'daemon')
                   ?? logIndex.components[0];
        selectedComponent = first.component;
        selectedStream = first.streams.some(s => s.stream === 'stderr') ? 'stderr'
                       : first.component === 'daemon' ? 'main'
                       : 'stdout';
        await refreshTail();
      }
    } catch (e) {
      logError = String(e);
    }
  }

  async function refreshTail() {
    if (!selectedComponent) return;
    stopFollowing();
    try {
      logText = await tailLog(selectedComponent, selectedStream, 500);
      logError = null;
      requestAnimationFrame(() => { if (logScroll) logScroll.scrollTop = logScroll.scrollHeight; });
    } catch (e) {
      logError = String(e);
    }
  }

  function startFollowing() {
    if (!selectedComponent) return;
    stopFollowing();
    following = true;
    logError = null;
    followCancel = streamLog(
      selectedComponent,
      selectedStream,
      (line) => {
        logText = logText ? `${logText}\n${line}` : line;
        if (logScroll) {
          const nearBottom = logScroll.scrollHeight - logScroll.scrollTop - logScroll.clientHeight < 80;
          if (nearBottom) requestAnimationFrame(() => { logScroll!.scrollTop = logScroll!.scrollHeight; });
        }
      },
      (err) => { logError = err; following = false; },
    );
  }

  function stopFollowing() {
    if (followCancel) { followCancel(); followCancel = null; }
    following = false;
  }

  function toggleFollow() {
    following ? stopFollowing() : startFollowing();
  }

  function onComponentChange() {
    refreshTail();
  }
  function onStreamChange() {
    refreshTail();
  }

  $: availableStreams = (logIndex.components.find(c => c.component === selectedComponent)?.streams ?? [])
    .map(s => s.stream);

  // ── Lifecycle ──────────────────────────────────────────────────────────
  onMount(() => {
    startMetricsPolling();
    refreshLogIndex();
  });
  onDestroy(() => {
    stopMetricsPolling();
    stopFollowing();
  });

  function fmtRate(num: number, denomSecs: number): string {
    if (denomSecs <= 0) return '—';
    const rps = num / denomSecs;
    if (rps >= 1) return `${rps.toFixed(1)}/s`;
    if (rps > 0) return `${(rps * 60).toFixed(1)}/min`;
    return '0';
  }

  function fmtMs(v: number | null | undefined): string {
    if (v == null) return '—';
    if (v >= 1000) return `${(v / 1000).toFixed(2)} s`;
    return `${v.toFixed(0)} ms`;
  }
</script>

<svelte:head><title>LMForge — Observability</title></svelte:head>

<div class="page">
  <div class="toolbar" data-tauri-drag-region onpointerdown={dragOnEmpty} role="toolbar">
    <h1>Observability</h1>
    <div class="tr">
      {#if $isOnline}
        <span class="badge badge--green">Live</span>
        <span class="el mono">uptime {fmtUptime(m.uptime_secs)}</span>
      {:else}
        <span class="badge badge--grey">Offline</span>
      {/if}
      {#if m.recorder_unavailable}
        <span class="badge badge--amber" title="Prometheus recorder failed to install at startup">recorder unavailable</span>
      {/if}
    </div>
  </div>

  <div class="body">
    {#if $metricsError}
      <div class="error-strip">{$metricsError}</div>
    {/if}

    <!-- ── KPI strip ─────────────────────────────────────────────────────── -->
    <div class="metrics">
      <div class="metric">
        <span class="mv mono">{m.requests_total.toLocaleString()}</span>
        <span class="ml">Requests</span>
        <span class="ms">{fmtRate(m.requests_total, m.uptime_secs)} avg</span>
      </div>
      <div class="metric">
        <span class="mv mono" style="color:{errorColor}">{errorRatePct}<span class="util-unit">%</span></span>
        <span class="ml">Error rate</span>
        <span class="ms">{m.errors_total.toLocaleString()} of {m.requests_total.toLocaleString()}</span>
      </div>
      <div class="metric">
        <span class="mv mono">{chatP95}</span>
        <span class="ml">Chat p95</span>
        <span class="ms">/v1/chat/completions</span>
      </div>
      <div class="metric">
        <span class="mv mono">{m.active_models}</span>
        <span class="ml">Active models</span>
        <span class="ms">{Object.keys(m.model_loads).length} known</span>
      </div>
    </div>

    <!-- ── Per-endpoint table ────────────────────────────────────────────── -->
    <section class="panel" aria-label="Endpoint stats">
      <header class="panel-hd">
        <h2>Endpoint latency &amp; volume</h2>
        <span class="ps">Polled every 5s</span>
      </header>
      {#if endpointRows.length === 0}
        <div class="empty">No traffic observed yet.</div>
      {:else}
        <div class="tbl-wrap">
          <table class="tbl">
            <thead>
              <tr>
                <th class="tl">Endpoint</th>
                <th>Requests</th>
                <th>Errors</th>
                <th>p50</th>
                <th>p95</th>
                <th>p99</th>
              </tr>
            </thead>
            <tbody>
              {#each endpointRows as [endpoint, stats]}
                <tr>
                  <td class="mono tl">{endpoint}</td>
                  <td class="mono">{stats.requests_total.toLocaleString()}</td>
                  <td class="mono" class:err={stats.errors_total > 0}>
                    {stats.errors_total.toLocaleString()}
                  </td>
                  <td class="mono">{fmtMs(stats.p50_ms)}</td>
                  <td class="mono">{fmtMs(stats.p95_ms)}</td>
                  <td class="mono">{fmtMs(stats.p99_ms)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>

    <!-- ── Cold-load + image preflight summary ───────────────────────────── -->
    <div class="two-col">
      <section class="panel" aria-label="Model loads">
        <header class="panel-hd">
          <h2>Model loads</h2>
          <span class="ps">{Object.keys(m.model_loads).length} models tracked</span>
        </header>
        {#if Object.keys(m.model_loads).length === 0}
          <div class="empty">No load attempts recorded.</div>
        {:else}
          <ul class="kv-list">
            {#each Object.entries(m.model_loads) as [model, stats]}
              <li>
                <span class="kv-k mono" title={model}>{model}</span>
                <span class="kv-v mono">
                  ok {stats.success}
                  {#if stats.failure > 0}<span class="err"> · fail {stats.failure}</span>{/if}
                  {#if stats.last_dur_s != null} · avg {stats.last_dur_s.toFixed(1)}s{/if}
                </span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section class="panel" aria-label="Other counters">
        <header class="panel-hd">
          <h2>Counters</h2>
        </header>
        <ul class="kv-list">
          <li><span class="kv-k">Image inputs (accepted)</span><span class="kv-v mono">{m.image_inputs.accepted}</span></li>
          <li><span class="kv-k">Image inputs (rejected)</span><span class="kv-v mono" class:err={m.image_inputs.rejected > 0}>{m.image_inputs.rejected}</span></li>
          <li><span class="kv-k">Image inputs (data URL)</span><span class="kv-v mono">{m.image_inputs.data_url}</span></li>
          <li><span class="kv-k">Auth rejections</span><span class="kv-v mono" class:err={m.auth_rejections > 0}>{m.auth_rejections}</span></li>
        </ul>
      </section>
    </div>

    <!-- ── Log viewer ────────────────────────────────────────────────────── -->
    <section class="panel panel--logs" aria-label="Logs">
      <header class="panel-hd log-hd">
        <h2>Logs</h2>
        <div class="log-controls">
          <select bind:value={selectedComponent} onchange={onComponentChange}>
            {#if logIndex.components.length === 0}
              <option value="">no logs found</option>
            {/if}
            {#each logIndex.components as c}
              <option value={c.component}>{c.component}</option>
            {/each}
          </select>
          <select bind:value={selectedStream} onchange={onStreamChange}>
            {#each availableStreams as s}
              <option value={s}>{s}</option>
            {/each}
            {#if availableStreams.length === 0}
              <option value="stderr">stderr</option>
              <option value="stdout">stdout</option>
              <option value="main">main</option>
            {/if}
          </select>
          <button class="btn btn--ghost" onclick={refreshTail} disabled={!selectedComponent}>Refresh</button>
          <button class="btn" class:active={following} onclick={toggleFollow} disabled={!selectedComponent}>
            {following ? 'Following' : 'Follow'}
          </button>
          <button class="btn btn--ghost" onclick={refreshLogIndex} title="Re-scan logs/">Re-scan</button>
        </div>
      </header>
      {#if logError}
        <div class="error-strip">{logError}</div>
      {/if}
      <pre bind:this={logScroll} class="log-pre">{logText || (selectedComponent ? '(empty)' : 'Pick a component to view its log.')}</pre>
    </section>
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

  .toolbar {
    height: var(--toolbar-h);
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 20px; border-bottom: 1px solid var(--border); flex-shrink: 0;
  }
  .toolbar h1 { font-size: 14px; font-weight: 600; color: var(--text); }
  .tr  { display: flex; align-items: center; gap: 8px; }
  .el  { font-size: 11px; color: var(--text-3); }

  .body {
    flex: 1; overflow-y: auto;
    padding: 16px 18px;
    display: flex; flex-direction: column; gap: 14px;
  }

  .error-strip {
    background: var(--danger-dim); color: var(--danger);
    padding: 7px 11px; border-radius: var(--radius-md);
    font-size: 12px;
  }

  /* ── KPI strip (mirrors Overview metric tiles) ─────────────────────── */
  .metrics { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; }
  .metric {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: var(--radius-lg); padding: 11px 14px;
    display: flex; flex-direction: column; gap: 2px;
    transition: border-color 130ms;
  }
  .metric:hover { border-color: var(--border-2); }
  .mv { font-size: 22px; font-weight: 600; color: var(--text); letter-spacing: -0.6px; line-height: 1.1; }
  .ml { font-size: 10px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.5px; margin-top: 2px; }
  .ms { font-size: 9.5px; color: var(--text-3); opacity: 0.7; }
  .util-unit { font-size: 12px; font-weight: 400; letter-spacing: 0; }

  /* ── Panels ─────────────────────────────────────────────────────────── */
  .panel {
    background: var(--surface); border: 1px solid var(--border);
    border-radius: var(--radius-xl); padding: 14px 16px;
    display: flex; flex-direction: column; gap: 10px;
  }
  .panel-hd { display: flex; justify-content: space-between; align-items: center; }
  .panel-hd h2 { font-size: 12.5px; font-weight: 600; }
  .ps { font-size: 10.5px; color: var(--text-3); }
  .empty { color: var(--text-3); font-size: 12px; font-style: italic; padding: 4px 0; }

  .two-col {
    display: grid; grid-template-columns: 1.4fr 1fr; gap: 12px;
  }

  /* ── Endpoint table ─────────────────────────────────────────────────── */
  .tbl-wrap { overflow-x: auto; }
  .tbl {
    width: 100%; border-collapse: collapse;
    font-size: 11.5px;
  }
  .tbl thead th {
    text-align: right; padding: 6px 10px;
    font-size: 10px; color: var(--text-3); font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.5px;
    border-bottom: 1px solid var(--divider);
  }
  .tbl thead .tl, .tbl tbody .tl { text-align: left; }
  .tbl tbody td {
    padding: 6px 10px; text-align: right;
    border-bottom: 1px solid var(--divider);
    color: var(--text-2);
  }
  .tbl tbody tr:last-child td { border-bottom: none; }
  .tbl tbody td.err { color: var(--danger); }

  /* ── Key/value lists ────────────────────────────────────────────────── */
  .kv-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  .kv-list li {
    display: flex; justify-content: space-between; gap: 12px;
    padding: 4px 0; border-bottom: 1px solid var(--divider);
    font-size: 11.5px;
  }
  .kv-list li:last-child { border-bottom: none; }
  .kv-k {
    color: var(--text-3); flex: 1; min-width: 0;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .kv-v { color: var(--text); flex-shrink: 0; }
  .err { color: var(--danger); }

  /* ── Logs ───────────────────────────────────────────────────────────── */
  .panel--logs { flex: 1; min-height: 280px; }
  .log-hd { gap: 12px; }
  .log-controls { display: flex; align-items: center; gap: 6px; }
  .log-controls select {
    background: var(--surface-2);
    color: var(--text);
    color-scheme: dark;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 4px 24px 4px 8px;
    font-size: 11.5px;
    font-family: var(--font-mono);
    -webkit-appearance: none;
    appearance: none;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6' fill='none'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%23ffffff99' stroke-width='1.4' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 8px center;
  }
  .log-controls select option {
    background: var(--surface-2);
    color: var(--text);
  }
  .btn {
    background: var(--surface-2); color: var(--text);
    border: 1px solid var(--border); border-radius: var(--radius-sm);
    padding: 4px 10px; font-size: 11px; cursor: pointer;
    transition: background 100ms, border-color 100ms;
  }
  .btn:hover:not(:disabled) { background: var(--surface-3); border-color: var(--border-2); }
  .btn:disabled { opacity: 0.4; cursor: default; }
  .btn--ghost { background: transparent; }
  .btn.active { background: var(--accent-dim); color: var(--accent-2); border-color: transparent; }

  .log-pre {
    flex: 1; min-height: 200px;
    background: #111114; border: 1px solid var(--border); border-radius: var(--radius-md);
    padding: 10px 12px; margin: 0;
    font-family: var(--font-mono); font-size: 11px; line-height: 1.45;
    color: var(--text-2); white-space: pre-wrap; word-break: break-all;
    overflow: auto;
    user-select: text;
  }
</style>
