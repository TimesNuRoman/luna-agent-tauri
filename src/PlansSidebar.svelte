<script lang="ts">
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import {
    plans,
    planMessageMap,
    createPlan,
    deletePlan,
    renamePlan,
    updateStep,
    addStep,
    removeStep,
    moveStep,
    summarize,
    type Plan,
    type PlanStep,
  } from './lib/planStore';

  // ---- Props / events ----
  /** True while the agent is mid-stream. Disables Run/Continue buttons
   *  and shows a tooltip explaining why. */
  export let busy = false;

  const dispatch = createEventDispatcher<{
    run: { plan: Plan };
    continue: { plan: Plan };
    switch: { mode: 'tasks' | 'plans' };
  }>();

  // ---- Local UI state ----
  let expanded: Record<string, boolean> = {};
  let menuOpenFor: string | null = null;
  let editingTitleFor: string | null = null;
  let editingTitleValue = '';

  $: stats = summarize($plans);
  $: sorted = [...$plans].sort((a, b) => {
    // running > pending > error > done
    const rank = (p: Plan) => {
      if (p.steps.length === 0) return 2;
      const hasRunning = p.steps.some((s) => s.status === 'in_progress');
      const hasError = p.steps.some((s) => s.status === 'error');
      const allDone = p.steps.every((s) => s.status === 'done');
      if (hasRunning) return 0;
      if (hasError) return 2;
      if (allDone) return 3;
      return 1; // pending (some pending, none running, no errors)
    };
    const r = rank(a) - rank(b);
    if (r !== 0) return r;
    return b.updatedAt - a.updatedAt; // newest first within a tier
  });

  // ---- Helpers ----
  function planAggregateStatus(p: Plan): 'running' | 'pending' | 'done' | 'error' | 'empty' {
    if (p.steps.length === 0) return 'empty';
    const hasRunning = p.steps.some((s) => s.status === 'in_progress');
    const hasError = p.steps.some((s) => s.status === 'error');
    const allDone = p.steps.every((s) => s.status === 'done');
    if (hasRunning) return 'running';
    if (hasError) return 'error';
    if (allDone) return 'done';
    return 'pending';
  }
  function statusIcon(s: ReturnType<typeof planAggregateStatus>): string {
    switch (s) {
      case 'running': return '⏳';
      case 'done': return '✅';
      case 'error': return '⚠';
      case 'empty': return '○';
      default: return '📋';
    }
  }
  function stepStatusIcon(s: PlanStep['status']): string {
    if (s.status === 'done') return '✓';
    if (s.status === 'in_progress') return '⏳';
    if (s.status === 'error') return '⚠';
    return '○';
  }
  function formatTime(ts: number): string {
    try {
      return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    } catch { return ''; }
  }

  // ---- Handlers ----
  function newEmptyPlan() {
    const p = createPlan('Новый план', [{ id: '', title: 'Первый шаг', status: 'pending' }]);
    expanded[p.id] = true;
    editingTitleFor = p.id;
    editingTitleValue = p.title;
    expanded = expanded; // Svelte reactivity
  }

  function toggleExpand(id: string) {
    expanded = { ...expanded, [id]: !expanded[id] };
  }

  function onRun(p: Plan) {
    if (busy) return;
    if (p.steps.length === 0) return;
    if (!p.title.trim()) return;
    if (p.chatLinked) {
      dispatch('continue', { plan: p });
    } else {
      dispatch('run', { plan: p });
    }
  }

  function onStepInput(planId: string, stepId: string, e: Event) {
    const target = e.target as HTMLTextAreaElement;
    updateStep(planId, stepId, { title: target.value });
  }

  function onAddStep(planId: string) {
    addStep(planId, 'Новый шаг');
  }

  function onDelete(plan: Plan) {
    if (!confirm(`Удалить план «${plan.title}»?`)) return;
    if (menuOpenFor === plan.id) menuOpenFor = null;
    deletePlan(plan.id);
  }

  function onDuplicate(plan: Plan) {
    const copy = createPlan(plan.title + ' (копия)', plan.steps.map((s) => ({ ...s })));
    expanded[copy.id] = true;
    expanded = expanded;
  }

  function startRename(plan: Plan) {
    editingTitleFor = plan.id;
    editingTitleValue = plan.title;
  }
  function commitRename() {
    if (editingTitleFor && editingTitleValue.trim()) {
      renamePlan(editingTitleFor, editingTitleValue);
    }
    editingTitleFor = null;
  }
  function cancelRename() {
    editingTitleFor = null;
  }

  function clearCompleted() {
    const done = $plans.filter((p) => planAggregateStatus(p) === 'done');
    if (done.length === 0) return;
    if (!confirm(`Удалить ${done.length} выполненных планов?`)) return;
    for (const p of done) deletePlan(p.id);
  }

  function toggleMenu(id: string) {
    menuOpenFor = menuOpenFor === id ? null : id;
  }

  // Close the per-plan menu when clicking anywhere else.
  function onWindowClick(e: MouseEvent) {
    if (menuOpenFor == null) return;
    const t = e.target as HTMLElement | null;
    if (t && t.closest('.ps-menu-wrap')) return;
    menuOpenFor = null;
  }
  onMount(() => {
    if (typeof window !== 'undefined') {
      window.addEventListener('click', onWindowClick);
    }
  });
  onDestroy(() => {
    if (typeof window !== 'undefined') {
      window.removeEventListener('click', onWindowClick);
    }
  });

  function switchToTasks() {
    dispatch('switch', { mode: 'tasks' });
  }
</script>

<aside class="ps-root">
  <header class="ps-header">
    <div class="ps-header-left">
      <h3>📋 Планы</h3>
      {#if stats.total > 0}
        <span class="ps-badge">{stats.total}</span>
      {/if}
    </div>
    <div class="ps-header-actions">
      <button
        class="ps-icon-btn"
        type="button"
        on:click={newEmptyPlan}
        title="Новый пустой план"
        aria-label="Новый план"
      >+</button>
      <button
        class="ps-icon-btn"
        type="button"
        on:click={switchToTasks}
        title="Переключить на Background Tasks"
        aria-label="Tasks"
      >🧬</button>
    </div>
  </header>

  {#if stats.total > 0}
    <div class="ps-stats">
      {#if stats.running}<span class="ps-stat ps-stat-running">⏳ {stats.running}</span>{/if}
      {#if stats.pending}<span class="ps-stat ps-stat-pending">📋 {stats.pending}</span>{/if}
      {#if stats.error}<span class="ps-stat ps-stat-error">⚠ {stats.error}</span>{/if}
      {#if stats.done}<span class="ps-stat ps-stat-done">✅ {stats.done}</span>{/if}
    </div>
  {/if}

  <div class="ps-list">
    {#if $plans.length === 0}
      <p class="ps-empty">
        Нет планов. Создайте в <b>Plan mode</b> (5-я кнопка в шапке чата)
        или нажмите <button class="ps-link" on:click={newEmptyPlan}>[+]</button>.
      </p>
    {:else}
      {#each sorted as p (p.id)}
        {@const agg = planAggregateStatus(p)}
        {@const isOpen = !!expanded[p.id]}
        <div class="ps-card" class:ps-open={isOpen} class:ps-running={agg === 'running'}>
          <button
            class="ps-card-head"
            type="button"
            on:click={() => toggleExpand(p.id)}
            aria-expanded={isOpen}
          >
            <span class="ps-card-icon">{statusIcon(agg)}</span>
            {#if editingTitleFor === p.id}
              <input
                class="ps-title-input"
                bind:value={editingTitleValue}
                on:click|stopPropagation
                on:keydown={(e) => {
                  if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
                  else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
                }}
                on:blur={commitRename}
                autofocus
              />
            {:else}
              <span class="ps-card-title" title={p.title}>{p.title}</span>
            {/if}
            <span class="ps-card-counter">
              {p.steps.filter((s) => s.status === 'done').length}/{p.steps.length}
            </span>
            <span class="ps-card-chevron">{isOpen ? '▾' : '▸'}</span>
          </button>

          {#if isOpen}
            <div class="ps-card-body">
              {#if p.agentOnly}
                <div class="ps-note">План создан агентом. Редактирование недоступно.</div>
              {/if}

              <ol class="ps-steps">
                {#each p.steps as s, i (s.id)}
                  <li class="ps-step ps-step-{s.status}">
                    <span class="ps-step-marker">{stepStatusIcon(s)}</span>
                    {#if p.agentOnly}
                      <span class="ps-step-text">{s.title}</span>
                    {:else}
                      <textarea
                        class="ps-step-input"
                        value={s.title}
                        rows="1"
                        on:input={(e) => onStepInput(p.id, s.id, e)}
                        on:click|stopPropagation
                      ></textarea>
                    {/if}
                    {#if !p.agentOnly}
                      <div class="ps-step-controls">
                        <button
                          class="ps-mini"
                          type="button"
                          on:click|stopPropagation={() => moveStep(p.id, s.id, -1)}
                          disabled={i === 0}
                          title="Шаг выше"
                          aria-label="Move up"
                        >▲</button>
                        <button
                          class="ps-mini"
                          type="button"
                          on:click|stopPropagation={() => moveStep(p.id, s.id, 1)}
                          disabled={i === p.steps.length - 1}
                          title="Шаг ниже"
                          aria-label="Move down"
                        >▼</button>
                        <button
                          class="ps-mini danger"
                          type="button"
                          on:click|stopPropagation={() => removeStep(p.id, s.id)}
                          disabled={p.steps.length <= 1}
                          title="Удалить шаг"
                          aria-label="Remove step"
                        >×</button>
                      </div>
                    {/if}
                    {#if s.note}
                      <div class="ps-step-note">— {s.note}</div>
                    {/if}
                  </li>
                {/each}
              </ol>

              {#if !p.agentOnly}
                <button
                  class="ps-add-step"
                  type="button"
                  on:click|stopPropagation={() => onAddStep(p.id)}
                >+ шаг</button>
              {/if}

              <div class="ps-card-actions">
                {#if !p.agentOnly}
                  <button
                    class="ps-run"
                    type="button"
                    on:click|stopPropagation={() => onRun(p)}
                    disabled={busy || p.steps.length === 0 || !p.title.trim()}
                    title={busy
                      ? 'Дождитесь завершения текущего ответа'
                      : (p.chatLinked ? 'Продолжить выполнение' : 'Запустить план')}
                  >
                    {p.chatLinked ? '↻ Продолжить' : '▶ Запустить'}
                  </button>
                {/if}
                <div class="ps-menu-wrap">
                  <button
                    class="ps-icon-btn"
                    type="button"
                    on:click|stopPropagation={() => toggleMenu(p.id)}
                    aria-label="Меню"
                    title="Меню"
                  >⋯</button>
                  {#if menuOpenFor === p.id}
                    <div class="ps-menu" on:click|stopPropagation>
                      {#if !p.agentOnly}
                        <button class="ps-menu-item" type="button" on:click={() => { startRename(p); menuOpenFor = null; }}>
                          Переименовать
                        </button>
                        <button class="ps-menu-item" type="button" on:click={() => { onDuplicate(p); menuOpenFor = null; }}>
                          Дублировать
                        </button>
                      {/if}
                      <button class="ps-menu-item danger" type="button" on:click={() => { onDelete(p); menuOpenFor = null; }}>
                        Удалить
                      </button>
                    </div>
                  {/if}
                </div>
              </div>

              <div class="ps-card-meta">
                Обновлён {formatTime(p.updatedAt)}
                {#if p.chatLinked}<span class="ps-link-flag">· запущен</span>{/if}
                {#if p.agentOnly}<span class="ps-agent-flag">· от агента</span>{/if}
              </div>
            </div>
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  {#if stats.done > 0}
    <footer class="ps-footer">
      <button class="ps-clear" type="button" on:click={clearCompleted}>
        Очистить выполненные ({stats.done})
      </button>
    </footer>
  {/if}
</aside>

<style>
  .ps-root {
    width: 280px;
    flex-shrink: 0;
    border-right: 1px solid var(--border, #e3e3e6);
    background: var(--bg-elevated, #fafafa);
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .ps-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ps-header-left { display: flex; align-items: center; gap: 6px; }
  .ps-header h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }
  .ps-header-actions { display: flex; gap: 4px; }
  .ps-icon-btn {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .ps-icon-btn:hover { background: rgba(0, 0, 0, 0.05); }

  .ps-badge {
    display: inline-block;
    background: var(--accent, #4a6fcf);
    color: #fff;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
  }

  .ps-stats {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    font-size: 11px;
  }
  .ps-stat {
    display: inline-flex;
    align-items: center;
    padding: 1px 7px;
    border-radius: 8px;
    background: rgba(0, 0, 0, 0.04);
    color: var(--text-muted, #6b6b70);
  }
  .ps-stat-running { color: #4a6fcf; }
  .ps-stat-error { color: #b03030; }
  .ps-stat-done { color: #1b7a3a; }

  .ps-list {
    flex: 1;
    overflow-y: auto;
    padding: 6px 0;
  }

  .ps-empty {
    padding: 18px 14px;
    font-size: 12px;
    text-align: center;
    color: var(--text-muted, #6b6b70);
    line-height: 1.5;
  }
  .ps-link {
    background: transparent;
    border: 0;
    color: var(--accent, #4a6fcf);
    cursor: pointer;
    font: inherit;
    padding: 0;
  }
  .ps-link:hover { text-decoration: underline; }

  .ps-card {
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ps-card-head {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    background: transparent;
    border: 0;
    cursor: pointer;
    text-align: left;
    font: inherit;
    color: inherit;
  }
  .ps-card-head:hover { background: rgba(0, 0, 0, 0.03); }
  .ps-running > .ps-card-head { background: rgba(74, 111, 207, 0.06); }

  .ps-card-icon { font-size: 14px; flex-shrink: 0; }
  .ps-card-title {
    flex: 1;
    font-size: 13px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ps-card-counter {
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
    flex-shrink: 0;
  }
  .ps-card-chevron {
    font-size: 12px;
    color: var(--text-muted, #6b6b70);
    flex-shrink: 0;
  }

  .ps-title-input {
    flex: 1;
    font: inherit;
    font-size: 13px;
    font-weight: 500;
    border: 1px solid var(--accent, #4a6fcf);
    border-radius: 3px;
    padding: 2px 6px;
    background: var(--bg, #fff);
    color: inherit;
    min-width: 0;
  }

  .ps-card-body {
    padding: 4px 10px 10px 10px;
  }
  .ps-note {
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
    margin: 4px 0 8px 0;
    font-style: italic;
  }

  .ps-steps {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .ps-step {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    padding: 4px 0;
    font-size: 12px;
  }
  .ps-step-marker {
    flex-shrink: 0;
    width: 14px;
    text-align: center;
    color: var(--text-muted, #6b6b70);
    line-height: 1.4;
  }
  .ps-step-done .ps-step-marker { color: #1b7a3a; }
  .ps-step-done .ps-step-text,
  .ps-step-done .ps-step-input { color: var(--text-muted, #6b6b70); text-decoration: line-through; }
  .ps-step-in_progress .ps-step-marker { color: #4a6fcf; }
  .ps-step-error .ps-step-marker { color: #b03030; }
  .ps-step-text { flex: 1; line-height: 1.4; word-break: break-word; }
  .ps-step-input {
    flex: 1;
    border: 1px solid transparent;
    background: transparent;
    resize: none;
    font: inherit;
    font-size: 12px;
    color: inherit;
    padding: 2px 4px;
    border-radius: 3px;
    line-height: 1.4;
    min-height: 22px;
    overflow: hidden;
  }
  .ps-step-input:hover { border-color: var(--border, #d0d0d4); }
  .ps-step-input:focus {
    border-color: var(--accent, #4a6fcf);
    background: var(--bg, #fff);
    outline: none;
  }
  .ps-step-controls {
    display: flex;
    gap: 1px;
    flex-shrink: 0;
  }
  .ps-mini {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 10px;
    padding: 1px 3px;
    border-radius: 3px;
    line-height: 1;
  }
  .ps-mini:hover:not(:disabled) { background: rgba(0, 0, 0, 0.05); color: var(--text, #1c1c1e); }
  .ps-mini:disabled { opacity: 0.3; cursor: not-allowed; }
  .ps-mini.danger:hover { background: rgba(176, 48, 48, 0.1); color: #b03030; }
  .ps-step-note {
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
    flex-basis: 100%;
    padding-left: 20px;
    font-style: italic;
  }

  .ps-add-step {
    margin-top: 4px;
    background: transparent;
    border: 1px dashed var(--border, #d0d0d4);
    color: var(--text-muted, #6b6b70);
    padding: 4px 8px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 11px;
    width: 100%;
  }
  .ps-add-step:hover { border-color: var(--accent, #4a6fcf); color: var(--accent, #4a6fcf); }

  .ps-card-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 8px;
  }
  .ps-run {
    flex: 1;
    background: var(--accent, #4a6fcf);
    color: #fff;
    border: none;
    padding: 6px 10px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    font-family: inherit;
  }
  .ps-run:hover:not(:disabled) { background: var(--accent-strong, #3a5fbf); }
  .ps-run:disabled { opacity: 0.45; cursor: not-allowed; }

  .ps-menu-wrap { position: relative; }
  .ps-menu {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 2px;
    background: var(--bg-elevated, #fff);
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.12);
    min-width: 140px;
    z-index: 10;
  }
  .ps-menu-item {
    display: block;
    width: 100%;
    text-align: left;
    background: transparent;
    border: 0;
    padding: 6px 10px;
    font-size: 12px;
    cursor: pointer;
    color: inherit;
    font-family: inherit;
  }
  .ps-menu-item:hover { background: var(--bg-hover, rgba(0, 0, 0, 0.04)); }
  .ps-menu-item.danger { color: #b03030; }
  .ps-menu-item.danger:hover { background: rgba(176, 48, 48, 0.08); }

  .ps-card-meta {
    margin-top: 6px;
    font-size: 10px;
    color: var(--text-muted, #6b6b70);
  }
  .ps-link-flag { color: #4a6fcf; }
  .ps-agent-flag { color: #b65a00; }

  .ps-footer {
    padding: 8px 12px;
    border-top: 1px solid var(--border, #e3e3e6);
  }
  .ps-clear {
    width: 100%;
    background: transparent;
    border: 1px solid var(--border, #d0d0d4);
    color: var(--text-muted, #6b6b70);
    padding: 5px 8px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 11px;
    font-family: inherit;
  }
  .ps-clear:hover { border-color: #b03030; color: #b03030; }
</style>
