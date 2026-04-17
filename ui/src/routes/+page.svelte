<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount, onDestroy } from 'svelte';
  import { statusStore, isOnline } from '$lib/stores/status';
  import { hardwareStore } from '$lib/stores/hardware';
  import { sysInfoStore } from '$lib/stores/sysinfo';
  import { fmtUptime } from '$lib/api';
  import { dragOnEmpty } from '$lib/drag';

  $: status  = $statusStore.overall_status;
  $: engine  = $statusStore.engine_id;
  $: version = $statusStore.engine_version;
  $: metrics = $statusStore.metrics;
  $: slots   = Object.values($statusStore.running_models);
  $: hw      = $hardwareStore;
  $: sys     = $sysInfoStore;

  // ── Uptime ticker ─────────────────────────────────────────────────────────────
  let clientUptime = 0;
  let uptimeTick: ReturnType<typeof setInterval> | null = null;
  let uptimeBase = 0, uptimeBaseAt = Date.now();
  $: { uptimeBase = metrics.uptime_secs ?? 0; uptimeBaseAt = Date.now(); }
  onMount(() => {
    uptimeTick = setInterval(() => {
      clientUptime = Math.round(uptimeBase + (Date.now() - uptimeBaseAt) / 1000);
    }, 1000);
  });
  onDestroy(() => { if (uptimeTick) clearInterval(uptimeTick); });

  function fmtSecs(s: number) {
    if (s < 60)   return `${s}s`;
    if (s < 3600) return `${Math.floor(s/60)}m ${s%60}s`;
    return fmtUptime(s);
  }

  // ── Metrics ────────────────────────────────────────────────────────────────
  $: ttft    = metrics.ttft_avg_ms > 0 ? `${Math.round(metrics.ttft_avg_ms)} ms` : '—';
  $: ttftSub = metrics.ttft_avg_ms > 0 ? 'avg first token' : 'no requests yet';
  $: reqs    = metrics.requests_total.toLocaleString();

  // ── Model VRAM (slot estimates only) ─────────────────────────────────────────
  $: modelVramGb  = slots.reduce((s, m) => s + (m.vram_est_gb ?? 0), 0);
  $: vramTotalGb  = hw?.vram_gb ?? 0;
  $: modelVramPct = vramTotalGb > 0 ? Math.min(100, (modelVramGb / vramTotalGb) * 100) : 0;
  $: vramBarColor = modelVramPct > 80 ? 'var(--danger)' : modelVramPct > 55 ? 'var(--warn)' : 'var(--accent)';

  // ── System memory (real, all-process) ────────────────────────────────────────
  $: memUsedGb  = sys?.mem_used_gb  ?? 0;
  $: memTotalGb = sys?.mem_total_gb ?? (hw?.total_ram_gb ?? 0);
  $: memPct     = sys?.mem_pct ?? 0;
  $: memAvailGb = sys?.mem_avail_gb ?? (memTotalGb - memUsedGb);
  $: memColor   = memPct > 85 ? 'var(--danger)' : memPct > 65 ? 'var(--warn)' : 'var(--accent)';

  // ── Model process memory (measured OS RSS of child model servers) ─────────────
  // This is the real physical RAM the AI models are holding, not a size estimate.
  // On Apple Silicon (unified memory) this covers both CPU + GPU portions of their footprint.
  $: modelProcs  = sys?.model_procs ?? [];
  $: modelRssGb  = sys?.model_rss_gb ?? 0;
  $: modelRssPct = memTotalGb > 0 ? Math.min(100, (modelRssGb / memTotalGb) * 100) : 0;
  $: hasModelProcs = modelProcs.length > 0;


  // ── CPU ───────────────────────────────────────────────────────────────────────
  $: cpuPct   = sys?.cpu_pct ?? null;
  $: cpuColor = cpuPct !== null
    ? (cpuPct > 85 ? 'var(--danger)' : cpuPct > 60 ? 'var(--warn)' : 'var(--accent)')
    : 'var(--text-3)';
  $: corePcts = (sys?.cpu_cores_pct ?? []).slice(0, 8);

  // ── GPU ───────────────────────────────────────────────────────────────────────
  $: gpu         = sys?.gpu;
  $: gpuUtil     = gpu?.util_pct ?? null;
  $: gpuMemUsed  = gpu?.mem_used_mb ?? null;
  $: gpuMemTotal = gpu?.mem_total_mb ?? null;
  $: gpuMemPct   = (gpuMemUsed !== null && gpuMemTotal !== null && gpuMemTotal > 0)
    ? (gpuMemUsed / gpuMemTotal * 100) : null;
  $: gpuColor    = gpuUtil !== null
    ? (gpuUtil > 85 ? 'var(--danger)' : gpuUtil > 60 ? 'var(--warn)' : 'var(--accent)')
    : 'var(--text-3)';
  $: gpuAvailable = gpu?.source !== 'unavailable';

  // ── Sparklines ────────────────────────────────────────────────────────────────
  const N = 30;
  let cpuHist: number[] = [], memHist: number[] = [], gpuHist: number[] = [];
  let sparkTick: ReturnType<typeof setInterval> | null = null;
  onMount(() => {
    sparkTick = setInterval(() => {
      if (cpuPct !== null) cpuHist = [...cpuHist.slice(-(N-1)), cpuPct];
      if (memPct > 0)      memHist = [...memHist.slice(-(N-1)), memPct];
      if (gpuUtil !== null) gpuHist = [...gpuHist.slice(-(N-1)), gpuUtil];
    }, 2000);
  });
  onDestroy(() => { if (sparkTick) clearInterval(sparkTick); });

  function sparkPath(hist: number[], w = 160, h = 26): string {
    if (hist.length < 2) return '';
    const step = w / (N - 1);
    const pad = Array<number>(N - hist.length).fill(0).concat(hist);
    return pad.map((p, i) => `${(i*step).toFixed(1)},${(h - (p/100)*(h-2) - 1).toFixed(1)}`).join(' ');
  }

  $: cpuSparkPts = sparkPath(cpuHist);
  $: memSparkPts = sparkPath(memHist);
  $: gpuSparkPts = sparkPath(gpuHist);

  // ── Role detection ────────────────────────────────────────────────────────────
  function inferRole(id: string) {
    const s = id.toLowerCase();
    if (s.includes('embed'))  return 'embed';
    if (s.includes('rerank')) return 'rerank';
    if (s.includes('vision') || s.includes('vl')) return 'vision';
    if (s.includes('code'))   return 'code';
    return 'chat';
  }
  const ROLE_CLS: Record<string, string> = {
    chat:'badge--green', embed:'badge--blue', rerank:'badge--amber', vision:'badge--blue', code:'badge--grey'
  };
</script>

<svelte:head><title>LMForge — Overview</title></svelte:head>

<div class="page">

  <div class="toolbar" data-tauri-drag-region onpointerdown={dragOnEmpty} role="toolbar">
    <h1>Overview</h1>
    <div class="tr">
      {#if $isOnline}
        <span class="badge badge--green">Ready</span>
        <span class="el mono">{engine} v{version}</span>
      {:else}
        <span class="badge badge--grey">Offline</span>
      {/if}
    </div>
  </div>

  <div class="body">

    <!-- ── Metrics strip ─────────────────────────────────────────────────────── -->
    <div class="metrics">
      <div class="metric metric--dim"><span class="mv mono">—</span><span class="ml">Avg TTFT</span><span class="ms">coming soon</span></div>
      <div class="metric metric--dim"><span class="mv mono">—</span><span class="ml">Uptime</span><span class="ms">coming soon</span></div>
      <div class="metric metric--dim"><span class="mv mono">—</span><span class="ml">Requests</span><span class="ms">coming soon</span></div>
      <div class="metric metric--dim"><span class="mv mono">—</span><span class="ml">Restarts</span><span class="ms">coming soon</span></div>
    </div>

    <!-- ── Main panels ────────────────────────────────────────────────────────── -->
    <div class="panels">

      <!-- ── Model Processes ───────────────────────────────────────────────── -->
      <section class="panel" aria-label="Model Processes">
        <header class="panel-hd">
          <div class="panel-hd-l">
            <h2>Model Processes</h2>
            {#if $isOnline}<span class="ps">{slots.length} active · {engine} v{version}</span>{/if}
          </div>
          {#if $isOnline && slots.length > 0}
            <div class="panel-hd-r">
              <span class="mini-bar"><span class="mini-bar-fill" style="width:{modelVramPct}%;background:{vramBarColor}"></span></span>
              <span class="ps mono">{modelVramGb.toFixed(2)} / {vramTotalGb.toFixed(0)} GB</span>
            </div>
          {/if}
        </header>

        {#if !$isOnline}
          <div class="empty"><span class="dot dot--stopped" style="width:8px;height:8px"></span><span>Daemon offline</span></div>
        {:else if slots.length === 0}
          <div class="empty-block">
            <div class="ei">🧩</div>
            <p class="et">No model processes running</p>
            <p class="ed">Models load on first request and stay warm in VRAM.<br>
              <a href="/models" onclick={(e)=>{e.preventDefault();goto('/models');}}>Open Library →</a>
            </p>
          </div>
        {:else}
          <div class="slot-cards">
            {#each slots as slot (slot.model_id)}
              {@const role  = slot.role || inferRole(slot.model_id)}
              {@const slotP = vramTotalGb > 0 ? Math.min(100, slot.vram_est_gb/vramTotalGb*100) : 0}
              {@const idle  = (slot.idle_secs ?? 0) > 30}
              <div class="slot-card" class:idle>
                <div class="slot-r1">
                  <span class="dot dot--{slot.status}" style="margin-top:1px"></span>
                  <span class="sn mono">{slot.model_id}</span>
                  <span class="badge {ROLE_CLS[role] ?? 'badge--grey'}" style="font-size:10px">{role}</span>
                </div>
                <div class="slot-r2">
                  <span class="sd"><span class="dk">port</span><span class="dv mono">{slot.port}</span></span>
                  <span class="sd"><span class="dk">vram</span><span class="dv mono">{slot.vram_est_gb.toFixed(2)} GB</span></span>
                  <div class="svt"><div class="svf" style="width:{slotP}%;background:{vramBarColor}"></div></div>
                  {#if idle}<span class="ib">idle {fmtSecs(slot.idle_secs??0)}</span>
                  {:else}<span class="ab">active</span>{/if}
                </div>
              </div>
            {/each}
          </div>
          <div class="cap-row">
            <span class="cap-lbl">VRAM committed</span>
            <div class="cap-track"><div class="cap-fill" style="width:{modelVramPct}%;background:{vramBarColor}"></div></div>
            <span class="cap-nums mono">{modelVramGb.toFixed(2)} / {vramTotalGb.toFixed(1)} GB · {(vramTotalGb-modelVramGb).toFixed(1)} GB free</span>
          </div>
        {/if}
      </section>

      <!-- ── Hardware / System panel (scrollable) ──────────────────────────── -->
      <section class="panel panel--hw" aria-label="Hardware">
        <header class="panel-hd" style="flex-shrink:0">
          <h2>Hardware</h2>
          {#if hw}<span class="ps">{hw.os} · {hw.arch}</span>{/if}
        </header>

        {#if !hw || !sys}
          <div class="sk-block">{#each [70,100,55,80,45,90,60] as w}<div class="skeleton" style="height:10px;width:{w}%;margin-bottom:9px"></div>{/each}</div>
        {:else}

          <!-- ── GPU Card ──────────────────────────────────────────────────── -->
          <div class="hw-card">
            <div class="hwc-head">
              <div class="hwc-title-row">
                <span class="hwc-icon">⬡</span>
                <span class="hwc-title">GPU</span>
                <span class="hwc-sub">{hw.gpu_vendor ? hw.gpu_vendor.toUpperCase() : '—'}{#if hw.unified_mem} · Unified{/if}</span>
              </div>
              {#if gpuUtil !== null}
                <span class="util-num" style="color:{gpuColor}">{gpuUtil.toFixed(0)}<span class="util-unit">%</span></span>
              {:else}
                <span class="na-tag">util N/A</span>
              {/if}
            </div>

            <!-- GPU util bar -->
            {#if gpuUtil !== null}
              <div class="bar-track"><div class="bar-fill" style="width:{gpuUtil}%;background:{gpuColor}"></div></div>
            {/if}

            <!-- GPU util sparkline -->
            {#if gpuHist.length >= 2}
              <div class="spark-wrap">
                <svg class="spark" viewBox="0 0 160 26" preserveAspectRatio="none">
                  <polyline points="{gpuSparkPts} 160,25 0,25" fill="{gpuColor}20" stroke="none"/>
                  <polyline points={gpuSparkPts} fill="none" stroke={gpuColor} stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
              </div>
            {/if}

            <!-- GPU Metal memory -->
            {#if gpuMemUsed !== null}
              <div class="hwc-row">
                <span class="hw-k">Metal mem</span>
                <span class="hw-v mono">{(gpuMemUsed/1024).toFixed(2)} GB{#if gpuMemTotal !== null} / {(gpuMemTotal/1024).toFixed(1)} GB{/if}</span>
              </div>
              {#if gpuMemPct !== null}
                <div class="bar-track bar-track--sm"><div class="bar-fill" style="width:{gpuMemPct}%;background:{gpuColor}"></div></div>
              {/if}
            {/if}

            {#if !gpuAvailable || (gpuUtil === null && gpuMemUsed === null)}
              <p class="hw-note">{gpu?.note ?? 'No GPU data available'}</p>
            {/if}
          </div>

          <!-- ── CPU Card ──────────────────────────────────────────────────── -->
          <div class="hw-card">
            <div class="hwc-head">
              <div class="hwc-title-row">
                <span class="hwc-icon">◈</span>
                <span class="hwc-title">CPU</span>
                <span class="hwc-sub">{hw.cpu_model} · {hw.cpu_cores}c</span>
              </div>
              {#if cpuPct !== null}
                <span class="util-num" style="color:{cpuColor}">{cpuPct.toFixed(0)}<span class="util-unit">%</span></span>
              {:else}
                <span class="na-tag">…</span>
              {/if}
            </div>

            <!-- CPU util bar -->
            {#if cpuPct !== null}
              <div class="bar-track"><div class="bar-fill" style="width:{cpuPct}%;background:{cpuColor}"></div></div>
            {/if}

            <!-- CPU sparkline -->
            {#if cpuSparkPts}
              <div class="spark-wrap">
                <svg class="spark" viewBox="0 0 160 26" preserveAspectRatio="none">
                  <polyline points="{cpuSparkPts} 160,25 0,25" fill="{cpuColor}20" stroke="none"/>
                  <polyline points={cpuSparkPts} fill="none" stroke={cpuColor} stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
              </div>
            {/if}

            <!-- Per-core bars -->
            {#if corePcts.length > 0}
              <div class="core-bars">
                {#each corePcts as pct, i}
                  <div class="core-col" title="Core {i}: {pct.toFixed(0)}%">
                    <div class="core-track"><div class="core-fill" style="height:{pct}%;background:{cpuColor}"></div></div>
                    <span class="core-lbl">{i}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>

          <!-- ── Memory Card ───────────────────────────────────────────────── -->
          <div class="hw-card">
            <div class="hwc-head">
              <div class="hwc-title-row">
                <span class="hwc-icon">▦</span>
                <span class="hwc-title">{hw.unified_mem ? 'Unified Memory' : 'RAM'}</span>
                <span class="hwc-sub">system-wide · all processes</span>
              </div>
              <span class="util-num" style="color:{memColor}">{memPct.toFixed(0)}<span class="util-unit">%</span></span>
            </div>

            <div class="bar-track"><div class="bar-fill" style="width:{memPct}%;background:{memColor}"></div></div>

            <!-- Memory sparkline -->
            {#if memSparkPts}
              <div class="spark-wrap">
                <svg class="spark" viewBox="0 0 160 26" preserveAspectRatio="none">
                  <polyline points="{memSparkPts} 160,25 0,25" fill="{memColor}20" stroke="none"/>
                  <polyline points={memSparkPts} fill="none" stroke={memColor} stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
              </div>
            {/if}

            <div class="mem-legend">
              <div class="hwc-row"><span class="hw-k">Used</span><span class="hw-v mono" style="color:{memColor}">{memUsedGb.toFixed(1)} GB</span></div>
              <div class="hwc-row"><span class="hw-k">Available</span><span class="hw-v mono">{memAvailGb.toFixed(1)} GB</span></div>
              <div class="hwc-row"><span class="hw-k">Total</span><span class="hw-v mono">{memTotalGb.toFixed(0)} GB</span></div>
            </div>

            <!-- Model process RSS breakdown (measured, not estimated) -->
            {#if hasModelProcs}
              <div class="model-mem-section">
                <div class="model-mem-hd">
                  <span class="hw-k">AI model processes</span>
                  <span class="hw-v mono" style="color:var(--accent-2)">{modelRssGb.toFixed(2)} GB</span>
                </div>

                <!-- Stacked bar: model RSS vs rest of used memory -->
                <div class="stacked-bar">
                  <div class="stacked-seg stacked-model" style="width:{modelRssPct}%" title="AI models: {modelRssGb.toFixed(2)} GB"></div>
                  <div class="stacked-seg stacked-other" style="width:{Math.max(0, memPct - modelRssPct)}%" title="Other processes"></div>
                </div>

                <!-- Per-model rows -->
                {#each modelProcs as proc}
                  <div class="model-proc-row">
                    <span class="proc-dot"></span>
                    <span class="proc-name mono" title={proc.model_id}>{proc.model_id.split('/').pop()}</span>
                    <span class="proc-rss mono">{(proc.rss_mb / 1024).toFixed(2)} GB</span>
                  </div>
                {/each}

                <p class="hw-note" style="margin-top:4px">
                  Measured RSS · actual physical {hw?.unified_mem ? 'unified' : ''} memory held by model servers
                </p>
              </div>
            {:else if slots.length > 0}
              <!-- Fallback to size estimate if proc scan hasn't matched yet -->
              <div class="hwc-row hwc-row--sub">
                <span class="hw-k">↳ models est. ({hw?.unified_mem ? 'unified' : 'VRAM'})</span>
                <span class="hw-v mono">{modelVramGb.toFixed(2)} GB <span style="opacity:0.5;font-size:9.5px">(size estimate)</span></span>
              </div>
            {/if}
          </div>

        {/if}
      </section>

    </div>
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

  /* Toolbar */
  .toolbar {
    height: var(--toolbar-h); display: flex; align-items: center; justify-content: space-between;
    padding: 0 20px; border-bottom: 1px solid var(--border); flex-shrink: 0;
  }
  .toolbar h1 { font-size: 14px; font-weight: 600; color: var(--text); }
  .tr  { display: flex; align-items: center; gap: 8px; }
  .el  { font-size: 11px; color: var(--text-3); }

  .body { flex: 1; overflow-y: auto; padding: 16px 18px; display: flex; flex-direction: column; gap: 14px; }

  /* ── Metrics ─────────────────────────────────────────────────────────────── */
  .metrics { display: grid; grid-template-columns: repeat(4,1fr); gap: 10px; flex-shrink: 0; }
  .metric {
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg);
    padding: 11px 14px; display: flex; flex-direction: column; gap: 2px; transition: border-color 130ms;
  }
  .metric:hover { border-color: var(--border-2); }
  .metric.warn .mv { color: var(--warn); }
  .mv { font-size: 20px; font-weight: 600; color: var(--text); letter-spacing: -0.6px; line-height: 1.1; }
  .ml { font-size: 10px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.5px; margin-top: 2px; }
  .ms { font-size: 9.5px; color: var(--text-3); opacity: 0.7; }
  .metric--dim { opacity: 0.4; pointer-events: none; }
  .metric--dim .mv { color: var(--text-3); }

  /* ── Layout panels ───────────────────────────────────────────────────────── */
  .panels { display: flex; gap: 12px; flex: 1; min-height: 0; overflow: hidden; }
  .panel {
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-xl);
    padding: 16px 18px; flex: 2; display: flex; flex-direction: column; gap: 10px;
    overflow-y: auto; transition: border-color 150ms;
  }
  .panel:hover { border-color: var(--border-2); }

  /* Hardware panel: scrollable column of hw-cards */
  .panel--hw { flex: 1.1; padding: 14px 16px; gap: 10px; }

  .panel-hd { display: flex; justify-content: space-between; align-items: flex-start; }
  .panel-hd-l { display: flex; flex-direction: column; gap: 2px; }
  .panel-hd-r { display: flex; align-items: center; gap: 7px; flex-shrink: 0; }
  .panel-hd h2 { font-size: 12.5px; font-weight: 600; }
  .ps { font-size: 10.5px; color: var(--text-3); }
  .mini-bar { width: 44px; height: 4px; background: rgba(255,255,255,0.07); border-radius: 99px; overflow: hidden; display: inline-block; position: relative; }
  .mini-bar-fill { position: absolute; inset: 0; border-radius: 99px; transition: width 600ms; }

  /* ── Empty states ────────────────────────────────────────────────────────── */
  .empty { display: flex; align-items: center; gap: 7px; color: var(--text-3); font-size: 12px; font-style: italic; }
  .empty-block { display: flex; flex-direction: column; gap: 6px; padding: 4px 0; }
  .ei { font-size: 26px; }
  .et { font-size: 13px; font-weight: 600; color: var(--text); }
  .ed { font-size: 12px; color: var(--text-2); line-height: 1.6; }
  .ed a { color: var(--accent-2); text-decoration: none; cursor: pointer; }
  .ed a:hover { text-decoration: underline; }

  /* ── Slot cards ──────────────────────────────────────────────────────────── */
  .slot-cards { display: flex; flex-direction: column; gap: 8px; }
  .slot-card {
    background: rgba(255,255,255,0.025); border: 1px solid var(--border); border-radius: var(--radius-lg);
    padding: 9px 12px; display: flex; flex-direction: column; gap: 7px;
    transition: border-color 140ms; animation: fade-in 220ms ease;
  }
  .slot-card:hover { border-color: var(--border-2); }
  .slot-card.idle { opacity: 0.68; }
  .slot-r1 { display: flex; align-items: center; gap: 7px; }
  .sn  { flex: 1; font-size: 11.5px; font-weight: 500; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .slot-r2 { display: flex; align-items: center; gap: 9px; }
  .sd  { display: flex; gap: 3px; align-items: baseline; flex-shrink: 0; }
  .dk  { font-size: 9.5px; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.4px; }
  .dv  { font-size: 11px; color: var(--text-2); }
  .svt { flex: 1; height: 3px; background: rgba(255,255,255,0.07); border-radius: 99px; overflow: hidden; }
  .svf { height: 100%; border-radius: 99px; transition: width 400ms; }
  .ib  { font-size: 9.5px; color: var(--text-3); padding: 1px 5px; background: rgba(255,255,255,0.05); border-radius: 3px; flex-shrink: 0; }
  .ab  { font-size: 9.5px; color: var(--success); padding: 1px 5px; background: var(--success-dim); border-radius: 3px; flex-shrink: 0; }
  .cap-row { margin-top: auto; display: flex; align-items: center; gap: 10px; padding-top: 10px; border-top: 1px solid var(--divider); flex-shrink: 0; }
  .cap-lbl   { font-size: 10.5px; color: var(--text-3); flex-shrink: 0; }
  .cap-track { flex: 1; height: 4px; background: rgba(255,255,255,0.07); border-radius: 99px; overflow: hidden; }
  .cap-fill  { height: 100%; border-radius: 99px; transition: width 600ms; }
  .cap-nums  { font-size: 10.5px; color: var(--text-2); flex-shrink: 0; }

  /* ── Hardware resource cards ─────────────────────────────────────────────── */
  .hw-card {
    background: rgba(255,255,255,0.025);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 11px 13px;
    display: flex; flex-direction: column; gap: 7px;
    transition: border-color 140ms;
  }
  .hw-card:hover { border-color: var(--border-2); }

  .hwc-head { display: flex; justify-content: space-between; align-items: center; }
  .hwc-title-row { display: flex; align-items: center; gap: 6px; flex: 1; min-width: 0; }
  .hwc-icon  { font-size: 13px; opacity: 0.6; flex-shrink: 0; }
  .hwc-title { font-size: 12px; font-weight: 600; color: var(--text); flex-shrink: 0; }
  .hwc-sub   { font-size: 10px; color: var(--text-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .util-num  { font-size: 22px; font-weight: 700; letter-spacing: -1px; flex-shrink: 0; font-family: var(--font-mono); }
  .util-unit { font-size: 12px; font-weight: 400; letter-spacing: 0; }
  .na-tag    { font-size: 10px; color: var(--text-3); background: rgba(255,255,255,0.06); padding: 2px 6px; border-radius: 4px; flex-shrink: 0; }

  /* Generic utilisation bar */
  .bar-track    { height: 4px; background: rgba(255,255,255,0.07); border-radius: 99px; overflow: hidden; }
  .bar-track--sm { height: 3px; margin-top: 2px; }
  .bar-fill     { height: 100%; border-radius: 99px; transition: width 600ms cubic-bezier(0.4,0,0.2,1); }

  /* Sparklines */
  .spark-wrap { width: 100%; }
  .spark      { width: 100%; height: 26px; overflow: visible; display: block; }

  /* Per-core bars */
  .core-bars { display: flex; gap: 4px; align-items: flex-end; margin-top: 3px; }
  .core-col  { display: flex; flex-direction: column; align-items: center; gap: 2px; flex: 1; }
  .core-track { width: 100%; height: 22px; background: rgba(255,255,255,0.06); border-radius: 3px; overflow: hidden; display: flex; align-items: flex-end; }
  .core-fill  { width: 100%; border-radius: 2px; transition: height 400ms; min-height: 1px; }
  .core-lbl   { font-size: 8px; color: var(--text-3); font-family: var(--font-mono); }

  /* Memory legend rows */
  .hwc-row { display: flex; justify-content: space-between; align-items: center; }
  .hwc-row--sub { opacity: 0.7; padding-top: 3px; border-top: 1px solid var(--divider); margin-top: 2px; }
  .mem-legend { display: flex; flex-direction: column; gap: 3px; }
  .hw-k { font-size: 10.5px; color: var(--text-3); }
  .hw-v { font-size: 11.5px; color: var(--text); font-weight: 500; }
  .hw-note { font-size: 9.5px; color: var(--text-3); opacity: 0.8; font-style: italic; line-height: 1.4; margin-top: 2px; }

  /* Skeleton */
  .sk-block { display: flex; flex-direction: column; }

  /* ── Model process memory breakdown ─────────────────────────────────────── */
  .model-mem-section {
    display: flex; flex-direction: column; gap: 5px;
    padding-top: 8px; border-top: 1px solid var(--divider); margin-top: 2px;
  }
  .model-mem-hd { display: flex; justify-content: space-between; align-items: center; }

  /* Stacked bar showing model vs other memory usage */
  .stacked-bar {
    height: 5px; border-radius: 99px; background: rgba(255,255,255,0.07);
    overflow: hidden; display: flex; gap: 1px;
  }
  .stacked-seg { height: 100%; border-radius: 2px; transition: width 600ms cubic-bezier(0.4,0,0.2,1); }
  .stacked-model { background: var(--accent-2, #6ee7b7); }
  .stacked-other { background: rgba(255,255,255,0.18); }

  /* Per-model rows */
  .model-proc-row {
    display: flex; align-items: center; gap: 6px;
    padding: 1px 0;
  }
  .proc-dot {
    width: 5px; height: 5px; border-radius: 50%;
    background: var(--accent-2, #6ee7b7); flex-shrink: 0; opacity: 0.8;
  }
  .proc-name {
    flex: 1; font-size: 10.5px; color: var(--text-2);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .proc-rss { font-size: 11px; color: var(--text); font-weight: 500; flex-shrink: 0; }

  @media (max-width: 820px) {

    .panels { flex-direction: column; overflow-y: auto; }
    .panel--hw { flex: none; min-height: 200px; }
    .metrics { grid-template-columns: repeat(2,1fr); }
  }
</style>
