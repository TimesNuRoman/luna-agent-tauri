// src/lib/planStore.ts
//
// Local-only plan store for Luna Agent's "Plan mode" feature.
//
// Plans are user-curated todo lists that the user can run against the
// agent. When the user clicks "Run", we send a `create_plan` tool
// prompt to the model. The tool's `ai_plan_created` event then gets
// linked back to the local plan via `linkPlanToMessage` so the chat
// card and the sidebar stay in sync.
//
// Storage: localStorage under `luna.plans.v1`. The file is the source
// of truth for the UI; we debounce writes by 300ms so a flurry of
// edits coalesces into a single write. localStorage is good enough
// for v1 — no cross-device sync, no Rust-side persistence. If the
// user wants cloud sync, we'll move to a `luna-plans.json` file in
// `%APPDATA%` (Windows) / `$XDG_DATA_HOME` (Linux) the same way the
// chat history is persisted.
//
// Key types are re-exported here so the rest of the app can
// `import { type Plan, type PlanStep } from './lib/planStore'`
// without coupling to the agent-side types in `lib/tauri.ts`.
// =====================================================================

import { writable, get, type Writable } from 'svelte/store';

// --- Types ------------------------------------------------------------

export type PlanStepStatus = 'pending' | 'in_progress' | 'done' | 'error';

export interface PlanStep {
  id: string;
  title: string;
  status: PlanStepStatus;
  note?: string;
}

export interface Plan {
  id: string;
  title: string;
  steps: PlanStep[];
  createdAt: number;
  updatedAt: number;
  /** True once the user clicked Run and the agent emitted a matching
   *  ai_plan_created. The plan then becomes read-only in the sidebar
   *  (we don't want the user to edit a plan that's already mid-flight). */
  chatLinked?: boolean;
  /** Optional chat message id produced by the agent. Used to attribute
   *  ai_step_updated events to the right plan in the sidebar. */
  chatMessageId?: number;
  /** True if the plan was created by the agent (not by the user) and
   *  the user hasn't taken ownership of it. Such plans get a different
   *  visual treatment and no Run button. */
  agentOnly?: boolean;
}

/** planId -> chat message id. Mirrored from Plan.chatMessageId so
 *  PlansSidebar can quickly find the plan by messageId. */
export type PlanMessageMap = Record<string, number>;

/** toolCallId -> planId. Transient, NOT persisted to localStorage
 *  (lives only for the current window session). Used to attribute
 *  ai_step_updated events (which carry the tool call id, not the
 *  plan id) to the correct plan. */
export const toolCallToPlan: Writable<Record<string, string>> = writable<Record<string, string>>({});

// --- Persistence ------------------------------------------------------

const STORAGE_KEY = 'luna.plans.v1';
const STORAGE_VERSION = 1;

interface Serialized {
  version: number;
  plans: Plan[];
  msgMap: PlanMessageMap;
}

function safeLocalStorage(): Storage | null {
  try {
    if (typeof window === 'undefined') return null;
    return window.localStorage;
  } catch {
    return null;
  }
}

function loadInitial(): { plans: Plan[]; msgMap: PlanMessageMap } {
  const ls = safeLocalStorage();
  if (!ls) return { plans: [], msgMap: {} };
  try {
    const raw = ls.getItem(STORAGE_KEY);
    if (!raw) return { plans: [], msgMap: {} };
    const parsed = JSON.parse(raw) as Serialized;
    if (!parsed || parsed.version !== STORAGE_VERSION) return { plans: [], msgMap: {} };
    if (!Array.isArray(parsed.plans)) return { plans: [], msgMap: {} };
    return {
      plans: parsed.plans as Plan[],
      msgMap: (parsed.msgMap ?? {}) as PlanMessageMap,
    };
  } catch (e) {
    console.warn('[planStore] load failed:', e);
    return { plans: [], msgMap: {} };
  }
}

const initial = loadInitial();

export const plans: Writable<Plan[]> = writable<Plan[]>(initial.plans);
export const planMessageMap: Writable<PlanMessageMap> = writable<PlanMessageMap>(initial.msgMap);

// --- Debounced persistence --------------------------------------------

let saveTimer: ReturnType<typeof setTimeout> | null = null;
let dirty = false;

function scheduleSave(): void {
  dirty = true;
  if (saveTimer != null) return;
  saveTimer = setTimeout(() => {
    saveTimer = null;
    if (!dirty) return;
    dirty = false;
    persist();
  }, 300);
}

function persist(): void {
  const ls = safeLocalStorage();
  if (!ls) return;
  try {
    const data: Serialized = {
      version: STORAGE_VERSION,
      plans: get(plans),
      msgMap: get(planMessageMap),
    };
    ls.setItem(STORAGE_KEY, JSON.stringify(data));
  } catch (e) {
    // Quota exceeded or storage disabled — log but don't crash.
    // The user keeps the in-memory state for the rest of the session.
    console.warn('[planStore] save failed:', e);
  }
}

plans.subscribe(scheduleSave);
planMessageMap.subscribe(scheduleSave);

// --- ID generation ----------------------------------------------------

function uuid(): string {
  // crypto.randomUUID is available in modern browsers and Tauri's
  // webview. Fall back to a timestamp+random mix for safety.
  try {
    if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID();
  } catch { /* ignore */ }
  return 'p-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10);
}

// --- Public API -------------------------------------------------------

/** Create a new user-side plan and add it to the store.
 *  Returns the created plan so the caller can highlight / scroll to it. */
export function createPlan(title: string, steps: PlanStep[]): Plan {
  const now = Date.now();
  const plan: Plan = {
    id: uuid(),
    title: title.trim(),
    steps: steps.map((s) => ({ ...s, id: s.id || uuid() })),
    createdAt: now,
    updatedAt: now,
  };
  plans.update((all) => [plan, ...all]);
  return plan;
}

/** Delete a plan and unlink any chat message it was bound to. */
export function deletePlan(id: string): void {
  plans.update((all) => all.filter((p) => p.id !== id));
  planMessageMap.update((m) => {
    if (m[id] != null) {
      const next = { ...m };
      delete next[id];
      return next;
    }
    return m;
  });
}

/** Rename a plan. No-op if the id is unknown. */
export function renamePlan(id: string, title: string): void {
  const trimmed = title.trim();
  if (!trimmed) return;
  plans.update((all) =>
    all.map((p) => (p.id === id ? { ...p, title: trimmed, updatedAt: Date.now() } : p))
  );
}

/** Update a single step's fields (title, status, note). */
export function updateStep(planId: string, stepId: string, patch: Partial<PlanStep>): void {
  plans.update((all) =>
    all.map((p) => {
      if (p.id !== planId) return p;
      return {
        ...p,
        updatedAt: Date.now(),
        steps: p.steps.map((s) => (s.id === stepId ? { ...s, ...patch } : s)),
      };
    })
  );
}

/** Add a new step to a plan. */
export function addStep(planId: string, title: string): void {
  const trimmed = title.trim();
  if (!trimmed) return;
  plans.update((all) =>
    all.map((p) => {
      if (p.id !== planId) return p;
      return {
        ...p,
        updatedAt: Date.now(),
        steps: [...p.steps, { id: uuid(), title: trimmed, status: 'pending' }],
      };
    })
  );
}

/** Remove a step. */
export function removeStep(planId: string, stepId: string): void {
  plans.update((all) =>
    all.map((p) => {
      if (p.id !== planId) return p;
      return { ...p, updatedAt: Date.now(), steps: p.steps.filter((s) => s.id !== stepId) };
    })
  );
}

/** Replace the entire step list (used when reordering or pasting). */
export function setSteps(planId: string, steps: PlanStep[]): void {
  plans.update((all) =>
    all.map((p) => (p.id === planId ? { ...p, steps, updatedAt: Date.now() } : p))
  );
}

/** Move a step up or down. dir = -1 up, +1 down. */
export function moveStep(planId: string, stepId: string, dir: -1 | 1): void {
  plans.update((all) =>
    all.map((p) => {
      if (p.id !== planId) return p;
      const idx = p.steps.findIndex((s) => s.id === stepId);
      if (idx < 0) return p;
      const ni = idx + dir;
      if (ni < 0 || ni >= p.steps.length) return p;
      const next = p.steps.slice();
      [next[idx], next[ni]] = [next[ni], next[idx]];
      return { ...p, steps: next, updatedAt: Date.now() };
    })
  );
}

/** Link a plan to the chat message the agent produced for it. */
export function linkPlanToMessage(planId: string, messageId: number): void {
  plans.update((all) =>
    all.map((p) => (p.id === planId ? { ...p, chatLinked: true, chatMessageId: messageId } : p))
  );
  planMessageMap.update((m) => ({ ...m, [planId]: messageId }));
}

/** Find a plan by its chat message id. */
export function findPlanByMessageId(messageId: number): Plan | undefined {
  return get(plans).find((p) => p.chatMessageId === messageId);
}

/** Mark a plan as "agent-only" (not runnable from the sidebar). */
export function markAgentOnly(planId: string, agentOnly: boolean): void {
  plans.update((all) =>
    all.map((p) => (p.id === planId ? { ...p, agentOnly } : p))
  );
}

/** Look up a planId by the tool call id from an ai_plan_created
 *  event. Returns undefined if the mapping is unknown. */
export function planIdForToolCall(toolCallId: string): string | undefined {
  return get(toolCallToPlan)[toolCallId];
}

/** Record that the agent just emitted a `create_plan` tool call.
 *  Returns the planId to use. If a plan with the same title already
 *  exists in the store (e.g. the user clicked Run on a user-side
 *  plan), we link the tool call to the existing plan and clear the
 *  agentOnly flag — the user "takes ownership" of the agent-side
 *  plan. Otherwise we create a new agent-only plan.
 *
 *  The mapping is transient (toolCallToPlan is not persisted). */
export function recordAgentPlan(args: {
  toolCallId: string;
  title: string;
  steps: Array<{ id: string; title: string; status?: PlanStepStatus; note?: string }>;
}): { planId: string; isNew: boolean } {
  // Try to attach to an existing user-side plan with the same title.
  const existing = findPlanByTitle(args.title);
  if (existing && !existing.agentOnly) {
    toolCallToPlan.update((m) => ({ ...m, [args.toolCallId]: existing.id }));
    // Refresh the step list from the agent's plan (the agent may
    // have added/renumbered steps compared to what the user typed).
    setSteps(
      existing.id,
      args.steps.map((s) => ({
        id: s.id,
        title: s.title,
        status: s.status || 'pending',
        note: s.note,
      })),
    );
    return { planId: existing.id, isNew: false };
  }
  // No user-side plan: create a new agent-only entry.
  const now = Date.now();
  const id = uuid();
  const plan: Plan = {
    id,
    title: args.title.trim() || 'Plan',
    steps: args.steps.map((s) => ({
      id: s.id || uuid(),
      title: s.title,
      status: s.status || 'pending',
      note: s.note,
    })),
    createdAt: now,
    updatedAt: now,
    agentOnly: true,
  };
  plans.update((all) => [plan, ...all]);
  toolCallToPlan.update((m) => ({ ...m, [args.toolCallId]: id }));
  return { planId: id, isNew: true };
}

/** Apply an ai_step_updated event to the matching local plan.
 *  No-op if the tool call id is unknown (e.g. a leftover event
 *  from a previous session, or an agent-only plan that was
 *  deleted from the sidebar mid-flight). */
export function applyAgentStepUpdate(args: {
  toolCallId: string;
  stepId: string;
  status: PlanStepStatus;
  note?: string;
}): void {
  const planId = get(toolCallToPlan)[args.toolCallId];
  if (!planId) return;
  const patch: Partial<PlanStep> = { status: args.status };
  if (args.note != null) patch.note = args.note;
  updateStep(planId, args.stepId, patch);
}

// --- Prompt builders --------------------------------------------------

/** Build the user message we send to the model when the user clicks
 *  "Run" on a plan. The model is expected to call the `create_plan`
 *  tool with the same steps, then walk through them one by one
 *  calling `update_step`. The format mirrors what we already use
 *  for ad-hoc `create_plan` tool calls elsewhere in the app. */
export function buildPlanRunPrompt(plan: Plan): string {
  const lines = plan.steps.map((s, i) => `${i + 1}. ${s.title}`).join('\n');
  return [
    `[Запусти план «${plan.title}»]`,
    '',
    'Используй tool create_plan с этими шагами:',
    lines,
    '',
    'Затем последовательно выполняй каждый шаг. Перед каждым — вызывай',
    'update_step со status="in_progress". После выполнения — update_step',
    'со status="done" и кратким note с результатом. Если шаг упал —',
    'status="error" и описание.',
  ].join('\n');
}

/** Build the user message we send when the user clicks "Continue" on a
 *  plan that's already mid-flight. We only list the remaining steps
 *  (not yet done / error) so the model picks up where it left off. */
export function buildPlanContinuePrompt(plan: Plan): string {
  const remaining = plan.steps.filter(
    (s) => s.status !== 'done' && s.status !== 'error',
  );
  if (remaining.length === 0) {
    return [
      `[План «${plan.title}»]`,
      '',
      'Все шаги уже выполнены или завершены ошибкой. Кратко покажи итог плана.',
    ].join('\n');
  }
  const lines = remaining.map((s, i) => `${i + 1}. ${s.title}`).join('\n');
  return [
    `[Продолжи план «${plan.title}»]`,
    '',
    'Эти шаги остались (с прошлой попытки):',
    lines,
    '',
    'Продолжай с первого. Перед каждым — update_step status="in_progress",',
    'после — status="done" с коротким note. Если упал — status="error".',
  ].join('\n');
}

/** Find an existing plan by title (fuzzy case-insensitive match).
 *  Used to link an agent-created plan to a user-created plan when
 *  the user clicks Run. */
export function findPlanByTitle(title: string): Plan | undefined {
  const t = title.trim().toLowerCase();
  if (!t) return undefined;
  return get(plans).find((p) => p.title.trim().toLowerCase() === t);
}

/** Snapshot of store for tests / debugging. */
export function snapshot(): { plans: Plan[]; msgMap: PlanMessageMap } {
  return { plans: get(plans), msgMap: get(planMessageMap) };
}

/** Wipe the store. Currently unused; exposed for Settings "Reset plans". */
export function clearAll(): void {
  plans.set([]);
  planMessageMap.set({});
}

// --- derived: helpers ------------------------------------------------

/** Count plans by aggregate status. Used by the sidebar badge. */
export function summarize(all: Plan[]): { total: number; running: number; done: number; pending: number; error: number } {
  let running = 0, done = 0, pending = 0, error = 0;
  for (const p of all) {
    if (p.steps.length === 0) { pending++; continue; }
    let pDone = 0, pErr = 0, pRunning = 0;
    for (const s of p.steps) {
      if (s.status === 'done') pDone++;
      else if (s.status === 'error') pErr++;
      else if (s.status === 'in_progress') pRunning++;
    }
    if (pErr > 0) error++;
    else if (pRunning > 0) running++;
    else if (pDone === p.steps.length) done++;
    else pending++;
  }
  return { total: all.length, running, done, pending, error };
}
