// src/lib/tauri.ts
// Typed wrappers around Tauri IPC. We use the global `__TAURI__` injected by
// the Tauri runtime when `withGlobalTauri: true` (see tauri.conf.json).

interface TauriCore {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

interface TauriEvent {
  listen<T>(
    event: string,
    handler: (e: { event: string; payload: T; id: number }) => void,
  ): Promise<() => void>;
}

declare global {
  interface Window {
    __TAURI__?: { core?: TauriCore; event?: TauriEvent };
  }
}

function core(): TauriCore {
  const w = window as any;
  if (!w.__TAURI__) {
    console.error('[tauri.ts] window.__TAURI__ is missing. Are you running outside the Tauri webview?');
    throw new Error('Tauri runtime not detected. Open the app via `npm run tauri:dev`, not a regular browser.');
  }
  if (!w.__TAURI__.core) {
    console.error('[tauri.ts] window.__TAURI__ exists but has no .core. keys=', Object.keys(w.__TAURI__));
    throw new Error('Tauri core API missing.');
  }
  return w.__TAURI__.core;
}

function events(): TauriEvent {
  const e = (window as any).__TAURI__?.event;
  if (!e) {
    throw new Error('Tauri event API not available.');
  }
  return e;
}

// ---------- Types (mirror Rust) ----------

export type MonitorInfo = {
  id: number;
  name: string;
  width: number;
  height: number;
  is_primary: boolean;
};

export type CaptureOptions = {
  monitor_id?: number;
  fps?: number;
  max_width?: number;
};

export type SingleFrame = {
  base64: string;
  width: number;
  height: number;
  bytes: number;
  seq: number;
  t_ms: number;
  monitor_id: number;
};

export type CaptureStatePayload = {
  running: boolean;
  monitor_id: number;
  fps: number;
  max_width: number;
  frames_sent: number;
  frames_budget: number;
  auto_invocations_used?: number;
};

export type ScreenFramePayload = {
  seq: number;
  base64: string;
  width: number;
  height: number;
  t_ms: number;
  monitor_id: number;
};

export type AgentHintKind = 'hint' | 'noop' | 'error' | 'no_goal' | 'budget_exhausted' | 'stopped';

export type AgentHintPayload = {
  kind: AgentHintKind;
  text: string;
  seq?: number;
  t_ms: number;
};

export type CaptureErrorPayload = {
  code: 'permission_denied' | 'monitor_disconnected' | 'internal';
  message: string;
  t_ms: number;
};

// ---------- Video Mode ↔ Chat bridge ----------

export type VideoAutoTriggerPayload = {
  hint_text: string;
  seq: number;
  monitor_id: number;
  width: number;
  height: number;
  goal: string;
  t_ms: number;
};

export type ChatInjectPayload = {
  text: string;
  t_ms: number;
  source?: 'telegram' | 'videomode' | 'user';
};

export type AiVideoFramePayload = {
  id: string;
  kind: 'observe_now' | 'latest_frame';
  monitor_id: number;
  width: number;
  height: number;
  bytes: number;
  seq: number;
  t_ms: number;
  data_url: string;
};

// ---------- Commands ----------

export function listMonitors() {
  return core().invoke<MonitorInfo[]>('list_monitors');
}

export function startScreenCapture(opts: CaptureOptions = {}) {
  return core().invoke<void>('start_screen_capture', { opts });
}

export function stopScreenCapture() {
  return core().invoke<void>('stop_screen_capture');
}

export function captureSingleFrame(opts: CaptureOptions = {}) {
  return core().invoke<SingleFrame>('capture_single_frame', { opts });
}

export function getLatestFrame() {
  return core().invoke<SingleFrame | null>('get_latest_frame');
}

export function setActiveGoal(goal: string | null) {
  return core().invoke<void>('set_active_goal', { goal });
}

// ---- Video bridge commands ----

/** Persist the auto-invoke setting on the backend. The UI mirrors this
 *  in localStorage and pushes it on every toggle change. */
export function setVideoAutoinvoke(enabled: boolean) {
  return core().invoke<void>('set_video_autoinvoke', { enabled });
}

/** Push a synthetic user message into the chat. The Chat tab picks it
 *  up via the `chat-inject` event listener. */
export function chatInjectUserMessage(text: string) {
  return core().invoke<void>('chat_inject_user_message', { text });
}

/** Drain the single-slot pending auto-invoke (if any). The Chat tab
 *  calls this on mount / on becoming visible so it can pick up a
 *  trigger that fired while the listener wasn't installed. */
export function takePendingVideoAutoInvoke() {
  return core().invoke<VideoAutoTriggerPayload | null>('take_pending_video_auto_invoke');
}

/** One-shot capture used by the `video_observe_now` tool. */
export function videoObserveNow(opts: CaptureOptions = {}) {
  return core().invoke<SingleFrame>('video_observe_now', { opts });
}

/** Peek the most recent frame from the running capture. */
export function videoGetLatestFrame() {
  return core().invoke<SingleFrame | null>('video_get_latest_frame');
}

/** Set / change / clear the active hint goal. */
export function videoSetGoal(goal: string | null) {
  return core().invoke<void>('video_set_goal', { goal });
}

/** Start the capture + hint loop from the chat side. Idempotent. */
export function videoStartCapture(opts: CaptureOptions = {}) {
  return core().invoke<CaptureStatePayload>('video_start_capture', { opts });
}

/** Stop the capture + hint loop from the chat side. */
export function videoStopCapture() {
  return core().invoke<CaptureStatePayload>('video_stop_capture');
}

/** Custom window controls вЂ” calls our Rust `window_control` command
 *  (more reliable than `window.__TAURI__.window.getCurrentWindow()` which
 *   may not be exposed in all Tauri 2 builds). */
export const appWindow = {
  async minimize() {
    return core().invoke('window_control', { action: 'minimize' });
  },
  async toggleMaximize() {
    return core().invoke('window_control', { action: 'toggleMaximize' });
  },
  async maximize() {
    return core().invoke('window_control', { action: 'maximize' });
  },
  async unmaximize() {
    return core().invoke('window_control', { action: 'unmaximize' });
  },
  async close() {
    return core().invoke('window_control', { action: 'close' });
  },
};

// ---------- Chat & news ----------

export type ChatMessage = { role: 'user' | 'assistant' | 'system'; content: string };

export type ChatRequest = {
  messages: ChatMessage[];
  model?: string | null;
  max_tokens?: number | null;
  /** "default" | "three_d" — selects the AI tool set. */
  tools_preset?: string | null;
  /** Optional system prompt (Luna 3D uses this to inject 3D instructions). */
  system_prompt?: string | null;
};

/** Streaming Anthropic chat. Emits `ai_chunk` (string) and `ai_done` (true) events. */
export function aiChatStream(req: ChatRequest) {
  return core().invoke<void>('ai_chat_stream', { req });
}

/** Non-streaming MiniMax chat. Returns the model's text reply. */
export function callMinimax(messages: ChatMessage[], model?: string | null) {
  return core().invoke<string>('call_minimax', { messages, model });
}

export type ImageAspect = '1:1' | '16:9' | '9:16' | '4:3' | '3:4' | '21:9';

export type ImageGenRequest = {
  prompt: string;
  n?: number;
  aspect_ratio?: ImageAspect;
};

/** MiniMax text-to-image (image-01). Returns base64-encoded PNGs. */
export function generateImageMinimax(req: ImageGenRequest) {
  return core().invoke<string[]>('generate_image_minimax', {
    prompt: req.prompt,
    n: req.n ?? 1,
    aspect_ratio: req.aspect_ratio ?? '1:1',
  });
}

/** Streaming MiniMax (Global) chat. Emits `ai_chunk` (string) and `ai_done` (true) events. */
export function minimaxChatStream(req: ChatRequest) {
  return core().invoke<void>('minimax_chat_stream', { req });
}

export type NewsResult = {
  title: string;
  snippet: string;
  url: string;
  source: string;
  image: string;
};

// ---------- Proprietary search tools (no external API) ----------

export type SearchMatch = {
  path: string;
  line: number;
  col: number;
  snippet: string;
  score: number;
};

export type SearchOpts = {
  is_regex?: boolean;
  case_sensitive?: boolean;
  max_results?: number;
  context?: number;
  glob?: string | null;
};

/** Full-text / regex search across the active workspace (sandboxed). */
export function searchWorkspace(query: string, opts: SearchOpts = {}) {
  return core().invoke<SearchMatch[]>('search_workspace', { query, opts });
}

export type FetchedPage = {
  url: string;
  final_url: string;
  title: string;
  text: string;
  content_type: string;
  bytes: number;
};

/** Fetch a URL, extract title and plain text (HTML/JSON/text). */
export function fetchUrl(url: string) {
  return core().invoke<FetchedPage>('fetch_url', { url });
}

// ---------- Proprietary news aggregator (RSS-based) ----------

export type NewsItem = {
  source: string;
  title: string;
  url: string;
  snippet: string;
  published: string;
  fetched_at: number;
};

export type NewsSource = { id: string; label: string };

/** List registered news sources (Hacker News, The Verge, BBC, Habr, Ars). */
export function listNewsSources() {
  return core().invoke<NewsSource[]>('list_news_sources');
}

/**
 * Pull news from the registered RSS feeds in parallel. `source` is optional
 * (one of the `listNewsSources` ids); if omitted, all sources are queried.
 */
export function fetchNews(source?: string | null, limit = 10) {
  return core().invoke<NewsItem[]>('fetch_news', { source, limit });
}

/**
 * Real web search via a hidden Tauri webview (Google).
 * Mimics a real user browser: opens Google, waits for load, extracts results.
 * `query` вЂ” text, `limit` вЂ” max results (default 10, max 50).
 * Results are cached on disk for 30 minutes (LRU max 200 entries).
 */
export function webSearch(query: string, limit = 10) {
  return core().invoke<NewsItem[]>('web_search', { query, limit });
}

/** Drop the entire web-search cache. Returns how many entries were cleared. */
export function clearWebSearchCache() {
  return core().invoke<number>('clear_web_search_cache');
}

/** Get stats about the web-search cache (path, total/fresh/stale entries, TTL). */
export function webSearchCacheStats() {
  return core().invoke<{
    path: string;
    total: number;
    fresh: number;
    stale: number;
    ttl_secs: number;
    max_entries: number;
  }>('web_search_cache_stats');
}

// ---------- Chat history (persistent) ----------

export type ChatSummary = {
  id: string;
  name: string;
  updated_at: number;
  message_count: number;
  preview: string;
};

export type ChatFull = {
  id: string;
  name: string;
  updated_at: number;
  created_at: number;
  messages: any[];
};

export function saveChat(id: string | null, name: string | null, messages: any[]) {
  return core().invoke<ChatSummary>('save_chat', { id, name, messages });
}
export function listChats() { return core().invoke<ChatSummary[]>('list_chats'); }
export function loadChat(id: string) { return core().invoke<ChatFull>('load_chat', { id }); }
export function deleteChat(id: string) { return core().invoke<void>('delete_chat', { id }); }
export function renameChat(id: string, name: string) { return core().invoke<void>('rename_chat', { id, name }); }
export function currentChatId() { return core().invoke<string | null>('current_chat_id'); }
export function clearAllChats() { return core().invoke<number>('clear_all_chats'); }

/**
 * @deprecated DuckDuckGo API removed. Use `webSearch` for live internet,
 * `fetchNews` for RSS, `searchWorkspace` for local code, or `fetchUrl`
 * for a specific page. Kept as a shim so older UIs keep building.
 */
export async function searchNews(_query: string, _numResults = 5) {
  return { results: [] as Array<{ title: string; url: string; snippet?: string; source: string }> };
}

/** Open a URL in the system default browser. */
export function openUrl(url: string) {
  return core().invoke<void>('open_url', { url });
}

export function onAiChunk(handler: (delta: string) => void) {
  return events().listen<string>('ai_chunk', (e) => handler(e.payload));
}
export function onAiThinking(handler: (delta: string) => void) {
  return events().listen<string>('ai_thinking', (e) => handler(e.payload));
}
export type AiToolUsePayload = {
  id: string;
  name: string;
  args: Record<string, unknown>;
};
export type AiToolResultPayload = {
  id: string;
  name: string;
  ok: boolean;
  error?: string;
  prompt?: string;
  aspect?: ImageAspect;
  data_url?: string;
};
export function onAiToolUse(handler: (p: AiToolUsePayload) => void) {
  return events().listen<AiToolUsePayload>('ai_tool_use', (e) => handler(e.payload));
}
export function onAiToolResult(handler: (p: AiToolResultPayload) => void) {
  return events().listen<AiToolResultPayload>('ai_tool_result', (e) => handler(e.payload));
}
export type AiUserInterestsPayload = {
  id: string;
  name: string;
  ok: boolean;
  interests: string[];
};
export function onAiUserInterests(handler: (p: AiUserInterestsPayload) => void) {
  return events().listen<AiUserInterestsPayload>('ai_user_interests', (e) => handler(e.payload));
}
export type AiSubagentResultPayload = {
  id: string;
  name?: string;
  kind: 'research' | 'images';
  queries?: string[];
  subagents: Array<{
    query?: string;
    results?: Array<{ title: string; snippet: string; url: string; source: string }>;
    prompt?: string;
    aspect?: ImageAspect;
    data_url?: string;
  }>;
};
export function onAiSubagentResult(handler: (p: AiSubagentResultPayload) => void) {
  return events().listen<AiSubagentResultPayload>('ai_subagent_result', (e) => handler(e.payload));
}
export type AiWebSearchResult = {
  title: string;
  url: string;
  snippet: string;
  host: string;
};
export type AiWebSearchPayload = {
  id: string;
  query: string;
  results: AiWebSearchResult[];
};
export function onAiWebSearch(handler: (p: AiWebSearchPayload) => void) {
  return events().listen<AiWebSearchPayload>('ai_web_search', (e) => handler(e.payload));
}

/**
 * Emitted when the agent calls the `ask_user` tool — the round pauses
 * here and the UI shows the question with clickable options. The user
 * either clicks a button or types a free-form reply, which is sent as
 * the next user message (the model picks up where it left off).
 */
export type AiAskUserPayload = {
  id: string;
  question: string;
  options: string[];
};
export function onAiAskUser(handler: (p: AiAskUserPayload) => void) {
  return events().listen<AiAskUserPayload>('ai_ask_user', (e) => handler(e.payload));
}
export type AiUserInterestsViewPayload = {
  id: string;
  name?: string;
  ok: boolean;
  interests: string[];
};
export function onAiUserInterestsView(handler: (p: AiUserInterestsViewPayload) => void) {
  return events().listen<AiUserInterestsViewPayload>('ai_user_interests_view', (e) => handler(e.payload));
}

// ---------- step-by-step plan tools ----------

export type PlanStepStatus = 'pending' | 'in_progress' | 'done' | 'error';
export type PlanStep = {
  id: string;
  title: string;
  status: PlanStepStatus;
  note?: string;
};
export type AiPlanCreatedPayload = {
  id: string;
  name?: string;
  ok: boolean;
  title: string;
  steps: PlanStep[];
};
export type AiStepUpdatedPayload = {
  id: string;
  name?: string;
  ok: boolean;
  step_id: string;
  status: PlanStepStatus;
  note?: string;
};
export function onAiPlanCreated(handler: (p: AiPlanCreatedPayload) => void) {
  return events().listen<AiPlanCreatedPayload>('ai_plan_created', (e) => handler(e.payload));
}
export function onAiStepUpdated(handler: (p: AiStepUpdatedPayload) => void) {
  return events().listen<AiStepUpdatedPayload>('ai_step_updated', (e) => handler(e.payload));
}
export function onAiDone(handler: () => void) {
  return events().listen<boolean>('ai_done', () => handler());
}

/**
 * Push the frontend's interest list to the Rust side so the
 * `get_user_interests` tool can answer without a round-trip.
 * Call this on app boot and after every merge.
 */
export async function setUserInterests(interests: string[]): Promise<void> {
  await invoke('set_user_interests', { interests });
}

// ---------- API keys (keyring-backed) ----------

/**
 * Providers whose keys Luna Agent can store. Names match the normalized
 * `sandbox::provider_id` in the Rust backend, so passing any casing works.
 */
export type ApiProvider = 'minimax' | 'anthropic' | 'openai' | 'openrouter';

/** Read a previously stored key. Resolves to `null` if no entry exists. */
export function getApiKey(provider: ApiProvider) {
  return core().invoke<string | null>('get_api_key', { provider });
}

/** Persist a key in the OS keyring (Windows Credential Manager, etc.). */
export function setApiKey(provider: ApiProvider, key: string) {
  return core().invoke<void>('set_api_key', { provider, key });
}

// ---------- Events ----------

export function onScreenFrame(handler: (p: ScreenFramePayload) => void) {
  return events().listen<ScreenFramePayload>('screen-frame', (e) =>
    handler(e.payload),
  );
}
export function onAgentHint(handler: (p: AgentHintPayload) => void) {
  return events().listen<AgentHintPayload>('agent-hint', (e) => handler(e.payload));
}
export function onCaptureError(handler: (p: CaptureErrorPayload) => void) {
  return events().listen<CaptureErrorPayload>('capture-error', (e) =>
    handler(e.payload),
  );
}
export function onCaptureState(handler: (p: CaptureStatePayload) => void) {
  return events().listen<CaptureStatePayload>('capture-state', (e) =>
    handler(e.payload),
  );
}

/** Fired by the video-mode hint loop when a real `kind=hint` lands
 *  AND the user has `luna.video.autoinvoke` on. The chat tab uses
 *  this to push a synthetic user message into itself. */
export function onVideoAutoTrigger(handler: (p: VideoAutoTriggerPayload) => void) {
  return events().listen<VideoAutoTriggerPayload>('video-auto-trigger', (e) =>
    handler(e.payload),
  );
}

/** Fired when `chat_inject_user_message` is called. The Chat tab
 *  listens to this and feeds the text into its `send()` flow. */
export function onChatInject(handler: (p: ChatInjectPayload) => void) {
  return events().listen<ChatInjectPayload>('chat-inject', (e) => handler(e.payload));
}

/** Fired when a `video_observe_now` or `video_get_latest_frame` tool
 *  captures a frame. The chat UI shows a "viewed this frame" card. */
export function onAiVideoFrame(handler: (p: AiVideoFramePayload) => void) {
  return events().listen<AiVideoFramePayload>('ai_video_frame', (e) =>
    handler(e.payload),
  );
}

// ---------- Workspace (luna-agent) ----------

export type WorkspaceInfo = {
  path: string;
  name: string;
  total_files: number;
};

/** Open a folder as the active workspace. Sets it as the sandbox root. */
export function openWorkspace(path: string) {
  return core().invoke<WorkspaceInfo>('open_workspace', { path });
}

/** Show the native folder picker, then open the chosen folder. */
export function pickWorkspace() {
  return core().invoke<string | null>('pick_workspace');
}

/** Return the currently-open workspace, or null. */
export function currentWorkspace() {
  return core().invoke<WorkspaceInfo | null>('current_workspace');
}

/**
 * Auto-pick a workspace on startup. Resolution order:
 *   1) whatever the user already has open (no-op);
 *   2) the most recent workspace that still exists on disk;
 *   3) the process CWD.
 * Returns the resulting WorkspaceInfo, or null if nothing usable was found.
 */
export function defaultWorkspace() {
  return core().invoke<WorkspaceInfo | null>('default_workspace');
}

// ---------- Recent workspaces (persistent) ----------

/** List the most-recently-opened workspaces (up to 10). */
export function listRecentWorkspaces() {
  return core().invoke<WorkspaceInfo[]>('list_recent_workspaces');
}

/** Manually add a path to the recent list (auto-called by open_workspace). */
export function addRecentWorkspace(path: string) {
  return core().invoke<void>('add_recent_workspace', { path });
}

/** Clear the recent list. */
export function clearRecentWorkspaces() {
  return core().invoke<void>('clear_recent_workspaces');
}

// ---------- Project templates & creation ----------

export type TemplateFile = {
  path: string;
  content: string;
};

export type ProjectTemplate = {
  id: string;
  label: string;
  description: string;
  files: TemplateFile[];
};

/** List available project templates (HTML+JS, Vite+TS, Vite+React, blank). */
export function getProjectTemplates() {
  return core().invoke<ProjectTemplate[]>('get_project_templates');
}

/**
 * Create a new project from a template under `parentDir/<name>`, then open
 * it as the active workspace and add to recent. Returns the new workspace.
 */
export function createProject(name: string, templateId: string, parentDir: string) {
  return core().invoke<WorkspaceInfo>('create_project', {
    name,
    templateId,
    parentDir,
  });
}

// ---------- File ops (sandboxed) ----------

export type FileEntry = {
  path: string;
  kind: 'file' | 'dir';
  size: number;
};

/** Read a UTF-8 text file from inside the active workspace. */
export function readFile(path: string) {
  return core().invoke<string>('read_file', { path });
}

export type EditResult = {
  path: string;
  diff: string;
  bytes_written: number;
};

/**
 * Atomically replace `old` with `new` in a file.
 * Fails if `old` is not unique in the file. Returns a unified diff.
 */
export function editFile(path: string, old: string, newText: string) {
  return core().invoke<EditResult>('edit_file', { path, old, new: newText });
}

/** List directory entries (respects .gitignore). Path is relative to workspace. */
export function listDir(path: string, depth = 2) {
  return core().invoke<FileEntry[]>('list_dir', { path, depth });
}

// ---------- Preview (web) ----------

export type DevServer = {
  url: string;
  pid: number;
};

/**
 * Start a dev server for a project. Uses vite if available, otherwise falls
 * back to a built-in static file server. Returns the URL and OS pid.
 */
export function startDevServer(project: string, port?: number) {
  return core().invoke<DevServer>('start_dev_server', { project, port });
}

/** Open a new Tauri webview window pointing at `url`. */
export function openPreviewWindow(url: string, title?: string) {
  return core().invoke<string>('open_preview_window', { url, title });
}

// ---------- Voice input (STT) ----------

export type SttState = 'idle' | 'listening' | 'processing' | 'unknown';
export type SttUiState = 'idle' | 'recording' | 'transcribing' | 'error' | 'unknown';

export interface SttStateChange {
  state: SttState;
  isAvailable: boolean;
  language?: string;
}

export interface SttResult {
  transcript: string;
  isFinal: boolean;
  confidence?: number;
  audioData?: string;
}

export interface SttError {
  code: string;
  message: string;
  details?: string;
}

export interface SttDownloadProgress {
  status: 'downloading' | 'complete' | 'error';
  modelId?: string;
  model: string;
  progress?: number;
  downloaded?: number;
  total?: number;
  message?: string;
}

export interface WhisperModelInfo {
  id: string;
  displayName: string;
  sizeMb: number;
  requiredMemoryMb: number;
  installed: boolean;
  active: boolean;
  recommended: boolean;
  tier: string;
  language?: string | null;
  fitsInMemory: boolean;
  advanced: boolean;
}

export interface WhisperModelsResponse {
  models: WhisperModelInfo[];
  active?: string | null;
  totalDiskBytes: number;
  systemMemoryMb: number;
}

/** Convenience wrapper for `plugin:stt|*` commands. */
export const stt = {
  startListening(config?: { language?: string; maxDuration?: number }) {
    return core().invoke<void>('plugin:stt|start_listening', { config: config ?? {} });
  },
  stopListening() {
    return core().invoke<void>('plugin:stt|stop_listening');
  },
  listModels(includeAdvanced = false) {
    return core().invoke<WhisperModelsResponse>('plugin:stt|list_models', {
      includeAdvanced,
    });
  },
  installModel(id: string) {
    return core().invoke<void>('plugin:stt|install_model', { id });
  },
  setActiveModel(id: string) {
    return core().invoke<void>('plugin:stt|set_active_model', { id });
  },
  unloadModel() {
    return core().invoke<void>('plugin:stt|unload_model');
  },
};

export function getMicDevices() {
  return core().invoke<string[]>('get_mic_devices');
}

/** Where the plugin currently stores Whisper models (next to the .exe in dev). */
export function getModelsDir() {
  return core().invoke<string>('get_models_dir');
}

export function setMicDevice(name: string) {
  return core().invoke<void>('set_mic_device', { name });
}

export function onSttStateChange(handler: (p: SttStateChange) => void) {
  return events().listen<SttStateChange>('plugin:stt:stateChange', (e) =>
    handler(e.payload),
  );
}
export function onSttResult(handler: (p: SttResult) => void) {
  const wrapped = (e: { payload: SttResult }) => handler(e.payload);
  // The plugin emits on both channels; subscribe to one and let the
  // duplicate slip вЂ” `isFinal: true` is the gate that decides UX updates.
  return events().listen<SttResult>('stt://result', wrapped);
}
export function onSttError(handler: (p: SttError) => void) {
  return events().listen<SttError>('stt://error', (e) => handler(e.payload));
}
export function onSttDownloadProgress(handler: (p: SttDownloadProgress) => void) {
  return events().listen<SttDownloadProgress>('stt://download-progress', (e) =>
    handler(e.payload),
  );
}
export function onHotkeyPressed(handler: () => void) {
  return events().listen<string>('hotkey-pressed', () => handler());
}
export function onHotkeyReleased(handler: () => void) {
  return events().listen<string>('hotkey-released', () => handler());
}

// ---------- Workspace events (agent-style) ----------

/** Emitted by Rust whenever the active workspace changes (open or close). */
export type WorkspaceChangedPayload = {
  /** Absolute path of the new workspace, or `null` if the workspace was closed. */
  path: string | null;
  /** Workspace's basename (e.g. "my-app"), or `null` on close. */
  name?: string | null;
};

export function onWorkspaceChanged(handler: (p: WorkspaceChangedPayload) => void) {
  return events().listen<WorkspaceChangedPayload>('workspace_changed', (e) => handler(e.payload));
}

/** Close the currently-open workspace. Idempotent. */
export function closeWorkspace() {
  return core().invoke<void>('close_workspace');
}

// ---------- File edit events (agent-style) ----------

/** Emitted by Rust after a successful `edit_file` or `create_file` command. */
export type AiFileEditPayload = {
  /** Stable id used by `revertFileEdit` to roll the change back. */
  id: string;
  /** Workspace-relative path of the changed file. */
  path: string;
  /** Unified diff text. */
  diff: string;
  /** Length of the file's contents before the change. */
  before_len: number;
  /** Length of the file's contents after the change. */
  after_len: number;
};

export function onAiFileEdit(handler: (p: AiFileEditPayload) => void) {
  return events().listen<AiFileEditPayload>('ai_file_edit', (e) => handler(e.payload));
}

/** Emitted by Rust after `revert_file_edit` successfully rolls an edit back. */
export type AiEditRevertedPayload = {
  id: string;
  path: string;
};

export function onAiEditReverted(handler: (p: AiEditRevertedPayload) => void) {
  return events().listen<AiEditRevertedPayload>('ai_edit_reverted', (e) => handler(e.payload));
}

/** Emitted by Rust after the agent calls `read_file`. Used by the UI to show a "read" card. */
export type AiFileReadPayload = {
  id: string;
  path: string;
  bytes: number;
  lines: number;
  content: string;
};

export function onAiFileRead(handler: (p: AiFileReadPayload) => void) {
  return events().listen<AiFileReadPayload>('ai_file_read', (e) => handler(e.payload));
}

// ---------- New file commands ----------

/** Re-export of the Rust `EditResult` struct. `edit_id` is empty for read-only ops. */
export type EditResultPayload = {
  path: string;
  diff: string;
  bytes_written: number;
  edit_id: string;
};

/** Create a new UTF-8 text file in the workspace. Sandbox-enforced. */
export function createFile(path: string, content: string) {
  return core().invoke<EditResultPayload>('create_file', { path, content });
}

/** Roll back a file edit by `edit_id`. The id comes from `EditResultPayload.edit_id`. */
export function revertFileEdit(editId: string) {
  return core().invoke<EditResultPayload>('revert_file_edit', { editId });
}

/**
 * Result of `startDevServer` (already declared above) вЂ” re-declared here so
 * callers can import the type from one place.
 */
export type { DevServer };



// =====================================================================
// Memory service (Phase M0 + M1)
// =====================================================================
// Mirrors `src-tauri/src/services/memory/schema.rs` on the Rust side.
// See ADR-0009 for the design. Keep this section in sync with that
// file - both the field names and the wire format are part of the
// IPC contract.

/** Event kinds. Matches `EventKind` in schema.rs. */
export type MemoryEventKind =
  | 'chat_turn'
  | 'file_edit'
  | 'vision_trigger'
  | 'interest_update'
  | 'tool_call'
  | 'user_fact';

export type MemoryEvent = {
  id: string;
  /** ms since Unix epoch */
  ts: number;
  kind: MemoryEventKind;
  /** Display text, redacted of secrets at write time. */
  content: string;
  /** Optional structured payload (path + diff summary, etc.). */
  payload?: unknown;
  tags: string[];
  source: string;
  /** 0..=1 */
  importance: number;
  /** True if the payload contained a likely secret. Filtered from
   *  auto-recall unless the user explicitly asks. */
  secret: boolean;
};

export type MemoryLayerStatus = {
  l0: boolean;
  l1: boolean;
  l2: boolean;
  l3: boolean;
  graph: boolean;
};

export type MemoryStats = {
  layers: MemoryLayerStatus;
  l1_events: number;
  l3_events: number;
  l2_facts: number;
  l2_entities: number;
  l2_edges: number;
  disk_bytes: number;
  uptime_ms: number;
  schema_version: number;
};

export type RecallLayer = 'l0' | 'l1' | 'l2' | 'l3';

export type RecallHit = {
  layer: RecallLayer;
  id: string;
  text: string;
  /** 0..=1 */
  score: number;
  source?: string;
  ts: number;
};

export type RecallCounts = {
  l0: number;
  l1: number;
  l2: number;
  l3: number;
};

export type RecallBundle = {
  query: string;
  hits: RecallHit[];
  counts: RecallCounts;
  partial: boolean;
  elapsed_ms: number;
};

export type ConsolidationReport = {
  archived: number;
  dropped: number;
  elapsed_ms: number;
  archive_files: string[];
};

// ---- Commands ----

/** Snapshot of the memory service. Always returns Ok - the
 *  `layers` flags show which sub-layers are live. */
export function memoryStats() {
  return core().invoke<MemoryStats>('memory_stats');
}

/** Append an event to L1. Used by the "remember this" button and
 *  (later) the `remember()` agent tool. */
export function memoryAddEvent(
  kind: MemoryEventKind,
  content: string,
  tags: string[] = [],
  source = 'ui',
) {
  return core().invoke<string>('memory_add_event', { kind, content, tags, source });
}

/** List the most recent events, newest first. */
export function memoryListRecent(n = 50, kind?: MemoryEventKind | null) {
  return core().invoke<MemoryEvent[]>('memory_list_recent', { n, kind: kind ?? null });
}

/** Cheap L1 keyword search. M4 replaces with the full pipeline. */
export function memorySearch(query: string, topK = 10) {
  return core().invoke<RecallHit[]>('memory_search', { query, top_k: topK });
}

/** Full recall (L0+L1+L2+graph). M0/M1 returns L1-only results. */
export function memoryRecall(query: string, topK = 10) {
  return core().invoke<RecallBundle>('memory_recall', { query, top_k: topK });
}

/** Run the L1 -> L3 archive rotation now. */
export function memoryConsolidateNow(olderThanDays = 90) {
  return core().invoke<ConsolidationReport>('memory_consolidate_now', { older_than_days: olderThanDays });
}

/** Forget a single L1 event by id. The JSONL line is left in place
 *  (for `rebuild_index` recovery) but the index row is dropped. */
export function memoryForget(id: string) {
  return core().invoke<void>('memory_forget', { id });
}

// ---------- Telegram Bot ----------

export type TelegramStatus = {
  token_set: boolean;
  running: boolean;
  bot_username: string | null;
  started_at_ms: number | null;
  allow_list_size: number;
  last_activity_ms: number;
  last_error: string | null;
};

export function getTelegramStatus(): Promise<TelegramStatus> {
  return core().invoke<TelegramStatus>('get_telegram_status');
}

export function setTelegramToken(token: string): Promise<void> {
  return core().invoke<void>('set_telegram_token', { token });
}

export function clearTelegramToken(): Promise<void> {
  return core().invoke<void>('clear_telegram_token');
}

export function setTelegramAllowList(ids: number[]): Promise<void> {
  return core().invoke<void>('set_telegram_allow_list', { ids });
}

export function startTelegramBot(): Promise<string> {
  return core().invoke<string>('start_telegram_bot');
}

export function stopTelegramBot(): Promise<void> {
  return core().invoke<void>('stop_telegram_bot');
}

// ---------- Shell allow-list ----------

export type ShellAllowListEntry = {
  name: string;
  subcommand_patterns: string[];
};

export type ShellAllowList = {
  commands: ShellAllowListEntry[];
  default_timeout_ms: number;
  max_output_bytes: number;
};

export function getShellAllowList(): Promise<ShellAllowList> {
  return core().invoke<ShellAllowList>('get_shell_allow_list');
}

export function setShellAllowList(list: ShellAllowList): Promise<void> {
  return core().invoke<void>('set_shell_allow_list', { list });
}

export type CommandResult = {
  exit_code: number | null;
  duration_ms: number;
  stdout: string;
  stderr: string;
  stdout_truncated: boolean;
  stderr_truncated: boolean;
  timed_out: boolean;
};

export function runShellCommand(cmd: string, args: string[]): Promise<CommandResult> {
  return core().invoke<CommandResult>('run_shell_command', { cmd, args });
}

export function addShellCommand(name: string, subcommandPatterns: string[]): Promise<ShellAllowList> {
  return core().invoke<ShellAllowList>('add_shell_command', { name, subcommandPatterns });
}

export function removeShellCommand(name: string): Promise<ShellAllowList> {
  return core().invoke<ShellAllowList>('remove_shell_command', { name });
}

export function resetShellAllowList(): Promise<ShellAllowList> {
  return core().invoke<ShellAllowList>('reset_shell_allow_list');
}

// ---- Extra M2 commands ----

export type MemoryEntity = {
  id: string;
  name: string;
  kind: string;
  ts: number;
  importance: number;
};

/** Add a fact to L2 directly. Used by the "Remember" UI button
 *  and (in M5) the agent's `remember` tool. */
export function memoryAddFact(
  text: string,
  importance = 0.6,
  tags: string[] = [],
) {
  return core().invoke<string>('memory_add_fact', { text, importance, tags });
}

/** List all entities in the knowledge graph. */
export function memoryListGraphEntities() {
  return core().invoke<MemoryEntity[]>('memory_list_graph_entities');
}

// =====================================================================
// 3D editor (Luna 3D tab) — see src-tauri/src/services/three_d.rs
// =====================================================================

/**
 * Low-level escape hatch. Most callers should use the typed wrappers below
 * (threeDApplyOps, etc.), but this is exported so ThreeDChat.svelte can
 * pass through to a new Tauri command without having to add a wrapper
 * here every time.
 */
export function invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return core().invoke<T>(cmd, args);
}

/** Tauri event listener (returns an unlisten function). */
export function listen<T>(
  event: string,
  handler: (e: { event: string; payload: T; id: number }) => void,
): Promise<() => void> {
  return events().listen<T>(event, handler);
}

export type ThreeDApplyOpsResult = {
  applied: number;
  errors: { op: string; error: string }[];
};

export function threeDApplyOps(
  ops: unknown[],
  scene?: unknown,
  actor: 'user' | 'ai' = 'user',
): Promise<ThreeDApplyOpsResult> {
  return core().invoke<ThreeDApplyOpsResult>('three_d_apply_ops', { ops, scene, actor });
}

export function threeDSaveSceneSync(path: string, sceneJson: unknown): Promise<string> {
  return core().invoke<string>('three_d_save_scene_sync', { path, sceneJson });
}

export function threeDLoadScene(path: string): Promise<unknown> {
  return core().invoke<unknown>('three_d_load_scene', { path });
}

export function threeDGenerateTexture(prompt: string, aspectRatio?: string | null): Promise<string> {
  return core().invoke<string>('three_d_generate_texture', { prompt, aspectRatio: aspectRatio ?? null });
}

