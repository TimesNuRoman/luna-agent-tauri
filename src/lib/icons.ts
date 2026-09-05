// src/lib/icons.ts
//
// Small set of custom SVG icons used across the Luna Agent UI. We
// keep them inline as a function that returns a string (the rendered
// SVG element) so the icons can be placed in @html or bound via
// {@html} in Svelte without needing a separate <Icon> component.
//
// Design language:
//   - 16x16 viewBox, currentColor strokes, no fill (stroke 1.6–1.8)
//   - Rounded line caps and joins for a soft, proprietary feel
//   - Single monochrome — picks up the surrounding text color, so
//     they look right in both light and dark themes
//   - Tiny details only — these are 16px-tall, anything finer is
//     noise at this scale
//
// All icons are 1.6-stroke on a 24x24 grid, scaled down with the
// `width` / `height` attributes of the <svg>.
// =====================================================================

function svg(body: string, title?: string): string {
  const t = title ? `<title>${title}</title>` : '';
  // Phase UX-2: 20x20 hits a sweet spot for 280px sidebars and the
  // 26-28px topbar/mode-tab buttons — large enough to be readable at
  // a glance, small enough that adjacent buttons keep breathing room.
  return `<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${t}${body}</svg>`;
}

// ---- mode tabs (Chat) ----

export const iconChat = (): string => svg(
  `<path d="M4 6.5a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-7l-4 3v-3H6a2 2 0 0 1-2-2v-8z"/>` +
  `<path d="M8 9.5h8M8 12.5h5"/>`
);

export const iconCode = (): string => svg(
  `<path d="M8 7l-5 5 5 5"/>` +
  `<path d="M16 7l5 5-5 5"/>` +
  `<path d="M14 5l-4 14"/>`
);

export const iconResearch = (): string => svg(
  `<circle cx="11" cy="11" r="6"/>` +
  `<path d="M15.5 15.5L20 20"/>` +
  `<path d="M9 11h4M11 9v4"/>` // small + inside the lens
);

export const iconMedia = (): string => svg(
  `<rect x="3" y="5" width="18" height="14" rx="2"/>` +
  `<circle cx="9" cy="11" r="1.5"/>` +
  `<path d="M21 16l-5-5-7 7"/>`
);

export const iconPlan = (): string => svg(
  `<rect x="4" y="4" width="16" height="16" rx="2"/>` +
  `<path d="M8 9h8M8 13h8M8 17h5"/>` +
  `<path d="M6.5 9.2l1 1 1.5-1.5"/>` // check on the first line
);

// ---- topbar tabs (App) ----

export const iconChatTopbar = (): string => svg(
  `<path d="M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-8l-4 3v-3H6a2 2 0 0 1-2-2V6z"/>` +
  `<path d="M8 9h8M8 12h5"/>`
);

export const iconVideo = (): string => svg(
  `<rect x="3" y="6" width="13" height="12" rx="2"/>` +
  `<path d="M16 10l5-3v10l-5-3z"/>`
);

export const iconMemory = (): string => svg(
  `<path d="M9 4a3 3 0 0 0-3 3v1a3 3 0 0 0-2 2.8 3 3 0 0 0 .8 4.6 3 3 0 0 0 1.2 3.8 3 3 0 0 0 3 .8v0a3 3 0 0 0 5 0v0a3 3 0 0 0 3-.8 3 3 0 0 0 1.2-3.8 3 3 0 0 0 .8-4.6 3 3 0 0 0-2-2.8V7a3 3 0 0 0-3-3 3 3 0 0 0-5 0z"/>` +
  `<path d="M9 12h.01M12 9h.01M15 12h.01M12 15h.01M12 12h.01"/>`
);

export const iconCube = (): string => svg(
  `<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3z"/>` +
  `<path d="M4 7.5l8 4.5 8-4.5"/>` +
  `<path d="M12 12v9"/>`
);

export const iconSelfEvolution = (): string => svg(
  `<circle cx="12" cy="12" r="2.5"/>` +
  `<path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1"/>` +
  `<circle cx="12" cy="12" r="6.5" stroke-dasharray="2 2"/>`
);

export const iconSettings = (): string => svg(
  `<circle cx="12" cy="12" r="3"/>` +
  `<path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>`
);

// ---- sidebar actions ----

export const iconPlus = (): string => svg(
  `<path d="M12 5v14M5 12h14"/>`
);

export const iconRefresh = (): string => svg(
  `<path d="M3 12a9 9 0 0 1 15.5-6.3L21 8"/>` +
  `<path d="M21 3v5h-5"/>` +
  `<path d="M21 12a9 9 0 0 1-15.5 6.3L3 16"/>` +
  `<path d="M3 21v-5h5"/>`
);

export const iconClose = (): string => svg(
  `<path d="M6 6l12 12M18 6L6 18"/>`
);

export const iconChevronUp = (): string => svg(
  `<path d="M6 15l6-6 6 6"/>`
);

export const iconChevronDown = (): string => svg(
  `<path d="M6 9l6 6 6-6"/>`
);

export const iconTrash = (): string => svg(
  `<path d="M4 7h16"/>` +
  `<path d="M9 7V5a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2"/>` +
  `<path d="M6 7l1 13a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-13"/>` +
  `<path d="M10 11v6M14 11v6"/>`
);

export const iconArrowUp = (): string => svg(
  `<path d="M12 19V5M5 12l7-7 7 7"/>`
);

export const iconArrowDown = (): string => svg(
  `<path d="M12 5v14M5 12l7 7 7-7"/>`
);

export const iconMore = (): string => svg(
  `<circle cx="5" cy="12" r="1.4"/>` +
  `<circle cx="12" cy="12" r="1.4"/>` +
  `<circle cx="19" cy="12" r="1.4"/>`
);

export const iconTasks = (): string => svg(
  `<rect x="3" y="4" width="18" height="16" rx="2"/>` +
  `<path d="M8 4v16M16 4v16"/>` +
  `<path d="M8 2v4M16 2v4"/>`
);

export const iconPlans = (): string => iconPlan();

// ---- chat history ----

/** A single chat bubble for the chat-history list. Slimmer than the
 *  iconChat used in the mode tab — fits in 18-20px sidebars without
 *  the inner "M8 9h8" detail lines. */
export const iconChatBubble = (): string => svg(
  `<path d="M5 5h14a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-9l-4 3v-3H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z"/>`
);

export const iconChatNew = (): string => svg(
  `<path d="M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-7l-4 3v-3H6a2 2 0 0 1-2-2V6z"/>` +
  `<path d="M12 8v6M9 11h6"/>`
);

export const iconTrashSmall = (): string => svg(
  `<path d="M5 7h14"/>` +
  `<path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/>` +
  `<path d="M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12"/>`
);

// ---- right sidebar (compact, single-purpose glyphs) ----
//
// The right sidebar is narrow (~340px) and the tab strip is even
// narrower (~52px per tab). These icons are designed to be read
// at 20x20 with the icon carrying 100% of the meaning — no text
// label is rendered next to them, only a `title` tooltip on
// hover. We follow the same 24-grid / 1.7-stroke / round-cap
// design language as the rest of icons.ts.

/** "План работ" — clipboard with a checked list. */
export const iconTabPlan = (): string => svg(
  `<rect x="5" y="4" width="14" height="17" rx="2"/>` +
  `<path d="M9 4V3a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v1"/>` +
  `<path d="M8 10l1.5 1.5L12 9"/>` +
  `<path d="M8 15l1.5 1.5L12 14"/>` +
  `<path d="M14 10h3"/>` +
  `<path d="M14 15h3"/>`
);

/** "Браузер" — globe with two arcs. */
export const iconTabBrowser = (): string => svg(
  `<circle cx="12" cy="12" r="9"/>` +
  `<path d="M3 12h18"/>` +
  `<path d="M12 3a14 14 0 0 1 0 18a14 14 0 0 1 0-18z"/>`
);

/** "Код" — angle brackets around a slash. */
export const iconTabCode = (): string => svg(
  `<path d="M9 7l-5 5 5 5"/>` +
  `<path d="M15 7l5 5-5 5"/>` +
  `<path d="M13 5l-2 14"/>`
);

/** "Веб-страница" — chain link. */
export const iconTabWeb = (): string => svg(
  `<path d="M10 14a4 4 0 0 1 0-5.7l3-3a4 4 0 0 1 5.7 5.7l-1.5 1.5"/>` +
  `<path d="M14 10a4 4 0 0 1 0 5.7l-3 3a4 4 0 0 1-5.7-5.7l1.5-1.5"/>`
);

/** "Ключи" — key. */
export const iconTabKeys = (): string => svg(
  `<circle cx="8" cy="12" r="4"/>` +
  `<path d="M11.5 12H21"/>` +
  `<path d="M18 12v3"/>` +
  `<path d="M21 12v2"/>`
);

/** "Self-эволюция" — brain (two halves with internal fold lines). */
export const iconTabSelf = (): string => svg(
  `<path d="M9 4a3 3 0 0 0-3 3v1a3 3 0 0 0-1.5 5.5A3 3 0 0 0 6 19v1a3 3 0 0 0 6 0V4a3 3 0 0 0-3 0z"/>` +
  `<path d="M15 4a3 3 0 0 1 3 3v1a3 3 0 0 1 1.5 5.5A3 3 0 0 1 18 19v1a3 3 0 0 1-6 0V4a3 3 0 0 1 3 0z"/>` +
  `<path d="M9 8h-1M9 12h-1M9 16h-1"/>` +
  `<path d="M15 8h1M15 12h1M15 16h1"/>`
);

/** "Азазель" — watchful eye. The pupil is offset to give the
 *  glyph a sense of gaze direction (Azazel = the messenger who
 *  sees everywhere). */
export const iconTabAzazel = (): string => svg(
  `<path d="M3 12s3.5-7 9-7 9 7 9 7-3.5 7-9 7-9-7-9-7z"/>` +
  `<circle cx="13" cy="12" r="2.5"/>`
);

/** "Люцифер" — MorningStar. A 4-point star with a tiny inner
 *  glow ring, hinting at the healer's "burn away the rot" vibe. */
export const iconTabLucifer = (): string => svg(
  `<path d="M12 3l1.8 6.2L20 11l-6.2 1.8L12 19l-1.8-6.2L4 11l6.2-1.8z"/>` +
  `<circle cx="12" cy="11" r="1.2"/>`
);

/** "Devices" — emulators & VMs. A small phone outline with a
 *  circular "running" dot — the universal "device is live" cue. */
export const iconTabDevices = (): string => svg(
  `<rect x="7" y="3" width="10" height="18" rx="2"/>` +
  `<path d="M11 18h2"/>` +
  `<circle cx="17.5" cy="6.5" r="1.2" fill="currentColor"/>`
);

// ---- composer (chat input bar) ----
//
// Slim glyphs designed for the 32-36 px icon-btn circles in the
// composer. The visual weight matches the rest of the icon set
// (24-grid, 1.7-stroke, round caps) so nothing feels out of place
// when the user toggles modes.

/** Microphone — voice input. Slight inner curve hints at a windscreen
 *  grille without drawing literal lines (which would muddy at 16 px). */
export const iconMic = (): string => svg(
  `<rect x="9" y="3" width="6" height="11" rx="3"/>` +
  `<path d="M5 11a7 7 0 0 0 14 0"/>` +
  `<path d="M12 18v3"/>` +
  `<path d="M8 21h8"/>`
);

/** Lightning — multitask mode. Single zigzag, intentionally
 *  asymmetric (the bolt leans right) to feel active rather than
 *  static. */
export const iconSpark = (): string => svg(
  `<path d="M13 3L5 14h6l-2 7 8-11h-6l2-7z"/>`
);

/** Key — credential manager. Round bow on the left, two teeth
 *  on the right (one square, one short pin) — reads as a classic
 *  door key at 16 px without aliasing. */
export const iconKey = (): string => svg(
  `<circle cx="8" cy="12" r="3.2"/>` +
  `<path d="M11 12h9"/>` +
  `<path d="M16 12v3"/>` +
  `<path d="M19 12v2"/>`
);

/** Paper plane — send. Filled chevron pointing up-and-right with
 *  a small notch — the universal "send" gesture. Stroke-only here
 *  so it picks up currentColor like the rest of the set. */
export const iconSend = (): string => svg(
  `<path d="M21 3L3 11l7 2 2 7 9-17z"/>` +
  `<path d="M10 13l5-5"/>`
);

/** Square with rounded corners — "stop generating" (replaces send
 *  while the LLM is mid-stream). Sits in the same 36px circle. */
export const iconStop = (): string => svg(
  `<rect x="6" y="6" width="12" height="12" rx="2"/>`
);

/** Speaker — TTS toggle. The two-tone "sound waves" mark is
 *  present but de-emphasised so the icon reads as a speaker, not
 *  a Wi-Fi glyph. */
export const iconVolume = (): string => svg(
  `<path d="M4 9v6h4l5 4V5L8 9H4z"/>` +
  `<path d="M16 9a3 3 0 0 1 0 6"/>` +
  `<path d="M19 6a7 7 0 0 1 0 12"/>`
);

/** Speaker muted — TTS off. Same shape with an X overlay. */
export const iconVolumeMute = (): string => svg(
  `<path d="M4 9v6h4l5 4V5L8 9H4z"/>` +
  `<path d="M16 10l4 4M20 10l-4 4"/>`
);
