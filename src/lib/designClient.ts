//! designClient.ts — typed wrappers for the Mephistopheles Tauri
//! commands. Mirrors the Rust types in `services::design`.
//!
//! Use these from the DesignStudio.svelte component and from
//! anywhere else that needs to talk to the design agent. Every
//! function returns a Promise; failures throw an Error with the
//! backend's error message.

import { invoke } from '@tauri-apps/api/core';

// ---- Aspect ratios (mirrors ImageAspect in services::design::image_gen) ----

export type ImageAspect =
  | '1:1'
  | '16:9'
  | '9:16'
  | '4:3'
  | '3:4'
  | '21:9';

// ---- Design system types ----

export interface DesignSystem {
  version: number;
  name: string;
  base_font: string;
  type_scale: number;
  radius_scale: number;
  spacing_unit: number;
}

export interface DesignBrief {
  style_prefix: string;
  mood: string;
  anti_patterns: string[];
  color_temperature: string;
  aspect_default: ImageAspect;
}

export interface Palette {
  primary: string;
  secondary: string;
  accent: string;
  neutral_bg: string;
  neutral_fg: string;
  semantic_ok: string;
  semantic_warn: string;
  semantic_err: string;
  version: number;
}

export interface VoiceGuide {
  name: string;
  description: string;
  tone_keywords: string[];
  example_phrases: string[];
  banned_words: string[];
  allow_profanity: boolean;
  formality: number; // 0..=10
  version: number;
}

// ---- Records ----

export interface ImageRecord {
  id: string;
  prompt: string;
  brief_snapshot: DesignBrief;
  palette_snapshot: Palette;
  aspect: ImageAspect;
  file: string; // PathBuf → string
  created_at: string; // ISO 8601
  model: string;
}

export type CopyContext =
  | 'hero'
  | 'cta'
  | 'section_header'
  | 'body'
  | 'error'
  | 'empty_state'
  | 'tooltip'
  | 'form_label'
  | 'form_placeholder'
  | 'form_error'
  | 'tagline'
  | 'meta_description'
  | 'microcopy'
  | 'nav_item'
  | 'modal_title'
  | 'toast';

export interface CopyVariant {
  text: string;
  char_count: number;
  tone_score: number;
  notes?: string;
}

export interface CopyAsset {
  id: string;
  context: CopyContext;
  language: 'ru' | 'en';
  variants: CopyVariant[];
  primary_idx: number;
  rationale: string;
  voice_snapshot: VoiceGuide;
  created_at: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
}

export type ScaffoldKind = 'component' | 'page' | 'app';

export interface ScaffoldFile {
  path: string;
  content: string;
}

export interface ScaffoldRecord {
  id: string;
  kind: ScaffoldKind;
  name: string;
  files: ScaffoldFile[];
  summary: string;
  palette_snapshot: Palette;
  brief_style_prefix: string;
  created_at: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
}

// ---- Top-level state ----

export interface DesignState {
  manifest: DesignSystem;
  brief: DesignBrief;
  palette: Palette;
  voice: VoiceGuide;
  images: ImageRecord[];
  copy: CopyAsset[];
  workspace_root: string;
}

// ---- Tauri command wrappers ----

/** Spawn a Mephistopheles design task. Returns the new task id. */
export async function mephistoChat(
  prompt: string,
  parentChatId?: string
): Promise<string> {
  return await invoke<string>('mephisto_chat', {
    prompt,
    parent_chatId: parentChatId ?? null,
  });
}

/** Read the current design state (manifest + brief + palette + voice + recent items). */
export async function mephistoGetState(): Promise<DesignState> {
  return await invoke<DesignState>('mephisto_get_state');
}

/** Apply a design artifact. Currently supports `kind: "tokens"`. */
export async function mephistoApplyDesign(args: {
  kind: 'tokens' | 'copy' | 'scaffold';
  target_path: string;
  format?: string;
}): Promise<{ ok: boolean; path?: string; bytes?: number; files_changed?: string[] }> {
  return await invoke('mephisto_apply_design', {
    kind: args.kind,
    targetPath: args.target_path,
    format: args.format ?? null,
  });
}

/** Bulk-export the design system as a JSON bundle at
 *  `<workspace>/.luna/design/dist/luna-design-bundle.json`. */
export async function mephistoExport(): Promise<{ ok: boolean; path: string; bytes: number }> {
  return await invoke('mephisto_export');
}

/** Apply a saved scaffold to the user's `src/` (allow-listed target). */
export async function mephistoSaveScaffold(
  scaffoldId: string,
  targetSubdir: string
): Promise<{ ok: boolean; copied: number; files: string[] }> {
  return await invoke('mephisto_save_scaffold', {
    scaffoldId,
    targetSubdir,
  });
}

// ---- Slash command parser ----

/**
 * Parse a `/design ...` slash command. Returns the kind + payload
 * that the chat UI uses to dispatch.
 *
 *   /design component Button primary brass accent
 *   /design page Home "main landing"
 *   /design app LunaViz
 *   /design copy hero "main landing"
 *   /design image "dark throne room"
 *   /design (no kind) — auto-detect from context
 */
export interface ParsedDesignCommand {
  kind: 'component' | 'page' | 'app' | 'copy' | 'image' | 'auto';
  /** Original text after the kind. */
  args: string;
  /** For copy: parsed context. */
  copyContext?: CopyContext;
}

const COPY_CONTEXTS: CopyContext[] = [
  'hero', 'cta', 'section_header', 'body', 'error', 'empty_state',
  'tooltip', 'form_label', 'form_placeholder', 'form_error',
  'tagline', 'meta_description', 'microcopy', 'nav_item',
  'modal_title', 'toast',
];

export function parseDesignSlashCommand(input: string): ParsedDesignCommand | null {
  const m = input.match(/^\/design\s+(.*)$/i);
  if (!m) return null;
  const rest = m[1].trim();
  const parts = rest.split(/\s+/);
  if (parts.length === 0) return null;

  const head = parts[0].toLowerCase();
  const tail = parts.slice(1).join(' ');

  if (head === 'component' || head === 'page' || head === 'app' || head === 'image') {
    return { kind: head, args: tail };
  }
  if (head === 'copy') {
    // /design copy hero "main landing" → kind=copy, context=hero, args=main landing
    const ctxHead = parts[1]?.toLowerCase();
    if (ctxHead && (COPY_CONTEXTS as string[]).includes(ctxHead)) {
      return {
        kind: 'copy',
        args: parts.slice(2).join(' '),
        copyContext: ctxHead as CopyContext,
      };
    }
    return { kind: 'copy', args: tail };
  }
  return { kind: 'auto', args: rest };
}
