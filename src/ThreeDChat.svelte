<script lang="ts">
  // src/ThreeDChat.svelte
  // "3D Thoughts" — conversational 3D control via MiniMax-M3.
  //
  // The M3 model streams its internal reasoning as `reasoning_content` (in
  // `<think>...</think>` tags by default) which we surface in a dedicated
  // "Thoughts" panel so the user can see WHY the model is producing each
  // set of scene ops. This is the namesake "3D Thoughts" feature.
  //
  // Flow per user turn:
  //   1. Snapshot the current scene (ids + names + positions + colors).
  //   2. Prepend the snapshot to the user message so the model has
  //      context — M3 cannot query the scene otherwise (it would guess).
  //   3. Invoke `minimax_chat_stream` with tools_preset="three_d" and the
  //      3D-specific system prompt.
  //   4. Stream `reasoning_content` to the Thoughts panel; stream
  //      `content` to the assistant message bubble.
  //   5. When the model calls `three_d_apply_ops` we apply the ops to the
  //      local scene store (defensive normalizer on the backend handles
  //      M3's stringified values).
  //   6. `ai_done` resets `busy` and folds the final text into the
  //      assistant message.
  //
  // If the user has no MiniMax key, we fall back to a tiny local stub
  // so the chat UI remains interactive in the demo (and explains the gap
  // in the banner).

  import { onMount, onDestroy, tick } from 'svelte';
  import { listen, minimaxChatStream, type ChatMessage } from './lib/tauri';
  import { getSceneStore, flattenScene, type SceneNode, type SceneOp } from './lib/three_d_store';
  import { apiKeyStatus } from './lib/keyStore';

  export let onToast: (text: string, kind: 'info' | 'error') => void = () => {};

  const store = getSceneStore();

  type UiMessage = { role: 'user' | 'assistant' | 'system'; content: string; ts: number };
  let messages: UiMessage[] = [];
  let input = '';
  let busy = false;
  let lastToolUse: { name: string; args: any } | null = null;
  let lastToolResult: { name: string; ok: boolean; error?: string; dataUrl?: string; prompt?: string } | null = null;
  let streamingText = '';
  /** Per-turn thought stream. Reset on each send. */
  let thoughts: string[] = [];      // list of completed thought blocks
  let currentThought = '';         // active streaming thought
  let thoughtsExpanded = true;
  let aiUnlisten: () => void = () => {};
  let toolUseUnlisten: () => void = () => {};
  let toolResultUnlisten: () => void = () => {};
  let doneUnlisten: () => void = () => {};
  let errorUnlisten: () => void = () => {};
  let opsUnlisten: () => void = () => {};
  let safetyTimer: ReturnType<typeof setTimeout> | null = null;

  const SYSTEM_PROMPT = `You are Luna 3D, an assistant embedded in Luna Agent's 3D editor. You build and edit 3D scenes by calling tools from the three_d_* set.

Coordinate units: meters. Y is up. Default camera: position (3, 2, 5), target (0, 0, 0). Origin (0, 0, 0) is the centre of the ground.

Always batch related ops in a single three_d_apply_ops call so the user sees the whole change at once. After the call, briefly confirm in 1 short sentence. Do NOT explain the JSON.

# Material presets (use the exact colors + PBR values)
- wood:    color #8b5a2b, metalness 0.0, roughness 0.65
- darkwood:color #5c3a1e, metalness 0.0, roughness 0.75
- metal:   color #9aa4ad, metalness 0.85, roughness 0.35
- gold:    color #d4a73b, metalness 0.95, roughness 0.20
- glass:   color #c0e3ff, metalness 0.0, roughness 0.05
- fabric:  color #cfb59b, metalness 0.0, roughness 0.95
- plastic: color #e8e8e8, metalness 0.0, roughness 0.55
- emissive:#22ffaa, metalness 0.0, roughness 0.50 (use for glowing accents)
- leaf:    color #2d5a2d, metalness 0.0, roughness 0.75
- skin:    color #e7b89a, metalness 0.0, roughness 0.70
- stone:   color #8a8a8a, metalness 0.0, roughness 0.90
- brick:   color #a14a2a, metalness 0.0, roughness 0.85
- water:   color #3a6ea5, metalness 0.0, roughness 0.10
If the user says "make a wooden table", reach for the "wood" preset. Be deliberate: presets are the difference between "looks like a render" and "looks like a sketch".

# Lighting
The viewport's default hemisphere + directional lights are on, but they're not enough for most scenes. Add lights explicitly when the scene is dark, moody, or has translucent materials. Use three_d_apply_ops with kind: "set_light":
- directional (sun): set position [5, 8, 5], intensity 0.8, color "#ffffff"
- hemisphere (sky/ground): intensity 0.6, color "#b1e1ff" sky / "#b97a20" ground — note: set_light for hemisphere only takes a single color; treat it as the sky color
- ambient: low intensity (0.2) for soft fill
- point: position somewhere meaningful (lamp at [0, 1.5, 0], candle at [0, 0.3, 0])

# Composition rules
- Centre-of-mass design: keep the focal object within ±2m of origin so the camera frames it well.
- Layered depth: foreground (1m) / midground (origin) / background (-3m and beyond) — vary Z so the scene doesn't look like a lineup.
- Stable silhouettes: heavy base + light top. A table needs legs, a robot needs feet.
- Reasonable scale: a person is ~1.7m tall, a door ~2m, a book ~0.2m. Don't make everything 1m cubes.
- Add a ground plane (primitive "plane" with color "#3a3f4a", scale [10, 10, 1]) if the scene needs to sit on something.
- Avoid floating objects unless the user asked for it.

# Examples (user → tool_call):

User: "place a red box at origin"
→ three_d_apply_ops({ops: [{kind: "add_primitive", id: "box_red", parent: null, primitive: "box", position: [0, 0.5, 0], rotation: [0, 0, 0], scale: [1, 1, 1], color: "#e05555", metalness: 0.0, roughness: 0.55, name: "Red Box"}]})

User: "make a wooden table"
→ three_d_apply_ops({ops: [
  {kind: "add_primitive", id: "tabletop", parent: null, primitive: "box", position: [0, 0.75, 0], scale: [2, 0.1, 1], color: "#8b5a2b", metalness: 0.0, roughness: 0.65, name: "Tabletop"},
  {kind: "add_primitive", id: "leg1", parent: null, primitive: "cylinder", position: [-0.9, 0.35, 0.4], scale: [0.1, 0.7, 0.1], color: "#8b5a2b", metalness: 0.0, roughness: 0.65, name: "Leg 1"},
  {kind: "add_primitive", id: "leg2", parent: null, primitive: "cylinder", position: [0.9, 0.35, 0.4], scale: [0.1, 0.7, 0.1], color: "#8b5a2b", metalness: 0.0, roughness: 0.65, name: "Leg 2"},
  {kind: "add_primitive", id: "leg3", parent: null, primitive: "cylinder", position: [-0.9, 0.35, -0.4], scale: [0.1, 0.7, 0.1], color: "#8b5a2b", metalness: 0.0, roughness: 0.65, name: "Leg 3"},
  {kind: "add_primitive", id: "leg4", parent: null, primitive: "cylinder", position: [0.9, 0.35, -0.4], scale: [0.1, 0.7, 0.1], color: "#8b5a2b", metalness: 0.0, roughness: 0.65, name: "Leg 4"}
]})

User: "remove the red box"
→ three_d_apply_ops({ops: [{kind: "remove_node", id: "box_red"}]})

User: "move it up by 1"
→ three_d_apply_ops({ops: [{kind: "update_node", id: "box_red", patch: {field: "transform", value: {position: [0, 1.5, 0], rotation: [0, 0, 0], scale: [1, 1, 1]}}}]})

If the user asks for a real human face or a copyrighted character, refuse briefly and suggest an alternative.`;

  function appendMsg(m: UiMessage) { messages = [...messages, m]; }

  function openSettings() {
    window.dispatchEvent(new CustomEvent('luna:switch-tab', { detail: 'settings' }));
  }

  function clearSafetyTimer() {
    if (safetyTimer) { clearTimeout(safetyTimer); safetyTimer = null; }
  }

  function startSafetyTimer() {
    clearSafetyTimer();
    safetyTimer = setTimeout(() => {
      if (busy) {
        busy = false;
        store.setLoading({ kind: null, label: '' });
        streamingText = '';
        onToast('AI timed out (60s) — check MiniMax key and connection.', 'error');
        appendMsg({ role: 'system', content: '(AI timed out after 60s. Press Send to retry.)', ts: Date.now() });
        tick();
      }
    }, 60_000);
  }

  /** Build a compact one-line-per-node summary of the current scene so the
   *  model has the context it needs. Truncated to ~1KB to keep tokens in
   *  check. */
  function describeScene(): string {
    const flat = flattenScene($store.scene);
    if (flat.length === 0) return 'The scene is currently empty.';
    const lines: string[] = ['Current scene (id | name | kind | pos | color):'];
    for (const n of flat.slice(0, 30)) {
      if (n.kind === 'mesh') {
        const p = n.transform.position.map((x) => x.toFixed(2)).join(',');
        const c = n.material.color;
        lines.push(`- ${n.id} | ${n.name} | ${n.primitive} | [${p}] | ${c}`);
      } else {
        lines.push(`- ${n.id} | ${n.name} | group | — | —`);
      }
    }
    if (flat.length > 30) lines.push(`… and ${flat.length - 30} more nodes`);
    return lines.join('\n');
  }

  onMount(async () => {
    appendMsg({
      role: 'system',
      content: '3D Thoughts. Ask the AI to build or modify a scene — e.g. "make a wooden table by the window" or "add a forest around the red box". Without a MiniMax key the chat uses a tiny local stub.',
      ts: Date.now(),
    });
    try {
      aiUnlisten = await listen<string>('ai_chunk', (ev) => {
        streamingText += ev.payload;
      });
      // M3 streams its reasoning in `reasoning_content` with <think>
      // tags. We surface it in a dedicated "Thoughts" panel above the
      // assistant message so the user can see how the model reasons.
      const thinkUnlisten = await listen<string>('ai_thinking', (ev) => {
        currentThought += ev.payload;
      });
      aiUnlisten = (() => { const a = aiUnlisten; return () => { a(); thinkUnlisten(); }; })();

      toolUseUnlisten = await listen<{ name: string; args: any; id?: string }>('ai_tool_use', (ev) => {
        lastToolUse = { name: ev.payload.name, args: ev.payload.args };
        if (ev.payload?.name?.startsWith('three_d_')) {
          store.setLoading({ kind: 'ai', label: ev.payload.name });
        }
      });
      toolResultUnlisten = await listen<{ name: string; ok: boolean; error?: string; data_url?: string; dataUrl?: string; prompt?: string }>('ai_tool_result', (ev) => {
        lastToolResult = {
          name: ev.payload.name,
          ok: ev.payload.ok,
          error: ev.payload.error,
          dataUrl: (ev.payload as any).dataUrl ?? (ev.payload as any).data_url,
          prompt: ev.payload.prompt,
        };
        store.setLoading({ kind: null, label: '' });
        if (ev.payload.name === 'three_d_generate_texture' && ev.payload.ok) {
          const sel = (store as any).selectedId ?? null;
          if (sel && (ev.payload as any).dataUrl) {
            const dataUrl = (ev.payload as any).dataUrl as string;
            const prompt = (ev.payload.prompt as string) ?? 'generated texture';
            store.pushOp({ kind: 'apply_texture', id: sel, prompt, dataUrl });
          }
        }
      });
      opsUnlisten = await listen<SceneOp[]>('three_d_ops', (ev) => {
        const r = store.applyOps(ev.payload);
        if (!r.ok) onToast(`AI op failed: ${r.error}`, 'error');
      });
      doneUnlisten = await listen<true>('ai_done', () => {
        clearSafetyTimer();
        if (currentThought.trim()) {
          thoughts = [...thoughts, currentThought.trim()];
          currentThought = '';
        }
        if (streamingText.trim()) {
          appendMsg({ role: 'assistant', content: streamingText.trim(), ts: Date.now() });
        } else if (lastToolResult && lastToolResult.ok) {
          appendMsg({ role: 'assistant', content: `Done (${lastToolResult.name}).`, ts: Date.now() });
        } else if (lastToolResult && !lastToolResult.ok) {
          appendMsg({ role: 'assistant', content: `Tool failed: ${lastToolResult.error}`, ts: Date.now() });
        }
        streamingText = '';
        busy = false;
        store.setLoading({ kind: null, label: '' });
        tick();
      });
      errorUnlisten = await listen<string>('ai_error', (ev) => {
        clearSafetyTimer();
        appendMsg({ role: 'assistant', content: `Error: ${ev.payload}`, ts: Date.now() });
        busy = false;
        store.setLoading({ kind: null, label: '' });
        streamingText = '';
        tick();
      });
    } catch (e) {
      // No Tauri runtime (e.g. dev browser) — events won't fire, but the
      // stub fallback below still works.
    }
  });

  onDestroy(() => {
    clearSafetyTimer();
    aiUnlisten(); toolUseUnlisten(); toolResultUnlisten(); opsUnlisten();
    doneUnlisten(); errorUnlisten();
  });

  /** Build the chat history sent to the model. Injects the current scene
   *  as a hidden system-reminder on the most recent user message. */
  function buildChatMessages(): ChatMessage[] {
    const out: ChatMessage[] = [];
    for (let i = 0; i < messages.length; i++) {
      const m = messages[i];
      if (m.role === 'system') continue;
      if (m.role === 'user' && i === messages.length - 1) {
        const sceneBlock = describeScene();
        out.push({
          role: 'user',
          content: `${m.content}\n\n[scene-context]\n${sceneBlock}`,
        });
      } else {
        out.push({ role: m.role as 'user' | 'assistant', content: m.content });
      }
    }
    return out;
  }

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    appendMsg({ role: 'user', content: text, ts: Date.now() });
    input = '';
    busy = true;
    streamingText = '';
    currentThought = '';
    thoughts = [];
    lastToolUse = null;
    lastToolResult = null;

    if ($apiKeyStatus !== 'present') {
      // Local stub: tiny keyword matcher that exercises the same scene store.
      const id = `mesh_${Math.random().toString(36).slice(2, 8)}`;
      const color = /red/i.test(text) ? '#e05555'
                  : /blue/i.test(text) ? '#5588e0'
                  : /green/i.test(text) ? '#55c878'
                  : '#cccccc';
      const sphere = /sphere/i.test(text);
      const cube = /cube|box/i.test(text);
      const plane = /plane|ground|floor/i.test(text);
      const torus = /torus|ring|donut/i.test(text);
      const prim = sphere ? 'sphere' : plane ? 'plane' : torus ? 'torus' : cube ? 'box' : 'box';
      const yPos = prim === 'plane' ? 0 : 0.5;
      const op: SceneOp = {
        kind: 'add_primitive', id, parent: null, primitive: prim as any,
        transform: { position: [0, yPos, 0], rotation: [0, 0, 0], scale: [1, 1, 1] },
        material: { color, metalness: 0, roughness: 0.7 }, name: prim,
      };
      lastToolUse = { name: 'three_d_apply_ops', args: { ops: [op] } };
      // Simulated thought so the Thoughts panel has something to show.
      thoughts = [`[stub] I will add a ${prim} to the scene. I have no MiniMax key so the AI is offline — this is a local simulation.`];
      await new Promise((r) => setTimeout(r, 250));
      const r = store.applyOps([op]);
      lastToolResult = { name: 'three_d_apply_ops', ok: r.ok, error: r.error };
      appendMsg({
        role: 'assistant',
        content: r.ok
          ? `[stub — no MiniMax key] Added a ${prim} (id: ${id}). Add a MiniMax key in Settings to enable real AI control.`
          : `[stub] Failed: ${r.error}`,
        ts: Date.now(),
      });
      busy = false;
      tick();
      return;
    }

    // Real path: invoke minimax_chat_stream with the three_d tool set.
    startSafetyTimer();
    try {
      await minimaxChatStream({
        messages: buildChatMessages(),
        model: 'MiniMax-M3',
        tools_preset: 'three_d',
        system_prompt: SYSTEM_PROMPT,
      });
      // The `ai_done` listener will set `busy = false`.
    } catch (e: any) {
      clearSafetyTimer();
      appendMsg({ role: 'assistant', content: `Failed to start chat: ${e?.message ?? e}`, ts: Date.now() });
      busy = false;
      store.setLoading({ kind: null, label: '' });
      tick();
    }
  }
</script>

<div class="chat">
  <!-- Key-status banner -->
  <div class="banner" class:ok={$apiKeyStatus === 'present'} class:bad={$apiKeyStatus !== 'present'}>
    {#if $apiKeyStatus === 'present'}
      <span class="dot ok"></span>
      <span>3D Thoughts · MiniMax-M3 · scene-aware · material presets + lighting</span>
    {:else}
      <span class="dot bad"></span>
      <span>MiniMax API key not set — AI control disabled (local stub below).</span>
      <button type="button" class="link" on:click={openSettings}>Open Settings →</button>
    {/if}
  </div>

  <div class="messages" id="three-d-chat-messages">
    {#each messages as m, i (i)}
      <div class="msg {m.role}">
        <div class="role">{m.role}</div>
        <div class="content">{m.content}</div>
      </div>
    {/each}

    <!-- 3D Thoughts panel: shows the model's reasoning_content -->
    {#if thoughts.length > 0 || currentThought}
      <div class="thoughts">
        <button type="button" class="thoughts-head" on:click={() => (thoughtsExpanded = !thoughtsExpanded)}>
          <span class="t-icon">💭</span>
          <span class="t-title">3D Thoughts</span>
          <span class="t-count">{thoughts.length}{currentThought ? '+' : ''}</span>
          <span class="t-chev">{thoughtsExpanded ? '▾' : '▸'}</span>
        </button>
        {#if thoughtsExpanded}
          <div class="thoughts-body">
            {#each thoughts as t, i (i)}
              <div class="thought">{t}</div>
            {/each}
            {#if currentThought}
              <div class="thought streaming">{currentThought}<span class="cursor">▍</span></div>
            {/if}
          </div>
        {/if}
      </div>
    {/if}

    {#if streamingText}
      <div class="msg assistant streaming">
        <div class="role">assistant</div>
        <div class="content">{streamingText}<span class="cursor">▍</span></div>
      </div>
    {/if}
    {#if lastToolUse}
      <div class="msg tool">
        <div class="role">tool_use</div>
        <div class="content">
          <code>{lastToolUse.name}</code>({JSON.stringify(lastToolUse.args).slice(0, 220)}{JSON.stringify(lastToolUse.args).length > 220 ? '…' : ''})
        </div>
      </div>
    {/if}
    {#if lastToolResult}
      <div class="msg tool-result" class:ok={lastToolResult.ok} class:err={!lastToolResult.ok}>
        <div class="role">tool_result</div>
        <div class="content">
          {lastToolResult.ok ? '✓ ok' : `✗ ${lastToolResult.error ?? 'error'}`}
          {#if lastToolResult.dataUrl}
            <div class="thumb"><img src={lastToolResult.dataUrl} alt="generated" /></div>
          {/if}
        </div>
      </div>
    {/if}
  </div>
  <form class="composer" on:submit|preventDefault={send}>
    <input type="text" placeholder="Ask 3D Thoughts to build or modify the scene…"
      bind:value={input} disabled={busy} />
    <button type="submit" class="primary" disabled={busy || !input.trim()}>
      {busy ? '…' : 'Send'}
    </button>
  </form>
</div>

<style>
  .chat {
    display: flex; flex-direction: column;
    background: var(--bg-elevated);
    border-top: 1px solid var(--border);
    height: 100%; min-height: 0;
  }
  .banner {
    display: flex; align-items: center; gap: 8px;
    padding: 6px 12px;
    font-size: 11px;
    border-bottom: 1px solid var(--border);
    background: rgba(240, 144, 144, 0.08);
    color: #f09090;
  }
  .banner.ok { background: rgba(109, 209, 143, 0.10); color: #6dd18f; }
  .banner.bad { background: rgba(240, 144, 144, 0.10); color: #f09090; }
  .dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
  .dot.ok { background: #6dd18f; }
  .dot.bad { background: #f09090; }
  .banner .link {
    margin-left: auto;
    background: transparent; color: inherit;
    border: 1px solid currentColor; border-radius: 4px;
    padding: 2px 8px; font: inherit; font-size: 10px; cursor: pointer;
  }
  .banner .link:hover { background: rgba(255,255,255,0.08); }

  .messages {
    flex: 1; overflow-y: auto;
    padding: 8px 12px;
    display: flex; flex-direction: column; gap: 6px;
  }
  .msg { font-size: 12px; padding: 6px 8px; border-radius: 6px; max-width: 95%; }
  .msg .role { font-size: 9px; text-transform: uppercase; color: #6c7280; margin-bottom: 2px; letter-spacing: 0.4px; }
  .msg.user { background: var(--accent-soft); color: var(--text); align-self: flex-end; }
  .msg.assistant { background: #1c1f26; color: var(--text); }
  .msg.assistant.streaming { background: #1c1f26; }
  .msg.system { background: transparent; color: var(--text-muted); font-style: italic; }
  .msg.tool { background: #0f1217; color: var(--accent-strong); font-family: ui-monospace, monospace; }
  .msg.tool-result { font-family: ui-monospace, monospace; }
  .msg.tool-result.ok { background: #14331f; color: #6dd18f; }
  .msg.tool-result.err { background: #2a1818; color: #f09090; }
  .thumb { margin-top: 4px; }
  .thumb img { max-width: 100%; border-radius: 4px; display: block; }
  code { background: #00000040; padding: 1px 4px; border-radius: 3px; }

  /* ---- 3D Thoughts panel ---- */
  .thoughts {
    background: linear-gradient(180deg, rgba(74, 120, 200, 0.08) 0%, rgba(74, 120, 200, 0.02) 100%);
    border: 1px solid rgba(74, 120, 200, 0.25);
    border-radius: 6px;
    font-size: 12px;
    color: #c8d2e0;
  }
  .thoughts-head {
    display: flex; align-items: center; gap: 6px;
    width: 100%;
    padding: 6px 10px;
    background: transparent; border: 0; color: #9ab0d0;
    cursor: pointer; font: inherit; font-size: 11px; font-weight: 500;
    text-align: left;
  }
  .thoughts-head:hover { color: #c8d2e0; }
  .t-icon { font-size: 14px; }
  .t-title { letter-spacing: 0.3px; }
  .t-count {
    margin-left: 4px; padding: 1px 6px; border-radius: 8px;
    background: rgba(74, 120, 200, 0.20); color: #9ab0d0;
    font-size: 10px;
  }
  .t-chev { margin-left: auto; opacity: 0.6; }
  .thoughts-body { padding: 0 10px 8px 10px; display: flex; flex-direction: column; gap: 4px; }
  .thought {
    background: rgba(0,0,0,0.18);
    border-left: 2px solid rgba(74, 120, 200, 0.5);
    padding: 4px 8px; border-radius: 0 4px 4px 0;
    font-size: 11px; line-height: 1.45;
    color: #c0c8d6;
    white-space: pre-wrap;
  }
  .thought.streaming { border-left-color: #6ea8ff; }

  .cursor { animation: blink 1s steps(1) infinite; }
  @keyframes blink { 50% { opacity: 0; } }

  .composer {
    display: flex; gap: 4px; padding: 6px 8px; border-top: 1px solid var(--border);
  }
  .composer input { flex: 1; background: #0f1217; color: #e6e8eb; border: 1px solid #2c313a; border-radius: 4px; padding: 5px 8px; font: inherit; }
  .composer input:disabled { opacity: 0.5; }
  button.primary { background: #4a78c8; color: white; border: 1px solid #4a78c8; border-radius: 4px; padding: 4px 12px; cursor: pointer; }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
