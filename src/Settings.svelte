<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { apiKeyStatus, refreshKeyStatus, saveKey, clearKey } from './lib/keyStore';
  import TelegramBot from './TelegramBot.svelte';
  import {
    getShellAllowList,
    addShellCommand,
    removeShellCommand,
    resetShellAllowList,
    type ShellAllowList,
    vaultList,
    vaultSet,
    vaultDelete,
    type VaultEntryPublic,
  } from './lib/tauri';

  // Theme props from App.svelte
  export let theme: 'light' | 'dark' | 'auto' = 'light';
  export let setTheme: (t: 'light' | 'dark' | 'auto') => void = () => {};

  type SectionId = 'api' | 'theme' | 'model' | 'interests' | 'voice' | 'telegram' | 'vault' | 'shell' | 'self_evolution' | 'about';
  let activeSection: SectionId = 'api';
  let searchQuery = '';

  // ---- API key ----
  // Status now comes from the shared keyStore so the "set/missing" badge
  // here, the 🔑 pill in App.svelte, and the chat composer placeholder
  // all reflect the same source.
  $: hasMinimax = $apiKeyStatus === 'present';
  $: checking = $apiKeyStatus === 'unknown';
  let keyValue = '';
  let showKey = false;
  let saving = false;
  let keyError = '';
  let keySavedFlash = '';

  // ---- Model ----
  type ModelOption = { id: string; label: string; model: string };
  const MODELS: ModelOption[] = [
    { id: 'auto', label: 'Auto (default)', model: '' },
    { id: 'minimax-abab65s', label: 'MiniMax-Text-01 (abab6.5s)', model: 'MiniMax-Text-01' },
    { id: 'minimax-M2', label: 'MiniMax M2 (MiniMax-Text-01)', model: 'MiniMax-Text-01' },
    { id: 'minimax-M2.5', label: 'MiniMax M2.5', model: 'MiniMax-M2.5' },
    { id: 'minimax-M2.7', label: 'MiniMax M2.7', model: 'MiniMax-M2.7' },
    { id: 'minimax-M3', label: 'MiniMax M3 (latest)', model: 'MiniMax-M3' },
  ];
  const MODEL_STORAGE_KEY = 'luna.chat.model';
  let selectedModelId = 'auto';

  // ---- Interests ----
  const INTERESTS_STORAGE_KEY = 'luna.news.interests';
  let interestsText = 'AI, Rust, Tauri, Luna Agent';
  let interestsSaved = '';

  // ---- Daimonion VAD (Phase D1) ----
  // Mirrors `services::daimonion::types::VadConfig`. Persisted to
  // localStorage; the D1+ supervisor reads it at startup.
  const VAD_STORAGE_KEY = 'luna.daimonion.vad';
  let vad = {
    energy_threshold: 0.015,
    start_hold_frames: 3,
    end_hold_frames: 80,
    frame_ms: 10,
  };
  function loadVad(): void {
    try {
      const raw = localStorage.getItem(VAD_STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (typeof parsed === 'object' && parsed !== null) {
          vad = { ...vad, ...parsed };
        }
      }
    } catch (e) {
      /* ignore corrupt storage */
    }
  }
  function saveVad(): void {
    try {
      localStorage.setItem(VAD_STORAGE_KEY, JSON.stringify(vad));
    } catch (e) {
      /* ignore */
    }
  }

  const SECTIONS: { id: SectionId; label: string; icon: string; desc: string; keywords: string[] }[] = [
    {
      id: 'api',
      label: 'API Keys',
      icon: '🔑',
      desc: 'MiniMax-ключ для запросов к модели',
      keywords: ['api', 'key', 'ключ', 'minimax', 'token'],
    },
    {
      id: 'theme',
      label: 'Theme',
      icon: '🎨',
      desc: 'Светлая, тёмная или авто (как в системе)',
      keywords: ['theme', 'appearance', 'color', 'dark', 'light', 'тема', 'цвет', 'оформление', 'внешний вид'],
    },
    {
      id: 'model',
      label: 'Model',
      icon: '🧠',
      desc: 'Модель по умолчанию для чата',
      keywords: ['model', 'модель', 'ai', 'llm', 'm2', 'm3', 'text'],
    },
    {
      id: 'interests',
      label: 'News Interests',
      icon: '📰',
      desc: 'Темы для автоподбора в News-режиме',
      keywords: ['news', 'interests', 'новости', 'темы', 'интересы', 'feed', 'rss'],
    },
    {
      id: 'voice',
      label: 'Voice & STT',
      icon: '🎙',
      desc: 'Распознавание речи и хоткеи (в разработке)',
      keywords: ['voice', 'stt', 'whisper', 'голос', 'микрофон', 'speech', 'mic'],
    },
    {
      id: 'telegram',
      label: 'Telegram Bot',
      icon: '🤖',
      desc: 'Управление ботом для удалённого доступа к агенту',
      keywords: ['telegram', 'bot', 'tg', 'телеграм', 'remote', 'удалённо'],
    },
    {
      id: 'vault',
      label: '🔐 Vault',
      icon: '🔐',
      desc: 'Логины и пароли для Azazel (хранятся в OS keyring, LLM их не видит)',
      keywords: ['vault', 'password', 'credentials', 'login', 'azazel', 'browser',
                 'пароль', 'логин', 'ключ', 'credentials', 'secret', 'keyring'],
    },
    {
      id: 'shell',
      label: 'Shell',
      icon: '🖥',
      desc: 'Allow-list системных команд (PowerShell, bash, cmd, cargo, git…)',
      keywords: ['shell', 'bash', 'powershell', 'cmd', 'pwsh', 'terminal', 'шелл', 'командная строка'],
    },
    {
      id: 'self_evolution',
      label: 'Self-evolution',
      icon: '🧬',
      desc: 'Luna читает и улучшает собственные исходники (E0–E4)',
      keywords: ['self', 'evolution', 'evolver', 'snapshot', 'sandbox', 'update', 'rollback', 'apply', 'luna', 'самоулучшение', 'эволюция', 'обновление'],
    },
    {
      id: 'about',
      label: 'About',
      icon: 'ℹ',
      desc: 'О приложении и подсказки',
      keywords: ['about', 'info', 'version', 'о приложении', 'помощь', 'help'],
    },
  ];

  $: filteredSections = SECTIONS.filter((s) => {
    if (!searchQuery.trim()) return true;
    const q = searchQuery.toLowerCase();
    return (
      s.label.toLowerCase().includes(q) ||
      s.desc.toLowerCase().includes(q) ||
      s.keywords.some((k) => k.toLowerCase().includes(q))
    );
  });

  $: currentSection = SECTIONS.find((s) => s.id === activeSection) ?? SECTIONS[0];
  $: currentModel = MODELS.find((m) => m.id === selectedModelId) ?? MODELS[0];
  $: interestsList = interestsText
    .split(/[,\n]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);

  // ---- Shell allow-list state ----
  let shellList: ShellAllowList | null = null;
  let newCmdName = '';
  let newCmdSubs = '';
  let shellBusy = false;
  async function refreshShellList() {
    try {
      shellList = await getShellAllowList();
    } catch (e) {
      console.warn('get_shell_allow_list failed', e);
    }
  }
  async function addCmd() {
    const name = newCmdName.trim();
    if (!name) return;
    const subs = newCmdSubs
      .split(/[,\n]/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    shellBusy = true;
    try {
      shellList = await addShellCommand(name, subs);
      newCmdName = '';
      newCmdSubs = '';
    } catch (e) {
      console.warn('add_shell_command failed', e);
    } finally {
      shellBusy = false;
    }
  }
  async function removeCmd(name: string) {
    if (!confirm(`Удалить "${name}" из allow-list?`)) return;
    shellBusy = true;
    try {
      shellList = await removeShellCommand(name);
    } catch (e) {
      console.warn('remove_shell_command failed', e);
    } finally {
      shellBusy = false;
    }
  }
  async function resetShellList() {
    if (!confirm('Сбросить allow-list к встроенным дефолтам?')) return;
    shellBusy = true;
    try {
      shellList = await resetShellAllowList();
    } catch (e) {
      console.warn('reset_shell_allow_list failed', e);
    } finally {
      shellBusy = false;
    }
  }

  // (removed) `refreshKey()` — the shared `refreshKeyStatus()` from
  // keyStore does the IPC read and updates `apiKeyStatus` reactively.
  // `checking` and `hasMinimax` are now `$:`-derived from that store.

  onMount(() => {
    try {
      const saved = localStorage.getItem(MODEL_STORAGE_KEY);
      if (saved && MODELS.some((m) => m.id === saved)) selectedModelId = saved;
    } catch { /* ignore */ }
    try {
      const savedInterests = localStorage.getItem(INTERESTS_STORAGE_KEY);
      if (savedInterests !== null) interestsText = savedInterests;
    } catch { /* ignore */ }
    loadVad();
    refreshKeyStatus().catch(() => { /* non-fatal */ });
    refreshShellList().catch(() => { /* non-fatal */ });
  });

  // Auto-save VAD changes.
  $: if (typeof localStorage !== 'undefined') saveVad();

  // Refresh the shell list when the user actually opens the section
  // — picks up edits made in another window or by the bot.
  $: if (activeSection === 'shell' && !shellList) refreshShellList();

  // ---- Vault (Azazel credentials) ----
  let vaultEntries: VaultEntryPublic[] = [];
  let vaultLoaded = false;
  let vaultLoading = false;
  let vaultDraft: { domain: string; login: string; password: string } = {
    domain: '',
    login: '',
    password: '',
  };
  let vaultRevealed: Record<string, boolean> = {};
  let vaultError = '';
  let vaultSavedFlash = '';
  let vaultSaveTimer: ReturnType<typeof setTimeout> | null = null;

  async function refreshVault(force = false) {
    if (vaultLoading) return;
    if (vaultLoaded && !force) return;
    vaultLoading = true;
    try {
      vaultEntries = await vaultList();
      vaultLoaded = true;
    } catch (e) {
      vaultError = `vault: ${e}`;
    } finally {
      vaultLoading = false;
    }
  }
  $: if (activeSection === 'vault' && !vaultLoaded) refreshVault();

  async function saveVaultDraft() {
    vaultError = '';
    const domain = vaultDraft.domain.trim();
    const login = vaultDraft.login.trim();
    const password = vaultDraft.password;
    if (!domain || !login || !password) {
      vaultError = 'Заполни domain, login и password.';
      return;
    }
    try {
      await vaultSet(domain, login, password);
      vaultDraft = { domain: '', login: '', password: '' };
      vaultLoaded = false;
      await refreshVault(true);
      vaultSavedFlash = '✓ сохранено';
      if (vaultSaveTimer) clearTimeout(vaultSaveTimer);
      vaultSaveTimer = setTimeout(() => (vaultSavedFlash = ''), 2000);
    } catch (e) {
      vaultError = `save: ${e}`;
    }
  }

  async function deleteVault(domain: string) {
    if (!confirm(`Удалить credential для "${domain}"?`)) return;
    try {
      await vaultDelete(domain);
      vaultLoaded = false;
      await refreshVault(true);
    } catch (e) {
      vaultError = `delete: ${e}`;
    }
  }

  async function saveKeyAction() {
    const trimmed = keyValue.trim();
    if (!trimmed) { keyError = 'Введите ключ'; return; }
    saving = true;
    keyError = '';
    try {
      await saveKey(trimmed);
      keyValue = '';
      keySavedFlash = 'API-ключ сохранён';
      setTimeout(() => (keySavedFlash = ''), 2000);
    } catch (e) {
      keyError = String(e);
    } finally {
      saving = false;
    }
  }

  async function clearKeyAction() {
    if (!confirm('Удалить сохранённый MiniMax-ключ из Credential Manager?')) return;
    try {
      await clearKey();
    } catch { /* ignore */ }
  }

  function onModelChange() {
    try { localStorage.setItem(MODEL_STORAGE_KEY, selectedModelId); } catch { /* ignore */ }
  }

  function saveInterests() {
    try {
      localStorage.setItem(INTERESTS_STORAGE_KEY, interestsText);
      interestsSaved = 'Сохранено';
      setTimeout(() => (interestsSaved = ''), 2000);
    } catch {
      interestsSaved = 'Ошибка';
    }
  }

  function selectSection(id: SectionId) {
    activeSection = id;
    tick().then(() => {
      const el = document.querySelector('.content-scroll');
      if (el) el.scrollTop = 0;
    });
  }
</script>

<div class="settings">
  <!-- Left sidebar (category nav) -->
  <aside class="sidebar">
    <div class="search-wrap">
      <span class="search-icon">🔍</span>
      <input
        class="search"
        type="text"
        placeholder="Search settings…"
        bind:value={searchQuery}
      />
    </div>

    <nav class="nav">
      {#each filteredSections as s (s.id)}
        <button
          class="nav-item"
          class:on={activeSection === s.id}
          on:click={() => selectSection(s.id)}
        >
          <span class="nav-icon">{s.icon}</span>
          <span class="nav-text">
            <span class="nav-label">{s.label}</span>
            <span class="nav-desc">{s.desc}</span>
          </span>
        </button>
      {/each}
      {#if filteredSections.length === 0}
        <div class="nav-empty">Ничего не найдено</div>
      {/if}
    </nav>

    <div class="sidebar-footer">
      <span class="ver">v0.1.0</span>
    </div>
  </aside>

  <!-- Right content area -->
  <main class="content">
    <div class="content-scroll" data-section={activeSection}>
      <header class="content-head">
        <h2>
          <span class="head-icon">{currentSection.icon}</span>
          {currentSection.label}
        </h2>
        <p class="head-desc">{currentSection.desc}</p>
      </header>

      {#if activeSection === 'api'}
        <section class="block">
          <div class="row-head">
            <h3>🔑 MiniMax API Key</h3>
            <span class="badge" class:ok={hasMinimax} class:miss={!hasMinimax && !checking}>
              {checking ? '…' : hasMinimax ? 'set' : 'missing'}
            </span>
          </div>
          <p class="hint-text">
            Ключ хранится в Windows Credential Manager через Rust keyring. В чат-трафик не уходит.
            Получить: <a href="https://platform.minimaxi.com/" target="_blank" rel="noopener">platform.minimaxi.com → API Keys</a>.
          </p>

          <div class="row">
            {#if showKey}
              <input
                type="text"
                bind:value={keyValue}
                placeholder="sk-cp-… или eyJ…"
                autocomplete="off"
                spellcheck="false"
                disabled={saving}
                on:keydown={(e) => { if (e.key === 'Enter') saveKeyAction(); }}
              />
            {:else}
              <input
                type="password"
                bind:value={keyValue}
                placeholder="sk-cp-… или eyJ…"
                autocomplete="off"
                spellcheck="false"
                disabled={saving}
                on:keydown={(e) => { if (e.key === 'Enter') saveKeyAction(); }}
              />
            {/if}
            <button
              class="toggle"
              type="button"
              on:click={() => (showKey = !showKey)}
              aria-label={showKey ? 'Скрыть ключ' : 'Показать ключ'}
              title={showKey ? 'Скрыть' : 'Показать'}
              tabindex="-1"
            >{showKey ? '🙈' : '👁'}</button>
          </div>

          {#if keyError}<p class="err">⚠ {keyError}</p>{/if}
          {#if keySavedFlash}<p class="ok-msg">✓ {keySavedFlash}</p>{/if}

          <div class="actions">
            <button class="ghost" type="button" on:click={clearKeyAction} disabled={!hasMinimax || saving}>Очистить</button>
            <button class="primary" type="button" on:click={saveKeyAction} disabled={saving || !keyValue.trim()}>
              {saving ? 'Сохранение…' : 'Сохранить'}
            </button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'theme'}
        <section class="block">
          <div class="row-head">
            <h3>🎨 Тема оформления</h3>
            <span class="badge neutral">{theme === 'auto' ? 'auto' : theme}</span>
          </div>
          <p class="hint-text">
            Светлая тема — по умолчанию. «Авто» следует за системной темой Windows.
            Выбор сохраняется в localStorage и применяется до первой отрисовки.
          </p>
          <div class="theme-grid">
            <button
              type="button"
              class="theme-card"
              class:on={theme === 'light'}
              on:click={() => setTheme('light')}
              aria-pressed={theme === 'light'}
            >
              <div class="theme-preview theme-preview-light" aria-hidden="true">
                <div class="tp-bar"></div>
                <div class="tp-body">
                  <div class="tp-side"></div>
                  <div class="tp-content">
                    <div class="tp-line w70"></div>
                    <div class="tp-line w90"></div>
                    <div class="tp-line w50"></div>
                  </div>
                </div>
              </div>
              <div class="theme-meta">
                <span class="theme-name">вЂ Светлая</span>
                <span class="theme-sub">мягкий тёплый фон</span>
              </div>
            </button>
            <button
              type="button"
              class="theme-card"
              class:on={theme === 'dark'}
              on:click={() => setTheme('dark')}
              aria-pressed={theme === 'dark'}
            >
              <div class="theme-preview theme-preview-dark" aria-hidden="true">
                <div class="tp-bar"></div>
                <div class="tp-body">
                  <div class="tp-side"></div>
                  <div class="tp-content">
                    <div class="tp-line w70"></div>
                    <div class="tp-line w90"></div>
                    <div class="tp-line w50"></div>
                  </div>
                </div>
              </div>
              <div class="theme-meta">
                <span class="theme-name">🌙 Тёмная</span>
                <span class="theme-sub">привычный ночной режим</span>
              </div>
            </button>
            <button
              type="button"
              class="theme-card"
              class:on={theme === 'auto'}
              on:click={() => setTheme('auto')}
              aria-pressed={theme === 'auto'}
            >
              <div class="theme-preview theme-preview-auto" aria-hidden="true">
                <div class="tp-bar"></div>
                <div class="tp-body">
                  <div class="tp-side"></div>
                  <div class="tp-content">
                    <div class="tp-line w70"></div>
                    <div class="tp-line w90"></div>
                    <div class="tp-line w50"></div>
                  </div>
                </div>
              </div>
              <div class="theme-meta">
                <span class="theme-name">🖥 Авто</span>
                <span class="theme-sub">как в Windows</span>
              </div>
            </button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'model'}
        <section class="block">
          <div class="row-head">
            <h3>🧠 Модель по умолчанию</h3>
            <span class="badge neutral">{currentModel.label}</span>
          </div>
          <p class="hint-text">
            Применяется к чату. В окне чата модель можно переключить через dropdown в шапке.
          </p>
          <div class="model-list">
            {#each MODELS as m (m.id)}
              <label class="model-row" class:on={selectedModelId === m.id}>
                <input
                  type="radio"
                  name="model"
                  value={m.id}
                  bind:group={selectedModelId}
                  on:change={onModelChange}
                />
                <span class="model-check"></span>
                <span class="model-info">
                  <span class="model-label">{m.label}</span>
                  <span class="model-meta">API: {m.model || 'по умолчанию провайдера'}</span>
                </span>
              </label>
            {/each}
          </div>
        </section>
      {/if}

      {#if activeSection === 'interests'}
        <section class="block">
          <div class="row-head">
            <h3>📰 РРЅС‚РµСЂРµСЃС‹ для News-агента</h3>
            <span class="badge neutral">{interestsList.length} тем</span>
          </div>
          <p class="hint-text">
            News-режим использует эти темы для автоподбора материалов через DuckDuckGo.
            Через запятую или с новой строки. Примеры: <code>AI, Rust, Tauri, anime, космос</code>.
          </p>
          <textarea
            class="interests-area"
            bind:value={interestsText}
            placeholder="AI, Rust, Tauri, Luna Agent, ..."
            rows="5"
            spellcheck="false"
          ></textarea>
          <div class="chips">
            {#each interestsList as tag (tag)}
              <span class="chip">{tag}</span>
            {/each}
            {#if interestsList.length === 0}
              <span class="chip empty">⚠ пусто</span>
            {/if}
          </div>
          {#if interestsSaved}<p class="ok-msg">✓ {interestsSaved}</p>{/if}
          <div class="actions">
            <button class="primary" type="button" on:click={saveInterests}>Сохранить интересы</button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'voice'}
        <section class="block">
          <div class="row-head">
            <h3>🎙 Голосовой ввод (Whisper)</h3>
            <span class="badge neutral">локальный fallback</span>
          </div>
          <p class="hint-text">
            Whisper-модели управляются прямо в чате — нажми 🎙 в композере, и если модели нет,
            появится предложение скачать. Глобальный хоткей: <kbd>Ctrl</kbd>+<kbd>Space</kbd>.
          </p>
          <ul class="info-list">
            <li><b>Старт/стоп записи</b>: 🎙 в композере чата или <kbd>Ctrl</kbd>+<kbd>Space</kbd> глобально</li>
            <li><b>Модели</b>: <code>base</code> ≈ 140 МБ, <code>small</code> ≈ 460 МБ — рекомендую <code>base</code></li>
            <li><b>Где хранятся</b>: <code>%APPDATA%\com.luna.agent\whisper-models\</code></li>
            <li><b>Текущая ошибка</b> (если была): <code>stt:allow-list-models</code> — уже добавлено в capabilities</li>
          </ul>
        </section>

        <section class="block">
          <div class="row-head">
            <h3>🔮 Daimonion — голосовой ассистент</h3>
            <span class="badge ok">D0+ готов</span>
          </div>
          <p class="hint-text">
            Daimonion (Δαιμόνιον) — голос-первый мультимодальный ассистент
            из тёмной линейки Luna (рядом с Lucifer / Azazel / Raziel /
            Mephistopheles). STT и TTS идут через <b>MiniMax</b> (нужна
            активная подписка Ultra и MiniMax-ключ в API Keys).
            Push-to-talk в панели Daimonion — <kbd>Space</kbd>.
          </p>

          <div class="vad-grid">
            <label>
              <span>Energy threshold</span>
              <input type="number" min="0.001" max="0.5" step="0.001"
                bind:value={vad.energy_threshold} />
              <small>0.001–0.5. Ниже = чувствительнее.</small>
            </label>
            <label>
              <span>Start hold (frames)</span>
              <input type="number" min="1" max="50" step="1"
                bind:value={vad.start_hold_frames} />
              <small>Сколько фреймов подряд выше порога до старта речи.</small>
            </label>
            <label>
              <span>End hold (frames)</span>
              <input type="number" min="10" max="500" step="1"
                bind:value={vad.end_hold_frames} />
              <small>Тишина (мс = frames × 10) до конца фразы.</small>
            </label>
            <label>
              <span>Frame (ms)</span>
              <input type="number" min="5" max="50" step="1"
                bind:value={vad.frame_ms} />
              <small>Размер фрейма для VAD-математики.</small>
            </label>
          </div>

          <ul class="info-list">
            <li><b>Pipeline</b>: cpal mic → VAD → MiniMax ASR → MiniMax-M3 → MiniMax T2A (speech-02) → cpal</li>
            <li><b>Latency budget</b>: ≤ 1.5 с p50, ≤ 2.5 с p95 end-to-end</li>
            <li><b>Vision</b>: модель сама решает, когда смотреть экран (D2+); не always-on</li>
            <li><b>Read-only</b>: Daimonion смотрит и говорит, не правит файлы</li>
          </ul>
        </section>
      {/if}

      {#if activeSection === 'telegram'}
        <section class="block">
          <TelegramBot />
        </section>
      {/if}

      {#if activeSection === 'vault'}
        <section class="block">
          <div class="row-head">
            <h3>🔐 Azazel Vault</h3>
            <span class="badge neutral">
              {vaultEntries.length} {vaultEntries.length === 1 ? 'запись' : 'записей'}
            </span>
          </div>
          <p class="hint-text">
            Логины и пароли для сайтов, на которые Azazel должен уметь логиниться
            (VK, GitHub, Gmail, etc). Хранятся в OS keyring — <strong>модель их никогда не
            видит</strong>. Когда модель вызывает <code>azazel_run</code> с
            <code>vault_domain="vk.com"</code>, пароль подставляется в браузерную сессию
            сервером Azazel.
          </p>

          {#if vaultError}
            <div class="banner banner-error">⚠ {vaultError}</div>
          {/if}
          {#if vaultSavedFlash}
            <div class="banner banner-ok">{vaultSavedFlash}</div>
          {/if}

          <div class="vault-add">
            <h4>+ Добавить credential</h4>
            <div class="vault-row">
              <label>
                Domain
                <input
                  type="text"
                  placeholder="vk.com"
                  bind:value={vaultDraft.domain}
                />
              </label>
              <label>
                Login
                <input
                  type="text"
                  placeholder="username / email / phone"
                  bind:value={vaultDraft.login}
                />
              </label>
              <label>
                Password
                <input
                  type="password"
                  placeholder="••••••••"
                  bind:value={vaultDraft.password}
                />
              </label>
              <button class="btn-primary" type="button" on:click={saveVaultDraft}>
                Сохранить
              </button>
            </div>
            <p class="hint-text subtle">
              Домен нормализуется: <code>https://www.VK.com/login</code> → <code>vk.com</code>.
              Чувствительные данные шифруются OS keyring.
            </p>
          </div>

          <h4>Сохранённые credentials</h4>
          {#if vaultLoading}
            <p class="hint-text">Загрузка…</p>
          {:else if vaultEntries.length === 0}
            <p class="hint-text subtle">
              Пока пусто. Добавь первую запись выше — Azazel сможет логиниться
              автоматически.
            </p>
          {:else}
            <table class="vault-table">
              <thead>
                <tr>
                  <th>Domain</th>
                  <th>Login</th>
                  <th>Password</th>
                  <th>Updated</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {#each vaultEntries as e (e.domain)}
                  <tr>
                    <td><code>{e.domain}</code></td>
                    <td>{e.login}</td>
                    <td>
                      {#if e.has_password}
                        <span class="muted">•••••••• (в keyring)</span>
                      {:else}
                        <span class="muted">—</span>
                      {/if}
                    </td>
                    <td class="muted">{e.updated_at}</td>
                    <td>
                      <button
                        class="btn-icon"
                        type="button"
                        title="Удалить"
                        on:click={() => deleteVault(e.domain)}
                      >🗑</button>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </section>
      {/if}

      {#if activeSection === 'shell'}
        <section class="block">
          <div class="row-head">
            <h3>🖥 Shell-команды</h3>
            <span class="badge neutral">{shellList?.commands.length ?? 0} команд</span>
          </div>
          <p class="hint-text">
            Только перечисленные команды могут вызываться из агента / чата. Поведение: argv-only, без <code>sh -c</code> / <code>cmd /c "&lt;string&gt;"</code> — никакой shell-инъекции.
            Файл allow-list: <code>%LOCALAPPDATA%\luna-agent\shell-allowlist.json</code>.
          </p>

          <div class="shell-list">
            {#each shellList?.commands ?? [] as e (e.name)}
              <div class="shell-row" class:empty={(e.subcommand_patterns ?? []).length === 0}>
                <div class="shell-name">{e.name}</div>
                <div class="shell-subs">
                  {#if (e.subcommand_patterns ?? []).length === 0}
                    <span class="shell-empty-tag">любые subcommand</span>
                  {:else}
                    {#each e.subcommand_patterns as p (p)}
                      <span class="shell-sub">{p}</span>
                    {/each}
                  {/if}
                </div>
                <button
                  type="button"
                  class="shell-del"
                  on:click={() => removeCmd(e.name)}
                  title="Удалить из allow-list"
                >✕</button>
              </div>
            {/each}
          </div>

          <div class="shell-add">
            <input
              type="text"
              class="shell-input"
              placeholder="имя (powershell, bash, cmd, …)"
              bind:value={newCmdName}
              on:keydown={(e) => { if (e.key === 'Enter') addCmd(); }}
            />
            <input
              type="text"
              class="shell-input shell-subs-input"
              placeholder="subcommand (через запятую, опционально)"
              bind:value={newCmdSubs}
              on:keydown={(e) => { if (e.key === 'Enter') addCmd(); }}
            />
            <button type="button" class="primary" on:click={addCmd} disabled={!newCmdName.trim()}>+ Добавить</button>
            <button type="button" class="secondary" on:click={resetShellList}>↺ Дефолты</button>
          </div>
          <p class="shell-hint">
            Примеры: <code>bash</code>, <code>cmd</code>, <code>powershell</code>, <code>cargo</code>, <code>pytest</code>, <code>node</code>.
            Если в поле <em>subcommand</em> указать <code>test, build, run</code> — допустимы только эти subcommand-ы (для команд с явной подкомандной структурой). Пусто = любые.
          </p>
        </section>
      {/if}

      {#if activeSection === 'self_evolution'}
        <section class="block">
          <div class="row-head">
            <h3>🧬 Self-evolution (E0–E4)</h3>
            <span class="badge neutral">экспериментально</span>
          </div>
          <p class="hint-text">
            Luna может читать собственные исходники, находить в них проблемы,
            составлять план правок, проверять его в песочнице (sandbox) и
            атомарно обновлять свой бинарь. <b>Только ручной запуск</b>:
            вы нажимаете кнопку, видите отчёт и подтверждаете apply.
          </p>
          <ul class="info-list">
            <li><b>Все этапы включены в текущей сборке</b> (E0 inspect · E1 snapshots · E2 diagnose · E3 sandbox · E4 apply/rollback)</li>
            <li><b>Где лежат данные</b>: <code>%LOCALAPPDATA%\com.luna.agent\evolver\</code>
              <ul>
                <li><code>active.json</code> — текущая версия</li>
                <li><code>snapshots/&lt;id&gt;/src/</code> — полные копии исходников</li>
                <li><code>feedback/&lt;id&gt;.json</code> — ваш фидбек (используется в следующей diagnose)</li>
                <li><code>plans/&lt;id&gt;.json</code> — последние планы</li>
              </ul>
            </li>
            <li><b>Окружение</b>: <code>LUNA_SOURCE_ROOT</code> env var или автодетект от бинаря</li>
            <li><b>Защищённые файлы</b> (worker откажется их трогать): <code>Cargo.toml</code>, <code>tauri.conf.json</code>, <code>package.json</code>, <code>capabilities/default.json</code>, <code>vendor/</code>, <code>LICENSE*</code></li>
            <li><b>Требования</b>: Rust toolchain (cargo), <code>tauri-build.cmd</code> в PATH (Windows)</li>
            <li><b>GC</b>: 5 последних non-important + все important + active</li>
            <li><b>Открыть вкладку</b>: <code>🧬 Self</code> в шапке главного окна</li>
          </ul>
          <p class="hint-text">
            <b>Внимание:</b> apply/rollback пересобирает бинарь и атомарно
            подменяет его. <b>Всегда делается pre-update snapshot</b>, к
            которому можно откатиться. В Windows после успешного apply
            нужно вручную перезапустить Luna, чтобы загрузить новый .exe.
          </p>
        </section>
      {/if}

      {#if activeSection === 'about'}
        <section class="block">
          <h3>ℹ About Luna Agent</h3>
          <p class="hint-text">Tauri 2 + Svelte десктоп-агент. Стриминг, DuckDuckGo, локальный keyring, голос через Whisper.</p>
          <ul class="info-list">
            <li><b>Голосовой ввод</b>: 🎙 в композере или <kbd>Ctrl</kbd>+<kbd>Space</kbd></li>
            <li><b>News-агент</b>: 📰 в шапке чата → автоподбор по интересам</li>
            <li><b>Кликабельные ссылки</b> в ответах → системный браузер</li>
            <li><b>Стриминг</b> пока не подключён — MiniMax возвращает полный ответ</li>
          </ul>
          <div class="meta-grid">
            <div class="meta-row"><span class="meta-k">Frontend</span><span class="meta-v">Svelte 4 + Vite</span></div>
            <div class="meta-row"><span class="meta-k">Backend</span><span class="meta-v">Tauri 2 + Rust</span></div>
            <div class="meta-row"><span class="meta-k">AI Provider</span><span class="meta-v">MiniMax (Coding Plan)</span></div>
            <div class="meta-row"><span class="meta-k">Window</span><span class="meta-v">Custom chrome, drag-region</span></div>
          </div>
        </section>
      {/if}
    </div>
  </main>
</div>

<style>
  .settings {
    height: 100%;
    display: grid;
    grid-template-columns: 240px 1fr;
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }

  /* ---- Sidebar ---- */
  .sidebar {
    display: flex; flex-direction: column;
    background: var(--bg-elevated);
    border-right: 1px solid var(--border);
    min-height: 0;
  }
  .search-wrap {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.04);
    min-width: 0;
  }
  .search-icon {
    flex: 0 0 auto;
    font-size: 11px; color: var(--text-faint); pointer-events: none;
    line-height: 1;
  }
  .search {
    flex: 1 1 auto;
    min-width: 0;
    box-sizing: border-box;
    width: 100%;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    font-family: inherit; font-size: 12px;
    padding: 6px 8px;
    outline: none;
    transition: border-color 150ms ease, box-shadow 150ms ease;
  }
  .search:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .search::placeholder { color: var(--text-faint); }

  .nav {
    flex: 1; overflow-y: auto;
    padding: 8px 6px;
    display: flex; flex-direction: column; gap: 2px;
  }
  .nav::-webkit-scrollbar { width: 6px; }
  .nav::-webkit-scrollbar-thumb { background: var(--border); border-radius: 3px; }

  .nav-item {
    display: flex; align-items: flex-start; gap: 10px;
    width: 100%;
    padding: 8px 10px;
    background: transparent;
    border: 0;
    border-radius: 6px;
    color: var(--text-muted);
    font-family: inherit; font-size: 12px;
    text-align: left;
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease;
  }
  .nav-item:hover { background: rgba(255, 255, 255, 0.04); color: var(--text); }
  .nav-item.on {
    background: var(--accent-soft);
    color: var(--text);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .nav-icon { font-size: 14px; line-height: 1; flex: 0 0 auto; padding-top: 1px; }
  .nav-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .nav-label { font-weight: 600; }
  .nav-desc {
    font-size: 10px; color: var(--text-faint); line-height: 1.35;
    overflow: hidden; text-overflow: ellipsis; display: -webkit-box;
    -webkit-line-clamp: 2; -webkit-box-orient: vertical;
  }
  .nav-item.on .nav-desc { color: var(--text-muted); }
  .nav-empty {
    padding: 20px 12px; text-align: center;
    color: var(--text-faint); font-size: 11px;
  }

  .sidebar-footer {
    padding: 8px 14px;
    border-top: 1px solid rgba(255, 255, 255, 0.04);
    font-size: 10px; color: var(--text-faint);
  }
  .ver { font-family: ui-monospace, monospace; }

  /* ---- Content ---- */
  .content {
    min-width: 0;
    display: flex; flex-direction: column;
  }
  .content-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 28px 36px 60px;
    scrollbar-width: thin; scrollbar-color: var(--border) transparent;
  }
  .content-scroll::-webkit-scrollbar { width: 8px; }
  .content-scroll::-webkit-scrollbar-thumb { background: var(--border); border-radius: 4px; }

  /* ---- Shell allow-list ---- */
  .shell-list { display: flex; flex-direction: column; gap: 4px; margin: 12px 0; }
  .shell-row {
    display: grid;
    grid-template-columns: 130px 1fr 28px;
    align-items: center; gap: 10px;
    padding: 6px 10px;
    background: var(--bg-elevated, rgba(255,255,255,0.03));
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .shell-name { font-family: ui-monospace, 'Cascadia Code', Menlo, monospace; font-size: 12px; font-weight: 600; color: var(--accent); }
  .shell-subs { display: flex; flex-wrap: wrap; gap: 4px; }
  .shell-sub {
    display: inline-block; padding: 1px 7px; border-radius: 999px;
    background: var(--accent-soft, rgba(176, 120, 120, 0.12));
    color: var(--text); font-size: 10.5px;
  }
  .shell-empty-tag {
    display: inline-block; padding: 1px 7px; border-radius: 999px;
    background: rgba(255,255,255,0.04);
    color: var(--text-muted); font-size: 10.5px; font-style: italic;
  }
  .shell-del {
    width: 26px; height: 26px; border-radius: 4px; border: 1px solid var(--border);
    background: transparent; color: var(--text-muted); cursor: pointer; font-size: 14px;
    display: flex; align-items: center; justify-content: center;
  }
  .shell-del:hover { background: rgba(216, 122, 122, 0.14); color: #ffb0b0; border-color: rgba(216, 122, 122, 0.40); }
  .shell-add { display: flex; gap: 6px; margin-top: 10px; flex-wrap: wrap; align-items: center; }
  .shell-input {
    flex: 1; min-width: 160px;
    background: var(--bg-input, #ffffff);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 12px; padding: 6px 10px;
  }
  .shell-input:focus { outline: none; border-color: var(--accent); }
  .shell-subs-input { flex: 2; }
  .shell-hint { font-size: 11px; color: var(--text-muted); margin-top: 8px; line-height: 1.5; }
  .shell-hint code { background: var(--bg-elevated); padding: 0 4px; border-radius: 3px; font-size: 10.5px; }

  .content-head { margin-bottom: 20px; padding-bottom: 14px; border-bottom: 1px solid var(--border); }
  .content-head h2 {
    margin: 0 0 4px; font-size: 18px; font-weight: 600;
    display: flex; align-items: center; gap: 8px;
  }
  .head-icon { font-size: 18px; }
  .head-desc { margin: 0; color: var(--text-muted); font-size: 12px; }

  .block {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 18px 20px;
    margin-bottom: 14px;
  }
  .block h3 { margin: 0; font-size: 14px; font-weight: 600; }
  .row-head {
    display: flex; align-items: center; justify-content: space-between;
    margin-bottom: 10px;
  }
  .hint-text { color: var(--text-muted); font-size: 12px; line-height: 1.5; margin: 0 0 12px; }
  .hint-text code {
    background: var(--bg-input); border: 1px solid var(--border);
    border-radius: 3px; padding: 1px 5px; font-size: 11px; color: var(--text);
  }
  .hint-text a { color: var(--accent); text-decoration: underline; text-decoration-style: dotted; }

  .badge {
    padding: 2px 8px; border-radius: 999px;
    font-size: 10px; text-transform: uppercase; letter-spacing: 0.5px;
    border: 1px solid var(--border); color: var(--text-muted);
  }
  .badge.ok { color: var(--success); border-color: var(--success-soft); background: var(--success-soft); }
  .badge.miss { color: var(--warn); border-color: var(--warn-soft); background: var(--warn-soft); }
  .badge.neutral { color: var(--text); border-color: var(--border-strong); background: var(--bg-input); }

  .row { display: flex; gap: 6px; margin-bottom: 4px; }
  .row input {
    flex: 1; min-width: 0;
    font-family: ui-monospace, 'Cascadia Code', Menlo, monospace;
    font-size: 13px;
  }
  .toggle {
    background: transparent; color: var(--text); border: 1px solid var(--border-strong);
    padding: 0 10px; font-size: 14px;
    border-radius: 6px; cursor: pointer;
  }
  .toggle:hover { background: var(--bg-hover); }

  .model-list { display: flex; flex-direction: column; gap: 4px; }
  .model-row {
    display: flex; align-items: center; gap: 10px;
    padding: 10px 12px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: 8px;
    cursor: pointer;
    transition: background 150ms ease, border-color 150ms ease;
  }
  .model-row:hover { background: var(--bg-hover); }
  .model-row.on {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .model-row input[type="radio"] { display: none; }
  .model-check {
    width: 16px; height: 16px;
    border: 1.5px solid var(--border-strong);
    border-radius: 50%;
    flex: 0 0 auto;
    position: relative;
  }
  .model-row.on .model-check {
    border-color: var(--accent);
  }
  .model-row.on .model-check::after {
    content: "";
    position: absolute;
    inset: 3px;
    border-radius: 50%;
    background: var(--accent);
  }
  .model-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .model-label { font-size: 13px; font-weight: 500; }
  .model-meta { font-size: 10px; color: var(--text-faint); font-family: ui-monospace, monospace; }

  /* ---- theme picker ---- */
  .theme-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 10px;
  }
  .theme-card {
    display: flex; flex-direction: column; gap: 8px;
    padding: 10px;
    background: var(--bg-input);
    border: 1.5px solid var(--border);
    border-radius: 10px;
    cursor: pointer;
    font-family: inherit;
    color: var(--text);
    text-align: left;
    transition: border-color 150ms ease, transform 100ms ease, box-shadow 150ms ease;
  }
  .theme-card:hover { border-color: var(--border-strong); transform: translateY(-1px); }
  .theme-card.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .theme-preview {
    width: 100%;
    aspect-ratio: 16 / 10;
    border-radius: 6px;
    overflow: hidden;
    border: 1px solid var(--border-subtle);
    display: flex; flex-direction: column;
  }
  .theme-preview-light { background: #f7f5f1; }
  .theme-preview-light .tp-bar { background: #fdfcf9; border-bottom: 1px solid #e2ddd2; }
  .theme-preview-light .tp-side { background: #f0ede5; border-right: 1px solid #e2ddd2; }
  .theme-preview-light .tp-line { background: #d6d0c2; }
  .theme-preview-dark { background: var(--bg); }
  .theme-preview-dark .tp-bar { background: var(--bg-elevated); border-bottom: 1px solid var(--border); }
  .theme-preview-dark .tp-side { background: var(--bg-elevated); border-right: 1px solid var(--border); }
  .theme-preview-dark .tp-line { background: var(--border); }
  .theme-preview-auto {
    background: linear-gradient(90deg, #f7f5f1 0%, #f7f5f1 50%, var(--bg) 50%, var(--bg) 100%);
  }
  .theme-preview-auto .tp-bar { background: rgba(255,255,255,0.05); border-bottom: 1px solid rgba(127,127,127,0.3); }
  .theme-preview-auto .tp-side { background: rgba(127,127,127,0.18); border-right: 1px solid rgba(127,127,127,0.3); }
  .theme-preview-auto .tp-line { background: rgba(127,127,127,0.45); }
  .tp-bar { height: 14%; }
  .tp-body { display: flex; flex: 1; min-height: 0; }
  .tp-side { width: 28%; }
  .tp-content { flex: 1; padding: 18% 10% 0 12%; display: flex; flex-direction: column; gap: 5px; }
  .tp-line { height: 4px; border-radius: 2px; }
  .tp-line.w90 { width: 90%; }
  .tp-line.w70 { width: 70%; }
  .tp-line.w50 { width: 50%; }
  .theme-meta { display: flex; flex-direction: column; gap: 2px; }
  .theme-name { font-size: 13px; font-weight: 600; color: var(--text); }
  .theme-sub { font-size: 11px; color: var(--text-muted); }

  /* No <select> lives in this component today (the model picker is in
     Chat.svelte, which styles its own select). Wrap in :global so the
     rule still applies if a future selector is added here without
     tripping vite-plugin-svelte's unused-CSS check. */
  :global(select) {
    width: 100%; padding: 8px 10px; border-radius: 8px;
    background: var(--bg-input); color: var(--text); border: 1px solid var(--border-strong);
    font-family: inherit; font-size: 13px;
  }
  :global(select option) { background: var(--bg-input); color: var(--text); }

  .interests-area {
    width: 100%; min-height: 90px; max-height: 200px;
    padding: 10px 12px; border-radius: 8px;
    background: var(--bg-input); color: var(--text); border: 1px solid var(--border-strong);
    font-family: inherit; font-size: 13px; line-height: 1.5;
    resize: vertical; outline: none;
    transition: border-color 150ms ease, box-shadow 150ms ease;
  }
  .interests-area:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }
  .chip {
    padding: 2px 9px; border-radius: 999px;
    background: var(--accent-soft);
    border: 1px solid rgba(201, 160, 160, 0.25);
    color: var(--accent); font-size: 11px;
  }
  .chip.empty { background: rgba(245, 181, 107, 0.08); border-color: rgba(245, 181, 107, 0.3); color: var(--warn); }

  .err { margin: 6px 0 0; color: var(--warn); font-size: 12px; }
  .ok-msg { margin: 6px 0 0; color: var(--success); font-size: 12px; }

  .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 14px; }
  .ghost {
    background: transparent; color: var(--text); border: 1px solid var(--border);
    padding: 7px 14px; font-size: 13px; font-weight: 500;
    border-radius: 6px; cursor: pointer;
  }
  .ghost:hover:not(:disabled) { background: var(--bg-hover); }
  .ghost:disabled { opacity: 0.4; cursor: not-allowed; }
  .primary {
    background: var(--accent); color: #1a0d0d; border: 0;
    padding: 7px 16px; font-size: 13px; font-weight: 600;
    border-radius: 6px; cursor: pointer;
  }
  .primary:hover:not(:disabled) { opacity: 0.92; }
  .primary:disabled { opacity: 0.4; cursor: not-allowed; }

  .info-list { margin: 0; padding-left: 18px; color: var(--text-muted); font-size: 12px; line-height: 1.75; }
  .info-list b { color: var(--text); font-weight: 500; }
  .info-list code {
    background: var(--bg-input); border: 1px solid var(--border);
    border-radius: 3px; padding: 1px 5px; font-size: 11px; color: var(--text);
  }

  /* Daimonion VAD settings (Phase D1) */
  .vad-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 12px 16px;
    margin: 12px 0;
  }
  .vad-grid label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .vad-grid label span { color: var(--text); font-weight: 500; }
  .vad-grid label small { color: var(--text-muted); font-size: 10px; }
  .vad-grid input {
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-input);
    color: var(--text);
    font: inherit;
  }
  .info-list kbd {
    display: inline-block; padding: 0 5px;
    font-family: ui-monospace, monospace; font-size: 10px;
    background: var(--bg-input); border: 1px solid var(--border);
    border-radius: 3px; color: var(--text);
  }
  .meta-grid {
    display: grid; grid-template-columns: 1fr 1fr; gap: 6px 16px;
    margin-top: 14px; padding-top: 14px;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
  }
  .meta-row { display: flex; align-items: center; gap: 8px; padding: 4px 0; font-size: 11px; }
  .meta-k { color: var(--text-muted); min-width: 80px; }
  .meta-v { color: var(--text); font-family: ui-monospace, monospace; font-size: 10px; }

  /* ---- Vault section ---- */
  .vault-add {
    border: 1px solid var(--border, #2a2a2e);
    border-radius: 8px;
    padding: 12px;
    margin: 12px 0 18px 0;
    background: var(--bg-elevated, #1c1c20);
  }
  .vault-add h4 {
    margin: 0 0 8px 0;
    font-size: 12px;
    font-weight: 600;
  }
  .vault-row {
    display: grid;
    grid-template-columns: 1.2fr 1.5fr 1.5fr auto;
    gap: 8px;
    align-items: end;
  }
  .vault-row label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--text-muted, #9aa0a6);
  }
  .vault-row input {
    padding: 6px 8px;
    border-radius: 6px;
    border: 1px solid var(--border, #2a2a2e);
    background: var(--bg, #0f1217);
    color: var(--text, #e6e8eb);
    font-size: 12px;
    font-family: inherit;
  }
  .vault-row input:focus {
    outline: 1px solid var(--accent, #8a7cff);
  }
  .vault-row .btn-primary {
    padding: 6px 14px;
    background: var(--accent, #8a7cff);
    color: white;
    border: 0;
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    font-size: 12px;
    font-weight: 600;
  }
  .vault-row .btn-primary:hover { opacity: 0.9; }
  .vault-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .vault-table th,
  .vault-table td {
    text-align: left;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border, #2a2a2e);
  }
  .vault-table th {
    color: var(--text-muted, #9aa0a6);
    font-weight: 600;
    font-size: 11px;
  }
  .vault-table .muted {
    color: var(--text-muted, #9aa0a6);
    font-size: 11px;
  }
  .vault-table .btn-icon {
    background: transparent;
    border: 0;
    color: var(--text-muted, #9aa0a6);
    cursor: pointer;
    font-size: 14px;
  }
  .vault-table .btn-icon:hover { color: var(--warn, #ff6b6b); }
  .hint-text.subtle { opacity: 0.7; font-size: 11px; }
  .banner {
    border-radius: 6px;
    padding: 6px 10px;
    margin: 8px 0;
    font-size: 12px;
  }
  .banner-error {
    background: var(--warn-soft, rgba(255, 107, 107, 0.12));
    color: var(--warn, #ff6b6b);
    border: 1px solid var(--warn, #ff6b6b);
  }
  .banner-ok {
    background: var(--ok-soft, rgba(120, 200, 120, 0.12));
    color: var(--ok, #78c878);
    border: 1px solid var(--ok, #78c878);
  }
</style>
