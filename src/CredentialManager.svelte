<script lang="ts">
  // CredentialManager.svelte — modal for managing per-site credentials.
  //
  // Phase UX-2. The user adds `{site}/{field}` slots here (e.g.
  // `vk.com/username`, `vk.com/password`). Values live in the OS
  // keyring. The model only ever sees slot NAMES through the
  // `azazel_run` tool's `credentials` field; the real values are
  // injected into the browser session by the Rust supervisor and
  // never enter the model context.

  import { createEventDispatcher, onMount } from 'svelte';
  import {
    credentialList,
    credentialSet,
    credentialDelete,
    credentialGet,
    credentialValidateSlot,
    isValidCredentialSlot,
    type CredentialInfo,
  } from './lib/tauri';

  export let onClose: () => void;

  const dispatch = createEventDispatcher<{
    changed: { slot: string; added: boolean };
  }>();

  let items: CredentialInfo[] = [];
  let loading = true;
  let error: string | null = null;

  // Add form.
  let newSlot = '';
  let newValue = '';
  let showValue = false;
  let addBusy = false;
  let addError: string | null = null;

  // Filter.
  let filter = '';

  $: filtered = filter.trim()
    ? items.filter((it) => it.slot.toLowerCase().includes(filter.toLowerCase()))
    : items;

  // Per-row reveal state — track which slot the user is currently
  // looking at the value of. Default: hidden.
  let revealed: Record<string, string> = {};
  let revealBusy: Record<string, boolean> = {};

  onMount(refresh);

  async function refresh() {
    loading = true;
    error = null;
    try {
      items = await credentialList();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function addCredential() {
    addError = null;
    const slot = newSlot.trim().toLowerCase();
    if (!isValidCredentialSlot(slot)) {
      addError = 'Slot must be `{site}/{field}` — lowercase, dots in site, [a-z0-9_] in field. Example: vk.com/password';
      return;
    }
    const validationError = await credentialValidateSlot(slot);
    if (validationError) {
      addError = validationError;
      return;
    }
    if (!newValue) {
      addError = 'Value is required';
      return;
    }
    addBusy = true;
    try {
      await credentialSet(slot, newValue);
      await refresh();
      dispatch('changed', { slot, added: true });
      newSlot = '';
      newValue = '';
      showValue = false;
    } catch (e) {
      addError = String(e);
    } finally {
      addBusy = false;
    }
  }

  async function deleteSlot(slot: string) {
    if (!confirm(`Delete credential "${slot}"? This cannot be undone.`)) return;
    try {
      await credentialDelete(slot);
      delete revealed[slot];
      await refresh();
      dispatch('changed', { slot, added: false });
    } catch (e) {
      error = String(e);
    }
  }

  async function reveal(slot: string) {
    if (revealed[slot] !== undefined) {
      // Toggle off.
      delete revealed[slot];
      revealed = { ...revealed };
      return;
    }
    revealBusy = { ...revealBusy, [slot]: true };
    try {
      const v = await credentialGet(slot);
      revealed = { ...revealed, [slot]: v };
    } catch (e) {
      error = String(e);
    } finally {
      revealBusy = { ...revealBusy, [slot]: false };
    }
  }

  function mask(value: string): string {
    if (!value) return '';
    if (value.length <= 4) return '•'.repeat(value.length);
    return value.slice(0, 2) + '•'.repeat(Math.min(value.length - 4, 20)) + value.slice(-2);
  }
</script>

<div class="overlay" role="dialog" aria-modal="true" aria-label="Credentials">
  <div class="modal" data-testid="credential-manager">
    <header>
      <h2>🔑 Credentials</h2>
      <button class="close" type="button" on:click={onClose} title="Close" aria-label="Close">×</button>
    </header>

    <p class="intro">
      Slots like <code>vk.com/username</code> + <code>vk.com/password</code>.
      The model only sees slot names; values live in the OS keyring and are
      injected into the browser session by the Azazel supervisor.
    </p>

    {#if error}
      <div class="err" role="alert">⚠ {error}</div>
    {/if}

    <section class="add">
      <h3>Add credential</h3>
      <div class="row">
        <input
          class="slot"
          type="text"
          placeholder="site/field, e.g. vk.com/password"
          bind:value={newSlot}
          autocomplete="off"
          spellcheck="false"
          aria-label="Slot name"
        />
        {#if showValue}
          <input
            class="value"
            type="text"
            placeholder="value"
            bind:value={newValue}
            autocomplete="off"
            spellcheck="false"
            aria-label="Credential value"
          />
        {:else}
          <input
            class="value"
            type="password"
            placeholder="value"
            bind:value={newValue}
            autocomplete="off"
            spellcheck="false"
            aria-label="Credential value"
          />
        {/if}
        <button
          class="ghost"
          type="button"
          on:click={() => (showValue = !showValue)}
          title={showValue ? 'Hide value' : 'Show value'}
        >
          {showValue ? '🙈' : '👁'}
        </button>
        <button
          class="primary"
          type="button"
          on:click={addCredential}
          disabled={addBusy}
        >
          {addBusy ? 'Saving…' : 'Save'}
        </button>
      </div>
      {#if addError}
        <div class="err" role="alert">⚠ {addError}</div>
      {/if}
    </section>

    <section class="list">
      <div class="list-head">
        <h3>Stored ({items.length})</h3>
        <input
          class="filter"
          type="search"
          placeholder="Filter…"
          bind:value={filter}
          aria-label="Filter credentials"
        />
      </div>

      {#if loading}
        <p class="muted">Loading…</p>
      {:else if filtered.length === 0}
        <p class="muted">No credentials yet. Add one above — e.g. <code>vk.com/username</code> + <code>vk.com/password</code>.</p>
      {:else}
        <ul>
          {#each filtered as it (it.slot)}
            <li>
              <div class="li-slot">
                <code>{it.slot}</code>
                <span class="li-len">{it.value_length} chars</span>
              </div>
              <div class="li-value">
                {#if revealed[it.slot] !== undefined}
                  <code class="revealed">{revealed[it.slot]}</code>
                {:else}
                  <code class="masked">{mask('•'.repeat(it.value_length))}</code>
                {/if}
              </div>
              <div class="li-actions">
                <button
                  class="ghost"
                  type="button"
                  on:click={() => reveal(it.slot)}
                  disabled={!!revealBusy[it.slot]}
                  title={revealed[it.slot] !== undefined ? 'Hide' : 'Show value'}
                >
                  {revealed[it.slot] !== undefined ? '🙈' : '👁'}
                </button>
                <button
                  class="ghost danger"
                  type="button"
                  on:click={() => deleteSlot(it.slot)}
                  title="Delete"
                >
                  🗑
                </button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>
</div>

<style>
  .overlay {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: var(--bg-elevated, #1c1c20);
    color: var(--text, #e6e6ea);
    border: 1px solid var(--border, #2a2a2e);
    border-radius: 12px;
    width: 560px;
    max-width: 90vw;
    max-height: 80vh;
    overflow-y: auto;
    padding: 20px 24px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5);
  }
  header { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; font-size: 18px; }
  h3 { margin: 16px 0 8px 0; font-size: 13px; color: var(--text-muted, #9aa0a6); text-transform: uppercase; letter-spacing: 0.5px; }
  .close {
    background: transparent; border: 0; color: var(--text-muted, #9aa0a6);
    font-size: 24px; line-height: 1; cursor: pointer; padding: 0 4px;
  }
  .close:hover { color: var(--text, #e6e6ea); }
  .intro { color: var(--text-muted, #9aa0a6); font-size: 12px; line-height: 1.5; margin: 8px 0 16px 0; }
  .intro code { background: var(--bg, #14141a); padding: 1px 4px; border-radius: 3px; }
  .err {
    background: rgba(220, 80, 80, 0.15);
    color: #ffaaaa;
    border: 1px solid rgba(220, 80, 80, 0.4);
    padding: 6px 10px; border-radius: 6px; font-size: 12px; margin: 8px 0;
  }
  .add { margin-bottom: 20px; }
  .row { display: flex; gap: 6px; }
  .row > input {
    background: var(--bg, #14141a);
    color: var(--text, #e6e6ea);
    border: 1px solid var(--border, #2a2a2e);
    border-radius: 6px;
    padding: 6px 10px;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 12px;
  }
  .row > input.slot { flex: 0 0 220px; }
  .row > input.value { flex: 1; }
  .row > input:focus { outline: 1px solid var(--accent, #8a7cff); border-color: var(--accent, #8a7cff); }
  .row > button {
    border: 1px solid var(--border, #2a2a2e);
    background: var(--bg, #14141a);
    color: var(--text, #e6e6ea);
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 13px;
    cursor: pointer;
  }
  .row > button.primary {
    background: var(--accent, #8a7cff);
    color: white; border-color: var(--accent, #8a7cff);
  }
  .row > button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .row > button.ghost { padding: 6px 8px; }
  .list-head { display: flex; align-items: center; gap: 8px; }
  .list-head .filter { flex: 1; max-width: 220px; }
  .filter {
    background: var(--bg, #14141a);
    color: var(--text, #e6e6ea);
    border: 1px solid var(--border, #2a2a2e);
    border-radius: 6px;
    padding: 4px 8px;
    font-size: 12px;
  }
  .filter:focus { outline: 1px solid var(--accent, #8a7cff); }
  ul { list-style: none; padding: 0; margin: 8px 0 0 0; }
  li {
    display: grid;
    grid-template-columns: 220px 1fr auto;
    gap: 8px;
    align-items: center;
    padding: 6px 4px;
    border-bottom: 1px solid var(--border, #2a2a2e);
  }
  li:last-child { border-bottom: 0; }
  .li-slot { display: flex; flex-direction: column; gap: 2px; }
  .li-slot code { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 12px; color: var(--text, #e6e6ea); }
  .li-len { font-size: 10px; color: var(--text-muted, #9aa0a6); }
  .li-value code {
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 12px;
    word-break: break-all;
  }
  .li-value code.masked { color: var(--text-muted, #9aa0a6); }
  .li-value code.revealed { color: #ffd0a0; background: rgba(255, 200, 100, 0.1); padding: 1px 4px; border-radius: 3px; }
  .li-actions { display: flex; gap: 4px; }
  .li-actions button { background: transparent; border: 0; cursor: pointer; padding: 4px 6px; border-radius: 4px; font-size: 14px; }
  .li-actions button:hover { background: var(--bg-hover, #2a2a30); }
  .li-actions button.danger:hover { background: rgba(220, 80, 80, 0.2); }
  .muted { color: var(--text-muted, #9aa0a6); font-size: 12px; }
</style>
