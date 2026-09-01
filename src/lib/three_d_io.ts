// src/lib/three_d_io.ts
// Thin helpers around Tauri dialog / fs plugins for the 3D editor.
// Lives here so ThreeDViewport.svelte doesn't have to know about IPC details.

import { save as saveDialog, open as openDialog } from '@tauri-apps/plugin-dialog';
import { readTextFile, writeFile, writeTextFile } from '@tauri-apps/plugin-fs';

export async function exportGltfToFile(blob: Blob, suggestedName: string): Promise<string | null> {
  const path = await saveDialog({
    title: 'Export scene',
    defaultPath: suggestedName,
    filters: [{ name: 'glTF Binary', extensions: ['glb'] }],
  });
  if (!path) return null;
  const buf = new Uint8Array(await blob.arrayBuffer());
  await writeFile(path, buf);
  return path;
}

export async function exportObjToFile(text: string, suggestedName: string): Promise<string | null> {
  const path = await saveDialog({
    title: 'Export scene',
    defaultPath: suggestedName,
    filters: [{ name: 'Wavefront OBJ', extensions: ['obj'] }],
  });
  if (!path) return null;
  await writeTextFile(path, text);
  return path;
}

export async function importGltfFromFile(): Promise<unknown | null> {
  const path = await openDialog({
    title: 'Open scene',
    multiple: false,
    filters: [
      { name: 'Luna 3D Scene', extensions: ['luna3d.json', 'json'] },
      { name: 'glTF Binary', extensions: ['glb'] },
      { name: 'glTF Text', extensions: ['gltf'] },
    ],
  });
  if (!path) return null;
  // For now: only our native format. GLB/GLTF binary parsing happens in a
  // later phase; we keep the API surface stable.
  if (path.endsWith('.glb') || path.endsWith('.gltf')) {
    throw new Error('GLB/GLTF import is on the roadmap; please use .luna3d.json files for now.');
  }
  const text = await readTextFile(path);
  return JSON.parse(text);
}

export async function saveLuna3dJson(json: object, suggestedName: string): Promise<string | null> {
  const path = await saveDialog({
    title: 'Save scene',
    defaultPath: suggestedName,
    filters: [{ name: 'Luna 3D Scene', extensions: ['json'] }],
  });
  if (!path) return null;
  await writeTextFile(path, JSON.stringify(json, null, 2));
  return path;
}
