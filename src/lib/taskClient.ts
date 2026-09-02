// src/lib/taskClient.ts
//
// Typed IPC wrappers for the background-agent (Phase M0+) Tauri commands.
// Mirrors the Rust types in `src-tauri/src/services/agent/task.rs`.

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
    throw new Error('[taskClient.ts] Tauri runtime not detected.');
  }
  return w.__TAURI__.core;
}

// ---------- Types (mirror Rust services::agent::task) ----------

export type TaskStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'timed_out';

export type TaskSummary = {
  id: string;
  title: string;
  status: TaskStatus;
  model: string;
  parent_chat_id: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  last_active_at: string;
  steps_completed: number;
  total_tokens: number;
  cancellation_requested: boolean;
};

export type TaskCost = {
  input_tokens: number;
  output_tokens: number;
  cache_hits: number;
  sub_agent_input_tokens: number;
  sub_agent_output_tokens: number;
  estimated_usd: number;
};

export type Task = {
  id: string;
  title: string;
  prompt: string;
  status: TaskStatus;
  model: string;
  sub_agent_model: string;
  parent_chat_id: string | null;
  parent_task_id: string | null;
  max_steps: number;
  max_subagents: number;
  max_cost_tokens: number;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  last_active_at: string;
  cost: TaskCost;
  error: string | null;
  cancellation_requested: boolean;
  steps_completed: number;
  sub_agent_count: number;
};

export type CreateTaskInput = {
  title: string;
  prompt: string;
  parentChatId?: string;
  model?: string;
  subAgentModel?: string;
  maxSteps?: number;
  maxSubagents?: number;
  maxCostTokens?: number;
  /**
   * Optional persona id (e.g. `"lucifer"` for the MorningStar
   * healer, `"raziel"` for the memory curator). When set, the
   * runner dispatches to that persona's supervisor loop and uses
   * its system prompt + tool whitelist. Defaults to `null`
   * (anonymous code-analysis task).
   *
   * Phase M3+: used by the "🌟 Heal project" button to spawn a
   * MorningStar task without going through the chat.
   */
  personaId?: string;
};

// ---------- IPC wrappers ----------

/** Create a new background task. Returns the new task id. */
export async function taskCreate(input: CreateTaskInput): Promise<string> {
  return core().invoke<string>('task_create', {
    title: input.title,
    prompt: input.prompt,
    parentChatId: input.parentChatId ?? null,
    model: input.model ?? null,
    subAgentModel: input.subAgentModel ?? null,
    maxSteps: input.maxSteps ?? null,
    maxSubagents: input.maxSubagents ?? null,
    maxCostTokens: input.maxCostTokens ?? null,
    personaId: input.personaId ?? null,
  });
}

/**
 * One-click heal of the current workspace. Spawns a MorningStar
 * (Lucifer) task — the healer persona — with the standard heal
 * prompt. The task appears in `TasksSidebar` and the user can
 * follow its progress (which files it changed, which commit it
 * made, or why it escalated).
 *
 * Phase M3+: equivalent to clicking the "🌟 Heal project" button
 * in the Sidebar. Returns the new task id.
 *
 * @param reason Optional human-readable prefix for the task title
 *   (e.g. "auto: build failed", "manual: I broke something"). The
 *   runner uses this to distinguish auto-triggers from manual
 *   ones; the UI can use it to decide whether to show "Утренняя
 *   Звезда" (auto) or "Люцифер" (manual) as the persona name.
 */
export async function healProject(reason?: string): Promise<string> {
  const title = reason
    ? `Heal project — ${reason}`
    : 'Heal project';
  return taskCreate({
    title,
    prompt:
      'Run a heal pass on the current workspace: detect the toolchain, ' +
      'run the check, fix any build / test errors, and commit the result. ' +
      'If the build is already green, just report and exit.',
    personaId: 'lucifer',
  });
}

/** List background tasks, newest first. Optional status filter. */
export async function taskList(
  status?: TaskStatus | 'all',
): Promise<TaskSummary[]> {
  return core().invoke<TaskSummary[]>('task_list', {
    status: status ?? null,
  });
}

/** Get a single task (full record, not just summary). */
export async function taskGet(taskId: string): Promise<Task> {
  return core().invoke<Task>('task_get', { taskId });
}

/** Delete a task and all its files. If running, fires cancel token. */
export async function taskDelete(taskId: string): Promise<void> {
  return core().invoke<void>('task_delete', { taskId });
}

/** Cancel a running or queued task. Idempotent. */
export async function taskCancel(taskId: string): Promise<void> {
  return core().invoke<void>('task_cancel', { taskId });
}

/** Read the final result markdown (or null if not yet completed). */
export async function taskResult(taskId: string): Promise<string | null> {
  return core().invoke<string | null>('task_result', { taskId });
}

/** Read all events emitted by the task (assistant text, tool calls, etc.). */
export async function taskSteps(
  taskId: string,
): Promise<Array<Record<string, unknown>>> {
  return core().invoke<Array<Record<string, unknown>>>('task_steps', { taskId });
}

// ---------- Event types ----------

/** A live `task_progress` event payload (per-step). */
export type TaskProgressEvent = {
  task_id: string;
  step: Record<string, unknown>;
};

/** A `task_finished` event payload (terminal status). */
export type TaskFinishedEvent = {
  task_id: string;
  status: TaskStatus;
  finished_at: string | null;
  error: string | null;
};

/** Subscribe to `task_progress` events. Returns an unsubscribe function. */
export function onTaskProgress(
  cb: (event: TaskProgressEvent) => void,
): () => void {
  // Lazy import to avoid breaking in non-Tauri contexts.
  const ev = (window as any).__TAURI__?.event;
  if (!ev?.listen) {
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  ev.listen('task_progress', (e: { payload: TaskProgressEvent }) => {
    cb(e.payload);
  }).then((u: () => void) => {
    unlisten = u;
  });
  return () => {
    if (unlisten) unlisten();
  };
}

/** Subscribe to `task_finished` events. Returns an unsubscribe function. */
export function onTaskFinished(
  cb: (event: TaskFinishedEvent) => void,
): () => void {
  const ev = (window as any).__TAURI__?.event;
  if (!ev?.listen) {
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  ev.listen('task_finished', (e: { payload: TaskFinishedEvent }) => {
    cb(e.payload);
  }).then((u: () => void) => {
    unlisten = u;
  });
  return () => {
    if (unlisten) unlisten();
  };
}

// ---------- UI helper: send a chat message to a background task ----------

/** Build a short title from a prompt (first 60 chars on a word boundary). */
export function titleFromPrompt(prompt: string): string {
  const trimmed = prompt.trim().replace(/\s+/g, ' ');
  if (trimmed.length <= 60) return trimmed;
  const cut = trimmed.slice(0, 60);
  const lastSpace = cut.lastIndexOf(' ');
  return (lastSpace > 20 ? cut.slice(0, lastSpace) : cut) + '…';
}

// ---------- UI helpers ----------

/** Russian-friendly status label. */
export function statusLabel(s: TaskStatus): string {
  switch (s) {
    case 'pending':
      return 'в очереди';
    case 'running':
      return 'выполняется';
    case 'completed':
      return 'готово';
    case 'failed':
      return 'ошибка';
    case 'cancelled':
      return 'отменено';
    case 'timed_out':
      return 'тайм-аут';
  }
}

/** Format token count into a short human-readable string. */
export function formatTokens(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}
