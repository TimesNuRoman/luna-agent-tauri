<script lang="ts">
  import { onMount } from 'svelte';
  import {
    selfInspect,
    getActiveVersion,
    snapshotList,
    snapshotCreate,
    snapshotRestore,
    snapshotDelete,
    snapshotMarkImportant,
    selfDiagnose,
    selfPlan,
    sandboxCreate,
    sandboxApply,
    sandboxRun,
    sandboxSmoke,
    sandboxCollect,
    sandboxDiscard,
    applySelfUpdate,
    rollbackSelfUpdate,
    feedbackSubmit,
    feedbackList,
    formatBytes,
    shortSha,
    sourceRootLabel,
    severityClass,
    severityLabel,
    riskColor,
    type SelfInfo,
    type ActiveVersion,
    type SnapshotInfo,
    type CreateResult,
    type Issue,
    type DiagnoseResult,
    type Plan,
    type PlanRequest,
    type Severity,
    type SandboxReport,
    type RunResult,
    type UpdateResult,
    type RollbackResult,
    type FeedbackEntry,
    type FeedbackCategory,
  } from './lib/selfEvolver';

  // --- State ---
  let info: SelfInfo | null = null;
  let active: ActiveVersion | null = null;
  let loading = false;
  let error: string | null = null;
  let lastRefreshedAt: Date | null = null;

  // --- Snapshot state (Phase E1) ---
  let snapshots: SnapshotInfo[] = [];
  let snapLoading = false;
  let snapError: string | null = null;
  let snapBusy: string | null = null; // id of a snapshot currently being acted on
  let snapCreateLabel = '';
  let snapCreateImportant = false;
  let snapCreating = false;
  // Restore flow
  let restoreTarget: SnapshotInfo | null = null;
  let restoreFeedback = '';
  let restoreSubmitting = false;

  // --- Diagnose/plan state (Phase E2) ---
  let diagnoseResult: DiagnoseResult | null = null;
  let diagnoseRunning = false;
  let selectedIssueIds = new Set<string>();
  let currentPlan: Plan | null = null;
  let planRunning = false;
  let diagnoseError: string | null = null;

  // --- Sandbox state (Phase E3) ---
  let sandboxId: string | null = null;
  let sandboxPath: string | null = null;
  let sandboxRunning = false;
  let sandboxStage = ''; // 'create' | 'apply' | 'build' | 'test' | 'smoke' | 'collect'
  let sandboxReport: SandboxReport | null = null;
  let sandboxError: string | null = null;

  // --- Apply / Rollback state (Phase E4) ---
  let applying = false;
  let updateResult: UpdateResult | null = null;
  // Rollback modal
  let rollbackTarget: SnapshotInfo | null = null;
  let rollbackFeedback = '';
  let rollbackSubmitting = false;
  // Feedback form
  let feedbackCategory: FeedbackCategory = 'bug';
  let feedbackMessage = '';
  let feedbackSubmitting = false;
  let feedbackListCache: FeedbackEntry[] = [];

  async function runApply() {
    if (!currentPlan || applying) return;
    if (!confirm(
      `Apply this plan to the PRODUCTION source tree?\n\n` +
      `Steps: ${currentPlan.steps.length}\nRisk: ${currentPlan.risk_score.toFixed(2)}\n\n` +
      `A pre-update snapshot will be taken automatically.`,
    )) return;
    applying = true;
    try {
      const r = await applySelfUpdate(currentPlan.id, currentPlan.steps);
      updateResult = r;
      if (r.smoke_passed) {
        await refresh();
        if (r.needs_restart) {
          alert(
            `Update applied successfully!\n\n` +
            `New version: ${r.new_version}\n` +
            `Backup binary: <exe>.prev-<ts>\n` +
            `Pre-update snapshot: ${r.pre_update_snapshot_id}\n\n` +
            `⚠ Please restart Luna to load the new binary.`,
          );
        }
      } else {
        alert(`Update FAILED.\n\n${r.error ?? 'unknown error'}\n\nPre-update snapshot: ${r.pre_update_snapshot_id} — you can roll back to it.`);
      }
    } catch (e) {
      alert(`Apply failed: ${e}`);
    } finally {
      applying = false;
    }
  }

  function openRollback(s: SnapshotInfo) {
    rollbackTarget = s;
    rollbackFeedback = '';
  }

  function cancelRollback() {
    rollbackTarget = null;
    rollbackFeedback = '';
  }

  async function submitRollback() {
    if (!rollbackTarget || rollbackSubmitting) return;
    if (rollbackFeedback.trim().length < 5) {
      alert('Feedback must be at least 5 characters.');
      return;
    }
    if (!confirm(`Roll back to ${rollbackTarget.id}?\n\nThis will rebuild and swap the binary.\nYour feedback will be saved.`)) return;
    rollbackSubmitting = true;
    try {
      const r = await rollbackSelfUpdate(rollbackTarget.id, rollbackFeedback);
      if (r.smoke_passed) {
        alert(`Rollback complete!\n\nRestored from: ${r.restored_from}\nFeedback id: ${r.feedback_id}\n\nRestart Luna to load the rolled-back binary.`);
        rollbackTarget = null;
        rollbackFeedback = '';
        await refresh();
        await refreshSnapshots();
        await refreshFeedback();
      } else {
        alert(`Rollback FAILED.\n\n${r.error ?? 'unknown error'}`);
      }
    } catch (e) {
      alert(`Rollback failed: ${e}`);
    } finally {
      rollbackSubmitting = false;
    }
  }

  async function submitFeedback() {
    if (feedbackSubmitting) return;
    if (feedbackMessage.trim().length < 5) {
      alert('Feedback must be at least 5 characters.');
      return;
    }
    feedbackSubmitting = true;
    try {
      await feedbackSubmit(feedbackCategory, feedbackMessage);
      feedbackMessage = '';
      await refreshFeedback();
    } catch (e) {
      alert(`Feedback submit failed: ${e}`);
    } finally {
      feedbackSubmitting = false;
    }
  }

  async function refreshFeedback() {
    try {
      feedbackListCache = await feedbackList('all');
    } catch (e) {
      console.error('[SelfEvolution] feedbackList failed:', e);
    }
  }

  async function runSandbox() {
    if (!currentPlan || sandboxRunning) return;
    sandboxRunning = true;
    sandboxError = null;
    sandboxReport = null;
    sandboxId = null;
    sandboxPath = null;
    try {
      // 1. create
      sandboxStage = 'create';
      const c = await sandboxCreate();
      sandboxId = c.sandbox_id;
      sandboxPath = c.path;
      console.info('[SelfEvolution] sandbox created:', c.sandbox_id, c.path);

      // 2. apply
      sandboxStage = 'apply';
      await sandboxApply(c.sandbox_id, currentPlan);
      console.info('[SelfEvolution] plan applied to sandbox');

      // 3. run cargo build
      sandboxStage = 'build';
      const buildRes = await sandboxRun(c.sandbox_id, 'cargo build --release');
      console.info('[SelfEvolution] build:', buildRes.verdict, buildRes.duration_ms, 'ms');
      if (buildRes.verdict !== 'pass') {
        sandboxError = `cargo build failed (exit ${buildRes.exit_code}). See stderr below.`;
        sandboxRunning = false;
        return;
      }

      // 4. run cargo test (best-effort, non-blocking on failure)
      sandboxStage = 'test';
      try {
        const testRes = await sandboxRun(c.sandbox_id, 'cargo test --release');
        console.info('[SelfEvolution] test:', testRes.verdict, testRes.duration_ms, 'ms');
        if (testRes.verdict !== 'pass') {
          console.warn('[SelfEvolution] cargo test failed; continuing to smoke');
        }
      } catch (e) {
        console.warn('[SelfEvolution] cargo test error (non-fatal):', e);
      }

      // 5. smoke
      sandboxStage = 'smoke';
      const smokeRes = await sandboxSmoke(c.sandbox_id);
      console.info('[SelfEvolution] smoke:', smokeRes.passed, smokeRes.duration_ms, 'ms');

      // 6. collect final report
      sandboxStage = 'collect';
      sandboxReport = await sandboxCollect(c.sandbox_id);
      console.info('[SelfEvolution] sandbox report verdict:', sandboxReport.verdict);
    } catch (e) {
      sandboxError = String(e);
      console.error('[SelfEvolution] sandbox flow failed:', e);
    } finally {
      sandboxStage = '';
      sandboxRunning = false;
    }
  }

  async function discardSandbox() {
    if (!sandboxId) return;
    if (!confirm('Discard the sandbox? All applied changes in the temp dir will be lost.')) return;
    try {
      await sandboxDiscard(sandboxId);
      sandboxId = null;
      sandboxPath = null;
      sandboxReport = null;
    } catch (e) {
      sandboxError = String(e);
    }
  }

  async function runDiagnose() {
    if (diagnoseRunning) return;
    diagnoseRunning = true;
    diagnoseError = null;
    try {
      const r = await selfDiagnose('all');
      diagnoseResult = r;
      // Pre-select all `high` and `crit` issues, leave the rest unchecked.
      selectedIssueIds = new Set(
        r.issues.filter((i) => i.severity === 'high' || i.severity === 'crit').map((i) => i.id),
      );
      currentPlan = null; // invalidate any previous plan
    } catch (e) {
      diagnoseError = String(e);
      console.error('[SelfEvolution] selfDiagnose failed:', e);
    } finally {
      diagnoseRunning = false;
    }
  }

  function toggleIssue(id: string) {
    const next = new Set(selectedIssueIds);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedIssueIds = next;
  }

  function selectAllIssues() {
    if (!diagnoseResult) return;
    selectedIssueIds = new Set(diagnoseResult.issues.map((i) => i.id));
  }

  function deselectAllIssues() {
    selectedIssueIds = new Set();
  }

  async function buildPlan() {
    if (!diagnoseResult || planRunning) return;
    if (selectedIssueIds.size === 0) {
      diagnoseError = 'Select at least one issue to plan for.';
      return;
    }
    planRunning = true;
    diagnoseError = null;
    try {
      const ids = Array.from(selectedIssueIds);
      const plan = await selfPlan(ids, diagnoseResult.issues, diagnoseResult.id);
      currentPlan = plan;
    } catch (e) {
      diagnoseError = String(e);
      console.error('[SelfEvolution] selfPlan failed:', e);
    } finally {
      planRunning = false;
    }
  }

  async function refresh() {
    loading = true;
    error = null;
    try {
      // Two cheap calls; both read-only. The first one (self_inspect)
      // already includes `active` (built from active.json), but we
      // also call get_active_version separately so the UI can show a
      // "current version" badge independent of git_sha presence.
      const [i, a] = await Promise.all([selfInspect(), getActiveVersion()]);
      info = i;
      active = a;
      lastRefreshedAt = new Date();
    } catch (e) {
      error = String(e);
      console.error('[SelfEvolution] refresh failed:', e);
    } finally {
      loading = false;
    }
  }

  async function refreshSnapshots() {
    snapLoading = true;
    snapError = null;
    try {
      snapshots = await snapshotList();
    } catch (e) {
      snapError = String(e);
      console.error('[SelfEvolution] snapshotList failed:', e);
    } finally {
      snapLoading = false;
    }
  }

  async function onCreateSnapshot() {
    if (snapCreating) return;
    snapCreating = true;
    snapError = null;
    try {
      const res: CreateResult = await snapshotCreate(
        snapCreateLabel.trim() || undefined,
        snapCreateImportant,
      );
      console.info('[SelfEvolution] snapshot created:', res.info.id, 'gc_deleted=', res.gc_deleted.length);
      snapCreateLabel = '';
      snapCreateImportant = false;
      await refreshSnapshots();
    } catch (e) {
      snapError = String(e);
      console.error('[SelfEvolution] snapshotCreate failed:', e);
    } finally {
      snapCreating = false;
    }
  }

  async function onToggleImportant(s: SnapshotInfo) {
    snapBusy = s.id;
    try {
      await snapshotMarkImportant(s.id, !s.important);
      await refreshSnapshots();
    } catch (e) {
      snapError = String(e);
    } finally {
      snapBusy = null;
    }
  }

  async function onDelete(s: SnapshotInfo) {
    if (!confirm(`Delete snapshot "${s.label || s.id}"?\n\n${formatBytes(s.total_size)} freed.\n\nThis cannot be undone.`)) {
      return;
    }
    snapBusy = s.id;
    try {
      const r = await snapshotDelete(s.id);
      if (!r.deleted) {
        snapError = `Cannot delete: ${r.reason ?? 'unknown reason'}`;
      }
      await refreshSnapshots();
    } catch (e) {
      snapError = String(e);
    } finally {
      snapBusy = null;
    }
  }

  function openRestore(s: SnapshotInfo) {
    restoreTarget = s;
    restoreFeedback = '';
  }

  function cancelRestore() {
    restoreTarget = null;
    restoreFeedback = '';
  }

  async function submitRestore() {
    if (!restoreTarget) return;
    if (restoreFeedback.trim().length < 5) {
      snapError = 'Feedback must be at least 5 characters.';
      return;
    }
    restoreSubmitting = true;
    try {
      const r = await snapshotRestore(restoreTarget.id, restoreFeedback);
      console.info(
        '[SelfEvolution] restored from',
        r.restored_from,
        'pre-restore snap:',
        r.pre_restore_snap_id,
        'needs_rebuild=',
        r.needs_rebuild,
      );
      restoreTarget = null;
      restoreFeedback = '';
      await refreshSnapshots();
      alert(
        `Restored from ${r.restored_from}.\n\n` +
        `${r.files_written} files written.\n` +
        `Pre-restore safety snapshot: ${r.pre_restore_snap_id}\n\n` +
        `⚠ Phase E1 does not rebuild — run \`cargo build\` (or \`tauri-build.cmd\`) yourself.`,
      );
    } catch (e) {
      snapError = String(e);
    } finally {
      restoreSubmitting = false;
    }
  }

  onMount(() => {
    refresh();
    refreshSnapshots();
    refreshFeedback();
  });

  // Derived UI bits
  $: phaseLabel = (() => {
    if (!info) return '—';
    const c = info.capabilities;
    const stages: string[] = [];
    if (c.self_inspect) stages.push('E0 inspect');
    if (c.snapshots) stages.push('E1 snap');
    if (c.diagnose) stages.push('E2 diagnose');
    if (c.sandbox) stages.push('E3 sandbox');
    if (c.apply_update) stages.push('E4 apply');
    return stages.length > 0 ? stages.join(' · ') : 'pre-E0';
  })();

  $: totalSnapBytes = snapshots.reduce((a, s) => a + s.total_size, 0);
  $: importantCount = snapshots.filter((s) => s.important).length;

  function formatTs(iso: string): string {
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }
</script>

<section class="se-root">
  <header class="se-header">
    <div>
      <h1>🧬 Self-evolution</h1>
      <p class="muted">
        Read-only introspection of Luna Agent's own source code, build, and version state.
        Future phases (E1-E5) will add snapshots, sandbox, AI-driven diagnose, and apply/rollback.
      </p>
    </div>
    <button class="se-refresh" on:click={refresh} disabled={loading}>
      {loading ? 'Refreshing…' : 'Refresh'}
    </button>
  </header>

  {#if error}
    <div class="se-banner se-error" role="alert">
      <strong>Error:</strong> {error}
    </div>
  {/if}

  {#if info}
    <div class="se-grid">
      <!-- Version card -->
      <article class="se-card">
        <h2>Version</h2>
        <dl>
          <dt>App version</dt>
          <dd><code>{info.version}</code></dd>
          <dt>Identifier</dt>
          <dd><code>{info.identifier}</code></dd>
          <dt>Phase</dt>
          <dd>{phaseLabel}</dd>
        </dl>
      </article>

      <!-- Source card -->
      <article class="se-card">
        <h2>Source</h2>
        <dl>
          <dt>Source root</dt>
          <dd>
            {#if info.source_root}
              <code title={info.source_root}>{info.source_root}</code>
            {:else}
              <span class="muted">—</span>
            {/if}
          </dd>
          <dt>Resolved via</dt>
          <dd>{sourceRootLabel(info.source_root_source)}</dd>
          <dt>Git SHA</dt>
          <dd><code>{shortSha(info.git_sha)}</code></dd>
          <dt>Git dirty</dt>
          <dd>
            {#if info.git_dirty === null}
              <span class="muted">—</span>
            {:else if info.git_dirty}
              <span class="warn">● yes</span>
            {:else}
              <span class="ok">○ clean</span>
            {/if}
          </dd>
        </dl>
      </article>

      <!-- Build card -->
      <article class="se-card">
        <h2>Build</h2>
        <dl>
          <dt>Build host</dt>
          <dd><code>{info.build_host}</code></dd>
          <dt>Exe path</dt>
          <dd>
            {#if info.exe_path}
              <code title={info.exe_path}>{info.exe_path}</code>
            {:else}
              <span class="muted">—</span>
            {/if}
          </dd>
          <dt>Source files</dt>
          <dd>{info.source_files ?? '—'}</dd>
          <dt>Source size</dt>
          <dd>{formatBytes(info.source_bytes)}</dd>
        </dl>
      </article>

      <!-- Active version card -->
      <article class="se-card">
        <h2>Active version</h2>
        {#if active}
          <dl>
            <dt>Version</dt>
            <dd><code>{active.version}</code></dd>
            <dt>Git SHA</dt>
            <dd><code>{shortSha(active.git_sha)}</code></dd>
            <dt>Built at</dt>
            <dd>
              {#if active.build_ts}
                {new Date(active.build_ts).toLocaleString()}
              {:else}
                <span class="muted">—</span>
              {/if}
            </dd>
            <dt>Snapshot</dt>
            <dd>
              {#if active.snapshot_id}
                <code>{active.snapshot_id}</code>
              {:else}
                <span class="muted">—</span>
              {/if}
            </dd>
          </dl>
        {:else}
          <p class="muted">
            No active version recorded. Luna has never been updated via self-evolution —
            you are running the originally installed build.
          </p>
        {/if}
      </article>

      <!-- Capabilities card -->
      <article class="se-card wide">
        <h2>Capabilities</h2>
        <ul class="se-caps">
          <li class:on={info.capabilities.self_inspect}>
            <span class="dot" /> E0 — self_inspect
          </li>
          <li class:on={info.capabilities.snapshots}>
            <span class="dot" /> E1 — snapshots
          </li>
          <li class:on={info.capabilities.diagnose}>
            <span class="dot" /> E2 — diagnose / plan
          </li>
          <li class:on={info.capabilities.sandbox}>
            <span class="dot" /> E3 — sandbox
          </li>
          <li class:on={info.capabilities.apply_update}>
            <span class="dot" /> E4 — apply / rollback
          </li>
        </ul>
      </article>

      <!-- Diagnose card (Phase E2) -->
      <article class="se-card wide">
        <header class="se-card-head">
          <h2>Diagnose &amp; plan</h2>
          <div class="muted se-dx-summary">
            {#if diagnoseResult}
              {diagnoseResult.issues.length} issues · {diagnoseResult.mode} · {diagnoseResult.latency_ms}ms
            {:else}
              Static scan + (optional) LLM review.
            {/if}
          </div>
        </header>

        <div class="se-dx-actions">
          <button class="se-btn primary" on:click={runDiagnose} disabled={diagnoseRunning}>
            {diagnoseRunning ? 'Running…' : 'Run self-diagnosis'}
          </button>
          {#if diagnoseResult}
            <button class="se-btn" on:click={selectAllIssues}>Select all</button>
            <button class="se-btn" on:click={deselectAllIssues}>Deselect all</button>
            <button
              class="se-btn primary"
              on:click={buildPlan}
              disabled={planRunning || selectedIssueIds.size === 0}>
              {planRunning ? 'Planning…' : `Plan (${selectedIssueIds.size} selected)`}
            </button>
          {/if}
        </div>

        {#if diagnoseError}
          <div class="se-banner se-error" role="alert">
            <strong>Error:</strong> {diagnoseError}
          </div>
        {/if}

        {#if diagnoseResult && diagnoseResult.llm_error}
          <div class="se-banner se-warn" role="status">
            <strong>LLM step:</strong> {diagnoseResult.llm_error}
          </div>
        {/if}

        {#if diagnoseResult && diagnoseResult.issues.length > 0}
          <table class="se-issue-table">
            <thead>
              <tr>
                <th class="cb">Pick</th>
                <th>Sev</th>
                <th>Category</th>
                <th>File / line</th>
                <th>Hint</th>
                <th>Source</th>
              </tr>
            </thead>
            <tbody>
              {#each diagnoseResult.issues as i (i.id)}
                <tr class={severityClass(i.severity)}>
                  <td class="cb">
                    <input
                      type="checkbox"
                      checked={selectedIssueIds.has(i.id)}
                      on:change={() => toggleIssue(i.id)}
                    />
                  </td>
                  <td><span class="sev-pill {severityClass(i.severity)}">{severityLabel(i.severity)}</span></td>
                  <td><code class="cat">{i.category}</code></td>
                  <td>
                    {#if i.file}
                      <code class="loc">{i.file}{i.line ? `:${i.line}` : ''}</code>
                    {:else}
                      <span class="muted">—</span>
                    {/if}
                  </td>
                  <td>{i.hint}</td>
                  <td><span class="src-pill">{i.source}</span></td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else if diagnoseResult}
          <p class="muted">No issues found. 🎉 (Either the code is clean, or the scan rules are too narrow.)</p>
        {/if}

        {#if currentPlan}
          <header class="se-card-head" style="margin-top:18px">
            <h2>Plan</h2>
            <div class="muted">
              {currentPlan.steps.length} steps · risk
              <strong style="color:{riskColor(currentPlan.risk_score)}">
                {currentPlan.risk_score.toFixed(2)}
              </strong>
              · {currentPlan.mode}
            </div>
          </header>

          {#if currentPlan.expected_impact}
            <p class="muted se-plan-impact">{currentPlan.expected_impact}</p>
          {/if}

          {#if currentPlan.steps.length === 0}
            <p class="muted">
              Plan is empty. {currentPlan.mode === 'trivial'
                ? 'Set an Anthropic API key in Settings to enable LLM planning.'
                : 'The LLM chose not to propose any steps for these issues.'}
            </p>
          {:else}
            <ol class="se-step-list">
              {#each currentPlan.steps as step, idx}
                <li class="se-step">
                  <div class="se-step-head">
                    <code class="se-step-kind">{step.kind}</code>
                    <span class="se-step-idx">#{idx + 1}</span>
                  </div>
                  {#if step.kind === 'edit_file'}
                    <code class="se-step-target">{step.path}</code>
                    <p class="se-step-rationale">{step.rationale}</p>
                  {:else if step.kind === 'create_file'}
                    <code class="se-step-target">{step.path}</code>
                    <p class="se-step-rationale">{step.rationale}</p>
                  {:else if step.kind === 'run_command'}
                    <code class="se-step-target">{step.command}</code>
                    <p class="se-step-rationale">{step.rationale}</p>
                  {/if}
                </li>
              {/each}
            </ol>

            <!-- Sandbox controls (Phase E3) -->
            <div class="se-sandbox-actions">
              <button
                class="se-btn primary"
                on:click={runSandbox}
                disabled={sandboxRunning}>
                {sandboxRunning ? `Sandbox ${sandboxStage}…` : 'Try in sandbox'}
              </button>
              {#if sandboxId}
                <button class="se-btn danger" on:click={discardSandbox} disabled={sandboxRunning}>
                  Discard sandbox
                </button>
                <code class="muted se-sandbox-path" title={sandboxPath ?? ''}>
                  {sandboxId}
                </code>
              {/if}
            </div>

            <!-- Apply controls (Phase E4) -->
            {#if sandboxReport && sandboxReport.verdict === 'pass'}
              <div class="se-apply-bar">
                <button
                  class="se-btn primary"
                  on:click={runApply}
                  disabled={applying}>
                  {applying ? 'Applying…' : 'Apply update to production'}
                </button>
                <span class="muted">
                  Sandbox passed. This will rebuild, run --smoke, and atomic-swap the binary.
                </span>
              </div>
            {/if}

            {#if updateResult}
              <div class="se-update-result" data-success={updateResult.smoke_passed}>
                {#if updateResult.smoke_passed}
                  <strong>✓ Update applied.</strong>
                  Version: <code>{updateResult.new_version}</code> ·
                  Pre-update snapshot: <code>{updateResult.pre_update_snapshot_id}</code>
                  {#if updateResult.needs_restart}
                    · <strong>Restart Luna to load the new binary.</strong>
                  {/if}
                {:else}
                  <strong>✗ Update failed.</strong>
                  {updateResult.error}
                  <br />Pre-update snapshot: <code>{updateResult.pre_update_snapshot_id}</code> — you can roll back to it.
                {/if}
              </div>
            {/if}

            {#if sandboxError}
              <div class="se-banner se-error" role="alert">
                <strong>Sandbox error:</strong> {sandboxError}
              </div>
            {/if}

            {#if sandboxReport}
              <div class="se-sandbox-report" data-verdict={sandboxReport.verdict}>
                <header>
                  <strong>Verdict:</strong>
                  <span class="se-verdict se-verdict-{sandboxReport.verdict}">{sandboxReport.verdict}</span>
                  <span class="muted"> · {Math.round(sandboxReport.total_elapsed_ms / 1000)}s total</span>
                </header>

                {#if sandboxReport.commands.length > 0}
                  <h4>Commands</h4>
                  <table class="se-cmd-table">
                    <thead>
                      <tr><th>command</th><th>exit</th><th>ms</th><th>verdict</th></tr>
                    </thead>
                    <tbody>
                      {#each sandboxReport.commands as c}
                        <tr>
                          <td><code>{c.command}</code></td>
                          <td>{c.exit_code}</td>
                          <td>{c.duration_ms}</td>
                          <td><span class="se-verdict se-verdict-{c.verdict}">{c.verdict}</span></td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                {/if}

                {#if sandboxReport.smoke}
                  <h4>Smoke</h4>
                  <div class="se-smoke" data-passed={sandboxReport.smoke.passed}>
                    <strong>{sandboxReport.smoke.passed ? '✓ passed' : '✗ failed'}</strong>
                    {#if sandboxReport.smoke.failure_reason}
                      — {sandboxReport.smoke.failure_reason}
                    {/if}
                    <span class="muted"> · {sandboxReport.smoke.duration_ms}ms</span>
                  </div>
                {/if}

                <p class="muted se-plan-foot">
                  ⓘ Phase E3 verifies in sandbox; <strong>apply to production</strong> lands in Phase E4.
                </p>
              </div>
            {/if}
          {/if}
        {/if}
      </article>

      <!-- Snapshots card (Phase E1) -->
      <article class="se-card wide">
        <header class="se-card-head">
          <h2>Snapshots</h2>
          <div class="se-snap-summary muted">
            {snapshots.length} total · {importantCount} important · {formatBytes(totalSnapBytes)} on disk
          </div>
        </header>

        <div class="se-snap-create">
          <input
            type="text"
            placeholder="Label (optional, e.g. 'before big refactor')"
            bind:value={snapCreateLabel}
            disabled={snapCreating}
            maxlength="80"
          />
          <label class="se-check">
            <input type="checkbox" bind:checked={snapCreateImportant} disabled={snapCreating} />
            Important (never auto-deleted)
          </label>
          <button class="se-btn primary" on:click={onCreateSnapshot} disabled={snapCreating}>
            {snapCreating ? 'Creating…' : 'Create snapshot'}
          </button>
        </div>

        {#if snapError}
          <div class="se-banner se-error" role="alert">
            <strong>Error:</strong> {snapError}
          </div>
        {/if}

        {#if snapLoading && snapshots.length === 0}
          <p class="muted">Loading snapshots…</p>
        {:else if snapshots.length === 0}
          <p class="muted">
            No snapshots yet. Create one above. Snapshots copy the entire source tree
            (excluding <code>target/</code>, <code>node_modules/</code>, <code>dist/</code>, <code>.git/</code>, <code>.luna/</code>)
            to <code>%LOCALAPPDATA%\com.luna.agent\evolver\snapshots\</code>.
          </p>
        {:else}
          <table class="se-snap-table">
            <thead>
              <tr>
                <th>Label / ID</th>
                <th>Created</th>
                <th>Files</th>
                <th>Size</th>
                <th class="actions">Actions</th>
              </tr>
            </thead>
            <tbody>
              {#each snapshots as s (s.id)}
                <tr class:important={s.important} class:active={s.is_active}>
                  <td>
                    <div class="snap-label">
                      {s.label || '—'}
                      {#if s.important}<span class="badge imp">★ important</span>{/if}
                      {#if s.is_active}<span class="badge act">● active</span>{/if}
                    </div>
                    <code class="snap-id" title={s.id}>{s.id}</code>
                  </td>
                  <td>{formatTs(s.ts)}</td>
                  <td>{s.source_files}</td>
                  <td>{formatBytes(s.total_size)}</td>
                  <td class="actions">
                    <button
                      class="se-btn"
                      on:click={() => openRestore(s)}
                      disabled={snapBusy === s.id || s.is_active}
                      title={s.is_active ? 'This is the active snapshot' : 'Restore this snapshot (overlay mode)'}>
                      Restore
                    </button>
                    <button
                      class="se-btn"
                      on:click={() => openRollback(s)}
                      disabled={snapBusy === s.id}
                      title="Roll back production to this snapshot: overlay + rebuild + atomic swap. Requires feedback.">
                      Rollback
                    </button>
                    <button
                      class="se-btn"
                      on:click={() => onToggleImportant(s)}
                      disabled={snapBusy === s.id}>
                      {s.important ? 'Unstar' : '★ Star'}
                    </button>
                    <button
                      class="se-btn danger"
                      on:click={() => onDelete(s)}
                      disabled={snapBusy === s.id || s.important || s.is_active}
                      title={s.important ? 'Unstar first' : s.is_active ? 'Cannot delete active' : 'Delete'}>
                      Delete
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </article>

      <!-- Feedback card (Phase E4) -->
      <article class="se-card wide">
        <header class="se-card-head">
          <h2>Feedback</h2>
          <button class="se-btn" on:click={refreshFeedback}>Refresh</button>
        </header>
        <p class="muted">
          Feedback is persisted to <code>%LOCALAPPDATA%\com.luna.agent\evolver\feedback\</code>.
          The next <code>self_diagnose</code> reads open feedback and includes it in the LLM prompt.
        </p>
        <div class="se-feedback-form">
          <select bind:value={feedbackCategory}>
            <option value="bug">bug</option>
            <option value="regression">regression</option>
            <option value="performance">performance</option>
            <option value="ux">ux</option>
            <option value="other">other</option>
          </select>
          <input
            type="text"
            placeholder="What went wrong? (min 5 chars)"
            bind:value={feedbackMessage}
            maxlength="2000"
            disabled={feedbackSubmitting}
          />
          <button
            class="se-btn primary"
            on:click={submitFeedback}
            disabled={feedbackSubmitting || feedbackMessage.trim().length < 5}>
            {feedbackSubmitting ? 'Submitting…' : 'Submit'}
          </button>
        </div>

        {#if feedbackListCache.length > 0}
          <table class="se-feedback-table">
            <thead>
              <tr>
                <th>ts</th>
                <th>category</th>
                <th>status</th>
                <th>message</th>
              </tr>
            </thead>
            <tbody>
              {#each feedbackListCache as fb (fb.id)}
                <tr data-status={fb.status}>
                  <td class="muted">{new Date(fb.ts).toLocaleString()}</td>
                  <td><code>{fb.category}</code></td>
                  <td><span class="status-pill status-{fb.status}">{fb.status}</span></td>
                  <td>{fb.message}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <p class="muted">No feedback yet.</p>
        {/if}
      </article>
    </div>

    <!-- Restore modal (overlay) -->
    {#if restoreTarget}
      <div class="se-modal-backdrop" role="dialog" aria-modal="true">
        <div class="se-modal">
          <h3>Restore snapshot</h3>
          <p>
            <strong>{restoreTarget.label || '—'}</strong>
            <br />
            <code class="snap-id">{restoreTarget.id}</code>
          </p>
          <p class="muted">
            This will overlay the snapshot's <code>src/</code> onto the source root.
            A pre-restore safety snapshot will be created automatically.
            <br />
            <strong>Phase E1 does not rebuild</strong> — you must run
            <code>cargo build</code> (or <code>tauri-build.cmd</code>) yourself afterwards.
          </p>
          <label>
            <span>Why are you rolling back? <em class="muted">(min 5 chars)</em></span>
            <textarea
              rows="3"
              bind:value={restoreFeedback}
              placeholder="e.g. broke telegram dispatcher after refactor"
              maxlength="2000"
            />
          </label>
          <div class="se-modal-actions">
            <button class="se-btn" on:click={cancelRestore} disabled={restoreSubmitting}>Cancel</button>
            <button
              class="se-btn primary"
              on:click={submitRestore}
              disabled={restoreSubmitting || restoreFeedback.trim().length < 5}>
              {restoreSubmitting ? 'Restoring…' : 'Restore'}
            </button>
          </div>
        </div>
      </div>
    {/if}

    <!-- Rollback modal (Phase E4: overlay + rebuild + swap) -->
    {#if rollbackTarget}
      <div class="se-modal-backdrop" role="dialog" aria-modal="true">
        <div class="se-modal">
          <h3>Rollback to snapshot</h3>
          <p>
            <strong>{rollbackTarget.label || '—'}</strong>
            <br />
            <code class="snap-id">{rollbackTarget.id}</code>
          </p>
          <p class="muted">
            <strong>⚠ This affects the running binary.</strong> Steps:
          </p>
          <ol class="muted">
            <li>Pre-rollback safety snapshot is taken</li>
            <li>Snapshot <code>src/</code> is overlaid on the source root</li>
            <li><code>cargo build --release</code> + <code>--smoke</code> run</li>
            <li>Binary atomic-swap; <code>active.json</code> updated</li>
            <li>Your feedback is saved for the next self-diagnose</li>
          </ol>
          <p class="muted">
            You must restart Luna after the swap to load the new (rolled-back) binary.
          </p>
          <label>
            <span>What broke? <em class="muted">(min 5 chars, mandatory)</em></span>
            <textarea
              rows="3"
              bind:value={rollbackFeedback}
              placeholder="e.g. telegram bot stops responding after 10 min idle"
              maxlength="2000"
            />
          </label>
          <div class="se-modal-actions">
            <button class="se-btn" on:click={cancelRollback} disabled={rollbackSubmitting}>Cancel</button>
            <button
              class="se-btn danger"
              on:click={submitRollback}
              disabled={rollbackSubmitting || rollbackFeedback.trim().length < 5}>
              {rollbackSubmitting ? 'Rolling back…' : 'Roll back'}
            </button>
          </div>
        </div>
      </div>
    {/if}

    {#if lastRefreshedAt}
      <p class="se-foot muted">Last refreshed at {lastRefreshedAt.toLocaleTimeString()}</p>
    {/if}
  {:else if !loading}
    <p class="muted">No data yet. Click Refresh.</p>
  {/if}
</section>

<style>
  .se-root {
    padding: 24px 28px;
    max-width: 1100px;
    margin: 0 auto;
    color: var(--text, #1c1c1e);
    background: var(--bg, #fafafa);
    height: 100%;
    overflow: auto;
  }
  .se-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
  }
  .se-header h1 {
    margin: 0 0 6px 0;
    font-size: 22px;
    font-weight: 600;
  }
  .se-header p {
    margin: 0;
    max-width: 60ch;
    line-height: 1.4;
  }
  .muted {
    color: var(--text-muted, #6b6b70);
  }
  .se-refresh {
    padding: 8px 14px;
    border-radius: 6px;
    border: 1px solid var(--border, #d0d0d4);
    background: var(--bg-elevated, #fff);
    cursor: pointer;
    font-size: 13px;
  }
  .se-refresh:disabled {
    opacity: 0.5;
    cursor: progress;
  }
  .se-banner {
    padding: 10px 14px;
    border-radius: 6px;
    margin-bottom: 16px;
    font-size: 13px;
  }
  .se-error {
    background: #ffe9e9;
    border: 1px solid #ff9b9b;
    color: #8b1a1a;
  }
  .se-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: 14px;
  }
  .se-card {
    background: var(--bg-elevated, #fff);
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 10px;
    padding: 14px 16px;
  }
  .se-card.wide {
    grid-column: 1 / -1;
  }
  .se-card h2 {
    margin: 0 0 10px 0;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-muted, #6b6b70);
    font-weight: 600;
  }
  .se-card dl {
    margin: 0;
    display: grid;
    grid-template-columns: 110px 1fr;
    row-gap: 6px;
    column-gap: 12px;
    font-size: 13px;
  }
  .se-card dt {
    color: var(--text-muted, #6b6b70);
  }
  .se-card dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .se-card code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    background: rgba(0, 0, 0, 0.04);
    padding: 1px 5px;
    border-radius: 4px;
  }
  .ok { color: #1b7a3a; }
  .warn { color: #b65a00; }
  .se-caps {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 10px 18px;
    font-size: 13px;
  }
  .se-caps li {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted, #6b6b70);
  }
  .se-caps li.on {
    color: var(--text, #1c1c1e);
  }
  .se-caps .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #d0d0d4;
  }
  .se-caps li.on .dot {
    background: #4a8a4a;
  }
  .se-foot {
    margin-top: 18px;
    font-size: 12px;
  }

  /* ---- Snapshots section ---- */
  .se-card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 10px;
  }
  .se-card-head h2 {
    margin: 0;
  }
  .se-snap-summary {
    font-size: 12px;
  }
  .se-snap-create {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    margin-bottom: 12px;
    padding: 10px 12px;
    background: rgba(0, 0, 0, 0.02);
    border-radius: 6px;
  }
  .se-snap-create input[type="text"] {
    flex: 1 1 220px;
    min-width: 200px;
    padding: 6px 10px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    font-size: 13px;
  }
  .se-check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--text-muted, #6b6b70);
  }
  .se-btn {
    padding: 6px 12px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg-elevated, #fff);
    color: var(--text, #1c1c1e);
    font-size: 12px;
    cursor: pointer;
    transition: background 80ms ease;
  }
  .se-btn:hover:not(:disabled) {
    background: rgba(0, 0, 0, 0.04);
  }
  .se-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .se-btn.primary {
    background: #4a6fcf;
    color: #fff;
    border-color: #4a6fcf;
  }
  .se-btn.primary:hover:not(:disabled) {
    background: #3a5fbf;
  }
  .se-btn.danger {
    color: #b03030;
    border-color: #e0a0a0;
  }
  .se-btn.danger:hover:not(:disabled) {
    background: rgba(176, 48, 48, 0.08);
  }
  .se-snap-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  .se-snap-table th,
  .se-snap-table td {
    text-align: left;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .se-snap-table th {
    font-weight: 600;
    color: var(--text-muted, #6b6b70);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }
  .se-snap-table th.actions,
  .se-snap-table td.actions {
    text-align: right;
    white-space: nowrap;
  }
  .se-snap-table td.actions .se-btn + .se-btn {
    margin-left: 6px;
  }
  .se-snap-table tr.important td {
    background: rgba(255, 215, 0, 0.06);
  }
  .se-snap-table tr.active td {
    background: rgba(74, 111, 207, 0.06);
  }
  .snap-label {
    font-weight: 500;
  }
  .snap-id {
    display: block;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
    margin-top: 2px;
  }
  .badge {
    display: inline-block;
    margin-left: 6px;
    padding: 1px 6px;
    border-radius: 10px;
    font-size: 10px;
    font-weight: 500;
    vertical-align: middle;
  }
  .badge.imp {
    background: #ffe48a;
    color: #6b4a00;
  }
  .badge.act {
    background: #c8d6f7;
    color: #2a448a;
  }

  /* ---- Restore modal ---- */
  .se-modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .se-modal {
    background: var(--bg-elevated, #fff);
    border-radius: 10px;
    padding: 22px 24px;
    max-width: 520px;
    width: calc(100% - 40px);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
  }
  .se-modal h3 {
    margin: 0 0 8px 0;
    font-size: 18px;
  }
  .se-modal p {
    margin: 8px 0;
    font-size: 13px;
    line-height: 1.45;
  }
  .se-modal label {
    display: block;
    margin-top: 14px;
    font-size: 13px;
  }
  .se-modal label span {
    display: block;
    margin-bottom: 4px;
    color: var(--text-muted, #6b6b70);
  }
  .se-modal textarea {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    font-family: inherit;
    font-size: 13px;
    resize: vertical;
  }
  .se-modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }

  /* ---- Diagnose & plan (Phase E2) ---- */
  .se-dx-summary {
    font-size: 12px;
  }
  .se-dx-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 12px;
  }
  .se-issue-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  .se-issue-table th,
  .se-issue-table td {
    text-align: left;
    padding: 7px 9px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    vertical-align: top;
  }
  .se-issue-table th {
    font-weight: 600;
    color: var(--text-muted, #6b6b70);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }
  .se-issue-table th.cb,
  .se-issue-table td.cb {
    width: 44px;
    text-align: center;
  }
  .se-issue-table code.cat {
    font-size: 11px;
    background: rgba(0, 0, 0, 0.04);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .se-issue-table code.loc {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
  }
  .se-issue-table tr.sev-crit td {
    background: rgba(176, 48, 48, 0.06);
  }
  .se-issue-table tr.sev-high td {
    background: rgba(218, 130, 0, 0.06);
  }
  .sev-pill {
    display: inline-block;
    padding: 1px 8px;
    border-radius: 10px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }
  .sev-pill.sev-crit { background: #b03030; color: #fff; }
  .sev-pill.sev-high { background: #da8200; color: #fff; }
  .sev-pill.sev-med  { background: #c8a000; color: #fff; }
  .sev-pill.sev-low  { background: #888; color: #fff; }
  .src-pill {
    font-size: 10px;
    color: var(--text-muted, #6b6b70);
    background: rgba(0, 0, 0, 0.05);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .se-plan-impact {
    margin: 6px 0 10px 0;
    font-style: italic;
  }
  .se-step-list {
    list-style: none;
    padding: 0;
    margin: 0;
    counter-reset: step;
  }
  .se-step {
    padding: 10px 12px;
    margin-bottom: 6px;
    background: rgba(0, 0, 0, 0.02);
    border-radius: 6px;
    border-left: 3px solid var(--accent, #4a6fcf);
  }
  .se-step-head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
  }
  .se-step-kind {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
    background: var(--accent, #4a6fcf);
    color: #fff;
    padding: 1px 8px;
    border-radius: 3px;
  }
  .se-step-idx {
    font-size: 11px;
    color: var(--text-muted, #6b6b70);
  }
  .se-step-target {
    display: block;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    color: var(--text, #1c1c1e);
    margin: 4px 0;
    overflow-wrap: anywhere;
  }
  .se-step-rationale {
    margin: 2px 0 0 0;
    font-size: 12px;
    color: var(--text-muted, #6b6b70);
  }
  .se-plan-foot {
    margin-top: 12px;
    font-size: 12px;
  }
  .se-banner.se-warn {
    background: #fff4e0;
    border: 1px solid #f0c060;
    color: #6b4500;
  }

  /* ---- Sandbox (Phase E3) ---- */
  .se-sandbox-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 12px;
    flex-wrap: wrap;
  }
  .se-sandbox-path {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
  }
  .se-sandbox-report {
    margin-top: 12px;
    padding: 10px 12px;
    background: rgba(0, 0, 0, 0.02);
    border-radius: 6px;
    border-left: 3px solid var(--border, #d0d0d4);
  }
  .se-sandbox-report[data-verdict="pass"] {
    border-left-color: #1b7a3a;
  }
  .se-sandbox-report[data-verdict="fail"] {
    border-left-color: #b03030;
  }
  .se-sandbox-report h4 {
    margin: 10px 0 4px 0;
    font-size: 12px;
    text-transform: uppercase;
    color: var(--text-muted, #6b6b70);
    letter-spacing: 0.3px;
  }
  .se-cmd-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .se-cmd-table th,
  .se-cmd-table td {
    text-align: left;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .se-cmd-table code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
  }
  .se-verdict {
    display: inline-block;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
  }
  .se-verdict-pass { background: #1b7a3a; color: #fff; }
  .se-verdict-fail { background: #b03030; color: #fff; }
  .se-verdict-timeout { background: #b65a00; color: #fff; }
  .se-verdict-cancelled { background: #888; color: #fff; }
  .se-smoke {
    padding: 6px 8px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.03);
  }
  .se-smoke[data-passed="false"] {
    background: rgba(176, 48, 48, 0.08);
  }
  .se-smoke[data-passed="true"] {
    background: rgba(27, 122, 58, 0.08);
  }

  /* ---- Apply (Phase E4) ---- */
  .se-apply-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    padding: 8px 10px;
    background: rgba(74, 111, 207, 0.08);
    border-radius: 6px;
  }
  .se-update-result {
    margin-top: 10px;
    padding: 8px 10px;
    border-radius: 6px;
    font-size: 12px;
  }
  .se-update-result[data-success="true"] {
    background: rgba(27, 122, 58, 0.08);
    color: #155224;
  }
  .se-update-result[data-success="false"] {
    background: rgba(176, 48, 48, 0.08);
    color: #6b1a1a;
  }

  /* ---- Feedback (Phase E4) ---- */
  .se-feedback-form {
    display: flex;
    gap: 8px;
    align-items: center;
    margin: 8px 0 12px 0;
    flex-wrap: wrap;
  }
  .se-feedback-form select {
    padding: 6px 8px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    font-size: 13px;
  }
  .se-feedback-form input[type="text"] {
    flex: 1 1 240px;
    min-width: 200px;
    padding: 6px 10px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    font-size: 13px;
  }
  .se-feedback-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .se-feedback-table th,
  .se-feedback-table td {
    text-align: left;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    vertical-align: top;
  }
  .se-feedback-table th {
    font-weight: 600;
    color: var(--text-muted, #6b6b70);
    font-size: 11px;
    text-transform: uppercase;
  }
  .se-feedback-table tr[data-status="open"] td {
    background: rgba(255, 215, 0, 0.05);
  }
  .se-feedback-table code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 11px;
    background: rgba(0, 0, 0, 0.04);
    padding: 1px 5px;
    border-radius: 3px;
  }
  .status-pill {
    display: inline-block;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
  }
  .status-pill.status-open { background: #ffd54a; color: #5a4500; }
  .status-pill.status-resolved { background: #1b7a3a; color: #fff; }
  .status-pill.status-wontfix { background: #888; color: #fff; }
</style>
