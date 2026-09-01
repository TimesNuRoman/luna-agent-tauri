<script lang="ts">
  // src/ThreeDViewport.svelte
  // Three.js viewport with everything needed to *evaluate* the agent's
  // models — not just edit them. Studio lighting, PBR environment, camera
  // presets, fit-to-scene, turntable, and an on-canvas stats overlay.

  import { onDestroy, onMount } from 'svelte';
  import * as THREE from 'three';
  import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
  import { TransformControls } from 'three/examples/jsm/controls/TransformControls.js';
  import { GLTFExporter } from 'three/examples/jsm/exporters/GLTFExporter.js';
  import { RoomEnvironment } from 'three/examples/jsm/environments/RoomEnvironment.js';
  import {
    getSceneStore,
    type SceneNode, type MeshNode, type Transform, type CameraState, flattenScene,
  } from './lib/three_d_store';
  import { exportGltfToFile, importGltfFromFile } from './lib/three_d_io';

  /** Shape handed to the parent (ThreeD.svelte) via the onReady callback. */
  export type ViewportApi = {
    exportScene: () => Promise<void>;
    importSceneFile: () => Promise<void>;
    setMode: (mode: 'translate' | 'rotate' | 'scale') => void;
    refresh: () => void;
    setCameraPreset: (preset: 'perspective' | 'top' | 'front' | 'side' | 'iso') => void;
    fitToScene: () => void;
    setTurntable: (on: boolean) => void;
    takeScreenshot: () => Promise<Blob | null>;
  };

  export let onReady: ((api: ViewportApi) => void) | undefined = undefined;
  export let onError: ((msg: string) => void) | undefined = undefined;

  const store = getSceneStore();
  let canvasEl: HTMLCanvasElement;
  let containerEl: HTMLDivElement;
  let webglOk: boolean = true;
  let initError: string | null = null;

  // Three.js core
  let renderer: THREE.WebGLRenderer | null = null;
  let scene: THREE.Scene;
  let camera: THREE.PerspectiveCamera;
  let orbit: OrbitControls | null = null;
  let transformGizmo: TransformControls | null = null;
  let gizmoHelper: THREE.Object3D | null = null;
  let grid: THREE.GridHelper;
  let axes: THREE.AxesHelper;

  // Studio lighting — 3-point rig (key + fill + rim) on top of the
  // PBR environment map. PBR materials only look right with an env
  // map, otherwise metalness/roughness read as flat.
  let keyLight: THREE.DirectionalLight;
  let fillLight: THREE.DirectionalLight;
  let rimLight: THREE.DirectionalLight;
  let hemi: THREE.HemisphereLight;
  let envMap: THREE.Texture | null = null;

  // ObjectId → Three.Object3D
  const nodeObjects = new Map<string, THREE.Object3D>();
  const objectToId = new WeakMap<THREE.Object3D, string>();
  const textureCache = new Map<string, THREE.Texture>();
  let resizeObserver: ResizeObserver | null = null;
  let rafId = 0;
  let unsubscribe: () => void = () => {};
  let lastSceneRef: SceneNode[] = [];

  // Turntable / stats state
  let turntable = false;
  let turntableSpeed = 0.15; // rad / s
  let stats = { fps: 0, draws: 0, nodes: 0, tris: 0 };
  let fpsAccum = 0;
  let fpsFrames = 0;
  let fpsLastT = performance.now();

  // ---------------- Scene-graph <-> Three sync ----------------

  function disposeObject(o: THREE.Object3D) {
    o.traverse((c) => {
      const m = c as THREE.Mesh;
      if (m.isMesh) {
        m.geometry?.dispose();
        const mat = m.material as THREE.Material | THREE.Material[];
        if (Array.isArray(mat)) mat.forEach((x) => x.dispose());
        else mat?.dispose();
      }
    });
  }

  function clearScene() {
    for (const [, obj] of nodeObjects) {
      scene?.remove(obj);
      disposeObject(obj);
    }
    nodeObjects.clear();
  }

  function buildGeometry(prim: MeshNode['primitive']): THREE.BufferGeometry {
    switch (prim) {
      case 'box': return new THREE.BoxGeometry(1, 1, 1);
      case 'sphere': return new THREE.SphereGeometry(0.5, 32, 24);
      case 'plane': return new THREE.PlaneGeometry(1, 1);
      case 'cylinder': return new THREE.CylinderGeometry(0.5, 0.5, 1, 32);
      case 'torus': return new THREE.TorusGeometry(0.5, 0.18, 16, 64);
      case 'cone': return new THREE.ConeGeometry(0.5, 1, 32);
      case 'capsule': return new THREE.CapsuleGeometry(0.4, 0.6, 8, 16);
    }
  }

  function applyTransform(obj: THREE.Object3D, t: Transform) {
    obj.position.set(t.position[0], t.position[1], t.position[2]);
    obj.rotation.set(t.rotation[0], t.rotation[1], t.rotation[2]);
    obj.scale.set(t.scale[0], t.scale[1], t.scale[2]);
  }

  function applyMaterial(mesh: THREE.Mesh, mat: MeshNode['material']) {
    const params: THREE.MeshStandardMaterialParameters = {
      color: new THREE.Color(mat.color || '#cccccc'),
      metalness: Math.max(0, Math.min(1, mat.metalness)),
      roughness: Math.max(0, Math.min(1, mat.roughness)),
    };
    if (mat.textureDataUrl) {
      let tex = textureCache.get(mat.textureDataUrl);
      if (!tex) {
        try {
          const loader = new THREE.TextureLoader();
          tex = loader.load(mat.textureDataUrl);
          tex.colorSpace = THREE.SRGBColorSpace;
          tex.wrapS = THREE.RepeatWrapping;
          tex.wrapT = THREE.RepeatWrapping;
          textureCache.set(mat.textureDataUrl, tex);
        } catch (e) {
          console.warn('[3D] failed to load texture:', e);
        }
      }
      if (tex) params.map = tex;
    }
    if (mesh.material instanceof THREE.MeshStandardMaterial) {
      mesh.material.setValues(params);
    } else {
      mesh.material = new THREE.MeshStandardMaterial(params);
    }
  }

  function createObjectFor(node: SceneNode): THREE.Object3D {
    if (node.kind === 'group') {
      const g = new THREE.Group();
      g.name = node.name;
      g.visible = node.visible;
      return g;
    }
    const geom = buildGeometry(node.primitive);
    const m = new THREE.Mesh(geom, new THREE.MeshStandardMaterial({ color: 0xcccccc }));
    m.castShadow = true;
    m.receiveShadow = true;
    m.name = node.name;
    m.visible = node.visible;
    applyTransform(m, node.transform);
    applyMaterial(m, node.material);
    return m;
  }

  function attachToParent(obj: THREE.Object3D, node: SceneNode) {
    const parentId = node.parent;
    if (parentId && nodeObjects.has(parentId)) {
      const pObj = nodeObjects.get(parentId)!;
      if (pObj instanceof THREE.Group) pObj.add(obj);
      else scene.add(obj);
    } else {
      scene.add(obj);
    }
  }

  function rebuildFromStore() {
    if (!scene) return;
    const next = get(store);
    if (next.scene === lastSceneRef) return;
    lastSceneRef = next.scene;
    clearScene();
    function walk(nodes: SceneNode[]) {
      for (const n of nodes) {
        const obj = createObjectFor(n);
        nodeObjects.set(n.id, obj);
        objectToId.set(obj, n.id);
        attachToParent(obj, n);
        if (n.kind === 'group') walk(n.children);
      }
    }
    walk(next.scene);
    bindTransformGizmo();
    stats.nodes = nodeObjects.size;
  }

  function bindTransformGizmo() {
    if (!transformGizmo) return;
    const sel = get(store).selectedId;
    const target = sel ? nodeObjects.get(sel) : null;
    if (target) transformGizmo.attach(target);
    else transformGizmo.detach();
  }

  function onGizmoChange() {
    if (!transformGizmo) return;
    const obj = transformGizmo.object;
    if (!obj) return;
    const id = objectToId.get(obj);
    if (!id) return;
    const t: Transform = {
      position: [obj.position.x, obj.position.y, obj.position.z],
      rotation: [obj.rotation.x, obj.rotation.y, obj.rotation.z],
      scale: [obj.scale.x, obj.scale.y, obj.scale.z],
    };
    store.pushOp({ kind: 'update_node', id, patch: { field: 'transform', value: t } });
  }

  function onPointerDown(ev: PointerEvent) {
    if (!renderer || !camera) return;
    if (transformGizmo?.dragging) return;
    const rect = renderer.domElement.getBoundingClientRect();
    const ndc = new THREE.Vector2(
      ((ev.clientX - rect.left) / rect.width) * 2 - 1,
      -((ev.clientY - rect.top) / rect.height) * 2 + 1,
    );
    const ray = new THREE.Raycaster();
    ray.setFromCamera(ndc, camera);
    const hits = ray.intersectObjects(scene.children, true);
    if (hits.length === 0) {
      store.select(null);
      return;
    }
    let o: THREE.Object3D | null = hits[0].object;
    let id: string | undefined;
    while (o) {
      id = objectToId.get(o);
      if (id) break;
      o = o.parent;
    }
    if (id) store.select(id);
  }

  function applyCameraState(c: CameraState) {
    if (!camera) return;
    camera.position.set(c.position[0], c.position[1], c.position[2]);
    if (orbit) {
      orbit.target.set(c.target[0], c.target[1], c.target[2]);
      orbit.update();
    }
  }

  function onResize() {
    if (!renderer || !containerEl) return;
    const w = containerEl.clientWidth;
    const h = containerEl.clientHeight;
    if (w === 0 || h === 0) return;
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }

  function countTriangles(root: THREE.Object3D): number {
    let tris = 0;
    root.traverse((o) => {
      const m = o as THREE.Mesh;
      if (m.isMesh && m.geometry) {
        const idx = m.geometry.getIndex();
        const pos = m.geometry.getAttribute('position');
        if (pos) tris += idx ? idx.count / 3 : pos.count / 3;
      }
    });
    return Math.round(tris);
  }

  function animate() {
    rafId = requestAnimationFrame(animate);
    if (!renderer || !scene || !camera) return;

    // Turntable: orbit the camera around the target on a horizontal
    // circle, keeping current elevation. Disable user orbit while on.
    if (turntable && orbit) {
      const t = orbit.target;
      const dx = camera.position.x - t.x;
      const dz = camera.position.z - t.z;
      const r = Math.hypot(dx, dz);
      const a = Math.atan2(dz, dx) + turntableSpeed * (1 / 60);
      camera.position.x = t.x + r * Math.cos(a);
      camera.position.z = t.z + r * Math.sin(a);
      camera.lookAt(t);
    }
    orbit?.update();
    renderer.render(scene, camera);

    // FPS / draw-call counter
    fpsFrames++;
    const now = performance.now();
    fpsAccum += now - fpsLastT;
    fpsLastT = now;
    if (fpsAccum >= 500) {
      stats.fps = Math.round((fpsFrames * 1000) / fpsAccum);
      fpsFrames = 0; fpsAccum = 0;
      stats.draws = renderer.info.render.calls;
      stats.tris = countTriangles(scene);
    }
  }

  // ---------------- Public API ----------------

  async function exportScene(): Promise<void> {
    if (!scene) return;
    if (transformGizmo) transformGizmo.detach();
    try {
      const blob = await new Promise<Blob>((resolve, reject) => {
        try {
          new GLTFExporter().parse(
            scene,
            (res) => resolve(res as Blob),
            (err) => reject(err),
            { binary: true, embedImages: true, onlyVisible: true },
          );
        } catch (e) { reject(e); }
      });
      await exportGltfToFile(blob, 'scene.glb');
    } catch (e: any) {
      console.error('[3D] export failed:', e);
      onError?.(`Export failed: ${e?.message ?? e}`);
    } finally {
      bindTransformGizmo();
    }
  }

  async function importSceneFile(): Promise<void> {
    try {
      const json = await importGltfFromFile();
      if (!json) return;
      const r = store.loadFrom(json);
      if (!r.ok) onError?.(`Failed to load: ${r.error}`);
    } catch (e: any) {
      onError?.(`Open failed: ${e?.message ?? e}`);
    }
  }

  function setMode(mode: 'translate' | 'rotate' | 'scale') {
    if (transformGizmo) transformGizmo.setMode(mode);
  }

  function refresh() {
    if (renderer && scene && camera) renderer.render(scene, camera);
  }

  // ---- Camera presets (orthographic-style framing) ----
  function setCameraPreset(preset: 'perspective' | 'top' | 'front' | 'side' | 'iso') {
    if (!orbit || !camera) return;
    const target = orbit.target.clone();
    const r = Math.max(2.5, Math.min(20, currentSceneRadius() * 2.2 + 1));
    switch (preset) {
      case 'perspective':
        camera.position.set(target.x + r * 0.7, target.y + r * 0.5, target.z + r * 0.9);
        break;
      case 'top':
        camera.position.set(target.x, target.y + r, target.z + 0.01);
        break;
      case 'front':
        camera.position.set(target.x, target.y, target.z + r);
        break;
      case 'side':
        camera.position.set(target.x + r, target.y, target.z);
        break;
      case 'iso':
        camera.position.set(target.x + r * 0.7, target.y + r * 0.5, target.z + r * 0.7);
        break;
    }
    camera.lookAt(target);
    orbit.update();
  }

  // Bounding sphere of the current scene (in world space).
  function currentSceneRadius(): number {
    const flat = flattenScene(get(store).scene);
    if (flat.length === 0) return 2;
    const box = new THREE.Box3();
    const tmp = new THREE.Box3();
    for (const n of flat) {
      if (n.kind !== 'mesh') continue;
      const obj = nodeObjects.get(n.id);
      if (!obj) continue;
      tmp.setFromObject(obj);
      box.union(tmp);
    }
    if (box.isEmpty()) return 2;
    const center = box.getCenter(new THREE.Vector3());
    const size = box.getSize(new THREE.Vector3());
    return Math.max(size.x, size.y, size.z) / 2 || 2;
  }

  function fitToScene() {
    if (!orbit || !camera) return;
    const flat = flattenScene(get(store).scene);
    if (flat.length === 0) {
      // Reset to default when empty
      camera.position.set(3, 2, 5);
      orbit.target.set(0, 0, 0);
      camera.lookAt(orbit.target);
      orbit.update();
      return;
    }
    const box = new THREE.Box3();
    const tmp = new THREE.Box3();
    for (const n of flat) {
      if (n.kind !== 'mesh') continue;
      const obj = nodeObjects.get(n.id);
      if (!obj) continue;
      tmp.setFromObject(obj);
      box.union(tmp);
    }
    if (box.isEmpty()) return;
    const center = box.getCenter(new THREE.Vector3());
    const size = box.getSize(new THREE.Vector3());
    const maxDim = Math.max(size.x, size.y, size.z);
    const fov = camera.fov * Math.PI / 180;
    const dist = (maxDim / 2) / Math.tan(fov / 2);
    const dir = new THREE.Vector3(0.7, 0.5, 1).normalize();
    camera.position.set(
      center.x + dir.x * dist * 1.5,
      center.y + dir.y * dist * 1.5,
      center.z + dir.z * dist * 1.5,
    );
    orbit.target.copy(center);
    camera.lookAt(center);
    orbit.update();
  }

  function setTurntable(on: boolean) { turntable = on; }

  async function takeScreenshot(): Promise<Blob | null> {
    if (!renderer || !scene || !camera) return null;
    if (transformGizmo) transformGizmo.detach();
    // Force one render with the gizmo detached, then grab the canvas.
    renderer.render(scene, camera);
    await new Promise((r) => requestAnimationFrame(r));
    const canvas = renderer.domElement as HTMLCanvasElement;
    return new Promise<Blob | null>((resolve) => {
      canvas.toBlob((b) => { bindTransformGizmo(); resolve(b); }, 'image/png');
    });
  }

  // ---------------- Mount / teardown ----------------

  function setupThree() {
    try {
      const probe = document.createElement('canvas').getContext('webgl2');
      if (!probe) { webglOk = false; return false; }
    } catch (e) {
      webglOk = false;
      return false;
    }

    try {
      renderer = new THREE.WebGLRenderer({ canvas: canvasEl, antialias: true, alpha: false, preserveDrawingBuffer: true });
    } catch (e: any) {
      initError = `WebGL renderer init failed: ${e?.message ?? e}`;
      onError?.(initError);
      return false;
    }
    renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
    renderer.shadowMap.enabled = true;
    renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    // Cinematic tone mapping — gives the agent's PBR materials a chance
    // to look like real materials, not flat shaded geometry.
    renderer.toneMapping = THREE.ACESFilmicToneMapping;
    renderer.toneMappingExposure = 1.1;
    renderer.outputColorSpace = THREE.SRGBColorSpace;

    scene = new THREE.Scene();
    scene.background = new THREE.Color(0x1a1d23);
    camera = new THREE.PerspectiveCamera(45, 1, 0.1, 1000);

    // Helpers (always visible)
    grid = new THREE.GridHelper(20, 20, 0x888888, 0x444444);
    scene.add(grid);
    axes = new THREE.AxesHelper(1);
    scene.add(axes);

    // PBR environment — required for metalness/roughness to read
    // correctly. RoomEnvironment is a tiny procedural cubemap that
    // ships with three and renders in <1ms.
    const pmrem = new THREE.PMREMGenerator(renderer);
    envMap = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
    scene.environment = envMap;
    pmrem.dispose();

    // 3-point studio lighting
    hemi = new THREE.HemisphereLight(0xb1e1ff, 0xb97a20, 0.35);
    scene.add(hemi);

    // Key — top-front, strong, casts shadow
    keyLight = new THREE.DirectionalLight(0xffffff, 1.4);
    keyLight.position.set(5, 8, 4);
    keyLight.castShadow = true;
    keyLight.shadow.mapSize.set(1024, 1024);
    keyLight.shadow.camera.left = -8; keyLight.shadow.camera.right = 8;
    keyLight.shadow.camera.top = 8; keyLight.shadow.camera.bottom = -8;
    keyLight.shadow.camera.near = 0.5; keyLight.shadow.camera.far = 30;
    keyLight.shadow.bias = -0.0008;
    scene.add(keyLight);

    // Fill — opposite side, softer, no shadow
    fillLight = new THREE.DirectionalLight(0xc9d7ff, 0.5);
    fillLight.position.set(-4, 5, 2);
    scene.add(fillLight);

    // Rim — back, cool blue, hair-light effect
    rimLight = new THREE.DirectionalLight(0x88aaff, 0.6);
    rimLight.position.set(-2, 4, -6);
    scene.add(rimLight);

    // Legacy alias for any caller still touching `dir`/`hemi`.
    dir = keyLight;
    hemi = hemi;

    orbit = new OrbitControls(camera, renderer.domElement);
    orbit.enableDamping = true;
    orbit.dampingFactor = 0.08;
    orbit.target.set(0, 0, 0);
    // Right-click pans, scroll zooms — matches Blender / Maya feel.
    orbit.mouseButtons = {
      LEFT: THREE.MOUSE.ROTATE,
      MIDDLE: THREE.MOUSE.DOLLY,
      RIGHT: THREE.MOUSE.PAN,
    } as any;

    transformGizmo = new TransformControls(camera, renderer.domElement);
    transformGizmo.setSize(0.8);
    transformGizmo.addEventListener('objectChange', onGizmoChange);
    transformGizmo.addEventListener('dragging-changed', (e: any) => {
      if (orbit) orbit.enabled = !e.value;
    });
    const maybeHelper = (transformGizmo as unknown as { getHelper?: () => THREE.Object3D }).getHelper;
    if (typeof maybeHelper === 'function') {
      gizmoHelper = maybeHelper.call(transformGizmo);
      scene.add(gizmoHelper);
    } else {
      try { scene.add(transformGizmo as unknown as THREE.Object3D); } catch {}
    }

    applyCameraState(get(store).camera);
    unsubscribe = store.subscribe((s) => {
      rebuildFromStore();
      applyCameraState(s.camera);
    });

    renderer.domElement.addEventListener('pointerdown', onPointerDown);
    resizeObserver = new ResizeObserver(onResize);
    resizeObserver.observe(containerEl);
    onResize();
    return true;
  }

  onMount(() => {
    const ok = setupThree();
    if (ok) {
      animate();
      onReady?.({ exportScene, importSceneFile, setMode, refresh,
                  setCameraPreset, fitToScene, setTurntable, takeScreenshot });
    }
  });

  onDestroy(() => {
    cancelAnimationFrame(rafId);
    unsubscribe();
    if (resizeObserver) resizeObserver.disconnect();
    if (orbit) orbit.dispose();
    if (transformGizmo) {
      try {
        transformGizmo.removeEventListener('objectChange', onGizmoChange);
        transformGizmo.dispose();
      } catch {}
    }
    if (gizmoHelper) {
      scene?.remove(gizmoHelper);
      disposeObject(gizmoHelper);
      gizmoHelper = null;
    }
    clearScene();
    for (const t of textureCache.values()) t.dispose();
    textureCache.clear();
    if (envMap) envMap.dispose();
    if (renderer) renderer.dispose();
  });
</script>

<div class="viewport" bind:this={containerEl}>
  {#if !webglOk}
    <div class="webgl-error">
      <div>⚠</div>
      <h3>3D requires WebGL2</h3>
      <p>Enable hardware acceleration in your WebView settings and restart the app.</p>
    </div>
  {:else if initError}
    <div class="webgl-error">
      <div>⚠</div>
      <h3>3D init failed</h3>
      <p>{initError}</p>
    </div>
  {:else}
    <canvas bind:this={canvasEl} class="three-canvas"></canvas>

    <!-- Camera presets bar (top-left) -->
    <div class="hud hud-tl">
      <button on:click={() => setCameraPreset('perspective')} title="Perspective view (1)">◐ Persp</button>
      <button on:click={() => setCameraPreset('top')} title="Top view (2)">▲ Top</button>
      <button on:click={() => setCameraPreset('front')} title="Front view (3)">▶ Front</button>
      <button on:click={() => setCameraPreset('side')} title="Side view (4)">◀ Side</button>
      <button on:click={() => setCameraPreset('iso')} title="Iso (5)">◇ Iso</button>
      <button on:click={fitToScene} title="Fit to scene (F)">⤢ Fit</button>
      <button on:click={() => setTurntable(!turntable)} class:on={turntable} title="Turntable (T)">
        {turntable ? '⏸' : '↻'} Turn
      </button>
    </div>

    <!-- Stats overlay (bottom-left) -->
    <div class="hud hud-bl">
      <span>{stats.fps} fps</span>
      <span>{stats.draws} draws</span>
      <span>{stats.nodes} nodes</span>
      <span>{stats.tris} tris</span>
    </div>
  {/if}
</div>

<style>
  .viewport {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 0;
    background: #1a1d23;
    overflow: hidden;
  }
  .three-canvas {
    display: block;
    width: 100%;
    height: 100%;
    cursor: grab;
  }
  .three-canvas:active { cursor: grabbing; }
  .webgl-error {
    position: absolute; inset: 0;
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    color: #cfd3da; gap: 8px;
    padding: 24px; text-align: center;
  }
  .webgl-error div { font-size: 32px; }
  .webgl-error h3 { margin: 0; }
  .webgl-error p { color: #8a8f99; max-width: 360px; }

  /* HUD overlays (camera presets, stats). They live inside the canvas
     container so they scale with the viewport, never with the page. */
  .hud {
    position: absolute;
    display: flex;
    gap: 4px;
    padding: 6px 8px;
    background: rgba(20, 23, 28, 0.78);
    border: 1px solid #2c313a;
    border-radius: 6px;
    color: #c8d2e0;
    font-size: 11px;
    backdrop-filter: blur(6px);
    pointer-events: auto;
    z-index: 5;
  }
  .hud-tl { top: 10px; left: 10px; flex-wrap: wrap; max-width: calc(100% - 20px); }
  .hud-bl { bottom: 10px; left: 10px; gap: 10px; font-family: ui-monospace, monospace; }
  .hud button {
    background: transparent; color: #c8d2e0;
    border: 1px solid #2c313a; border-radius: 4px;
    padding: 3px 8px; font: inherit; font-size: 11px;
    cursor: pointer;
  }
  .hud button:hover { color: #fff; background: rgba(74, 120, 200, 0.18); border-color: #4a78c8; }
  .hud button.on { color: #6dd18f; border-color: #6dd18f; background: rgba(109, 209, 143, 0.10); }
  .hud span { white-space: nowrap; }
</style>
