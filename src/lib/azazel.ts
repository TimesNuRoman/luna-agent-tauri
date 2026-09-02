// src/lib/azazel.ts
// Typed wrappers around the Azazel browser-use agent's Tauri IPC.
// Mirrors the Rust types in `services::azazel::state` and
// `services::azazel::safety`.

import { core, events } from './tauri';

// ---------- Types (mirror Rust) ----------

/** `services::azazel::safety::ApprovalPolicy`. */
export type ApprovalPolicy = 'strict' | 'normal' | 'yolo';

/** `services::azazel::safety::ApprovalDecision`. */
export type ApprovalDecision = 'approve' | 'reject' | 'approve_always_for_session';

/** `services::azazel::safety::RiskLevel`. */
export type RiskLevel = 'low' | 'medium' | 'high';

/** A single screenshot frame from the browser supervisor. */
export type BrowserFrame = {
  bytes: number[];        // Vec<u8> serialised as array
  width: number;
  height: number;
  seq: number;
  t_ms: number;
  url: string;
  title: string;
};

/** `services::azazel::state::BrowserStateDto`. */
export type BrowserStateDto = {
  launched: boolean;
  profile_dir: string;
  last_error: string | null;
  running_task_count: number;
  last_frame_seq: number;
};

/** `services::azazel::supervisor::SupervisorError`-reduced info. */
export type AzazelStepEvent = {
  task_id: string;
  step_n: number;
  tool: string;
  is_error: boolean;
  preview?: string;
  screenshot_b64?: string;
  url?: string;
  ts: string;
};

/** `services::azazel::supervisor` approval request payload. */
export type AzazelApprovalEvent = {
  task_id: string;
  tool_name: string;
  tool_args: Record<string, unknown>;
  risk: RiskLevel;
  prompt_text: string;
};

/** `services::azazel::supervisor` terminal event. */
export type AzazelDoneEvent = {
  task_id: string;
  status: 'completed' | 'failed' | 'cancelled' | 'timed_out';
  summary: string;
  cost: number;
  ts: string;
};

/** `services::azazel::supervisor` error event. */
export type AzazelErrorEvent = {
  task_id: string;
  error: string;
  ts: string;
};

// ---------- Commands ----------

/** Spawn a new Azazel browser-use task. Returns the new `task_id`. */
export async function azazelRun(req: {
  title?: string;
  prompt: string;
  parent_chat_id?: string;
  max_steps?: number;
  max_cost_tokens?: number;
}): Promise<string> {
  return await core().invoke<string>('azazel_run', { req });
}

/** Cancel a running Azazel task. */
export async function azazelCancel(taskId: string): Promise<void> {
  await core().invoke('azazel_cancel', { taskId });
}

/** Read the latest cached screenshot for a browser task, if any. */
export async function azazelScreenshot(taskId: string): Promise<BrowserFrame | null> {
  const f = await core().invoke<BrowserFrame | null>('azazel_screenshot', { taskId });
  return f;
}

/** Read the browser state (launched? profile? running count?) for the UI badge. */
export async function azazelGetBrowserState(): Promise<BrowserStateDto> {
  return await core().invoke<BrowserStateDto>('azazel_get_browser_state');
}

/** Switch the approval policy. */
export async function azazelSetPolicy(policy: ApprovalPolicy): Promise<void> {
  await core().invoke('azazel_set_policy', { policy });
}

/** Resolve a pending approval. */
export async function azazelApprove(
  taskId: string,
  decision: ApprovalDecision,
): Promise<void> {
  await core().invoke('azazel_approve', { taskId, decision });
}

/** How many approvals are waiting right now (for a UI badge). */
export async function azazelPendingApprovals(): Promise<number> {
  return await core().invoke<number>('azazel_pending_approvals');
}

// ---------- Events ----------

/**
 * Subscribe to per-step supervisor events. The handler is called
 * for every tool the agent runs (success or error). The
 * `screenshot_b64` field, if present, is a `data:image/jpeg;base64,...`
 * URL you can drop straight into an `<img src=...>`.
 */
export function onAzazelStep(
  handler: (e: AzazelStepEvent) => void,
): Promise<() => void> {
  return events().listen<AzazelStepEvent>('azazel:step', (e) => handler(e.payload));
}

/** Subscribe to approval requests. The UI must pop a modal and
 * call `azazelApprove(taskId, ...)` in response. */
export function onAzazelApprovalNeeded(
  handler: (e: AzazelApprovalEvent) => void,
): Promise<() => void> {
  return events().listen<AzazelApprovalEvent>(
    'azazel:approval-needed',
    (e) => handler(e.payload),
  );
}

/** Subscribe to terminal events. */
export function onAzazelDone(
  handler: (e: AzazelDoneEvent) => void,
): Promise<() => void> {
  return events().listen<AzazelDoneEvent>('azazel:done', (e) => handler(e.payload));
}

/** Subscribe to error events. */
export function onAzazelError(
  handler: (e: AzazelErrorEvent) => void,
): Promise<() => void> {
  return events().listen<AzazelErrorEvent>('azazel:error', (e) => handler(e.payload));
}
