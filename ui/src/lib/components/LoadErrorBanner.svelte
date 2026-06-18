<script lang="ts">
  /**
   * Engine Load Errors banner — dismissable, severity-aware surface that sits
   * above the Model Processes panel on Overview.
   *
   * Behaviour (see ADR-003):
   *   - `user_error` / `engine_bug`: red card, stays until the user resolves
   *     (model loads → daemon clears it) or dismisses it. `user_error` gets an
   *     "Open Library" action since the fix is usually "pull the model".
   *   - `transient`: amber card that auto-demotes to a compact pill after 30 s
   *     so a recoverable hiccup stops dominating the layout but isn't lost.
   *   - Dismissal is occurrence-keyed (`model_id@at`) and survives the 2 s
   *     status re-push; a NEW failure (new `at`) re-appears on its own.
   */
  import { goto } from '$app/navigation';
  import { onDestroy } from 'svelte';
  import { dismissLoadError, type ModelLoadError } from '$lib/api';
  import {
    dismissedErrors,
    dismissError,
    errorKey,
    pruneDismissed,
  } from '$lib/stores/dismissedErrors';

  let { errors = [] }: { errors: [string, ModelLoadError][] } = $props();

  const DEMOTE_MS = 30_000;
  type Severity = 'user_error' | 'transient' | 'engine_bug';

  let expandedTail = $state(new Set<string>());
  let demoted = $state(new Set<string>());
  // Non-reactive: auto-demote timers keyed by occurrence.
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  function severityOf(e: ModelLoadError): Severity {
    if (e.severity) return e.severity;
    // Fallback for daemons that predate the `severity` field.
    const m = e.message.toLowerCase();
    if (/pull the model|no \.gguf|no safetensors|not found in model directory|model directory|unknown model|no such model/.test(m))
      return 'user_error';
    if (/health|timed out|timeout|port|address already in use|out of memory|oom|connection refused/.test(m))
      return 'transient';
    return 'engine_bug';
  }

  function fmtRelative(iso: string): string {
    try {
      const d = new Date(iso);
      const delta = Math.round((Date.now() - d.getTime()) / 1000);
      if (delta < 60) return `${delta}s ago`;
      if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
      if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
      return d.toLocaleString();
    } catch {
      return iso;
    }
  }

  let live = $derived(
    errors
      .map(([modelId, err]) => ({
        modelId,
        err,
        key: errorKey(modelId, err.at),
        severity: severityOf(err),
      }))
      .filter((e) => !$dismissedErrors.has(e.key)),
  );

  let activeCards = $derived(live.filter((e) => !demoted.has(e.key)));
  let mutedPills = $derived(live.filter((e) => demoted.has(e.key)));

  // Reconcile auto-demote timers against the live set + keep stores bounded.
  $effect(() => {
    const liveKeys = new Set(live.map((e) => e.key));

    for (const e of live) {
      if (e.severity === 'transient' && !demoted.has(e.key) && !timers.has(e.key)) {
        const k = e.key;
        timers.set(
          k,
          setTimeout(() => {
            timers.delete(k);
            demoted = new Set(demoted).add(k);
          }, DEMOTE_MS),
        );
      }
    }
    for (const k of [...timers.keys()]) {
      if (!liveKeys.has(k)) {
        clearTimeout(timers.get(k));
        timers.delete(k);
      }
    }
    pruneDismissed(new Set(errors.map(([id, er]) => errorKey(id, er.at))));
  });

  onDestroy(() => {
    for (const t of timers.values()) clearTimeout(t);
    timers.clear();
  });

  function toggleTail(key: string) {
    const next = new Set(expandedTail);
    next.has(key) ? next.delete(key) : next.add(key);
    expandedTail = next;
  }
  function mute(key: string) {
    const t = timers.get(key);
    if (t) {
      clearTimeout(t);
      timers.delete(key);
    }
    demoted = new Set(demoted).add(key);
  }
  function unmute(key: string) {
    const next = new Set(demoted);
    next.delete(key);
    demoted = next;
  }
  // Dismiss = clear on the daemon (suppressed until next successful load) so the
  // re-attempted failure stops coming back. Hide locally too as an anti-flash
  // until the next /lf/status push drops the entry.
  function dismiss(modelId: string, key: string) {
    dismissError(key);
    dismissLoadError(modelId).catch(() => {});
  }
</script>

{#if live.length > 0}
  <section class="leb" aria-label="Engine Load Errors">
    {#each activeCards as e (e.key)}
      <div class="leb-card leb-card--{e.severity}" role="alert">
        <div class="leb-r1">
          <span class="leb-dot"></span>
          <span class="leb-id mono">{e.modelId}</span>
          <span class="leb-sev">{e.severity === 'transient' ? 'transient' : e.severity === 'user_error' ? 'action needed' : 'error'}</span>
          {#if e.err.count && e.err.count > 1}
            <span class="leb-count" title="{e.err.count} occurrences">{e.err.count}×</span>
          {/if}
          <span class="leb-when" title={e.err.at}>last seen {fmtRelative(e.err.at)}</span>
          <div class="leb-actions">
            <button class="leb-x" title="Mute" aria-label="Mute this error" onclick={() => mute(e.key)}>–</button>
            <button class="leb-x" title="Dismiss" aria-label="Dismiss this error" onclick={() => dismiss(e.modelId, e.key)}>✕</button>
          </div>
        </div>

        <div class="leb-msg">{e.err.message}</div>

        <div class="leb-r2">
          {#if e.severity === 'user_error'}
            <button class="leb-btn" onclick={() => goto('/models')}>Open Library →</button>
          {/if}
          {#if e.err.stderr_tail && e.err.stderr_tail.length > 0}
            <button
              class="leb-toggle"
              onclick={() => toggleTail(e.key)}
              aria-expanded={expandedTail.has(e.key)}
            >
              {expandedTail.has(e.key) ? '▾' : '▸'} stderr tail ({e.err.stderr_tail.length.toLocaleString()} bytes)
            </button>
          {/if}
        </div>

        {#if expandedTail.has(e.key) && e.err.stderr_tail}
          <pre class="leb-tail">{e.err.stderr_tail}</pre>
        {/if}
      </div>
    {/each}

    {#if mutedPills.length > 0}
      <div class="leb-muted" aria-label="Muted load errors">
        <span class="leb-muted-lbl">Muted:</span>
        {#each mutedPills as e (e.key)}
          <button class="leb-pill leb-pill--{e.severity}" title="Show {e.modelId}" onclick={() => unmute(e.key)}>
            <span class="leb-pill-dot"></span>
            <span class="mono">{e.modelId}</span>
          </button>
        {/each}
      </div>
    {/if}
  </section>
{/if}

<style>
  .leb {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex-shrink: 0;
  }

  .leb-card {
    border: 1px solid var(--border);
    border-left-width: 3px;
    border-radius: var(--radius-lg, 12px);
    background: var(--surface);
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .leb-card--user_error,
  .leb-card--engine_bug {
    border-color: rgba(248, 113, 113, 0.35);
    border-left-color: var(--danger, #f87171);
    background: rgba(248, 113, 113, 0.05);
  }
  .leb-card--transient {
    border-color: rgba(251, 191, 36, 0.35);
    border-left-color: var(--warn, #fbbf24);
    background: rgba(251, 191, 36, 0.05);
  }

  .leb-r1 { display: flex; align-items: center; gap: 8px; }
  .leb-dot { width: 7px; height: 7px; border-radius: 50%; flex-shrink: 0; }
  .leb-card--user_error .leb-dot,
  .leb-card--engine_bug .leb-dot { background: var(--danger, #f87171); }
  .leb-card--transient .leb-dot { background: var(--warn, #fbbf24); }

  .leb-id { font-size: 12px; color: var(--text); font-weight: 600; }
  .leb-sev {
    font-size: 9.5px; text-transform: uppercase; letter-spacing: 0.04em;
    color: var(--text-3); border: 1px solid var(--border); border-radius: 4px;
    padding: 1px 5px;
  }
  .leb-count {
    font-size: 9.5px; font-weight: 600; letter-spacing: 0.02em;
    color: var(--text-2); background: rgba(255, 255, 255, 0.07);
    border-radius: 4px; padding: 1px 5px;
    font-family: var(--font-mono, monospace);
  }
  .leb-when { font-size: 10.5px; color: var(--text-3); margin-left: auto; }
  .leb-actions { display: flex; gap: 2px; flex-shrink: 0; }
  .leb-x {
    background: none; border: none; cursor: pointer;
    color: var(--text-3); font-size: 13px; line-height: 1;
    width: 22px; height: 22px; border-radius: 5px;
  }
  .leb-x:hover { color: var(--text); background: rgba(255, 255, 255, 0.07); }

  .leb-msg {
    font-size: 12px; line-height: 1.5;
    color: var(--text-2);
    font-family: var(--font-mono, monospace);
    white-space: pre-wrap; word-break: break-word;
  }

  .leb-r2 { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  .leb-btn {
    background: rgba(248, 113, 113, 0.12);
    border: 1px solid rgba(248, 113, 113, 0.4);
    color: #fcaaaa;
    border-radius: var(--radius-sm, 6px);
    font-size: 11px; padding: 3px 9px; cursor: pointer;
  }
  .leb-btn:hover { background: rgba(248, 113, 113, 0.2); }
  .leb-toggle {
    background: none; border: none; cursor: pointer;
    font-size: 11px; color: var(--text-3); padding: 0;
    font-family: var(--font-mono, monospace);
  }
  .leb-toggle:hover { color: var(--text); }
  .leb-tail {
    font-family: var(--font-mono, monospace); font-size: 10.5px; line-height: 1.45;
    color: var(--text-2);
    background: rgba(0, 0, 0, 0.4);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 6px);
    padding: 8px 10px; margin: 0;
    max-height: 220px; overflow: auto;
    white-space: pre; user-select: text;
  }

  .leb-muted { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 2px 0; }
  .leb-muted-lbl { font-size: 10.5px; color: var(--text-3); }
  .leb-pill {
    display: inline-flex; align-items: center; gap: 5px;
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 99px; padding: 2px 9px; cursor: pointer;
    font-size: 10.5px; color: var(--text-2);
  }
  .leb-pill:hover { border-color: var(--border-2, var(--border)); color: var(--text); }
  .leb-pill-dot { width: 6px; height: 6px; border-radius: 50%; flex-shrink: 0; }
  .leb-pill--transient .leb-pill-dot { background: var(--warn, #fbbf24); }
  .leb-pill--user_error .leb-pill-dot,
  .leb-pill--engine_bug .leb-pill-dot { background: var(--danger, #f87171); }

  .mono { font-family: var(--font-mono, monospace); }
</style>
