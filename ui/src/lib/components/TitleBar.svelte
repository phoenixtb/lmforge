<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { platform } from '@tauri-apps/plugin-os';

  const win = getCurrentWindow();
  let pl = $state('');

  onMount(async () => {
    try { pl = await platform(); } catch { pl = 'unknown'; }
  });

  /**
   * macOS Accessory mode has no Dock icon → minimize() is a no-op.
   * Use hide() instead — the tray icon lets the user show it again.
   * On Windows/Linux, minimize normally.
   */
  function minimize() {
    if (pl === 'macos') {
      win.hide();
    } else {
      win.minimize();
    }
  }
  function toggleMax() { win.toggleMaximize(); }
  /**
   * Close = hide to tray (same as our on_window_event handler).
   * Calling win.hide() directly is cleaner than relying on CloseRequested interception.
   */
  function closeWin() { win.hide(); }
</script>

<!--
  Custom frameless titlebar — cross-platform.
  data-tauri-drag-region enables dragging on all platforms.
  Window controls are rendered by us (not the OS), so they look identical
  on macOS, Linux, and Windows.
-->
<div class="titlebar" data-tauri-drag-region>
  <!-- Left: icon + name -->
  <div class="titlebar-left" data-tauri-drag-region>
    <div class="titlebar-icon" aria-hidden="true">
      <span class="icon-dot icon-dot--green"></span>
    </div>
    <span class="titlebar-name">LMForge</span>
  </div>

  <!-- Right: window controls — class:win adjusts order/sizing for Windows -->
  <div class="titlebar-controls" class:win={pl === 'windows'}>
    <button class="wbtn wbtn--min" onclick={minimize} aria-label="Minimize" title="Minimize">
      &#xFF0D;
    </button>
    <button class="wbtn wbtn--max" onclick={toggleMax} aria-label="Maximise" title="Maximise">
      &#x25A1;
    </button>
    <button class="wbtn wbtn--close" onclick={closeWin} aria-label="Close" title="Close">
      &#xFF38;
    </button>
  </div>
</div>

<style>
  .titlebar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--titlebar-h);
    padding: 0 4px 0 12px;
    background: var(--sidebar);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    z-index: 100;
  }

  .titlebar-left {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .titlebar-icon {
    display: flex;
    align-items: center;
    gap: 3px;
  }

  /* Single decorative green dot — cross-platform, not OS traffic lights */
  .icon-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .icon-dot--green { background: var(--success); }

  .titlebar-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    letter-spacing: 0.2px;
  }

  /* Window control buttons */
  .titlebar-controls {
    display: flex;
    gap: 0;
    -webkit-app-region: no-drag;
    app-region: no-drag;
  }

  .wbtn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 38px;
    height: var(--titlebar-h);
    background: transparent;
    border: none;
    color: var(--text-3);
    font-size: 14px;
    cursor: pointer;
    transition: background 100ms ease, color 100ms ease;
    border-radius: 0;
  }
  .wbtn:hover { background: var(--surface-2); color: var(--text); }
  .wbtn--close:hover { background: var(--danger); color: #fff; }

  /* Windows: close button is wider, order is min/max/close (same here but widths differ) */
  .win .wbtn { width: 46px; }
</style>
