<!--
  Playground — a lightweight inference client for ad-hoc testing of chat and
  VLM models. Talks directly to the OpenAI-compatible /v1/chat/completions
  endpoint (streaming). Stateless by design: no history persistence, no stores.
-->
<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { isOnline } from '$lib/stores/status';
  import { dragOnEmpty } from '$lib/drag';
  import {
    listModels,
    streamChat,
    fileToDataUrl,
    type ModelEntry,
    type ChatMsg,
    type ChatTextPart,
    type ChatImagePart,
  } from '$lib/api';

  interface UiMsg {
    role: 'user' | 'assistant';
    text: string;
    reasoning?: string;  // streamed thinking (assistant turns only)
    images?: string[];   // data: or http(s) URLs (user turns only)
    error?: boolean;
    notice?: string;     // non-error hint (e.g. budget exhausted, no answer)
    streaming?: boolean;
  }

  let models: ModelEntry[] = [];
  let selected = '';
  let modelsError = '';

  let msgs: UiMsg[] = [];
  let input = '';
  let pendingImages: string[] = [];
  let maxTokens = 512;     // max ANSWER tokens (final response cap)
  let thinkBudget = 2048;  // max REASONING tokens before the model must answer
  let thinking = false;
  let busy = false;
  let abort: AbortController | null = null;

  // ── Sampling ────────────────────────────────────────────────────────────
  // Two profiles. The thinking profile is the loop-breaker: at low temp with
  // default top_p and no penalty, Qwen3-class models degenerate into a
  // "Wait. Wait. …" loop that burns the whole reasoning budget. Values mirror
  // docintel's LLM_THINKING_* defaults. Snapped on the think toggle; every
  // value stays user-overridable via the Advanced row.
  const CHAT_PROFILE = { temperature: 0.7, topP: 0.95, topK: 20, repPen: 1.1, presPen: 0.0 };
  const THINK_PROFILE = { temperature: 0.6, topP: 0.95, topK: 20, repPen: 1.2, presPen: 0.3 };

  // `max_tokens` on the wire is the TOTAL budget (reasoning + answer). When
  // thinking we send `thinkBudget` (reasoning cap) + `maxTokens` (answer cap) so
  // an exhausted reasoning budget never starves the answer (e.g. 512−2048 → 64
  // tokens of answer). Both are user-tunable knobs. Weak models (qwen3.5:2b) can
  // still loop within the budget; use ≥4B for multi-step reasoning.

  let advancedOpen = false;
  let temperature = CHAT_PROFILE.temperature;
  let topP = CHAT_PROFILE.topP;
  let topK = CHAT_PROFILE.topK;
  let repPen = CHAT_PROFILE.repPen;
  let presPen = CHAT_PROFILE.presPen;

  function applyProfile(think: boolean) {
    const p = think ? THINK_PROFILE : CHAT_PROFILE;
    temperature = p.temperature;
    topP = p.topP;
    topK = p.topK;
    repPen = p.repPen;
    presPen = p.presPen;
  }

  function toggleThinking() {
    thinking = !thinking;
    applyProfile(thinking);
  }

  let scrollEl: HTMLDivElement | null = null;
  let fileInput: HTMLInputElement | null = null;
  let dragOver = false;
  let urlOpen = false;
  let urlInput = '';

  const isChatCapable = (m: ModelEntry) =>
    (m.capabilities.chat || m.capabilities.vision) &&
    !m.capabilities.reranking &&
    !m.capabilities.embeddings;

  $: selModel = models.find((m) => m.id === selected);
  $: visionOn = !!selModel?.capabilities.vision;
  $: thinkingSupported = !!selModel?.capabilities.thinking;
  // Native-reasoning models (phi4:reasoning, R1 distills) always think — the
  // toggle is shown locked-on and no think flag / budget goes on the wire (the
  // daemon splits their inline <think> output on the passthrough path).
  $: nativeReasoning = !!selModel?.capabilities.native_reasoning;
  $: chatModels = models.filter(isChatCapable);
  $: canSend = !busy && !!selected && (input.trim().length > 0 || pendingImages.length > 0);

  onMount(loadModels);
  onDestroy(() => abort?.abort());

  async function loadModels() {
    try {
      const res = await listModels();
      models = res.models ?? [];
      modelsError = '';
      if (!selected) {
        const firstChat = models.find(isChatCapable);
        if (firstChat) selected = firstChat.id;
      }
    } catch (e) {
      modelsError = String(e);
    }
  }

  async function scrollToBottom() {
    await tick();
    if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
  }

  // Keep the (independently scrollable) reasoning panel pinned to the latest
  // token as it streams. `max-height` gives .rtext its own scrollbar, so the
  // outer transcript scroll alone no longer follows the thinking text.
  function autoscroll(node: HTMLElement, _dep: unknown) {
    const pin = () => { node.scrollTop = node.scrollHeight; };
    pin();
    return { update: pin };
  }

  /** Map the visible transcript to OpenAI wire messages (multimodal for VLM). */
  function toWireMessages(): ChatMsg[] {
    const out: ChatMsg[] = [];
    for (const m of msgs) {
      if (m.streaming) continue;
      if (m.role === 'user' && m.images && m.images.length > 0) {
        const parts: (ChatTextPart | ChatImagePart)[] = [];
        if (m.text.trim()) parts.push({ type: 'text', text: m.text });
        for (const url of m.images) parts.push({ type: 'image_url', image_url: { url } });
        out.push({ role: 'user', content: parts });
      } else if (m.text.trim()) {
        out.push({ role: m.role, content: m.text });
      }
    }
    return out;
  }

  async function send() {
    if (!canSend) return;
    const userMsg: UiMsg = {
      role: 'user',
      text: input.trim(),
      images: pendingImages.length ? pendingImages.slice() : undefined,
    };
    msgs = [...msgs, userMsg];
    input = '';
    pendingImages = [];

    const wire = toWireMessages();
    const ai = msgs.length;
    msgs = [...msgs, { role: 'assistant', text: '', streaming: true }];
    await scrollToBottom();

    // Only send the think flag for models that support it. Native-reasoning
    // models never get the flag: reasoning is hardwired, not toggleable.
    const useThink = thinkingSupported && !nativeReasoning ? thinking : undefined;

    busy = true;
    abort = new AbortController();
    try {
      const result = await streamChat(
        selected,
        wire,
        {
          temperature,
          max_tokens: useThink ? thinkBudget + maxTokens : maxTokens,
          think: useThink,
          thinking_budget: useThink ? thinkBudget : undefined,
          top_p: topP,
          top_k: topK,
          presence_penalty: presPen,
          repetition_penalty: repPen,
          signal: abort.signal,
        },
        (delta, kind) => {
          if (kind === 'reasoning') {
            msgs[ai] = { ...msgs[ai], reasoning: (msgs[ai].reasoning ?? '') + delta };
          } else {
            msgs[ai] = { ...msgs[ai], text: msgs[ai].text + delta };
          }
          msgs = msgs;
          scrollToBottom();
        },
      );
      // Blank answer: the model produced no content. Almost always a thinking
      // model that burned its whole budget while reasoning (finish=length)
      // before closing </think>, so the user gets a silent empty bubble.
      // Surface a clear, actionable hint instead.
      let notice: string | undefined;
      if (!msgs[ai].text.trim()) {
        if (useThink && result.reasoningChars > 0) {
          notice = result.finishReason === 'length'
            ? 'The model used its entire thinking budget before answering. Try raising the budget, lowering it so it commits sooner, or use a larger model (≥4B) for multi-step reasoning.'
            : 'The model finished while still thinking and produced no answer. Try again, adjust the thinking budget, or use a larger model (≥4B).';
        } else if (result.finishReason === 'length') {
          notice = 'Output was cut off at the token limit before any answer. Raise “max”.';
        } else {
          notice = 'The model returned an empty response. Try again or rephrase.';
        }
      }
      msgs[ai] = { ...msgs[ai], streaming: false, notice };
      msgs = msgs;
    } catch (e) {
      const aborted = abort?.signal.aborted;
      const cur = msgs[ai];
      if (aborted && cur.text) {
        msgs[ai] = { ...cur, streaming: false };
      } else {
        msgs[ai] = {
          ...cur,
          streaming: false,
          error: true,
          text: cur.text ? `${cur.text}\n\n[stream error: ${e}]` : `Request failed — ${e}`,
        };
      }
      msgs = msgs;
    } finally {
      busy = false;
      abort = null;
      scrollToBottom();
    }
  }

  function stop() {
    abort?.abort();
  }

  function clearChat() {
    if (busy) stop();
    msgs = [];
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  async function addFiles(files: FileList | File[]) {
    const imgs = Array.from(files).filter((f) => f.type.startsWith('image/'));
    for (const f of imgs.slice(0, 4 - pendingImages.length)) {
      try {
        pendingImages = [...pendingImages, await fileToDataUrl(f)];
      } catch {
        /* skip unreadable file */
      }
    }
  }

  function onFilePick(e: Event) {
    const t = e.target as HTMLInputElement;
    if (t.files) addFiles(t.files);
    t.value = '';
  }

  function addUrl() {
    const u = urlInput.trim();
    if (!/^https?:\/\//i.test(u)) return;
    if (pendingImages.length < 4) pendingImages = [...pendingImages, u];
    urlInput = '';
    urlOpen = false;
  }

  function onUrlKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); addUrl(); }
    else if (e.key === 'Escape') { urlOpen = false; urlInput = ''; }
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragOver = false;
    if (!visionOn) return;
    if (e.dataTransfer?.files) addFiles(e.dataTransfer.files);
  }

  function removeImage(i: number) {
    pendingImages = pendingImages.filter((_, idx) => idx !== i);
  }
</script>

<svelte:head><title>LMForge — Playground</title></svelte:head>

<div class="page">
  <div class="toolbar" data-tauri-drag-region onpointerdown={dragOnEmpty} role="toolbar">
    <h1>Playground</h1>
    <div class="tr">
      <select class="model-sel" bind:value={selected} disabled={busy} aria-label="Model">
        {#if chatModels.length === 0}
          <option value="">No chat models</option>
        {/if}
        {#each chatModels as m}
          <option value={m.id}>{m.id}</option>
        {/each}
      </select>
      {#if visionOn}
        <span class="badge badge--blue" title="Supports image input">vision</span>
      {/if}
      {#if thinkingSupported}
        <button
          class="toggle"
          class:on={thinking || nativeReasoning}
          class:locked={nativeReasoning}
          onclick={toggleThinking}
          disabled={busy || nativeReasoning}
          title={nativeReasoning
            ? 'This model always reasons — thinking is built into its template and cannot be toggled off'
            : 'Toggle the model\'s reasoning/thinking mode (snaps sampling to the thinking profile)'}
          aria-pressed={thinking || nativeReasoning}
        >
          <span class="knob"></span>
          think
        </button>
      {/if}
      <label class="ctl" title="Sampling temperature">
        temp
        <input type="number" min="0" max="2" step="0.1" bind:value={temperature} disabled={busy} />
      </label>
      {#if thinkingSupported && !nativeReasoning}
        <label
          class="ctl"
          title="Thinking budget — max reasoning tokens the model may spend before it must answer. Applies only when think is on; the answer still gets the full 'max' on top."
        >
          budget
          <input type="number" min="128" max="8192" step="128" bind:value={thinkBudget} disabled={busy || !thinking} />
        </label>
      {/if}
      <label class="ctl" title="Max answer tokens — caps the final response length (added on top of the thinking budget when think is on)">
        max
        <input type="number" min="1" max="8192" step="1" bind:value={maxTokens} disabled={busy} />
      </label>
      <button
        class="btn btn--ghost btn--sm"
        class:active={advancedOpen}
        onclick={() => (advancedOpen = !advancedOpen)}
        title="Advanced sampling parameters"
        aria-pressed={advancedOpen}
      >sampling</button>
      <button class="btn btn--ghost btn--sm" onclick={clearChat} disabled={msgs.length === 0}>Clear</button>
    </div>
  </div>

  <div class="body">
    {#if modelsError}
      <div class="error-strip">{modelsError}</div>
    {/if}

    {#if advancedOpen}
      <div class="sampling-bar">
        <label class="ctl" title="Nucleus sampling — cumulative probability cutoff">
          top_p
          <input type="number" min="0" max="1" step="0.01" bind:value={topP} disabled={busy} />
        </label>
        <label class="ctl" title="Top-k sampling — 0 disables">
          top_k
          <input type="number" min="0" max="200" step="1" bind:value={topK} disabled={busy} />
        </label>
        <label class="ctl" title="Repetition penalty — &gt;1 discourages repeats (loop breaker)">
          rep_pen
          <input type="number" min="1" max="2" step="0.05" bind:value={repPen} disabled={busy} />
        </label>
        <label class="ctl" title="Presence penalty — discourages repeating tokens">
          pres_pen
          <input type="number" min="0" max="2" step="0.1" bind:value={presPen} disabled={busy} />
        </label>
        <span class="profile-hint">
          {thinking ? 'thinking profile' : 'chat profile'} · snaps on think toggle
        </span>
        <button
          class="btn btn--ghost btn--sm reset"
          onclick={() => applyProfile(thinking)}
          disabled={busy}
          title="Reset to the {thinking ? 'thinking' : 'chat'} profile defaults"
        >reset</button>
      </div>
    {/if}

    <div
      class="transcript"
      bind:this={scrollEl}
      class:drag={dragOver}
      role="log"
      ondragover={(e) => { if (visionOn) { e.preventDefault(); dragOver = true; } }}
      ondragleave={() => (dragOver = false)}
      ondrop={onDrop}
    >
      {#if msgs.length === 0}
        <div class="empty">
          {#if chatModels.length === 0}
            <p>No chat-capable model installed.</p>
            <p class="dim">Pull one from the Library, then come back here.</p>
          {:else}
            <p>Send a message to {selected || 'the model'}.</p>
            {#if visionOn}<p class="dim">This is a vision model — drop or attach an image to test VLM.</p>{/if}
          {/if}
        </div>
      {/if}

      {#each msgs as m}
        <div class="row {m.role}">
          <div class="bubble" class:error={m.error}>
            {#if m.images && m.images.length}
              <div class="imgs">
                {#each m.images as src}
                  <img class="thumb" {src} alt="attachment" />
                {/each}
              </div>
            {/if}
            {#if m.reasoning}
              <details class="reasoning" open={m.streaming}>
                <summary>
                  <svg class="ricon" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                       stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M12 3a4 4 0 0 0-4 4 3 3 0 0 0-1 5.83V17a3 3 0 0 0 6 0"/>
                    <path d="M12 3a4 4 0 0 1 4 4 3 3 0 0 1 1 5.83V17a3 3 0 0 1-6 0"/>
                  </svg>
                  <span class="rlabel">Thinking</span>
                  {#if m.streaming && !m.text}<span class="rpulse">reasoning…</span>{/if}
                  <svg class="rchev" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                       stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="m6 9 6 6 6-6"/>
                  </svg>
                </summary>
                <div class="rtext" use:autoscroll={m.reasoning}>{m.reasoning}</div>
              </details>
            {/if}
            {#if m.text}
              <div class="text">{m.text}</div>
            {/if}
            {#if m.notice}
              <div class="notice">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
                     stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <circle cx="12" cy="12" r="9"/><path d="M12 8v5"/><path d="M12 16h.01"/>
                </svg>
                <span>{m.notice}</span>
              </div>
            {/if}
            {#if m.streaming && !m.text}
              <div class="typing"><span></span><span></span><span></span></div>
            {/if}
          </div>
        </div>
      {/each}
    </div>

    <!-- ── Composer ─────────────────────────────────────────────────────── -->
    <div class="composer">
      {#if pendingImages.length}
        <div class="pending">
          {#each pendingImages as src, i}
            <div class="pthumb-wrap">
              <img class="pthumb" {src} alt="pending attachment" />
              <button class="rm" onclick={() => removeImage(i)} aria-label="Remove image">×</button>
            </div>
          {/each}
        </div>
      {/if}
      {#if urlOpen}
        <div class="url-row">
          <input
            class="url-input"
            type="url"
            bind:value={urlInput}
            onkeydown={onUrlKeydown}
            placeholder="https://example.com/image.jpg"
            aria-label="Image URL"
          />
          <button class="btn btn--ghost btn--sm" onclick={addUrl} disabled={!/^https?:\/\//i.test(urlInput.trim())}>Add</button>
          <button class="btn btn--ghost btn--sm" onclick={() => { urlOpen = false; urlInput = ''; }}>Cancel</button>
        </div>
      {/if}
      <div class="input-row">
        {#if visionOn}
          <button
            class="attach"
            title="Attach image file"
            aria-label="Attach image file"
            onclick={() => fileInput?.click()}
            disabled={busy || pendingImages.length >= 4}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"
                 stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="m18.375 12.739-7.693 7.693a4.5 4.5 0 0 1-6.364-6.364l10.94-10.94A3 3 0 1 1 19.5 7.372L8.552 18.32m.009-.01-.01.01m5.699-9.941-7.81 7.81a1.5 1.5 0 0 0 2.112 2.13"/>
            </svg>
          </button>
          <button
            class="attach"
            title="Attach image by URL"
            aria-label="Attach image by URL"
            onclick={() => (urlOpen = !urlOpen)}
            disabled={busy || pendingImages.length >= 4}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"
                 stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M13.19 8.688a4.5 4.5 0 0 1 1.242 7.244l-4.5 4.5a4.5 4.5 0 0 1-6.364-6.364l1.757-1.757m13.35-.622 1.757-1.757a4.5 4.5 0 0 0-6.364-6.364l-4.5 4.5a4.5 4.5 0 0 0 1.242 7.244"/>
            </svg>
          </button>
          <input
            bind:this={fileInput}
            type="file"
            accept="image/*"
            multiple
            hidden
            onchange={onFilePick}
          />
        {/if}
        <textarea
          class="input"
          bind:value={input}
          onkeydown={onKeydown}
          placeholder={visionOn ? 'Message (Enter to send · drop an image to test VLM)' : 'Message (Enter to send · Shift+Enter for newline)'}
          rows="1"
          disabled={!selected}
        ></textarea>
        {#if busy}
          <button class="btn btn--danger send" onclick={stop}>Stop</button>
        {:else}
          <button class="btn btn--primary send" onclick={send} disabled={!canSend}>Send</button>
        {/if}
      </div>
      {#if !$isOnline}
        <p class="offhint dim">Engine offline — start it to run inference.</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; overflow: hidden; }

  .toolbar {
    height: var(--toolbar-h); flex-shrink: 0;
    display: flex; align-items: center; gap: 12px;
    padding: 0 16px; border-bottom: 1px solid var(--border);
  }
  .toolbar h1 { flex-shrink: 0; }
  .tr { margin-left: auto; display: flex; align-items: center; gap: 10px; }

  .model-sel {
    background: var(--surface-2); color: var(--text);
    color-scheme: dark;
    border: 1px solid var(--border-2); border-radius: var(--radius-sm);
    font-size: 12px; padding: 4px 8px; max-width: 280px;
    font-family: var(--font-mono);
  }
  .model-sel option { background: var(--surface-2); color: var(--text); }
  .model-sel:disabled { opacity: 0.6; }

  .ctl {
    display: inline-flex; align-items: center; gap: 5px;
    font-size: 11px; color: var(--text-2);
  }
  .ctl input {
    width: 52px; background: var(--surface-2); color: var(--text);
    border: 1px solid var(--border-2); border-radius: var(--radius-xs);
    font-size: 11px; padding: 3px 5px; font-family: var(--font-mono);
  }

  /* Thinking toggle */
  .toggle {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 3px 9px 3px 5px; border-radius: 99px;
    background: var(--surface-2); border: 1px solid var(--border-2);
    color: var(--text-2); font-size: 11px; cursor: pointer;
    transition: color 110ms, background 110ms, border-color 110ms;
  }
  .toggle .knob {
    width: 18px; height: 12px; border-radius: 99px; flex-shrink: 0;
    background: var(--text-3); position: relative;
    transition: background 130ms;
  }
  .toggle .knob::after {
    content: ''; position: absolute; top: 1px; left: 1px;
    width: 10px; height: 10px; border-radius: 50%; background: #fff;
    transition: transform 130ms;
  }
  .toggle:hover:not(:disabled):not(.on) { color: var(--text); border-color: var(--border); }
  .toggle.on {
    color: var(--accent-2); border-color: var(--accent); background: var(--accent-dim);
    box-shadow: 0 0 0 1px hsla(252, 87%, 67%, 0.25), 0 0 10px hsla(252, 87%, 67%, 0.28);
  }
  .toggle.on .knob { background: var(--accent); }
  .toggle.on .knob::after { transform: translateX(6px); }
  .toggle:disabled { opacity: 0.5; cursor: not-allowed; }
  /* Native-reasoning models: toggle locked on — full on-state visuals, just not clickable. */
  .toggle.locked:disabled { opacity: 1; cursor: default; }

  .body { flex: 1; display: flex; flex-direction: column; overflow: hidden; }

  /* Advanced sampling row */
  .btn.active { color: var(--accent-2); border-color: var(--accent); background: var(--accent-dim); }
  .sampling-bar {
    flex-shrink: 0; display: flex; align-items: center; flex-wrap: wrap; gap: 14px;
    padding: 8px 16px; border-bottom: 1px solid var(--border);
    background: var(--surface-1, var(--surface-2));
  }
  .sampling-bar .ctl input { width: 58px; }
  .profile-hint {
    font-size: 11px; color: var(--accent-2); opacity: 0.85;
    font-family: var(--font-mono);
  }
  .sampling-bar .reset { margin-left: auto; }

  .error-strip {
    margin: 10px 16px 0; padding: 8px 12px;
    background: var(--danger-dim); color: var(--danger);
    border: 1px solid hsla(3,78%,60%,.25); border-radius: var(--radius-sm);
    font-size: 12px;
  }

  .transcript {
    flex: 1; overflow-y: auto; padding: 18px 16px;
    display: flex; flex-direction: column; gap: 12px;
  }
  .transcript.drag { outline: 2px dashed var(--accent); outline-offset: -8px; }

  .empty {
    margin: auto; text-align: center; color: var(--text-2);
    display: flex; flex-direction: column; gap: 4px;
  }
  .empty p { font-size: 13px; }

  .row { display: flex; }
  .row.user { justify-content: flex-end; }
  .row.assistant { justify-content: flex-start; }

  .bubble {
    max-width: 76%; padding: 9px 13px; border-radius: var(--radius-lg);
    font-size: 13px; line-height: 1.55;
    animation: fade-in 120ms ease;
  }
  .row.user .bubble { background: var(--accent); color: #fff; border-bottom-right-radius: var(--radius-xs); }
  .row.assistant .bubble {
    background: var(--surface-2); color: var(--text);
    border: 1px solid hsla(252, 87%, 67%, 0.4);
    border-bottom-left-radius: var(--radius-xs);
    box-shadow: 0 0 0 1px hsla(252, 87%, 67%, 0.06), 0 1px 8px hsla(252, 87%, 67%, 0.07);
  }
  .bubble.error { background: var(--danger-dim); color: var(--danger); border-color: hsla(3,78%,60%,.25); box-shadow: none; }

  .text { white-space: pre-wrap; word-break: break-word; }

  /* Non-error hint (e.g. thinking budget exhausted with no answer) */
  .notice {
    display: flex; align-items: flex-start; gap: 7px;
    margin-top: 2px; padding: 8px 10px; border-radius: var(--radius-sm);
    background: hsla(38, 92%, 50%, 0.10);
    border: 1px solid hsla(38, 92%, 50%, 0.30);
    color: var(--text-2); font-size: 12px; line-height: 1.45;
  }
  .notice svg { width: 15px; height: 15px; flex: none; margin-top: 1px; color: hsl(38, 92%, 52%); }

  /* Thinking / reasoning panel */
  .reasoning {
    margin-bottom: 9px; border-radius: var(--radius-sm);
    background: var(--accent-dim);
    border: 1px solid hsla(252, 87%, 67%, 0.22);
    overflow: hidden;
  }
  .reasoning summary {
    display: flex; align-items: center; gap: 7px;
    cursor: pointer; padding: 6px 9px; user-select: none;
    font-size: 11px; font-weight: 600; letter-spacing: 0.4px;
    color: var(--accent-2);
    list-style: none;
  }
  .reasoning summary::-webkit-details-marker { display: none; }
  .reasoning summary:hover { background: hsla(252, 87%, 67%, 0.08); }
  .ricon { width: 13px; height: 13px; flex-shrink: 0; opacity: 0.9; }
  .rlabel { text-transform: uppercase; }
  .rchev {
    width: 13px; height: 13px; margin-left: auto; opacity: 0.7;
    transition: transform 160ms ease;
  }
  .reasoning[open] .rchev { transform: rotate(180deg); }
  .rpulse {
    font-size: 10px; font-weight: 500; letter-spacing: 0.3px;
    text-transform: none; color: var(--accent-2);
    opacity: 0.85; animation: rpulse 1.3s ease-in-out infinite;
  }
  @keyframes rpulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 0.95; } }
  .rtext {
    margin: 0; padding: 8px 10px 10px;
    border-top: 1px solid hsla(252, 87%, 67%, 0.16);
    white-space: pre-wrap; word-break: break-word;
    font-size: 11.5px; color: var(--text-2); font-family: var(--font-mono);
    line-height: 1.55;
    max-height: 320px; overflow-y: auto;
  }

  .imgs { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 6px; }
  .thumb { max-width: 160px; max-height: 160px; border-radius: var(--radius-sm); display: block; }

  .typing { display: inline-flex; gap: 4px; padding: 3px 0; }
  .typing span {
    width: 6px; height: 6px; border-radius: 50%; background: var(--text-3);
    animation: blink 1.2s infinite ease-in-out;
  }
  .typing span:nth-child(2) { animation-delay: 0.2s; }
  .typing span:nth-child(3) { animation-delay: 0.4s; }
  @keyframes blink { 0%, 80%, 100% { opacity: 0.25; } 40% { opacity: 1; } }

  .composer {
    flex-shrink: 0; border-top: 1px solid var(--border);
    padding: 10px 16px 14px; background: var(--content-bg);
  }
  .pending { display: flex; gap: 8px; margin-bottom: 8px; flex-wrap: wrap; }
  .pthumb-wrap { position: relative; }
  .pthumb { width: 56px; height: 56px; object-fit: cover; border-radius: var(--radius-sm); border: 1px solid var(--border-2); display: block; }
  .rm {
    position: absolute; top: -6px; right: -6px;
    width: 18px; height: 18px; border-radius: 50%;
    background: var(--danger); color: #fff; border: none; cursor: pointer;
    font-size: 12px; line-height: 1; display: flex; align-items: center; justify-content: center;
  }

  .url-row { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .url-input {
    flex: 1; background: var(--surface-2); color: var(--text);
    border: 1px solid var(--border-2); border-radius: var(--radius-sm);
    padding: 6px 10px; font-size: 12px; font-family: var(--font-mono);
  }
  .url-input:focus { outline: none; border-color: var(--accent); }

  .input-row { display: flex; align-items: flex-end; gap: 8px; }

  .attach {
    flex-shrink: 0; width: 34px; height: 34px;
    background: var(--surface-2); border: 1px solid var(--border-2);
    border-radius: var(--radius-sm); color: var(--text-2); cursor: pointer;
    display: flex; align-items: center; justify-content: center;
    transition: color 110ms, background 110ms;
  }
  .attach:hover:not(:disabled) { color: var(--text); background: var(--surface-3); }
  .attach:disabled { opacity: 0.4; cursor: not-allowed; }
  .attach svg { width: 17px; height: 17px; }

  .input {
    flex: 1; resize: none; max-height: 160px; min-height: 34px;
    background: var(--surface-2); color: var(--text);
    border: 1px solid var(--border-2); border-radius: var(--radius-sm);
    padding: 8px 11px; font-size: 13px; font-family: var(--font-sans);
    line-height: 1.45;
  }
  .input:focus { outline: none; border-color: var(--accent); }
  .input:disabled { opacity: 0.5; }

  .send { flex-shrink: 0; height: 34px; padding: 0 18px; }

  .offhint { margin: 8px 0 0; font-size: 11px; }
</style>
