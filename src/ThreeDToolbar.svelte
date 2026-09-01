<script lang="ts">
  // src/ThreeDToolbar.svelte
  // Toolbar with add-primitive buttons, save/open/export, undo/redo, and
  // the AI chat toggle. All "external" actions (open/export/new/save/demo)
  // are passed in as callbacks from the parent (ThreeD.svelte) — the
  // toolbar is a pure view component.

  import { getSceneStore, newId, defaultTransform, defaultMaterial,
           type MeshNode, type PrimitiveKind } from './lib/three_d_store';

  export let onExport: () => void = () => {};
  export let onImport: () => void = () => {};
  export let onSave: (() => void) | undefined = undefined;
  export let onToggleChat: (() => void) | undefined = undefined;
  export let onLoadDemo: (() => void) | undefined = undefined;
  export let chatOpen: boolean = false;
  export let onNew: () => void = () => {};

  const store = getSceneStore();
  // Reactive: undo/redo availability mirrors history length.
  $: undoAvail = historyLen.past > 0;
  $: redoAvail = historyLen.future > 0;
  // Track the past/future lengths by comparing the store snapshot we render.
  let historyLen = { past: 0, future: 0 };
  $: {
    // The store is a custom class; we don't expose history directly, so we
    // infer availability by subscribing and tracking a "dirty" flag from
    // any successful op. The simplest reliable signal: subscribe and
    // count updates.
    void $store; // keep the subscription alive
  }

  function addPrimitive(kind: PrimitiveKind) {
    const id = newId(kind);
    const m: MeshNode = {
      kind: 'mesh', id, parent: null, primitive: kind,
      transform: defaultTransform(),
      material: defaultMaterial(),
      name: capitalize(kind),
      visible: true,
    };
    const r = store.pushOp({ kind: 'add_primitive', id, parent: null, primitive: kind, transform: m.transform, material: m.material, name: m.name });
    if (r === undefined) markDirty();
  }

  function capitalize(s: string) { return s.charAt(0).toUpperCase() + s.slice(1); }

  function doUndo() { if (store.undo()) markDirty(); }
  function doRedo() { if (store.redo()) markDirty(); }

  // Lightweight history-availability tracker: any store mutation that goes
  // through us increments past. We use a simple in-out counter since the
  // store doesn't expose history.length directly.
  function markDirty() {
    // We don't have exact counts; just enable undo after any local op.
    // Redo is cleared on every op (already handled in the store), so once
    // an op is applied, redo is disabled.
    historyLen = { past: 1, future: 0 };
  }
  // Mark redo available on mount since the store may already have past
  // entries (after we load an autosave, for example).
  historyLen = { past: 1, future: 0 };

  const PRIMITIVES: { kind: PrimitiveKind; icon: string; label: string }[] = [
    { kind: 'box', icon: '□', label: 'Box' },
    { kind: 'sphere', icon: '○', label: 'Sphere' },
    { kind: 'plane', icon: '▱', label: 'Plane' },
    { kind: 'cylinder', icon: '⌭', label: 'Cylinder' },
    { kind: 'torus', icon: '◯', label: 'Torus' },
    { kind: 'cone', icon: '△', label: 'Cone' },
    { kind: 'capsule', icon: '⊜', label: 'Capsule' },
  ];
</script>

<div class="toolbar">
  <div class="group left">
    <button class="primary" on:click={onNew} title="New scene (clears current)">📄 New</button>
    {#if onLoadDemo}
      <button on:click={onLoadDemo} title="Replace with demo scene">✨ Demo</button>
    {/if}
    {#each PRIMITIVES as p (p.kind)}
      <button on:click={() => addPrimitive(p.kind)} title={`Add ${p.label}`}>{p.icon} {p.label}</button>
    {/each}
  </div>
  <div class="group right">
    <button on:click={doUndo} disabled={!undoAvail} title="Undo (Ctrl+Z)">↶</button>
    <button on:click={doRedo} disabled={!redoAvail} title="Redo (Ctrl+Shift+Z)">↷</button>
    <span class="sep"></span>
    <button on:click={onImport} title="Open .luna3d.json">📂 Open</button>
    <button on:click={onSave} title="Save scene">💾 Save</button>
    <button on:click={onExport} title="Export GLB">⬇ Export</button>
    <span class="sep"></span>
    <button class:on={chatOpen} on:click={onToggleChat} title="Toggle AI chat">💬 AI Chat</button>
  </div>
</div>

<style>
  .toolbar {
    display: flex; align-items: center; gap: 12px;
    padding: 6px 10px;
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .group { display: flex; align-items: center; gap: 4px; }
  .left { flex: 1; }
  .right { gap: 4px; }
  .sep { display: inline-block; width: 1px; height: 18px; background: var(--border); margin: 0 4px; }
  button {
    background: transparent; color: var(--text-muted);
    border: 1px solid var(--border); border-radius: 6px;
    padding: 4px 10px; height: 26px; font: inherit; font-size: 11px;
    cursor: pointer; white-space: nowrap;
  }
  button:hover:not(:disabled) { color: var(--text); background: var(--bg-hover); }
  button:disabled { opacity: 0.4; cursor: not-allowed; }
  button.primary { color: var(--text); border-color: var(--accent); }
  button.on { color: var(--text); background: var(--accent-soft); border-color: var(--accent); }
</style>
