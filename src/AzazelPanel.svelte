<script lang="ts">
  // AzazelPanel — sidebar/watch-pane UI for the browser-use agent.
  //
  // Layout (top → bottom):
  //   1. Header: policy selector, "Run task" button.
  //   2. Tabs: one tab per running task (most recent first).
  //   3. Active tab body: latest screenshot + action timeline.
  //   4. Approval modal: pops when `pending_approval` is set.
  //
  // The screenshot is the single most important signal — it shows
  // what M3 sees at each step. The action timeline (one row per
  // tool call) lets the user understand what the agent is doing.
  // The approval modal is the human-in-the-loop checkpoint for
  // Medium/High risk tools.

  import { onDestroy, onMount } from 'svelte';
  import {
    azazelStore,
    runningTaskIds,
    startAzazelListeners,
    type AzazelAction,
    type AzazelTaskState,
  } from './lib/stores/azazel';
  import {
    azazelRun,
    azazelCancel,
    azazelSetPolicy,
    azazelApprove,
    azazelGetBrowserState,
    type ApprovalPolicy,
    type AzazelApprovalEvent,
  } from './lib/azazel';

  // ---- Local state ----

  /** ID of the tab currently shown. Defaults to the most recent run. */
  let activeTaskId: string | null = null;

  /** Free-form input for the user to start a new task. */
  let newPrompt = '';

  /** "Run" button in flight. */
  let launching = false;

  /** Tab list. */
  $: tasks = Object.values($azazelStore.tasks);
  $: activeTask = activeTaskId
    ? $azazelStore.tasks[activeTaskId] ?? null
    : null;
  $: runningIds = $runningTaskIds;

  // Keep `activeTaskId` valid: jump to the most recent task if
  // the current one is gone or if we don't have one yet.
  $: {
    if (activeTaskId == null || !$azazelStore.tasks[activeTaskId]) {
      activeTaskId = tasks.length > 0 ? tasks[tasks.length - 1].task_id : null;
    }
  }

  // ---- Lifecycle ----

  let stopListeners: (() => void) | null = null;

  onMount(async () => {
    stopListeners = startAzazelListeners();
    // Pull an initial state so the "running X" badge is correct
    // even before the first event lands.
    try {
      const s = await azazelGetBrowserState();
      azazelStore.browser_state.set({
        running_task_count: s.running_task_count,
        last_frame_seq: s.last_frame_seq,
        launched: s.launched,
      });
    } catch (e) {
      // Non-fatal: the badge just stays empty until the first poll.
      console.warn('[AzazelPanel] initial state fetch failed', e);
    }
  });

  onDestroy(() => {
    if (stopListeners) stopListeners();
  });

  // ---- Actions ----

  async function runNewTask() {
    const prompt = newPrompt.trim();
    if (!prompt || launching) return;
    launching = true;
    try {
      const id = await azazelRun({ prompt, title: prompt.slice(0, 60) });
      newPrompt = '';
      // Switch to the new tab.
      activeTaskId = id;
    } catch (e) {
      console.error('[AzazelPanel] run failed', e);
      alert(`Failed to start Azazel task: ${e}`);
    } finally {
      launching = false;
    }
  }

  async function cancel(taskId: string) {
    if (!confirm('Cancel this Azazel task? Any progress is lost.')) return;
    try {
      await azazelCancel(taskId);
    } catch (e) {
      console.error('[AzazelPanel] cancel failed', e);
    }
  }

  async function approve(decision: 'approve' | 'reject' | 'approve_always_for_session') {
    const p = $azazelStore.pending_approval;
    if (!p) return;
    try {
      await azazelApprove(p.task_id, decision);
      azazelStore.pending_approval.set(null);
    } catch (e) {
      console.error('[AzazelPanel] approve failed', e);
    }
  }

  async function setPolicy(p: ApprovalPolicy) {
    azazelStore.policy.set(p);
    try {
      await azazelSetPolicy(p);
    } catch (e) {
      console.error('[AzazelPanel] setPolicy failed', e);
    }
  }

  function formatTime(iso: string): string {
    try {
      const d = new Date(iso);
      return d.toLocaleTimeString();
    } catch {
      return iso;
    }
  }

  function riskColor(risk: AzazelApprovalEvent['risk']): string {
    if (risk === 'high') return '#ff5b5b';
    if (risk === 'medium') return '#f0a500';
    return '#7fb069';
  }
</script>

<section class="azazel-panel">
  <!-- ===== Header ===== -->
  <header>
    <h2>Azazel</h2>
    <span class="subtitle">autonomous browser agent</span>
  </header>

  <div class="controls">
    <textarea
      rows="2"
      placeholder="What should Azazel do? E.g. 'Log in to example.com and post a hello message.'"
      bind:value={newPrompt}
    ></textarea>
    <div class="row">
      <button
        class="primary"
        on:click={runNewTask}
        disabled={launching || newPrompt.trim().length === 0}
      >
        {launching ? 'Launching…' : '▶ Run task'}
      </button>
      <select
        aria-label="Approval policy"
        value={$azazelStore.policy}
        on:change={(e) => setPolicy(e.currentTarget.value)}
      >
        <option value="strict">Strict (approve Medium + High)</option>
        <option value="normal">Normal (approve High)</option>
        <option value="yolo">Yolo (no approvals)</option>
      </select>
    </div>
  </div>

  <!-- ===== Tabs ===== -->
  {#if tasks.length > 0}
    <div class="tabs" role="tablist">
      {#each tasks.slice(-6) as t (t.task_id)}
        <button
          class="tab"
          class:active={t.task_id === activeTaskId}
          class:done={!t.running}
          role="tab"
          aria-selected={t.task_id === activeTaskId}
          on:click={() => (activeTaskId = t.task_id)}
          title={`Task ${t.task_id}`}
        >
          {t.running ? '●' : '✓'} {t.task_id.slice(0, 8)}
        </button>
      {/each}
    </div>
  {/if}

  <!-- ===== Active tab body ===== -->
  {#if activeTask}
    <div class="task-body">
      <div class="task-header">
        <div class="task-title">
          <span class="status" class:running={activeTask.running} class:done={!activeTask.running}>
            {activeTask.running ? '● running' : '✓ done'}
          </span>
          <code class="task-id">{activeTask.task_id}</code>
        </div>
        {#if activeTask.running}
          <button class="danger" on:click={() => cancel(activeTask.task_id)}>
            Cancel
          </button>
        {/if}
      </div>

      {#if activeTask.final_summary || activeTask.final_error}
        <div class="result" class:ok={!activeTask.final_error}>
          {activeTask.final_error ? '❌ ' : ''}{activeTask.final_summary || activeTask.final_error}
        </div>
      {/if}

      <!-- Screenshot (or placeholder). -->
      <div class="screenshot-wrap">
        {#if activeTask.latest_screenshot}
          <img
            class="screenshot"
            src={activeTask.latest_screenshot}
            alt="Latest browser screenshot"
          />
        {:else}
          <div class="screenshot placeholder">No screenshot yet.</div>
        {/if}
      </div>

      <!-- Action timeline (most recent first). -->
      <h4>Actions</h4>
      <div class="timeline">
        {#each [...activeTask.actions].reverse() as a (a.ts + a.step_n)}
          <div class="action" class:error={a.is_error}>
            <span class="ts">{formatTime(a.ts)}</span>
            <span class="tool">{a.tool}</span>
            {#if a.preview}
              <span class="preview">{a.preview}</span>
            {/if}
          </div>
        {/each}
        {#if activeTask.actions.length === 0}
          <div class="empty">No actions yet.</div>
        {/if}
      </div>
    </div>
  {:else}
    <p class="empty-state">
      No Azazel tasks yet. Type a goal above and click <b>Run task</b>.
    </p>
  {/if}

  <!-- ===== Approval modal ===== -->
  {#if $azazelStore.pending_approval}
    {@const p = $azazelStore.pending_approval}
    <div
      class="modal-backdrop"
      role="dialog"
      aria-modal="true"
      on:click|self={() => approve('reject')}
    >
      <div class="modal" on:click|stopPropagation>
        <header>
          <h3>⚠ Azazel wants your approval</h3>
          <span
            class="risk-badge"
            style="background: {riskColor(p.risk)};"
            title="Risk level"
          >{p.risk}</span>
        </header>
        <p class="prompt">{p.prompt_text}</p>
        <div class="args">
          <strong>Tool:</strong> <code>{p.tool_name}</code>
          <pre>{JSON.stringify(p.tool_args, null, 2)}</pre>
        </div>
        <div class="actions">
          <button class="danger" on:click={() => approve('reject')}>Reject</button>
          <button class="warn" on:click={() => approve('approve_always_for_session')}>
            Approve for session
          </button>
          <button class="primary" on:click={() => approve('approve')}>Approve</button>
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  .azazel-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: #14141a;
    color: #e7e7ea;
    font-size: 13px;
  }
  header {
    padding: 12px 16px 6px;
    border-bottom: 1px solid #2a2a32;
  }
  header h2 {
    margin: 0;
    font-size: 18px;
    letter-spacing: 0.5px;
  }
  .subtitle {
    color: #7a7a85;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 1.5px;
  }
  .controls {
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .controls textarea {
    width: 100%;
    background: #1c1c24;
    color: inherit;
    border: 1px solid #2a2a32;
    border-radius: 6px;
    padding: 8px;
    font: inherit;
    resize: vertical;
  }
  .controls .row {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .controls .row > button { flex: 0 0 auto; }
  .controls select { flex: 1; }
  button {
    background: #2a2a32;
    color: inherit;
    border: 1px solid #3a3a44;
    border-radius: 6px;
    padding: 6px 10px;
    font: inherit;
    cursor: pointer;
  }
  button:hover { background: #3a3a44; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  button.primary { background: #4a7cff; border-color: #4a7cff; }
  button.primary:hover { background: #6a92ff; }
  button.danger { background: #b13b3b; border-color: #b13b3b; }
  button.warn { background: #b17a3b; border-color: #b17a3b; }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 8px 12px 0;
    overflow-x: auto;
    border-bottom: 1px solid #2a2a32;
  }
  .tab {
    background: transparent;
    border: 1px solid transparent;
    color: #9a9aa5;
    padding: 6px 10px;
    border-radius: 6px 6px 0 0;
    white-space: nowrap;
    font-size: 12px;
  }
  .tab.active { background: #1c1c24; border-color: #2a2a32; color: #fff; }
  .tab.done { color: #7fb069; }
  .task-body {
    padding: 12px 16px;
    overflow-y: auto;
    flex: 1;
  }
  .task-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }
  .task-title { display: flex; gap: 8px; align-items: baseline; }
  .status { font-weight: 600; font-size: 12px; }
  .status.running { color: #f0a500; }
  .status.done { color: #7fb069; }
  .task-id { color: #7a7a85; font-size: 11px; }
  .result {
    padding: 8px 10px;
    border-radius: 6px;
    background: #2a1f2a;
    border: 1px solid #5a2a3a;
    margin-bottom: 12px;
    white-space: pre-wrap;
  }
  .result.ok { background: #1f2a1f; border-color: #2a5a3a; }
  .screenshot-wrap {
    margin-bottom: 12px;
    border: 1px solid #2a2a32;
    border-radius: 6px;
    overflow: hidden;
    background: #000;
  }
  .screenshot { width: 100%; display: block; }
  .screenshot.placeholder {
    aspect-ratio: 16/9;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #5a5a65;
  }
  h4 {
    margin: 0 0 6px;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 1.2px;
    color: #9a9aa5;
  }
  .timeline { display: flex; flex-direction: column; gap: 2px; }
  .action {
    display: grid;
    grid-template-columns: 60px 140px 1fr;
    gap: 8px;
    padding: 4px 6px;
    border-bottom: 1px solid #1a1a22;
    font-size: 12px;
  }
  .action.error { background: rgba(177, 59, 59, 0.15); }
  .ts { color: #7a7a85; }
  .tool { font-family: monospace; color: #f0a500; }
  .preview { color: #c0c0c8; overflow: hidden; text-overflow: ellipsis; }
  .empty { color: #5a5a65; padding: 12px; text-align: center; }
  .empty-state { color: #7a7a85; padding: 16px; text-align: center; }

  /* ---- Approval modal ---- */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9999;
  }
  .modal {
    background: #1c1c24;
    border: 1px solid #2a2a32;
    border-radius: 8px;
    padding: 20px;
    min-width: 360px;
    max-width: 520px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .modal header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0;
    border: none;
  }
  .modal h3 { margin: 0; flex: 1; }
  .risk-badge {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    color: #fff;
    text-transform: uppercase;
    letter-spacing: 1px;
  }
  .prompt { margin: 0; line-height: 1.4; color: #c0c0c8; }
  .args { font-size: 12px; }
  .args pre {
    background: #14141a;
    border: 1px solid #2a2a32;
    border-radius: 4px;
    padding: 6px 8px;
    max-height: 160px;
    overflow: auto;
    margin: 4px 0 0;
  }
  .modal .actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
</style>
