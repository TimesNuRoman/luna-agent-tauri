// augmentations-bootstrap.ts
//
// Side-effect import: registers all built-in chat augmentations
// with the registry. Imported once from App.svelte (top of <script>)
// so every aug is available before the first chat render.
//
// Each entry is the minimum descriptor needed for activation +
// rendering. Augs without a `body` field render as a thin card
// (icon + label + args). Augs with a `body` host richer per-aug
// UIs in the chat side-strip (Telegram uses the full TelegramBot
// form; the rest stay header-only and rely on the fullscreen button
// for their dedicated panel).
//
// Retention policy follows the table from the UX-1 plan:
//   - memory:     next_message   (collapses after the next user msg)
//   - azazel:     until_done     (waits for `azazel:done`)
//   - video:      oneshot        (just attaches the last frame)
//   - design:     until_done     (waits for `design_apply`)
//   - daimonion:  manual         (user dismisses)
//   - three_d:    manual         (the editor stays open)
//   - self:       manual         (admin UI)
//   - telegram:   manual         (admin UI, lives in the chat)

import TelegramBot from '../TelegramBot.svelte';
import { register } from './augmentations';

// Re-registration guard: HMR can re-import this module in dev; without
// this, the second import would throw on the duplicate-id check.
let registered = false;
export function bootstrapAugmentations(): void {
  if (registered) return;
  registered = true;

  register({
    id: 'memory',
    label: 'Memory',
    icon: '🧠',
    slashCommands: ['/memory', '/remember', '/recall'],
    toolTriggers: [
      'memory_recall',
      'memory_search',
      'memory_add_event',
      'memory_add_fact',
      'memory_list_graph_entities',
      'memory_add_graph_entity',
      'memory_add_graph_relation',
      'memory_forget',
      'memory_consolidate_now',
      'memory_stats',
    ],
    placement: 'sidecard',
    retention: 'next_message',
    fullscreenAvailable: true,
    fullscreenTab: 'memory',
  });

  register({
    id: 'azazel',
    label: 'Azazel',
    icon: '😈',
    slashCommands: ['/azazel', '/browser'],
    toolTriggers: [
      'browser_navigate',
      'browser_click',
      'browser_type',
      'browser_screenshot',
      'browser_extract_text',
      'browser_current_url',
      'browser_wait',
      'browser_press_key',
      'browser_scroll',
      'browser_select_option',
      'browser_done',
    ],
    placement: 'sidecard',
    retention: 'until_done',
    fullscreenAvailable: true,
    fullscreenTab: 'azazel',
  });

  register({
    id: 'video',
    label: 'Video Mode',
    icon: '🎥',
    slashCommands: ['/video', '/screen', '/capture'],
    toolTriggers: ['capture_frame', 'screen_capture', 'video_capture'],
    placement: 'sidecard',
    retention: 'oneshot',
    fullscreenAvailable: true,
    fullscreenTab: 'video',
  });

  register({
    id: 'design',
    label: 'Mephistopheles',
    icon: '🎨',
    slashCommands: ['/design', '/mephisto', '/mephistopheles'],
    toolTriggers: [
      'design_manifest_get',
      'design_manifest_set',
      'design_brief_get',
      'design_brief_set',
      'design_palette_generate',
      'design_image_generate',
      'design_scaffold_generate',
      'design_copy_generate',
      'design_copy_apply',
      'design_component_propose',
      'design_apply',
    ],
    placement: 'sidecard',
    retention: 'until_done',
    fullscreenAvailable: true,
    fullscreenTab: 'design',
  });

  register({
    id: 'daimonion',
    label: 'Daimonion',
    icon: '🔮',
    slashCommands: ['/daimonion', '/voice'],
    toolTriggers: [
      'daimonion_transcribe',
      'daimonion_chat',
      'daimonion_synthesize',
      'daimonion_capture_frame',
    ],
    placement: 'sidecard',
    retention: 'manual',
    fullscreenAvailable: true,
    fullscreenTab: 'daimonion',
  });

  register({
    id: 'three_d',
    label: '3D Thoughts',
    icon: '💭',
    slashCommands: ['/3d', '/thoughts'],
    toolTriggers: [],
    placement: 'split',
    retention: 'manual',
    fullscreenAvailable: true,
    fullscreenTab: 'three_d',
  });

  register({
    id: 'self',
    label: 'Self',
    icon: '🧬',
    slashCommands: ['/self', '/evolve'],
    toolTriggers: [
      'self_inspect',
      'self_diagnose',
      'self_plan',
      'apply_self_update',
      'rollback_self_update',
    ],
    placement: 'sidecard',
    retention: 'manual',
    fullscreenAvailable: true,
    fullscreenTab: 'self_evolution',
  });

  register({
    id: 'telegram',
    label: 'Telegram Bot',
    icon: '🤖',
    slashCommands: ['/telegram', '/tg', '/телеграм', '/bot'],
    toolTriggers: [
      'telegram_connect',
      'telegram_status',
      'telegram_set_token',
      'telegram_clear_token',
      'telegram_set_allow_list',
      'telegram_start',
      'telegram_stop',
    ],
    placement: 'sidecard',
    // Full TelegramBot form in the chat side-strip (user chose
    // "Полный как Settings" — every control in one place).
    body: TelegramBot,
    retention: 'manual',
    fullscreenAvailable: true,
    fullscreenTab: 'telegram',
  });
}
