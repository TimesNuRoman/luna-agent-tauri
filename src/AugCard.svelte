<script lang="ts">
  // AugCard.svelte — chat-side panel for an active augmentation.
  //
  // Renders a small dismissible card with the aug's icon, label, and
  // the args that activated it. Two actions: 📌 pin/unpin (the aug
  // stays across the next user message) and "open fullscreen" (uses
  // the luna:switch-tab event to jump to the legacy tab while the
  // migration is in progress; P4 routes removed tabs through the aug
  // system instead).
  //
  // If the registered aug descriptor carries a `component`, that
  // component is rendered as the card body, receiving the full
  // AugProps so it can host its own rich UI (form, timeline, etc.).

  import type { AugProps } from './lib/augmentations';

  export let augId: AugProps['augId'];
  export let instanceId: string;
  export let args: string = '';
  export let pinned: boolean = false;
  export let onDismiss: () => void;
  export let onTogglePin: () => void;

  // Resolve the aug descriptor from the registry at render time.
  // The registry is a plain Map; reading it per-render is cheap.
  import { get } from './lib/augmentations';
  $: aug = get(augId);

  function openFullscreen() {
    if (!aug?.fullscreenTab) return;
    window.dispatchEvent(
      new CustomEvent('luna:switch-tab', { detail: aug.fullscreenTab })
    );
  }
</script>

{#if aug}
  <div
    class="aug-card"
    class:pinned
    data-aug-id={aug.id}
    data-instance={instanceId}
    role="region"
    aria-label="{aug.label} augmentation"
  >
    <header class="aug-head">
      <span class="aug-icon" aria-hidden="true">{aug.icon}</span>
      <span class="aug-label">{aug.label}</span>
      <span class="aug-spacer" />
      <button
        class="aug-btn"
        type="button"
        title={pinned ? 'Unpin' : 'Pin across next message'}
        aria-pressed={pinned}
        on:click={onTogglePin}
      >
        {pinned ? '📌' : '📍'}
      </button>
      {#if aug.fullscreenTab}
        <button
          class="aug-btn"
          type="button"
          title="Open fullscreen view"
          on:click={openFullscreen}
        >
          ⤢
        </button>
      {/if}
      <button
        class="aug-btn"
        type="button"
        title="Dismiss"
        aria-label="Dismiss augmentation"
        on:click={onDismiss}
      >
        ×
      </button>
    </header>
    {#if args}
      <p class="aug-args">{args}</p>
    {/if}
    {#if aug.body}
      <div class="aug-body">
        <svelte:component
          this={aug.body}
          {instanceId}
          {augId}
          {args}
          {pinned}
          {onDismiss}
          {onTogglePin}
        />
      </div>
    {/if}
  </div>
{:else}
  <div class="aug-card aug-missing" data-aug-id={augId}>
    <span class="aug-icon" aria-hidden="true">⚠️</span>
    <span class="aug-label">Unknown aug: {augId}</span>
    <button
      class="aug-btn"
      type="button"
      title="Dismiss"
      on:click={onDismiss}
    >×</button>
  </div>
{/if}

<style>
  .aug-card {
    border: 1px solid var(--border, #2a2a2e);
    background: var(--bg-elevated, #1c1c20);
    border-radius: 8px;
    padding: 8px 10px;
    margin: 6px 12px;
    font-size: 12px;
    color: var(--text, #e6e6ea);
    transition: opacity 140ms ease, border-color 140ms ease;
  }
  .aug-card.pinned {
    border-color: var(--accent, #8a7cff);
  }
  .aug-missing {
    opacity: 0.6;
  }
  .aug-head {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .aug-icon { font-size: 14px; }
  .aug-label {
    font-weight: 600;
    letter-spacing: 0.2px;
  }
  .aug-spacer { flex: 1; }
  .aug-btn {
    background: transparent;
    border: 0;
    color: var(--text-muted, #9aa0a6);
    cursor: pointer;
    padding: 0 4px;
    font-size: 13px;
    line-height: 1;
    border-radius: 4px;
  }
  .aug-btn:hover { color: var(--text, #e6e6ea); background: var(--bg-hover, #2a2a30); }
  .aug-args {
    margin: 4px 0 0 0;
    color: var(--text-muted, #9aa0a6);
    font-size: 11px;
    word-break: break-word;
  }
  .aug-body {
    margin-top: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border, #2a2a2e);
    font-size: 12px;
  }
</style>
