<script lang="ts">
  // src/ThreeDOutliner.svelte
  // Tree view of the scene. Click to select, right-click to remove.
  // Subscribes to the scene store directly.

  import { getSceneStore, flattenScene, type SceneNode } from './lib/three_d_store';

  const store = getSceneStore();
  $: flat = flattenScene($store.scene);
  $: selectedId = $store.selectedId;

  function selectNode(n: SceneNode) {
    store.select(n.id);
  }

  function remove(n: SceneNode, ev: MouseEvent) {
    ev.stopPropagation();
    if (!confirm(`Delete "${n.name}"?`)) return;
    store.pushOp({ kind: 'remove_node', id: n.id });
  }
</script>

<div class="outliner">
  <div class="header">Outliner</div>
  {#if flat.length === 0}
    <div class="empty">
      <div>Empty scene</div>
      <p>Add a primitive or ask the AI to build one.</p>
    </div>
  {:else}
    <ul class="tree">
      {#each flat as node (node.id)}
        <li
          class="node"
          class:selected={node.id === selectedId}
          on:click={() => selectNode(node)}
          on:contextmenu={(e) => remove(node, e)}
          title="Click to select • Right-click to remove">
          <span class="icon">{node.kind === 'group' ? '🗂' : iconFor(node)}</span>
          <span class="name">{node.name}</span>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<script context="module" lang="ts">
  function iconFor(n: SceneNode): string {
    if (n.kind !== 'mesh') return '⬚';
    return ({
      box: '□', sphere: '○', plane: '▱', cylinder: '⌭', torus: '◯', cone: '△', capsule: '⊜',
    } as Record<string, string>)[n.primitive] ?? '⬚';
  }
</script>

<style>
  .outliner {
    display: flex; flex-direction: column;
    background: var(--bg-elevated);
    border-right: 1px solid var(--border);
    overflow: hidden;
  }
  .header {
    padding: 8px 12px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.5px;
    text-transform: uppercase;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
  }
  .empty {
    padding: 16px 12px;
    color: var(--text-muted);
    font-size: 12px;
  }
  .empty p { margin: 4px 0 0 0; font-size: 11px; color: #6c7280; }
  .tree {
    list-style: none; margin: 0; padding: 4px 0;
    overflow-y: auto; flex: 1;
  }
  .node {
    display: flex; align-items: center; gap: 8px;
    padding: 5px 12px;
    font-size: 12px;
    color: var(--text-muted);
    cursor: pointer;
    user-select: none;
  }
  .node:hover { background: var(--bg-hover); color: var(--text); }
  .node.selected {
    background: var(--accent-soft);
    color: var(--accent-strong);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .icon { font-size: 13px; width: 16px; text-align: center; }
  .name { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
</style>
