// src/lib/videomode-store.ts
// Svelte stores for the video-mode UI. Centralises state so the components
// stay declarative.

import { writable, derived, type Readable, type Writable } from 'svelte/store';
import type {
  AgentHintPayload,
  CaptureErrorPayload,
  CaptureStatePayload,
  MonitorInfo,
  ScreenFramePayload,
  VideoAutoTriggerPayload,
} from './tauri';

export const monitors: Writable<MonitorInfo[]> = writable([]);
export const selectedMonitorId: Writable<number> = writable(0);
export const fps: Writable<number> = writable(1.0);
export const maxWidth: Writable<number> = writable(1280);
export const goal: Writable<string> = writable('');
export const consentAccepted: Writable<boolean> = writable(
  typeof localStorage !== 'undefined' &&
    localStorage.getItem('luna.videomode.consent') === '1',
);

export const running: Writable<boolean> = writable(false);
export const captureState: Writable<CaptureStatePayload | null> = writable(null);
export const latestFrame: Writable<ScreenFramePayload | null> = writable(null);
export const lastError: Writable<CaptureErrorPayload | null> = writable(null);

export const hints: Writable<AgentHintPayload[]> = writable([]);

export const minimaxKeyStatus: Writable<'unknown' | 'set' | 'missing'> =
  writable('unknown');

// ---------- Video ↔ Chat bridge ----------

/** Master switch for the auto-invoke bridge. Persisted in localStorage
 *  so it survives page reloads. The first read seeds the value; the
 *  helper `setVideoAutoInvoke(value)` writes to localStorage and
 *  pushes the new value to the backend (see `subscribeToAutoTriggers`). */
const AUTOINVOKE_KEY = 'luna.video.autoinvoke';
function readAutoInvokeDefault(): boolean {
  try {
    const raw = localStorage.getItem(AUTOINVOKE_KEY);
    if (raw === '0' || raw === 'false') return false;
    // Default ON (per the implementation plan).
    return true;
  } catch {
    return true;
  }
}
export const videoAutoInvoke: Writable<boolean> = writable(readAutoInvokeDefault());

/** Mirrors the backend's `auto_invocations_used` counter. Updated on
 *  every `capture-state` event. */
export const autoInvocationsUsed: Writable<number> = writable(0);

/** When a `video-auto-trigger` lands, this holds the most recent
 *  payload so a listener mounted AFTER the event can still pick it
 *  up via the Rust single-slot (see `takePendingVideoAutoInvoke`). */
export const lastAutoTrigger: Writable<VideoAutoTriggerPayload | null> =
  writable(null);

export async function setVideoAutoInvoke(value: boolean) {
  videoAutoInvoke.set(value);
  try {
    localStorage.setItem(AUTOINVOKE_KEY, value ? '1' : '0');
  } catch {
    /* no-op */
  }
  // Mirror to the backend so `hint_loop` reads the right value.
  try {
    const { setVideoAutoinvoke } = await import('./tauri');
    await setVideoAutoinvoke(value);
  } catch {
    /* no-op */
  }
}

/** Subscribe to `video-auto-trigger` events. Returns an unlisten
 *  function (matches the rest of the Tauri listener surface). */
export async function subscribeToAutoTriggers(
  handler: (p: VideoAutoTriggerPayload) => void,
): Promise<() => void> {
  const { onVideoAutoTrigger } = await import('./tauri');
  return onVideoAutoTrigger((p) => {
    lastAutoTrigger.set(p);
    handler(p);
  });
}

export const budgetExhausted: Readable<boolean> = derived(
  captureState,
  ($s) =>
    !!$s && $s.frames_budget > 0 && $s.frames_sent >= $s.frames_budget,
);

export const goalCharCount: Readable<number> = derived(
  goal,
  ($g) => $g.length,
);

export const goalOk: Readable<boolean> = derived(goal, ($g) => $g.trim().length > 0);

export const canStart: Readable<boolean> = derived(
  [running, consentAccepted, goalOk, monitors],
  ([$running, $consent, $goalOk, $monitors]) =>
    !$running && $consent && $goalOk && $monitors.length > 0,
);

export function pushHint(h: AgentHintPayload) {
  hints.update(($h) => [h, ...$h].slice(0, 50));
}
export function clearHints() {
  hints.set([]);
}
export function acceptConsent() {
  consentAccepted.set(true);
  try {
    localStorage.setItem('luna.videomode.consent', '1');
  } catch {
    /* no-op */
  }
}
export function revokeConsent() {
  consentAccepted.set(false);
  try {
    localStorage.removeItem('luna.videomode.consent');
  } catch {
    /* no-op */
  }
}

export async function refreshMinimaxKeyStatus() {
  try {
    const { getApiKey } = await import('./tauri');
    const k = await getApiKey('minimax');
    minimaxKeyStatus.set(k && k.length > 0 ? 'set' : 'missing');
  } catch {
    minimaxKeyStatus.set('missing');
  }
}
