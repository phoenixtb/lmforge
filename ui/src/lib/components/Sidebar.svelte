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
  $: currentPath = $page.url.pathname;

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
    <button
      class="settings-btn"
      class:active={currentPath === '/settings'}
      onclick={() => goto('/settings')}
      title="Settings"
      aria-label="Open settings"
    >
      <!-- Gear icon (heroicons cog-6-tooth) -->
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"
           stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z"/>
        <path d="M15 12a3 3 0 11-6 0 3 3 0 016 0z"/>
      </svg>
    </button>
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
  .footer-engine {
    font-size: 10px; color: var(--text-3);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    flex: 1; /* push settings btn to the far right */
  }

  /* Settings gear button */
  .settings-btn {
    display: flex; align-items: center; justify-content: center;
    width: 24px; height: 24px;
    background: none; border: none; border-radius: var(--radius-xs);
    color: var(--text-3); cursor: pointer; flex-shrink: 0;
    transition: color 110ms ease, background 110ms ease;
    margin-left: auto;
    padding: 3px;
  }
  .settings-btn svg { width: 16px; height: 16px; }
  .settings-btn:hover { color: var(--text-2); background: rgba(255,255,255,0.06); }
  .settings-btn.active { color: var(--accent-2); background: var(--accent-dim); }
</style>
