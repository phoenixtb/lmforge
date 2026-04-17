<script lang="ts">
  import { statusStore, isOnline } from '$lib/stores/status';
  import { fmtUptime } from '$lib/api';

  // Derive display values from the reactive store
  $: status    = $statusStore.overall_status;
  $: engineId  = $statusStore.engine_id;
  $: metrics   = $statusStore.metrics;
  $: models    = Object.values($statusStore.running_models);
  $: ttft      = metrics.ttft_avg_ms > 0 ? `${Math.round(metrics.ttft_avg_ms)} ms` : '—';
  $: uptime    = metrics.uptime_secs > 0 ? fmtUptime(metrics.uptime_secs) : '—';
  $: model     = models.length > 0 ? models[0].model_id : '—';

  $: dotClass = {
    ready:    'dot dot--ready',
    degraded: 'dot dot--degraded',
    error:    'dot dot--error',
    starting: 'dot dot--starting',
    stopped:  'dot dot--offline',
  }[status] ?? 'dot dot--offline';

  $: statusLabel = {
    ready:    'Ready',
    degraded: 'Degraded',
    error:    'Error',
    starting: 'Starting',
    stopped:  'Offline',
  }[status] ?? 'Offline';
</script>

<!--
  Always-visible engine health bar.
  Reads from statusStore — zero additional HTTP requests.
  Format: [● ready]  oMLX  |  Qwen3-8B  |  TTFT 180 ms  |  ↑ 3h 24m
-->
<div class="statusbar" role="status" aria-live="polite">
  <!-- Status indicator -->
  <div class="statusbar-group">
    <span class={dotClass}></span>
    <span class="statusbar-label" class:muted={status === 'stopped'}>{statusLabel}</span>
  </div>

  {#if $isOnline}
    <span class="statusbar-sep">|</span>

    <!-- Engine ID -->
    <div class="statusbar-group">
      <span class="statusbar-value mono">{engineId}</span>
    </div>

    {#if model !== '—'}
      <span class="statusbar-sep">|</span>
      <div class="statusbar-group">
        <span class="statusbar-icon">&#x2B26;</span>
        <span class="statusbar-value">{model}</span>
      </div>
    {/if}

    {#if metrics.ttft_avg_ms > 0}
      <span class="statusbar-sep">|</span>
      <div class="statusbar-group">
        <span class="statusbar-dimval">TTFT</span>
        <span class="statusbar-value mono">{ttft}</span>
      </div>
    {/if}

    {#if metrics.uptime_secs > 0}
      <span class="statusbar-sep">|</span>
      <div class="statusbar-group">
        <span class="statusbar-icon">&#x2191;</span>
        <span class="statusbar-value mono">{uptime}</span>
      </div>
    {/if}
  {:else}
    <span class="statusbar-sep">|</span>
    <span class="statusbar-offline">Daemon offline — waiting for connection</span>
  {/if}
</div>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--statusbar-h);
    padding: 0 14px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    overflow: hidden;
  }

  .statusbar-group {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
  }

  .statusbar-sep {
    color: var(--border-2);
    font-size: 11px;
    flex-shrink: 0;
  }

  .statusbar-label,
  .statusbar-value {
    font-size: 11.5px;
    color: var(--text-2);
    white-space: nowrap;
  }

  .statusbar-dimval {
    font-size: 11px;
    color: var(--text-3);
  }

  .statusbar-icon {
    font-size: 11px;
    color: var(--text-3);
  }

  .statusbar-offline {
    font-size: 11.5px;
    color: var(--text-3);
    font-style: italic;
  }

  .muted { color: var(--text-3); }
</style>
