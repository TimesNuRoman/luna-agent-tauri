<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import {
    getApiKey,
    minimaxChatStream,
    webSearch,
    searchWorkspace,
    fetchUrl,
    fetchNews,
    webSearchCacheStats,
    clearWebSearchCache,
    openUrl,
    onAiChunk,
    onAiThinking,
    onAiToolUse,
    onAiToolResult,
    onAiUserInterests,
    onAiSubagentResult,
    onAiWebSearch,
    onAiAskUser,
    onAiUserInterestsView,
    onAiPlanCreated,
    onAiStepUpdated,
    onAiDone,
    onAiVideoFrame,
    onChatInject,
    takePendingVideoAutoInvoke,
    setUserInterests,
    stt,
    getModelsDir,
    onSttStateChange,
    onSttResult,
    onSttError,
    onSttDownloadProgress,
    onHotkeyPressed,
    onHotkeyReleased,
    // --- Agent (file/workspace) tools ---
    openWorkspace,
    closeWorkspace,
    pickWorkspace,
    currentWorkspace,
    defaultWorkspace,
    listRecentWorkspaces,
    clearRecentWorkspaces,
    addRecentWorkspace,
    listDir,
    readFile,
    startDevServer,
    openPreviewWindow,
    getProjectTemplates,
    createProject,
    createFile,
    revertFileEdit,
    onWorkspaceChanged,
    onAiFileEdit,
    onAiEditReverted,
    onAiFileRead,
    type ChatMessage,
    type SttUiState,
    type WhisperModelInfo,
    type ImageAspect,
    type WorkspaceInfo,
    type FileEntry,
    type ProjectTemplate,
    type DevServer,
    type AiFileEditPayload,
    type AiFileReadPayload,
    type AiAskUserPayload,
  } from './lib/tauri';
  import { saveChat, loadChat, currentChatId, clearAllChats } from './lib/tauri';
  import { apiKeyStatus, refreshKeyStatus } from './lib/keyStore';
  import { safeRenderMarkdown as renderMarkdown } from './lib/markdown';
  import {
    createPlan,
    recordAgentPlan,
    applyAgentStepUpdate,
    linkPlanToMessage,
    findPlanByTitle,
    buildPlanRunPrompt,
    buildPlanContinuePrompt,
    type Plan,
    type PlanStep,
    type PlanStepStatus,
  } from './lib/planStore';

  export let providerLabel = 'Luna Agent';

  // Single source of truth for "is the MiniMax key present?". Mirrored
  // from the shared store so we don't re-read the keyring here.
  $: hasMinimax = $apiKeyStatus === 'present';

  // ---- state ----
  type Mode = 'chat' | 'code' | 'research' | 'media' | 'plan';
  let mode: Mode = 'chat';

  // ---- plan mode composer state ----
  // When `mode === 'plan'`, the composer renders a title field and a
  // multi-line steps textarea instead of the regular chat input. "Save"
  // pushes the draft into the local plan store; "Run" creates a real
  // plan, then dispatches a `luna:plan-run` window event that this
  // component (in another onMount) catches and turns into a `doChat`.
  let planTitle = '';
  let planStepsText = '1. \n2. \n3. ';
  let planTitleInputEl: HTMLInputElement | null = null;
  let planStepsInputEl: HTMLTextAreaElement | null = null;
  /** When the user clicks Run (from the sidebar OR from the composer)
   *  we set this so the upcoming ai_plan_created event can wire the
   *  resulting chat message back to the local plan. Cleared after
   *  the link. */
  let pendingLinkPlanId: string | null = null;

  // ---- chat history (persisted on the Rust side) ----
  // `chatId` is null for a fresh conversation; once we save, the backend
  // mints an id and keeps it across restarts. `loadedFromDisk` guards
  // against the auto-intro overwriting a restored chat in onMount.
  let chatId: string | null = null;
  let loadedFromDisk = false;
  let chatSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let chatSaving = false;

  // ---- ask_user (human-in-the-loop) ----
  // When the agent calls ask_user(question, options?) we render an
  // inline card with the question + clickable options. The current
  // pending question lives here so the rest of the UI (e.g. the
  // composer) can adapt.
  let pendingAskUser: { id: number; callId: string; question: string; options: string[] } | null = null;

  // Debounced auto-save: every change to `messages` schedules a save
  // ~600 ms later. Streaming chunks during model output get coalesced
  // into a single write so the disk doesn't churn.
  $: if (loadedFromDisk || chatId) {
    scheduleChatSave(messages);
  }

  function scheduleChatSave(msgs: typeof messages) {
    if (chatSaveTimer) clearTimeout(chatSaveTimer);
    chatSaveTimer = setTimeout(() => {
      chatSaveTimer = null;
      persistChat(msgs);
    }, 600);
  }

  async function persistChat(msgs: typeof messages) {
    // Don't save an empty conversation (e.g. right after reset) — the
    // backend's derive_chat_name would produce a generic timestamp.
    const real = msgs.filter((m) => m.role !== 'system');
    if (real.length === 0) return;
    try {
      chatSaving = true;
      // Strip non-serializable DOM fields before crossing the IPC bridge.
      const slim = msgs.map((m) => ({
        id: m.id,
        role: m.role,
        html: m.html,
        raw: m.raw,
        streaming: !!m.streaming,
        thinking: m.thinking,
        thinkingOpen: m.thinkingOpen,
        kind: m.kind,
        imageDataUrl: m.imageDataUrl,
        imagePrompt: m.imagePrompt,
        imageAspect: m.imageAspect,
        toolName: m.toolName,
        toolArgs: m.toolArgs,
        toolArgsOpen: m.toolArgsOpen,
        toolError: m.toolError,
        toolStatus: m.toolStatus,
        subagents: m.subagents,
        subKind: m.subKind,
        webQuery: m.webQuery,
        webResults: m.webResults,
        webExpanded: m.webExpanded,
        planTitle: m.planTitle,
        planSteps: m.planSteps,
        typedText: m.typedText,
        pendingText: m.pendingText,
        toolCount: m.toolCount,
        modelTag: m.modelTag,
        createdAt: m.createdAt,
        filePath: m.filePath,
        fileDiff: m.fileDiff,
        fileEditId: m.fileEditId,
        fileEditState: m.fileEditState,
        fileReadBytes: m.fileReadBytes,
        fileReadLines: m.fileReadLines,
        fileReadContent: m.fileReadContent,
        fileReadOpen: m.fileReadOpen,
        videoFrameUrl: m.videoFrameUrl,
        videoFrameKind: m.videoFrameKind,
        videoFrameMeta: m.videoFrameMeta,
      }));
      const summary = await saveChat(chatId, null, slim);
      chatId = summary.id;
    } catch (e) {
      console.warn('chat save failed', e);
    } finally {
      chatSaving = false;
    }
  }

  async function startNewChat() {
    // Flush any pending save before switching so we don't lose the
    // trailing chunks of the previous conversation.
    if (chatSaveTimer) {
      clearTimeout(chatSaveTimer);
      chatSaveTimer = null;
      await persistChat(messages);
    }
    chatId = null;
    messages = [];
    loadedFromDisk = true; // skip the auto-intro; user wants a clean slate
    appendMessage('system', introForMode(mode, hasMinimax));
    loadedFromDisk = false;
  }

  async function nukeAllChats() {
    if (!confirm('Удалить ВСЮ историю чатов? Это необратимо.')) return;
    try {
      if (chatSaveTimer) {
        clearTimeout(chatSaveTimer);
        chatSaveTimer = null;
      }
      await clearAllChats();
      chatId = null;
      messages = [];
      loadedFromDisk = true;
      appendMessage('system', introForMode(mode, hasMinimax));
      loadedFromDisk = false;
    } catch (e) {
      console.warn('clear_all_chats failed', e);
    }
  }

  // The user clicked one of the `ask_user` option buttons. We:
  //   1. mark the ask_user message as answered (collapses the buttons);
  //   2. clear the pendingAskUser hint;
  //   3. append the user's choice as a regular user message so the
  //      next model turn picks it up (the round on the backend was
  //      already ended by the `ask_user` tool).
  function answerAskUser(askMsgId: number, answer: string) {
    if (!answer.trim()) return;
    messages = messages.map((m) =>
      m.id === askMsgId ? { ...m, askAnswer: answer } : m
    );
    pendingAskUser = null;
    appendMessage('user', answer);
    // Send immediately so the user doesn't have to hit Enter on a
    // pre-filled input.
    setTimeout(() => doChat(), 0);
  }

  // The user picked "Свой ответ" — pre-fill the composer with the
  // question as a hint and focus it. They can edit and hit Enter.
  function focusComposerForAskUser(question: string) {
    pendingAskUser = null;
    inputText = question ? question + ' — ' : '';
    setTimeout(() => inputEl?.focus(), 50);
  }
  let mediaSearch = '';
  let messages: Array<{ id: number; role: string; html: string; raw?: string; streaming?: boolean; thinking?: string; thinkingOpen?: boolean; kind?: 'text' | 'image' | 'image_loading' | 'tool_use' | 'tool_result' | 'subagents' | 'web_search' | 'plan' | 'file_edit' | 'file_read' | 'video_frame' | 'ask_user'; imageDataUrl?: string; imagePrompt?: string; imageAspect?: ImageAspect; toolName?: string; toolArgs?: string; toolArgsOpen?: boolean; toolError?: string; toolStatus?: 'pending' | 'ok' | 'error'; subagents?: Array<{ id: number; title: string; status: 'pending' | 'ok' | 'error'; result?: { title: string; snippet: string; url: string; source: string }[]; dataUrl?: string; aspect?: string }>; subKind?: 'research' | 'images'; webQuery?: string; webResults?: Array<{ title: string; url: string; snippet: string; host: string }>; webExpanded?: boolean; planTitle?: string; planSteps?: Array<{ id: string; title: string; status: 'pending' | 'in_progress' | 'done' | 'error'; note?: string }>; typedText?: string; pendingText?: string; toolCount?: number; modelTag?: string; createdAt?: number; filePath?: string; fileDiff?: string; fileEditId?: string; fileEditState?: 'pending' | 'accepted' | 'rejected' | 'expired'; fileReadBytes?: number; fileReadLines?: number; fileReadContent?: string; fileReadOpen?: boolean; videoFrameUrl?: string; videoFrameKind?: 'observe_now' | 'latest_frame'; videoFrameMeta?: { monitor_id: number; width: number; height: number; bytes: number; seq: number; t_ms: number }; askQuestion?: string; askOptions?: string[]; askCallId?: string; askAnswer?: string }> = [];
  let inputText = '';
  // Exposed via bind:busy so the parent (App.svelte) can pass it to
  // the PlansSidebar and disable the Run button while we stream.
  export let busy = false;
  let errorBanner = '';

  // ---- multitask mode ----
  // A sub-toggle that lives inside the input field (not a top-level mode tab).
  // When ON, the next chat request is prefixed with a system message that
  // tells the model to fan out via `parallel_research` / `parallel_generate_images`
  // instead of running things sequentially. The hint is sent in the request
  // body only — it never lands in `history` or in the visible chat.
  const MULTITASK_STORAGE_KEY = 'luna.chat.multitask';
  const MULTITASK_HINT =
    '[MULTITASK MODE] The user has enabled parallel-mode for this turn. ' +
    'Prefer fan-out tools: use `parallel_research` when the question spans ' +
    '2+ topics (compare, survey, news across subjects), and ' +
    '`parallel_generate_images` when the user wants several visuals at once. ' +
    'Keep individual sub-queries short (1–4 words). Do not narrate the ' +
    'parallelism — just call the tool.';
  let multitask = false;
  let nextId = 1;

  // ---- thinking extraction (defensive) ----
  // The Rust side already splits `delta.content` and `delta.reasoning_content`
  // into separate events. But some models also wrap their thinking in
  // `<think>...</think>` tags INSIDE the content field, which would
  // otherwise leak into the visible message. We strip those tags on
  // the fly and route the body to the `m.thinking` field, which the
  // UI renders in a separate collapsible block.
  function stripThinkingTags(s: string): { text: string; thinking: string } {
    const re = /<think>([\s\S]*?)<\/think>/gi;
    let thinking = '';
    const text = s.replace(re, (_m, body: string) => {
      thinking += (thinking ? '\n\n' : '') + body.trim();
      return '';
    });
    // Drop any orphan tags the model may have emitted as leftovers
    // (e.g. when the stream cut mid-block).
    const cleaned = text.replace(/<\/?think\s*\/?>/gi, '').trimStart();
    return { text: cleaned, thinking };
  }

  // ---- typewriter ----
  // Streamed chunks land in `pendingText`; a 16ms tick (rAF-paced) pulls
  // a couple of characters at a time into `typedText` and re-renders.
  // When the stream ends, we flush the rest in one shot so the final
  // word lands immediately instead of trailing off.
  let typeTickHandle: number | null = null;
  function startTypeTick(id: number) {
    if (typeTickHandle != null) return;
    const tick = () => {
      // Process every currently-streaming message.
      let anyProgress = false;
      messages = messages.map((m) => {
        if (!m.streaming) return m;
        const pending = m.pendingText ?? '';
        if (!pending) return m;
        // Pull up to 4 graphemes per tick. CJK / emoji are 1-2 graphemes
        // each so this is a comfortable 60-120 chars/sec.
        const take = Math.min(4, [...pending].length);
        const cut = [...pending].slice(0, take).join('');
        const rest = [...pending].slice(take).join('');
        anyProgress = true;
        const typedText = (m.typedText ?? '') + cut;
        return {
          ...m,
          typedText,
          pendingText: rest,
          html: renderMarkdown(typedText),
          raw: typedText,
        };
      });
      // If no streaming message has pending text left, stop the loop.
      const stillStreaming = messages.some((m) => m.streaming && (m.pendingText ?? '') !== '');
      if (!stillStreaming) {
        if (typeTickHandle != null) {
          cancelAnimationFrame(typeTickHandle);
          typeTickHandle = null;
        }
        return;
      }
      typeTickHandle = requestAnimationFrame(tick);
    };
    typeTickHandle = requestAnimationFrame(tick);
  }
  function stopTypeTickAndFlush() {
    if (typeTickHandle != null) {
      cancelAnimationFrame(typeTickHandle);
      typeTickHandle = null;
    }
    messages = messages.map((m) => {
      if (!m.streaming) return m;
      const pending = m.pendingText ?? '';
      const typedText = (m.typedText ?? '') + pending;
      return {
        ...m,
        typedText,
        pendingText: '',
        html: renderMarkdown(typedText),
        raw: typedText,
      };
    });
  }
  function formatTime(ts?: number): string {
    if (!ts) return '';
    const d = new Date(ts);
    const hh = String(d.getHours()).padStart(2, '0');
    const mm = String(d.getMinutes()).padStart(2, '0');
    return `${hh}:${mm}`;
  }

  // ---- voice input ----
  let voiceState: SttUiState = 'idle';
  let voiceError = '';
  let voiceUnlistens: Array<() => void> = [];
  // Whisper model management
  let whisperModels: WhisperModelInfo[] = [];
  let activeModelId: string | null = null;
  let modelsDir = '';
  let modelPanelOpen = false;
  let autoModalOpen = false;
  let installingId: string | null = null;
  let downloadProgress = '';
  let downloadPct: number | null = null;
  async function toggleVoice() {
    voiceError = '';
    try {
      if (voiceState === 'recording') {
        await stt.stopListening();
      } else if (voiceState !== 'transcribing') {
        await stt.startListening({ maxDuration: 30_000 });
      }
    } catch (e) {
      voiceError = String(e);
    }
  }
  async function refreshWhisperModels() {
    try {
      const res = await stt.listModels(false);
      whisperModels = res.models;
      activeModelId = res.active ?? null;
    } catch (e) {
      console.warn('[voice] list_models failed:', e);
    }
  }
  async function installWhisperModel(id: string) {
    installingId = id;
    downloadPct = 0;
    downloadProgress = `${id}: starting...`;
    try {
      await stt.installModel(id);
      await refreshWhisperModels();
    } catch (e) {
      voiceError = `install ${id}: ${e}`;
    } finally {
      installingId = null;
      setTimeout(() => {
        if (installingId == null) {
          downloadProgress = '';
          downloadPct = null;
        }
      }, 1500);
    }
  }
  async function setActiveWhisperModel(id: string) {
    try {
      await stt.setActiveModel(id);
      await refreshWhisperModels();
    } catch (e) {
      voiceError = `set_active ${id}: ${e}`;
    }
  }
  function postponeModel() { autoModalOpen = false; }
  function dismissModel() { autoModalOpen = false; }

  // MiniMax key — now derived from the shared `keyStore` (see the `$:`
  // declaration near the top of the script). We still need `checkingKeys`
  // for the header pill ("…", "set", "missing") until the first refresh
  // completes.
  $: checkingKeys = $apiKeyStatus === 'unknown';

  type ModelOption = { id: string; label: string; model: string; contextWindow: number };
  // Model catalog aligned with the official MiniMax API docs (verified
  // 2026-08-13). M3 is the frontier model with a 1M-token context,
  // multimodal inputs (text + images + video), and tool use. M2.7 is
  // the stable workhorse for engineering flows. M2-her was an early
  // internal alias that no longer appears in the public catalog, so
  // we dropped it in favour of the documented tiers.
  const MODELS: ModelOption[] = [
    { id: 'auto',          label: 'Auto (MiniMax-M3)',    model: '',                contextWindow: 1_048_576 },
    { id: 'minimax-M3',     label: 'MiniMax M3 (latest)',  model: 'MiniMax-M3',     contextWindow: 1_048_576 },
    { id: 'minimax-M2.7-highspeed', label: 'MiniMax M2.7 Highspeed', model: 'MiniMax-M2.7-highspeed', contextWindow: 204_800 },
    { id: 'minimax-M2.7',   label: 'MiniMax M2.7',         model: 'MiniMax-M2.7',   contextWindow: 204_800 },
    { id: 'minimax-M2.5',   label: 'MiniMax M2.5 (legacy)', model: 'MiniMax-M2.5', contextWindow: 204_800 },
  ];
  const MODEL_STORAGE_KEY = 'luna.chat.model';
  let selectedModelId = 'auto';

  let history: ChatMessage[] = [];
  // ---- context usage ----
  // Show the user how full the model's context window is. Recomputed
  // whenever the visible message list or the current model changes.
  // The token estimate is approximate — we count characters (with
  // a per-grapheme weight) since real tokenization would require
  // shipping a tokenizer in the bundle.
  function estimateTokens(text: string): number {
    if (!text) return 0;
    // CJK and emoji are usually 1-2 tokens per character; Latin/cyrillic
    // is ~1 token per 4 chars. We split into graphemes via the spread
    // operator (handles surrogate pairs correctly).
    const chars = [...text];
    let t = 0;
    for (const ch of chars) {
      const code = ch.codePointAt(0) || 0;
      if (code > 0x2E80) t += 1.5; // CJK / fullwidth
      else if (code > 0x7F) t += 0.6; // accented / cyrillic
      else if (ch === ' ' || ch === '\n') t += 0.25;
      else t += 0.27;
    }
    return Math.max(1, Math.round(t));
  }
  let contextPopover = false;
  let contextView: 'summary' | 'content' = 'summary';
  // Refs for the outside-click detector. Without these, the popover
  // would either close on every click (broken `on:blur` race) or
  // refuse to close at all. `<svelte:window>` does the heavy lifting.
  let contextBtnEl: HTMLButtonElement | null = null;
  let contextPopEl: HTMLDivElement | null = null;
  let contextCopyHint = '';
  let contextCopyTimer: ReturnType<typeof setTimeout> | null = null;
  function computeContext(): { used: number; window: number; pct: number; perMessage: Array<{ id: number; role: string; preview: string; tokens: number }> } {
    const m = selectedModel ?? MODELS[0];
    const window = m?.contextWindow ?? 0;
    let used = 0;
    const perMessage: Array<{ id: number; role: string; preview: string; tokens: number }> = [];
    for (const m of messages) {
      // Reconstruct the text we'd actually send. Image and tool messages
      // also count, but at a small constant cost.
      let text = '';
      if (m.kind === 'image') {
        // Images count as ~765 tokens for low-res or 1500+ for high-res.
        text = ' '.repeat(800);
      } else if (m.kind === 'subagents' && m.subagents) {
        text = m.subagents.map((s) => s.title + (s.dataUrl ? ' ' : '') + (s.result || []).map((r) => r.title + ' ' + r.snippet).join(' ')).join(' ');
      } else if (m.kind === 'web_search' && m.webResults) {
        text = (m.webQuery || '') + ' ' + m.webResults.map((r) => r.title + ' ' + r.snippet).join(' ');
      } else if (m.kind === 'plan' && m.planSteps) {
        text = (m.planTitle || '') + ' ' + m.planSteps.map((s) => s.title + (s.note ? ' ' + s.note : '')).join(' ');
      } else if (m.kind === 'tool_use' || m.kind === 'tool_result') {
        text = m.toolArgs || m.toolError || m.toolName || '';
      } else {
        text = m.raw || m.html || '';
      }
      const tokens = estimateTokens(text);
      used += tokens + 4; // ~4 tokens of role/format overhead per message
      if (tokens > 0) {
        perMessage.push({ id: m.id, role: m.role, preview: text.slice(0, 60), tokens: tokens + 4 });
      }
    }
    // The model also burns tokens for system prompt + tool schemas —
    // bake in a flat overhead so the gauge doesn't sit at 0% early.
    used += 600;
    const pct = window > 0 ? Math.min(100, Math.round((used / window) * 100)) : 0;
    return { used, window, pct, perMessage };
  }
  $: contextInfo = (() => {
    // Re-run whenever messages or the model id change.
    void messages; void selectedModelId;
    return computeContext();
  })();
  function contextBucket(pct: number): 'low' | 'mid' | 'high' | 'crit' {
    if (pct >= 85) return 'crit';
    if (pct >= 65) return 'high';
    if (pct >= 40) return 'mid';
    return 'low';
  }
  function formatTokens(n: number): string {
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
    if (n >= 10_000) return (n / 1000).toFixed(0) + 'K';
    if (n >= 1_000) return (n / 1000).toFixed(1) + 'K';
    return String(n);
  }
  function clearContext() {
    if (busy) return;
    if (!confirm('Очистить историю чата? Контекст сбросится, текущая беседа удалится.')) return;
    messages = [];
    history = [];
    researchResults = [];
    appendMessage('system', 'Чат очищен.');
    contextPopover = false;
  }
  // ---- "real" context payload ----
  // We mirror the live `messages` array so the popover is meaningful
  // from the first user message and stays in sync as the assistant
  // streams chunks. `history` is also built from the same data right
  // before the API call, so the breakdown here is what the model
  // will see minus the Rust-side system prompt + tool schemas.
  //
  // Each item carries:
  //   - `msgId`  — the original message id, used for scroll-into-view
  //   - `kind`   — display category for the summary breakdown
  //   - `role`   — raw API role
  //   - `content`— full reconstructed text (not a preview)
  //   - `tokens` / `chars` — usage estimate
  type RealContextKind = 'system' | 'user' | 'assistant' | 'thinking' | 'tool' | 'image' | 'plan' | 'web' | 'subagent' | 'file';
  type RealContextItem = {
    id: string;
    msgId: number;
    kind: RealContextKind;
    role: string;
    content: string;
    tokens: number;
    chars: number;
    label: string;
  };
  function buildRealContext(): RealContextItem[] {
    const items: RealContextItem[] = [];
    // 1) System prompt placeholder. Rust adds the actual system prompt
    //    + tool schemas on top of `history`; we don't have that text
    //    here, so we render a labelled stub so the user sees the cost.
    items.push({
      id: 'system',
      msgId: 0,
      kind: 'system',
      role: 'system',
      content: '[Системный промпт + схемы инструментов]\nФормируется на стороне Rust. Содержит инструкции поведения ассистента, описание доступных tool calls (web_search, parallel_research, parallel_generate_images, generate_image) и правила формата ответов.',
      tokens: 600,
      chars: 2400,
      label: 'Системный промпт',
    });
    // 2) Walk the live message list. We deliberately include the
    //    streaming message too, so the gauge moves as Luna types.
    for (const m of messages) {
      if (m.role === 'system') continue; // already represented by stub
      let text = '';
      let kind: RealContextKind = 'user';
      let label = m.role;
      if (m.role === 'user') {
        text = m.raw || m.html || '';
        kind = 'user';
        label = '👤 Ты';
      } else if (m.role === 'assistant') {
        if (m.thinking) text += m.thinking + '\n';
        text += m.raw || m.html || '';
        if (m.thinking) {
          items.push({
            id: `t-${m.id}`,
            msgId: m.id,
            kind: 'thinking',
            role: 'assistant',
            content: m.thinking,
            tokens: estimateTokens(m.thinking) + 4,
            chars: m.thinking.length,
            label: '💭 Рассуждения',
          });
        }
        if (m.kind === 'tool_use' || m.kind === 'tool_result') {
          text = (m.toolName || '') + ' ' + (m.toolArgs || '') + ' ' + (m.toolError || '');
          kind = 'tool';
          label = '🛠 ' + (m.toolName || 'инструмент');
        } else if (m.kind === 'web_search') {
          text = (m.webQuery || '') + ' ' + (m.webResults || []).map((r) => r.title + ' ' + r.snippet).join(' ');
          kind = 'web';
          label = '🔍 Web search';
        } else if (m.kind === 'subagents') {
          text = (m.subagents || []).map((s) => s.title + ' ' + (s.result || []).map((r) => r.title + ' ' + r.snippet).join(' ')).join(' ');
          kind = 'subagent';
          label = '🧩 Subagent';
        } else if (m.kind === 'plan') {
          text = (m.planTitle || '') + ' ' + (m.planSteps || []).map((s) => s.title + ' ' + (s.note || '')).join(' ');
          kind = 'plan';
          label = '📋 План';
        } else if (m.kind === 'file_edit' || m.kind === 'file_read') {
          text = (m.filePath || '') + ' ' + (m.fileDiff || m.fileReadContent || '');
          kind = 'file';
          label = '📄 ' + (m.kind === 'file_edit' ? 'Правка' : 'Чтение');
        } else if (m.kind === 'image') {
          // Images are converted to a flat ~800-token cost on the API side.
          kind = 'image';
          text = '(изображение, ≈800 токенов)';
          label = '🖼 Изображение';
        } else {
          kind = 'assistant';
          label = '🌙 Luna';
        }
      }
      const tokens = kind === 'image' ? 800 : estimateTokens(text) + 4;
      if (kind === 'image' || tokens > 4) {
        items.push({
          id: `m-${m.id}`,
          msgId: m.id,
          kind,
          role: m.role,
          content: text,
          tokens,
          chars: text.length,
          label,
        });
      }
    }
    return items;
  }
  $: realContext = (() => { void messages; return buildRealContext(); })();

  // Per-kind breakdown for the summary view.
  $: contextBreakdown = (() => {
    const groups: Record<RealContextKind, { count: number; tokens: number; label: string; color: string }> = {
      system:    { count: 0, tokens: 0, label: 'Системный',     color: 'var(--accent)' },
      user:      { count: 0, tokens: 0, label: 'Ты',            color: 'var(--info)' },
      assistant: { count: 0, tokens: 0, label: 'Luna',          color: 'var(--success)' },
      thinking:  { count: 0, tokens: 0, label: 'Рассуждения',   color: '#a882c8' },
      tool:      { count: 0, tokens: 0, label: 'Инструменты',   color: 'var(--warn)' },
      image:     { count: 0, tokens: 0, label: 'Изображения',   color: '#d8a8a8' },
      web:       { count: 0, tokens: 0, label: 'Web-поиск',      color: '#6f9ce8' },
      plan:      { count: 0, tokens: 0, label: 'Планы',          color: 'var(--code-cta)' },
      subagent:  { count: 0, tokens: 0, label: 'Sub-агенты',     color: '#a8c97a' },
      file:      { count: 0, tokens: 0, label: 'Файлы',         color: '#cfb37a' },
    };
    for (const it of realContext) {
      groups[it.kind].count += 1;
      groups[it.kind].tokens += it.tokens;
    }
    const total = realContext.reduce((s, it) => s + it.tokens, 0) || 1;
    return Object.entries(groups)
      .filter(([, v]) => v.count > 0)
      .map(([k, v]) => ({ kind: k as RealContextKind, ...v, pct: Math.round((v.tokens / total) * 100) }))
      .sort((a, b) => b.tokens - a.tokens);
  })();

  // Estimate $ cost using rough published per-1M-token rates. We only
  // need an order of magnitude; the numbers come from MiniMax's own
  // pricing page and may drift. Keep them in one place so a price
  // change is a one-line edit. M3 has a tiered price — the >512K
  // context tier is 2× the ≤512K one. We pick the tier at estimate
  // time based on the in-token total.
  const COST_PER_M_TOKENS: Record<string, { in: number; out: number; inHigh?: number; outHigh?: number; highThreshold?: number }> = {
    'minimax-M3':               { in: 0.30, out: 1.20, inHigh: 0.60, outHigh: 2.40, highThreshold: 512_000 },
    'minimax-M2.7-highspeed':   { in: 0.60, out: 2.40 },
    'minimax-M2.7':             { in: 0.30, out: 1.20 },
    'minimax-M2.5':             { in: 0.20, out: 1.00 },
  };
  function estimateCost(): { in: number; out: number; total: number } {
    const id = (selectedModel?.model || '').toLowerCase();
    const tier = COST_PER_M_TOKENS[id] ?? { in: 0.30, out: 1.20 };
    const inTokens = realContext.reduce((s, it) => s + it.tokens, 0);
    // Output tokens are unknown to us; surface the input cost only
    // plus a "last reply" estimate.
    const outTokens = realContext
      .filter((it) => it.kind === 'assistant')
      .reduce((s, it) => s + it.tokens, 0);
    // M3 (and any future tiered model) charges more once the
    // request exceeds the high-context threshold. Apply the high tier
    // on the portion above the threshold.
    let inRate = rate.in;
    let outRate = rate.out;
    if (rate.inHigh != null && rate.highThreshold != null && inTokens > rate.highThreshold) {
      const below = rate.highThreshold;
      const above = inTokens - below;
      const inCost = (below / 1_000_000) * rate.in + (above / 1_000_000) * rate.inHigh;
      const outCost = rate.inHigh ? (outTokens / 1_000_000) * rate.outHigh! : (outTokens / 1_000_000) * rate.out;
      return { in: inCost, out: outCost, total: inCost + outCost };
    }
    const inCost = (inTokens / 1_000_000) * inRate;
    const outCost = (outTokens / 1_000_000) * outRate;
    return { in: inCost, out: outCost, total: inCost + outCost };
  }
  $: costEstimate = (() => { void realContext; return estimateCost(); })();
  function toggleContextPopover() {
    contextPopover = !contextPopover;
    if (contextPopover) contextView = 'summary';
  }
  function onWindowClick(e: MouseEvent) {
    if (!contextPopover) return;
    const t = e.target as Node | null;
    if (!t) return;
    if (contextBtnEl && contextBtnEl.contains(t)) return;
    if (contextPopEl && contextPopEl.contains(t)) return;
    contextPopover = false;
  }
  function onWindowKey(e: KeyboardEvent) {
    if (!contextPopover) return;
    if (e.key === 'Escape') {
      contextPopover = false;
      e.preventDefault();
    }
  }
  async function copyRealContext() {
    const parts: string[] = [];
    for (const it of realContext) {
      const header = `[${it.role.toUpperCase()}] ${it.label} ~${it.tokens} tok · ${it.chars} зн.`;
      parts.push(`${header}\n${it.content}`);
    }
    const full = parts.join('\n\n---\n\n');
    try {
      await navigator.clipboard.writeText(full);
      contextCopyHint = '✓ Всё скопировано';
    } catch {
      contextCopyHint = '✕ Не удалось';
    }
    if (contextCopyTimer != null) clearTimeout(contextCopyTimer);
    contextCopyTimer = setTimeout(() => { contextCopyHint = ''; }, 1500);
  }

  // Per-item copy: stash the last-clicked id so the row can show a
  // brief "✓" confirmation.
  let contextItemCopied: string | null = null;
  let contextItemCopyTimer: ReturnType<typeof setTimeout> | null = null;
  async function copyContextItem(it: RealContextItem) {
    const header = `[${it.role.toUpperCase()}] ${it.label} ~${it.tokens} tok · ${it.chars} зн.`;
    const text = `${header}\n${it.content}`;
    try {
      await navigator.clipboard.writeText(text);
      contextItemCopied = it.id;
    } catch {
      contextItemCopied = null;
    }
    if (contextItemCopyTimer) clearTimeout(contextItemCopyTimer);
    contextItemCopyTimer = setTimeout(() => { contextItemCopied = null; }, 1200);
  }

  // Scroll the chat to the original message and pulse-highlight it.
  // `msgId` 0 means the system-prompt stub — there's nothing to scroll
  // to, so we just briefly flash the row itself.
  let contextItemHighlight: string | null = null;
  let contextHighlightTimer: ReturnType<typeof setTimeout> | null = null;
  function jumpToContextItem(it: RealContextItem) {
    if (it.msgId === 0) {
      contextItemHighlight = it.id;
      if (contextHighlightTimer) clearTimeout(contextHighlightTimer);
      contextHighlightTimer = setTimeout(() => { contextItemHighlight = null; }, 1200);
      return;
    }
    const el = document.querySelector(`[data-msg-id="${it.msgId}"]`);
    if (el) {
      el.scrollIntoView({ behavior: 'smooth', block: 'center' });
      contextItemHighlight = it.id;
      if (contextHighlightTimer) clearTimeout(contextHighlightTimer);
      contextHighlightTimer = setTimeout(() => { contextItemHighlight = null; }, 1500);
    }
  }
  let streamingId: number | null = null;
  let streamToken = 0;
  // Per-request cleanup slot. Every `doChat()` replaces the array and
  // disposes the previous listeners before subscribing again — that
  // fixes the leak where N chat messages would leave N × 9 listeners
  // active at once. `onDestroy` still calls them as a safety net on
  // unmount.
  let streamUnlisten: Array<() => void> = [];

  let inputFocused = false;

  // ---- agent (workspace + file context) ----
  let currentWorkspaceInfo: WorkspaceInfo | null = null;
  let recentWorkspaces: WorkspaceInfo[] = [];
  let workspaceTree: FileEntry[] = [];
  let workspaceLoading = false;
  let workspaceUnlisten: Array<() => void> = [];
  let agentUnlisten: Array<() => void> = [];
  // File-tool pill tracker: maps Rust's tool_call id to the placeholder
  // message id for `edit_file` / `create_file` tools. This MUST be shared
  // (not per-request) because the matching `ai_file_edit` event comes
  // through `attachAgentListeners` which lives at the module level —
  // it can fire after the originating `doChat` has already returned.
  const pendingFileEdits = new Map<string, number>();

  // Preview pane (right column in Code mode)
  let previewPort: number = 5173;
  let previewUrl: string = '';
  let previewPid: number | null = null;
  let previewError: string = '';
  let previewBusy = false;
  let previewRefreshKey = 0; // bump to force-reload the iframe

  // New-project modal (migrated from Workspace.svelte)
  let showNewProject = false;
  let npName = '';
  let npTemplateId = 'html-vanilla';
  let npParent = '';
  let npError = '';
  let npBusy = false;
  let npTemplates: ProjectTemplate[] = [];

  // @-mention popover
  let mentionQuery = '';
  let mentionOpen = false;
  let mentionStart = -1; // index in inputText where '@' sits
  $: mentionSuggestions = (() => {
    if (!mentionOpen) return [];
    const q = mentionQuery.toLowerCase();
    if (!q) return workspaceTree.filter((e) => e.kind === 'file').slice(0, 10);
    return workspaceTree
      .filter((e) => e.kind === 'file' && e.path.toLowerCase().includes(q))
      .slice(0, 10);
  })();

  async function refreshRecent() {
    try {
      const list = await listRecentWorkspaces();
      // Legacy data on disk may have stored paths with a `///` (or even
      // `file:///`) URL-style prefix from older builds. Strip it so the
      // sidebar shows clean Windows paths.
      recentWorkspaces = list.map((w) => ({
        ...w,
        path: w.path.replace(/^(?:file:)?\/+/, '').replace(/\//g, '\\'),
      }));
    } catch {
      recentWorkspaces = [];
    }
  }

  async function refreshTree() {
    if (!currentWorkspaceInfo) {
      workspaceTree = [];
      return;
    }
    workspaceLoading = true;
    try {
      workspaceTree = await listDir('.', 3);
    } catch (e) {
      showError('list_dir: ' + e);
      workspaceTree = [];
    } finally {
      workspaceLoading = false;
    }
  }

  async function pickAndOpenWorkspace() {
    if (busy) return;
    try {
      const path = await pickWorkspace();
      if (!path) return;
      const ws = await openWorkspace(path);
      currentWorkspaceInfo = ws;
      addRecentWorkspace(ws.path).catch(() => {});
      await refreshTree();
      await refreshRecent();
    } catch (e) {
      showError('open_workspace: ' + e);
    }
  }

  async function openRecentWorkspace(w: WorkspaceInfo) {
    try {
      const ws = await openWorkspace(w.path);
      currentWorkspaceInfo = ws;
      await refreshTree();
      await refreshRecent();
    } catch (e) {
      showError('open_workspace: ' + e);
    }
  }

  async function closeCurrentWorkspace() {
    try {
      await closeWorkspace();
      // Reset local state immediately so the file-tree pane doesn't
      // briefly keep showing the previous workspace's files. The
      // `onWorkspaceChanged` listener will also fire, but doing it here
      // makes the UI consistent even if the event arrives late.
      currentWorkspaceInfo = null;
      workspaceTree = [];
      previewUrl = '';
      previewPid = null;
      previewError = '';
    } catch (e) {
      showError('close_workspace: ' + e);
    }
  }

  function insertMention(path: string) {
    // Replace `@<partial>` (or insert at cursor) with `@<path> `.
    if (mentionStart < 0) {
      inputText = `${inputText}@${path} `;
    } else {
      const end = mentionStart + 1 + mentionQuery.length;
      inputText = inputText.slice(0, mentionStart) + `@${path} ` + inputText.slice(end);
    }
    mentionOpen = false;
    mentionQuery = '';
    mentionStart = -1;
    tick().then(() => inputEl?.focus());
  }

  function detectMention(value: string, caret: number) {
    // Find the most recent '@' before the caret that's at start-of-token.
    const before = value.slice(0, caret);
    const at = before.lastIndexOf('@');
    if (at < 0) {
      mentionOpen = false;
      mentionStart = -1;
      mentionQuery = '';
      return;
    }
    // Must not be inside a word (no alphanum directly before @)
    if (at > 0 && /\w/.test(before[at - 1])) {
      mentionOpen = false;
      return;
    }
    const fragment = before.slice(at + 1);
    // Don't open if there's a space inside the fragment (mention is closed)
    if (/\s/.test(fragment)) {
      mentionOpen = false;
      return;
    }
    mentionStart = at;
    mentionQuery = fragment;
    mentionOpen = true;
  }

  async function startPreview() {
    if (!currentWorkspaceInfo) {
      previewError = 'Сначала откройте workspace';
      return;
    }
    previewBusy = true;
    previewError = '';
    try {
      const res: DevServer = await startDevServer('.', previewPort);
      previewUrl = res.url;
      previewPid = res.pid ?? null;
      previewRefreshKey++;
    } catch (e) {
      previewError = String((e as Error)?.message || e);
      previewUrl = '';
    } finally {
      previewBusy = false;
    }
  }

  function refreshPreview() {
    if (!previewUrl) return;
    previewRefreshKey++;
  }

  async function openPreviewInWindow() {
    if (!previewUrl || !currentWorkspaceInfo) return;
    try {
      await openPreviewWindow(previewUrl, `Preview — ${currentWorkspaceInfo.name}`);
    } catch (e) {
      showError('open_preview_window: ' + e);
    }
  }

  function openPreviewInBrowser() {
    if (!previewUrl) return;
    openUrl(previewUrl).catch((e) => showError('open_url: ' + e));
  }

  async function loadTemplates() {
    if (npTemplates.length > 0) return;
    try {
      npTemplates = await getProjectTemplates();
    } catch (e) {
      showError('get_project_templates: ' + e);
    }
  }

  function openNewProjectDialog() {
    npError = '';
    npName = '';
    npTemplateId = 'html-vanilla';
    npParent = currentWorkspaceInfo?.path ?? '';
    if (!npParent) {
      // best-effort default for Windows
      npParent = 'C:\\Users\\Public\\Documents';
    }
    showNewProject = true;
    loadTemplates();
  }

  async function submitNewProject() {
    if (npBusy) return;
    npError = '';
    if (!npName.trim()) {
      npError = 'Введите имя проекта';
      return;
    }
    if (!npParent.trim()) {
      npError = 'Укажите папку';
      return;
    }
    npBusy = true;
    try {
      const info = await createProject(npName.trim(), npTemplateId, npParent.trim());
      showNewProject = false;
      currentWorkspaceInfo = info;
      await refreshTree();
      await refreshRecent();
    } catch (e) {
      npError = String((e as Error)?.message || e);
    } finally {
      npBusy = false;
    }
  }

  async function rejectFileEdit(editId: string, msgId: number) {
    try {
      await revertFileEdit(editId);
      messages = messages.map((m) => (m.id === msgId ? { ...m, fileEditState: 'rejected' } : m));
    } catch (e) {
      showError('revert: ' + e);
    }
  }

  function acceptFileEdit(msgId: number) {
    messages = messages.map((m) => (m.id === msgId ? { ...m, fileEditState: 'accepted' } : m));
  }

  function toggleFileRead(msgId: number) {
    messages = messages.map((m) =>
      m.id === msgId ? { ...m, fileReadOpen: !m.fileReadOpen } : m
    );
  }

  // Wire Rust workspace/agent events. We attach the listeners on mount
  // and tear them down in onDestroy.
  function attachAgentListeners() {
    // workspace_changed: refresh state when the active workspace rotates.
    workspaceUnlisten.push(
      onWorkspaceChanged((p) => {
        if (!p.path) {
          currentWorkspaceInfo = null;
          workspaceTree = [];
        } else {
          currentWorkspaceInfo = { path: p.path, name: p.name ?? '', total_files: 0 };
          refreshTree();
        }
      })
    );
    // ai_file_edit: the agent successfully wrote a file. We either update
    // the placeholder we already created (matching via pendingFileEdits) or
    // insert a brand-new diff card. emit `ai_edit_reverted` flips the state.
    agentUnlisten.push(
      onAiFileEdit((p) => {
        const msgId = pendingFileEdits.get(p.id);
        if (msgId != null) {
          messages = messages.map((m) =>
            m.id === msgId
              ? {
                  ...m,
                  kind: 'file_edit',
                  filePath: p.path,
                  fileDiff: p.diff,
                  fileEditId: p.id,
                  fileEditState: 'accepted',
                }
              : m
          );
        } else {
          // No prior placeholder — create one (defensive; should not normally happen).
          const id = nextId++;
          messages = [
            ...messages,
            {
              id,
              role: 'assistant',
              html: '',
              kind: 'file_edit',
              filePath: p.path,
              fileDiff: p.diff,
              fileEditId: p.id,
              fileEditState: 'accepted',
              createdAt: Date.now(),
            },
          ];
        }
        scrollToBottom();
        // File changed on disk — the tree may be stale; refresh.
        refreshTree();
      })
    );
    agentUnlisten.push(
      onAiEditReverted((p) => {
        // Flip any matching card to "rejected" so the user sees the effect.
        messages = messages.map((m) =>
          m.fileEditId === p.id ? { ...m, fileEditState: 'rejected' } : m
        );
        refreshTree();
      })
    );
    agentUnlisten.push(
      onAiFileRead((p) => {
        const msgId = nextId++;
        messages = [
          ...messages,
          {
            id: msgId,
            role: 'assistant',
            html: '',
            kind: 'file_read',
            filePath: p.path,
            fileReadBytes: p.bytes,
            fileReadLines: p.lines,
            fileReadContent: p.content,
            fileReadOpen: false,
            createdAt: Date.now(),
          },
        ];
        scrollToBottom();
      })
    );

    // ---- Video Mode bridge ----
    // `chat-inject` is fired by the `chat_inject_user_message` Tauri
    // command (currently driven by the VideoMode's auto-invoke
    // hook). We append the text as a user message and run the normal
    // send() flow, so the model reacts in the background even if the
    // user is on a different tab.
    agentUnlisten.push(
      onChatInject((p) => {
        if (!p.text || !p.text.trim()) return;
        // Telegram-injected messages get a small badge so the user
        // can tell where the trigger came from.
        if (p.source === 'telegram') {
          const noteId = nextId++;
          messages = [
            ...messages,
            {
              id: noteId,
              role: 'system',
              html: '📨 from Telegram',
              createdAt: Date.now(),
            },
          ];
        }
        injectUserMessage(p.text);
      })
    );

    // ---- ask_user: human-in-the-loop question from the agent ----
    // The agent calls `ask_user(question, options?)` to clarify before
    // doing something expensive or irreversible. We render an inline
    // card with the question + clickable options; the user's reply
    // (button click or typed answer) becomes the next user message, so
    // the model picks up where it left off on the next turn.
    agentUnlisten.push(
      onAiAskUser((p) => {
        if (!p.question) return;
        const askId = nextId++;
        // Mark any in-flight ask_user pill as resolved (we just
        // received the actual question event).
        messages = messages.map((mm) =>
          mm.kind === 'tool_use' && mm.toolName === 'ask_user' && mm.toolStatus === 'pending'
            ? { ...mm, toolStatus: 'ok' }
            : mm,
        );
        pendingAskUser = {
          id: askId,
          callId: p.id,
          question: p.question,
          options: p.options || [],
        };
        messages = [
          ...messages,
          {
            id: askId,
            role: 'assistant',
            html: '',
            kind: 'ask_user',
            askQuestion: p.question,
            askOptions: p.options || [],
            askCallId: p.id,
            askAnswer: '',
            createdAt: Date.now(),
          },
        ];
        scrollToBottom();
      })
    );
    // `ai_video_frame` lets the chat show a "viewed this frame" card
    // whenever the model uses video_observe_now / video_get_latest_frame.
    agentUnlisten.push(
      onAiVideoFrame((p) => {
        const msgId = nextId++;
        messages = [
          ...messages,
          {
            id: msgId,
            role: 'assistant',
            html: '',
            kind: 'video_frame',
            videoFrameUrl: p.data_url,
            videoFrameKind: p.kind,
            videoFrameMeta: {
              monitor_id: p.monitor_id,
              width: p.width,
              height: p.height,
              bytes: p.bytes,
              seq: p.seq,
              t_ms: p.t_ms,
            },
            createdAt: Date.now(),
          },
        ];
        scrollToBottom();
      })
    );
    // Drain any single-slot pending auto-invoke that fired before this
    // listener was attached.
    takePendingVideoAutoInvoke()
      .then((p) => {
        if (p && p.hint_text) {
          const text = [
            '[Video Mode] На экране замечено:',
            `"${p.hint_text}"`,
            `Кадр #${p.seq}, монитор ${p.monitor_id} (${p.width}×${p.height}).`,
            p.goal ? `Цель наблюдения: ${p.goal}` : 'Цель не задана.',
            'Прокомментируй и предложи, что делать.',
          ].join(' ');
          injectUserMessage(text);
        }
      })
      .catch(() => { /* non-fatal */ });
  }

  /**
   * Public method: append a synthetic user message and run the normal
   * `send()` flow. Used by the VideoMode auto-invoke bridge.
   * Exposed on `window` for cross-tab use (see `injectUserMessageGlobal`).
   */
  function injectUserMessage(text: string) {
    if (busy) {
      // The chat is mid-turn — we still append the message so the
      // user sees it, but we don't trigger `send()` to avoid
      // interleaving turns. The user can press Enter to re-run.
    }
    appendMessage('user', text);
    tick().then(() => {
      scrollToBottom();
      if (!busy) send();
    });
  }

  // research (was: news)
  type NewsCard = {
    id: number;
    interest: string;
    title: string;
    snippet: string;
    url: string;
    source: string;
    image: string;
    isGlobal?: boolean; // true for fallback "world news" topics
  };
  let researchResults: NewsCard[] = [];
  let researchLoading = false;
  let researchError = '';
  let userInterests: string[] = [];

  // ---- Fusion Research v2: multi-source, modern UI ----
  type ResearchSource = 'web' | 'workspace' | 'news';
  type ToolProgress = {
    source: ResearchSource;
    status: 'pending' | 'ok' | 'error';
    count: number;
    error?: string;
  };
  type FusedItem = {
    id: number;
    source: ResearchSource;
    sourceLabel: string;
    title: string;
    snippet: string;
    url: string;
    line?: number;
    path?: string;
    fetchedAt?: number;
  };
  let researchQuery: string = '';
  let researchActiveSource: 'all' | ResearchSource = 'all';
  let researchAllResults: FusedItem[] = [];
  let researchProgress: ToolProgress[] = [
    { source: 'web', status: 'pending', count: 0 },
    { source: 'workspace', status: 'pending', count: 0 },
    { source: 'news', status: 'pending', count: 0 },
  ];
  let researchCacheStats: { fresh: number; stale: number; total: number; path: string } | null = null;
  let readMore: { url: string; title: string; text: string; loading: boolean } | null = null;
  let researchDrawer: { loading: boolean; url: string; html: string } | null = null;

  function fuseId(): number { return Math.floor(Math.random() * 1e9) + 1; }

  async function runResearch(query: string) {
    const q = query.trim();
    if (!q) return;
    researchQuery = q;
    researchAllResults = [];
    researchProgress = [
      { source: 'web', status: 'pending', count: 0 },
      { source: 'workspace', status: 'pending', count: 0 },
      { source: 'news', status: 'pending', count: 0 },
    ];
    researchLoading = true;
    researchError = '';

    const updateProgress = (src: ResearchSource, p: Partial<ToolProgress>) => {
      researchProgress = researchProgress.map(x => x.source === src ? { ...x, ...p } : x);
    };

    const tasks = [
      // web (Google + DDG fallback + кэш 30 мин)
      (async () => {
        try {
          const items: any[] = await webSearch(q, 5);
          updateProgress('web', { status: 'ok', count: items.length });
          return items.map((it) => ({
            id: fuseId(),
            source: 'web' as const,
            sourceLabel: it.source || 'Web',
            title: it.title || '(без заголовка)',
            snippet: (it.snippet || '').trim(),
            url: it.url || '',
            fetchedAt: it.fetched_at,
          })).filter((x) => x.url);
        } catch (e) {
          updateProgress('web', { status: 'error', error: String(e), count: 0 });
          return [];
        }
      })(),
      // workspace (если открыт)
      (async () => {
        try {
          const ws = await currentWorkspace();
          if (!ws) {
            updateProgress('workspace', { status: 'pending', count: 0 });
            return [];
          }
          const items: any[] = await searchWorkspace(q, { max_results: 5 });
          updateProgress('workspace', { status: 'ok', count: items.length });
          return items.map((it) => ({
            id: fuseId(),
            source: 'workspace' as const,
            sourceLabel: `Workspace · ${it.path?.split('/').pop() || 'file'}`,
            title: it.path ? `${it.path}:${it.line}` : '(без имени)',
            snippet: (it.snippet || '').trim(),
            url: '',
            line: it.line,
            path: it.path,
          })).filter((x) => x.path);
        } catch (e) {
          updateProgress('workspace', { status: 'error', error: String(e), count: 0 });
          return [];
        }
      })(),
      // news (RSS)
      (async () => {
        try {
          const items: any[] = await fetchNews(null, 4);
          updateProgress('news', { status: 'ok', count: items.length });
          return items.map((it) => ({
            id: fuseId(),
            source: 'news' as const,
            sourceLabel: it.source || 'News',
            title: it.title || '(без заголовка)',
            snippet: (it.snippet || '').trim(),
            url: it.url || '',
            fetchedAt: it.fetched_at,
          })).filter((x) => x.url);
        } catch (e) {
          updateProgress('news', { status: 'error', error: String(e), count: 0 });
          return [];
        }
      })(),
    ];

    // Стримим результаты по мере поступления.
    await Promise.allSettled(tasks.map(async (t) => {
      const result = await t;
      if (result.length) {
        // Дедуп по URL/path.
        setTimeout(() => {
          const seen = new Set(researchAllResults.map((x) => x.url || x.path));
          const fresh = result.filter((x) => !(seen.has(x.url || x.path)));
          researchAllResults = [...researchAllResults, ...fresh];
        }, 0);
      }
    }));

    researchLoading = false;
    try {
      researchCacheStats = (await webSearchCacheStats()) as any;
    } catch {
      researchCacheStats = null;
    }
  }

  async function clearResearchCache() {
    try {
      await clearWebSearchCache();
      if (researchCacheStats) {
        researchCacheStats = { ...researchCacheStats, fresh: 0, stale: 0, total: 0 };
      }
    } catch (e) {
      researchError = 'clear cache: ' + e;
    }
  }

  async function openFused(it: FusedItem) {
    try {
      if (it.source === 'workspace' && it.path) {
        // emit event для Workspace.svelte — он сам откроет файл.
        await invoke('open_workspace' as any, { path: '' } as any).catch(() => {});
      } else if (it.url) {
        await openUrl(it.url);
      }
    } catch (e) {
      researchError = `open: ${e}`;
    }
  }

  async function readMoreUrl(it: FusedItem) {
    if (!it.url || !it.url.startsWith('http')) return;
    readMore = { url: it.url, title: it.title, text: '', loading: true };
    try {
      const page = await fetchUrl(it.url);
      readMore = {
        url: page.final_url || it.url,
        title: page.title || it.title,
        text: (page.text || '').slice(0, 4000),
        loading: false,
      };
    } catch (e) {
      readMore = { url: it.url, title: it.title, text: '⚠ ' + String(e), loading: false };
    }
  }

  function sourceLabel(src: ResearchSource): string {
    return src === 'web' ? 'Web' : src === 'workspace' ? 'Workspace' : 'News';
  }
  function sourceIcon(src: ResearchSource): string {
    return src === 'web' ? '🌐' : src === 'workspace' ? '📁' : '📡';
  }
  function sourceColor(src: ResearchSource): string {
    return src === 'web' ? '#6f9ce8' : src === 'workspace' ? '#c9a0a0' : '#a8c97a';
  }

  $: visibleResults = researchActiveSource === 'all'
    ? researchAllResults
    : researchAllResults.filter((x) => x.source === researchActiveSource);
  $: countBySource = {
    web: researchAllResults.filter((x) => x.source === 'web').length,
    workspace: researchAllResults.filter((x) => x.source === 'workspace').length,
    news: researchAllResults.filter((x) => x.source === 'news').length,
  };
  $: anyProgress = researchProgress.some((p) => p.status === 'pending');
  // Topical queries used when the user has no interests yet — covers
  // general world news so Fusion Research is never empty.
  const GLOBAL_TOPICS = ['world news', 'top stories', 'breaking news'];
  const INTERESTS_STORAGE_KEY = 'luna.user.interests';

  // image
  type ImageCard = {
    id: number;
    dataUrl: string;
    prompt: string;
    aspect: ImageAspect;
  };
  let imagePrompt = '';
  let imageAspect: ImageAspect = '1:1';
  let imageBusy = false;
  let imageError = '';
  let imageResults: ImageCard[] = [];
  let imageNextId = 1;
  const IMAGE_HISTORY_KEY = 'luna.image.history';
  let imageLightbox: ImageCard | null = null;

  try {
    const raw = localStorage.getItem(IMAGE_HISTORY_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Array<{ dataUrl: string; prompt: string; aspect: ImageAspect }>;
      if (Array.isArray(parsed)) {
        imageResults = parsed.slice(0, 24).map((it) => ({
          id: imageNextId++,
          dataUrl: it.dataUrl,
          prompt: it.prompt,
          aspect: it.aspect,
        }));
      }
    }
  } catch { /* ignore */ }

  function persistImages() {
    try {
      const slim = imageResults.slice(0, 24).map((it) => ({
        dataUrl: it.dataUrl,
        prompt: it.prompt,
        aspect: it.aspect,
      }));
      localStorage.setItem(IMAGE_HISTORY_KEY, JSON.stringify(slim));
    } catch { /* quota */ }
  }

  async function generateImage() {
    if (imageBusy) return;
    const prompt = imagePrompt.trim();
    if (!prompt) { imageError = 'Введите описание картинки.'; return; }
    if (!hasMinimax) { imageError = 'MiniMax-ключ не задан. Откройте ⚙ Settings.'; return; }
    imageBusy = true;
    imageError = '';
    try {
      const list = await generateImageMinimax({ prompt, n: 1, aspect_ratio: imageAspect });
      for (const b64 of list) {
        imageResults = [
          { id: imageNextId++, dataUrl: `data:image/png;base64,${b64}`, prompt, aspect: imageAspect },
          ...imageResults,
        ];
      }
      persistImages();
    } catch (e) {
      imageError = String((e as Error)?.message || e);
    } finally {
      imageBusy = false;
    }
  }

  function deleteImage(id: number) {
    imageResults = imageResults.filter((r) => r.id !== id);
    persistImages();
    if (imageLightbox?.id === id) imageLightbox = null;
  }

  function clearAllMedia() {
    if (!confirm(`Удалить все ${imageResults.length} картинок?`)) return;
    imageResults = [];
    imageLightbox = null;
    persistImages();
  }

  function downloadImage(card: ImageCard) {
    const a = document.createElement('a');
    a.href = card.dataUrl;
    const safe = card.prompt.replace(/[^a-z0-9а-яё\s-]+/gi, '').trim().slice(0, 50) || 'image';
    a.download = `luna-${Date.now()}-${safe}.png`;
    document.body.appendChild(a);
    a.click();
    a.remove();
  }

  // Compact one-line summary of a tool-call's JSON args.
  function summarizeToolArgs(json: string): string {
    try {
      const obj = JSON.parse(json);
      if (obj && typeof obj === 'object') {
        if (obj.prompt) {
          const s = String(obj.prompt);
          const trimmed = s.length > 56 ? s.slice(0, 55) + '…' : s;
          const aspect = obj.aspect_ratio ? ` · ${obj.aspect_ratio}` : '';
          return `“${trimmed}”${aspect}`;
        }
        const firstStr = Object.values(obj).find((v) => typeof v === 'string');
        if (firstStr) {
          const s = String(firstStr);
          return s.length > 60 ? s.slice(0, 59) + '…' : s;
        }
      }
    } catch { /* ignore */ }
    return json.length > 60 ? json.slice(0, 59) + '…' : json;
  }

  // Map tool names to user-facing icons. Falls back to a generic
  // wrench when we don't recognise the call. Pairing an icon with
  // the spinner state makes "what is the agent doing right now?"
  // answerable at a glance — no need to expand the args panel.
  function toolIcon(name: string | undefined): string {
    switch (name) {
      case 'read_file':          return '📖';
      case 'list_dir':           return '📂';
      case 'search_workspace':   return '🔎';
      case 'create_file':        return '📄';
      case 'edit_file':          return '✏️';
      case 'generate_image':     return '🎨';
      case 'parallel_research':  return '🧠';
      case 'parallel_generate_images': return '🖼️';
      case 'web_search':         return '🌐';
      case 'fetch_url':          return '🔗';
      case 'video_observe_now':  return '📸';
      case 'video_get_latest_frame': return '🎞️';
      case 'video_start_capture':return '⏺';
      case 'video_stop_capture': return '⏹';
      case 'telegram_status':    return '🤖';
      case 'telegram_set_token': return '🔑';
      case 'telegram_start':     return '▶';
      case 'telegram_stop':      return '⏹';
      case 'update_user_interests': return '⭐';
      case 'three_d_apply_ops':  return '🧊';
      case 'remember':           return '💭';
      default:                   return '🛠';
    }
  }

  function openLightboxForMsg(m: { id: number; imageDataUrl?: string; imagePrompt?: string; imageAspect?: ImageAspect }) {
    if (!m.imageDataUrl) return;
    imageLightbox = {
      id: m.id,
      dataUrl: m.imageDataUrl,
      prompt: m.imagePrompt || '',
      aspect: (m.imageAspect || '1:1') as ImageAspect,
    };
  }

  function openLightboxForSub(s: { id: number; dataUrl?: string; title: string; aspect?: string }) {
    if (!s.dataUrl) return;
    imageLightbox = {
      id: s.id,
      dataUrl: s.dataUrl,
      prompt: s.title || '',
      aspect: (s.aspect || '1:1') as ImageAspect,
    };
  }

  function downloadMsgImage(m: { imageDataUrl?: string; imagePrompt?: string; imageAspect?: ImageAspect }) {
    if (!m.imageDataUrl) return;
    downloadImage({
      id: 0,
      dataUrl: m.imageDataUrl,
      prompt: m.imagePrompt || '',
      aspect: (m.imageAspect || '1:1') as ImageAspect,
    });
  }

  const ASPECTS: Array<{ id: ImageAspect; label: string; w: number; h: number }> = [
    { id: '1:1',  label: '1:1',  w: 1, h: 1 },
    { id: '16:9', label: '16:9', w: 16, h: 9 },
    { id: '9:16', label: '9:16', w: 9, h: 16 },
    { id: '4:3',  label: '4:3',  w: 4, h: 3 },
    { id: '3:4',  label: '3:4',  w: 3, h: 4 },
    { id: '21:9', label: '21:9', w: 21, h: 9 },
  ];

  function readInterests(): string[] {
    try {
      const raw = localStorage.getItem(INTERESTS_STORAGE_KEY) ?? '';
      return raw.split(/[,\n]/).map((s) => s.trim()).filter((s) => s.length > 0);
    } catch { return []; }
  }

  function persistInterests() {
    try {
      localStorage.setItem(INTERESTS_STORAGE_KEY, userInterests.join(', '));
    } catch { /* quota */ }
  }

  function interestKey(raw: string): string {
    return raw.toLowerCase().replace(/[^\p{L}\p{N}\s]+/gu, ' ').replace(/\s+/g, ' ').trim();
  }

  function mergeInterests(incoming: string[]): boolean {
    const existing = new Map<string, string>();
    for (const it of userInterests) existing.set(interestKey(it), it);
    let changed = false;
    for (const raw of incoming) {
      const t = (raw ?? '').trim();
      if (!t) continue;
      const k = interestKey(t);
      if (!k) continue;
      if (!existing.has(k)) { existing.set(k, t); changed = true; }
    }
    if (!changed) return false;
    userInterests = Array.from(existing.values()).slice(0, 64);
    persistInterests();
    // Mirror the new list to Rust so `get_user_interests` can answer
    // without round-tripping to the frontend.
    setUserInterests(userInterests).catch(() => { /* non-fatal */ });
    return true;
  }

  function removeInterest(text: string) {
    userInterests = userInterests.filter((it) => it !== text);
    persistInterests();
    setUserInterests(userInterests).catch(() => { /* non-fatal */ });
  }

  function clearAllInterests() {
    userInterests = [];
    persistInterests();
    setUserInterests(userInterests).catch(() => { /* non-fatal */ });
  }

  // Per-source favicon background — deterministic pastel gradient from the
  // URL so each source feels visually distinct (Perplexity-style) without
  // shipping a favicon service. Keeps the same first-letter avatar idea
  // but tints it from a per-source hue.
  function faviconBg(url: string): string {
    let seed = 0;
    for (let i = 0; i < url.length; i++) seed = (seed * 31 + url.charCodeAt(i)) | 0;
    const h = ((seed % 360) + 360) % 360;
    return `linear-gradient(135deg, hsl(${h} 55% 48%), hsl(${(h + 38) % 360} 60% 38%))`;
  }
  function sourceHost(url: string): string {
    try { return new URL(url).host.replace(/^www\./, ''); } catch { return ''; }
  }
  // Add from the sidebar's input. Triggers a fresh research fetch so the
  // user immediately sees articles for the new interest.
  let sidebarNewInterest = '';
  function addInterestFromSidebar() {
    const v = sidebarNewInterest.trim();
    if (!v) return;
    sidebarNewInterest = '';
    if (mergeInterests([v])) {
      fetchResearch();
    }
  }

  function fallbackImage(url: string): string {
    try { return new URL(url).host.charAt(0).toUpperCase() || 'N'; } catch { return 'N'; }
  }

  function imgError(e: Event) {
    const img = e.currentTarget as HTMLImageElement | null;
    if (img) img.style.display = 'none';
  }

  async function fetchResearch() {
    if (researchLoading) return;
    researchLoading = true;
    researchError = '';
    // Fallback: if the user has no interests yet, show general world news
    // so the tab is never empty / confusing. As soon as the agent learns
    // (or the user adds) a single interest, this stops kicking in.
    const isFallback = userInterests.length === 0;
    const topics = isFallback ? GLOBAL_TOPICS : userInterests;
    try {
      const all: NewsCard[] = [];
      const seen = new Set<string>();
      let nextId = 1;
      const settled = await Promise.allSettled(
        topics.map((q) => webSearch(q, 4)),
      );
      for (let i = 0; i < settled.length; i++) {
        const interest = topics[i];
        const r = settled[i];
        if (r.status !== 'fulfilled' || !r.value) continue;
        // webSearch возвращает массив напрямую (не { results: [] }).
        const list: Array<{ title?: string; snippet?: string; url?: string; source?: string }> =
          Array.isArray(r.value) ? r.value : (r.value as any).results || [];
        for (const item of list) {
          const url = item.url || '';
          if (!url || seen.has(url)) continue;
          seen.add(url);
          all.push({
            id: nextId++,
            interest,
            title: item.title || '(без заголовка)',
            snippet: (item.snippet || '').trim(),
            url,
            source: item.source || '',
            // webSearch не возвращает картинку — пусть будет пусто (UI уже это поддерживает).
            image: '',
            isGlobal: isFallback,
          });
        }
      }
      researchResults = all;
    } catch (e) {
      researchError = String(e);
      researchResults = [];
    } finally {
      researchLoading = false;
    }
  }

  $: selectedModel = MODELS.find((m) => m.id === selectedModelId) ?? MODELS[0];

  // Markdown rendering moved to src/lib/markdown.ts — imported above as
  // `renderMarkdown` (alias for `safeRenderMarkdown`). The old hand-rolled
  // escHtml/renderInline/renderMarkdown functions used to live here; they
  // are now superseded by a token-based parser with proper <p> paragraphs,
  // GFM tables, task lists, fenced code with language label + copy button,
  // and `==mark==` highlight. See `src/lib/markdown.ts` for details.

  // Startup "what now" message. Adapts to the current `mode` so the user
  // doesn't see "press Ctrl+Space" while looking at Code / Research / Media.
  function introForMode(m: Mode, keyPresent: boolean): string {
    if (!keyPresent) {
      return 'Откройте вкладку ⚙ Settings и введите MiniMax-ключ, чтобы начать.';
    }
    switch (m) {
      case 'code':
        return 'Code mode · откройте воркспейс слева и опишите задачу — агент прочитает и отредактирует файлы.';
      case 'research':
        return 'Research · лента обновится автоматически по вашим интересам. Можно добавить или убрать темы в сайдбаре.';
      case 'media':
        return 'Media · попросите агента нарисовать что-нибудь в чате — картинки появятся здесь.';
      case 'plan':
        return 'Plan mode · введите название и шаги плана (каждая строка — шаг). Сохраните в сайдбар или сразу запустите — агент пройдёт шаги через `create_plan` tool.';
      default:
        return `MiniMax · ${selectedModel.label}. Введите сообщение или нажмите 🎙 (Ctrl+Space).`;
    }
  }

  function appendMessage(role: string, text: string, opts: { streaming?: boolean; modelTag?: string } = {}) {
    const id = nextId++;
    const msg = {
      id, role,
      html: renderMarkdown(text), raw: text,
      streaming: !!opts.streaming,
      modelTag: opts.modelTag,
      createdAt: Date.now(),
      typedText: '',
      pendingText: '',
      toolCount: 0,
    };
    messages = [...messages, msg];
    scrollToBottom();
    return id;
  }

  function patchMessage(id: number, patch: Partial<{ html: string; raw: string; streaming: boolean; thinking?: string; thinkingOpen?: boolean }>) {
    messages = messages.map((m) => (m.id === id ? { ...m, ...patch } : m));
    scrollToBottom();
  }

  function toggleThinking(id: number) {
    messages = messages.map((m) => {
      if (m.id !== id) return m;
      const currentlyOpen = m.thinkingOpen === true || (m.thinkingOpen == null && streamingId === m.id);
      return { ...m, thinkingOpen: !currentlyOpen };
    });
  }

  function scrollToBottom(smooth = false) {
    tick().then(() => {
      const el = document.getElementById('chat-scroll');
      if (el) {
        if (smooth) el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
        else el.scrollTop = el.scrollHeight;
      }
    });
  }

  // "Scroll to bottom" button — visible when the user is not at the bottom
  // and new content arrives (or they scrolled up manually). Click jumps to
  // the latest message, matching Perplexity / ChatGPT behavior.
  let stickToBottom = true;
  function onScroll(e: Event) {
    const el = e.currentTarget as HTMLElement;
    const distance = el.scrollHeight - el.clientHeight - el.scrollTop;
    stickToBottom = distance < 60;
  }

  function jumpToBottom() {
    scrollToBottom(true);
    stickToBottom = true;
  }

  // When a new chunk/result lands while the user is following the conversation,
  // auto-scroll. If they've scrolled up to read, leave them alone.
  $: if (stickToBottom && typeof document !== 'undefined') scrollToBottom();

  function showError(msg: string) {
    errorBanner = msg;
    setTimeout(() => { if (errorBanner === msg) errorBanner = ''; }, 8000);
  }

  function clearChat() {
    messages = [];
    history = [];
    researchResults = [];
    appendMessage('system', 'Чат очищен.');
  }

  function setMode(m: Mode) {
    if (busy) return;
    mode = m;
    if (m === 'research' && researchResults.length === 0 && !researchLoading) {
      fetchResearch();
    }
    if (m === 'plan' && planTitle === '' && planStepsText === '') {
      // Pre-fill the steps textarea with three empty bullets so the
      // user can tab through and start typing immediately. We only
      // do this the first time the user opens plan mode; if they
      // re-open it after editing, the existing values are kept.
      planStepsText = '1. \n2. \n3. ';
      tick().then(() => planTitleInputEl?.focus());
    }
    // When the user switches tabs, rebuild the system message in place so
    // it matches the new mode. We only do this if the chat is otherwise
    // empty (otherwise we'd be rewriting real history).
    rebuildIntroIfEmpty();
    inputEl?.focus();
  }

  // ---- plan mode helpers ----

  /** Split the multi-line steps text into clean step titles. We strip
   *  leading numbering ("1. ", "12)") so the user can type naturally
   *  without having to match the agent's expected format. */
  function parseSteps(text: string): string[] {
    return text
      .split(/\r?\n/)
      .map((s) => s.replace(/^\s*\d+[.)]\s*/, '').trim())
      .filter((s) => s.length > 0);
  }

  $: parsedSteps = parseSteps(planStepsText);
  $: canSavePlan = planTitle.trim() !== '' && parsedSteps.length > 0 && !busy;
  $: canRunPlan = planTitle.trim() !== '' && parsedSteps.length > 0;

  /** Enter on a non-empty step creates a fresh "N. " line below the
   *  caret and moves the caret there. Shift+Enter is left alone so
   *  the user can put a literal newline inside a step. */
  function onPlanStepsKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      const ta = e.target as HTMLTextAreaElement;
      const pos = ta.selectionStart ?? planStepsText.length;
      const before = planStepsText.slice(0, pos);
      const after = planStepsText.slice(pos);
      // Continue the existing numbering if the line we were on already
      // starts with "N. ". Otherwise start at 1.
      const m = before.match(/(\n|^)(\s*)(\d+)([.)]\s*)$/);
      const nextNum = m ? parseInt(m[3], 10) + 1 : parsedSteps.length + 1;
      const sep = before.endsWith('\n') || before === '' ? '' : '\n';
      const insertion = `${sep}${nextNum}. `;
      const newText = before + insertion + after;
      planStepsText = newText;
      tick().then(() => {
        if (planStepsInputEl) {
          const caret = (before + insertion).length;
          planStepsInputEl.selectionStart = caret;
          planStepsInputEl.selectionEnd = caret;
          planStepsInputEl.focus();
        }
      });
    }
  }

  /** Save the current draft into the local plan store without sending
   *  anything to the model. The plan lands at the top of the sidebar
   *  and is editable. */
  function savePlan() {
    if (!canSavePlan) return;
    createPlan(planTitle, parsedSteps.map((title) => ({ id: '', title, status: 'pending' })));
    planTitle = '';
    planStepsText = '1. \n2. \n3. ';
    tick().then(() => planTitleInputEl?.focus());
  }

  /** Save the draft, then send a doChat() with the run-prompt.
   *  pendingLinkPlanId is set so the upcoming ai_plan_created can
   *  wire the chat message back to the just-created store plan. */
  async function runPlan() {
    if (!canRunPlan || busy) return;
    const realPlan = createPlan(
      planTitle,
      parsedSteps.map((title) => ({ id: '', title, status: 'pending' })),
    );
    pendingLinkPlanId = realPlan.id;
    const prompt = buildPlanRunPrompt(realPlan);
    planTitle = '';
    planStepsText = '1. \n2. \n3. ';
    await runPlanFromSidebarInternal(realPlan, prompt);
    tick().then(() => planTitleInputEl?.focus());
  }

  /** Called by the parent (App.svelte) when the user clicks "Run"
   *  on a plan in the PlansSidebar. Reuses the same flow as the
   *  composer button: set pendingLinkPlanId, append the user message
   *  visibly, and start a chat stream. Exposed via `<svelte:component>`
   *  bind:this so App.svelte can drive Chat from outside.
   *
   *  If the plan is already linked to a chat message we treat this
   *  as a "continue" run (skip the steps the agent already finished). */
  export async function runPlanFromSidebar(plan: Plan): Promise<void> {
    if (busy) return;
    if (plan.chatLinked) {
      // The plan already ran once — the user wants to resume. We
      // build a continue prompt and dispatch the same flow.
      pendingLinkPlanId = plan.id;
      const prompt = buildPlanContinuePrompt(plan);
      await runPlanFromSidebarInternal(plan, prompt);
    } else {
      // First run from a sidebar plan. mark chatLinked will flip
      // once the agent's ai_plan_created comes back. For now we
      // remember the planId so the linking in the event handler
      // works as expected.
      pendingLinkPlanId = plan.id;
      const prompt = buildPlanRunPrompt(plan);
      await runPlanFromSidebarInternal(plan, prompt);
    }
  }

  /** Shared tail of runPlan / runPlanFromSidebar. We deliberately
   *  re-implement the few lines from `send()` here because we need
   *  to feed a custom prompt (not `inputText`) and we also want to
   *  re-use the existing busy-cancellation semantics. */
  async function runPlanFromSidebarInternal(plan: Plan, prompt: string): Promise<void> {
    if (busy) return;
    if (!hasMinimax) {
      showError('MiniMax-ключ не задан. Открой вкладку ⚙ Settings и введи его.');
      return;
    }
    errorBanner = '';
    appendMessage('user', prompt);
    if (busy) {
      // Race: another send snuck in between our `if (busy)` check
      // and now. Bump the token so any prior stream stops updating
      // the UI, then proceed.
      streamToken++;
      streamingId = null;
    }
    busy = true;
    try {
      await doChat(prompt);
    } catch (e) {
      const m = (e && (e as Error).message) || String(e);
      showError(m);
      if (mode === 'chat') history.pop();
    } finally {
      busy = false;
      inputEl?.focus();
    }
  }

  // The intro line is the only "system" message that should follow the
  // current mode. Drop it (if present) and append a fresh one. Called on
  // mount, on `setMode`, and on key status change.
  function rebuildIntroIfEmpty() {
    if (messages.length !== 1) return;
    const m = messages[0];
    if (m.role !== 'system') return;
    messages = [{
      ...m,
      html: renderMarkdown(introForMode(mode, hasMinimax)),
      raw: introForMode(mode, hasMinimax),
    }];
  }

  // When the key status changes (e.g. user pastes a key in Settings) the
  // intro message needs to flip between the "open Settings" hint and the
  // mode-specific greeting.
  $: { void hasMinimax; void mode; rebuildIntroIfEmpty(); }

  function toggleMultitask() {
    multitask = !multitask;
    try { localStorage.setItem(MULTITASK_STORAGE_KEY, multitask ? '1' : '0'); } catch { /* ignore */ }
    inputEl?.focus();
  }

  function onModelChange() {
    try { localStorage.setItem(MODEL_STORAGE_KEY, selectedModelId); } catch { /* ignore */ }
  }

  function onMessagesClick(e: MouseEvent) {
    const t = e.target as HTMLElement | null;
    if (!t) return;

    // 1) Copy a code block (button is rendered inside the markdown HTML).
    const copyBtn = t.closest('.codeblock-copy') as HTMLButtonElement | null;
    if (copyBtn) {
      e.preventDefault();
      e.stopPropagation();
      const code = copyBtn.closest('.codeblock')?.querySelector('code');
      const text = code?.textContent ?? '';
      copyToClipboard(text, copyBtn, '⧉ Копировать', '✓ Скопировано');
      return;
    }

    // 2) Copy an entire message (button is rendered in msg-head).
    const msgCopy = t.closest('.msg-copy') as HTMLButtonElement | null;
    if (msgCopy) {
      e.preventDefault();
      e.stopPropagation();
      const id = +(msgCopy.getAttribute('data-msg-id') || '0');
      const m = messages.find((mm) => mm.id === id);
      const text = m?.raw ?? m?.html ?? '';
      copyToClipboard(text, msgCopy, '⧉', '✓');
      return;
    }

    // 3) Open a link via the Tauri shell (was the only handler before).
    const a = t.closest('a[data-url]') as HTMLAnchorElement | null;
    if (!a) return;
    e.preventDefault();
    const url = a.getAttribute('data-url');
    if (url) openUrl(url).catch((err) => showError('open_url: ' + err));
  }

  // Tiny clipboard helper that flashes the button label for 1.5s.
  async function copyToClipboard(text: string, btn: HTMLElement, idleLabel: string, okLabel: string) {
    try {
      await navigator.clipboard.writeText(text);
      btn.textContent = okLabel;
      btn.classList.add('copied');
    } catch {
      btn.textContent = '✕ Ошибка';
    }
    setTimeout(() => {
      btn.textContent = idleLabel;
      btn.classList.remove('copied');
    }, 1500);
  }

  // (removed) `refreshKeys()` — superseded by `refreshKeyStatus()` from
  // the shared keyStore. App.svelte, Chat.svelte and Settings.svelte all
  // call it; the store dedupes the IPC so the keyring is hit once.

  async function send() {
    if (mode === 'research') {
      fetchResearch();
      return;
    }
    const text = inputText.trim();
    if (!text || !hasMinimax) return;
    inputText = '';
    if (inputEl) inputEl.style.height = 'auto';
    errorBanner = '';
    appendMessage('user', text);
    // If a stream is already in flight, the user is "steering" it.
    // Bump the token so the old stream's event handlers stop updating
    // the UI, and mark its current bubble as interrupted. The old
    // `doChat` will return shortly (the next `ai_done` it sees is
    // ignored because of the token bump, so the await unwinds on
    // its own when the backend's stream endpoint closes). We then
    // start a fresh round immediately — no queueing, no waiting
    // for the previous response to fully drain.
    if (busy) {
      streamToken++;
      streamingId = null;
      // Append a small note to the cancelled assistant bubble so the
      // user knows the partial answer was superseded.
      if (messages.length > 0) {
        const last = messages[messages.length - 1];
        if (last.role === 'assistant' && last.streaming) {
          messages = messages.map((m) =>
            m.id === last.id
              ? {
                  ...m,
                  streaming: false,
                  html: (m.html || '') + '\n\n_[прервано — новое сообщение]_',
                }
              : m,
          );
        }
      }
    }
    busy = true;
    try {
      await doChat(text);
    } catch (e) {
      const m = (e && (e as Error).message) || String(e);
      showError(m);
      if (mode === 'chat') history.pop();
    } finally {
      busy = false;
      inputEl?.focus();
    }
  }

  // Pure "stop" — abandon the current stream without sending anything.
  // Wired to the cancel button (visible only while busy).
  function cancelCurrent() {
    if (!busy) return;
    streamToken++;
    streamingId = null;
    if (messages.length > 0) {
      const last = messages[messages.length - 1];
      if (last.role === 'assistant' && last.streaming) {
        messages = messages.map((m) =>
          m.id === last.id
            ? { ...m, streaming: false, html: (m.html || '') + '\n\n_[остановлено]_' }
            : m,
        );
      }
    }
    busy = false;
  }

  async function doChat(text: string) {
    if (!hasMinimax) {
      throw new Error('MiniMax-ключ не задан. Открой вкладку ⚙ Settings и введи его.');
    }
    history.push({ role: 'user', content: text });
    const myToken = ++streamToken;
    const id = appendMessage('assistant', '', { streaming: true, modelTag: selectedModel.label });
    streamingId = id;
    const model = selectedModel.model || null;

    // ---- per-request scope for streaming listeners + tool-pill map ----
    // Before subscribing to a new round of events, dispose the previous
    // round's listeners. Without this, every `send()` would push another
    // 9 listeners into `streamUnlisten` and the array would grow
    // unbounded. We keep one safety net: `onDestroy` also disposes them.
    for (const off of streamUnlisten) {
      try { off(); } catch { /* listener may already be torn down */ }
    }
    streamUnlisten = [];
    // The tool-pill map is also per-request: it links Rust tool_call ids
    // to the message ids we create for the current stream. Carrying the
    // map across requests would risk a stale id from a previous (failed)
    // request colliding with a fresh one and mis-routing a result.
    const pendingToolPills = new Map<string, number>();
    // Multitask is a per-request system hint: prepend to the outgoing
    // payload only, never to `history` (keeps the visible chat clean and
    // means toggling multitask off later doesn't leave a stale message in
    // context). The hint steers the model toward `parallel_research` /
    // `parallel_generate_images` fan-out calls.
    const messagesToSend = multitask
      ? [{ role: 'system', content: MULTITASK_HINT }, ...history]
      : history;
    let acc = '';
    let think = '';
    let cancelled = false;
    // The id of the assistant message that the current text stream is
    // writing into. Starts as the initial id; gets rotated to a fresh
    // bubble every time `ai_tool_use` fires so that the chat visually
    // splits text → tool → text into separate cards. The `acc` string
    // is *not* rotated — it accumulates the full reply for `history`
    // so the model still sees one continuous turn.
    let currentTextId = id;
    /**
     * Commit whatever text is in `pendingText` for `currentTextId` and
     * start a fresh streaming bubble. Called when a `ai_tool_use`
     * event arrives (and at the end of the stream). Safe to call
     * multiple times — no-ops if the current bubble is already empty.
     */
    const rotateTextBubble = () => {
      const cur = messages.find((m) => m.id === currentTextId);
      const pending = cur?.pendingText ?? '';
      if (cur) {
        // Commit: stop streaming, render pending → html, drop pendingText.
        if (pending.length > 0) {
          messages = messages.map((m) =>
            m.id === currentTextId
              ? {
                  ...m,
                  streaming: false,
                  html: renderMarkdown(pending),
                  pendingText: '',
                }
              : m
          );
        } else {
          // Empty preamble — just drop the bubble entirely so the
          // chat doesn't show a stray empty card.
          messages = messages.filter((m) => m.id !== currentTextId);
        }
      }
      // Open a fresh text bubble. The first chunk after a rotate will
      // land here.
      const newId = nextId++;
      messages = [
        ...messages,
        {
          id: newId,
          role: 'assistant',
          html: '',
          streaming: true,
          modelTag: selectedModel.label,
          pendingText: '',
        },
      ];
      currentTextId = newId;
      streamingId = newId;
    };
    const offChunk = await onAiChunk((delta) => {
      if (myToken !== streamToken || cancelled) return;
      acc += delta;
      // Strip `<think>...</think>` from the chunk. The Rust side already
      // separates reasoning_content as a separate `ai_thinking` event,
      // but some models ALSO wrap their reasoning in tags inside the
      // content field — without this strip, the user would see the
      // tags literally in the message body. Extracted bodies are
      // appended to the existing thinking field so they end up in
      // the collapsible 💭 block (and the visible text stays clean).
      const { text, thinking: fromTags } = stripThinkingTags(delta);
      if (text) {
        messages = messages.map((m) =>
          m.id === currentTextId
            ? { ...m, pendingText: (m.pendingText ?? '') + text }
            : m
        );
      }
      if (fromTags) {
        think += (think ? '\n\n' : '') + fromTags;
        messages = messages.map((m) =>
          m.id === currentTextId ? { ...m, thinking: think } : m
        );
      }
      startTypeTick(currentTextId);
      const el = document.getElementById('chat-scroll');
      if (el && stickToBottom) el.scrollTop = el.scrollHeight;
    });
    const offThink = await onAiThinking((delta) => {
      if (myToken !== streamToken || cancelled) return;
      think += delta;
      // Thinking lives on the *first* text bubble of the turn so the
      // 💭 block stays anchored to the user-visible "preamble" rather
      // than jumping to a post-tool bubble.
      patchMessage(id, { thinking: think });
    });
    const offDone = await onAiDone(() => {
      if (myToken !== streamToken || cancelled) return;
      // Flush whatever is still in the typewriter queue.
      stopTypeTickAndFlush();
      const hadThink = !!think && think.trim().length > 0;
      const cur = messages.find((m) => m.id === id);
      patchMessage(id, {
        streaming: false,
        thinkingOpen: hadThink ? (cur?.thinkingOpen === true ? true : false) : false,
      });
      // Close the *current* (possibly post-tool) text bubble. If it's
      // empty, drop it so we don't leave a stray empty card.
      const tail = messages.find((m) => m.id === currentTextId);
      const tailText = tail?.pendingText ?? '';
      if (tail && currentTextId !== id) {
        if (tailText.length > 0) {
          messages = messages.map((m) =>
            m.id === currentTextId
              ? {
                  ...m,
                  streaming: false,
                  html: renderMarkdown(tailText),
                  pendingText: '',
                }
              : m
          );
        } else {
          messages = messages.filter((m) => m.id !== currentTextId);
        }
      }
      if (acc.trim()) {
        // History persists only the visible text — strip thinking tags.
        const { text: cleanAcc } = stripThinkingTags(acc);
        history.push({ role: 'assistant', content: cleanAcc });
      } else {
        patchMessage(id, { html: renderMarkdown('_(пустой ответ)_'), raw: '(пустой ответ)' });
      }
      if (streamingId === id || streamingId === currentTextId) streamingId = null;
    });
  // Build a placeholder message for a tool_use event so the UI can show
  // something the moment the model invokes a tool. Returned messages use
  // either kind: 'image_loading' (single image), 'subagents' (parallel
  // fan-out) or 'tool_use' (generic compact pill).
  function buildToolPlaceholder(
    name: string,
    args: Record<string, unknown> | null | undefined,
    argsStr: string,
    pillId: number,
  ): typeof messages[number] | null {
    if (name === 'parallel_research' || name === 'parallel_generate_images') {
      const subKind: 'research' | 'images' = name === 'parallel_generate_images' ? 'images' : 'research';
      const titles: string[] = subKind === 'images'
        ? ((args?.items as Array<{ prompt?: string }> | undefined)?.map((it) => it.prompt || 'image') ?? [])
        : ((args?.queries as string[] | undefined)?.slice() ?? []);
      return {
        id: pillId,
        role: 'assistant',
        html: '',
        kind: 'subagents',
        subKind,
        toolName: name,
        toolStatus: 'pending',
        subagents: titles.map((t, i) => ({ id: i + 1, title: t, status: 'pending' as const })),
      } as any;
    }
    return {
      id: pillId,
      role: 'assistant',
      html: '',
      kind: 'tool_use',
      toolName: name,
      toolArgs: argsStr,
      toolStatus: 'pending',
    } as any;
  }

    const offToolUse = await onAiToolUse((p) => {

      if (myToken !== streamToken || cancelled) return;
      const argsStr = p.args ? JSON.stringify(p.args, null, 2) : '';
      const pillId = nextId++;

      // Close out the current text bubble and open a fresh one so
      // the text → tool_use → text pattern renders as visually
      // distinct cards in the chat. (See `rotateTextBubble`.)
      rotateTextBubble();

      // Count tools on the *first* text bubble of this turn for the footer.
      // The first bubble id is the original `id` from the doChat start.
      messages = messages.map((m) =>
        m.id === id ? { ...m, toolCount: (m.toolCount ?? 0) + 1 } : m
      );

      // For file-edit tools we eagerly create a `file_edit` placeholder
      // so the matching `ai_file_edit` event can fill the diff in. We
      // also stash the tool_call id → message id mapping in
      // `pendingFileEdits` for the same reason.
      if (p.name === 'edit_file' || p.name === 'create_file') {
        const fp = (p.args?.path as string) || '';
        const placeholderId = nextId++;
        pendingFileEdits.set(p.id, placeholderId);
        messages = [
          ...messages,
          {
            id: placeholderId,
            role: 'assistant',
            html: '',
            kind: 'file_edit',
            filePath: fp,
            fileDiff: '',
            fileEditId: '',
            fileEditState: 'pending',
            createdAt: Date.now(),
          },
        ];
        pendingToolPills.set(p.id, placeholderId);
        scrollToBottom();
        return;
      }

      if (p.name === 'generate_image') {
        // Perplexity-style: show a rose-gold shimmering image placeholder
        // that gets swapped for the real picture when the tool resolves.
        const args = p.args || {};
        const aspect = (args.aspect_ratio || '1:1') as ImageAspect;
        const prompt = (args.prompt as string) || '';
        messages = [
          ...messages,
          {
            id: pillId,
            role: 'assistant',
            html: '',
            kind: 'image_loading',
            imageAspect: aspect,
            imagePrompt: prompt,
          },
        ];
      } else {
        const placeholder = buildToolPlaceholder(p.name, p.args, argsStr, pillId);
        if (placeholder) {
          messages = [...messages, placeholder];
        }
      }

      pendingToolPills.set(p.id, pillId);
      scrollToBottom();

      // Stale-pill watchdog. If the backend never sends `ai_tool_result`
      // (e.g. stream dropped, model returned malformed tool_call, etc.)
      // the pill would stay "работаю…" forever. After 45s we flip it
      // to an error state and drop the id from the map so the model
      // can move on.
      const myPillId = pillId;
      const myCallId = p.id;
      setTimeout(() => {
        if (myToken !== streamToken || cancelled) return;
        if (!pendingToolPills.has(myCallId)) return; // already resolved
        pendingToolPills.delete(myCallId);
        messages = messages.map((mm) =>
          mm.id === myPillId
            ? { ...mm, toolStatus: 'error', toolError: 'Таймаут: ответ от бэкенда не пришёл (45 с).' }
            : mm,
        );
      }, 45_000);
    });
    const offToolResult = await onAiToolResult((p) => {
      if (myToken !== streamToken || cancelled) return;
      const pillId = pendingToolPills.get(p.id);
      if (!pillId) return;
      if (!p.ok) {
        messages = messages.map((m) =>
          m.id === pillId
            ? { ...m, toolStatus: 'error', toolError: p.error || 'tool failed' }
            : m,
        );
        return;
      }
      const dataUrl = p.data_url || '';
      if (dataUrl) {
        const card: ImageCard = {
          id: pillId,
          dataUrl,
          prompt: p.prompt || '',
          aspect: (p.aspect || '1:1') as ImageAspect,
        };
        imageResults = [card, ...imageResults];
        persistImages();
        messages = messages.map((m) =>
          m.id === pillId
            ? {
                ...m,
                kind: 'image',
                html: '',
                imageDataUrl: dataUrl,
                imagePrompt: p.prompt || '',
                imageAspect: (p.aspect || '1:1') as ImageAspect,
                toolStatus: 'ok',
              }
            : m,
        );
      } else {
        messages = messages.map((m) =>
          m.id === pillId ? { ...m, toolStatus: 'ok' } : m,
        );
      }
      scrollToBottom();
    });
    const offInterests = await onAiUserInterests((p) => {
      if (myToken !== streamToken || cancelled) return;
      if (p.ok && Array.isArray(p.interests)) mergeInterests(p.interests);
    });
    // Multitask: the agent launched several sub-agents in parallel
    // (parallel_research or parallel_generate_images). Each call gets a
    // placeholder `subagents` message; the result event fills the cards.
    const offSubagents = await onAiSubagentResult((p) => {
      if (myToken !== streamToken || cancelled) return;
      const pillId = pendingToolPills.get(p.id);
      if (!pillId) return;
      const subs = (p.subagents || []).map((s, i) => ({
        id: i + 1,
        title: p.kind === 'images'
          ? (s.prompt || `image ${i + 1}`)
          : (s.query || s.title || `topic ${i + 1}`),
        status: 'ok' as const,
        result: p.kind === 'research' ? s.results : undefined,
        dataUrl: p.kind === 'images' ? s.data_url : undefined,
        aspect: p.kind === 'images' ? s.aspect : undefined,
      }));
      messages = messages.map((m) =>
        m.id === pillId
          ? { ...m, kind: 'subagents', subKind: p.kind, subagents: subs, toolStatus: 'ok' }
          : m,
      );
      // If we got images, also push them into the gallery.
      if (p.kind === 'images') {
        for (const s of p.subagents) {
          if (s.data_url) {
            const dataUrl = s.data_url;
            const prompt = s.prompt || '';
            const aspect = (s.aspect || '1:1') as ImageAspect;
            imageResults = [
              { id: nextId++, dataUrl, prompt, aspect },
              ...imageResults,
            ];
          }
        }
        persistImages();
      }
      scrollToBottom();
    });
    // Web search results from the `web_search` tool. We attach them to the
    // matching tool_use pill so the user sees the actual links the model
    // got back (not just the final summary). Falls back to a standalone
    // card if the pill can't be found (e.g. the event arrived out of order).
    const offWebSearch = await onAiWebSearch((p) => {
      if (myToken !== streamToken || cancelled) return;
      const results = (p.results || []).slice(0, 10).map((r) => ({
        title: r.title || '',
        url: r.url || '',
        snippet: r.snippet || '',
        host: r.host || (() => { try { return new URL(r.url).host; } catch { return ''; } })(),
      }));
      const pillId = pendingToolPills.get(p.id);
      if (pillId != null) {
        messages = messages.map((m) =>
          m.id === pillId
            ? { ...m, kind: 'web_search', webQuery: p.query, webResults: results, toolStatus: 'ok' }
            : m,
        );
      } else {
        messages = [
          ...messages,
          {
            id: nextId++,
            role: 'assistant',
            html: '',
            kind: 'web_search',
            webQuery: p.query,
            webResults: results,
          },
        ];
      }
      scrollToBottom();
    });
    // Step-by-step plan: create_plan opens a visible plan card; update_step
    // flips a step's status in-place. We map the tool_use id from Rust
    // (`p.id`) to our internal plan-card message id via `pendingToolPills`,
    // but for plans we want a *new* message rather than piggy-backing on
    // the generic tool_use pill. Strategy: stash the plan message id
    // under the plan's own tool_use id in a separate Map.
    const planByToolCall: Map<string, number> = new Map();
    const offPlanCreated = await onAiPlanCreated((p) => {
      if (myToken !== streamToken || cancelled) return;
      const planMsgId = nextId++;
      planByToolCall.set(p.id, planMsgId);
      pendingToolPills.set(p.id, planMsgId);
      // Sync the plan into the local store. If the user already had a
      // matching plan in the sidebar (because they clicked Run), this
      // attaches the tool call to that plan. Otherwise we create a new
      // agent-only plan in the sidebar so the user can still see it.
      const { planId: storePlanId } = recordAgentPlan({
        toolCallId: p.id,
        title: p.title || 'Plan',
        steps: p.steps || [],
      });
      // Link the chat message back to the store plan so future
      // ai_step_updated events can find it via either the store's
      // chatMessageId lookup OR the transient toolCallToPlan map.
      linkPlanToMessage(storePlanId, planMsgId);
      // If the user just clicked Run, clear the pending hint now that
      // we've matched the agent's plan to their sidebar plan.
      if (pendingLinkPlanId && pendingLinkPlanId === storePlanId) {
        pendingLinkPlanId = null;
      }
      const steps = (p.steps || []).map((s) => ({
        id: s.id,
        title: s.title,
        status: s.status || 'pending',
      }));
      messages = [
        ...messages,
        {
          id: planMsgId,
          role: 'assistant',
          html: '',
          kind: 'plan',
          planTitle: p.title || 'Plan',
          planSteps: steps,
        },
      ];
      scrollToBottom();
    });
    const offStepUpdated = await onAiStepUpdated((p) => {
      if (myToken !== streamToken || cancelled) return;
      // Forward to the local plan store so the sidebar updates in
      // real time alongside the chat card. No-op if the tool call
      // is unknown (e.g. an event from a previous session that the
      // store has already forgotten).
      applyAgentStepUpdate({
        toolCallId: p.id,
        stepId: p.step_id,
        status: p.status,
        note: p.note,
      });
      // Find the plan card this update belongs to. We track the most
      // recent plan message — if the model opened a new plan in between,
      // we want the latest one to receive updates.
      let planMsgId: number | null = planByToolCall.get(p.id) ?? null;
      if (planMsgId == null) {
        for (let i = messages.length - 1; i >= 0; i--) {
          if (messages[i].kind === 'plan') { planMsgId = messages[i].id; break; }
        }
      }
      if (planMsgId == null) return;
      messages = messages.map((m) => {
        if (m.id !== planMsgId) return m;
        const steps = (m.planSteps || []).map((s) =>
          s.id === p.step_id
            ? { ...s, status: p.status as typeof s.status, note: p.note ?? s.note }
            : s
        );
        return { ...m, planSteps: steps };
      });
      scrollToBottom();
    });
    streamUnlisten.push(offChunk, offThink, offDone, offToolUse, offToolResult, offInterests, offSubagents, offWebSearch, offPlanCreated, offStepUpdated);
    try {
      await minimaxChatStream({ messages: messagesToSend, model });
    } catch (e) {
      cancelled = true;
      patchMessage(id, {
        html: renderMarkdown('⚠ ' + ((e as Error)?.message || e)),
        raw: '⚠ ' + ((e as Error)?.message || e),
        streaming: false,
      });
      if (streamingId === id) streamingId = null;
      history.pop();
      throw e;
    }
  }

  let inputEl: HTMLTextAreaElement | null = null;
  function onInputKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }
  function autosize() {
    if (!inputEl) return;
    inputEl.style.height = 'auto';
    inputEl.style.height = Math.min(inputEl.scrollHeight, 160) + 'px';
  }

  onMount(async () => {
    try {
      const saved = localStorage.getItem(MODEL_STORAGE_KEY);
      if (saved && MODELS.some((m) => m.id === saved)) selectedModelId = saved;
    } catch { /* ignore */ }
    try {
      multitask = localStorage.getItem(MULTITASK_STORAGE_KEY) === '1';
    } catch { /* ignore */ }

    userInterests = readInterests();
    // Seed the Rust-side cache so `get_user_interests` works right away.
    setUserInterests(userInterests).catch(() => { /* non-fatal */ });
    // Pull initial key status from the shared store (deduped with App.svelte).
    await refreshKeyStatus();

    // Restore the most recent chat from disk. The Rust side keeps
    // `current_chat_id` so we know which conversation was open last.
    try {
      const id = await currentChatId();
      if (id) {
        const full = await loadChat(id);
        if (full && Array.isArray(full.messages) && full.messages.length > 0) {
          chatId = full.id;
          messages = full.messages as typeof messages;
          // Skip the auto-intro below — we already have content.
          loadedFromDisk = true;
        }
      }
    } catch (e) {
      console.warn('chat history load failed', e);
    }

    if (messages.length === 0) {
      // The startup system message adapts to the current `mode` so the
      // user doesn't see "press Ctrl+Space" while looking at Code/Research/
      // Media. We rebuild the message when the user later switches tabs
      // (see the `reactiveIntro` block below).
      appendMessage('system', introForMode(mode, hasMinimax));
    }

    try {
      voiceUnlistens.push(
        await onSttStateChange((p) => {
          if (p.state === 'idle') voiceState = 'idle';
          else if (p.state === 'listening') voiceState = 'recording';
          else if (p.state === 'processing') voiceState = 'transcribing';
          else voiceState = p.state;
        }),
      );
      voiceUnlistens.push(
        await onSttResult((p) => {
          if (p.isFinal && p.transcript.trim()) {
            const text = p.transcript.trim();
            inputText = inputText.trim() ? `${inputText.trim()} ${text}` : text;
            tick().then(() => {
              if (inputEl) {
                inputEl.style.height = 'auto';
                inputEl.style.height = Math.min(inputEl.scrollHeight, 160) + 'px';
                inputEl.focus();
              }
            });
          }
        }),
      );
      voiceUnlistens.push(
        await onSttError((e) => {
          voiceError = `${e.code}: ${e.message}`;
          voiceState = 'error';
          if (e.code === 'MODEL_NOT_INSTALLED') {
            autoModalOpen = true;
            refreshWhisperModels();
          }
        }),
      );
      voiceUnlistens.push(
        await onSttDownloadProgress((p) => {
          if (p.status === 'downloading') {
            downloadPct = p.progress ?? 0;
            const mb = p.downloaded != null && p.total != null
              ? `${(p.downloaded / 1024 / 1024).toFixed(0)} / ${(p.total / 1024 / 1024).toFixed(0)} MB`
              : '';
            downloadProgress = `${p.modelId ?? '?'}: ${(p.progress ?? 0).toFixed(0)}%${mb ? ' · ' + mb : ''}`;
          } else if (p.status === 'complete') {
            downloadProgress = `${p.modelId}: ✓ готово`;
            downloadPct = 100;
            autoModalOpen = false;
            refreshWhisperModels();
          } else if (p.status === 'error') {
            downloadProgress = `${p.modelId ?? '?'}: ошибка${p.message ? ' — ' + p.message : ''}`;
            downloadPct = null;
          }
        }),
      );
      voiceUnlistens.push(
        await onHotkeyPressed(() => {
          if (voiceState === 'recording') return;
          toggleVoice();
        }),
      );
      voiceUnlistens.push(
        await onHotkeyReleased(() => {
          if (voiceState === 'recording') toggleVoice();
        }),
      );
    } catch { /* non-fatal */ }

    refreshWhisperModels();
    getModelsDir().then((p) => (modelsDir = p)).catch(() => {});

    // Agent (workspace + file events). Fetch initial state, then attach
    // listeners. We re-use `currentWorkspace` and `workspaceTree` from
    // the Code-mode sidebar.
    try {
      currentWorkspaceInfo = await currentWorkspace();
      // If nothing is open yet, auto-pick: existing recent workspace, or
      // the process CWD. The user can override with 📂 → pick folder.
      if (!currentWorkspaceInfo) {
        try {
          currentWorkspaceInfo = await defaultWorkspace();
        } catch (e) {
          console.warn('default_workspace failed', e);
        }
      }
    } catch { /* non-fatal */ }
    await refreshRecent();
    if (currentWorkspaceInfo) {
      await refreshTree();
    }
    attachAgentListeners();
  });

  onDestroy(() => {
    for (const u of voiceUnlistens) try { u(); } catch { /* ignore */ }
    for (const u of streamUnlisten) try { u(); } catch { /* ignore */ }
    for (const u of workspaceUnlisten) try { u(); } catch { /* ignore */ }
    for (const u of agentUnlisten) try { u(); } catch { /* ignore */ }
  });
</script>

<svelte:window on:click={onWindowClick} on:keydown={onWindowKey} />
<div class="chat">
  <header class="bar">
    <div class="left">
      <span class="title">{providerLabel}</span>
      <span class="sub" class:ok={hasMinimax} class:miss={!hasMinimax && !checkingKeys}>
        {checkingKeys ? '…' : hasMinimax ? `minimax · ${selectedModel.label}` : 'нет ключа'}
      </span>
    </div>
    <div class="middle">
      <button class="seg" class:on={mode === 'chat'} on:click={() => setMode('chat')}>💬 Chat</button>
      <button class="seg" class:on={mode === 'code'} on:click={() => setMode('code')}>
        💻 Code{currentWorkspace ? '' : ' · no ws'}
      </button>
      <button class="seg" class:on={mode === 'research'} on:click={() => setMode('research')}>🔬 Fusion Research</button>
      <button class="seg" class:on={mode === 'media'} on:click={() => setMode('media')}>
        🖼 Media{#if imageResults.length > 0}<span class="seg-badge">{imageResults.length}</span>{/if}
      </button>
      <button class="seg" class:on={mode === 'plan'} on:click={() => setMode('plan')}>
        📋 План
      </button>
    </div>
    <div class="right">
      <label class="model-pick" title="Выбор модели MiniMax">
        <span class="model-label">model</span>
        <select bind:value={selectedModelId} on:change={onModelChange}>
          {#each MODELS as m (m.id)}
            <option value={m.id}>{m.label}</option>
          {/each}
        </select>
      </label>
      <button class="ico danger" on:click={clearChat} title="Очистить чат" aria-label="Clear">🗑</button>
    </div>
  </header>

  <main class="scroll" id="chat-scroll" on:scroll={onScroll} on:click={onMessagesClick}>
    {#if mode === 'code'}
      <div class="code-grid">
        <!-- LEFT: file tree -->
        <aside class="file-tree-pane" aria-label="Файлы">
          <div class="ft-head">
            <div class="ft-title">
              <span class="ft-icon">📁</span>
              <span class="ft-name" title={currentWorkspace?.path || ''}>
                {currentWorkspace?.name || 'no workspace'}
              </span>
            </div>
            <div class="ft-actions">
              <button class="ft-btn" on:click={pickAndOpenWorkspace} title="Открыть папку">📂</button>
              <button class="ft-btn" on:click={openNewProjectDialog} title="Новый проект">＋</button>
              <button class="ft-btn" on:click={refreshTree} disabled={!currentWorkspace || workspaceLoading} title="Обновить">↻</button>
              {#if currentWorkspace}
                <button class="ft-btn danger" on:click={closeCurrentWorkspace} title="Закрыть">✕</button>
              {/if}
            </div>
          </div>
          {#if !currentWorkspace}
            <div class="ft-empty">
              <div class="ft-empty-h">Нет открытого workspace</div>
              <div class="ft-empty-sub">Откройте папку с проектом или создайте новый</div>
              <div class="ft-empty-actions">
                <button class="ft-cta" on:click={pickAndOpenWorkspace}>📂 Открыть папку</button>
                <button class="ft-cta ghost" on:click={openNewProjectDialog}>＋ Новый проект</button>
              </div>
            </div>
          {:else}
            {#if recentWorkspaces.length > 0}
              <details class="ft-recent">
                <summary>🕘 Recent ({recentWorkspaces.length})</summary>
                <ul class="ft-recent-list">
                  {#each recentWorkspaces as w (w.path)}
                    <li>
                      <button class="ft-recent-item" on:click={() => openRecentWorkspace(w)}>
                        <div class="ft-recent-name">{w.name}</div>
                        <div class="ft-recent-path">{w.path}</div>
                      </button>
                    </li>
                  {/each}
                </ul>
              </details>
            {/if}
            <ul class="ft-list">
              {#if workspaceLoading}
                <li class="ft-loading">Загрузка…</li>
              {/if}
              {#each workspaceTree as entry (entry.path)}
                <li>
                  <button
                    class="ft-item"
                    class:dir={entry.kind === 'dir'}
                    on:click={() => insertMention(entry.path)}
                    title={`Insert @${entry.path}`}
                  >
                    <span class="ft-item-icon">{entry.kind === 'dir' ? '📁' : '📄'}</span>
                    <span class="ft-item-name">{entry.path}</span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </aside>

        <!-- CENTER: chat (reuse existing message loop) -->
        <div class="code-center">
          <div class="cc-scroll" on:scroll={onScroll} on:click={onMessagesClick}>
            {#each messages as m (m.id)}
              {#if m.role === 'system'}
                <div class="msg msg-system"><div class="body">{m.html ? m.html : m.raw}</div></div>
              {:else}
                {@const isUser = m.role === 'user'}
                <div class="msg-row" class:user={isUser} class:assistant={!isUser}>
                  <div class="msg-avatar" aria-hidden="true">{isUser ? '👤' : '🌙'}</div>
                  <div class="msg-col">
                    <div class="msg-head">
                      <span class="msg-name">{isUser ? 'Ты' : 'Luna'}</span>
                      {#if !isUser && m.modelTag}<span class="msg-model">{m.modelTag}</span>{/if}
                      <span class="msg-time">{formatTime(m.createdAt)}</span>
                      {#if !isUser && !m.streaming}
                        <button class="msg-copy" data-msg-id={m.id} type="button" title="Скопировать сообщение" aria-label="Скопировать сообщение">⧉</button>
                      {/if}
                    </div>
                    <div class="msg-bubble">
                      {#if m.kind === 'file_edit'}
                        {@const state = m.fileEditState || 'pending'}
                        <div class="edit-card" class:rejected={state === 'rejected'} class:accepted={state === 'accepted'}>
                          <div class="edit-card-head">
                            <span class="edit-card-icon">{state === 'rejected' ? '↩' : (state === 'pending' ? '✎' : '✓')}</span>
                            <span class="edit-card-path" title={m.filePath}>{m.filePath}</span>
                            <span class="edit-card-state">{state === 'pending' ? 'применено' : (state === 'rejected' ? 'откачено' : 'принято')}</span>
                          </div>
                          {#if m.fileDiff}
                            <pre class="diff-body">{m.fileDiff}</pre>
                          {:else}
                            <div class="edit-card-pending">применяю…</div>
                          {/if}
                          {#if state === 'accepted'}
                            <div class="edit-card-actions">
                              <button class="ea-btn" disabled>✓ принято</button>
                              <button class="ea-btn reject" on:click={() => m.fileEditId && rejectFileEdit(m.fileEditId, m.id)} title="Откатить изменение и восстановить файл из бэкапа">✗ Откатить</button>
                            </div>
                          {:else if state === 'rejected'}
                            <div class="edit-card-actions">
                              <span class="ea-note">Файл восстановлен из бэкапа</span>
                            </div>
                          {/if}
                        </div>
                      {:else if m.kind === 'file_read'}
                        <div class="read-card">
                          <button class="read-card-head" on:click={() => toggleFileRead(m.id)}>
                            <span class="read-card-icon">📄</span>
                            <span class="read-card-path" title={m.filePath}>{m.filePath}</span>
                            <span class="read-card-meta">{m.fileReadLines ?? 0} строк · {m.fileReadBytes ?? 0} B</span>
                            <span class="read-card-chevron">{m.fileReadOpen ? '▾' : '▸'}</span>
                          </button>
                          {#if m.fileReadOpen && m.fileReadContent}
                            <pre class="read-card-body">{m.fileReadContent}</pre>
                          {/if}
                        </div>
                      {:else}
                        <div class="body">{@html m.html}</div>
                      {/if}
                    </div>
                  </div>
                </div>
              {/if}
            {/each}
            {#if messages.length === 0}
              <div class="empty-chat-hint">
                <div class="ech-h">💻 Code mode</div>
                <div class="ech-sub">Задайте задачу агенту — он прочитает, спланирует и отредактирует файлы в workspace.</div>
              </div>
            {/if}
          </div>
          <!-- Input — shared with the chat mode template below -->
          <div class="cc-input-wrap">
            <textarea
              bind:this={inputEl}
              bind:value={inputText}
              on:keydown={onInputKey}
              on:input={() => { if (inputEl) detectMention(inputText, inputEl.selectionStart); }}
              on:click={() => { if (inputEl) detectMention(inputText, inputEl.selectionStart); }}
              on:keyup={() => { if (inputEl) detectMention(inputText, inputEl.selectionStart); }}
              placeholder="Опишите задачу или @-упомяните файл…"
              rows="1"
            ></textarea>
            <button class="send" on:click={send} disabled={busy || !inputText.trim()}>↑</button>
            {#if mentionOpen}
              <div class="mention-popover">
                {#if mentionSuggestions.length === 0}
                  <div class="mention-empty">Нет файлов</div>
                {:else}
                  {#each mentionSuggestions as s, i (s.path)}
                    <button class="mention-item" on:click={() => insertMention(s.path)}>
                      <span class="mention-icon">📄</span>
                      <span class="mention-path">{s.path}</span>
                    </button>
                  {/each}
                {/if}
              </div>
            {/if}
          </div>
        </div>

        <!-- RIGHT: preview pane -->
        <aside class="preview-pane" aria-label="Preview">
          <div class="pv-head">
            <span class="pv-title">Preview</span>
            <input class="pv-port" type="number" min="1024" max="65535" bind:value={previewPort} title="Порт" />
            <button class="pv-btn primary" on:click={startPreview} disabled={!currentWorkspace || previewBusy}>
              {previewBusy ? '…' : (previewUrl ? '↻' : '▶')}
            </button>
            {#if previewUrl}
              <button class="pv-btn" on:click={refreshPreview} title="Reload">↻</button>
              <button class="pv-btn" on:click={openPreviewInWindow} title="Открыть в окне">↗</button>
              <button class="pv-btn" on:click={openPreviewInBrowser} title="Браузер">🌐</button>
            {/if}
          </div>
          {#if previewError}
            <div class="pv-error">⚠ {previewError}</div>
          {/if}
          {#if previewUrl}
            <iframe
              class="pv-frame"
              key={previewRefreshKey}
              src={previewUrl}
              title="preview"
              sandbox="allow-scripts allow-same-origin allow-forms"
            ></iframe>
          {:else}
            <div class="pv-empty">
              <div class="pv-empty-h">Нет активного preview</div>
              <div class="pv-empty-sub">Запустите dev-сервер, чтобы увидеть приложение здесь.</div>
            </div>
          {/if}
        </aside>
      </div>
    {:else if mode === 'research'}
      <div class="research-view">
        <aside class="research-sidebar" aria-label="Интересы">
          <div class="sidebar-head">
            <div class="sidebar-title">
              <span class="sidebar-icon">📚</span>
              <span>Интересы</span>
            </div>
            <span class="sidebar-count">{userInterests.length}</span>
          </div>
          <p class="sidebar-sub">
            {#if userInterests.length === 0}
              Список пуст — покажем мировые новости. Агент заполнит его в чате, либо добавь вручную ↓
            {:else}
              Агент сам пополняет список в чате. Можно добавить или удалить здесь.
            {/if}
          </p>

          <form class="sidebar-add" on:submit|preventDefault={addInterestFromSidebar}>
            <input
              class="sidebar-input"
              type="text"
              bind:value={sidebarNewInterest}
              placeholder="+ Новый интерес…"
              aria-label="Добавить интерес"
            />
            <button class="sidebar-add-btn" type="submit" title="Добавить" aria-label="Add">＋</button>
          </form>

          <button class="refresh-btn sidebar-refresh" on:click={fetchResearch} disabled={researchLoading} title="Обновить ленту">
            {#if researchLoading}<span class="spinner-mini"></span>Ищу…{:else}🔄 Обновить{/if}
          </button>

          <div class="sidebar-interests">
            {#if userInterests.length === 0}
              <div class="sidebar-empty">⚠ пока пусто</div>
            {/if}
            {#each userInterests as t (t)}
              <div class="sidebar-interest">
                <span class="interest-hash">#</span>
                <span class="interest-text" title={t}>{t}</span>
                <button class="interest-remove" on:click={() => removeInterest(t)} title="Удалить «{t}»" aria-label="Удалить {t}">×</button>
              </div>
            {/each}
          </div>

          {#if userInterests.length > 0}
            <button class="sidebar-clear" on:click={clearAllInterests} title="Очистить все интересы">× Очистить все</button>
          {/if}

          {#if researchCacheStats}
            <div class="cache-stats" title={researchCacheStats.path}>
              <span class="cache-icon">📦</span>
              <span class="cache-text">
                {researchCacheStats.fresh} свежих / {researchCacheStats.stale} устаревших
              </span>
              <button class="cache-clear" on:click={clearResearchCache} title="Очистить кэш">×</button>
            </div>
          {/if}
        </aside>

        <div class="research-feed">
          <div class="research-head">
            <div class="research-h-row">
              <div class="research-h">🔬 Fusion Research</div>
              <div class="research-sub" aria-live="polite">
                {#if researchQuery}Запрос: <b>«{researchQuery}»</b>{:else if researchAllResults.length}Найдено {researchAllResults.length} {researchAllResults.length === 1 ? 'результат' : (researchAllResults.length < 5 ? 'результата' : 'результатов')}{:else}Поиск по web · workspace · RSS{/if}
              </div>
            </div>
            <form class="research-query" on:submit|preventDefault={() => runResearch(researchQuery || (userInterests[0] ?? 'AI news'))}>
              <input
                class="research-input"
                type="text"
                bind:value={researchQuery}
                placeholder="🔍 Что исследуем? (например: AI agents, Rust async, SpaceX)"
                aria-label="Поисковый запрос"
                disabled={researchLoading}
              />
              <button class="research-run" type="submit" disabled={researchLoading || !researchQuery.trim()}>
                {#if researchLoading}<span class="spinner-mini"></span>Ищу…{:else}▶ Run{/if}
              </button>
            </form>

            <div class="research-sources" role="tablist">
              <button class="src-tab" class:on={researchActiveSource === 'all'} on:click={() => (researchActiveSource = 'all')} role="tab" aria-selected={researchActiveSource === 'all'}>
                <span>Все</span><span class="src-count">{researchAllResults.length}</span>
              </button>
              <button class="src-tab" class:on={researchActiveSource === 'web'} on:click={() => (researchActiveSource = 'web')} role="tab" aria-selected={researchActiveSource === 'web'}>
                <span class="src-ico">🌐</span><span>Web</span>
                <span class="src-count">{countBySource.web}</span>
                <span class="src-status" data-status={researchProgress.find((p) => p.source === 'web')?.status}>●</span>
              </button>
              <button class="src-tab" class:on={researchActiveSource === 'workspace'} on:click={() => (researchActiveSource = 'workspace')} role="tab" aria-selected={researchActiveSource === 'workspace'}>
                <span class="src-ico">📁</span><span>Workspace</span>
                <span class="src-count">{countBySource.workspace}</span>
                <span class="src-status" data-status={researchProgress.find((p) => p.source === 'workspace')?.status}>●</span>
              </button>
              <button class="src-tab" class:on={researchActiveSource === 'news'} on:click={() => (researchActiveSource = 'news')} role="tab" aria="tab" aria-selected={researchActiveSource === 'news'}>
                <span class="src-ico">📡</span><span>News</span>
                <span class="src-count">{countBySource.news}</span>
                <span class="src-status" data-status={researchProgress.find((p) => p.source === 'news')?.status}>●</span>
              </button>
            </div>
          </div>

          {#if researchError}<div class="research-banner err">⚠ {researchError}</div>{/if}

          <div class="research-grid">
            {#if researchLoading && researchAllResults.length === 0}
              {#each Array(6) as _, i (i)}
                <div class="r-card r-skel">
                  <div class="r-skel-bar r-skel-src"></div>
                  <div class="r-skel-bar r-skel-title"></div>
                  <div class="r-skel-bar r-skel-snippet"></div>
                  <div class="r-skel-bar r-skel-snippet short"></div>
                </div>
              {/each}
            {:else if researchAllResults.length === 0 && !researchLoading}
              <div class="r-empty">
                <div class="r-empty-icon">🔬</div>
                <div class="r-empty-h">Готов к исследованию</div>
                <div class="r-empty-sub">Введи запрос выше или попробуй примеры:</div>
                <div class="r-empty-chips">
                  {#each ['AI agents 2026', 'Rust async runtime', 'SpaceX Starship', 'WebGPU', 'PostgreSQL 17'] as q (q)}
                    <button class="r-empty-chip" on:click={() => { researchQuery = q; runResearch(q); }}>{q}</button>
                  {/each}
                </div>
              </div>
            {/if}
            {#each visibleResults as r (r.id)}
              <article class="r-card" data-source={r.source}>
                <div class="r-card-head">
                  <span class="r-source-badge" style="background: {sourceColor(r.source)}22; color: {sourceColor(r.source)}">
                    <span>{sourceIcon(r.source)}</span>
                    <span>{r.sourceLabel}</span>
                  </span>
                  <span class="r-time">·</span>
                </div>
                <h3 class="r-title">{r.title}</h3>
                {#if r.snippet}<p class="r-snippet">{r.snippet}</p>{/if}
                <div class="r-card-foot">
                  {#if r.url}
                    <span class="r-host" title={r.url}>
                      {(() => { try { return new URL(r.url).host.replace(/^www\./, ''); } catch { return r.url; } })()}
                    </span>
                  {/if}
                  <span class="r-spacer"></span>
                  <div class="r-actions">
                    {#if r.url}
                      <button class="r-action" on:click={() => readMoreUrl(r)} title="Прочитать inline" disabled={readMore?.url === r.url && readMore?.loading}>
                        {readMore?.url === r.url && readMore?.loading ? '⏳' : '📖'} Read
                      </button>
                      <button class="r-action" on:click={() => openFused(r)} title="Открыть в браузере">↗ Open</button>
                    {/if}
                    {#if r.source === 'workspace' && r.path}
                      <button class="r-action primary" on:click={() => openFused(r)} title="Открыть файл">📂 Open</button>
                    {/if}
                  </div>
                </div>
              </article>
            {/each}
          </div>
        </div>
      </div>

      {#if readMore}
        <div class="modal-backdrop" on:click={() => (readMore = null)} role="presentation">
          <div class="modal-card" on:click|stopPropagation role="dialog" aria-label="Read more">
            <div class="modal-head">
              <h3>{readMore.title}</h3>
              <button class="modal-close" on:click={() => (readMore = null)} title="Закрыть" aria-label="Close">×</button>
            </div>
            <div class="modal-host">{readMore.url}</div>
            <div class="modal-body">
              {#if readMore.loading}
                <div class="modal-loading"><span class="spinner-big"></span> Загружаю…</div>
              {:else}
                <pre class="modal-text">{readMore.text || '(пусто)'}</pre>
              {/if}
            </div>
          </div>
        </div>
      {/if}
    {:else if mode === 'media'}
      <div class="media-view">
        <div class="media-head">
          <div>
            <div class="media-h">🖼 Медиа</div>
            <div class="media-sub">
              {imageResults.length} {imageResults.length === 1 ? 'картинка' : (imageResults.length >= 2 && imageResults.length <= 4 ? 'картинки' : 'картинок')}
              · сгенерировано агентом через <code>generate_image</code>
            </div>
          </div>
          <div class="media-actions">
            <input class="media-search" type="text" placeholder="🔍 Поиск по промпту…" bind:value={mediaSearch} />
            {#if imageResults.length > 0}
              <button class="media-btn danger" on:click={clearAllMedia} title="Удалить все">🗑 Очистить</button>
            {/if}
          </div>
        </div>

        {#if imageResults.length === 0}
          <div class="media-empty">
            <div class="media-empty-icon">🖼</div>
            <div class="media-empty-h">Пока пусто</div>
            <div class="media-empty-sub">Попросите агента нарисовать что-нибудь в чате — например:<br /><em>«Нарисуй рыжего кота в скафандре на Марсе»</em></div>
          </div>
        {:else}
          {@const q = mediaSearch.trim().toLowerCase()}
          {@const filtered = q ? imageResults.filter((it) => (it.prompt || '').toLowerCase().includes(q)) : imageResults}
          {#if filtered.length === 0}
            <div class="media-empty"><div class="media-empty-icon">🔍</div><div class="media-empty-h">Ничего не найдено</div><div class="media-empty-sub">По запросу <em>{mediaSearch}</em> нет картинок</div></div>
          {:else}
            <div class="media-grid">
              {#each filtered as r (r.id)}
                <div class="media-card">
                  <button class="media-card-img" on:click={() => (imageLightbox = r)} title="Открыть" aria-label="Open image">
                    <img src={r.dataUrl} alt={r.prompt} loading="lazy" />
                    <div class="media-card-overlay"><span class="media-card-aspect">{r.aspect}</span></div>
                  </button>
                  <div class="media-card-foot">
                    <div class="media-card-prompt">{r.prompt}</div>
                    <div class="media-card-actions">
                      <button class="image-icon-btn" on:click={() => downloadImage(r)} title="Скачать">⤓</button>
                      <button class="image-icon-btn danger" on:click={() => deleteImage(r.id)} title="Удалить">×</button>
                    </div>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </div>
    {:else}
      {#each messages as m, mIdx (m.id)}
        {#if m.role === 'system'}
          <div class="msg msg-system"><div class="body">{m.html ? m.html : m.raw}</div></div>
        {:else}
          {@const isUser = m.role === 'user'}
          {@const prev = mIdx > 0 ? messages[mIdx - 1] : null}
          {@const prevIsTool = !!prev && !prev.role?.startsWith?.('user') && prev.kind && ['tool_use', 'tool_result', 'file_edit', 'file_read', 'image', 'image_loading', 'subagents', 'web_search', 'plan', 'video_frame'].includes(prev.kind)}
          <div class="msg-row" class:user={isUser} class:assistant={!isUser} class:post-tool={prevIsTool}>
            <div class="msg-avatar" aria-hidden="true">{isUser ? '👤' : '🌙'}</div>
            <div class="msg-col">
              <div class="msg-head">
                <span class="msg-name">{isUser ? 'Ты' : 'Luna'}</span>
                {#if !isUser && m.modelTag}<span class="msg-model">{m.modelTag}</span>{/if}
                <span class="msg-time">{formatTime(m.createdAt)}</span>
                {#if m.streaming && !isUser}
                  <span class="msg-status streaming"><span class="msg-dots"><span></span><span></span><span></span></span> печатает</span>
                {:else if !isUser && !m.streaming}
                  <span class="msg-status done" title="Готово">✓</span>
                  <button class="msg-copy" data-msg-id={m.id} type="button" title="Скопировать сообщение" aria-label="Скопировать сообщение">⧉</button>
                {/if}
              </div>
              <div class="msg-bubble-wrap">
                {#if m.thinking && m.thinking.trim() && m.kind !== 'image' && m.kind !== 'tool_use'}
                  {@const thinkingOpen = m.thinkingOpen === true || (m.thinkingOpen == null && streamingId === m.id)}
                  <div class="thinking-block" class:open={thinkingOpen}>
                    <button class="thinking-toggle" on:click={() => toggleThinking(m.id)} aria-expanded={thinkingOpen}>
                      <span class="think-icon">💭</span>
                      <span class="think-label">
                        {#if streamingId === m.id}Думаю<span class="think-spinner"></span>{:else}Думала{/if}
                      </span>
                      <span class="think-chevron">{thinkingOpen ? '▾' : '▸'}</span>
                    </button>
                    {#if thinkingOpen}<div class="think-body">{m.thinking}</div>{/if}
                  </div>
                {/if}
                <div class="msg-bubble" class:streaming={m.streaming && m.kind !== 'image' && m.kind !== 'tool_use'}>
                {#if m.kind === 'image' && m.imageDataUrl}
                  <div class="inline-image" style="aspect-ratio: {(m.imageAspect || '1:1').replace(':', ' / ')};">
                    <button class="inline-image-btn" on:click={() => openLightboxForMsg(m)} title="Открыть" aria-label="Open image">
                      <img src={m.imageDataUrl} alt={m.imagePrompt || ''} loading="lazy" />
                    </button>
                    <div class="inline-image-foot">
                      <div class="inline-image-prompt">{m.imagePrompt}</div>
                      <div class="inline-image-actions">
                        <span class="inline-image-aspect">{m.imageAspect}</span>
                        <button class="image-icon-btn" on:click={() => downloadMsgImage(m)} title="Скачать">⤓</button>
                      </div>
                    </div>
                  </div>
                {:else if m.kind === 'image_loading'}
                  <div class="image-loading" style="aspect-ratio: {(m.imageAspect || '1:1').replace(':', ' / ')};">
                    <div class="image-loading-shimmer"></div>
                    <div class="image-loading-meta">
                      <span class="image-loading-icon">🎨</span>
                      <span class="image-loading-prompt">{m.imagePrompt || 'генерирую…'}</span>
                      <span class="image-loading-aspect">{m.imageAspect || '1:1'}</span>
                    </div>
                  </div>
                {:else if m.kind === 'subagents'}
                  <div class="subagents">
                    <div class="subagents-head">
                      <span class="subagents-icon">{m.subKind === 'images' ? '🎨' : '🔬'}</span>
                      <span class="subagents-title">{m.subKind === 'images' ? 'Параллельная генерация' : 'Параллельный research'} · {m.subagents?.length ?? 0} субагентов</span>
                      {#if m.toolStatus === 'pending'}<span class="subagents-spinner"></span>{/if}
                    </div>
                    <div class="subagents-grid">
                      {#each m.subagents || [] as s (s.id)}
                        <div class="subagent-card" class:done={s.status === 'ok'} class:error={s.status === 'error'}>
                          <div class="subagent-head">
                            <span class="subagent-status">{#if s.status === 'pending'}<span class="subagent-mini-spinner"></span>{:else if s.status === 'ok'}✓{:else}⚠{/if}</span>
                            <span class="subagent-title">{s.title}</span>
                          </div>
                          {#if s.dataUrl}
                            <button
                              class="subagent-img-btn"
                              style="aspect-ratio: {(s.aspect || '1:1').replace(':', ' / ')};"
                              on:click={() => openLightboxForSub(s)}
                              title="Открыть"
                              aria-label="Open image"
                            >
                              <img src={s.dataUrl} alt={s.title} loading="lazy" />
                            </button>
                          {/if}
                          {#if s.result && s.result.length > 0}
                            <ul class="subagent-list">
                              {#each s.result.slice(0, 2) as r}
                                <li><a href={r.url} target="_blank" rel="noopener">{r.title}</a></li>
                              {/each}
                            </ul>
                          {/if}
                        </div>
                      {/each}
                    </div>
                  </div>
                {:else if m.kind === 'plan'}
                  {@const done = (m.planSteps || []).filter((s) => s.status === 'done').length}
                  {@const total = (m.planSteps || []).length}
                  {@const inProgress = (m.planSteps || []).some((s) => s.status === 'in_progress')}
                  <div class="plan-card" class:plan-done={total > 0 && done === total} class:plan-active={inProgress}>
                    <div class="plan-head">
                      <span class="plan-icon">{done === total && total > 0 ? '✅' : (inProgress ? '⏳' : '📋')}</span>
                      <span class="plan-title">{m.planTitle || 'Plan'}</span>
                      <span class="plan-counter">{done} / {total}</span>
                    </div>
                    <div class="plan-progress">
                      <div class="plan-progress-fill" style="width: {total > 0 ? (done / total) * 100 : 0}%"></div>
                    </div>
                    <ol class="plan-steps">
                      {#each m.planSteps || [] as s (s.id)}
                        <li class="plan-step plan-step-{s.status}">
                          <span class="plan-step-marker" aria-hidden="true">
                            {#if s.status === 'done'}✓{:else if s.status === 'in_progress'}⏳{:else if s.status === 'error'}⚠{:else}○{/if}
                          </span>
                          <span class="plan-step-title">{s.title}</span>
                          {#if s.note}<span class="plan-step-note">— {s.note}</span>{/if}
                        </li>
                      {/each}
                    </ol>
                  </div>
                {:else if m.kind === 'web_search'}
                  {@const count = m.webResults?.length ?? 0}
                  {@const expanded = m.webExpanded ?? (count > 0 && count <= 3)}
                  {#if count === 0 && m.toolStatus !== 'pending'}
                    <!-- Empty result: collapse to a single tiny pill. -->
                    <div class="web-search-empty" title={m.webQuery || ''}>
                      <span class="web-search-icon">🌐</span>
                      <span class="web-search-empty-text">No results for “{m.webQuery || ''}”</span>
                    </div>
                  {:else}
                    <div class="web-search" class:web-search-compact={!expanded}>
                      <button
                        type="button"
                        class="web-search-head"
                        on:click={() => { messages = messages.map((mm) => mm.id === m.id ? { ...mm, webExpanded: !expanded } : mm); }}
                        aria-expanded={expanded}
                      >
                        <span class="web-search-icon">🌐</span>
                        <span class="web-search-title">Web search</span>
                        {#if m.webQuery}<span class="web-search-query">“{m.webQuery}”</span>{/if}
                        <span class="web-search-count">{count} {count === 1 ? 'source' : 'sources'}</span>
                        {#if m.toolStatus === 'pending'}<span class="web-search-spinner"></span>{/if}
                        <span class="web-search-chevron">{expanded ? '▾' : '▸'}</span>
                      </button>
                      {#if expanded && count > 0}
                        <ul class="web-search-list">
                          {#each (m.webResults || []).slice(0, 5) as r, i (i)}
                            <li class="web-search-item">
                              <a class="web-search-link" href={r.url} target="_blank" rel="noopener">
                                <span class="web-search-link-title">{r.title || r.url}</span>
                                <span class="web-search-link-host">{r.host || r.url}</span>
                              </a>
                              {#if r.snippet}<p class="web-search-snippet">{r.snippet}</p>{/if}
                            </li>
                          {/each}
                          {#if count > 5}
                            <li class="web-search-more">… ещё {count - 5} источников</li>
                          {/if}
                        </ul>
                      {/if}
                    </div>
                  {/if}
                {:else if m.kind === 'tool_use'}
                  {@const argsOpen = m.toolArgsOpen ?? (m.toolStatus === 'pending')}
                  <div class="tool-pill" class:error={m.toolStatus === 'error'} class:pending={m.toolStatus === 'pending'} class:open={argsOpen}>
                    <button class="tool-pill-head" on:click={() => { messages = messages.map((mm) => mm.id === m.id ? { ...mm, toolArgsOpen: !argsOpen } : mm); }} aria-expanded={argsOpen}>
                      <span class="tool-emoji" aria-hidden="true">{toolIcon(m.toolName)}</span>
                      <span class="tool-icon">
                        {#if m.toolStatus === 'pending'}<span class="tool-spinner"></span>{:else if m.toolStatus === 'ok'}✓{:else if m.toolStatus === 'error'}⚠{/if}
                      </span>
                      <span class="tool-name">{m.toolName || 'tool'}</span>
                      {#if m.toolStatus === 'pending'}<span class="tool-status pending">работаю…</span>{/if}
                      {#if m.toolArgs}<span class="tool-args-preview">{summarizeToolArgs(m.toolArgs)}</span>{/if}
                      <span class="tool-chevron">{argsOpen ? '▾' : '▸'}</span>
                    </button>
                    {#if argsOpen && m.toolArgs}<pre class="tool-args">{m.toolArgs}</pre>{/if}
                    {#if m.toolError}<div class="tool-err-text">⚠ {m.toolError}</div>{/if}
                  </div>
                {:else if m.kind === 'ask_user'}
                  <div class="ask-user-card" class:answered={!!m.askAnswer}>
                    <div class="ask-user-head">
                      <span class="ask-user-emoji" aria-hidden="true">❓</span>
                      <span class="ask-user-label">Агент спрашивает</span>
                    </div>
                    <div class="ask-user-q">{m.askQuestion}</div>
                    {#if !m.askAnswer}
                      <div class="ask-user-options">
                        {#each m.askOptions ?? [] as opt (opt)}
                          <button
                            type="button"
                            class="ask-option"
                            on:click={() => answerAskUser(m.id, opt)}
                            title="Отправить как ответ"
                          >{opt}</button>
                        {/each}
                        <button
                          type="button"
                          class="ask-option ask-option-freetext"
                          on:click={() => focusComposerForAskUser(m.askQuestion)}
                          title="Ответить своим текстом"
                        >✏️ Свой ответ</button>
                      </div>
                    {:else}
                      <div class="ask-user-answered">
                        <span class="ask-user-answered-emoji" aria-hidden="true">↩</span>
                        <span class="ask-user-answered-text">{m.askAnswer}</span>
                      </div>
                    {/if}
                  </div>
                {:else if m.kind === 'file_edit'}
                  {@const state = m.fileEditState || 'pending'}
                  <div class="edit-card" class:accepted={state === 'accepted'} class:rejected={state === 'rejected'}>
                    <div class="edit-card-head">
                      <span class="edit-card-icon">{state === 'rejected' ? '↩' : (state === 'pending' ? '✎' : '✓')}</span>
                      <span class="edit-card-path" title={m.filePath}>{m.filePath}</span>
                      <span class="edit-card-state">{state === 'pending' ? 'применяю…' : (state === 'rejected' ? 'откачено' : 'принято')}</span>
                    </div>
                    {#if m.fileDiff}
                      <pre class="diff-body">{m.fileDiff}</pre>
                    {:else}
                      <div class="edit-card-pending">применяю…</div>
                    {/if}
                    {#if state === 'accepted'}
                      <div class="edit-card-actions">
                        <button class="ea-btn" disabled>✓ принято</button>
                        <button class="ea-btn reject" on:click={() => m.fileEditId && rejectFileEdit(m.fileEditId, m.id)} title="Откатить изменение и восстановить файл из бэкапа">✗ Откатить</button>
                      </div>
                    {:else if state === 'rejected'}
                      <div class="edit-card-actions">
                        <span class="ea-note">Файл восстановлен из бэкапа</span>
                      </div>
                    {/if}
                  </div>
                {:else if m.kind === 'file_read'}
                  <div class="read-card">
                    <button class="read-card-head" on:click={() => toggleFileRead(m.id)}>
                      <span class="read-card-icon">📄</span>
                      <span class="read-card-path" title={m.filePath}>{m.filePath}</span>
                      <span class="read-card-meta">{m.fileReadLines ?? 0} строк · {m.fileReadBytes ?? 0} B</span>
                      <span class="read-card-chevron">{m.fileReadOpen ? '▾' : '▸'}</span>
                    </button>
                    {#if m.fileReadOpen && m.fileReadContent}
                      <pre class="read-card-body">{m.fileReadContent}</pre>
                    {/if}
                  </div>
                {:else if m.kind === 'video_frame' && m.videoFrameUrl}
                  <div class="read-card">
                    <div class="read-card-head" style="cursor: default;">
                      <span class="read-card-icon">🎥</span>
                      <span class="read-card-path">
                        {m.videoFrameKind === 'observe_now' ? 'Снимок экрана' : 'Последний кадр'}
                      </span>
                      <span class="read-card-meta">
                        {m.videoFrameMeta?.width ?? '?'}×{m.videoFrameMeta?.height ?? '?'} ·
                        {Math.round((m.videoFrameMeta?.bytes ?? 0) / 1024)} KB ·
                        кадр #{m.videoFrameMeta?.seq ?? '?'}
                      </span>
                    </div>
                    <img
                      class="video-frame-body"
                      src={m.videoFrameUrl}
                      alt="Кадр экрана, просмотренный моделью"
                    />
                  </div>
                {:else}
                  <div class="body">{@html m.html}</div>
                  {#if m.streaming}<span class="caret">▍</span>{/if}
                {/if}
                </div>
              </div>
              {#if !isUser && !m.streaming && (m.toolCount ?? 0) > 0}
                <div class="msg-foot">
                  <span class="msg-tools" title="Инструменты вызванные моделью">
                    ⌐ {m.toolCount} {m.toolCount === 1 ? 'инструмент' : (m.toolCount < 5 ? 'инструмента' : 'инструментов')}
                  </span>
                </div>
              {/if}
            </div>
          </div>
        {/if}
      {/each}
    {/if}
  </main>

  {#if errorBanner}<div class="error">{errorBanner}</div>{/if}

  <!-- Footer composer is only rendered outside Code mode. The Code-mode
       input lives inside the center column (see `cc-input-wrap` above) so
       the user gets one consistent textarea regardless of mode. -->
  {#if mode !== 'code'}
  <footer class="composer">
    <div class="chat-history-bar" role="toolbar" aria-label="История чатов">
      <button
        type="button"
        class="hist-btn"
        on:click={startNewChat}
        title="Начать новый чат (текущий будет сохранён в историю)"
        aria-label="Начать новый чат"
      >
        <span aria-hidden="true">🆕</span>
        <span>Новый чат</span>
      </button>
      <button
        type="button"
        class="hist-btn danger"
        on:click={nukeAllChats}
        title="Удалить всю историю (необратимо)"
        aria-label="Очистить всю историю"
      >
        <span aria-hidden="true">🗑</span>
        <span>Очистить</span>
      </button>
      <span class="hist-status" class:saving={chatSaving} aria-live="polite">
        {#if chatSaving}💾 Сохранение…{:else if chatId}💾 История включена{/if}
      </span>
    </div>
    <div class="input-shell" class:focused={inputFocused} class:has-text={inputText.length > 0} class:disabled={!hasMinimax} class:multitask={multitask} class:busy={busy && mode === 'chat'}>
      <div class="input-bg"></div>
      <textarea
        bind:this={inputEl}
        bind:value={inputText}
        on:keydown={onInputKey}
        on:input={autosize}
        on:focus={() => (inputFocused = true)}
        on:blur={() => (inputFocused = false)}
        placeholder={mode === 'research'
          ? 'Research — автоподбор. Нажми ↑ или Enter для обновления…'
          : (hasMinimax
              ? (multitask
                  ? '⚡ Multitask: попроси сравнить темы или сгенерить несколько картинок — агент запустит субагентов параллельно…'
                  : 'Спроси у Луны — она умеет рисовать, если попросить…')
              : 'Сначала введи MiniMax-ключ в ⚙ Settings…')}
        rows="1"
        disabled={mode === 'chat' && !hasMinimax}
        spellcheck="true"
        autocomplete="off"
      ></textarea>
      <div class="input-actions">
        <button
          class="icon-btn multitask-btn"
          class:on={multitask}
          disabled={!hasMinimax}
          on:click={toggleMultitask}
          title={multitask
            ? 'Multitask: ON — ассистент будет использовать parallel_research / parallel_generate_images. Клик чтобы выключить.'
            : 'Multitask: OFF — клик включит параллельный режим для следующего запроса (parallel_research, parallel_generate_images).'}
          aria-label="Toggle multitask mode"
          aria-pressed={multitask}
        >
          <span class="multitask-glyph" aria-hidden="true">⚡</span>
        </button>
        <button class="icon-btn" disabled={!hasMinimax} title="Агент сам нарисует, если попросить" aria-label="Image hint" on:click={() => inputEl?.focus()}>🎨</button>
        <button class="icon-btn" class:active={voiceState === 'recording'} class:transcribing={voiceState === 'transcribing'} class:error={voiceState === 'error'} on:click={toggleVoice} disabled={voiceState === 'transcribing' || !hasMinimax} title={voiceState === 'recording' ? 'Остановить запись (Ctrl+Space)' : 'Голосовой ввод (Ctrl+Space)'} aria-label="Toggle voice input">
          {#if voiceState === 'recording'}<span class="rec-dot"></span>{:else if voiceState === 'transcribing'}<span class="spinner"></span>{:else}🎙{/if}
        </button>
        <button class="send-btn" class:active={(mode === 'chat' ? (inputText.trim().length > 0 && hasMinimax) : (!researchLoading && userInterests.length > 0))} on:click={send} disabled={mode === 'chat' ? (!inputText.trim() || !hasMinimax) : (researchLoading || userInterests.length === 0)} title={busy && mode === 'chat' ? 'Отменить текущий ответ и отправить новое сообщение' : (mode === 'research' ? 'Обновить исследование' : 'Отправить (Enter)')} aria-label="Send">
          {#if busy && mode === 'chat'}
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><line x1="5" y1="5" x2="19" y2="19"/><line x1="19" y1="5" x2="5" y2="19"/></svg>
          {:else}
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>
          {/if}
        </button>
      </div>
    </div>
    <div class="hint" class:focused={inputFocused}>
      {#if voiceError}<span class="voice-err">🎙 {voiceError}</span>{/if}
      {#if multitask}
        <button
          type="button"
          class="hint-pill multitask-pill"
          on:click={toggleMultitask}
          title="Multitask включён — клик чтобы выключить"
          aria-label="Disable multitask mode"
        >⚡ Multitask</button>
      {/if}
      <span class="hint-pill model-pill">MiniMax</span>
      <span class="hint-pill model-pill">{selectedModel.label}</span>
      {#if busy && mode === 'chat'}
        <button
          type="button"
          class="busy-pill"
          on:click={cancelCurrent}
          title="Остановить текущий ответ (или просто нажми Enter с новым сообщением)"
          aria-label="Остановить ответ"
        >
          <span class="busy-spinner" aria-hidden="true"></span>
          <span>печатает…</span>
          <span class="busy-stop" aria-hidden="true">✕</span>
        </button>
      {/if}
      <span class="context-wrap">
        <button
          type="button"
          class="context-btn {contextBucket(contextInfo.pct)}"
          bind:this={contextBtnEl}
          on:click|stopPropagation={toggleContextPopover}
          title="Контекст: {formatTokens(contextInfo.used)} / {formatTokens(contextInfo.window)} токенов — клик покажет подробности"
          aria-haspopup="true"
          aria-expanded={contextPopover}
        >
          <span class="context-ring" aria-hidden="true">
            <svg viewBox="0 0 16 16" width="14" height="14">
              <circle cx="8" cy="8" r="6.5" class="ring-bg" />
              <circle cx="8" cy="8" r="6.5" class="ring-fg" style="stroke-dasharray: {40.84 * contextInfo.pct / 100} 40.84" />
            </svg>
          </span>
          <span class="context-pct">{contextInfo.pct}%</span>
        </button>
        {#if contextPopover}
        <div
          class="context-pop"
          role="dialog"
          aria-label="Context usage"
          bind:this={contextPopEl}
          on:click|stopPropagation
          on:mousedown|stopPropagation
        >
          <div class="context-pop-head">
            <span class="context-pop-title">Контекст</span>
            <span class="context-pop-model">{selectedModel.label}</span>
          </div>
          <div class="context-tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={contextView === 'summary'}
              class="context-tab"
              class:active={contextView === 'summary'}
              on:click={() => (contextView = 'summary')}
            >📊 Сводка</button>
            <button
              type="button"
              role="tab"
              aria-selected={contextView === 'content'}
              class="context-tab"
              class:active={contextView === 'content'}
              on:click={() => (contextView = 'content')}
              title="Показать то, что реально уходит в модель"
            >📝 Содержимое <span class="context-tab-count">{realContext.length}</span></button>
          </div>
          {#if contextView === 'summary'}
            <div class="context-bar-row">
              <div class="context-bar">
                <div class="context-bar-fill {contextBucket(contextInfo.pct)}" style="width: {contextInfo.pct}%"></div>
              </div>
              <span class="context-bar-pct">{contextInfo.pct}%</span>
            </div>
            <div class="context-numbers">
              <span><b>{formatTokens(contextInfo.used)}</b> использовано</span>
              <span class="context-numbers-sep">/</span>
              <span><b>{formatTokens(contextInfo.window)}</b> окно</span>
              <span class="context-numbers-sep">·</span>
              <span class="context-remaining">осталось <b>{formatTokens(Math.max(0, contextInfo.window - contextInfo.used))}</b></span>
            </div>

            <div class="context-bd-title">По типам</div>
            <div class="context-bd-list">
              {#each contextBreakdown as g (g.kind)}
                <div class="context-bd-row" title="{g.count} блок(ов) · {g.tokens} tok">
                  <span class="context-bd-dot" style="background: {g.color}"></span>
                  <span class="context-bd-label">{g.label}</span>
                  <span class="context-bd-mini">
                    <span class="context-bd-mini-fill" style="width: {g.pct}%; background: {g.color}"></span>
                  </span>
                  <span class="context-bd-tok">{formatTokens(g.tokens)}</span>
                  <span class="context-bd-pct">{g.pct}%</span>
                </div>
              {/each}
            </div>

            <div class="context-cost" title="Оценка по публичным тарифам MiniMax; выход считается только по последним ответам ассистента">
              <span class="context-cost-label">≈ ${costEstimate.total.toFixed(4)}</span>
              <span class="context-cost-sep">·</span>
              <span class="context-cost-sub">in ${costEstimate.in.toFixed(4)}</span>
              <span class="context-cost-sep">·</span>
              <span class="context-cost-sub">out ${costEstimate.out.toFixed(4)}</span>
            </div>

            <div class="context-bd-title">Последние сообщения</div>
            <div class="context-breakdown">
              {#each contextInfo.perMessage.slice(-6).reverse() as bm (bm.id)}
                <div class="context-row" title={bm.preview}>
                  <span class="context-row-role {bm.role}">{bm.role}</span>
                  <span class="context-row-preview">{bm.preview}</span>
                  <span class="context-row-tokens">{formatTokens(bm.tokens)}</span>
                </div>
              {/each}
              {#if contextInfo.perMessage.length > 6}
                <div class="context-row context-row-more">… ещё {contextInfo.perMessage.length - 6}</div>
              {/if}
            </div>
          {:else}
            <div class="context-content">
              <div class="context-content-meta">
                <span><b>{realContext.length}</b> блок(ов)</span>
                <span class="context-content-meta-sep">·</span>
                <span>~<b>{formatTokens(realContext.reduce((s, it) => s + it.tokens, 0))}</b> токенов</span>
                <span class="context-content-spacer"></span>
                <button
                  type="button"
                  class="context-copy"
                  class:ok={contextCopyHint.startsWith('✓')}
                  class:err={contextCopyHint.startsWith('✕')}
                  on:click={copyRealContext}
                  title="Скопировать весь видимый контекст в буфер обмена"
                >{contextCopyHint || '📋 Всё'}</button>
              </div>
              <div class="context-content-list">
                {#each realContext as it (it.id)}
                  <div
                    class="context-content-item"
                    class:system={it.kind === 'system'}
                    class:highlight={contextItemHighlight === it.id}
                    on:click={() => jumpToContextItem(it)}
                    role="button"
                    tabindex="0"
                    on:keydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); jumpToContextItem(it); } }}
                    title="Клик — прыгнуть к сообщению в чате"
                  >
                    <div class="context-content-head">
                      <span class="context-row-role {it.role}">{it.label}</span>
                      <span class="context-content-tokens">~{formatTokens(it.tokens)} tok</span>
                      <span class="context-content-chars">{it.chars} зн.</span>
                      <span class="context-content-spacer"></span>
                      <button
                        type="button"
                        class="context-item-copy"
                        class:ok={contextItemCopied === it.id}
                        on:click|stopPropagation={() => copyContextItem(it)}
                        title="Скопировать этот блок"
                        aria-label="Скопировать"
                      >{contextItemCopied === it.id ? '✓' : '⧉'}</button>
                    </div>
                    <pre class="context-content-text">{it.content}</pre>
                  </div>
                {/each}
                {#if realContext.length === 0}
                  <div class="context-content-empty">Контекст пуст — отправьте первое сообщение, и здесь появится его полный текст.</div>
                {/if}
              </div>
              <div class="context-content-foot">Клик по блоку — прокрутка к сообщению в чате. <kbd>⧉</kbd> копирует один блок, «📋 Всё» — весь контекст. Системный промпт формируется в Rust, его размер показан приблизительно (≈600 токенов).</div>
            </div>
          {/if}
          <div class="context-pop-actions">
            <button class="context-action primary" on:click={() => { contextPopover = false; startNewChat(); }} title="Начать новый чат (текущий сохранится в историю)" type="button">🆕 Новый чат</button>
            <button class="context-action" on:click={clearContext} title="Очистить контекст в текущем чате" type="button">↺ Очистить</button>
            <span class="context-hint">Esc — закрыть · оценка приблизительная</span>
          </div>
        </div>
      {/if}
      </span>
      <span class="hint-spacer"></span>
      <span class="hint-keys">
        <kbd>Enter</kbd> отправить
        {#if busy && mode === 'chat'}
          <span class="hint-sep">·</span>
          <span class="hint-steer">Enter во время ответа — отменит и пошлёт новое</span>
        {:else}
          <span class="hint-sep">·</span>
          <kbd>Shift</kbd>+<kbd>Enter</kbd> перенос
        {/if}
      </span>
    </div>

    <!-- Floating "scroll to bottom" button — appears when user scrolls up. -->
    {#if !stickToBottom && (mode === 'chat' || mode === 'media')}
      <button class="scroll-bottom" on:click={jumpToBottom} title="К последнему сообщению" aria-label="Scroll to bottom">
        ↓
      </button>
    {/if}

    <div class="voice-bar">
      <button class="model-chip" class:warn={!activeModelId} on:click={() => (modelPanelOpen = !modelPanelOpen)} title={modelsDir ? `Whisper model\n${modelsDir}` : 'Whisper model'}>🎙 {activeModelId ? activeModelId : 'no model'}</button>
      {#if downloadProgress}<span class="download-info" class:done={downloadPct === 100}>{downloadProgress}</span>{/if}
    </div>
  </footer>

  <!-- Plan-mode composer: shown only when `mode === 'plan'`. The
       regular chat composer above stays in the DOM but visually
       collapses (its textarea is hidden) so we don't have to round-
       trip the focus state. -->
  {#if mode === 'plan'}
    <footer class="composer plan-composer">
      <div class="plan-form">
        <input
          class="plan-title-input"
          type="text"
          bind:value={planTitle}
          bind:this={planTitleInputEl}
          placeholder="Название плана (например: «Рефакторинг авторизации»)"
          on:keydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); planStepsInputEl?.focus(); } }}
        />
        <textarea
          class="plan-steps-input"
          bind:value={planStepsText}
          bind:this={planStepsInputEl}
          rows="6"
          placeholder={'1. первый шаг\n2. второй шаг\n3. третий шаг'}
          spellcheck="false"
          on:keydown={onPlanStepsKeydown}
        ></textarea>
        <div class="plan-form-bar">
          <span class="plan-hint">
            {#if busy}
              ⏳ Агент работает — кнопки заблокированы
            {:else}
              Enter — новый шаг · Shift+Enter — перенос
            {/if}
          </span>
          <div class="plan-buttons">
            <button
              type="button"
              class="plan-save-btn"
              on:click={savePlan}
              disabled={!canSavePlan}
              title="Сохранить план в сайдбар (без отправки агенту)"
            >Сохранить план</button>
            <button
              type="button"
              class="plan-run-btn"
              on:click={runPlan}
              disabled={!canRunPlan || busy}
              title={busy ? 'Дождитесь завершения текущего ответа' : 'Сохранить и отправить агенту'}
            >▶ Запустить</button>
          </div>
        </div>
      </div>
    </footer>
  {/if}
  {/if}

  {#if modelPanelOpen}
    <div class="model-panel" role="dialog" aria-label="Whisper models">
      <div class="model-panel-head">
        <strong>Whisper models</strong>
        <button class="link" on:click={() => (modelPanelOpen = false)}>×</button>
      </div>
      <p class="muted">Хранятся в <code>%APPDATA%\com.luna.agent\whisper-models\</code> (≈ AppData на macOS/Linux).</p>
      <ul>
        {#each whisperModels as m}
          <li class:active-model={m.active} class:installing={installingId === m.id}>
            <div class="model-line">
              <span class="model-id">{m.id}</span>
              <span class="model-tier">{m.tier}</span>
              <span class="model-size">{m.sizeMb} MB</span>
            </div>
            <div class="model-action">
              {#if m.active}<span class="badge">active</span>
              {:else if m.installed}<button class="secondary" on:click={() => setActiveWhisperModel(m.id)}>Use</button>
              {:else if m.fitsInMemory}<button class="primary" disabled={installingId !== null} on:click={() => installWhisperModel(m.id)}>{#if installingId === m.id}Installing…{:else}Install{/if}</button>
              {:else}<span class="muted">needs {m.requiredMemoryMb} MB</span>{/if}
            </div>
          </li>
        {/each}
      </ul>
      {#if downloadProgress}
        <div class="download-row">
          <div class="progress-bar"><div class="fill" style="width: {downloadPct ?? 0}%"></div></div>
          <span class="muted">{downloadProgress}</span>
        </div>
      {/if}
    </div>
  {/if}

  {#if autoModalOpen}
    <div class="modal-bg" role="dialog" aria-modal="true" aria-label="Voice model required">
      <div class="modal model-modal">
        <h3>🎙 Нужна модель Whisper</h3>
        <p>Чтобы распознавать речь, Luna нужно скачать модель Whisper. <strong>base</strong> (142&nbsp;МБ) — хороший баланс скорости и качества для русского и английского.</p>
        {#if downloadProgress && installingId}
          <div class="download-row" style="margin: 12px 0 4px;">
            <div class="progress-bar"><div class="fill" style="width: {downloadPct ?? 0}%"></div></div>
            <span class="muted">{downloadProgress}</span>
          </div>
        {/if}
        <div class="modal-actions">
          <button class="primary" on:click={() => installWhisperModel('base')} disabled={installingId !== null}>{#if installingId === 'base'}Скачиваю…{:else}Скачать base (142 МБ){/if}</button>
          <button class="secondary" on:click={postponeModel} disabled={installingId !== null}>Отложить до следующего нажатия</button>
          <button class="link" on:click={dismissModel} title="Закрыть" disabled={installingId !== null}>×</button>
        </div>
      </div>
    </div>
  {/if}

  {#if imageLightbox}
    <div class="image-lightbox" role="dialog" aria-modal="true" aria-label="Image preview" on:click={() => (imageLightbox = null)} on:keydown={(e) => { if (e.key === 'Escape') imageLightbox = null; }}>
      <button class="lightbox-close" on:click={() => (imageLightbox = null)} title="Закрыть (Esc)">×</button>
      <img src={imageLightbox.dataUrl} alt={imageLightbox.prompt} on:click|stopPropagation />
      <div class="lightbox-foot" on:click|stopPropagation>
        <div class="lightbox-prompt">{imageLightbox.prompt}</div>
        <div class="lightbox-actions">
          <span class="lightbox-aspect">{imageLightbox.aspect}</span>
          <button class="secondary" on:click={() => downloadImage(imageLightbox)}>⤓ Скачать</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- New-project modal (Code mode) -->
  {#if showNewProject}
    <div class="modal-backdrop" on:click={() => (showNewProject = false)} role="presentation">
      <div class="modal" on:click|stopPropagation role="dialog" aria-labelledby="np-title">
        <h2 id="np-title">🆕 Новый проект</h2>
        <div class="form-row">
          <label for="np-name">Имя проекта</label>
          <input
            id="np-name"
            type="text"
            bind:value={npName}
            placeholder="my-app"
            on:keydown={(e) => e.key === 'Enter' && submitNewProject()}
          />
          <small class="muted">латиница, цифры, '.', '_', '-'</small>
        </div>
        <div class="form-row">
          <label>Шаблон</label>
          <div class="templates">
            {#each npTemplates as t (t.id)}
              <label class="tpl" class:active={npTemplateId === t.id}>
                <input type="radio" bind:group={npTemplateId} value={t.id} />
                <div class="tpl-body">
                  <div class="tpl-label">{t.label}</div>
                  <div class="tpl-desc">{t.description}</div>
                </div>
              </label>
            {/each}
            {#if npTemplates.length === 0}
              <div class="muted">Загрузка…</div>
            {/if}
          </div>
        </div>
        <div class="form-row">
          <label for="np-parent">Папка для создания</label>
          <input id="np-parent" type="text" bind:value={npParent} placeholder="C:\Users\you\Projects" />
          <small class="muted">Проект: <code>{npParent || '...'}/{npName || '...'}</code></small>
        </div>
        {#if npError}
          <div class="error">⚠ {npError}</div>
        {/if}
        <div class="modal-actions">
          <button class="ghost" on:click={() => (showNewProject = false)} disabled={npBusy}>Отмена</button>
          <button class="primary" on:click={submitNewProject} disabled={npBusy || !npName.trim()}>
            {npBusy ? 'Создаю…' : 'Создать'}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .chat { height: 100%; min-height: 0; display: flex; flex-direction: column; background: #0f1217; color: #e6e8eb; }

  .bar {
    display: flex; align-items: center; justify-content: space-between;
    padding: 6px 10px; background: #181b22; border-bottom: 1px solid #2c313a;
    flex: 0 0 auto; gap: 8px;
  }
  .left { display: flex; align-items: baseline; gap: 10px; min-width: 0; }
  .middle { display: flex; gap: 4px; }
  .right { display: flex; align-items: center; gap: 4px; }

  .title { font-size: 14px; font-weight: 600; }
  .sub { font-size: 11px; color: #8a93a6; }
  .sub.ok { color: #7ac98a; }
  .sub.miss { color: #f5b56b; }

  .seg {
    background: transparent; color: #b6bcc7; border: 1px solid transparent;
    padding: 5px 10px; font-size: 12px; font-weight: 500; border-radius: 6px; cursor: pointer;
  }
  .seg:hover { color: #e6e8eb; background: #252932; }
  .seg.on { color: #e6e8eb; background: #0f1217; border-color: #2c313a; }

  .ico {
    background: transparent; border: 1px solid #2c313a; color: #b6bcc7;
    width: 30px; height: 30px; border-radius: 6px; cursor: pointer; font-size: 14px; padding: 0;
  }
  .ico:hover { color: #e6e8eb; background: #252932; }
  .ico.danger:hover { color: #e07a7a; border-color: #5a3030; }

  .model-pick {
    display: inline-flex; align-items: center; gap: 6px;
    background: transparent; border: 1px solid #2c313a; border-radius: 6px;
    padding: 2px 8px; height: 30px; color: #cfd3da; font-size: 12px; cursor: pointer;
  }
  .model-pick:hover { background: #252932; border-color: #3a414b; }
  .model-pick:focus-within { border-color: #c9a0a0; box-shadow: 0 0 0 2px rgba(201,160,160,0.15); }
  .model-label { color: #6c7280; font-size: 10px; text-transform: uppercase; letter-spacing: 0.5px; }
  .model-pick select {
    background: transparent; border: none; color: #e6e8eb;
    font-family: inherit; font-size: 12px; outline: none; padding: 2px 4px 2px 0; max-width: 240px; cursor: pointer;
  }
  .model-pick select option { background: #1c1f26; color: #e6e8eb; }

  .scroll {
    flex: 1 1 0;
    min-height: 0;
    overflow-y: auto;
    padding: 20px;
    display: flex; flex-direction: column; gap: 10px;
    scrollbar-width: thin; scrollbar-color: #2c313a transparent;
    overscroll-behavior: contain;
  }
  .scroll::-webkit-scrollbar { width: 8px; }
  .scroll::-webkit-scrollbar-thumb { background: #2c313a; border-radius: 4px; }

  /* ---- Message rows: avatar + bubble ---- */
  .msg-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 4px 0;
    min-width: 0;
  }
  .msg-row.user { flex-direction: row-reverse; }
  .msg-avatar {
    flex: 0 0 30px;
    width: 30px; height: 30px;
    border-radius: 50%;
    display: flex; align-items: center; justify-content: center;
    font-size: 15px;
    line-height: 1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-1);
    user-select: none;
    flex-shrink: 0;
  }
  .msg-row.user .msg-avatar { background: var(--accent-soft); border-color: var(--accent); }
  .msg-col {
    flex: 1; min-width: 0;
    max-width: calc(100% - 50px);
    display: flex; flex-direction: column;
    gap: 4px;
  }
  .msg-row.user .msg-col { align-items: flex-end; }
  .msg-head {
    display: flex; align-items: baseline;
    gap: 8px;
    font-size: 11px;
    color: var(--text-faint);
    padding: 0 4px;
    flex-wrap: wrap;
  }
  .msg-name { font-weight: 600; color: var(--text); font-size: 12px; letter-spacing: 0.1px; }
  .msg-model {
    padding: 1px 7px; border-radius: 999px;
    font-size: 9px; text-transform: uppercase; letter-spacing: 0.5px;
    background: var(--accent-soft); color: var(--accent);
    font-weight: 600;
  }
  .msg-time { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; opacity: 0.7; }
  .msg-status { display: inline-flex; align-items: center; gap: 4px; font-size: 10px; opacity: 0.7; }
  .msg-status.streaming { color: var(--accent); }
  .msg-status.done { color: var(--success); opacity: 0.7; }
  .msg-dots { display: inline-flex; gap: 2px; }
  .msg-dots span {
    width: 3px; height: 3px; border-radius: 50%;
    background: var(--accent);
    animation: msg-dot 1.2s ease-in-out infinite;
  }
  .msg-dots span:nth-child(2) { animation-delay: 0.15s; }
  .msg-dots span:nth-child(3) { animation-delay: 0.30s; }
  @keyframes msg-dot {
    0%, 60%, 100% { opacity: 0.3; transform: translateY(0); }
    30% { opacity: 1; transform: translateY(-2px); }
  }
  .msg-bubble {
    padding: 10px 14px;
    border-radius: 14px;
    font-size: 14px; line-height: 1.55;
    word-wrap: break-word; overflow-wrap: anywhere; word-break: break-word;
    user-select: text;
    min-width: 0;
    box-sizing: border-box;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-1);
    position: relative;
  }
  .msg-row.user .msg-bubble {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--text);
    border-bottom-right-radius: 4px;
  }
  .msg-row.assistant .msg-bubble {
    border-bottom-left-radius: 4px;
  }
  /* Assistant text that follows a tool card gets a thin accent stripe
     on the left + a soft tinted background. Makes the
     "text → tool → text" pattern visually obvious. */
  .msg-row.assistant.post-tool {
    position: relative;
    margin-top: 2px;
  }
  .msg-row.assistant.post-tool::before {
    content: '';
    position: absolute;
    left: 14px;
    top: 14px;
    bottom: 14px;
    width: 2px;
    background: linear-gradient(180deg, rgba(99, 102, 241, 0.55), rgba(99, 102, 241, 0.15));
    border-radius: 2px;
  }
  .msg-row.assistant.post-tool .msg-bubble {
    background: rgba(99, 102, 241, 0.06);
    border-color: rgba(99, 102, 241, 0.20);
    margin-left: 18px;
  }
  .msg-row.assistant.post-tool .msg-head {
    margin-left: 18px;
  }
  .msg-bubble.streaming {
    box-shadow: 0 0 0 1px var(--accent-soft), var(--shadow-1);
  }
  .msg-foot {
    display: flex; align-items: center; gap: 6px;
    padding: 0 6px;
    font-size: 10px;
    color: var(--text-faint);
  }
  .msg-tools {
    display: inline-flex; align-items: center; gap: 3px;
    padding: 2px 8px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 999px;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    color: var(--text-muted);
  }

  /* System messages (centered, italic, subtle) */
  .msg-system {
    align-self: center;
    padding: 6px 14px;
    background: var(--bg-elevated);
    border: 1px dashed var(--border);
    color: var(--text-muted);
    font-size: 12px; font-style: italic;
    border-radius: 999px;
    text-align: center;
    max-width: 80%;
  }

  /* Body typography inside any bubble */
  .msg-bubble .body :global(code) { background: var(--code-bg); border: 1px solid var(--border); border-radius: 3px; padding: 1px 5px; font-size: 12px; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; color: var(--code-fg); }
  .msg-bubble .body :global(pre) { background: var(--code-bg); border: 1px solid var(--border); border-radius: 8px; padding: 10px 12px; overflow-x: auto; margin: 8px 0 0; max-width: 100%; white-space: pre-wrap; word-break: break-word; }
  .msg-bubble .body :global(pre code) { background: transparent; border: 0; padding: 0; }
  .msg-bubble .body :global(a) { color: var(--accent); text-decoration: underline; text-decoration-style: dotted; text-underline-offset: 2px; word-break: break-all; }
  .msg-bubble .body :global(strong) { color: var(--text); font-weight: 600; }
  .msg-bubble .body :global(em) { color: var(--text-muted); font-style: italic; }
  .msg-bubble .body :global(s) { color: var(--text-faint); text-decoration: line-through; }
  .msg-bubble .body :global(mark) { background: var(--warn-soft); color: var(--text); padding: 0 2px; border-radius: 2px; }
  .msg-bubble .body :global(h1), .msg-bubble .body :global(h2), .msg-bubble .body :global(h3), .msg-bubble .body :global(h4), .msg-bubble .body :global(h5), .msg-bubble .body :global(h6) { margin: 14px 0 8px; font-weight: 600; line-height: 1.3; color: var(--text); }
  .msg-bubble .body :global(h1:first-child), .msg-bubble .body :global(h2:first-child), .msg-bubble .body :global(h3:first-child) { margin-top: 4px; }
  .msg-bubble .body :global(h1) { font-size: 20px; }
  .msg-bubble .body :global(h2) { font-size: 17px; padding-bottom: 4px; border-bottom: 1px solid var(--border-subtle); }
  .msg-bubble .body :global(h3) { font-size: 15px; }
  .msg-bubble .body :global(h4), .msg-bubble .body :global(h5), .msg-bubble .body :global(h6) { font-size: 14px; }
  .msg-bubble .body :global(p) { margin: 8px 0; line-height: 1.65; }
  .msg-bubble .body :global(p:first-child) { margin-top: 0; }
  .msg-bubble .body :global(p:last-child) { margin-bottom: 0; }
  .msg-bubble .body :global(ul), .msg-bubble .body :global(ol) { margin: 8px 0; padding-left: 26px; }
  .msg-bubble .body :global(li) { margin: 4px 0; line-height: 1.6; }
  .msg-bubble .body :global(li > p) { margin: 4px 0; }
  .msg-bubble .body :global(li::marker) { color: var(--accent); }
  .msg-bubble .body :global(li.task) { list-style: none; margin-left: -24px; padding-left: 0; display: flex; align-items: flex-start; gap: 6px; }
  .msg-bubble .body :global(li.task .task-box) { margin-top: 3px; flex: 0 0 auto; accent-color: var(--accent); cursor: default; }
  .msg-bubble .body :global(blockquote) { margin: 10px 0; padding: 8px 16px; border-left: 3px solid var(--accent); background: var(--accent-soft); color: var(--text-muted); border-radius: 0 8px 8px 0; font-style: italic; line-height: 1.6; }
  .msg-bubble .body :global(hr) { border: 0; border-top: 1px solid var(--border-subtle); margin: 12px 0; }
  .msg-bubble .body :global(table) { border-collapse: collapse; margin: 8px 0; font-size: 13px; width: 100%; max-width: 100%; border-radius: 6px; overflow: hidden; }
  .msg-bubble .body :global(th), .msg-bubble .body :global(td) { padding: 6px 10px; border: 1px solid var(--border); text-align: left; }
  .msg-bubble .body :global(th) { background: var(--bg-hover); font-weight: 600; }
  .msg-bubble .body :global(tr:nth-child(even) td) { background: var(--bg-elevated); }

  /* Fenced code blocks — produced by the new markdown renderer.
     The outer .codeblock wraps a small head (lang + copy) and a <pre> body
     that no longer wraps lines, so long lines scroll horizontally. */
  .msg-bubble .body :global(.codeblock) { margin: 8px 0; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; background: var(--code-bg); }
  .msg-bubble .body :global(.codeblock-head) { display: flex; align-items: center; justify-content: space-between; padding: 4px 8px 4px 12px; background: var(--bg-elevated); border-bottom: 1px solid var(--border); font-size: 11px; color: var(--text-muted); }
  .msg-bubble .body :global(.codeblock-lang) { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; text-transform: lowercase; letter-spacing: 0.3px; opacity: 0.85; }
  .msg-bubble .body :global(.codeblock-lang:empty)::before { content: 'код'; opacity: 0.5; }
  .msg-bubble .body :global(.codeblock-copy) { background: transparent; border: 0; padding: 3px 8px; border-radius: 4px; cursor: pointer; color: var(--text-muted); font-family: inherit; font-size: 11px; transition: background 120ms ease, color 120ms ease; }
  .msg-bubble .body :global(.codeblock-copy:hover) { background: var(--bg-hover); color: var(--text); }
  .msg-bubble .body :global(.codeblock-copy.copied) { color: var(--success); }
  .msg-bubble .body :global(.codeblock-pre) { background: var(--code-bg); border: 0; border-radius: 0; padding: 10px 12px; margin: 0; overflow-x: auto; max-width: 100%; white-space: pre; word-break: normal; tab-size: 2; }
  .msg-bubble .body :global(.codeblock-code) { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 12.5px; line-height: 1.55; color: var(--code-fg); background: transparent; border: 0; padding: 0; white-space: pre; }

  /* CSS-token syntax highlighting for the most common languages. The actual
     tokenization happens in src/lib/markdown.ts (very small regex per
     language); these rules just paint the spans. Adding a new language means
     (a) emitting `<span class="tok-X">` in markdown.ts, (b) writing a
     `.language-Y .tok-X` rule here. The light theme uses deeper, more
     saturated hues for AA contrast on cream surfaces; the dark theme
     uses softer pastel hues. */
  .msg-bubble .body :global(.tok-keyword) { color: #8a4848; font-weight: 600; }
  .msg-bubble .body :global(.tok-string)  { color: #2f6a45; }
  .msg-bubble .body :global(.tok-number)  { color: #8a5a1f; font-weight: 500; }
  .msg-bubble .body :global(.tok-comment) { color: var(--text-faint); font-style: italic; }
  .msg-bubble .body :global(.tok-punct)   { color: var(--text-muted); }
  .theme-dark .msg-bubble .body :global(.tok-keyword) { color: #e0b4b4; font-weight: 600; }
  .theme-dark .msg-bubble .body :global(.tok-string)  { color: #9bd9a8; }
  .theme-dark .msg-bubble .body :global(.tok-number)  { color: #e8d8aa; font-weight: 500; }

  .caret { display: inline-block; vertical-align: text-bottom; width: 1ch; color: var(--accent); margin-left: 1px; animation: blink 0.9s steps(1) infinite; }
  @keyframes blink { 0%, 50% { opacity: 1; } 51%, 100% { opacity: 0; } }

  /* "Copy message" button — visible on hover of the assistant row. */
  .msg-copy { background: transparent; border: 0; padding: 0 4px; cursor: pointer; color: var(--text-faint); font-family: inherit; font-size: 12px; line-height: 1; opacity: 0; transition: opacity 120ms ease, color 120ms ease; }
  .msg-row.assistant:hover .msg-copy { opacity: 0.7; }
  .msg-copy:hover { color: var(--accent); opacity: 1 !important; }
  .msg-copy.copied { color: var(--success); opacity: 1 !important; }

  .thinking-block { margin: 0 0 8px; border: 1px solid var(--think-border); background: var(--think-bg); border-radius: 10px; overflow: hidden; box-shadow: var(--shadow-1); }
  .msg-bubble-wrap { display: flex; flex-direction: column; gap: 0; min-width: 0; }
  .thinking-toggle { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 10px; background: transparent; border: 0; color: var(--think-fg); font-family: inherit; font-size: 11px; font-weight: 500; text-align: left; cursor: pointer; transition: background 150ms ease; }
  .thinking-toggle:hover { background: var(--think-bg); filter: brightness(1.10); }
  .think-icon { font-size: 12px; }
  .think-label { flex: 1; display: inline-flex; align-items: center; gap: 6px; }
  .think-chevron { font-size: 10px; opacity: 0.7; }
  .think-spinner { display: inline-block; width: 10px; height: 10px; border: 2px solid var(--think-spin-track); border-top-color: var(--think-fg); border-radius: 50%; animation: tool-spin 0.7s linear infinite; }
  .think-body { padding: 8px 12px 10px; font-size: 12px; line-height: 1.5; color: var(--think-fg-soft); font-style: italic; border-top: 1px solid var(--think-border); white-space: pre-wrap; word-wrap: break-word; max-height: 320px; overflow-y: auto; }

  .msg.image-msg { padding: 0; overflow: hidden; }
  .msg.image-msg .role { padding: 8px 14px 0; }
  .inline-image { background: var(--image-bg); width: 100%; max-width: 480px; margin: 8px 0 0; border-radius: 8px; overflow: hidden; box-shadow: var(--shadow-1); }
  .inline-image-btn { background: transparent; border: 0; padding: 0; margin: 0; width: 100%; height: 100%; display: block; cursor: zoom-in; }
  .inline-image-btn img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .inline-image-foot { display: flex; align-items: center; gap: 10px; padding: 8px 12px; background: var(--image-foot-bg); border-top: 1px solid var(--image-foot-border); }
  .inline-image-prompt { flex: 1; min-width: 0; font-size: 12px; color: var(--image-foot-fg); line-height: 1.4; overflow: hidden; text-overflow: ellipsis; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
  .inline-image-actions { display: flex; align-items: center; gap: 6px; flex: 0 0 auto; }
  .inline-image-aspect { font-size: 10px; color: var(--text-faint); font-family: ui-monospace, monospace; padding: 2px 6px; background: var(--bg-active); border-radius: 4px; }

  /* ---- Scroll-to-bottom floating button (Perplexity/ChatGPT style) ---- */
  .scroll-bottom {
    position: absolute;
    right: 18px;
    bottom: 130px;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: var(--scroll-btn-bg);
    border: 1px solid var(--scroll-btn-border);
    color: var(--scroll-btn-fg);
    font-size: 18px;
    cursor: pointer;
    box-shadow: var(--shadow-2);
    transition: transform 160ms ease, background 160ms ease, color 160ms ease;
    z-index: 5;
    animation: scroll-bottom-in 200ms ease;
  }
  .scroll-bottom:hover {
    background: var(--code-pane-active);
    color: var(--accent-strong);
    transform: translateY(-2px);
  }
  @keyframes scroll-bottom-in {
    from { opacity: 0; transform: translateY(8px); }
    to   { opacity: 1; transform: translateY(0); }
  }

  /* ---- Image loading skeleton (rose-gold shimmer) ---- */
  .image-loading {
    position: relative;
    background: linear-gradient(135deg, rgba(201, 160, 160, 0.10), rgba(168, 130, 200, 0.10), rgba(201, 160, 160, 0.06));
    background-size: 200% 200%;
    border: 1px solid rgba(201, 160, 160, 0.30);
    border-radius: 10px;
    overflow: hidden;
    max-width: 480px;
    margin: 8px 0 0;
    box-shadow: 0 4px 16px rgba(201, 160, 160, 0.10);
    animation: image-loading-bg 4s ease-in-out infinite;
  }
  .image-loading-shimmer {
    position: absolute; inset: 0;
    background: linear-gradient(110deg, transparent 30%, rgba(255, 255, 255, 0.18) 50%, transparent 70%);
    background-size: 200% 100%;
    animation: image-loading-shimmer 1.6s linear infinite;
  }
  @keyframes image-loading-shimmer {
    0% { background-position: 200% 0; }
    100% { background-position: -200% 0; }
  }
  @keyframes image-loading-bg {
    0%, 100% { background-position: 0% 50%; }
    50% { background-position: 100% 50%; }
  }
  .image-loading-meta {
    position: absolute; bottom: 0; left: 0; right: 0;
    display: flex; align-items: center; gap: 8px;
    padding: 8px 12px;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px);
    border-top: 1px solid rgba(201, 160, 160, 0.18);
  }
  .image-loading-icon { font-size: 14px; }
  .image-loading-prompt {
    flex: 1; min-width: 0;
    font-size: 12px; color: #f0d6c4; line-height: 1.4;
    overflow: hidden; text-overflow: ellipsis;
    display: -webkit-box; -webkit-line-clamp: 1; -webkit-box-orient: vertical;
  }
  .image-loading-aspect {
    font-size: 10px; color: #c9a0a0; font-family: ui-monospace, monospace;
    padding: 2px 6px; background: rgba(201, 160, 160, 0.10); border: 1px solid rgba(201, 160, 160, 0.25);
    border-radius: 4px; flex: 0 0 auto;
  }

  .msg.tool-msg { padding: 6px 14px; background: transparent; border: 0; }

  /* ---- Subagent grid (parallel_research / parallel_generate_images) ---- */
  .subagents {
    display: flex; flex-direction: column; gap: 8px;
    background: rgba(99, 102, 241, 0.04);
    border: 1px solid rgba(99, 102, 241, 0.18);
    border-radius: 12px;
    padding: 10px 12px;
  }
  .subagents-head {
    display: flex; align-items: center; gap: 8px;
    font-size: 12px;
    color: #c4a8ff;
    font-weight: 500;
  }
  .subagents-icon { font-size: 14px; }
  .subagents-title { flex: 1; }
  .subagents-spinner {
    display: inline-block;
    width: 12px; height: 12px;
    border: 2px solid rgba(196, 168, 255, 0.25);
    border-top-color: #c4a8ff;
    border-radius: 50%;
    animation: tool-spin 0.7s linear infinite;
  }
  .subagents-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 8px;
  }
  .subagent-card {
    background: #15171d;
    border: 1px solid #2c313a;
    border-radius: 8px;
    padding: 8px 10px;
    display: flex; flex-direction: column; gap: 6px;
    min-height: 60px;
    transition: border-color 200ms ease;
  }
  .subagent-card.done { border-color: rgba(168, 130, 200, 0.30); }
  .subagent-card.error { border-color: rgba(224, 122, 122, 0.35); }
  .subagent-head {
    display: flex; align-items: center; gap: 6px;
    font-size: 11px;
  }
  .subagent-status {
    display: inline-flex; align-items: center; justify-content: center;
    width: 14px; height: 14px;
    color: #c4a8ff;
    flex: 0 0 auto;
    font-size: 10px;
  }
  .subagent-mini-spinner {
    display: inline-block;
    width: 10px; height: 10px;
    border: 2px solid rgba(196, 168, 255, 0.25);
    border-top-color: #c4a8ff;
    border-radius: 50%;
    animation: tool-spin 0.7s linear infinite;
  }
  .subagent-title {
    flex: 1; min-width: 0;
    font-size: 11px; color: #cfd3da; line-height: 1.3;
    overflow: hidden; text-overflow: ellipsis;
    display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical;
  }
  .subagent-img-btn {
    width: 100%;
    background: #0a0c12;
    border: 0; padding: 0;
    border-radius: 6px;
    overflow: hidden;
    cursor: zoom-in;
    display: block;
  }
  .subagent-img-btn img {
    width: 100%; height: 100%;
    object-fit: cover; display: block;
  }
  .subagent-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; }
  .subagent-list li { font-size: 10px; }
  .subagent-list a { color: #88c0ff; text-decoration: none; overflow: hidden; text-overflow: ellipsis; display: -webkit-box; -webkit-line-clamp: 1; -webkit-box-orient: vertical; }
  .subagent-list a:hover { color: #c4a8ff; text-decoration: underline; }
  .web-search { background: var(--info-soft); border: 1px solid var(--border); border-radius: 10px; padding: 8px 10px; max-width: 100%; color: var(--text-muted); font-size: 12px; }
  .web-search-compact .web-search-head { padding: 0; background: transparent; border: 0; }
  .web-search-head {
    display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
    background: transparent; border: 0;
    color: inherit; font: inherit; cursor: pointer;
    text-align: left; width: 100%;
  }
  .web-search-chevron { margin-left: auto; color: var(--text-faint); font-size: 10px; }
  .web-search-empty {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 10px;
    background: var(--bg-elevated);
    border: 1px dashed var(--border);
    border-radius: 999px;
    color: var(--text-faint);
    font-size: 11px;
    font-style: italic;
    align-self: flex-start;
  }
  .web-search-empty-text { overflow: hidden; text-overflow: ellipsis; max-width: 320px; white-space: nowrap; }
  .web-search-more { list-style: none; padding: 4px 0; color: var(--text-faint); font-size: 11px; text-align: center; }

  /* ---- step-by-step plan card ---- */
  .plan-card {
    background: linear-gradient(135deg, rgba(201, 160, 160, 0.06), rgba(176, 196, 222, 0.04));
    border: 1px solid rgba(201, 160, 160, 0.30);
    border-radius: 10px;
    padding: 10px 12px;
    color: #cfd3da;
    font-size: 13px;
    transition: border-color 0.2s, background 0.2s;
  }
  .plan-card.plan-active { border-color: rgba(216, 200, 154, 0.55); background: linear-gradient(135deg, rgba(216, 200, 154, 0.10), rgba(176, 196, 222, 0.04)); }
  .plan-card.plan-done { border-color: rgba(158, 196, 168, 0.40); background: linear-gradient(135deg, rgba(158, 196, 168, 0.08), rgba(176, 196, 222, 0.02)); }
  .plan-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
  .plan-icon { font-size: 14px; }
  .plan-title { font-weight: 600; color: #f5e3d6; font-size: 13px; flex: 1; }
  .plan-counter { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 11px; color: #8a93a6; }
  .plan-progress { height: 4px; background: rgba(255, 255, 255, 0.06); border-radius: 2px; overflow: hidden; margin-bottom: 8px; }
  .plan-progress-fill { height: 100%; background: linear-gradient(90deg, #9ab8d8, #9ec4a8); border-radius: 2px; transition: width 0.4s ease; }
  .plan-card.plan-done .plan-progress-fill { background: linear-gradient(90deg, #9ec4a8, #b8d4c0); }
  .plan-card.plan-active .plan-progress-fill { background: linear-gradient(90deg, #d8c89a, #9ab8d8); }
  .plan-steps { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .plan-step { display: flex; align-items: baseline; gap: 8px; padding: 3px 0; font-size: 12px; line-height: 1.45; }
  .plan-step-marker { flex: 0 0 18px; text-align: center; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; color: #6c7280; }
  .plan-step-title { flex: 1; color: #cfd3da; }
  .plan-step-note { color: #8a93a6; font-size: 11px; font-style: italic; }
  .plan-step-done .plan-step-marker { color: #9ec4a8; }
  .plan-step-done .plan-step-title { color: #7d8590; text-decoration: line-through; text-decoration-color: rgba(125, 133, 144, 0.5); }
  .plan-step-in_progress .plan-step-marker { color: #d8c89a; animation: plan-pulse 1.4s ease-in-out infinite; }
  .plan-step-in_progress .plan-step-title { color: #f5e3d6; font-weight: 500; }
  .plan-step-error .plan-step-marker { color: #d88a8a; }
  .plan-step-error .plan-step-title { color: #d8a89a; }
  .web-search-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding-bottom: 6px; border-bottom: 1px solid rgba(176, 196, 222, 0.18); margin-bottom: 6px; }
  .web-search-icon { font-size: 14px; }
  .web-search-title { color: #b0c4de; font-weight: 500; font-size: 12px; }
  .web-search-query { color: #d4d8e0; font-size: 12px; font-style: italic; opacity: 0.9; }
  .web-search-count { color: #7d8590; font-size: 10px; margin-left: auto; }
  .web-search-spinner { width: 10px; height: 10px; border: 1.5px solid rgba(176, 196, 222, 0.3); border-top-color: #b0c4de; border-radius: 50%; animation: tool-spin 0.7s linear infinite; display: inline-block; }
  .web-search-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .web-search-item { padding: 4px 0; border-bottom: 1px solid rgba(176, 196, 222, 0.10); }
  .web-search-item:last-child { border-bottom: 0; }
  .web-search-link { display: flex; flex-direction: column; gap: 1px; text-decoration: none; }
  .web-search-link-title { color: #b0c4de; font-size: 12px; line-height: 1.35; word-break: break-word; }
  .web-search-link-title:hover { color: #c9a0a0; text-decoration: underline; }
  .web-search-link-host { color: #7d8590; font-size: 10px; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; opacity: 0.85; overflow: hidden; text-overflow: ellipsis; display: -webkit-box; -webkit-line-clamp: 1; -webkit-box-orient: vertical; }
  .web-search-snippet { color: #8b929c; font-size: 11px; line-height: 1.45; margin: 2px 0 0 0; word-break: break-word; }
  .msg.tool-msg .role { display: none; }
  .tool-pill {
    display: flex; flex-direction: column;
    background: rgba(176, 120, 120, 0.10);
    border: 1px solid rgba(176, 120, 120, 0.30);
    border-radius: 10px;
    color: #b6bcc7;
    font-size: 12px;
    max-width: 100%;
    overflow: hidden;
    transition: background 200ms ease, border-color 200ms ease, box-shadow 200ms ease;
  }
  .tool-pill.pending {
    background: rgba(245, 181, 107, 0.10);
    border-color: rgba(245, 181, 107, 0.40);
    box-shadow: 0 0 0 1px rgba(245, 181, 107, 0.20), 0 0 12px rgba(245, 181, 107, 0.10);
    animation: tool-pulse 1.4s ease-in-out infinite;
  }
  .tool-pill.error {
    background: rgba(224, 122, 122, 0.10);
    border-color: rgba(224, 122, 122, 0.40);
  }
  @keyframes tool-pulse {
    0%, 100% { box-shadow: 0 0 0 1px rgba(245, 181, 107, 0.20), 0 0 12px rgba(245, 181, 107, 0.10); }
    50%      { box-shadow: 0 0 0 2px rgba(245, 181, 107, 0.30), 0 0 18px rgba(245, 181, 107, 0.18); }
  }
  .tool-pill-head { display: flex; align-items: center; gap: 8px; padding: 6px 10px; background: transparent; border: 0; color: inherit; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 12px; cursor: pointer; text-align: left; width: 100%; }
  .tool-pill-head:hover { background: rgba(99, 102, 241, 0.10); }
  .tool-pill.error .tool-pill-head:hover { background: rgba(224, 122, 122, 0.10); }
  .tool-emoji { font-size: 14px; line-height: 1; flex: 0 0 auto; }
  .tool-icon { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; color: #c4a8ff; flex: 0 0 auto; }
  .tool-status { font-size: 10.5px; padding: 1px 7px; border-radius: 999px; flex: 0 0 auto; letter-spacing: 0.2px; }
  .tool-status.pending { background: rgba(245, 181, 107, 0.18); color: #f5d8a8; animation: tool-status-pulse 1.2s ease-in-out infinite; }
  @keyframes tool-status-pulse { 0%, 100% { opacity: 0.7; } 50% { opacity: 1; } }
  .tool-pill.error .tool-icon { color: #ffaaaa; }
  .tool-name { color: #c4a8ff; font-weight: 600; flex: 0 0 auto; }
  .tool-pill.error .tool-name { color: #ffaaaa; }
  .tool-args-preview { color: #8a93a6; font-size: 11px; flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .tool-chevron { color: #6c7280; font-size: 10px; flex: 0 0 auto; }
  .tool-args { margin: 0; padding: 8px 10px 10px 30px; background: rgba(0, 0, 0, 0.25); border-top: 1px solid rgba(99, 102, 241, 0.18); color: #a8b0c0; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 11px; line-height: 1.4; white-space: pre-wrap; word-break: break-word; overflow-x: auto; }
  .tool-err-text { padding: 0 10px 8px 30px; color: #ffaaaa; font-size: 11px; }
  .tool-spinner { display: inline-block; width: 12px; height: 12px; border: 2px solid rgba(196, 168, 255, 0.25); border-top-color: #c4a8ff; border-radius: 50%; animation: tool-spin 0.7s linear infinite; }
  @keyframes tool-spin { to { transform: rotate(360deg); } }

  .media-view { display: flex; flex-direction: column; gap: 16px; max-width: 1100px; width: 100%; margin: 0 auto; }
  .media-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
  .media-h { font-size: 20px; font-weight: 600; color: #e6e8eb; }
  .media-sub { font-size: 12px; color: #8a93a6; margin-top: 4px; }
  .media-sub code { background: #1c1f26; border: 1px solid #2c313a; border-radius: 4px; padding: 1px 5px; font-size: 11px; }
  .media-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .media-search { background: #1c1f26; border: 1px solid #2c313a; border-radius: 8px; color: #e6e8eb; font-family: inherit; font-size: 13px; padding: 7px 12px; min-width: 220px; outline: none; transition: border-color 150ms ease, box-shadow 150ms ease; }
  .media-search:focus { border-color: rgba(201, 160, 160, 0.5); box-shadow: 0 0 0 3px rgba(201, 160, 160, 0.10); }
  .media-btn { background: transparent; border: 1px solid #2c313a; border-radius: 8px; color: #b6bcc7; font-family: inherit; font-size: 12px; padding: 7px 12px; cursor: pointer; transition: background 150ms ease, color 150ms ease, border-color 150ms ease; }
  .media-btn:hover { background: #252932; color: #e6e8eb; }
  .media-btn.danger:hover { color: #ffaaaa; border-color: #5a3030; background: rgba(224, 122, 122, 0.08); }
  .media-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 60px 20px; color: #6c7280; text-align: center; }
  .media-empty-icon { font-size: 56px; opacity: 0.5; }
  .media-empty-h { font-size: 16px; font-weight: 500; color: #b6bcc7; }
  .media-empty-sub { font-size: 13px; color: #6c7280; line-height: 1.5; }
  .media-empty-sub em { color: #c4a8ff; font-style: normal; }
  .media-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 14px; }
  .media-card { background: #1c1f26; border: 1px solid #2c313a; border-radius: 14px; overflow: hidden; display: flex; flex-direction: column; transition: transform 200ms cubic-bezier(0.16, 1, 0.3, 1), border-color 200ms ease, box-shadow 200ms ease; box-shadow: 0 2px 8px rgba(0, 0, 0, 0.15); }
  .media-card:hover { transform: translateY(-2px); border-color: rgba(201, 160, 160, 0.4); box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4), 0 0 0 1px rgba(201, 160, 160, 0.1); }
  .media-card-img { position: relative; background: #0a0c12; border: 0; padding: 0; width: 100%; aspect-ratio: 1 / 1; cursor: zoom-in; display: block; overflow: hidden; }
  .media-card-img img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .media-card-overlay { position: absolute; top: 8px; right: 8px; background: rgba(0, 0, 0, 0.55); backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px); padding: 2px 8px; border-radius: 999px; font-size: 10px; color: #fff; font-family: ui-monospace, monospace; border: 1px solid rgba(255, 255, 255, 0.15); }
  .media-card-foot { padding: 8px 10px; background: rgba(0, 0, 0, 0.25); border-top: 1px solid rgba(255, 255, 255, 0.04); display: flex; flex-direction: column; gap: 6px; }
  .media-card-prompt { font-size: 12px; color: #b6bcc7; line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .media-card-actions { display: flex; align-items: center; justify-content: flex-end; gap: 4px; }

  .seg-badge { display: inline-flex; align-items: center; justify-content: center; min-width: 18px; height: 18px; padding: 0 5px; background: rgba(201, 160, 160, 0.20); color: #f0d6c4; border-radius: 999px; font-size: 10px; font-weight: 600; margin-left: 4px; }

  /* ---- Fusion Research: 2-column layout (sidebar + Perplexity-style feed) ---- */
  .research-view {
    display: grid;
    grid-template-columns: 280px 1fr;
    gap: 22px;
    max-width: 1200px;
    width: 100%;
    margin: 0 auto;
    align-items: start;
  }
  .research-feed { display: flex; flex-direction: column; gap: 14px; min-width: 0; }
  .research-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; flex-wrap: wrap; padding-bottom: 4px; }
  .research-h { font-size: 22px; font-weight: 600; color: #e6e8eb; letter-spacing: -0.01em; }
  .research-sub { font-size: 12px; color: #8a93a6; margin-top: 4px; line-height: 1.5; }
  .research-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 60px 20px; color: #6c7280; font-size: 13px; text-align: center; }
  .research-banner.err { margin: 0; padding: 8px 12px; background: rgba(224, 122, 122, 0.08); border: 1px solid rgba(224, 122, 122, 0.30); border-radius: 8px; color: #f5b56b; font-size: 12px; }

  /* ---- left sidebar: interests ---- */
  .research-sidebar {
    position: sticky;
    top: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: #16191f;
    border: 1px solid #2c313a;
    border-radius: 14px;
    padding: 14px 14px 12px;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.18);
    max-height: calc(100vh - 24px);
    overflow-y: auto;
    scrollbar-width: thin;
    scrollbar-color: #2c313a transparent;
  }
  .research-sidebar::-webkit-scrollbar { width: 6px; }
  .research-sidebar::-webkit-scrollbar-thumb { background: #2c313a; border-radius: 3px; }
  .sidebar-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .sidebar-title { display: inline-flex; align-items: center; gap: 6px; font-size: 14px; font-weight: 600; color: #e6e8eb; }
  .sidebar-icon { font-size: 14px; }
  .sidebar-count {
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 11px;
    color: #8a93a6;
    padding: 1px 8px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 999px;
    min-width: 22px;
    text-align: center;
  }
  .sidebar-sub { margin: 0; font-size: 11px; color: #8a93a6; line-height: 1.5; }
  .sidebar-add { display: flex; gap: 6px; align-items: center; }
  .sidebar-input {
    flex: 1; min-width: 0;
    background: #0f1217;
    border: 1px solid #2c313a;
    border-radius: 8px;
    color: #e6e8eb;
    font-family: inherit; font-size: 12px;
    padding: 7px 10px;
    outline: none;
    transition: border-color 150ms ease, box-shadow 150ms ease;
  }
  .sidebar-input::placeholder { color: #6c7280; }
  .sidebar-input:focus { border-color: rgba(201, 160, 160, 0.5); box-shadow: 0 0 0 3px rgba(201, 160, 160, 0.10); }
  .sidebar-add-btn {
    flex: 0 0 auto;
    width: 30px; height: 30px;
    border-radius: 8px;
    border: 1px solid rgba(201, 160, 160, 0.30);
    background: rgba(201, 160, 160, 0.10);
    color: #c9a0a0;
    font-size: 16px; line-height: 1;
    cursor: pointer;
    display: inline-flex; align-items: center; justify-content: center;
    transition: background 150ms ease, border-color 150ms ease, color 150ms ease;
  }
  .sidebar-add-btn:hover { background: rgba(201, 160, 160, 0.20); border-color: rgba(201, 160, 160, 0.5); color: #f0d6c4; }
  .sidebar-refresh { width: 100%; justify-content: center; }
  .sidebar-interests { display: flex; flex-direction: column; gap: 4px; margin-top: 2px; }
  .sidebar-interest {
    display: flex; align-items: center; gap: 6px;
    padding: 6px 8px;
    border-radius: 8px;
    background: rgba(168, 85, 247, 0.06);
    border: 1px solid rgba(168, 85, 247, 0.18);
    transition: background 150ms ease, border-color 150ms ease;
  }
  .sidebar-interest:hover { background: rgba(168, 85, 247, 0.12); border-color: rgba(168, 85, 247, 0.32); }
  .interest-hash { color: #c4a8ff; font-size: 11px; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; flex: 0 0 auto; }
  .interest-text { flex: 1; min-width: 0; font-size: 12px; color: #cfd3da; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .interest-remove {
    flex: 0 0 auto;
    background: transparent; border: 0;
    color: #8a93a6; cursor: pointer;
    width: 18px; height: 18px;
    border-radius: 4px;
    font-size: 14px; line-height: 1;
    display: inline-flex; align-items: center; justify-content: center;
    transition: background 150ms ease, color 150ms ease;
  }
  .interest-remove:hover { background: rgba(224, 122, 122, 0.18); color: #ffaaaa; }
  .sidebar-empty {
    font-size: 11px; color: #f5b56b; text-align: center;
    padding: 12px 8px;
    background: rgba(245, 181, 107, 0.06);
    border: 1px dashed rgba(245, 181, 107, 0.25);
    border-radius: 8px;
  }
  .sidebar-clear {
    background: transparent;
    border: 1px dashed rgba(255, 255, 255, 0.16);
    color: #8a93a6;
    font-size: 11px;
    padding: 5px 10px;
    border-radius: 8px;
    cursor: pointer;
    margin-top: 2px;
    transition: color 150ms ease, border-color 150ms ease, background 150ms ease;
  }
  .sidebar-clear:hover { color: #ffaaaa; border-color: #5a3030; background: rgba(224, 122, 122, 0.05); }

  /* ---- Perplexity-style news cards (horizontal, source + thumb) ---- */
  .news-list { display: flex; flex-direction: column; gap: 10px; }
  .news-card {
    display: flex;
    align-items: stretch;
    gap: 16px;
    background: #1a1d23;
    border: 1px solid #2c313a;
    border-radius: 14px;
    padding: 14px 16px;
    color: inherit;
    text-decoration: none;
    cursor: pointer;
    transition: background 180ms ease, border-color 180ms ease, transform 180ms ease, box-shadow 180ms ease;
  }
  .news-card:hover {
    background: #1d2027;
    border-color: rgba(201, 160, 160, 0.40);
    transform: translateY(-1px);
    box-shadow: 0 4px 18px rgba(0, 0, 0, 0.30);
  }
  .news-card-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 6px; }
  .news-card-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .news-source { display: inline-flex; align-items: center; gap: 8px; min-width: 0; }
  .news-favicon {
    flex: 0 0 auto;
    width: 18px; height: 18px;
    border-radius: 5px;
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    display: inline-flex; align-items: center; justify-content: center;
    text-transform: uppercase;
    letter-spacing: 0;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.30);
  }
  .news-source-name { font-size: 12px; color: #8a93a6; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 240px; }
  .news-citation {
    flex: 0 0 auto;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 11px;
    color: #6c7280;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.06);
  }
  .news-card-title {
    margin: 0;
    font-size: 15.5px;
    font-weight: 600;
    line-height: 1.35;
    color: #e6e8eb;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .news-card:hover .news-card-title { color: #f5e3d6; }
  .news-card-snippet {
    margin: 0;
    font-size: 13px;
    color: #b6bcc7;
    line-height: 1.5;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .news-card-foot { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-top: 4px; }
  .news-card-topic {
    font-size: 11px;
    color: #c4a8ff;
    padding: 2px 9px;
    background: rgba(168, 85, 247, 0.10);
    border: 1px solid rgba(168, 85, 247, 0.25);
    border-radius: 999px;
    font-weight: 500;
  }
  .news-card-topic.global {
    background: rgba(99, 102, 241, 0.12);
    border-color: rgba(99, 102, 241, 0.30);
    color: #a8b0ff;
  }
  .news-card-arrow {
    font-size: 12px;
    color: #6c7280;
    font-weight: 500;
    transition: color 160ms ease, transform 160ms ease;
  }
  .news-card:hover .news-card-arrow { color: #c9a0a0; transform: translateX(2px); }
  .news-card-thumb {
    flex: 0 0 144px;
    width: 144px;
    align-self: stretch;
    min-height: 96px;
    border-radius: 10px;
    overflow: hidden;
    background: #0f1217;
    position: relative;
  }
  .news-card-thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }

  .refresh-btn { display: inline-flex; align-items: center; gap: 6px; background: rgba(201, 160, 160, 0.10); border: 1px solid rgba(201, 160, 160, 0.30); color: #c9a0a0; font-family: inherit; font-size: 12px; padding: 6px 12px; border-radius: 999px; cursor: pointer; transition: background 150ms ease, transform 120ms ease, border-color 150ms ease; }
  .refresh-btn:hover:not(:disabled) { background: rgba(201, 160, 160, 0.18); border-color: rgba(201, 160, 160, 0.5); transform: translateY(-1px); }
  .refresh-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .spinner-mini { width: 12px; height: 12px; border: 2px solid rgba(201, 160, 160, 0.25); border-top-color: #c9a0a0; border-radius: 50%; animation: spin-mini 0.7s linear infinite; display: inline-block; }
  @keyframes spin-mini { to { transform: rotate(360deg); } }
  .spinner-big { width: 28px; height: 28px; border: 3px solid rgba(201, 160, 160, 0.18); border-top-color: #c9a0a0; border-radius: 50%; animation: spin-mini 0.8s linear infinite; }

  .image-icon-btn { background: transparent; border: 0; width: 24px; height: 24px; color: #8a93a6; border-radius: 4px; cursor: pointer; font-size: 14px; transition: background 150ms ease, color 150ms ease; }
  .image-icon-btn:hover { background: rgba(255, 255, 255, 0.08); color: #e6e8eb; }

  .image-lightbox { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.85); backdrop-filter: blur(6px); -webkit-backdrop-filter: blur(6px); display: flex; align-items: center; justify-content: center; z-index: 100; padding: 32px; animation: fade-in 200ms ease; }
  @keyframes fade-in { from { opacity: 0; } to { opacity: 1; } }
  .image-lightbox img { max-width: 90vw; max-height: 80vh; object-fit: contain; border-radius: 10px; box-shadow: 0 20px 60px rgba(0, 0, 0, 0.5); }
  .lightbox-close { position: absolute; top: 16px; right: 20px; width: 36px; height: 36px; background: rgba(255, 255, 255, 0.08); border: 0; border-radius: 50%; color: #fff; font-size: 20px; cursor: pointer; transition: background 150ms ease; }
  .lightbox-close:hover { background: rgba(255, 255, 255, 0.16); }
  .lightbox-foot { position: absolute; bottom: 24px; left: 50%; transform: translateX(-50%); background: rgba(15, 18, 23, 0.85); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 12px; padding: 10px 14px; max-width: 600px; width: calc(100% - 64px); display: flex; align-items: center; gap: 12px; }
  .lightbox-prompt { flex: 1; min-width: 0; font-size: 12px; color: #b6bcc7; line-height: 1.4; overflow: hidden; text-overflow: ellipsis; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
  .lightbox-actions { display: flex; align-items: center; gap: 8px; flex: 0 0 auto; }
  .lightbox-aspect { font-size: 10px; color: #6c7280; font-family: ui-monospace, monospace; padding: 2px 6px; background: rgba(255, 255, 255, 0.04); border-radius: 4px; }

  .error { margin: 0 16px 8px; padding: 8px 12px; background: rgba(224,122,122,0.1); border: 1px solid #5a3030; border-radius: 6px; color: #f5b56b; font-size: 12px; }

  .composer { flex: 0 0 auto; padding: 12px 16px 14px; background: linear-gradient(to top, rgba(15, 18, 23, 0.96), rgba(15, 18, 23, 0.6) 60%, transparent); }

  /* ---- plan-mode composer ---- */
  .plan-composer { padding-top: 8px; }
  .plan-form {
    display: flex; flex-direction: column;
    background: rgba(28, 31, 38, 0.7);
    border: 1px solid rgba(201, 160, 160, 0.18);
    border-radius: 14px;
    padding: 10px 12px;
    backdrop-filter: blur(10px);
    -webkit-backdrop-filter: blur(10px);
    box-shadow: 0 1px 0 rgba(255, 255, 255, 0.03) inset, 0 6px 22px rgba(0, 0, 0, 0.3);
  }
  .plan-title-input {
    background: transparent;
    border: 0;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    color: var(--text, #e6e8ee);
    font: inherit;
    font-size: 14px;
    font-weight: 500;
    padding: 6px 4px 8px;
    outline: none;
  }
  .plan-title-input::placeholder { color: var(--text-muted, #6b6b70); }
  .plan-title-input:focus { border-bottom-color: var(--accent, #4a6fcf); }
  .plan-steps-input {
    background: transparent;
    border: 0;
    color: var(--text, #e6e8ee);
    font: inherit;
    font-size: 13px;
    line-height: 1.55;
    padding: 8px 4px;
    resize: vertical;
    min-height: 80px;
    max-height: 280px;
    outline: none;
    font-family: ui-monospace, 'SF Mono', Consolas, monospace;
  }
  .plan-steps-input::placeholder { color: var(--text-muted, #6b6b70); }
  .plan-form-bar {
    display: flex; align-items: center; justify-content: space-between;
    gap: 12px; margin-top: 6px;
    padding-top: 8px; border-top: 1px solid rgba(255, 255, 255, 0.06);
  }
  .plan-hint { font-size: 11px; color: var(--text-muted, #6b6b70); }
  .plan-buttons { display: flex; gap: 6px; }
  .plan-save-btn,
  .plan-run-btn {
    border: 0;
    padding: 6px 12px;
    border-radius: 8px;
    font: inherit;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    transition: background 120ms ease, transform 120ms ease;
  }
  .plan-save-btn {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text, #e6e8ee);
  }
  .plan-save-btn:hover:not(:disabled) { background: rgba(255, 255, 255, 0.10); }
  .plan-run-btn {
    background: var(--accent, #4a6fcf);
    color: #fff;
  }
  .plan-run-btn:hover:not(:disabled) { background: var(--accent-strong, #3a5fbf); }
  .plan-save-btn:disabled,
  .plan-run-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .chat-history-bar {
    display: flex; align-items: center; gap: 8px;
    margin: 0 0 8px;
    padding: 4px 4px 4px 6px;
    font-size: 11px;
  }
  .hist-btn {
    display: inline-flex; align-items: center; gap: 5px;
    padding: 4px 10px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.10);
    border-radius: 999px;
    color: #b6bcc7;
    font: inherit; font-size: 11px;
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease, border-color 120ms ease;
  }
  .hist-btn:hover { background: rgba(201, 160, 160, 0.12); color: #e6e8eb; border-color: rgba(201, 160, 160, 0.35); }
  .hist-btn.danger:hover { background: rgba(216, 122, 122, 0.16); color: #ffd0d0; border-color: rgba(216, 138, 138, 0.55); }
  .hist-status { color: #6c7280; font-size: 10.5px; margin-left: auto; padding-right: 4px; }
  .hist-status.saving { color: #c9a0a0; }
  .theme-dark .hist-btn { background: rgba(255, 255, 255, 0.04); border-color: rgba(255, 255, 255, 0.10); }
  .theme-dark .hist-status { color: #6c7280; }
  /* Light theme tweaks so the bar reads on cream surfaces. */
  :global(html:not(.theme-dark)) .hist-btn {
    background: rgba(60, 50, 40, 0.04);
    border-color: rgba(60, 50, 40, 0.12);
    color: #5a6068;
  }
  :global(html:not(.theme-dark)) .hist-btn:hover { background: rgba(176, 120, 120, 0.10); color: #1a1c20; border-color: rgba(176, 120, 120, 0.35); }
  :global(html:not(.theme-dark)) .hist-status { color: #8a8f97; }
  .input-shell { position: relative; display: flex; align-items: flex-end; gap: 6px; padding: 6px 6px 6px 14px; background: rgba(28, 31, 38, 0.7); border: 1px solid rgba(201, 160, 160, 0.18); border-radius: 18px; backdrop-filter: blur(10px); -webkit-backdrop-filter: blur(10px); box-shadow: 0 1px 0 rgba(255, 255, 255, 0.03) inset, 0 6px 22px rgba(0, 0, 0, 0.3); transition: border-color 200ms cubic-bezier(0.16, 1, 0.3, 1), box-shadow 240ms cubic-bezier(0.16, 1, 0.3, 1), background 200ms ease, transform 200ms ease; overflow: hidden; }
  .input-bg { position: absolute; inset: 0; border-radius: inherit; background: linear-gradient(135deg, rgba(99, 102, 241, 0.08), rgba(168, 85, 247, 0.08), rgba(236, 72, 153, 0.06)); opacity: 0; transition: opacity 240ms ease; pointer-events: none; }
  .input-shell.focused { border-color: rgba(201, 160, 160, 0.55); box-shadow: 0 1px 0 rgba(255, 255, 255, 0.05) inset, 0 0 0 4px rgba(201, 160, 160, 0.10), 0 0 28px rgba(201, 160, 160, 0.18), 0 8px 28px rgba(0, 0, 0, 0.35); background: rgba(28, 31, 38, 0.92); }
  .input-shell.focused .input-bg { opacity: 1; }
  .input-shell.has-text { border-color: rgba(201, 160, 160, 0.32); }
  .input-shell.disabled { opacity: 0.5; }
  .input-shell textarea { flex: 1; min-height: 28px; max-height: 180px; border: 0; outline: 0; resize: none; background: transparent; color: #e6e8eb; font-family: inherit; font-size: 14px; line-height: 1.5; padding: 8px 4px; caret-color: #c9a0a0; }
  .input-shell textarea::placeholder { color: #6c7280; transition: color 200ms ease, opacity 200ms ease; }
  .input-shell.focused textarea::placeholder { color: #8a93a6; }
  .input-shell textarea:disabled { cursor: not-allowed; }
  .input-shell textarea::-webkit-scrollbar { width: 6px; }
  .input-shell textarea::-webkit-scrollbar-thumb { background: #2c313a; border-radius: 3px; }
  .input-actions { display: flex; align-items: center; gap: 4px; padding-bottom: 2px; }
  .icon-btn { width: 36px; height: 36px; border-radius: 50%; border: 0; background: transparent; color: #8a93a6; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 16px; transition: background 160ms ease, color 160ms ease, transform 160ms ease; position: relative; }
  .icon-btn:hover:not(:disabled) { background: rgba(255, 255, 255, 0.06); color: #cfd3da; }
  .icon-btn:disabled { opacity: 0.35; cursor: not-allowed; }
  .icon-btn.active { background: rgba(208, 64, 64, 0.18); color: #ff8a8a; animation: vpulse 1.2s ease-in-out infinite; }
  .icon-btn.transcribing { background: rgba(208, 144, 64, 0.18); color: #ffc88a; }
  .icon-btn.error { background: rgba(106, 26, 26, 0.4); color: #ffaaaa; }
  @keyframes vpulse { 0% { box-shadow: 0 0 0 0 rgba(208, 64, 64, 0.55); } 50% { box-shadow: 0 0 0 10px rgba(208, 64, 64, 0.15); } 100% { box-shadow: 0 0 0 10px rgba(208, 64, 64, 0); } }
  /* Multitask mode button — purple/indigo accent so it doesn't clash with
     the red voice-recording `.active` state. Off = the same muted gray as
     the other icon buttons; on = a soft purple wash with a subtle pulse. */
  .icon-btn.multitask-btn .multitask-glyph { font-size: 17px; line-height: 1; filter: grayscale(0.4); transition: filter 160ms ease, transform 160ms ease; }
  .icon-btn.multitask-btn:hover:not(:disabled) .multitask-glyph { filter: grayscale(0); transform: scale(1.05); }
  .icon-btn.multitask-btn.on { background: rgba(139, 92, 246, 0.22); color: #c4b5fd; box-shadow: 0 0 0 1px rgba(139, 92, 246, 0.45) inset, 0 0 14px rgba(139, 92, 246, 0.28); }
  .icon-btn.multitask-btn.on .multitask-glyph { filter: none; animation: mt-glow 1.6s ease-in-out infinite; }
  @keyframes mt-glow { 0%, 100% { text-shadow: 0 0 4px rgba(196, 181, 253, 0.55); } 50% { text-shadow: 0 0 10px rgba(196, 181, 253, 0.9); } }
  /* When the input shell is in multitask mode, the border + glow shift to
     the same purple so the mode is obvious without reading the button. */
  .input-shell.multitask { border-color: rgba(139, 92, 246, 0.55); box-shadow: 0 1px 0 rgba(255, 255, 255, 0.03) inset, 0 0 0 1px rgba(139, 92, 246, 0.25) inset, 0 0 18px rgba(139, 92, 246, 0.14); }
  .input-shell.multitask .input-bg { opacity: 1; background: linear-gradient(135deg, rgba(99, 102, 241, 0.18), rgba(168, 85, 247, 0.20), rgba(236, 72, 153, 0.12)); }
  /* Pill in the hint line — only shown when multitask is on. Clickable so
     the user can toggle off without having to reach for the icon button. */
  .hint-pill.multitask-pill { background: rgba(139, 92, 246, 0.18); border: 1px solid rgba(139, 92, 246, 0.45); color: #c4b5fd; font-size: 10px; font-weight: 600; cursor: pointer; transition: background 150ms ease, border-color 150ms ease, color 150ms ease; padding: 1px 8px; border-radius: 999px; }
  .hint-pill.multitask-pill:hover { background: rgba(139, 92, 246, 0.28); border-color: rgba(139, 92, 246, 0.65); color: #ddd6fe; }
  .rec-dot { width: 12px; height: 12px; border-radius: 50%; background: #ff6060; box-shadow: 0 0 10px rgba(255, 80, 80, 0.6); }
  .spinner { width: 14px; height: 14px; border: 2px solid rgba(255, 200, 138, 0.3); border-top-color: #ffc88a; border-radius: 50%; animation: spin 0.7s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .send-btn { width: 36px; height: 36px; border-radius: 50%; border: 0; cursor: pointer; display: flex; align-items: center; justify-content: center; color: #6c7280; background: rgba(255, 255, 255, 0.04); transition: background 200ms ease, color 200ms ease, transform 160ms ease, box-shadow 200ms ease; }
  .send-btn:hover:not(:disabled) { transform: scale(1.05); }
  .send-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .send-btn.active { color: #050509; background: linear-gradient(135deg, #f0d6c4 0%, #c9a0a0 50%, #a87a7a 100%); box-shadow: 0 4px 16px rgba(201, 160, 160, 0.45), inset 0 1px 0 rgba(255, 255, 255, 0.4); }
  .send-btn.active:hover { transform: scale(1.08); }
  .hint { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; font-size: 11px; color: #6c7280; margin-top: 8px; padding: 0 6px; transition: opacity 200ms ease; position: relative; }
  .hint.focused { opacity: 0.55; }
  .hint-pill { padding: 1px 8px; border-radius: 999px; background: rgba(201, 160, 160, 0.08); border: 1px solid rgba(201, 160, 160, 0.18); color: #c9a0a0; font-size: 10px; font-weight: 500; }
  .hint-pill.model-pill { color: #8a93a6; }

  /* ---- busy / streaming indicator ---- */
  .busy-pill {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 2px 8px 2px 6px;
    border-radius: 999px;
    background: rgba(245, 181, 107, 0.16);
    border: 1px solid rgba(245, 181, 107, 0.45);
    color: #f5d8a8;
    font-size: 10px; font-weight: 600;
    cursor: pointer;
    text-transform: uppercase; letter-spacing: 0.5px;
    transition: background 120ms ease, border-color 120ms ease, transform 80ms ease;
  }
  .busy-pill:hover { background: rgba(224, 122, 122, 0.18); border-color: rgba(224, 122, 122, 0.55); color: #ffd0d0; }
  .busy-pill:active { transform: translateY(1px); }
  .busy-spinner { display: inline-block; width: 9px; height: 9px; border: 2px solid rgba(245, 181, 107, 0.30); border-top-color: #f5d8a8; border-radius: 50%; animation: tool-spin 0.7s linear infinite; }
  .busy-stop { font-size: 11px; opacity: 0.7; margin-left: 1px; }
  .busy-pill:hover .busy-stop { opacity: 1; }

  /* The composer shifts to a "steer-ready" state while busy so the
     user immediately sees that typing + Enter will cancel the current
     round and start a new one. */
  .input-shell.busy { border-color: rgba(245, 181, 107, 0.45); box-shadow: 0 0 0 1px rgba(245, 181, 107, 0.18); }
  .input-shell.busy .send-btn.active { background: rgba(224, 122, 122, 0.85); color: #fff; }
  .input-shell.busy .send-btn.active:hover { background: rgba(224, 122, 122, 1); }
  :global(html:not(.theme-dark)) .input-shell.busy { border-color: rgba(245, 181, 107, 0.65); box-shadow: 0 0 0 1px rgba(245, 181, 107, 0.30); }
  :global(html:not(.theme-dark)) .busy-pill { background: rgba(245, 181, 107, 0.22); border-color: rgba(245, 181, 107, 0.55); color: #8a5a1f; }
  :global(html:not(.theme-dark)) .busy-pill:hover { background: rgba(224, 122, 122, 0.22); border-color: rgba(224, 122, 122, 0.55); color: #a04040; }
  .hint-spacer { flex: 1; }
  .hint-keys { display: inline-flex; align-items: center; gap: 4px; color: #6c7280; }
  .hint-keys .hint-steer { color: #f5d8a8; font-weight: 500; }
  :global(html:not(.theme-dark)) .hint-keys .hint-steer { color: #8a5a1f; }
  .hint-keys kbd { display: inline-block; padding: 1px 6px; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 10px; background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 4px; color: #cfd3da; }
  .hint-sep { color: #3a414b; margin: 0 2px; }
  .voice-err { color: #ff8a8a; font-family: 'Cascadia Code', monospace; }

  /* ---- context usage button + popover ---- */
  .context-wrap { position: relative; display: inline-flex; }
  .context-btn {
    display: inline-flex; align-items: center; gap: 5px;
    padding: 3px 8px 3px 6px; height: 22px;
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 999px;
    color: #b6bcc7;
    font-size: 11px; font-weight: 500;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s, color 0.15s;
    position: relative;
  }
  .context-btn:hover { background: rgba(255, 255, 255, 0.05); }
  .context-btn.low { color: #9ec4a8; border-color: rgba(158, 196, 168, 0.25); }
  .context-btn.mid { color: #d8c89a; border-color: rgba(216, 200, 154, 0.30); }
  .context-btn.high { color: #d8a89a; border-color: rgba(216, 168, 154, 0.40); }
  .context-btn.crit { color: #d88a8a; border-color: rgba(216, 138, 138, 0.55); animation: context-pulse 2s ease-in-out infinite; }
  @keyframes context-pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(216, 138, 138, 0); }
    50% { box-shadow: 0 0 0 3px rgba(216, 138, 138, 0.18); }
  }
  @keyframes plan-pulse {
    0%, 100% { opacity: 0.6; }
    50% { opacity: 1; }
  }
  .context-ring { display: inline-flex; align-items: center; justify-content: center; }
  .context-ring svg { transform: rotate(-90deg); }
  .context-ring .ring-bg { fill: none; stroke: rgba(255, 255, 255, 0.10); stroke-width: 2; }
  .context-ring .ring-fg { fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; transition: stroke-dasharray 0.3s ease; }
  .context-pct { line-height: 1; }
  .context-pop {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    z-index: 50;
    width: 320px;
    background: #16191f;
    border: 1px solid #2c313a;
    border-radius: 10px;
    padding: 12px 14px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    z-index: 100;
    color: #cfd3da;
    font-size: 12px;
  }
  .context-pop-head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 8px; }
  .context-pop-title { font-weight: 600; color: #e6e8eb; font-size: 12px; }
  .context-pop-model { color: #8a93a6; font-size: 10px; text-transform: uppercase; letter-spacing: 0.5px; }
  .context-bar-row { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
  .context-bar { flex: 1; height: 6px; background: rgba(255, 255, 255, 0.06); border-radius: 3px; overflow: hidden; }
  .context-bar-fill { height: 100%; border-radius: 3px; transition: width 0.3s ease; }
  .context-bar-fill.low { background: linear-gradient(90deg, #7ac98a, #9ec4a8); }
  .context-bar-fill.mid { background: linear-gradient(90deg, #d8c89a, #e0d2a0); }
  .context-bar-fill.high { background: linear-gradient(90deg, #d8a89a, #e0b89a); }
  .context-bar-fill.crit { background: linear-gradient(90deg, #d88a8a, #e8a0a0); }
  .context-bar-pct { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 11px; min-width: 38px; text-align: right; color: #b6bcc7; }
  .context-numbers { display: flex; align-items: center; gap: 6px; font-size: 11px; color: #8a93a6; margin-bottom: 10px; }
  .context-numbers b { color: #cfd3da; font-weight: 600; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; }
  .context-numbers-sep { color: #3a414b; }
  .context-breakdown { display: flex; flex-direction: column; gap: 2px; max-height: 140px; overflow-y: auto; margin-bottom: 10px; padding: 4px 0; border-top: 1px solid rgba(255, 255, 255, 0.06); border-bottom: 1px solid rgba(255, 255, 255, 0.06); }
  .context-row { display: flex; align-items: center; gap: 8px; padding: 3px 0; font-size: 10px; line-height: 1.3; }
  .context-row-role { flex: 0 0 56px; text-transform: uppercase; letter-spacing: 0.4px; font-size: 9px; opacity: 0.7; }
  .context-row-role.user { color: #d8b09a; }
  .context-row-role.assistant { color: #9ab8d8; }
  .context-row-role.system { color: #8a93a6; }
  .context-row-preview { flex: 1; color: #8a93a6; overflow: hidden; text-overflow: ellipsis; display: -webkit-box; -webkit-line-clamp: 1; -webkit-box-orient: vertical; }
  .context-row-tokens { flex: 0 0 auto; color: #b6bcc7; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; }
  .context-row-more { color: #6c7280; justify-content: center; padding: 4px 0; }
  .context-pop-actions { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .context-action { background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.10); border-radius: 6px; color: #cfd3da; font-size: 11px; padding: 4px 10px; cursor: pointer; font-family: inherit; }
  .context-action:hover { background: rgba(216, 138, 138, 0.12); border-color: rgba(216, 138, 138, 0.35); color: #f0c9c9; }
  .context-action.primary { background: rgba(176, 120, 120, 0.18); border-color: rgba(176, 120, 120, 0.45); color: #f5d8d8; }
  .context-action.primary:hover { background: rgba(176, 120, 120, 0.30); color: #fff; }
  .context-hint { color: #6c7280; font-size: 10px; font-style: italic; }

  /* ---- context popover: breakdown by kind ---- */
  .context-bd-title { font-size: 10px; text-transform: uppercase; letter-spacing: 0.6px; color: #6c7280; margin: 8px 0 4px; }
  .context-bd-list { display: flex; flex-direction: column; gap: 3px; margin-bottom: 8px; }
  .context-bd-row { display: grid; grid-template-columns: 8px 1fr 90px 40px 32px; align-items: center; gap: 6px; font-size: 10.5px; }
  .context-bd-dot { width: 8px; height: 8px; border-radius: 50%; }
  .context-bd-label { color: #cfd3da; }
  .context-bd-mini { position: relative; height: 4px; background: rgba(255, 255, 255, 0.06); border-radius: 2px; overflow: hidden; }
  .context-bd-mini-fill { position: absolute; inset: 0 auto 0 0; border-radius: inherit; transition: width 0.2s ease; }
  .context-bd-tok { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; color: #b6bcc7; text-align: right; }
  .context-bd-pct { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; color: #6c7280; text-align: right; }
  .context-remaining { color: #6c7280; }
  .context-cost { display: flex; align-items: center; gap: 4px; font-size: 10.5px; color: #8a93a6; padding: 4px 6px; background: rgba(255, 255, 255, 0.03); border-radius: 5px; margin-bottom: 6px; }
  .context-cost-label { color: #f5d8a8; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; }
  .context-cost-sep { color: #3a414b; }
  .context-cost-sub { color: #6c7280; }

  /* ---- context popover: tabs ---- */
  .context-tabs {
    display: flex; gap: 4px; margin-bottom: 10px;
    padding: 3px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 8px;
  }
  .context-tab {
    flex: 1;
    background: transparent;
    border: 0;
    color: #8a93a6;
    font-size: 11px;
    font-family: inherit;
    padding: 5px 8px;
    border-radius: 5px;
    cursor: pointer;
    display: inline-flex; align-items: center; justify-content: center; gap: 5px;
    transition: background 0.12s, color 0.12s;
  }
  .context-tab:hover { color: #cfd3da; background: rgba(255, 255, 255, 0.04); }
  .context-tab.active { background: rgba(255, 255, 255, 0.10); color: #e6e8eb; }
  .context-tab-count {
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 9px;
    background: rgba(255, 255, 255, 0.08);
    border-radius: 999px;
    padding: 1px 6px;
    color: #b6bcc7;
  }
  .context-tab.active .context-tab-count { background: rgba(255, 255, 255, 0.15); color: #fff; }

  /* ---- context popover: real content view ---- */
  .context-content { display: flex; flex-direction: column; gap: 8px; }
  .context-content-meta {
    display: flex; align-items: center; gap: 6px;
    font-size: 11px; color: #8a93a6;
    padding-bottom: 6px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }
  .context-content-meta b { color: #cfd3da; font-weight: 600; font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; }
  .context-content-meta-sep { color: #3a414b; }
  .context-content-spacer { flex: 1; }
  .context-copy {
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.10);
    border-radius: 6px;
    color: #cfd3da;
    font-size: 10px;
    padding: 3px 8px;
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, border-color 0.12s, color 0.12s;
  }
  .context-copy:hover { background: rgba(255, 255, 255, 0.10); border-color: rgba(255, 255, 255, 0.20); }
  .context-copy.ok { background: rgba(122, 201, 138, 0.12); border-color: rgba(122, 201, 138, 0.35); color: #c8e0d0; }
  .context-copy.err { background: rgba(216, 138, 138, 0.12); border-color: rgba(216, 138, 138, 0.35); color: #f0c9c9; }

  .context-content-list {
    display: flex; flex-direction: column; gap: 6px;
    max-height: 260px; overflow-y: auto;
    padding-right: 2px;
  }
  .context-content-item {
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 6px;
    padding: 6px 8px;
    transition: background 0.12s, border-color 0.12s, box-shadow 0.4s ease;
    cursor: pointer;
  }
  .context-content-item:hover { background: rgba(255, 255, 255, 0.06); border-color: rgba(201, 160, 160, 0.30); }
  .context-content-item:focus-visible { outline: 2px solid rgba(201, 160, 160, 0.55); outline-offset: 1px; }
  .context-content-item.highlight { background: rgba(245, 181, 107, 0.18); border-color: rgba(245, 181, 107, 0.55); box-shadow: 0 0 0 3px rgba(245, 181, 107, 0.20); }
  .context-content-item.system {
    background: rgba(110, 140, 200, 0.06);
    border-color: rgba(110, 140, 200, 0.18);
  }
  .context-item-copy {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    color: #8a93a6;
    font-size: 12px;
    line-height: 1;
    padding: 2px 5px;
    cursor: pointer;
    font-family: inherit;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
  }
  .context-item-copy:hover { background: rgba(255, 255, 255, 0.08); color: #e6e8eb; border-color: rgba(255, 255, 255, 0.12); }
  .context-item-copy.ok { color: #7ac98a; background: rgba(122, 201, 138, 0.10); border-color: rgba(122, 201, 138, 0.30); }
  .context-content-head {
    display: flex; align-items: center; gap: 8px;
    margin-bottom: 4px;
    font-size: 10px;
  }
  .context-content-tokens, .context-content-chars {
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    color: #6c7280;
  }
  .context-content-text {
    margin: 0;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 11px;
    line-height: 1.45;
    color: #b6bcc7;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 180px;
    overflow-y: auto;
  }
  .context-content-item.system .context-content-text { color: #c8d0e0; }
  .context-content-empty {
    padding: 18px 10px;
    text-align: center;
    color: #6c7280;
    font-size: 11px;
    font-style: italic;
  }
  .context-content-foot {
    font-size: 10px;
    color: #6c7280;
    padding-top: 6px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
    line-height: 1.4;
  }
  .context-content-foot code {
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 10px;
    background: rgba(255, 255, 255, 0.05);
    padding: 1px 4px;
    border-radius: 3px;
    color: #b6bcc7;
  }
  .context-content-list::-webkit-scrollbar,
  .context-content-text::-webkit-scrollbar { width: 6px; height: 6px; }
  .context-content-list::-webkit-scrollbar-thumb,
  .context-content-text::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.10); border-radius: 3px; }
  .context-content-list::-webkit-scrollbar-thumb:hover,
  .context-content-text::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.20); }

  .modal-bg { position: fixed; inset: 0; background: rgba(8, 10, 14, 0.65); display: flex; align-items: center; justify-content: center; z-index: 100; }
  .modal { background: #181b21; border: 1px solid #2c313a; border-radius: 10px; padding: 20px 24px; box-shadow: 0 8px 28px rgba(0, 0, 0, 0.5); color: #e6e8eb; }
  .voice-bar { display: flex; align-items: center; gap: 8px; margin-top: 4px; padding: 0 4px; justify-content: flex-end; }
  .model-chip { background: #1c2027; border: 1px solid #2c313a; color: #b6bcc7; padding: 3px 10px; border-radius: 999px; font-size: 11px; font-family: 'Cascadia Code', monospace; cursor: pointer; }
  .model-chip:hover { background: #2c313a; color: #e6e8eb; }
  .model-chip.warn { border-color: #5a3f1f; color: #f5b56b; background: #2a2018; }
  .download-info { font-size: 11px; color: #88c0ff; font-family: 'Cascadia Code', monospace; }
  .download-info.done { color: #8ad498; }
  .model-panel { position: absolute; right: 12px; bottom: 110px; width: 360px; max-height: 360px; overflow-y: auto; background: #181b21; border: 1px solid #2c313a; border-radius: 8px; padding: 10px 12px; box-shadow: 0 4px 18px rgba(0, 0, 0, 0.4); z-index: 20; }
  .model-panel-head { display: flex; justify-content: space-between; align-items: center; margin-bottom: 4px; }
  .model-panel-head strong { color: #e6e8eb; font-size: 13px; }
  .model-panel .link { background: transparent; border: none; color: #8a929c; font-size: 18px; cursor: pointer; padding: 0 4px; }
  .model-panel .muted { color: #6a7280; font-size: 11px; margin: 4px 0 8px; }
  .model-panel code { font-family: 'Cascadia Code', monospace; font-size: 10px; color: #88c0ff; }
  .model-panel ul { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .model-panel li { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 6px 8px; background: #1c2027; border-radius: 6px; }
  .model-panel li.active-model { background: #1d2a3a; border: 1px solid #2c4a6a; }
  .model-panel li.installing { background: #2a2018; border: 1px solid #5a3f1f; }
  .model-line { display: flex; align-items: center; gap: 8px; font-size: 11px; }
  .model-id { font-family: 'Cascadia Code', monospace; color: #e6e8eb; }
  .model-tier { color: #8a929c; }
  .model-size { color: #6a7280; margin-left: auto; }
  .model-action { flex: 0 0 auto; }
  .model-action .badge { background: #2c4a6a; color: #88c0ff; padding: 2px 8px; border-radius: 999px; font-size: 10px; text-transform: uppercase; }
  .model-action .primary { background: #4a7cff; color: white; border: none; padding: 3px 10px; border-radius: 4px; font-size: 11px; cursor: pointer; }
  .model-action .primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .model-action .secondary { background: #2c313a; color: #e6e8eb; border: none; padding: 3px 10px; border-radius: 4px; font-size: 11px; cursor: pointer; }
  .download-row { margin-top: 8px; display: flex; flex-direction: column; gap: 4px; }
  .progress-bar { width: 100%; height: 4px; background: #2c313a; border-radius: 2px; overflow: hidden; }
  .progress-bar .fill { height: 100%; background: linear-gradient(90deg, #4a7cff, #88c0ff); transition: width 200ms ease; }
  .model-modal { max-width: 420px; text-align: left; }
  .model-modal h3 { margin: 0 0 8px; font-size: 16px; }
  .model-modal p { margin: 0 0 12px; font-size: 13px; color: #b6bcc7; line-height: 1.5; }
  .modal-actions { display: flex; align-items: center; gap: 8px; margin-top: 8px; }
  .modal-actions .primary { background: #4a7cff; color: white; border: none; padding: 8px 16px; border-radius: 6px; font-size: 13px; font-weight: 600; cursor: pointer; }
  .modal-actions .primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .modal-actions .secondary { background: #2c313a; color: #e6e8eb; border: none; padding: 8px 16px; border-radius: 6px; font-size: 13px; cursor: pointer; }
  .modal-actions .secondary:disabled { opacity: 0.5; cursor: not-allowed; }
  .modal-actions .link { background: transparent; border: none; color: #6c7280; font-size: 20px; cursor: pointer; margin-left: auto; padding: 0 6px; }

  @media (max-width: 760px) {
    .bar { flex-wrap: wrap; row-gap: 6px; }
    .left .sub { display: none; }
    .model-pick .model-label { display: none; }
    .model-pick select { max-width: 130px; }
    .hint { flex-wrap: wrap; }
  }
  @media (max-width: 560px) {
    .model-pick { display: none; }
    .composer { padding: 10px 12px 12px; }
  }
  /* ---- Fusion Research: stack sidebar on narrow screens ---- */
  @media (max-width: 920px) {
    .research-view { grid-template-columns: 1fr; gap: 14px; }
    .research-sidebar { position: static; max-height: none; }
    .research-grid { grid-template-columns: 1fr; }
  }
  @media (max-width: 600px) {
    .news-card { flex-direction: column-reverse; padding: 12px; }
    .news-card-thumb { width: 100%; flex: 0 0 auto; height: 160px; }
    .news-source-name { max-width: 180px; }
    .research-query { flex-direction: column; }
    .research-run { width: 100%; }
  }

  /* ---- Fusion Research v2: query bar ---- */
  .research-h-row { display: flex; flex-direction: column; gap: 4px; margin-bottom: 12px; }
  .research-query {
    display: flex; gap: 8px; margin-bottom: 10px;
  }
  .research-input {
    flex: 1; min-width: 0;
    background: #0f1217; color: #e6e8eb;
    border: 1px solid #2c313a; border-radius: 8px;
    padding: 9px 12px; font-size: 13px; font-family: inherit;
    outline: none;
  }
  .research-input:focus { border-color: #f5b56b; }
  .research-input:disabled { opacity: 0.5; }
  .research-run {
    background: #f5b56b; color: #1a1108; border: 0;
    border-radius: 8px; padding: 0 18px;
    font-size: 13px; font-weight: 600; cursor: pointer;
    white-space: nowrap;
  }
  .research-run:hover:not(:disabled) { opacity: 0.9; }
  .research-run:disabled { opacity: 0.4; cursor: not-allowed; }

  /* ---- Fusion Research v2: source tabs ---- */
  .research-sources {
    display: flex; gap: 4px; flex-wrap: wrap;
    margin-bottom: 14px; padding-bottom: 12px;
    border-bottom: 1px solid var(--code-pane-border);
  }
  .src-tab {
    display: inline-flex; align-items: center; gap: 6px;
    background: transparent; color: var(--code-pane-muted);
    border: 1px solid transparent; border-radius: 6px;
    padding: 5px 10px; font-size: 12px; cursor: pointer;
    transition: background 0.1s, border-color 0.1s, color 0.1s;
  }
  .src-tab:hover { background: var(--code-pane-hover); color: var(--code-pane-fg); }
  .src-tab.on { background: var(--code-pane-active); color: var(--code-pane-fg); border-color: var(--border-strong); }
  .src-ico { font-size: 13px; }
  .src-count {
    background: var(--bg-active);
    color: var(--text-muted); font-size: 10px; padding: 1px 6px;
    border-radius: 8px; min-width: 18px; text-align: center;
  }
  .src-tab.on .src-count { background: var(--code-cta-soft); color: var(--code-cta); }
  .src-status {
    font-size: 8px; line-height: 1;
    color: var(--text-faint); margin-left: 1px;
  }
  .src-status[data-status="pending"] { color: var(--code-cta); animation: pulse 1.2s ease-in-out infinite; }
  .src-status[data-status="ok"] { color: var(--success); }
  .src-status[data-status="error"] { color: var(--danger); }
  @keyframes pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 1; } }

  /* ---- Fusion Research v2: card grid ---- */
  .research-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 10px;
    align-content: start;
  }
  .r-card {
    display: flex; flex-direction: column;
    background: var(--code-pane-card);
    border: 1px solid var(--code-pane-border);
    border-radius: 8px;
    padding: 12px 14px;
    transition: border-color 0.12s, transform 0.12s, box-shadow 0.12s;
    min-height: 140px;
  }
  .r-card:hover { border-color: var(--border-strong); transform: translateY(-1px); box-shadow: var(--shadow-1); }
  .r-card[data-source="workspace"] { border-left: 3px solid var(--source-workspace); }
  .r-card[data-source="web"]       { border-left: 3px solid var(--source-web); }
  .r-card[data-source="news"]      { border-left: 3px solid var(--source-news); }
  .r-card-head { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
  .r-source-badge {
    display: inline-flex; align-items: center; gap: 4px;
    padding: 2px 8px; border-radius: 4px;
    font-size: 10px; font-weight: 600;
    text-transform: lowercase;
    letter-spacing: 0.02em;
  }
  .r-time { color: var(--text-faint); font-size: 10px; }
  .r-title {
    margin: 0 0 6px 0;
    font-size: 14px; font-weight: 600; line-height: 1.35;
    color: var(--text);
    display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .r-snippet {
    margin: 0 0 10px 0;
    font-size: 12px; line-height: 1.5; color: var(--text-muted);
    flex: 1; min-height: 0;
    display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .r-card-foot {
    display: flex; align-items: center; gap: 6px;
    padding-top: 8px; border-top: 1px solid var(--border);
  }
  .r-host {
    font-size: 11px; color: var(--text-faint); font-family: ui-monospace, monospace;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 50%;
  }
  .r-spacer { flex: 1; }
  .r-actions { display: flex; gap: 4px; }
  .r-action {
    background: transparent; color: var(--text-muted);
    border: 1px solid var(--code-pane-border); border-radius: 5px;
    padding: 3px 8px; font-size: 11px; cursor: pointer;
  }
  .r-action:hover:not(:disabled) { background: var(--code-pane-hover); color: var(--text); }
  .r-action:disabled { opacity: 0.4; cursor: not-allowed; }
  .r-action.primary { background: var(--code-cta-soft); color: var(--code-cta); border-color: var(--code-cta-soft); }

  /* ---- Skeleton ---- */
  .r-skel { pointer-events: none; min-height: 140px; }
  .r-skel-bar {
    background: linear-gradient(90deg, var(--bg-elevated) 0%, var(--code-pane-hover) 50%, var(--bg-elevated) 100%);
    background-size: 200% 100%;
    animation: shimmer 1.4s linear infinite;
    border-radius: 4px;
    height: 12px; margin-bottom: 8px;
  }
  .r-skel-src { width: 30%; height: 16px; }
  .r-skel-title { width: 80%; height: 16px; }
  .r-skel-snippet { width: 95%; }
  .r-skel-snippet.short { width: 60%; }
  @keyframes shimmer { 0% { background-position: 200% 0; } 100% { background-position: -200% 0; } }

  /* ---- Empty state ---- */
  .r-empty {
    grid-column: 1 / -1;
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    gap: 6px; padding: 40px 20px;
    color: var(--text-faint); text-align: center;
  }
  .r-empty-icon { font-size: 32px; opacity: 0.5; }
  .r-empty-h { font-size: 14px; color: var(--text-muted); font-weight: 500; }
  .r-empty-sub { font-size: 12px; }
  .r-empty-chips { display: flex; flex-wrap: wrap; gap: 6px; justify-content: center; margin-top: 8px; }
  .r-empty-chip {
    background: var(--code-pane-card); color: var(--text-muted);
    border: 1px solid var(--code-pane-border); border-radius: 16px;
    padding: 4px 12px; font-size: 12px; cursor: pointer;
  }
  .r-empty-chip:hover { background: var(--code-pane-hover); color: var(--text); border-color: var(--code-cta); }

  /* ---- Cache stats in sidebar ---- */
  .cache-stats {
    display: flex; align-items: center; gap: 6px;
    margin-top: auto; padding: 8px 10px;
    background: var(--code-pane-card); border: 1px solid var(--code-pane-border); border-radius: 6px;
    font-size: 11px; color: var(--code-pane-muted);
  }
  .cache-icon { font-size: 13px; }
  .cache-text { flex: 1; }
  .cache-clear {
    background: transparent; color: var(--text-faint);
    border: 0; cursor: pointer; font-size: 14px;
    padding: 0 4px; line-height: 1;
  }
  .cache-clear:hover { color: var(--danger); }

  /* ---- Read-more modal ---- */
  .modal-backdrop {
    position: fixed; inset: 0;
    background: var(--bg-overlay);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
    padding: 20px;
  }
  .modal-card {
    background: var(--bg-elevated); color: var(--text);
    border: 1px solid var(--border); border-radius: 10px;
    width: 720px; max-width: 100%; max-height: 86vh;
    display: flex; flex-direction: column;
    box-shadow: var(--shadow-2);
    overflow: hidden;
  }
  .modal-head {
    display: flex; align-items: flex-start; justify-content: space-between;
    gap: 12px; padding: 16px 20px;
    border-bottom: 1px solid var(--border);
  }
  .modal-head h3 {
    margin: 0; font-size: 16px; font-weight: 600;
    color: var(--code-cta); line-height: 1.4;
  }
  .modal-close {
    background: transparent; color: var(--text-faint);
    border: 0; font-size: 22px; line-height: 1;
    padding: 0 4px; cursor: pointer;
  }
  .modal-close:hover { color: var(--text); }
  .modal-host {
    padding: 6px 20px 12px;
    font-size: 11px; color: var(--text-faint); font-family: ui-monospace, monospace;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .modal-body { flex: 1; overflow: auto; padding: 0 20px 20px; }
  .modal-loading {
    display: flex; align-items: center; gap: 10px;
    padding: 40px 0; justify-content: center;
    color: var(--text-muted); font-size: 13px;
  }
  .modal-text {
    margin: 0; padding: 12px 14px;
    background: var(--code-pane-bg); border: 1px solid var(--code-pane-border); border-radius: 6px;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 12px; line-height: 1.55; color: var(--text);
    white-space: pre-wrap; word-break: break-word;
    max-height: 60vh; overflow: auto;
  }

  /* ===========================================================
   * Code mode (Cursor-like 3-column layout: file tree | chat | preview)
   * =========================================================== */
  .code-grid {
    display: grid;
    grid-template-columns: 280px 1fr 360px;
    gap: 0;
    height: 100%;
    min-height: 0;
  }
  @media (max-width: 1100px) {
    .code-grid { grid-template-columns: 240px 1fr 320px; }
  }
  @media (max-width: 900px) {
    .code-grid { grid-template-columns: 220px 1fr; }
    .preview-pane { display: none; }
  }

  /* --- File tree pane --- */
  .file-tree-pane {
    background: var(--code-pane-bg);
    border-right: 1px solid var(--code-pane-border);
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  .ft-head {
    display: flex; align-items: center; gap: 6px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--code-pane-border);
  }
  .ft-title { display: flex; align-items: center; gap: 6px; min-width: 0; flex: 1; }
  .ft-icon { font-size: 14px; }
  .ft-name {
    font-size: 12px; font-weight: 600; color: var(--text);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .ft-actions { display: flex; gap: 2px; }
  .ft-btn {
    background: transparent; border: 1px solid transparent; color: var(--text-muted);
    width: 26px; height: 26px; border-radius: 4px; cursor: pointer; font-size: 13px;
    display: flex; align-items: center; justify-content: center;
  }
  .ft-btn:hover:not(:disabled) { background: var(--code-pane-hover); color: var(--text); }
  .ft-btn:disabled { opacity: 0.35; cursor: not-allowed; }
  .ft-btn.danger:hover { background: var(--danger); color: var(--text-inverse); }

  .ft-empty {
    flex: 1; display: flex; flex-direction: column;
    align-items: center; justify-content: center;
    padding: 24px 16px; text-align: center; color: var(--text-muted);
  }
  .ft-empty-h { font-size: 13px; color: var(--text); margin-bottom: 4px; }
  .ft-empty-sub { font-size: 11px; margin-bottom: 16px; }
  .ft-empty-actions { display: flex; flex-direction: column; gap: 6px; width: 100%; }
  .ft-cta {
    background: var(--code-cta); color: var(--code-cta-fg); border: 1px solid var(--code-cta);
    padding: 8px 12px; border-radius: 5px; font-weight: 600; font-size: 12px; cursor: pointer;
  }
  .ft-cta.ghost {
    background: transparent; color: var(--text-muted); border-color: var(--code-pane-border);
  }
  .ft-cta.ghost:hover { background: var(--code-pane-hover); color: var(--text); }

  .ft-recent { border-bottom: 1px solid var(--code-pane-border); padding: 4px 0; }
  .ft-recent > summary {
    cursor: pointer; padding: 6px 10px; font-size: 11px;
    color: var(--text-muted); user-select: none;
  }
  .ft-recent > summary:hover { color: var(--text); }
  .ft-recent-list { list-style: none; padding: 0 4px 6px; margin: 0; }
  .ft-recent-item {
    display: block; width: 100%; text-align: left;
    background: transparent; border: 0; color: var(--text-muted);
    padding: 4px 6px; border-radius: 4px; cursor: pointer;
  }
  .ft-recent-item:hover { background: var(--code-pane-hover); color: var(--text); }
  .ft-recent-name { font-size: 12px; font-weight: 600; }
  .ft-recent-path { font-size: 10px; color: var(--text-faint); font-family: ui-monospace, monospace; }

  .ft-list { list-style: none; padding: 6px 4px; margin: 0; overflow: auto; flex: 1; }
  .ft-loading { padding: 6px 10px; color: var(--text-muted); font-size: 11px; }
  .ft-item {
    display: flex; align-items: center; gap: 6px;
    width: 100%; text-align: left;
    background: transparent; border: 1px solid transparent;
    color: var(--text-muted); padding: 3px 6px; border-radius: 4px;
    font-size: 11.5px; cursor: pointer; font-family: ui-monospace, monospace;
  }
  .ft-item:hover { background: var(--code-pane-hover); color: var(--text); }
  .ft-item.dir { color: var(--text-muted); }
  .ft-item-icon { flex: 0 0 auto; }
  .ft-item-name {
    flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }

  /* --- Center chat column --- */
  .code-center {
    display: flex; flex-direction: column;
    min-width: 0; min-height: 0;
  }
  .cc-scroll {
    flex: 1 1 auto; min-height: 0; overflow: auto;
    padding: 16px;
  }
  .empty-chat-hint {
    padding: 32px 16px; text-align: center; color: var(--text-muted);
  }
  .ech-h { font-size: 16px; color: var(--text); margin-bottom: 6px; }
  .ech-sub { font-size: 12px; }

  .cc-input-wrap {
    position: relative;
    flex: 0 0 auto;
    border-top: 1px solid var(--code-pane-border);
    padding: 8px 12px 10px;
    background: var(--code-pane-bg);
  }
  .cc-input-wrap textarea {
    display: block;
    width: 100%; min-height: 36px; max-height: 160px;
    background: var(--code-pane-input); color: var(--text);
    border: 1px solid var(--code-pane-border); border-radius: 8px;
    padding: 8px 44px 8px 12px;
    font-family: inherit; font-size: 13px; line-height: 1.5;
    resize: none; outline: none;
  }
  .cc-input-wrap textarea:focus { border-color: var(--code-cta); }
  .cc-input-wrap .send {
    position: absolute; right: 18px; bottom: 16px;
    background: var(--code-cta); color: var(--code-cta-fg); border: 0;
    width: 30px; height: 30px; border-radius: 6px;
    font-size: 16px; font-weight: 700; cursor: pointer;
  }
  .cc-input-wrap .send:disabled { opacity: 0.4; cursor: not-allowed; }

  .mention-popover {
    position: absolute; bottom: 100%; left: 12px; right: 12px;
    margin-bottom: 4px; max-height: 240px; overflow: auto;
    background: var(--bg-elevated); border: 1px solid var(--border); border-radius: 6px;
    box-shadow: var(--shadow-2);
    z-index: 50;
  }
  .mention-empty { padding: 10px 12px; color: var(--text-muted); font-size: 12px; }
  .mention-item {
    display: flex; align-items: center; gap: 6px;
    width: 100%; text-align: left;
    background: transparent; border: 0; color: var(--text-muted);
    padding: 6px 10px; font-size: 12px; cursor: pointer;
    font-family: ui-monospace, monospace;
  }
  .mention-item:hover { background: var(--code-pane-hover); color: var(--text); }
  .mention-icon { flex: 0 0 auto; }
  .mention-path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  /* --- Preview pane --- */
  .preview-pane {
    background: var(--code-pane-bg);
    border-left: 1px solid var(--code-pane-border);
    display: flex; flex-direction: column;
    min-height: 0; overflow: hidden;
  }
  .pv-head {
    display: flex; align-items: center; gap: 4px;
    padding: 8px 10px; border-bottom: 1px solid var(--code-pane-border);
  }
  .pv-title { font-size: 12px; font-weight: 600; color: var(--text); margin-right: 4px; }
  .pv-port {
    width: 60px; background: var(--code-pane-input); color: var(--text);
    border: 1px solid var(--code-pane-border); border-radius: 4px;
    padding: 3px 6px; font-size: 12px;
  }
  .pv-btn {
    background: transparent; border: 1px solid var(--code-pane-border); color: var(--text-muted);
    width: 28px; height: 26px; border-radius: 4px; cursor: pointer; font-size: 12px;
    display: flex; align-items: center; justify-content: center;
  }
  .pv-btn:hover:not(:disabled) { background: var(--code-pane-hover); color: var(--text); }
  .pv-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .pv-btn.primary { background: var(--code-cta); color: var(--code-cta-fg); border-color: var(--code-cta); font-weight: 600; }
  .pv-error { padding: 8px 10px; color: var(--danger); font-size: 11px; }
  .pv-frame {
    flex: 1 1 auto; border: 0; width: 100%; height: 100%;
    background: var(--bg);
  }
  .pv-empty {
    flex: 1; display: flex; flex-direction: column;
    align-items: center; justify-content: center;
    padding: 24px 16px; text-align: center; color: var(--text-muted);
  }
  .pv-empty-h { font-size: 13px; color: var(--text); margin-bottom: 4px; }
  .pv-empty-sub { font-size: 11px; }

  /* --- File edit / read cards (also used in regular chat mode) --- */
  .edit-card {
    border: 1px solid var(--code-pane-border); border-radius: 6px;
    background: var(--code-pane-card); overflow: hidden;
  }
  .edit-card.accepted { border-color: rgba(94, 146, 114, 0.55); }
  .edit-card.rejected { border-color: rgba(168, 81, 81, 0.55); opacity: 0.65; }
  .edit-card-head {
    display: flex; align-items: center; gap: 6px;
    padding: 6px 10px; background: var(--code-pane-bg); border-bottom: 1px solid var(--code-pane-border);
  }
  .edit-card-icon { font-size: 13px; }
  .edit-card-path {
    flex: 1; font-family: ui-monospace, monospace; font-size: 12px;
    color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .edit-card-state {
    font-size: 10px; color: var(--text-muted); text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .diff-body {
    margin: 0; padding: 8px 12px;
    font-family: ui-monospace, monospace; font-size: 11px; line-height: 1.5;
    color: var(--text); max-height: 320px; overflow: auto;
    background: var(--code-pane-bg);
  }
  .edit-card-pending { padding: 10px 12px; color: var(--text-muted); font-size: 11px; }
  .edit-card-actions {
    display: flex; align-items: center; gap: 6px;
    padding: 6px 10px; border-top: 1px solid var(--code-pane-border);
    background: var(--code-pane-card);
  }
  .ea-btn {
    background: var(--code-pane-active); color: var(--text); border: 1px solid var(--code-pane-border);
    padding: 4px 10px; border-radius: 4px; font-size: 11px; cursor: pointer;
  }
  .ea-btn.reject:hover { background: var(--danger); color: var(--text-inverse); border-color: var(--danger); }
  .ea-btn:disabled { opacity: 0.5; cursor: default; }
  .ea-note { font-size: 11px; color: var(--text-muted); }

  .read-card {
    border: 1px solid var(--code-pane-border); border-radius: 6px; background: var(--code-pane-card); overflow: hidden;
  }
  .read-card-head {
    display: flex; align-items: center; gap: 6px;
    width: 100%; text-align: left;
    background: transparent; border: 0; color: var(--text-muted);
    padding: 6px 10px; cursor: pointer; font-size: 12px;
  }
  .read-card-head:hover { background: var(--code-pane-hover); color: var(--text); }
  .read-card-icon { font-size: 13px; }
  .read-card-path {
    flex: 1; font-family: ui-monospace, monospace;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .read-card-meta { font-size: 11px; color: var(--text-muted); }
  .read-card-chevron { font-size: 11px; color: var(--text-muted); }
  .read-card-body {
    margin: 0; padding: 8px 12px;
    font-family: ui-monospace, monospace; font-size: 11px; line-height: 1.5;
    color: var(--text); max-height: 360px; overflow: auto;
    background: var(--code-pane-bg); border-top: 1px solid var(--code-pane-border);
  }
  .video-frame-body {
    display: block;
    width: 100%;
    max-height: 480px;
    object-fit: contain;
    background: var(--code-pane-bg);
    border-top: 1px solid var(--code-pane-border);
  }

  /* ---- ask_user card ---- */
  .ask-user-card {
    display: flex; flex-direction: column; gap: 8px;
    padding: 10px 12px;
    background: rgba(168, 130, 200, 0.10);
    border: 1px solid rgba(168, 130, 200, 0.32);
    border-radius: 10px;
    color: #c4a8ff;
    max-width: 100%;
  }
  .ask-user-card.answered { background: rgba(94, 146, 114, 0.10); border-color: rgba(94, 146, 114, 0.30); color: #b4d4c0; }
  .ask-user-head { display: flex; align-items: center; gap: 8px; font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; opacity: 0.85; }
  .ask-user-emoji { font-size: 14px; }
  .ask-user-label { font-weight: 600; }
  .ask-user-q { font-size: 14px; line-height: 1.5; color: #e6e8eb; }
  .ask-user-options { display: flex; flex-wrap: wrap; gap: 6px; }
  .ask-option {
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(168, 130, 200, 0.30);
    border-radius: 8px;
    color: #c4a8ff;
    font: inherit; font-size: 12px;
    padding: 5px 12px;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s, color 0.12s, transform 0.06s;
  }
  .ask-option:hover { background: rgba(168, 130, 200, 0.18); color: #fff; border-color: rgba(168, 130, 200, 0.55); }
  .ask-option:active { transform: translateY(1px); }
  .ask-option-freetext { color: #b6bcc7; border-color: rgba(255, 255, 255, 0.12); }
  .ask-option-freetext:hover { color: #e6e8eb; background: rgba(255, 255, 255, 0.08); border-color: rgba(255, 255, 255, 0.20); }
  .ask-user-answered { display: flex; align-items: flex-start; gap: 6px; font-size: 12px; line-height: 1.4; opacity: 0.85; }
  .ask-user-answered-emoji { flex: 0 0 auto; }
  :global(html:not(.theme-dark)) .ask-user-card { background: rgba(168, 130, 200, 0.10); border-color: rgba(168, 130, 200, 0.40); color: #6a4a8a; }
  :global(html:not(.theme-dark)) .ask-user-card.answered { background: rgba(94, 146, 114, 0.10); border-color: rgba(94, 146, 114, 0.40); color: #2f6a45; }
  :global(html:not(.theme-dark)) .ask-user-q { color: #1a1c20; }
  :global(html:not(.theme-dark)) .ask-option { background: rgba(255, 255, 255, 0.7); border-color: rgba(168, 130, 200, 0.45); color: #6a4a8a; }
  :global(html:not(.theme-dark)) .ask-option:hover { background: rgba(168, 130, 200, 0.20); color: #2d2418; }
  :global(html:not(.theme-dark)) .ask-option-freetext { color: #5a6068; border-color: rgba(60, 50, 40, 0.18); }

  /* ---- light-theme overrides for the tool pill ---- */
  :global(html:not(.theme-dark)) .tool-pill { background: rgba(176, 120, 120, 0.06); border-color: rgba(176, 120, 120, 0.30); color: #1a1c20; }
  :global(html:not(.theme-dark)) .tool-pill.pending { background: rgba(245, 181, 107, 0.10); border-color: rgba(245, 181, 107, 0.50); }
  :global(html:not(.theme-dark)) .tool-pill.error { background: rgba(216, 122, 122, 0.12); border-color: rgba(216, 122, 122, 0.45); }
  :global(html:not(.theme-dark)) .tool-name { color: #8a4848; }
  :global(html:not(.theme-dark)) .tool-pill.error .tool-name { color: #a04040; }
  :global(html:not(.theme-dark)) .tool-args-preview { color: #5a6068; }
  :global(html:not(.theme-dark)) .tool-status.pending { background: rgba(245, 181, 107, 0.22); color: #8a5a1f; }
  :global(html:not(.theme-dark)) .tool-args { background: rgba(60, 50, 40, 0.04); border-top-color: rgba(176, 120, 120, 0.20); color: #2d2418; }
  :global(html:not(.theme-dark)) .tool-err-text { color: #a04040; }

  /* ---- light-theme overrides for the context popover ----
     The dark defaults use rgba(255,255,255,...) surfaces and muted
     tints that vanish on cream. Flip to the cream-side palette. */
  :global(html:not(.theme-dark)) .context-btn {
    background: rgba(60, 50, 40, 0.05);
    color: #5a6068;
  }
  :global(html:not(.theme-dark)) .context-btn:hover {
    background: rgba(176, 120, 120, 0.10);
  }
  :global(html:not(.theme-dark)) .context-pop {
    background: #fdfcf9;
    border-color: #d6d0c2;
    box-shadow: 0 8px 32px rgba(60, 50, 40, 0.18), 0 2px 6px rgba(60, 50, 40, 0.10);
  }
  :global(html:not(.theme-dark)) .context-pop-title,
  :global(html:not(.theme-dark)) .context-pop-model { color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-tab { color: #5a6068; }
  :global(html:not(.theme-dark)) .context-tab:hover { background: rgba(60, 50, 40, 0.05); color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-tab.active { background: rgba(176, 120, 120, 0.18); color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-tab-count { background: rgba(60, 50, 40, 0.10); color: #5a6068; }
  :global(html:not(.theme-dark)) .context-bar { background: rgba(60, 50, 40, 0.10); }
  :global(html:not(.theme-dark)) .context-numbers b { color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-bd-label { color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-bd-mini { background: rgba(60, 50, 40, 0.10); }
  :global(html:not(.theme-dark)) .context-bd-tok { color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-cost { background: rgba(60, 50, 40, 0.04); }
  :global(html:not(.theme-dark)) .context-content-item { background: rgba(60, 50, 40, 0.03); border-color: rgba(60, 50, 40, 0.10); }
  :global(html:not(.theme-dark)) .context-content-item:hover { background: rgba(176, 120, 120, 0.08); }
  :global(html:not(.theme-dark)) .context-content-text { color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-action { background: rgba(60, 50, 40, 0.05); border-color: rgba(60, 50, 40, 0.12); color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-action.primary { background: rgba(176, 120, 120, 0.18); border-color: rgba(176, 120, 120, 0.45); color: #1a1c20; }
  :global(html:not(.theme-dark)) .context-row-role.user { color: #8a4848; }
  :global(html:not(.theme-dark)) .context-row-role.assistant { color: #2f6a45; }
  :global(html:not(.theme-dark)) .context-row-preview { color: #5a6068; }
  :global(html:not(.theme-dark)) .context-content-item.system { background: rgba(110, 140, 200, 0.10); border-color: rgba(110, 140, 200, 0.30); }
  :global(html:not(.theme-dark)) .context-content-item.system .context-content-text { color: #2d2418; }
  :global(html:not(.theme-dark)) .context-content-item.highlight { background: rgba(245, 181, 107, 0.22); border-color: rgba(245, 181, 107, 0.55); }
  :global(html:not(.theme-dark)) .context-content-meta-sep,
  :global(html:not(.theme-dark)) .context-cost-sep,
  :global(html:not(.theme-dark)) .context-numbers-sep { color: #cfc8b8; }
</style>
