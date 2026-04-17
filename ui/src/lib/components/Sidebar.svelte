<!--
  Shared sidebar — used in both / and /models via the root +layout.svelte.
  Contains: traffic-light zone, grouped navigation, daemon status footer.
  All pages share this exact sidebar; pages render only their content area.
-->
<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { statusStore, isOnline } from '$lib/stores/status';
  import { dragAlways } from '$lib/drag';

  $: status = $statusStore.overall_status;
  $: engine = $statusStore.engine_id;

  $: dotClass = ({
    ready: 'dot dot--ready', degraded: 'dot dot--degraded',
    error: 'dot dot--error', starting: 'dot dot--starting',
    stopped: 'dot dot--stopped',
  } as Record<string,string>)[status] ?? 'dot dot--stopped';

  $: statusLabel = ({
    ready: 'Ready', degraded: 'Degraded', error: 'Error',
    starting: 'Starting…', stopped: 'Offline',
  } as Record<string,string>)[status] ?? 'Offline';

  const NAV: { section: string; items: { label: string; href: string; icon: string; color: string }[] }[] = [
    {
      section: 'Engine',
      items: [
        {
          label: 'Overview', href: '/',
          icon: 'M13 10V3L4 14h7v7l9-11h-7z',
          color: 'hsl(38, 92%, 58%)',
        },
      ],
    },
    {
      section: 'Models',
      items: [
        {
          label: 'Library', href: '/models',
          icon: 'M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4',
          color: 'hsl(211, 90%, 62%)',
        },
      ],
    },
  ];

  $: currentPath = $page.url.pathname;
</script>

<nav class="sidebar" aria-label="Main navigation">

  <!-- Traffic-light zone: macOS Overlay mode paints 🔴🟡🟢 here -->
  <div class="tl-zone" data-tauri-drag-region onpointerdown={dragAlways} aria-hidden="true"></div>

  <!-- Navigation -->
  <div class="nav-body">
    {#each NAV as group}
      <div class="nav-group">
        <span class="nav-section-label">{group.section}</span>
        {#each group.items as item}
          {@const active = currentPath === item.href}
          <a
            href={item.href}
            class="nav-item"
            class:active
            aria-current={active ? 'page' : undefined}
            onclick={(e) => { e.preventDefault(); goto(item.href); }}
          >
            <span class="nav-icon-wrap" style="--item-color:{item.color}" class:active>
              <svg class="nav-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.75" d={item.icon}/>
              </svg>
            </span>
            <span class="nav-label">{item.label}</span>
          </a>
        {/each}
      </div>
    {/each}
  </div>

  <!-- Footer -->
  <div class="sidebar-footer">
    <span class={dotClass}></span>
    <span class="footer-status">{statusLabel}</span>
    {#if $isOnline}
      <span class="footer-engine mono">{engine}</span>
    {/if}
  </div>
</nav>

<style>
  .sidebar {
    width: var(--sidebar-w);
    flex-shrink: 0;
    background: var(--sidebar-bg);
    border-right: 1px solid var(--border);
    display: flex; flex-direction: column;
    overflow: hidden;
  }

  .tl-zone {
    height: var(--tl-zone-h);
    flex-shrink: 0;
    cursor: default;
  }

  .nav-body {
    flex: 1; overflow-y: auto;
    padding: 0 6px;
    display: flex; flex-direction: column; gap: 20px;
  }

  .nav-group { display: flex; flex-direction: column; gap: 1px; }

  .nav-section-label {
    font-size: 10px; font-weight: 600; color: var(--text-3);
    text-transform: uppercase; letter-spacing: 0.75px;
    padding: 0 8px; margin-bottom: 3px;
  }

  .nav-item {
    display: flex; align-items: center; gap: 8px;
    padding: 6px 8px; border-radius: var(--radius-sm);
    color: var(--text-2); text-decoration: none;
    font-size: 13px; font-weight: 400;
    transition: background 100ms ease, color 100ms ease;
    cursor: default;
  }
  .nav-item:hover:not(.active) { background: rgba(255,255,255,0.05); color: var(--text); }
  .nav-item.active { background: var(--accent-dim); color: var(--accent-2); font-weight: 500; }

  .nav-icon-wrap {
    display: flex; align-items: center; justify-content: center;
    width: 20px; height: 20px; border-radius: var(--radius-xs);
    background: rgba(255,255,255,0.05);
    color: var(--item-color, var(--text-3));
    transition: background 100ms ease, color 100ms ease;
    flex-shrink: 0;
  }
  .nav-item:hover:not(.active) .nav-icon-wrap { background: rgba(255,255,255,0.08); }
  .nav-icon-wrap.active { background: var(--accent-dim); color: var(--accent-2); }

  .nav-icon { width: 13px; height: 13px; }
  .nav-label { flex: 1; }

  .sidebar-footer {
    display: flex; align-items: center; gap: 6px;
    padding: 10px 14px; border-top: 1px solid var(--divider); flex-shrink: 0;
  }
  .footer-status { font-size: 12px; color: var(--text-2); font-weight: 500; }
  .footer-engine { font-size: 10px; color: var(--text-3); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
