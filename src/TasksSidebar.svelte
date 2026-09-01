<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import {
    taskList,
    taskGet,
    taskDelete,
    statusLabel,
    formatTokens,
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
  let refreshInterval: ReturnType<typeof setInterval> | null = null;

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
    try {
      selectedTask = await taskGet(t.id);
    } catch (e) {
      error = String(e);
    } finally {
      selectedLoading = false;
    }
  }

  function closeDetail() {
    selectedTask = null;
  }

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
          <button
            class="ts-btn-danger"
            on:click|stopPropagation={() => deleteTask(t)}
            title="Delete task and all its files">
            ×
          </button>
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
    padding: 10px 32px 10px 12px;
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
    position: absolute;
    top: 6px;
    right: 6px;
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 80ms ease, background 80ms ease;
  }
  .ts-item:hover .ts-btn-danger { opacity: 1; }
  .ts-btn-danger:hover { background: rgba(176, 48, 48, 0.1); color: #b03030; }

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
