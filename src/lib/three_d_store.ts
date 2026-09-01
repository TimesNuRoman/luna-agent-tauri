// src/lib/three_d_store.ts
// Single source of truth for the 3D editor's scene graph.
//
// The scene graph is purely a frontend concept — it lives in this store and
// is mirrored into the Three.js scene by ThreeDViewport.svelte. The backend
// (services/three_d.rs) only validates ops and persists the JSON; it never
// owns scene state.
//
// Two flavors of mutations go through the same `pushOp` channel:
//   1. User-driven: Inspector sliders, Outliner add/remove, Toolbar buttons.
//   2. AI-driven: ThreeDChat.svelte parses `ai_tool_use` events and calls
//      `applyOps(ops)` after the backend returns a `three_d_ops` event.
// Both code paths look identical to the rest of the UI.

import { derived, get, writable, type Readable, type Writable } from 'svelte/store';

export type NodeId = string;
export type Vec3 = [number, number, number];

export type PrimitiveKind =
  | 'box' | 'sphere' | 'plane' | 'cylinder' | 'torus' | 'cone' | 'capsule';

export interface Transform {
  position: Vec3;
  rotation: Vec3;
  scale: Vec3;
}

export interface MaterialState {
  color: string; // "#rrggbb"
  metalness: number; // 0..1
  roughness: number; // 0..1
  textureDataUrl?: string;
  texturePrompt?: string;
}

export interface MeshNode {
  kind: 'mesh';
  id: NodeId;
  name: string;
  parent: NodeId | null;
  primitive: PrimitiveKind;
  transform: Transform;
  material: MaterialState;
  visible: boolean;
}

export interface GroupNode {
  kind: 'group';
  id: NodeId;
  name: string;
  parent: NodeId | null;
  children: SceneNode[];
  visible: boolean;
}

export type SceneNode = MeshNode | GroupNode;

export interface CameraState {
  position: Vec3;
  target: Vec3;
}

export interface LoadingState {
  kind: 'image' | 'ai' | 'export' | 'save' | 'load' | null;
  label: string;
}

interface History {
  past: SceneSnapshot[];
  future: SceneSnapshot[];
}

export interface SceneSnapshot {
  ts: number;
  scene: SceneNode[];
  camera: CameraState;
  selectedId: NodeId | null;
}

const HISTORY_CAP = 64;
const initialCamera: CameraState = {
  position: [3, 2, 5],
  target: [0, 0, 0],
};

// -------- SceneOp (mirrors Rust services::three_d::SceneOp) --------

export type SceneOp =
  | { kind: 'add_primitive'; id: NodeId; parent: NodeId | null;
      primitive: PrimitiveKind; transform: Transform; material: MaterialState; name?: string }
  | { kind: 'add_group'; id: NodeId; parent: NodeId | null; name: string; visible?: boolean }
  | { kind: 'remove_node'; id: NodeId }
  | { kind: 'update_node'; id: NodeId; patch: NodePatch }
  | { kind: 'apply_texture'; id: NodeId; prompt: string; dataUrl: string }
  | { kind: 'set_camera'; position: Vec3; target: Vec3 }
  | { kind: 'clear_scene' };

export type NodePatch =
  | { field: 'name'; value: string }
  | { field: 'transform'; value: Transform }
  | { field: 'material'; value: MaterialState }
  | { field: 'visible'; value: boolean }
  | { field: 'parent'; value: NodeId | null };

// -------- Default factories --------

export function defaultTransform(): Transform {
  return { position: [0, 0, 0], rotation: [0, 0, 0], scale: [1, 1, 1] };
}

export function defaultMaterial(): MaterialState {
  return { color: '#cccccc', metalness: 0.0, roughness: 0.7 };
}

export function makeMesh(args: {
  id: NodeId; parent: NodeId | null; primitive: PrimitiveKind;
  transform?: Transform; material?: MaterialState; name?: string;
}): MeshNode {
  return {
    kind: 'mesh',
    id: args.id,
    parent: args.parent,
    primitive: args.primitive,
    transform: args.transform ?? defaultTransform(),
    material: args.material ?? defaultMaterial(),
    name: args.name ?? humanName(args.primitive, args.id),
    visible: true,
  };
}

// -------- Store --------

export interface SceneStoreState {
  scene: SceneNode[]; // root-level children (flat list, parents via `.parent`)
  selectedId: NodeId | null;
  camera: CameraState;
  loading: LoadingState;
}

export interface SceneStore extends Readable<SceneStoreState> {
  pushOp: (op: SceneOp, opts?: { silent?: boolean }) => void;
  applyOps: (ops: SceneOp[]) => { ok: boolean; error?: string };
  undo: () => void;
  redo: () => void;
  select: (id: NodeId | null) => void;
  setLoading: (loading: LoadingState) => void;
  setCamera: (position: Vec3, target: Vec3) => void;
  serialize: () => object;
  loadFrom: (json: any) => { ok: boolean; error?: string };
  reset: () => void;
}

function makeId(prefix: string): NodeId {
  // Crypto.randomUUID is available in modern WebView; fall back if not.
  const c: any = (globalThis as any).crypto;
  if (c?.randomUUID) return `${prefix}_${c.randomUUID().slice(0, 8)}`;
  return `${prefix}_${Math.random().toString(36).slice(2, 10)}`;
}

function humanName(primitive: PrimitiveKind, id: NodeId): string {
  const cap = primitive.charAt(0).toUpperCase() + primitive.slice(1);
  // Strip the prefix for display only.
  const tail = id.includes('_') ? id.split('_').slice(1).join('_') : id;
  return `${cap} (${tail})`;
}

function deepCloneScene(scene: SceneNode[]): SceneNode[] {
  return JSON.parse(JSON.stringify(scene)) as SceneNode[];
}

function findNode(scene: SceneNode[], id: NodeId): SceneNode | null {
  for (const n of scene) {
    if (n.id === id) return n;
    if (n.kind === 'group') {
      const sub = findNode(n.children, id);
      if (sub) return sub;
    }
  }
  return null;
}

function findParentRef(scene: SceneNode[], id: NodeId): { parent: SceneNode | null; parentIsRoot: boolean; index: number } | null {
  for (let i = 0; i < scene.length; i++) {
    if (scene[i].id === id) return { parent: null, parentIsRoot: true, index: i };
  }
  for (const n of scene) {
    if (n.kind === 'group') {
      const sub = findParentRef(n.children, id);
      if (sub) return { parent: n, parentIsRoot: false, index: sub.index };
    }
  }
  return null;
}

function wouldCycle(scene: SceneNode[], childId: NodeId, newParentId: NodeId | null): boolean {
  if (newParentId === null) return false;
  if (newParentId === childId) return true;
  let cur: NodeId | null = newParentId;
  const seen = new Set<NodeId>();
  while (cur) {
    if (cur === childId) return true;
    if (seen.has(cur)) return true; // already cyclic
    seen.add(cur);
    const p = findNode(scene, cur);
    if (!p) return false;
    cur = p.parent;
  }
  return false;
}

function applyOpInternal(scene: SceneNode[], op: SceneOp): { ok: true } | { ok: false; error: string } {
  switch (op.kind) {
    case 'add_primitive': {
      if (findNode(scene, op.id)) return { ok: false, error: `id already exists: ${op.id}` };
      if (op.parent && !findNode(scene, op.parent)) return { ok: false, error: `parent missing: ${op.parent}` };
      const mesh: MeshNode = {
        kind: 'mesh', id: op.id, parent: op.parent,
        primitive: op.primitive, transform: op.transform,
        material: op.material, name: op.name ?? humanName(op.primitive, op.id),
        visible: true,
      };
      attach(scene, mesh);
      return { ok: true };
    }
    case 'add_group': {
      if (findNode(scene, op.id)) return { ok: false, error: `id already exists: ${op.id}` };
      if (op.parent && !findNode(scene, op.parent)) return { ok: false, error: `parent missing: ${op.parent}` };
      const g: GroupNode = { kind: 'group', id: op.id, parent: op.parent, name: op.name, children: [], visible: op.visible ?? true };
      attach(scene, g);
      return { ok: true };
    }
    case 'remove_node': {
      const ref = findParentRef(scene, op.id);
      if (!ref) return { ok: false, error: `id missing: ${op.id}` };
      detach(scene, ref);
      return { ok: true };
    }
    case 'update_node': {
      const n = findNode(scene, op.id);
      if (!n) return { ok: false, error: `id missing: ${op.id}` };
      switch (op.patch.field) {
        case 'name': n.name = String(op.patch.value); break;
        case 'transform': n.transform = { ...op.patch.value }; break;
        case 'material': n.material = { ...op.patch.value }; break;
        case 'visible': n.visible = Boolean(op.patch.value); break;
        case 'parent': {
          const newParent = op.patch.value;
          if (newParent !== null && !findNode(scene, newParent)) {
            return { ok: false, error: `parent missing: ${newParent}` };
          }
          if (wouldCycle(scene, op.id, newParent)) {
            return { ok: false, error: 'cycle detected' };
          }
          const ref = findParentRef(scene, op.id);
          if (!ref) return { ok: false, error: `id missing: ${op.id}` };
          detach(scene, ref);
          n.parent = newParent;
          attach(scene, n);
          break;
        }
      }
      return { ok: true };
    }
    case 'apply_texture': {
      const n = findNode(scene, op.id);
      if (!n) return { ok: false, error: `id missing: ${op.id}` };
      if (n.kind !== 'mesh') return { ok: false, error: `id is not a mesh: ${op.id}` };
      n.material = { ...n.material, textureDataUrl: op.dataUrl, texturePrompt: op.prompt };
      return { ok: true };
    }
    case 'set_camera': {
      // camera lives in store.camera, not in scene; handled by caller.
      return { ok: true };
    }
    case 'clear_scene': {
      scene.length = 0;
      return { ok: true };
    }
  }
}

function attach(scene: SceneNode[], node: SceneNode): void {
  if (node.parent === null) { scene.push(node); return; }
  const p = findNode(scene, node.parent);
  if (!p) { scene.push(node); node.parent = null; return; } // parent missing → root
  if (p.kind === 'group') p.children.push(node);
  else scene.push(node); // parent is mesh, can't nest; place at root
}

function detach(scene: SceneNode[], ref: { parent: SceneNode | null; parentIsRoot: boolean; index: number }): void {
  if (ref.parentIsRoot) scene.splice(ref.index, 1);
  else if (ref.parent?.kind === 'group') ref.parent.children.splice(ref.index, 1);
}

function snapshot(state: SceneStoreState, selectedId: NodeId | null): SceneSnapshot {
  return { ts: Date.now(), scene: deepCloneScene(state.scene), camera: { ...state.camera }, selectedId };
}

function createStore(): SceneStore {
  const state: Writable<SceneStoreState> = writable({
    scene: [],
    selectedId: null,
    camera: { ...initialCamera },
    loading: { kind: null, label: '' },
  });
  const history: Writable<History> = writable({ past: [], future: [] });

  function pushHistory(prev: SceneStoreState, next: SceneStoreState, sel: NodeId | null) {
    history.update((h) => {
      const past = [...h.past, snapshot(prev, sel)].slice(-HISTORY_CAP);
      return { past, future: [] };
    });
    // note: `next` snapshot is implicitly the current one (not stored).
  }

  function pushOp(op: SceneOp, opts?: { silent?: boolean }) {
    let prev: SceneStoreState | null = null;
    state.update((s) => {
      prev = JSON.parse(JSON.stringify(s));
      const next = { ...s, scene: deepCloneScene(s.scene) };
      const r = applyOpInternal(next.scene, op);
      if (!r.ok) {
        if (!opts?.silent) console.error('[three_d] op failed:', r.error, op);
        return s; // no change
      }
      pushHistory(s, next, s.selectedId);
      return next;
    });
    if (op.kind === 'set_camera') {
      state.update((s) => ({ ...s, camera: { position: op.position, target: op.target } }));
    }
  }

  function applyOps(ops: SceneOp[]) {
    let prev: SceneStoreState | null = null;
    let lastError: string | undefined;
    state.update((s) => {
      prev = JSON.parse(JSON.stringify(s));
      const next = { ...s, scene: deepCloneScene(s.scene) };
      for (const op of ops) {
        // set_camera handled separately (mutates next.camera)
        if (op.kind === 'set_camera') {
          next.camera = { position: op.position, target: op.target };
          continue;
        }
        const r = applyOpInternal(next.scene, op);
        if (!r.ok) { lastError = r.error; break; }
      }
      if (!lastError) pushHistory(s, next, s.selectedId);
      return lastError ? s : next;
    });
    return lastError ? { ok: false, error: lastError } : { ok: true };
  }

  function undo() {
    let didUndo = false;
    let lastFuture: SceneSnapshot | null = null;
    history.update((h) => {
      if (h.past.length === 0) return h;
      const newPast = h.past.slice(0, -1);
      const snap = h.past[h.past.length - 1];
      lastFuture = snapshot(get(state), get(state).selectedId);
      didUndo = true;
      state.update((s) => ({
        ...s,
        scene: deepCloneScene(snap.scene),
        camera: { ...snap.camera },
        selectedId: snap.selectedId,
      }));
      return { past: newPast, future: [lastFuture!, ...h.future].slice(0, HISTORY_CAP) };
    });
    return didUndo;
  }

  function redo() {
    let didRedo = false;
    history.update((h) => {
      if (h.future.length === 0) return h;
      const newFuture = h.future.slice(1);
      const snap = h.future[0];
      const last = snapshot(get(state), get(state).selectedId);
      didRedo = true;
      state.update((s) => ({
        ...s,
        scene: deepCloneScene(snap.scene),
        camera: { ...snap.camera },
        selectedId: snap.selectedId,
      }));
      return { past: [...h.past, last].slice(-HISTORY_CAP), future: newFuture };
    });
    return didRedo;
  }

  function select(id: NodeId | null) { state.update((s) => ({ ...s, selectedId: id })); }
  function setLoading(loading: LoadingState) { state.update((s) => ({ ...s, loading })); }
  function setCamera(position: Vec3, target: Vec3) {
    state.update((s) => ({ ...s, camera: { position, target } }));
  }
  function serialize() {
    const s = get(state);
    return {
      format: 'luna3d',
      version: 1,
      scene: s.scene,
      camera: s.camera,
      savedAt: new Date().toISOString(),
      minimaxModelUsed: 'MiniMax-M3',
    };
  }
  function loadFrom(json: any) {
    if (!json || typeof json !== 'object') return { ok: false, error: 'empty payload' };
    if (json.format !== 'luna3d') return { ok: false, error: `unknown format: ${json.format}` };
    if (typeof json.version !== 'number') return { ok: false, error: 'missing version' };
    if (json.version > 1) return { ok: false, error: `unsupported version: ${json.version}` };
    if (!Array.isArray(json.scene)) return { ok: false, error: 'missing scene[]' };
    state.set({
      scene: json.scene as SceneNode[],
      selectedId: null,
      camera: (json.camera as CameraState) ?? { ...initialCamera },
      loading: { kind: null, label: '' },
    });
    history.set({ past: [], future: [] });
    return { ok: true };
  }
  function reset() {
    state.set({ scene: [], selectedId: null, camera: { ...initialCamera }, loading: { kind: null, label: '' } });
    history.set({ past: [], future: [] });
  }

  return {
    subscribe: state.subscribe,
    pushOp, applyOps, undo, redo, select, setLoading, setCamera, serialize, loadFrom, reset,
  };
}

let _store: SceneStore | null = null;
export function getSceneStore(): SceneStore {
  if (!_store) _store = createStore();
  return _store;
}

export function newId(prefix: string): NodeId { return makeId(prefix); }

// Helper for derived stores that need to flatten the scene tree.
export function flattenScene(scene: SceneNode[]): SceneNode[] {
  const out: SceneNode[] = [];
  for (const n of scene) {
    out.push(n);
    if (n.kind === 'group') out.push(...flattenScene(n.children));
  }
  return out;
}

export const selectedNode: Readable<SceneNode | null> = derived(
  { subscribe: getSceneStore().subscribe },
  ($s: SceneStoreState) => $s.selectedId ? findNode($s.scene, $s.selectedId) : null,
);
