// Augmentation Registry (Phase UX-1).
//
// "Augmentations" are chat-side panels for sub-systems that previously
// lived as top-level tabs (Video / Memory / Azazel / 3D Thoughts /
// Self / Daimonion / Design). Each aug registers itself with a small
// declarative descriptor: how to activate it (slash command, tool
// trigger), where to render (sidecard / popover / overlay), and the
// Svelte component that renders its body.
//
// The registry is plain — no Svelte store, no reactivity. Chat reads
// `all()` once on mount; activation toggles a local Set<AugmentationId>
// that drives the side-strip re-render.

import type { ComponentType, SvelteComponent } from 'svelte';

export type AugmentationId =
  | 'memory'
  | 'azazel'
  | 'video'
  | 'design'
  | 'daimonion'
  | 'three_d'
  | 'self';

/** Where the aug renders in the chat layout. */
export type AugPlacement = 'sidecard' | 'overlay' | 'popover' | 'split' | 'modal';

/**
 * Retention policy: how long the aug stays active after its trigger
 * resolves.
 * - `until_done` — until the aug emits its completion event
 *                    (e.g. `azazel:done`, `design_apply`).
 * - `next_message` — collapses on the next user message.
 * - `oneshot` — flashes for the current tool call, no card persists.
 * - `manual` — stays open until the user explicitly dismisses.
 */
export type AugRetention = 'until_done' | 'next_message' | 'oneshot' | 'manual';

/**
 * Props every aug component receives. The aug-specific component
 * destructures what it needs.
 */
export interface AugProps {
  /** Stable id of this aug instance (one per active aug, not the type id). */
  instanceId: string;
  /** The type id, e.g. 'memory'. */
  augId: AugmentationId;
  /** Free-form args parsed from the slash command, if any. */
  args: string;
  /** True when the user pinned this aug (📌). */
  pinned: boolean;
  /** Imperative handle to dismiss the aug from inside the component. */
  onDismiss: () => void;
  /** Imperative handle to pin/unpin from inside the component. */
  onTogglePin: () => void;
}

/** A registered augmentation. */
export interface Augmentation {
  id: AugmentationId;
  /** Visible label, e.g. "Memory". */
  label: string;
  /** Single emoji, e.g. "🧠". */
  icon: string;
  /** Slash commands that activate this aug from chat input. */
  slashCommands: string[];
  /** Tool names whose `onAiToolUse` should activate this aug. */
  toolTriggers: string[];
  /** Where the aug renders. */
  placement: AugPlacement;
  /** Svelte component for the card/popover body. */
  component: ComponentType<SvelteComponent<AugProps>>;
  /** Retention policy after the trigger resolves. */
  retention: AugRetention;
  /** True if the aug can also be opened in a fullscreen viewer
   *  (legacy tab-style). Drives the "Open fullscreen" affordance. */
  fullscreenAvailable: boolean;
  /** Legacy tab id used by the "Open fullscreen" button (P4 shim
   *  routes removed tabs back to the aug system). Null for augs
   *  that have no fullscreen view. */
  fullscreenTab: string | null;
}

const REGISTRY = new Map<AugmentationId, Augmentation>();

/**
 * Register an aug. Throws if an aug with the same id is already
 * registered — HMR re-imports shouldn't silently overwrite state.
 * Use `unregister(id)` first if you need to swap a component (e.g.
 * during dev iteration).
 */
export function register(a: Augmentation): void {
  if (REGISTRY.has(a.id)) {
    throw new Error(
      `augmentation "${a.id}" is already registered. ` +
        `Call unregister("${a.id}") first.`
    );
  }
  REGISTRY.set(a.id, a);
}

/** Remove an aug from the registry. Idempotent. */
export function unregister(id: AugmentationId): void {
  REGISTRY.delete(id);
}

/** Get a single aug by id. */
export function get(id: AugmentationId): Augmentation | undefined {
  return REGISTRY.get(id);
}

/** All registered augs, in insertion order. */
export function all(): Augmentation[] {
  return Array.from(REGISTRY.values());
}

/**
 * Resolve which aug a slash command maps to. The check is
 * case-insensitive and trims leading whitespace. Returns the first
 * match, or null.
 *
 * Example: `resolveSlash('/memory remember cats')` → the memory aug
 * with `args = 'remember cats'`.
 */
export function resolveSlash(text: string): { aug: Augmentation; args: string } | null {
  const trimmed = text.trimStart();
  for (const aug of REGISTRY.values()) {
    for (const cmd of aug.slashCommands) {
      // Match either bare command or command + space.
      const lower = trimmed.toLowerCase();
      const cmdLower = cmd.toLowerCase();
      if (lower === cmdLower) {
        return { aug, args: '' };
      }
      if (lower.startsWith(cmdLower + ' ')) {
        return { aug, args: trimmed.slice(cmd.length + 1).trim() };
      }
      if (lower.startsWith(cmdLower + '\t')) {
        return { aug, args: trimmed.slice(cmd.length + 1).trim() };
      }
    }
  }
  return null;
}

/**
 * Find an aug that should be activated by a given tool name.
 * Returns the first match or null.
 */
export function resolveTool(toolName: string): Augmentation | null {
  for (const aug of REGISTRY.values()) {
    if (aug.toolTriggers.includes(toolName)) return aug;
  }
  return null;
}
