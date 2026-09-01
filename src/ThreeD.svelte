<script lang="ts">
  // src/ThreeD.svelte
  // Root of the 3D editor tab. Hosts the toolbar, three-pane layout
  // (outliner | viewport | inspector) and the AI chat at the bottom.
  //
  // The 3D viewport hands us an api via onReady (see ThreeDViewport.svelte)
  // — we never rely on `bind:this` + instance-method export, which is
  // unreliable across Svelte 4 + TS builds.

  import { onMount, onDestroy, tick } from 'svelte';
  import ThreeDToolbar from './ThreeDToolbar.svelte';
  import ThreeDOutliner from './ThreeDOutliner.svelte';
  import ThreeDInspector from './ThreeDInspector.svelte';
  import ThreeDViewport, { type ViewportApi } from './ThreeDViewport.svelte';
  import ThreeDChat from './ThreeDChat.svelte';
  import { getSceneStore, type SceneOp, type Transform } from './lib/three_d_store';
  import { saveLuna3dJson } from './lib/three_d_io';
  import { newId } from './lib/three_d_store';

  const store = getSceneStore();
  let viewportApi: ViewportApi | null = null;
  let chatOpen = false;
  let toastTimer: ReturnType<typeof setTimeout> | null = null;
  let toast: { text: string; kind: 'info' | 'error' } | null = null;

  function showToast(text: string, kind: 'info' | 'error' = 'info') {
    toast = { text, kind };
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toast = null; }, 3500);
  }

  function handleViewportReady(api: ViewportApi) { viewportApi = api; }
  function handleViewportError(msg: string) { showToast(msg, 'error'); }

  // -------- Demo scene: shown on first launch (no autosave + empty scene) --------
  const DEMO_KEY = 'luna.three_d.demo_seeded';
  const AUTOSAVE_KEY = 'luna.three_d.last_scene';
  function seedDemoScene() {
    if ($store.scene.length > 0) return;
    if (localStorage.getItem(DEMO_KEY)) return;
    if (localStorage.getItem(AUTOSAVE_KEY)) return; // user had a real scene, don't overwrite
    const ops: SceneOp[] = [
      // Ground plane (rotated -90° on X to lie flat).
      { kind: 'add_primitive', id: 'ground', parent: null, primitive: 'plane',
        transform: { position: [0, 0, 0], rotation: [-Math.PI / 2, 0, 0], scale: [10, 10, 1] },
        material: { color: '#3a3f4a', metalness: 0, roughness: 1 }, name: 'Ground' },
      // Red box.
      { kind: 'add_primitive', id: 'box1', parent: null, primitive: 'box',
        transform: { position: [0, 0.5, 0], rotation: [0, 0.2, 0], scale: [1, 1, 1] },
        material: { color: '#e05555', metalness: 0.1, roughness: 0.5 }, name: 'Red Box' },
      // Blue sphere.
      { kind: 'add_primitive', id: 'sphere1', parent: null, primitive: 'sphere',
        transform: { position: [2, 0.7, -1], rotation: [0, 0, 0], scale: [1, 1, 1] },
        material: { color: '#5588e0', metalness: 0.4, roughness: 0.3 }, name: 'Blue Sphere' },
      // A torus behind the box.
      { kind: 'add_primitive', id: 'torus1', parent: null, primitive: 'torus',
        transform: { position: [-1.8, 0.6, 0.5], rotation: [Math.PI / 2, 0, 0], scale: [1, 1, 1] },
        material: { color: '#7ec07a', metalness: 0.2, roughness: 0.4 }, name: 'Green Torus' },
    ];
    const r = store.applyOps(ops);
    if (r.ok) {
      try { localStorage.setItem(DEMO_KEY, '1'); } catch {}
    }
  }

  function loadDemoScene() {
    // Always reloads the demo, overwriting whatever's there (with confirm).
    if (!confirm('Replace the current scene with the demo?')) return;
    store.reset();
    try { localStorage.removeItem(DEMO_KEY); } catch {}
    seedDemoScene();
  }

  // -------- Keyboard: Cmd/Ctrl+Z = undo, Shift+Z = redo, W/E/R = gizmo mode --------
  // 1-5 = camera presets, F = fit, T = turntable
  function onKey(e: KeyboardEvent) {
    const meta = e.ctrlKey || e.metaKey;
    const tag = (e.target as HTMLElement)?.tagName;
    const inField = tag === 'INPUT' || tag === 'TEXTAREA';

    if (meta && (e.key === 'z' || e.key === 'Z')) {
      if (e.shiftKey) store.redo(); else store.undo();
      e.preventDefault();
      return;
    }
    if (meta && (e.key === 'y' || e.key === 'Y')) {
      store.redo();
      e.preventDefault();
      return;
    }
    // Gizmo mode + view shortcuts — only when canvas/UI is focused
    // (skip when typing in a text field).
    if (!inField && viewportApi) {
      switch (e.key) {
        case 'w': case 'W': viewportApi.setMode('translate'); e.preventDefault(); return;
        case 'e': case 'E': viewportApi.setMode('rotate');    e.preventDefault(); return;
        case 'r': case 'R': viewportApi.setMode('scale');     e.preventDefault(); return;
        case '1': viewportApi.setCameraPreset('perspective'); e.preventDefault(); return;
        case '2': viewportApi.setCameraPreset('top');        e.preventDefault(); return;
        case '3': viewportApi.setCameraPreset('front');      e.preventDefault(); return;
        case '4': viewportApi.setCameraPreset('side');       e.preventDefault(); return;
        case '5': viewportApi.setCameraPreset('iso');        e.preventDefault(); return;
        case 'f': case 'F': viewportApi.fitToScene();        e.preventDefault(); return;
        case 't': case 'T': viewportApi.setTurntable(true);   e.preventDefault(); return;
        case 'Escape':      viewportApi.setTurntable(false);  e.preventDefault(); return;
      }
    }
  }

  // -------- Auto-save --------
  const AUTOSAVE_INTERVAL = 30000;
  let autosaveTimer: ReturnType<typeof setInterval> | null = null;
  function startAutosave() {
    autosaveTimer = setInterval(() => {
      try {
        const s = $store;
        if (s.scene.length === 0) return;
        localStorage.setItem(AUTOSAVE_KEY, JSON.stringify(store.serialize()));
      } catch {}
    }, AUTOSAVE_INTERVAL);
  }
  function tryRestoreAutosave() {
    try {
      const raw = localStorage.getItem(AUTOSAVE_KEY);
      if (!raw) return;
      const json = JSON.parse(raw);
      if (!json?.scene?.length) return;
      if (!confirm('Restore previous 3D scene?')) return;
      const r = store.loadFrom(json);
      if (!r.ok) console.warn('[three_d] restore failed:', r.error);
    } catch {}
  }

  async function doSave() {
    try {
      const path = await saveLuna3dJson(store.serialize(), 'scene.luna3d.json');
      if (path) showToast('Scene saved.', 'info');
    } catch (e: any) {
      showToast(`Save failed: ${e?.message ?? e}`, 'error');
    }
  }
  function doNew() {
    if ($store.scene.length > 0 && !confirm('Discard current scene and start fresh?')) return;
    store.reset();
  }

  function onBeforeUnload() {
    if ($store.scene.length > 0) {
      try { localStorage.setItem(AUTOSAVE_KEY, JSON.stringify(store.serialize())); } catch {}
    }
  }

  onMount(() => {
    // Restore first (so a real scene takes priority), then seed demo if still empty.
    tryRestoreAutosave();
    seedDemoScene();
    startAutosave();
    window.addEventListener('keydown', onKey);
    window.addEventListener('beforeunload', onBeforeUnload);
  });
  onDestroy(() => {
    if (autosaveTimer) clearInterval(autosaveTimer);
    if (toastTimer) clearTimeout(toastTimer);
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('beforeunload', onBeforeUnload);
  });
</script>

<div class="three-d-root">
  <ThreeDToolbar
    onNew={doNew}
    onSave={doSave}
    onImport={() => viewportApi?.importSceneFile()}
    onExport={() => viewportApi?.exportScene()}
    onLoadDemo={loadDemoScene}
    onToggleChat={() => (chatOpen = !chatOpen)}
    {chatOpen}
  />

  <div class="layout" class:chat-open={chatOpen}>
    <aside class="outliner-pane">
      <ThreeDOutliner />
    </aside>
    <main class="viewport-pane">
      <ThreeDViewport onReady={handleViewportReady} onError={handleViewportError} />
    </main>
    <aside class="inspector-pane">
      <ThreeDInspector onToast={showToast} />
    </aside>
  </div>

  {#if chatOpen}
    <div class="chat-pane">
      <ThreeDChat onToast={showToast} />
    </div>
  {/if}

  {#if toast}
    <div class="toast" class:error={toast.kind === 'error'}>{toast.text}</div>
  {/if}
</div>

<style>
  .three-d-root {
    display: flex; flex-direction: column;
    width: 100%; height: 100%; min-height: 0;
    background: var(--bg);
    color: var(--text);
    font-family: inherit;
    position: relative;
  }
  .layout {
    flex: 1; min-height: 0;
    display: grid;
    grid-template-columns: 220px 1fr 280px;
    grid-template-rows: 1fr;
  }
  .layout.chat-open { grid-template-rows: 1fr 220px; }
  .outliner-pane { display: flex; min-height: 0; overflow: hidden; }
  .viewport-pane { display: flex; min-height: 0; overflow: hidden; }
  .inspector-pane { display: flex; min-height: 0; overflow: hidden; }
  .chat-pane { display: flex; min-height: 0; overflow: hidden; }

  .toast {
    position: absolute; bottom: 18px; left: 50%; transform: translateX(-50%);
    background: #1c1f26; color: #e6e8eb; border: 1px solid #2c313a;
    border-radius: 6px; padding: 8px 14px; font-size: 12px;
    box-shadow: 0 6px 20px rgba(0,0,0,0.4);
    pointer-events: none;
    z-index: 10;
  }
  .toast.error { color: #f09090; border-color: #8a3a3a; }
</style>
