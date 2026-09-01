// src/lib/selfEvolver.ts
//
// Typed IPC wrappers for the self-evolution subsystem (Phase E0+).
// Mirrors the Rust types in `src-tauri/src/services/evolver/`.
//
// Phase E0 ships only the read-only commands; later phases (E1-E5)
// will add snapshot/sandbox/apply/rollback/feedback wrappers here.

interface TauriCore {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

declare global {
  interface Window {
    __TAURI__?: { core?: TauriCore };
  }
}

function core(): TauriCore {
  const w = window as any;
  if (!w.__TAURI__?.core) {
    throw new Error(
      '[selfEvolver.ts] Tauri runtime not detected. Open via `npm run tauri:dev`.',
    );
  }
  return w.__TAURI__.core;
}

// ---------- Types (mirror Rust services::evolver::inspect) ----------

/** How the source root was resolved: env, autodetect, or none. */
export type SourceRootSource = 'env' | 'autodetect' | 'none';

/** Snapshot of Luna's own metadata at a point in time. */
export type SelfInfo = {
  version: string;
  identifier: string;
  source_root: string | null;
  source_root_source: SourceRootSource;
  git_sha: string | null;
  git_dirty: boolean | null;
  build_host: string;
  exe_path: string | null;
  source_files: number | null;
  source_bytes: number | null;
  active: ActiveVersion | null;
  last_evolution_at: string | null; // ISO 8601
  capabilities: Capabilities;
};

export type ActiveVersion = {
  version: string;
  git_sha: string | null;
  build_ts: string | null; // ISO 8601
  snapshot_id: string | null;
};

/** High-level capabilities available in this build. */
export type Capabilities = {
  self_inspect: boolean;
  snapshots: boolean;
  diagnose: boolean;
  sandbox: boolean;
  apply_update: boolean;
};

/** A description of the in-flight evolution operation. */
export type EvolutionOp =
  | { kind: 'diagnosing'; plan_id: string }
  | { kind: 'sandbox'; sandbox_id: string; step: string }
  | { kind: 'building'; snapshot_id: string }
  | { kind: 'applying'; snapshot_id: string; plan_id: string }
  | { kind: 'rolling_back'; snapshot_id: string };

/** Progress payload for in-flight operations. */
export type ProgressInfo = {
  stage: string;
  pct: number; // 0..=100
  message: string;
};

/** Cheap poll of the current evolver state. */
export type EvolverStateSnapshot = {
  idle: boolean;
  current: EvolutionOp | null;
  progress: ProgressInfo;
  last_evolution_at: string | null;
};

// ---------- IPC wrappers ----------

/** Read-only: return a snapshot of Luna's own metadata. */
export async function selfInspect(): Promise<SelfInfo> {
  return core().invoke<SelfInfo>('self_inspect');
}

/** Read-only: return the currently-active version (or null if none). */
export async function getActiveVersion(): Promise<ActiveVersion | null> {
  return core().invoke<ActiveVersion | null>('get_active_version');
}

/** Cheap poll: idle/busy + current op + progress. */
export async function getEvolverState(): Promise<EvolverStateSnapshot> {
  return core().invoke<EvolverStateSnapshot>('get_evolver_state');
}

// ---------- Snapshot types (Phase E1, mirror Rust services::evolver::snapshot) ----------

export type SnapshotInfo = {
  id: string;
  label: string;
  /** ISO 8601 UTC timestamp. */
  ts: string;
  version: string;
  source_files: number;
  total_size: number;
  important: boolean;
  is_active: boolean;
  path: string;
};

export type CreateResult = {
  info: SnapshotInfo;
  gc_deleted: string[];
  gc_freed_bytes: number;
};

export type RestoreResult = {
  restored_from: string;
  files_written: number;
  pre_restore_snap_id: string;
  feedback_id: string;
  /** In Phase E1 always true; user must run `cargo build` themselves. */
  needs_rebuild: boolean;
};

export type DeleteResult = {
  deleted: boolean;
  reason: string | null;
  freed_bytes: number;
};

// ---------- Snapshot IPC wrappers (Phase E1) ----------

/** Create a full-source snapshot. Runs the GC pass as a side effect. */
export async function snapshotCreate(
  label?: string,
  important = false,
): Promise<CreateResult> {
  return core().invoke<CreateResult>('snapshot_create', {
    label: label ?? null,
    important,
  });
}

/** List all known snapshots, newest first. */
export async function snapshotList(): Promise<SnapshotInfo[]> {
  return core().invoke<SnapshotInfo[]>('snapshot_list');
}

/**
 * Restore a snapshot by overlaying its `src/` onto the source root.
 * `feedbackMessage` must be at least 5 characters; we always log why
 * the user rolled back.
 *
 * Phase E1: does NOT run `cargo build`; the user must rebuild.
 * Returns `RestoreResult { needs_rebuild: true }`.
 */
export async function snapshotRestore(
  snapshotId: string,
  feedbackMessage: string,
): Promise<RestoreResult> {
  if (!feedbackMessage || feedbackMessage.trim().length < 5) {
    throw new Error('Feedback must be at least 5 characters.');
  }
  return core().invoke<RestoreResult>('snapshot_restore', {
    snapshotId,
    feedbackMessage,
  });
}

/** Delete a snapshot. Returns `deleted: false` + `reason` if refused. */
export async function snapshotDelete(snapshotId: string): Promise<DeleteResult> {
  return core().invoke<DeleteResult>('snapshot_delete', { snapshotId });
}

/** Toggle the `important` flag on a snapshot. */
export async function snapshotMarkImportant(
  snapshotId: string,
  important: boolean,
): Promise<SnapshotInfo> {
  return core().invoke<SnapshotInfo>('snapshot_mark_important', {
    snapshotId,
    important,
  });
}

// ---------- Diagnose types (Phase E2, mirror Rust services::evolver::diagnose) ----------

export type Severity = 'low' | 'med' | 'high' | 'crit';

export type IssueCategory =
  | 'bug'
  | 'security'
  | 'performance'
  | 'correctness'
  | 'dead_code'
  | 'style'
  | 'ux'
  | 'other';

export type IssueSource = 'static' | 'llm' | 'user_feedback';

export type Issue = {
  id: string;
  severity: Severity;
  file?: string;
  line?: number;
  hint: string;
  category: IssueCategory;
  source: IssueSource;
};

export type DiagnoseScope = 'all' | 'rust' | 'frontend' | 'security' | 'deps';

export type DiagnoseResult = {
  id: string;
  issues: Issue[];
  latency_ms: number;
  /** "static" if no API key / LLM failed; "static+llm" if LLM ran. */
  mode: string;
  llm_error?: string;
};

// ---------- Plan types (Phase E2, mirror Rust services::evolver::planner) ----------

export type PlanStep =
  | {
      kind: 'edit_file';
      path: string;
      old_text: string;
      new_text: string;
      rationale: string;
    }
  | {
      kind: 'create_file';
      path: string;
      content: string;
      rationale: string;
    }
  | { kind: 'run_command'; command: string; rationale: string };

export type Plan = {
  id: string;
  created_at: string; // ISO 8601
  diagnose_id: string;
  issues_addressed: string[];
  risk_score: number; // 0..=1
  expected_impact: string;
  steps: PlanStep[];
  /** "llm" if a real LLM plan was produced; "trivial" if no key or LLM failed. */
  mode: string;
};

export type PlanRequest = {
  issue_ids: string[];
  risk_threshold?: number;
};

// ---------- Diagnose/Plan IPC wrappers (Phase E2) ----------

/** Run self-diagnose. Always runs static scan; runs LLM only if a key is set. */
export async function selfDiagnose(
  scope: DiagnoseScope = 'all',
): Promise<DiagnoseResult> {
  return core().invoke<DiagnoseResult>('self_diagnose', { scope });
}

/** Build a plan from a set of issue ids. */
export async function selfPlan(
  issueIds: string[],
  knownIssues?: Issue[],
  diagnoseId?: string,
  riskThreshold?: number,
): Promise<Plan> {
  return core().invoke<Plan>('self_plan', {
    req: {
      issue_ids: issueIds,
      risk_threshold: riskThreshold ?? null,
    },
    knownIssues: knownIssues ?? null,
    diagnoseId: diagnoseId ?? null,
  });
}

// ---------- Diagnose UI helpers ----------

/** Color hint for severity (returned as a CSS class suffix). */
export function severityClass(sev: Severity): string {
  return `sev-${sev}`;
}

export function severityLabel(sev: Severity): string {
  switch (sev) {
    case 'crit':
      return 'critical';
    case 'high':
      return 'high';
    case 'med':
      return 'medium';
    case 'low':
      return 'low';
  }
}

/** Visual risk level from a 0..1 score. */
export function riskLevel(score: number): 'low' | 'med' | 'high' {
  if (score < 0.3) return 'low';
  if (score < 0.7) return 'med';
  return 'high';
}

export function riskColor(score: number): string {
  const lvl = riskLevel(score);
  return lvl === 'low' ? '#1b7a3a' : lvl === 'med' ? '#b65a00' : '#b03030';
}

// ---------- Sandbox types (Phase E3, mirror Rust services::evolver::sandbox) ----------

export type Verdict = 'pass' | 'fail' | 'timeout' | 'cancelled';

export type CreateSandboxResult = {
  sandbox_id: string;
  path: string;
  source_files: number;
  source_bytes: number;
  elapsed_ms: number;
};

export type AppliedStep = {
  step_index: number;
  kind: string;
  path: string;
  diff: string;
  elapsed_ms: number;
};

export type RunResult = {
  command: string;
  exit_code: number;
  stdout_excerpt: string;
  stderr_excerpt: string;
  duration_ms: number;
  truncated: boolean;
  verdict: Verdict;
};

export type SmokeResult = {
  passed: boolean;
  exit_code: number | null;
  stderr_excerpt: string;
  stdout_excerpt: string;
  duration_ms: number;
  failure_reason: string | null;
};

export type SandboxReport = {
  sandbox_id: string;
  steps_applied: AppliedStep[];
  commands: RunResult[];
  smoke: SmokeResult | null;
  verdict: Verdict;
  total_elapsed_ms: number;
};

// ---------- Sandbox IPC wrappers (Phase E3) ----------

/** Create a fresh sandbox (copies source tree to a temp dir). */
export async function sandboxCreate(): Promise<CreateSandboxResult> {
  return core().invoke<CreateSandboxResult>('sandbox_create');
}

/** Apply a plan to a sandbox. */
export async function sandboxApply(
  sandboxId: string,
  plan: Plan,
): Promise<AppliedStep[]> {
  return core().invoke<AppliedStep[]>('sandbox_apply', {
    sandboxId,
    plan,
  });
}

/** Run an allow-listed command in a sandbox. */
export async function sandboxRun(
  sandboxId: string,
  command: string,
): Promise<RunResult> {
  return core().invoke<RunResult>('sandbox_run', { sandboxId, command });
}

/** Run --smoke on the freshly built binary in a sandbox. */
export async function sandboxSmoke(sandboxId: string): Promise<SmokeResult> {
  return core().invoke<SmokeResult>('sandbox_smoke', { sandboxId });
}

/** Collect the final report for a sandbox. */
export async function sandboxCollect(sandboxId: string): Promise<SandboxReport> {
  return core().invoke<SandboxReport>('sandbox_collect', { sandboxId });
}

/** Discard a sandbox (delete its temp dir). */
export async function sandboxDiscard(sandboxId: string): Promise<void> {
  return core().invoke<void>('sandbox_discard', { sandboxId });
}

// ---------- Apply / Rollback / Feedback types (Phase E4) ----------

export type UpdateResult = {
  new_version: string;
  pre_update_snapshot_id: string;
  build_exit_code: number;
  build_duration_ms: number;
  smoke_passed: boolean;
  needs_restart: boolean;
  new_exe_path: string;
  error: string | null;
};

export type RollbackResult = {
  restored_from: string;
  pre_rollback_snapshot_id: string;
  build_exit_code: number;
  build_duration_ms: number;
  smoke_passed: boolean;
  needs_restart: boolean;
  feedback_id: string;
  new_exe_path: string;
  error: string | null;
};

export type FeedbackCategory = 'bug' | 'regression' | 'performance' | 'ux' | 'other';
export type FeedbackStatus = 'open' | 'resolved' | 'wontfix';

export type FeedbackEntry = {
  id: string;
  ts: string;
  category: FeedbackCategory;
  message: string;
  plan_id: string | null;
  snapshot_id: string | null;
  status: FeedbackStatus;
  resolution_plan_id: string | null;
};

// ---------- Apply / Rollback / Feedback IPC wrappers (Phase E4) ----------

export async function applySelfUpdate(
  planId: string,
  planSteps: PlanStep[],
): Promise<UpdateResult> {
  return core().invoke<UpdateResult>('apply_self_update', {
    planId,
    planSteps,
  });
}

export async function rollbackSelfUpdate(
  snapshotId: string,
  feedbackMessage: string,
): Promise<RollbackResult> {
  if (!feedbackMessage || feedbackMessage.trim().length < 5) {
    throw new Error('Feedback must be at least 5 characters.');
  }
  return core().invoke<RollbackResult>('rollback_self_update', {
    snapshotId,
    feedbackMessage,
  });
}

export async function feedbackSubmit(
  category: FeedbackCategory,
  message: string,
  planId?: string,
  snapshotId?: string,
): Promise<string> {
  if (!message || message.trim().length < 5) {
    throw new Error('Feedback must be at least 5 characters.');
  }
  return core().invoke<string>('feedback_submit', {
    category,
    message,
    planId: planId ?? null,
    snapshotId: snapshotId ?? null,
  });
}

export async function feedbackList(
  status?: FeedbackStatus | 'all',
): Promise<FeedbackEntry[]> {
  return core().invoke<FeedbackEntry[]>('feedback_list', {
    status: status ?? null,
  });
}

export async function feedbackResolve(
  feedbackId: string,
  resolutionPlanId: string,
): Promise<void> {
  return core().invoke<void>('feedback_resolve', {
    feedbackId,
    resolutionPlanId,
  });
}

// ---------- UI helpers ----------

/** Pretty-print bytes (binary or SI; we use SI for source sizes). */
export function formatBytes(n: number | null | undefined): string {
  if (n == null) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Pretty-print a short git SHA. */
export function shortSha(sha: string | null | undefined): string {
  if (!sha) return '—';
  return sha.length > 10 ? `${sha.slice(0, 10)}…` : sha;
}

/** Human label for `SourceRootSource`. */
export function sourceRootLabel(s: SourceRootSource): string {
  switch (s) {
    case 'env':
      return 'env: LUNA_SOURCE_ROOT';
    case 'autodetect':
      return 'autodetect';
    case 'none':
      return 'not found';
  }
}
