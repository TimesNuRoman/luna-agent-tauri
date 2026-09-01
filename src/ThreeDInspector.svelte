<script lang="ts">
  // src/ThreeDInspector.svelte
  // Property editor for the currently selected node. All edits go through
  // store.pushOp so the same channel serves UI edits and AI-driven edits.

  import { getSceneStore, type SceneNode, type Transform, type MaterialState,
           type NodePatch, type MeshNode } from './lib/three_d_store';
  import { invoke } from './lib/tauri';
  import { apiKeyStatus } from './lib/keyStore';

  export let onToast: (text: string, kind: 'info' | 'error') => void = () => {};

  const store = getSceneStore();
  $: selected = $store.selectedId ? findNode($store.scene, $store.selectedId) : null;

  function findNode(scene: SceneNode[], id: string): SceneNode | null {
    for (const n of scene) {
      if (n.id === id) return n;
      if (n.kind === 'group') {
        const sub = findNode(n.children, id);
        if (sub) return sub;
      }
    }
    return null;
  }

  // -- helpers that accept an Event and pull the value out --
  function valOf(ev: Event): string { return (ev.target as HTMLInputElement).value; }
  function valChecked(ev: Event): boolean { return (ev.target as HTMLInputElement).checked; }
  function valNumber(ev: Event): number { return parseFloat(valOf(ev)) || 0; }

  function setTransformVec3(field: 'position' | 'rotation' | 'scale', axis: 0 | 1 | 2, value: number) {
    if (!selected) return;
    const t: Transform = {
      position: [...selected.transform.position] as Transform['position'],
      rotation: [...selected.transform.rotation] as Transform['rotation'],
      scale: [...selected.transform.scale] as Transform['scale'],
    };
    t[field][axis] = value;
    const patch: NodePatch = { field: 'transform', value: t };
    store.pushOp({ kind: 'update_node', id: selected.id, patch });
  }
  function onTransformChange(field: 'position' | 'rotation' | 'scale', axis: 0 | 1 | 2) {
    return (e: Event) => setTransformVec3(field, axis, valNumber(e));
  }

  function setColor(value: string) {
    if (!selected || selected.kind !== 'mesh') return;
    const mat: MaterialState = { ...selected.material, color: value };
    store.pushOp({ kind: 'update_node', id: selected.id, patch: { field: 'material', value: mat } });
  }
  function onColorChange(e: Event) { setColor(valOf(e)); }

  function setScalar(field: 'metalness' | 'roughness', value: number) {
    if (!selected || selected.kind !== 'mesh') return;
    const mat: MaterialState = { ...selected.material, [field]: value };
    store.pushOp({ kind: 'update_node', id: selected.id, patch: { field: 'material', value: mat } });
  }
  function onMetalness(e: Event) { setScalar('metalness', valNumber(e)); }
  function onRoughness(e: Event) { setScalar('roughness', valNumber(e)); }

  function setVisible(value: boolean) {
    if (!selected) return;
    store.pushOp({ kind: 'update_node', id: selected.id, patch: { field: 'visible', value } });
  }
  function onVisibleChange(e: Event) { setVisible(valChecked(e)); }

  function setName(value: string) {
    if (!selected) return;
    store.pushOp({ kind: 'update_node', id: selected.id, patch: { field: 'name', value } });
  }
  function onNameChange(e: Event) { setName(valOf(e)); }

  // -- texture generation --
  let texturePrompt = '';
  let generating = false;
  async function generateTexture() {
    if (!selected || selected.kind !== 'mesh') return;
    if ($apiKeyStatus !== 'present') { onToast('MiniMax API key not set. Open Settings.', 'error'); return; }
    if (!texturePrompt.trim()) { onToast('Enter a prompt first.', 'error'); return; }
    generating = true;
    store.setLoading({ kind: 'image', label: 'Generating texture…' });
    try {
      const dataUrl = await invoke<string>('three_d_generate_texture', {
        prompt: texturePrompt,
        aspectRatio: '1:1',
      });
      store.pushOp({ kind: 'apply_texture', id: selected.id, prompt: texturePrompt, dataUrl });
      onToast('Texture applied.', 'info');
    } catch (e: any) {
      onToast(`Texture failed: ${e?.message ?? e}`, 'error');
    } finally {
      generating = false;
      store.setLoading({ kind: null, label: '' });
    }
  }
</script>

<div class="inspector">
  <div class="header">Inspector</div>
  {#if !selected}
    <div class="empty">
      <div>No selection</div>
      <p>Click an object in the viewport or outliner.</p>
    </div>
  {:else}
    <div class="body">
      <label class="row">
        <span class="lbl">Name</span>
        <input type="text" value={selected.name} on:change={onNameChange} />
      </label>

      <label class="row">
        <span class="lbl">Visible</span>
        <input type="checkbox" checked={selected.visible} on:change={onVisibleChange} />
      </label>

      <fieldset class="group">
        <legend>Transform</legend>
        {#each ['position', 'rotation', 'scale'] as field (field)}
          <div class="vec3">
            <span class="vec3-name">{field}</span>
            {#each ['x', 'y', 'z'] as axis, idx (axis)}
              <input
                type="number"
                step={field === 'rotation' ? '0.01' : '0.1'}
                value={selected.transform[field][idx]}
                on:change={onTransformChange(field, idx)} />
            {/each}
          </div>
        {/each}
      </fieldset>

      {#if selected.kind === 'mesh'}
        <fieldset class="group">
          <legend>Material</legend>
          <label class="row">
            <span class="lbl">Color</span>
            <input type="color" value={selected.material.color} on:change={onColorChange} />
            <span class="hex">{selected.material.color}</span>
          </label>
          <label class="row">
            <span class="lbl">Metalness</span>
            <input type="range" min="0" max="1" step="0.01" value={selected.material.metalness}
              on:input={onMetalness} />
            <span class="num">{selected.material.metalness.toFixed(2)}</span>
          </label>
          <label class="row">
            <span class="lbl">Roughness</span>
            <input type="range" min="0" max="1" step="0.01" value={selected.material.roughness}
              on:input={onRoughness} />
            <span class="num">{selected.material.roughness.toFixed(2)}</span>
          </label>
          <div class="texture">
            {#if selected.material.textureDataUrl}
              <img src={selected.material.textureDataUrl} alt="texture" />
              <div class="texture-meta">
                <span class="texture-prompt">{selected.material.texturePrompt ?? '(no prompt)'}</span>
              </div>
            {/if}
            <div class="gen-row">
              <input type="text" placeholder="e.g. wooden planks" bind:value={texturePrompt} />
              <button type="button" class="primary" disabled={generating} on:click={generateTexture}>
                {generating ? 'Generating…' : 'Generate'}
              </button>
            </div>
            <div class="hint">Powered by MiniMax image-01</div>
          </div>
        </fieldset>
      {/if}
    </div>
  {/if}
</div>

<style>
  .inspector {
    display: flex; flex-direction: column;
    background: var(--bg-elevated);
    border-left: 1px solid var(--border);
    overflow: hidden;
  }
  .header {
    padding: 8px 12px;
    font-size: 11px; font-weight: 600; letter-spacing: 0.5px; text-transform: uppercase;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
  }
  .empty { padding: 16px 12px; color: var(--text-muted); font-size: 12px; }
  .empty p { margin: 4px 0 0 0; font-size: 11px; color: #6c7280; }
  .body { padding: 8px 12px; overflow-y: auto; flex: 1; }
  .row { display: flex; align-items: center; gap: 8px; margin: 4px 0; font-size: 12px; }
  .lbl { color: var(--text-muted); width: 70px; }
  .hex { color: #6c7280; font-family: ui-monospace, monospace; font-size: 11px; }
  .num { color: #6c7280; font-family: ui-monospace, monospace; font-size: 11px; width: 28px; text-align: right; }
  input[type=text] { flex: 1; background: #0f1217; color: #e6e8eb; border: 1px solid #2c313a; border-radius: 4px; padding: 4px 6px; font: inherit; }
  input[type=number] { width: 60px; background: #0f1217; color: #e6e8eb; border: 1px solid #2c313a; border-radius: 4px; padding: 4px 6px; font: inherit; }
  input[type=color] { width: 30px; height: 22px; padding: 0; border: 1px solid #2c313a; background: transparent; }
  input[type=range] { flex: 1; }
  .group {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 8px 4px 8px;
    margin: 10px 0;
  }
  .group legend { font-size: 10px; color: var(--text-muted); padding: 0 4px; text-transform: uppercase; letter-spacing: 0.5px; }
  .vec3 { display: flex; gap: 4px; align-items: center; margin: 4px 0; }
  .vec3-name { width: 50px; color: var(--text-muted); font-size: 11px; }
  .texture { margin-top: 8px; padding-top: 8px; border-top: 1px dashed var(--border); }
  .texture img { width: 100%; border-radius: 4px; display: block; }
  .texture-meta { padding: 4px 0 8px 0; }
  .texture-prompt { color: var(--text-muted); font-size: 11px; }
  .gen-row { display: flex; gap: 4px; margin-top: 4px; }
  .gen-row input { flex: 1; }
  .gen-row button { white-space: nowrap; padding: 4px 8px; }
  .hint { color: #6c7280; font-size: 10px; margin-top: 4px; }
  button.primary { background: #4a78c8; color: white; border: 1px solid #4a78c8; border-radius: 4px; cursor: pointer; }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
