<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import {
    monitors,
    selectedMonitorId,
    fps,
    maxWidth,
    goal,
    running,
    captureState,
    latestFrame,
    lastError,
    hints,
    budgetExhausted,
    canStart,
    goalCharCount,
    pushHint,
    clearHints,
    consentAccepted,
    minimaxKeyStatus,
    refreshMinimaxKeyStatus,
    videoAutoInvoke,
    autoInvocationsUsed,
    setVideoAutoInvoke,
    subscribeToAutoTriggers,
  } from './lib/videomode-store';
  import {
    listMonitors,
    startScreenCapture,
    stopScreenCapture,
    setActiveGoal,
    onScreenFrame,
    onAgentHint,
    onCaptureError,
    onCaptureState,
    captureSingleFrame,
    chatInjectUserMessage,
  } from './lib/tauri';
  import ConsentModal from './ConsentModal.svelte';
  import ApiKeyModal from './ApiKeyModal.svelte';

  const MAX_GOAL_LEN = 2048;
  let showConsent = false;
  let showApiKey = false;
  let unlisteners: Array<() => void> = [];
  let starting = false;
  let stopping = false;
  let lastStartError: string | null = null;

  $: goalTooLong = $goalCharCount > MAX_GOAL_LEN;
  $: effectiveGoal = goalTooLong ? $goal.slice(0, MAX_GOAL_LEN) : $goal;

  // `on:change` inline attribute can't hold a TS `as` cast without
  // breaking Svelte's parser in production builds. Hoist to a handler
  // at the script top level (must NOT be inside `onMount`, or the
  // template can't see it).
  function onAutoInvokeChange(e: Event) {
    const checked = (e.currentTarget as HTMLInputElement).checked;
    setVideoAutoInvoke(checked);
  }

  onMount(async () => {
    try {
      const m = await listMonitors();
      monitors.set(m);
    } catch (e) {
      lastError.set({ code: 'internal', message: String(e), t_ms: Date.now() });
    }
    // Probe keyring for MiniMax key on first mount.
    await refreshMinimaxKeyStatus();

    unlisteners.push(await onScreenFrame((p) => latestFrame.set(p)));
    unlisteners.push(await onAgentHint((p) => pushHint(p)));
    unlisteners.push(
      await onCaptureError((p) => lastError.set(p)),
    );
    unlisteners.push(
      await onCaptureState((p) => {
        captureState.set(p);
        running.set(p.running);
        if (typeof p.auto_invocations_used === 'number') {
          autoInvocationsUsed.set(p.auto_invocations_used);
        }
      }),
    );

    // Auto-invoke bridge: when a real hint lands AND
    // `videoAutoInvoke` is on, the Rust side has already emitted
    // `video-auto-trigger` (subject to the 30 s debounce). We
    // forward that into the chat tab via a synthetic user message.
    unlisteners.push(
      await subscribeToAutoTriggers(async (p) => {
        // Mirror counter for the UI badge.
        autoInvocationsUsed.update((n) => n + 1);
        // Build a short, model-friendly text and push it into the
        // chat. The chat tab is responsible for switching focus and
        // running the actual `minimaxChatStream` call.
        const text = [
          '[Video Mode] На экране замечено:',
          `"${p.hint_text}"`,
          `Кадр #${p.seq}, монитор ${p.monitor_id} (${p.width}×${p.height}).`,
          p.goal ? `Цель наблюдения: ${p.goal}` : 'Цель не задана.',
          'Прокомментируй и предложи, что делать.',
        ].join(' ');
        try {
          await chatInjectUserMessage(text);
        } catch (e) {
          lastError.set({
            code: 'internal',
            message: `Auto-invoke failed: ${String(e)}`,
            t_ms: Date.now(),
          });
        }
      }),
    );

    // Push the current `videoAutoInvoke` value to the backend on
    // mount so the hint loop sees the right setting immediately.
    try {
      const { setVideoAutoinvoke } = await import('./lib/tauri');
      await setVideoAutoinvoke($videoAutoInvoke);
    } catch {
      /* no-op */
    }

    // Esc → stop
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && $running) {
        e.preventDefault();
        handleStop();
      }
    };
    window.addEventListener('keydown', onKey);
    unlisteners.push(() => window.removeEventListener('keydown', onKey));
  });

  onDestroy(() => {
    for (const u of unlisteners) {
      try { u(); } catch { /* no-op */ }
    }
  });

  async function handleStartClick() {
    if (starting || $running) return;
    if (!$consentAccepted) {
      // Show consent modal; on accept, it calls handleStart() directly.
      showConsent = true;
      return;
    }
    await handleStart();
  }

  async function handleStart() {
    if (starting || $running) return;
    starting = true;
    lastStartError = null;
    try {
      // Re-probe keyring right before starting, in case the user saved the
      // key in another window since we last checked.
      await refreshMinimaxKeyStatus();
      if ($minimaxKeyStatus !== 'set') {
        lastStartError =
          'MiniMax API ключ не задан. Нажмите 🔑 API Keys и сохраните ключ.';
        showApiKey = true;
        return;
      }
      // Push the goal before start so the first hint_loop tick uses it.
      await setActiveGoal(effectiveGoal.trim() || null);
      clearHints();
      await startScreenCapture({
        monitor_id: $selectedMonitorId,
        fps: $fps,
        max_width: $maxWidth,
      });
      showConsent = false;
      running.set(true);
    } catch (e) {
      lastStartError = String(e);
      lastError.set({ code: 'internal', message: String(e), t_ms: Date.now() });
    } finally {
      starting = false;
    }
  }

  async function handleStop() {
    if (stopping) return;
    stopping = true;
    try {
      await stopScreenCapture();
      running.set(false);
    } catch (e) {
      lastError.set({ code: 'internal', message: String(e), t_ms: Date.now() });
    } finally {
      stopping = false;
    }
  }

  async function handleSnapshot() {
    try {
      const f = await captureSingleFrame({
        monitor_id: $selectedMonitorId,
        max_width: $maxWidth,
      });
      latestFrame.set({
        seq: 0,
        base64: f.base64,
        width: f.width,
        height: f.height,
        t_ms: f.t_ms,
        monitor_id: f.monitor_id,
      });
    } catch (e) {
      lastError.set({ code: 'internal', message: String(e), t_ms: Date.now() });
    }
  }

  function hintLabel(kind: string): string {
    switch (kind) {
      case 'hint': return '💡';
      case 'noop': return '·';
      case 'error': return '⚠';
      case 'no_goal': return '—';
      case 'budget_exhausted': return '⏹';
      case 'stopped': return '■';
      default: return '·';
    }
  }
</script>

<div class="vm">
  {#if showConsent}
    <ConsentModal
      onAccept={handleStart}
      onDecline={() => (showConsent = false)} />
  {/if}

  {#if showApiKey}
    <ApiKeyModal onClose={() => (showApiKey = false)} />
  {/if}

  <header class="vm-header">
    <h1>🎥 Video Mode</h1>
    <div class="header-actions">
      <button class="ghost small" on:click={() => (showApiKey = true)} title="API ключи">
        🔑 {$minimaxKeyStatus === 'set' ? '✓' : 'API Keys'}
      </button>
      <div class="status">
        {#if $running}
          <span class="dot live" aria-hidden="true"></span>
          <span>Luna смотрит экран · монитор {$captureState?.monitor_id ?? 0} ·
            {($captureState?.fps ?? 1).toFixed(1)} fps ·
            {$captureState?.frames_sent ?? 0} / {$captureState?.frames_budget ?? 100} кадров
          </span>
        {:else}
          <span class="dot idle" aria-hidden="true"></span>
          <span>Остановлено</span>
        {/if}
      </div>
    </div>
  </header>

  {#if $minimaxKeyStatus === 'missing'}
    <div class="key-banner" role="status">
      <span>
        <strong>MiniMax API ключ не задан.</strong>
        Vision-вызовы не будут работать, пока вы не сохраните ключ.
      </span>
      <button class="primary small" on:click={() => (showApiKey = true)}>
        🔑 Ввести ключ
      </button>
      <button class="ghost small" on:click={() => refreshMinimaxKeyStatus()}>
        ↻ Проверить снова
      </button>
    </div>
  {/if}

  <section class="preview" class:live={$running}>
    {#if $latestFrame}
      <img src={$latestFrame.base64} alt="Последний кадр экрана" />
    {:else}
      <div class="placeholder">Кадров ещё нет. Нажмите Start.</div>
    {/if}
  </section>

  <section class="controls">
    <div class="row">
      <label>
        <span>Монитор</span>
        <select bind:value={$selectedMonitorId} disabled={$running}>
          {#each $monitors as m (m.id)}
            <option value={m.id}>
              {m.id}: {m.name} ({m.width}×{m.height}){m.is_primary ? ' · primary' : ''}
            </option>
          {/each}
        </select>
      </label>

      <label>
        <span>FPS</span>
        <select bind:value={$fps} disabled={$running}>
          <option value={0.5}>0.5</option>
          <option value={1.0}>1.0</option>
          <option value={2.0}>2.0</option>
        </select>
      </label>

      <label>
        <span>Разрешение</span>
        <select bind:value={$maxWidth} disabled={$running}>
          <option value={640}>640px (эконом)</option>
          <option value={1280}>1280px (средне)</option>
          <option value={1920}>1920px (макс)</option>
        </select>
      </label>

      <button on:click={handleSnapshot} disabled={$monitors.length === 0}
        title="Один кадр без запуска цикла">
        📸 Snapshot
      </button>
    </div>

    <div class="row goal">
      <label class="grow">
        <span>
          Цель наблюдения ({$goalCharCount} / {MAX_GOAL_LEN})
          {#if goalTooLong}<span class="warn">— обрезано</span>{/if}
        </span>
        <input
          type="text"
          bind:value={$goal}
          placeholder="предупреди, когда на экране появится полоска HP босса"
          disabled={$running}
          maxlength={MAX_GOAL_LEN}
        />
      </label>
    </div>

    <div class="row autoinvoke">
      <label class="toggle">
        <input
          type="checkbox"
          checked={$videoAutoInvoke}
          on:change={onAutoInvokeChange}
          disabled={$running}
        />
        <span>🤖 Авто-вызов агента чата</span>
        <small>
          При срабатывании подсказки агент в Chat-вкладке получит
          синтетическое сообщение и ответит. Дебаунс 30 с.
        </small>
      </label>
      <span class="badge" title="Сколько раз видеорежим уже вызвал агента в этой сессии">
        Авто-вызовов: <strong>{$autoInvocationsUsed}</strong>
      </span>
    </div>

    <div class="row actions">
      {#if !$running}
        <button
          class="primary"
          on:click={handleStartClick}
          disabled={!$canStart || starting}>
          {starting ? 'Запускаю…' : '🎥 Start Video Mode'}
        </button>
        {#if lastStartError}
          <span class="error">{lastStartError}</span>
        {/if}
      {:else}
        <button class="danger" on:click={handleStop} disabled={stopping}>
          {stopping ? 'Останавливаю…' : '⏹ Stop (Esc)'}
        </button>
      {/if}
      <button class="ghost" on:click={clearHints} disabled={$hints.length === 0}>
        Очистить лог
      </button>
    </div>

    {#if $lastError}
      <div class="error-banner" role="alert">
        <strong>
          {#if $lastError.code === 'permission_denied'}Доступ к экрану запрещён
          {:else if $lastError.code === 'monitor_disconnected'}Монитор отключился
          {:else}Ошибка{/if}
        </strong>
        <span>{$lastError.message}</span>
        <button class="ghost small" on:click={() => lastError.set(null)}>×</button>
      </div>
    {/if}

    {#if $budgetExhausted}
      <div class="warn-banner">
        Бюджет подсказок исчерпан. Перезапусти сессию (Stop → Start), чтобы продолжить.
      </div>
    {/if}
  </section>

  <section class="hints">
    <h2>Подсказки ({$hints.length})</h2>
    {#if $hints.length === 0}
      <p class="muted">
        Подсказки появятся, когда агент заметит на экране что-то, подходящее под цель.
      </p>
    {:else}
      <ul>
        {#each $hints as h, i (h.t_ms + '-' + i)}
          <li class="hint kind-{h.kind}">
            <span class="badge" aria-hidden="true">{hintLabel(h.kind)}</span>
            <span class="text">{h.text}</span>
            <span class="meta">
              {h.kind}{h.seq !== undefined ? ` · кадр #${h.seq}` : ''}
            </span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .vm {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 20px;
    color: #e6e8eb;
    max-width: 1000px;
    margin: 0 auto;
  }
  .vm-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 10px;
  }
  .vm-header h1 { margin: 0; font-size: 20px; }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .key-banner {
    background: #2a2018;
    border: 1px solid #f5b56b;
    color: #f5b56b;
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 13px;
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .key-banner strong { color: #ffce8a; }
  .key-banner button { margin-left: auto; }
  .status {
    font-size: 13px;
    color: #b6bcc7;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    display: inline-block;
  }
  .dot.live { background: #c34c4c; box-shadow: 0 0 8px #c34c4c; }
  .dot.idle { background: #4a505c; }

  .preview {
    position: relative;
    background: #0d0f12;
    border: 1px solid #2c313a;
    border-radius: 8px;
    overflow: hidden;
    min-height: 240px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .preview.live { border-color: #c34c4c; box-shadow: 0 0 0 1px #c34c4c inset; }
  .preview img { max-width: 100%; max-height: 480px; object-fit: contain; }
  .placeholder { color: #6c7280; font-size: 14px; }

  .controls {
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: #181b21;
    padding: 12px 14px;
    border-radius: 8px;
    border: 1px solid #2c313a;
  }
  .row {
    display: flex;
    gap: 10px;
    align-items: end;
    flex-wrap: wrap;
  }
  .row.goal .grow { flex: 1 1 100%; }
  .row.autoinvoke {
    align-items: center;
    gap: 14px;
  }
  .row.autoinvoke .toggle {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    flex: 1 1 auto;
    color: #b6bcc7;
    font-size: 13px;
  }
  .row.autoinvoke .toggle input[type="checkbox"] {
    width: 16px;
    height: 16px;
    accent-color: #c34c4c;
    cursor: pointer;
  }
  .row.autoinvoke .toggle small {
    color: #6c7280;
    font-size: 11px;
    margin-left: 6px;
  }
  .row.autoinvoke .badge {
    background: #181b21;
    border: 1px solid #2c313a;
    border-radius: 6px;
    padding: 4px 10px;
    font-size: 12px;
    color: #b6bcc7;
  }
  .row.autoinvoke .badge strong {
    color: #ffce8a;
    margin-left: 4px;
  }
  label {
    display: flex;
    flex-direction: column;
    font-size: 12px;
    color: #b6bcc7;
    gap: 4px;
  }
  label.grow { flex: 1; }
  input, select {
    background: #0f1217;
    color: #e6e8eb;
    border: 1px solid #2c313a;
    border-radius: 4px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
  }
  input:focus, select:focus { outline: 1px solid #4a78c8; }

  .actions { align-items: center; }
  button {
    padding: 7px 14px;
    border-radius: 6px;
    border: 1px solid transparent;
    cursor: pointer;
    font-size: 13px;
    background: #2c313a;
    color: #e6e8eb;
  }
  button:hover:not(:disabled) { background: #353c47; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  button.primary {
    background: #c34c4c;
    color: white;
    border-color: #c34c4c;
  }
  button.primary:hover:not(:disabled) { background: #d75a5a; }
  button.danger {
    background: #8a3a3a;
    color: white;
    border-color: #8a3a3a;
  }
  button.danger:hover:not(:disabled) { background: #a04848; }
  button.ghost {
    background: transparent;
    border-color: #3a414b;
    color: #cfd3da;
  }
  button.ghost:hover:not(:disabled) { background: #252932; }
  button.small { padding: 2px 8px; font-size: 16px; line-height: 1; }

  .error { color: #f09090; font-size: 13px; }
  .error-banner {
    background: #2a1818;
    border: 1px solid #8a3a3a;
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 13px;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .error-banner strong { color: #f5a5a5; }
  .error-banner button { margin-left: auto; }
  .warn { color: #f5b56b; }
  .warn-banner {
    background: #2a2018;
    border: 1px solid #f5b56b;
    color: #f5b56b;
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 13px;
  }

  .hints h2 { font-size: 14px; margin: 0 0 6px 0; color: #b6bcc7; }
  .hints ul {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .hint {
    display: flex;
    gap: 10px;
    align-items: baseline;
    background: #181b21;
    border: 1px solid #2c313a;
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .hint.kind-error { border-color: #8a3a3a; background: #1f1414; }
  .hint.kind-noop, .hint.kind-no_goal { opacity: 0.6; }
  .hint .badge {
    font-family: ui-monospace, monospace;
    color: #6c7280;
    min-width: 18px;
  }
  .hint .text { flex: 1; }
  .hint .meta { color: #6c7280; font-size: 11px; }
  .muted { color: #6c7280; font-size: 13px; margin: 4px 0; }
</style>
