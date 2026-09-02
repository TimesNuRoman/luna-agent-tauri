<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import VideoMode from './VideoMode.svelte';
  import Chat from './Chat.svelte';
  import Settings from './Settings.svelte';
  import Memory from './Memory.svelte';
  import ThreeD from './ThreeD.svelte';
  import SelfEvolution from './SelfEvolution.svelte';
  import TasksSidebar from './TasksSidebar.svelte';
  import PlansSidebar from './PlansSidebar.svelte';
  import DesignStudio from './DesignStudio.svelte';
  import Daimonion from './Daimonion.svelte';
  import AzazelPanel from './AzazelPanel.svelte';
  import { appWindow, getTelegramStatus } from './lib/tauri';
  import type { TelegramStatus } from './lib/tauri';
  import { apiKeyStatus, refreshKeyStatus } from './lib/keyStore';
  import type { Plan } from './lib/planStore';
  import { onTaskFinished } from './lib/taskClient';
  import { statusLabel } from './lib/taskClient';
  import { azazelStore, runningTaskIds } from './lib/stores/azazel';

  type TabId = 'chat' | 'video' | 'memory' | 'settings' | 'three_d' | 'self_evolution' | 'daimonion' | 'azazel';
  type SidebarMode = 'none' | 'tasks' | 'plans' | 'design';
  let activeTab: TabId = 'chat';
  // One sidebar slot. Default to `plans` — the new feature we want
  // the user to discover first. The old boolean was renamed to a
  // 3-state enum so each sidebar (Tasks / Plans / Design) can be the
  // one visible at a time. A "none" mode hides all three.
  let sidebarMode: SidebarMode = 'plans';

  /** True while a chat stream is in flight, so PlansSidebar can disable
   *  the Run button. Bound two-way from `<Chat bind:busy={chatBusy}>`. */
  let chatBusy = false;
  /** Transient toast message (used as a fallback when the Notification
   * API isn't available, e.g. in some Tauri webview configurations). */
  let toastMessage: string | null = null;
  /** Cleanup callbacks to invoke on unmount. */
  const onDestroyHandlers: Array<() => void> = [];
  /** Imperative ref to the active Chat instance so we can drive
   *  `runPlanFromSidebar(plan)` from sidebar events. Typed as
   *  `any` because we want to keep the Chat import out of the
   *  Svelte script prelude (it's already imported as a default
   *  elsewhere in the project). */
  let chatRef: any = null;

  // `keyMissing` is now derived from the shared store. Chat and Settings
  // also subscribe to the same store, so the pill can't disagree with the
  // badge in Settings or the chat input placeholder.
  $: keyMissing = $apiKeyStatus === 'missing';

  // ---- theme ----
  // Stored in localStorage. 'light' | 'dark' | 'auto'. Default: light.
  // The inline script in index.html applies the class BEFORE first paint
  // to avoid a flash. We re-read it here so reactive code (Settings UI)
  // sees the current value.
  type Theme = 'light' | 'dark' | 'auto';
  const THEME_KEY = 'luna.theme';
  function readTheme(): Theme {
    try {
      const t = localStorage.getItem(THEME_KEY) as Theme | null;
      if (t === 'light' || t === 'dark' || t === 'auto') return t;
    } catch (e) { /* noop */ }
    return 'light';
  }
  let theme: Theme = readTheme();

  function applyTheme(t: Theme) {
    const isDark = t === 'dark'
      || (t === 'auto' && typeof window !== 'undefined'
          && window.matchMedia
          && window.matchMedia('(prefers-color-scheme: dark)').matches);
    document.documentElement.classList.toggle('theme-dark', isDark);
  }
  // Expose for the Settings component (and any other caller) to dispatch
  // a custom event when the theme changes, so they can refresh their UI.
  function setTheme(t: Theme) {
    theme = t;
    try { localStorage.setItem(THEME_KEY, t); } catch (e) { /* noop */ }
    applyTheme(t);
    window.dispatchEvent(new CustomEvent('luna:theme', { detail: t }));
  }

  // Re-apply on system theme change when in 'auto' mode.
  let mq: MediaQueryList | null = null;
  function onSystemThemeChange() {
    if (theme === 'auto') applyTheme('auto');
  }

  onMount(async () => {
    // Pull initial key status from the keyring. Chat and Settings also
    // call this in their own onMount; the keyStore dedupes the IPC call
    // via an in-flight promise so the keyring is hit at most once.
    refreshKeyStatus().catch((e) => console.error('[Luna] getApiKey failed:', e));
    applyTheme(theme);
    // Phase M3: subscribe to background-task completion. We use the
    // browser Notification API (works in the Tauri webview on every
    // desktop platform) to surface a non-modal toast. If the user
    // has switched away from Luna the toast is still visible in the
    // system notification center.
    try {
      const unsubFinished = onTaskFinished((event) => {
        const label = statusLabel(event.status);
        const title = `Luna task: ${label}`;
        const body = event.error ?? `Task ${event.task_id} finished.`;
        if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
          try {
            new Notification(title, { body });
          } catch {
            // Webview may disallow Notification constructor; fall back
            // to a visible in-app toast by setting a transient state.
            toastMessage = `${title} — ${body}`;
            setTimeout(() => (toastMessage = null), 4000);
          }
        } else {
          toastMessage = `${title} — ${body}`;
          setTimeout(() => (toastMessage = null), 4000);
        }
      });
      onDestroyHandlers.push(unsubFinished);
    } catch (e) {
      console.warn('[Luna] could not subscribe to task_finished:', e);
    }
    if (typeof window !== 'undefined' && window.matchMedia) {
      mq = window.matchMedia('(prefers-color-scheme: dark)');
      if (mq.addEventListener) mq.addEventListener('change', onSystemThemeChange);
      else if ((mq as any).addListener) (mq as any).addListener(onSystemThemeChange);
    }
    refreshTg();
    tgPoll = window.setInterval(refreshTg, 5000);
  });

  onDestroy(() => {
    if (mq) {
      if (mq.removeEventListener) mq.removeEventListener('change', onSystemThemeChange);
      else if ((mq as any).removeListener) (mq as any).removeListener(onSystemThemeChange);
    }
    if (tgPoll) clearInterval(tgPoll);
    for (const h of onDestroyHandlers) {
      try {
        h();
      } catch {
        // ignore
      }
    }
  });

  // Telegram bot status (light poll; cheap)
  let tgStatus: TelegramStatus | null = null;
  let tgPoll: number | null = null;
  async function refreshTg() {
    try { tgStatus = await getTelegramStatus(); } catch {}
  }

  function switchTo(tab: TabId) {
    if (activeTab === tab) return;
    activeTab = tab;
  }

  /** Called by PlansSidebar when the user clicks "▶ Запустить" or
   *  "↻ Продолжить". We make sure the chat tab is visible, then
   *  delegate to Chat.runPlanFromSidebar — Chat owns the actual
   *  doChat() flow so the user message, system prompt, and busy
   *  flag all live in one place. */
  async function handleRunPlan(plan: Plan) {
    if (activeTab !== 'chat') switchTo('chat');
    // Small tick so Chat has a chance to mount if we just switched
    // tabs from a non-chat tab (the binding updates after the
    // tab-panel re-renders).
    await Promise.resolve();
    if (chatRef && typeof (chatRef as any).runPlanFromSidebar === 'function') {
      await (chatRef as any).runPlanFromSidebar(plan);
    } else {
      console.warn('[App] chatRef missing runPlanFromSidebar');
    }
  }

  function handleContinuePlan(plan: Plan) {
    // Same flow — Chat's runPlanFromSidebar auto-detects chatLinked
    // and switches to a continue prompt.
    return handleRunPlan(plan);
  }

  function handleSwitchSidebar(e: CustomEvent<{ mode: SidebarMode }>) {
    sidebarMode = e.detail.mode;
  }

  // Tab order used by both the keyboard shortcut (Ctrl/Cmd+1..6,
  // Ctrl+Tab) and the Next/Prev helpers. Kept in one place so adding
  // a new tab is a one-line change.
  const TAB_ORDER: TabId[] = ['chat', 'video', 'memory', 'azazel', 'three_d', 'self_evolution', 'settings'];
  const TAB_HOTKEY: Record<string, TabId> = {
    '1': 'chat',
    '2': 'video',
    '3': 'memory',
    '4': 'azazel',
    '5': 'three_d',
    '6': 'self_evolution',
    '7': 'settings',
  };

  function nextTab(dir: 1 | -1) {
    const i = TAB_ORDER.indexOf(activeTab);
    const ni = (i + dir + TAB_ORDER.length) % TAB_ORDER.length;
    switchTo(TAB_ORDER[ni]);
  }

  // Global keyboard shortcuts. The handler is intentionally
  // non-exclusive — when focus is in a textarea / contenteditable we
  // let the keystroke through so typing digits still works; otherwise
  // we intercept and switch tabs. This is the only reliable way to
  // escape the chat input when the agent is mid-stream.
  function onWindowKey(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (!mod) return;
    // Don't hijack Ctrl+1..6 while the user is editing a text field —
    // they might be quoting a footnote or similar.
    const t = e.target as HTMLElement | null;
    const tag = (t?.tagName || '').toUpperCase();
    const editable = t?.isContentEditable || tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
    if (e.altKey || e.shiftKey) {
      // Ctrl+Tab / Ctrl+Shift+Tab — cycle through tabs even while
      // editing text. Browsers normally eat Ctrl+Tab, but inside a
      // Tauri webview it can fall through if the OS doesn't claim it.
      if (e.key === 'Tab' && !editable) {
        e.preventDefault();
        nextTab(e.shiftKey ? -1 : 1);
      }
      return;
    }
    if (e.key >= '1' && e.key <= '6' && !editable) {
      const target = TAB_HOTKEY[e.key];
      if (target) {
        e.preventDefault();
        switchTo(target);
      }
    }
  }
  let kbUnlisten: (() => void) | null = null;
  // We register the listener on mount; it stays active even when the
  // agent is streaming, so the user can leave the chat tab at any
  // moment. The handler is idempotent — repeated presses are no-ops.
  onMount(() => {
    if (typeof window !== 'undefined') {
      window.addEventListener('keydown', onWindowKey);
      kbUnlisten = () => window.removeEventListener('keydown', onWindowKey);
    }
    // Cross-component tab switch: any child can `dispatchEvent(new
    // CustomEvent('luna:switch-tab', { detail: 'settings' }))` and we
    // honor it. Used by ThreeDChat's "Open Settings" banner.
    function onSwitchTab(e: Event) {
      const ce = e as CustomEvent<string>;
      const tab = ce?.detail;
      if (tab === 'chat' || tab === 'video' || tab === 'memory' || tab === 'settings' || tab === 'three_d' || tab === 'self_evolution') {
        switchTo(tab as TabId);
      }
    }
    window.addEventListener('luna:switch-tab', onSwitchTab as EventListener);
  });
  onDestroy(() => { if (kbUnlisten) kbUnlisten(); });

  // Custom window controls
  async function winMinimize() { try { await appWindow.minimize(); } catch (e) { console.warn(e); } }
  async function winToggleMax() { try { await appWindow.toggleMaximize(); } catch (e) { console.warn(e); } }
  async function winClose() { try { await appWindow.close(); } catch (e) { console.warn(e); } }
</script>

<div class="root">
  <!-- Custom title bar (replaces OS chrome) -->
  <header class="topbar">
    <!-- Brand block: draggable -->
    <div class="brand-block">
      <span class="brand-logo">🌙</span>
      <span class="brand-name">Luna Agent</span>
    </div>

    <!-- Tabs: buttons opt out of drag. `tabindex` follows the roving
         pattern (active = 0, others = -1) so Tab/Shift+Tab move *into*
         and *out of* the tabstrip as a single stop, while arrow keys
         (handled by browser) move between siblings. -->
    <nav class="tabs" aria-label="Разделы" role="tablist">
      <button
        role="tab"
        class:on={activeTab === 'chat'}
        aria-selected={activeTab === 'chat'}
        tabindex={activeTab === 'chat' ? 0 : -1}
        on:click={() => switchTo('chat')}
        title="Чат (Ctrl+1)">💬 Chat</button>
      <button
        role="tab"
        class:on={activeTab === 'video'}
        aria-selected={activeTab === 'video'}
        tabindex={activeTab === 'video' ? 0 : -1}
        on:click={() => switchTo('video')}
        title="Video Mode (Ctrl+2)">🎥 Video Mode</button>
      <button
        role="tab"
        class:on={activeTab === 'memory'}
        aria-selected={activeTab === 'memory'}
        tabindex={activeTab === 'memory' ? 0 : -1}
        on:click={() => switchTo('memory')}
        title="Memory (Ctrl+3)">🧠 Memory</button>
      <button
        role="tab"
        class:on={activeTab === 'azazel'}
        aria-selected={activeTab === 'azazel'}
        tabindex={activeTab === 'azazel' ? 0 : -1}
        on:click={() => switchTo('azazel')}
        title="Azazel — autonomous browser-use agent (Ctrl+4)">
        😈 Azazel
        {#if $runningTaskIds.length > 0}
          <span class="badge">{$runningTaskIds.length}</span>
        {/if}
      </button>
      <button
        role="tab"
        class:on={activeTab === 'three_d'}
        aria-selected={activeTab === 'three_d'}
        tabindex={activeTab === 'three_d' ? 0 : -1}
        on:click={() => switchTo('three_d')}
        title="Luna 3D Thoughts — proprietary editor (Ctrl+5)">💭 3D Thoughts</button>
      <button
        role="tab"
        class:on={activeTab === 'self_evolution'}
        aria-selected={activeTab === 'self_evolution'}
        tabindex={activeTab === 'self_evolution' ? 0 : -1}
        on:click={() => switchTo('self_evolution')}
        title="Self-evolution (Ctrl+5)">🧬 Self</button>
      <button
        role="tab"
        class:on={activeTab === 'daimonion'}
        aria-selected={activeTab === 'daimonion'}
        tabindex={activeTab === 'daimonion' ? 0 : -1}
        on:click={() => switchTo('daimonion')}
        title="Daimonion — voice-first assistant (Ctrl+7)">🔮 Daimonion</button>
      <button
        role="tab"
        class:on={activeTab === 'settings'}
        aria-selected={activeTab === 'settings'}
        tabindex={activeTab === 'settings' ? 0 : -1}
        on:click={() => switchTo('settings')}
        title="Settings (Ctrl+6)">⚙ Settings</button>
    </nav>

    <!-- Right cluster: status pill + window controls -->
    <div class="right">
      {#if tgStatus?.running}
        <button
          class="tg-pill"
          type="button"
          title="Telegram bot running as @{tgStatus.bot_username ?? '?'}"
          on:click={() => switchTo('settings')}>
          🤖 TG
        </button>
      {/if}
      {#if keyMissing && activeTab !== 'settings'}
        <button class="key-pill" type="button" on:click={() => switchTo('settings')}>
          🔑 Нет ключа
        </button>
      {/if}
      <button class="win-btn" type="button" on:click={winMinimize} title="Свернуть" aria-label="Minimize">─</button>
      <button class="win-btn" type="button" on:click={winToggleMax} title="Развернуть" aria-label="Maximize">□</button>
      <button class="win-btn close" type="button" on:click={winClose} title="Закрыть" aria-label="Close">×</button>
    </div>
  </header>

  <!-- Body: optional sidebar + tab content. Only one of Tasks / Plans
       is visible at a time, controlled by `sidebarMode`. -->
  <div class="body">
    {#if (activeTab === 'chat' || activeTab === 'self_evolution')}
      {#if sidebarMode === 'tasks'}
        <TasksSidebar on:switch={handleSwitchSidebar} />
      {:else if sidebarMode === 'plans'}
        <PlansSidebar
          busy={chatBusy}
          on:run={(e) => handleRunPlan(e.detail.plan)}
          on:continue={(e) => handleContinuePlan(e.detail.plan)}
          on:switch={handleSwitchSidebar}
        />
      {:else if sidebarMode === 'design'}
        <DesignStudio on:switch={handleSwitchSidebar} />
      {/if}
    {/if}

    <!-- Content -->
    <section class="tab-panel" role="tabpanel">
    {#if activeTab === 'chat'}
      <Chat providerLabel="Luna Agent" bind:busy={chatBusy} bind:this={chatRef} />
    {:else if activeTab === 'video'}
      <VideoMode />
    {:else if activeTab === 'memory'}
      <Memory />
    {:else if activeTab === 'three_d'}
      <ThreeD />
    {:else if activeTab === 'self_evolution'}
      <SelfEvolution />
    {:else if activeTab === 'daimonion'}
      <Daimonion />
    {:else if activeTab === 'azazel'}
      <AzazelPanel />
    {:else}
      <Settings {theme} {setTheme} />
    {/if}
  </section>
  </div>
</div>

{#if toastMessage}
  <div class="toast" role="status" aria-live="polite">{toastMessage}</div>
{/if}

<style>
  /* ---- App-level frame ---- */
  :global(html), :global(body) {
    margin: 0; padding: 0;
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }
  .root {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    /* Inner border + drop shadow for depth (since OS chrome is gone) */
    box-shadow: inset 0 0 0 1px var(--border);
  }

  /* ---- Top bar (custom chrome) ---- */
  .topbar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    height: 38px;
    background: linear-gradient(180deg, var(--bg-elevated) 0%, var(--bg) 100%);
    border-bottom: 1px solid var(--border);
    user-select: none;
    -webkit-app-region: drag;
    app-region: drag;
  }
  .brand-block {
    display: flex; align-items: center; gap: 8px;
    padding: 0 14px 0 16px;
    height: 100%;
  }
  .brand-logo { font-size: 16px; line-height: 1; }
  .brand-name {
    font-size: 12px; font-weight: 600; letter-spacing: 0.3px;
    background: linear-gradient(135deg, var(--accent-strong) 0%, var(--accent) 100%);
    -webkit-background-clip: text; -webkit-text-fill-color: transparent; background-clip: text;
  }

  .tabs {
    display: flex; align-items: center; gap: 2px;
    height: 100%;
    padding: 0 8px;
    -webkit-app-region: no-drag;
    app-region: no-drag;
  }
  .tabs button {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    padding: 4px 12px;
    height: 26px;
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    font-size: 12px;
    font-weight: 500;
    -webkit-app-region: no-drag;
    app-region: no-drag;
    transition: background 140ms ease, color 140ms ease;
  }
  .tabs button:hover { color: var(--text); background: var(--bg-hover); }
  .tabs button.on {
    color: var(--text);
    background: var(--bg);
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  .right {
    margin-left: auto;
    display: flex; align-items: center; gap: 4px;
    padding-right: 4px;
    height: 100%;
    -webkit-app-region: no-drag;
    app-region: no-drag;
  }
  .key-pill {
    background: var(--warn-soft);
    color: var(--warn);
    border: 1px solid var(--warn);
    padding: 3px 10px;
    border-radius: 999px;
    font-size: 11px;
    cursor: pointer;
    font-family: inherit;
    margin-right: 4px;
    -webkit-app-region: no-drag;
    app-region: no-drag;
    transition: background 150ms ease, color 150ms ease;
  }
  .key-pill:hover { background: var(--warn); color: var(--text-inverse); }

  .tg-pill {
    background: var(--ok-soft); color: var(--ok);
    border: 1px solid var(--ok);
    padding: 3px 10px; border-radius: 999px;
    font-size: 11px; cursor: pointer;
    font-family: inherit;
    margin-right: 4px;
    -webkit-app-region: no-drag; app-region: no-drag;
    transition: background 150ms ease, color 150ms ease;
  }
  .tg-pill:hover { background: var(--ok); color: var(--text-inverse); }

  .win-btn {
    width: 38px; height: 28px;
    border: 0;
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    font-family: inherit;
    font-size: 14px;
    line-height: 1;
    border-radius: 4px;
    display: flex; align-items: center; justify-content: center;
    -webkit-app-region: no-drag;
    app-region: no-drag;
    transition: background 120ms ease, color 120ms ease;
  }
  .win-btn:hover { background: var(--bg-hover); color: var(--text); }
  .win-btn.close:hover { background: var(--danger); color: #ffffff; }

  /* ---- Toast (transient fallback when Notification API is unavailable) ---- */
  .toast {
    position: fixed;
    bottom: 18px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--bg-elevated, rgba(20,22,28,0.95));
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 14px;
    font-size: 13px;
    z-index: 1000;
    box-shadow: 0 8px 24px rgba(0,0,0,0.25);
  }

  /* ---- Content ---- */
  .body {
    display: flex;
    flex: 1;
    min-height: 0; /* allow children to scroll */
  }

  .tab-panel {
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
    display: flex;
  }
  .tab-panel > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .badge {
    background: #b13b3b;
    color: #fff;
    font-size: 10px;
    padding: 0 5px;
    border-radius: 8px;
    margin-left: 4px;
  }
</style>
