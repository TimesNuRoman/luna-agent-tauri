<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import {
    taskList,
    taskGet,
    taskDelete,
    taskCancel,
    taskCreate,
    healProject,
    statusLabel,
    formatTokens,
    titleFromPrompt,
    type TaskSummary,
    type Task,
    type TaskStatus,
  } from './lib/taskClient';

  // --- Events ---
  // The parent App.svelte renders either TasksSidebar OR PlansSidebar
  // in the same 280px slot, controlled by `sidebarMode`. This dispatcher
  // lets the user hop to the plans view from the tasks view (and back)
  // without going through any top-level control.
  const dispatch = createEventDispatcher<{
    switch: { mode: 'tasks' | 'plans' };
  }>();

  function switchToPlans() {
    dispatch('switch', { mode: 'plans' });
  }

  // --- State ---
  let tasks: TaskSummary[] = [];
  let loading = false;
  let error: string | null = null;
  let selectedTask: Task | null = null;
  let selectedLoading = false;
  // F6: per-step cost. Loaded when the detail modal opens.
  let stepDetails: Array<Record<string, unknown>> = [];
  let stepsLoading = false;
  let refreshInterval: ReturnType<typeof setInterval> | null = null;
  /// True while we're spawning a heal task. Disables the
  /// "🌟 Heal" button to prevent double-clicks.
  let healing = false;

  // Filters
  let activeFilter: 'all' | TaskStatus = 'all';

  async function refresh() {
    loading = true;
    error = null;
    try {
      const list = await taskList('all');
      tasks = list;
    } catch (e) {
      error = String(e);
      console.error('[TasksSidebar] refresh failed:', e);
    } finally {
      loading = false;
    }
  }

  async function openDetail(t: TaskSummary) {
    selectedLoading = true;
    stepsLoading = true;
    stepDetails = [];
    try {
      selectedTask = await taskGet(t.id);
    } catch (e) {
      error = String(e);
    } finally {
      selectedLoading = false;
    }
    // F6: per-step cost from the `task_steps` IPC. Best-effort: if it
    // fails, we just keep the modal without per-step rows.
    try {
      const { taskSteps } = await import('./lib/taskClient');
      const steps = await taskSteps(t.id);
      stepDetails = Array.isArray(steps) ? steps : [];
    } catch {
      stepDetails = [];
    } finally {
      stepsLoading = false;
    }
  }

  function closeDetail() {
    selectedTask = null;
    stepDetails = [];
  }

  /// F6: extract a numeric USD cost from a step record. The Rust
  /// TaskStep type carries `cost_usd` (f64) plus token fields; we
  /// accept any number-like value to be forward-compatible.
  function stepCostUsd(s: Record<string, unknown>): number {
    const c = s.cost_usd ?? s.costUsd ?? s.usd;
    return typeof c === 'number' && Number.isFinite(c) ? c : 0;
  }
  function stepLabel(s: Record<string, unknown>): string {
    const t = (s.type ?? s.kind ?? 'step') as string;
    const idx = (s.index ?? s.step ?? s.seq) as number | undefined;
    return idx !== undefined ? `${t} #${idx}` : t;
  }
  function stepTokens(s: Record<string, unknown>): { inTok: number; outTok: number } {
    const i = (s.input_tokens ?? s.inputTokens ?? 0) as number;
    const o = (s.output_tokens ?? s.outputTokens ?? 0) as number;
    return { inTok: i, outTok: o };
  }
  $: totalStepUsd = stepDetails.reduce((sum, s) => sum + stepCostUsd(s), 0);
  $: hasStepCost = stepDetails.some((s) => stepCostUsd(s) > 0);

  async function deleteTask(t: TaskSummary) {
    if (!confirm(`Delete task "${t.title || t.id}"? This removes all its files and cannot be undone.`)) return;
    try {
      await taskDelete(t.id);
      if (selectedTask?.id === t.id) selectedTask = null;
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  /// F2: cancel a running/pending task. The button is only rendered for
  /// tasks whose status is 'pending' or 'running'. `taskCancel` is
  /// idempotent on the backend, so a double-click is safe.
  async function cancelTask(t: TaskSummary, ev: Event) {
    ev.stopPropagation();
    try {
      await taskCancel(t.id);
      // The 5s poll will pick up the new status, but call refresh()
      // immediately so the UI flips within ~100ms.
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  /// F6: retry a failed/cancelled/timed_out task by re-spawning it with
  /// the same prompt. Title is preserved if available; otherwise we
  /// derive a short title from the prompt.
  async function retryTask(t: TaskSummary, ev: Event) {
    ev.stopPropagation();
    try {
      const full = await taskGet(t.id);
      const title = (full.title && full.title.trim()) || titleFromPrompt(full.prompt);
      await taskCreate({
        title: `${title} (retry)`,
        prompt: full.prompt,
        personaId: undefined,
      });
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  /**
   * One-click heal of the current workspace. Spawns a MorningStar
   * (Lucifer) task. After the spawn we refresh the list so the
   * new task appears at the top. The runner then drives the fix
   * loop; we don't await its completion here.
   */
  async function triggerHeal() {
    if (healing) return;
    healing = true;
    try {
      await healProject('manual');
      // Give the runner a moment to persist the new task, then
      // refresh so it shows up at the top of the list.
      await new Promise((r) => setTimeout(r, 200));
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      healing = false;
    }
  }

  onMount(() => {
    refresh();
    // Refresh every 5s for the Running / Pending pills. Cheap RPC.
    refreshInterval = setInterval(refresh, 5000);
    return () => {
      if (refreshInterval) clearInterval(refreshInterval);
    };
  });

  $: filtered = activeFilter === 'all'
    ? tasks
    : tasks.filter((t) => t.status === activeFilter);

  $: inProgressCount = tasks.filter(
    (t) => t.status === 'pending' || t.status === 'running',
  ).length;

  function formatTs(iso: string | null): string {
    if (!iso) return '—';
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }

  function statusClass(s: TaskStatus): string {
    return `ts-pill ts-pill-${s}`;
  }
</script>

<aside class="ts-root">
  <header class="ts-header">
    <div>
      <h3>🧬 Background tasks</h3>
      {#if inProgressCount > 0}
        <span class="ts-badge">{inProgressCount} active</span>
      {/if}
    </div>
    <div class="ts-header-actions">
      <button
        class="ts-heal"
        type="button"
        on:click={triggerHeal}
        disabled={healing}
        title="Запустить Люцифера — healer починит проект"
        aria-label="Heal project"
      >{healing ? '…' : '🌟'}</button>
      <button
        class="ts-switch"
        type="button"
        on:click={switchToPlans}
        title="Переключить на Plans Sidebar"
        aria-label="Plans"
      >📋</button>
      <button class="ts-refresh" on:click={refresh} disabled={loading} title="Refresh">
        {loading ? '…' : '↻'}
      </button>
    </div>
  </header>

  {#if error}
    <div class="ts-error" role="alert">
      <strong>Error:</strong> {error}
    </div>
  {/if}

  <div class="ts-filters">
    <select bind:value={activeFilter}>
      <option value="all">All ({tasks.length})</option>
      <option value="pending">Pending</option>
      <option value="running">Running</option>
      <option value="completed">Completed</option>
      <option value="failed">Failed</option>
      <option value="cancelled">Cancelled</option>
      <option value="timed_out">Timed out</option>
    </select>
  </div>

  {#if tasks.length === 0 && !loading}
    <p class="ts-empty">
      No background tasks yet.
      <br />
      <span class="muted">Open a chat → "Send to background" (coming in Phase M1).</span>
    </p>
  {:else}
    <ul class="ts-list">
      {#each filtered as t (t.id)}
        <li class="ts-item" on:click={() => openDetail(t)} on:keydown={(e) => e.key === 'Enter' && openDetail(t)} role="button" tabindex="0">
          <div class="ts-item-head">
            <span class="ts-title">{t.title || t.id}</span>
            <span class={statusClass(t.status)}>{statusLabel(t.status)}</span>
          </div>
          <div class="ts-item-meta muted">
            {formatTokens(t.total_tokens)} tok · {t.steps_completed} steps
            {#if t.parent_chat_id}· chat: {t.parent_chat_id.slice(0, 12)}…{/if}
          </div>
          <div class="ts-item-time muted">
            {formatTs(t.started_at ?? t.created_at)}
            {#if t.cancellation_requested}<span class="ts-cancel-flag">· cancel requested</span>{/if}
          </div>
          <!-- F2: cancel button for active tasks. F6: retry for terminal-failure states. -->
          <div class="ts-item-actions">
            {#if t.status === 'pending' || t.status === 'running'}
              <button
                class="ts-btn-cancel"
                on:click|stopPropagation={(e) => cancelTask(t, e)}
                title="Отменить задачу"
                aria-label="Cancel task {t.title || t.id}">■</button>
            {/if}
            {#if t.status === 'failed' || t.status === 'cancelled' || t.status === 'timed_out'}
              <button
                class="ts-btn-retry"
                on:click|stopPropagation={(e) => retryTask(t, e)}
                title="Перезапустить с тем же промптом"
                aria-label="Retry task {t.title || t.id}">↻</button>
            {/if}
            <button
              class="ts-btn-danger"
              on:click|stopPropagation={() => deleteTask(t)}
              title="Delete task and all its files"
              aria-label="Delete task {t.title || t.id}">
              ×
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- Detail modal -->
  {#if selectedTask}
    <div class="ts-modal-backdrop" role="dialog" aria-modal="true" on:click={closeDetail}>
      <div class="ts-modal" on:click|stopPropagation>
        {#if selectedLoading}
          <p class="muted">Loading…</p>
        {:else}
          <header class="ts-modal-head">
            <h3>{selectedTask.title || selectedTask.id}</h3>
            <button class="ts-btn-close" on:click={closeDetail}>×</button>
          </header>
          <dl class="ts-modal-meta">
            <dt>Status</dt>
            <dd><span class={statusClass(selectedTask.status)}>{statusLabel(selectedTask.status)}</span></dd>
            <dt>Model</dt><dd><code>{selectedTask.model}</code></dd>
            <dt>Sub-agent</dt><dd><code>{selectedTask.sub_agent_model}</code></dd>
            <dt>Created</dt><dd>{formatTs(selectedTask.created_at)}</dd>
            <dt>Started</dt><dd>{formatTs(selectedTask.started_at)}</dd>
            <dt>Finished</dt><dd>{formatTs(selectedTask.finished_at)}</dd>
            <dt>Steps</dt><dd>{selectedTask.steps_completed} / {selectedTask.max_steps}</dd>
            <dt>Sub-agents</dt><dd>{selectedTask.sub_agent_count} / {selectedTask.max_subagents}</dd>
            <dt>Tokens</dt>
            <dd>
              {formatTokens(selectedTask.cost.input_tokens + selectedTask.cost.output_tokens)} in/out
              {#if selectedTask.cost.estimated_usd > 0}
                · ~${selectedTask.cost.estimated_usd.toFixed(4)}
              {/if}
            </dd>
            {#if selectedTask.parent_chat_id}
              <dt>Parent chat</dt><dd><code>{selectedTask.parent_chat_id}</code></dd>
            {/if}
          </dl>

          {#if selectedTask.error}
            <div class="ts-error">
              <strong>Error:</strong>
              <pre>{selectedTask.error}</pre>
            </div>
          {/if}

          <!-- F6: per-step cost breakdown. Hidden if there are no
               steps or no step carries cost_usd. -->
          {#if stepDetails.length > 0}
            <details open class="ts-steps-block">
              <summary>
                Per-step cost ({stepDetails.length} step{stepDetails.length === 1 ? '' : 's'}
                {#if hasStepCost}· total ~${totalStepUsd.toFixed(4)}{/if})
              </summary>
              {#if stepsLoading}
                <p class="muted">Loading steps…</p>
              {:else}
                <table class="ts-steps-table">
                  <thead>
                    <tr>
                      <th>Step</th>
                      <th class="num">In</th>
                      <th class="num">Out</th>
                      <th class="num">USD</th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each stepDetails as s, i (i)}
                      {@const tk = stepTokens(s)}
                      <tr>
                        <td>{stepLabel(s)}</td>
                        <td class="num">{formatTokens(tk.inTok)}</td>
                        <td class="num">{formatTokens(tk.outTok)}</td>
                        <td class="num">{stepCostUsd(s) > 0 ? `$${stepCostUsd(s).toFixed(4)}` : '—'}</td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              {/if}
            </details>
          {/if}

          <details>
            <summary>Prompt</summary>
            <pre class="ts-prompt">{selectedTask.prompt}</pre>
          </details>
        {/if}
      </div>
    </div>
  {/if}
</aside>

<style>
  .ts-root {
    width: 280px;
    flex-shrink: 0;
    border-right: 1px solid var(--border, #e3e3e6);
    background: var(--bg-elevated, #fafafa);
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .ts-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ts-header h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    display: inline-block;
    margin-right: 6px;
  }
  .ts-header-actions { display: flex; gap: 2px; }
  .ts-heal {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 16px;
    line-height: 1;
    transition: background 0.15s, color 0.15s;
  }
  .ts-heal:hover:not(:disabled) {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .ts-heal:disabled {
    opacity: 0.5;
    cursor: wait;
  }
  .ts-switch {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 4px 6px;
    border-radius: 4px;
  }
  .ts-switch:hover { background: rgba(0,0,0,0.05); color: var(--text, #1c1c1e); }
  .ts-badge {
    display: inline-block;
    background: var(--accent, #4a6fcf);
    color: #fff;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
    vertical-align: middle;
  }
  .ts-refresh {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .ts-refresh:hover:not(:disabled) { background: rgba(0,0,0,0.05); }
  .ts-refresh:disabled { opacity: 0.5; }

  .ts-filters {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ts-filters select {
    width: 100%;
    padding: 4px 6px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    font-size: 12px;
  }

  .ts-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    flex: 1;
  }
  .ts-item {
    position: relative;
    padding: 10px 12px 10px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    cursor: pointer;
    transition: background 80ms ease;
  }
  .ts-item:hover { background: rgba(0,0,0,0.04); }
  .ts-item-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
    padding-right: 80px;
  }
  .ts-title {
    font-size: 13px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .ts-item-meta,
  .ts-item-time {
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
  }
  .ts-cancel-flag {
    color: #b65a00;
    font-weight: 500;
  }
  .ts-btn-danger {
    position: relative;
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 80ms ease, background 80ms ease;
  }
  .ts-item:hover .ts-btn-danger,
  .ts-item:hover .ts-btn-cancel,
  .ts-item:hover .ts-btn-retry { opacity: 1; }
  .ts-btn-danger:hover { background: rgba(176, 48, 48, 0.1); color: #b03030; }
  /* F2: cancel button for in-flight tasks */
  .ts-btn-cancel {
    position: relative;
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 12px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 80ms ease, background 80ms ease;
  }
  .ts-btn-cancel:hover { background: rgba(176, 96, 0, 0.12); color: #b65a00; }
  /* F6: retry button for failed/cancelled tasks */
  .ts-btn-retry {
    position: relative;
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 80ms ease, background 80ms ease;
  }
  .ts-btn-retry:hover { background: rgba(74, 111, 207, 0.12); color: var(--accent, #4a6fcf); }
  .ts-item-actions {
    position: absolute;
    top: 6px;
    right: 6px;
    display: flex;
    gap: 2px;
    align-items: center;
  }
  /* F6: per-step cost table */
  .ts-steps-block {
    margin: 10px 0;
    font-size: 12px;
  }
  .ts-steps-block summary {
    cursor: pointer;
    font-weight: 500;
    padding: 4px 0;
    color: var(--text-muted, #6b6b70);
  }
  .ts-steps-table {
    width: 100%;
    border-collapse: collapse;
    margin-top: 4px;
    font-size: 12px;
  }
  .ts-steps-table th,
  .ts-steps-table td {
    text-align: left;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ts-steps-table th.num,
  .ts-steps-table td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .ts-steps-table th {
    color: var(--text-muted, #6b6b70);
    font-weight: 500;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
  }

  .ts-pill {
    display: inline-block;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    color: #fff;
  }
  .ts-pill-pending { background: #888; }
  .ts-pill-running { background: #4a6fcf; }
  .ts-pill-completed { background: #1b7a3a; }
  .ts-pill-failed { background: #b03030; }
  .ts-pill-cancelled { background: #5a5a5a; }
  .ts-pill-timed_out { background: #b65a00; }

  .ts-empty {
    padding: 18px 12px;
    font-size: 12px;
    text-align: center;
    color: var(--text-muted, #6b6b70);
  }
  .ts-error {
    margin: 8px 12px;
    padding: 6px 8px;
    background: rgba(176, 48, 48, 0.08);
    border: 1px solid #b03030;
    border-radius: 4px;
    font-size: 12px;
    color: #6b1a1a;
  }
  .ts-error pre {
    margin: 4px 0 0 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: ui-monospace, monospace;
    font-size: 11px;
  }
  .muted { color: var(--text-muted, #6b6b70); }

  .ts-modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .ts-modal {
    background: var(--bg-elevated, #fff);
    border-radius: 10px;
    padding: 16px 20px;
    max-width: 640px;
    width: calc(100% - 40px);
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 12px 40px rgba(0,0,0,0.25);
  }
  .ts-modal-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  .ts-modal-head h3 {
    margin: 0;
    font-size: 16px;
  }
  .ts-btn-close {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 22px;
    line-height: 1;
    padding: 0 8px;
    border-radius: 4px;
  }
  .ts-btn-close:hover { background: rgba(0,0,0,0.05); }
  .ts-modal-meta {
    display: grid;
    grid-template-columns: 110px 1fr;
    row-gap: 4px;
    column-gap: 12px;
    font-size: 12px;
    margin-bottom: 12px;
  }
  .ts-modal-meta dt {
    color: var(--text-muted, #6b6b70);
  }
  .ts-modal-meta dd {
    margin: 0;
  }
  .ts-modal-meta code {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    background: rgba(0,0,0,0.04);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .ts-prompt {
    white-space: pre-wrap;
    word-break: break-word;
    font-family: ui-monospace, monospace;
    font-size: 11px;
    background: rgba(0,0,0,0.04);
    padding: 8px;
    border-radius: 4px;
    max-height: 240px;
    overflow-y: auto;
  }
  details summary {
    cursor: pointer;
    font-size: 12px;
    color: var(--text-muted, #6b6b70);
    margin-bottom: 4px;
  }




</style>
