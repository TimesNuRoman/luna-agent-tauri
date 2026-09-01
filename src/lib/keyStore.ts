// src/lib/keyStore.ts
// Single source of truth for "do we have a MiniMax API key?".
//
// Three Svelte components used to check this independently:
//   - App.svelte (keyMissing → 🔑 pill in the topbar)
//   - Chat.svelte (hasMinimax → system message + composer placeholder)
//   - Settings.svelte (hasMinimax → "set/missing" badge)
//
// That duplication caused the visible bug: pill says "no key" but the
// chat's startup system message already assumed a key was set. Now all
// three components subscribe to this store. Mutators (`refreshKeyStatus`,
// `setKeyStatus`, `clearKeyStatus`) are the only path to a status change,
// so the UI is guaranteed to be in sync.

import { writable, type Writable } from 'svelte/store';
import { getApiKey, setApiKey } from './tauri';

export type ApiKeyStatus = 'unknown' | 'present' | 'missing';

/** Writable store. The default 'unknown' means we haven't asked keyring yet. */
export const apiKeyStatus: Writable<ApiKeyStatus> = writable('unknown');

/** True while a `refreshKeyStatus()` is in flight. UI can show a spinner. */
export const apiKeyLoading: Writable<boolean> = writable(false);

/**
 * Read the key from the keyring and publish the result. Safe to call from
 * multiple `onMount` handlers — only the first one actually does IPC; the
 * rest short-circuit while a refresh is in flight.
 */
let inflight: Promise<void> | null = null;
export function refreshKeyStatus(): Promise<void> {
  if (inflight) return inflight;
  apiKeyLoading.set(true);
  inflight = (async () => {
    try {
      const k = await getApiKey('minimax').catch(() => null);
      apiKeyStatus.set(k ? 'present' : 'missing');
    } finally {
      apiKeyLoading.set(false);
      inflight = null;
    }
  })();
  return inflight;
}

/**
 * Persist a new key and reflect the new status immediately. Optimistic:
 * we update the store *before* the IPC returns because keyring writes
 * are synchronous on the Rust side and there's no useful rollback path.
 */
export async function saveKey(value: string): Promise<void> {
  const trimmed = value.trim();
  await setApiKey('minimax', trimmed);
  apiKeyStatus.set(trimmed ? 'present' : 'missing');
}

/** Clear the key (same as saving an empty string). */
export async function clearKey(): Promise<void> {
  await saveKey('');
}
