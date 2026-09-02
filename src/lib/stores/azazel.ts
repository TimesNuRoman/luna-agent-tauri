// src/lib/stores/azazel.ts
// A small Svelte store that holds the live state of the Azazel
// supervisor: the latest frame per task, the action history, and
// the current approval (if any). Components subscribe via
// `azazelStore.subscribe` to react to Tauri events.

import { writable, derived, type Readable, type Writable } from 'svelte/store';

import {
  onAzazelStep,
  onAzazelApprovalNeeded,
  onAzazelDone,
  onAzazelError,
  type AzazelStepEvent,
  type AzazelApprovalEvent,
  type AzazelDoneEvent,
  type AzazelErrorEvent,
  type ApprovalPolicy,
} from '../azazel';

export type AzazelAction = {
  ts: string;
  tool: string;
  preview?: string;
  is_error: boolean;
  step_n: number;
};

export type AzazelTaskState = {
  task_id: string;
  /** Latest screenshot as a data URL. */
  latest_screenshot: string | null;
  /** Most recent action the agent took. */
  actions: AzazelAction[];
  /** Set while the task is running; cleared on `azazel:done`. */
  running: boolean;
  /** Final summary on `azazel:done`, else null. */
  final_summary: string | null;
  /** Set on terminal failure; clears running. */
  final_error: string | null;
};

export type AzazelStore = {
  /** task_id → live state. */
  tasks: Writable<Record<string, AzazelTaskState>>;
  /** Pending approval across all browser-kind tasks. Only one at a time per task. */
  pending_approval: Writable<AzazelApprovalEvent | null>;
  /** Last browser-level state snapshot (for the sidebar badge). */
  browser_state: Writable<{ running_task_count: number; last_frame_seq: number; launched: boolean } | null>;
  /** Active policy. */
  policy: Writable<ApprovalPolicy>;
};

function emptyTask(task_id: string): AzazelTaskState {
  return {
    task_id,
    latest_screenshot: null,
    actions: [],
    running: true,
    final_summary: null,
    final_error: null,
  };
}

export const azazelStore: AzazelStore = {
  tasks: writable<Record<string, AzazelTaskState>>({}),
  pending_approval: writable<AzazelApprovalEvent | null>(null),
  browser_state: writable(null),
  policy: writable<ApprovalPolicy>('normal'),
};

/** Wire up the Tauri event listeners. Idempotent — call once at
 * App boot. Returns an unsubscribe function for hot-reload. */
export function startAzazelListeners(): () => void {
  const stopStep = onAzazelStep((e: AzazelStepEvent) => {
    azazelStore.tasks.update((all) => {
      const cur = all[e.task_id] ?? emptyTask(e.task_id);
      return {
        ...all,
        [e.task_id]: {
          ...cur,
          latest_screenshot:
            (e.screenshot_b64 as string | undefined) ?? cur.latest_screenshot,
          actions: [
            ...cur.actions,
            {
              ts: e.ts,
              tool: e.tool,
              preview: e.preview,
              is_error: e.is_error,
              step_n: e.step_n,
            },
          ].slice(-200), // keep the last 200 actions
        },
      };
    });
  });

  const stopApproval = onAzazelApprovalNeeded((e) => {
    azazelStore.pending_approval.set(e);
  });

  const stopDone = onAzazelDone((e: AzazelDoneEvent) => {
    azazelStore.tasks.update((all) => {
      const cur = all[e.task_id] ?? emptyTask(e.task_id);
      return {
        ...all,
        [e.task_id]: {
          ...cur,
          running: false,
          final_summary: e.summary,
          final_error:
            e.status === 'failed' || e.status === 'timed_out'
              ? e.summary
              : cur.final_error,
        },
      };
    });
    // If the approval that just resolved was for this task, clear
    // the modal.
    azazelStore.pending_approval.update((p) =>
      p && p.task_id === e.task_id ? null : p,
    );
  });

  const stopErr = onAzazelError((e: AzazelErrorEvent) => {
    azazelStore.tasks.update((all) => {
      const cur = all[e.task_id] ?? emptyTask(e.task_id);
      return {
        ...all,
        [e.task_id]: {
          ...cur,
          running: false,
          final_error: e.error,
        },
      };
    });
  });

  return () => {
    stopStep();
    stopApproval();
    stopDone();
    stopErr();
  };
}

/** Convenience derived store: just the IDs of currently-running tasks. */
export const runningTaskIds: Readable<string[]> = derived(
  azazelStore.tasks,
  ($t) => Object.values($t).filter((t) => t.running).map((t) => t.task_id),
);
