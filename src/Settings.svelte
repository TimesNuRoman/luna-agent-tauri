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
  // here, the рџ”‘ pill in App.svelte, and the chat composer placeholder
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
      icon: 'рџ”‘',
      desc: 'MiniMax-РєР»СЋС‡ РґР»СЏ Р·Р°РїСЂРѕСЃРѕРІ Рє РјРѕРґРµР»Рё',
      keywords: ['api', 'key', 'РєР»СЋС‡', 'minimax', 'token'],
    },
    {
      id: 'theme',
      label: 'Theme',
      icon: 'рџЋЁ',
      desc: 'РЎРІРµС‚Р»Р°СЏ, С‚С‘РјРЅР°СЏ РёР»Рё Р°РІС‚Рѕ (РєР°Рє РІ СЃРёСЃС‚РµРјРµ)',
      keywords: ['theme', 'appearance', 'color', 'dark', 'light', 'С‚РµРјР°', 'С†РІРµС‚', 'РѕС„РѕСЂРјР»РµРЅРёРµ', 'РІРЅРµС€РЅРёР№ РІРёРґ'],
    },
    {
      id: 'model',
      label: 'Model',
      icon: 'рџ§ ',
      desc: 'РњРѕРґРµР»СЊ РїРѕ СѓРјРѕР»С‡Р°РЅРёСЋ РґР»СЏ С‡Р°С‚Р°',
      keywords: ['model', 'РјРѕРґРµР»СЊ', 'ai', 'llm', 'm2', 'm3', 'text'],
    },
    {
      id: 'interests',
      label: 'News Interests',
      icon: 'рџ“°',
      desc: 'РўРµРјС‹ РґР»СЏ Р°РІС‚РѕРїРѕРґР±РѕСЂР° РІ News-СЂРµР¶РёРјРµ',
      keywords: ['news', 'interests', 'РЅРѕРІРѕСЃС‚Рё', 'С‚РµРјС‹', 'РёРЅС‚РµСЂРµСЃС‹', 'feed', 'rss'],
    },
    {
      id: 'voice',
      label: 'Voice & STT',
      icon: 'рџЋ™',
      desc: 'Р Р°СЃРїРѕР·РЅР°РІР°РЅРёРµ СЂРµС‡Рё Рё С…РѕС‚РєРµРё (РІ СЂР°Р·СЂР°Р±РѕС‚РєРµ)',
      keywords: ['voice', 'stt', 'whisper', 'РіРѕР»РѕСЃ', 'РјРёРєСЂРѕС„РѕРЅ', 'speech', 'mic'],
    },
    {
      id: 'telegram',
      label: 'Telegram Bot',
      icon: 'рџ¤–',
      desc: 'РЈРїСЂР°РІР»РµРЅРёРµ Р±РѕС‚РѕРј РґР»СЏ СѓРґР°Р»С‘РЅРЅРѕРіРѕ РґРѕСЃС‚СѓРїР° Рє Р°РіРµРЅС‚Сѓ',
      keywords: ['telegram', 'bot', 'tg', 'С‚РµР»РµРіСЂР°Рј', 'remote', 'СѓРґР°Р»С‘РЅРЅРѕ'],
    },
    {
      id: 'vault',
      label: 'рџ”ђ Vault',
      icon: 'рџ”ђ',
      desc: 'Р›РѕРіРёРЅС‹ Рё РїР°СЂРѕР»Рё РґР»СЏ Azazel (С…СЂР°РЅСЏС‚СЃСЏ РІ OS keyring, LLM РёС… РЅРµ РІРёРґРёС‚)',
      keywords: ['vault', 'password', 'credentials', 'login', 'azazel', 'browser',
                 'РїР°СЂРѕР»СЊ', 'Р»РѕРіРёРЅ', 'РєР»СЋС‡', 'credentials', 'secret', 'keyring'],
    },
    {
      id: 'shell',
      label: 'Shell',
      icon: 'рџ–Ґ',
      desc: 'Allow-list СЃРёСЃС‚РµРјРЅС‹С… РєРѕРјР°РЅРґ (PowerShell, bash, cmd, cargo, gitвЂ¦)',
      keywords: ['shell', 'bash', 'powershell', 'cmd', 'pwsh', 'terminal', 'С€РµР»Р»', 'РєРѕРјР°РЅРґРЅР°СЏ СЃС‚СЂРѕРєР°'],
    },
    {
      id: 'self_evolution',
      label: 'Self-evolution',
      icon: 'рџ§¬',
      desc: 'Luna С‡РёС‚Р°РµС‚ Рё СѓР»СѓС‡С€Р°РµС‚ СЃРѕР±СЃС‚РІРµРЅРЅС‹Рµ РёСЃС…РѕРґРЅРёРєРё (E0вЂ“E4)',
      keywords: ['self', 'evolution', 'evolver', 'snapshot', 'sandbox', 'update', 'rollback', 'apply', 'luna', 'СЃР°РјРѕСѓР»СѓС‡С€РµРЅРёРµ', 'СЌРІРѕР»СЋС†РёСЏ', 'РѕР±РЅРѕРІР»РµРЅРёРµ'],
    },
    {
      id: 'about',
      label: 'About',
      icon: 'в„№',
      desc: 'Рћ РїСЂРёР»РѕР¶РµРЅРёРё Рё РїРѕРґСЃРєР°Р·РєРё',
      keywords: ['about', 'info', 'version', 'Рѕ РїСЂРёР»РѕР¶РµРЅРёРё', 'РїРѕРјРѕС‰СЊ', 'help'],
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
    if (!confirm(`РЈРґР°Р»РёС‚СЊ "${name}" РёР· allow-list?`)) return;
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
    if (!confirm('РЎР±СЂРѕСЃРёС‚СЊ allow-list Рє РІСЃС‚СЂРѕРµРЅРЅС‹Рј РґРµС„РѕР»С‚Р°Рј?')) return;
    shellBusy = true;
    try {
      shellList = await resetShellAllowList();
    } catch (e) {
      console.warn('reset_shell_allow_list failed', e);
    } finally {
      shellBusy = false;
    }
  }

  // (removed) `refreshKey()` вЂ” the shared `refreshKeyStatus()` from
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
  // вЂ” picks up edits made in another window or by the bot.
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
      vaultError = 'Р—Р°РїРѕР»РЅРё domain, login Рё password.';
      return;
    }
    try {
      await vaultSet(domain, login, password);
      vaultDraft = { domain: '', login: '', password: '' };
      vaultLoaded = false;
      await refreshVault(true);
      vaultSavedFlash = 'вњ“ СЃРѕС…СЂР°РЅРµРЅРѕ';
      if (vaultSaveTimer) clearTimeout(vaultSaveTimer);
      vaultSaveTimer = setTimeout(() => (vaultSavedFlash = ''), 2000);
    } catch (e) {
      vaultError = `save: ${e}`;
    }
  }

  async function deleteVault(domain: string) {
    if (!confirm(`РЈРґР°Р»РёС‚СЊ credential РґР»СЏ "${domain}"?`)) return;
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
    if (!trimmed) { keyError = 'Р’РІРµРґРёС‚Рµ РєР»СЋС‡'; return; }
    saving = true;
    keyError = '';
    try {
      await saveKey(trimmed);
      keyValue = '';
      keySavedFlash = 'API-РєР»СЋС‡ СЃРѕС…СЂР°РЅС‘РЅ';
      setTimeout(() => (keySavedFlash = ''), 2000);
    } catch (e) {
      keyError = String(e);
    } finally {
      saving = false;
    }
  }

  async function clearKeyAction() {
    if (!confirm('РЈРґР°Р»РёС‚СЊ СЃРѕС…СЂР°РЅС‘РЅРЅС‹Р№ MiniMax-РєР»СЋС‡ РёР· Credential Manager?')) return;
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
      interestsSaved = 'РЎРѕС…СЂР°РЅРµРЅРѕ';
      setTimeout(() => (interestsSaved = ''), 2000);
    } catch {
      interestsSaved = 'РћС€РёР±РєР°';
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
      <span class="search-icon">рџ”Ќ</span>
      <input
        class="search"
        type="text"
        placeholder="Search settingsвЂ¦"
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
        <div class="nav-empty">РќРёС‡РµРіРѕ РЅРµ РЅР°Р№РґРµРЅРѕ</div>
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
            <h3>рџ”‘ MiniMax API Key</h3>
            <span class="badge" class:ok={hasMinimax} class:miss={!hasMinimax && !checking}>
              {checking ? 'вЂ¦' : hasMinimax ? 'set' : 'missing'}
            </span>
          </div>
          <p class="hint-text">
            РљР»СЋС‡ С…СЂР°РЅРёС‚СЃСЏ РІ Windows Credential Manager С‡РµСЂРµР· Rust keyring. Р’ С‡Р°С‚-С‚СЂР°С„РёРє РЅРµ СѓС…РѕРґРёС‚.
            РџРѕР»СѓС‡РёС‚СЊ: <a href="https://platform.minimaxi.com/" target="_blank" rel="noopener">platform.minimaxi.com в†’ API Keys</a>.
          </p>

          <div class="row">
            {#if showKey}
              <input
                type="text"
                bind:value={keyValue}
                placeholder="sk-cp-вЂ¦ РёР»Рё eyJвЂ¦"
                autocomplete="off"
                spellcheck="false"
                disabled={saving}
                on:keydown={(e) => { if (e.key === 'Enter') saveKeyAction(); }}
              />
            {:else}
              <input
                type="password"
                bind:value={keyValue}
                placeholder="sk-cp-вЂ¦ РёР»Рё eyJвЂ¦"
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
              aria-label={showKey ? 'РЎРєСЂС‹С‚СЊ РєР»СЋС‡' : 'РџРѕРєР°Р·Р°С‚СЊ РєР»СЋС‡'}
              title={showKey ? 'РЎРєСЂС‹С‚СЊ' : 'РџРѕРєР°Р·Р°С‚СЊ'}
              tabindex="-1"
            >{showKey ? 'рџ™€' : 'рџ‘Ѓ'}</button>
          </div>

          {#if keyError}<p class="err">вљ  {keyError}</p>{/if}
          {#if keySavedFlash}<p class="ok-msg">вњ“ {keySavedFlash}</p>{/if}

          <div class="actions">
            <button class="ghost" type="button" on:click={clearKeyAction} disabled={!hasMinimax || saving}>РћС‡РёСЃС‚РёС‚СЊ</button>
            <button class="primary" type="button" on:click={saveKeyAction} disabled={saving || !keyValue.trim()}>
              {saving ? 'РЎРѕС…СЂР°РЅРµРЅРёРµвЂ¦' : 'РЎРѕС…СЂР°РЅРёС‚СЊ'}
            </button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'theme'}
        <section class="block">
          <div class="row-head">
            <h3>рџЋЁ РўРµРјР° РѕС„РѕСЂРјР»РµРЅРёСЏ</h3>
            <span class="badge neutral">{theme === 'auto' ? 'auto' : theme}</span>
          </div>
          <p class="hint-text">
            РЎРІРµС‚Р»Р°СЏ С‚РµРјР° вЂ” РїРѕ СѓРјРѕР»С‡Р°РЅРёСЋ. В«РђРІС‚РѕВ» СЃР»РµРґСѓРµС‚ Р·Р° СЃРёСЃС‚РµРјРЅРѕР№ С‚РµРјРѕР№ Windows.
            Р’С‹Р±РѕСЂ СЃРѕС…СЂР°РЅСЏРµС‚СЃСЏ РІ localStorage Рё РїСЂРёРјРµРЅСЏРµС‚СЃСЏ РґРѕ РїРµСЂРІРѕР№ РѕС‚СЂРёСЃРѕРІРєРё.
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
                <span class="theme-name">вЂ РЎРІРµС‚Р»Р°СЏ</span>
                <span class="theme-sub">РјСЏРіРєРёР№ С‚С‘РїР»С‹Р№ С„РѕРЅ</span>
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
                <span class="theme-name">рџЊ™ РўС‘РјРЅР°СЏ</span>
                <span class="theme-sub">РїСЂРёРІС‹С‡РЅС‹Р№ РЅРѕС‡РЅРѕР№ СЂРµР¶РёРј</span>
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
                <span class="theme-name">рџ–Ґ РђРІС‚Рѕ</span>
                <span class="theme-sub">РєР°Рє РІ Windows</span>
              </div>
            </button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'model'}
        <section class="block">
          <div class="row-head">
            <h3>рџ§  РњРѕРґРµР»СЊ РїРѕ СѓРјРѕР»С‡Р°РЅРёСЋ</h3>
            <span class="badge neutral">{currentModel.label}</span>
          </div>
          <p class="hint-text">
            РџСЂРёРјРµРЅСЏРµС‚СЃСЏ Рє С‡Р°С‚Сѓ. Р’ РѕРєРЅРµ С‡Р°С‚Р° РјРѕРґРµР»СЊ РјРѕР¶РЅРѕ РїРµСЂРµРєР»СЋС‡РёС‚СЊ С‡РµСЂРµР· dropdown РІ С€Р°РїРєРµ.
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
                  <span class="model-meta">API: {m.model || 'РїРѕ СѓРјРѕР»С‡Р°РЅРёСЋ РїСЂРѕРІР°Р№РґРµСЂР°'}</span>
                </span>
              </label>
            {/each}
          </div>
        </section>
      {/if}

      {#if activeSection === 'interests'}
        <section class="block">
          <div class="row-head">
            <h3>рџ“° РРЅС‚РµСЂРµСЃС‹ РґР»СЏ News-Р°РіРµРЅС‚Р°</h3>
            <span class="badge neutral">{interestsList.length} С‚РµРј</span>
          </div>
          <p class="hint-text">
            News-СЂРµР¶РёРј РёСЃРїРѕР»СЊР·СѓРµС‚ СЌС‚Рё С‚РµРјС‹ РґР»СЏ Р°РІС‚РѕРїРѕРґР±РѕСЂР° РјР°С‚РµСЂРёР°Р»РѕРІ С‡РµСЂРµР· DuckDuckGo.
            Р§РµСЂРµР· Р·Р°РїСЏС‚СѓСЋ РёР»Рё СЃ РЅРѕРІРѕР№ СЃС‚СЂРѕРєРё. РџСЂРёРјРµСЂС‹: <code>AI, Rust, Tauri, anime, РєРѕСЃРјРѕСЃ</code>.
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
              <span class="chip empty">вљ  РїСѓСЃС‚Рѕ</span>
            {/if}
          </div>
          {#if interestsSaved}<p class="ok-msg">вњ“ {interestsSaved}</p>{/if}
          <div class="actions">
            <button class="primary" type="button" on:click={saveInterests}>РЎРѕС…СЂР°РЅРёС‚СЊ РёРЅС‚РµСЂРµСЃС‹</button>
          </div>
        </section>
      {/if}

      {#if activeSection === 'voice'}
        <section class="block">
          <div class="row-head">
            <h3>рџЋ™ Р“РѕР»РѕСЃРѕРІРѕР№ РІРІРѕРґ (Whisper)</h3>
            <span class="badge neutral">Р»РѕРєР°Р»СЊРЅС‹Р№ fallback</span>
          </div>
          <p class="hint-text">
            Whisper-РјРѕРґРµР»Рё СѓРїСЂР°РІР»СЏСЋС‚СЃСЏ РїСЂСЏРјРѕ РІ С‡Р°С‚Рµ вЂ” РЅР°Р¶РјРё рџЋ™ РІ РєРѕРјРїРѕР·РµСЂРµ, Рё РµСЃР»Рё РјРѕРґРµР»Рё РЅРµС‚,
            РїРѕСЏРІРёС‚СЃСЏ РїСЂРµРґР»РѕР¶РµРЅРёРµ СЃРєР°С‡Р°С‚СЊ. Р“Р»РѕР±Р°Р»СЊРЅС‹Р№ С…РѕС‚РєРµР№: <kbd>Ctrl</kbd>+<kbd>Space</kbd>.
          </p>
          <ul class="info-list">
            <li><b>РЎС‚Р°СЂС‚/СЃС‚РѕРї Р·Р°РїРёСЃРё</b>: рџЋ™ РІ РєРѕРјРїРѕР·РµСЂРµ С‡Р°С‚Р° РёР»Рё <kbd>Ctrl</kbd>+<kbd>Space</kbd> РіР»РѕР±Р°Р»СЊРЅРѕ</li>
            <li><b>РњРѕРґРµР»Рё</b>: <code>base</code> в‰€ 140 РњР‘, <code>small</code> в‰€ 460 РњР‘ вЂ” СЂРµРєРѕРјРµРЅРґСѓСЋ <code>base</code></li>
            <li><b>Р“РґРµ С…СЂР°РЅСЏС‚СЃСЏ</b>: <code>%APPDATA%\com.luna.agent\whisper-models\</code></li>
            <li><b>РўРµРєСѓС‰Р°СЏ РѕС€РёР±РєР°</b> (РµСЃР»Рё Р±С‹Р»Р°): <code>stt:allow-list-models</code> вЂ” СѓР¶Рµ РґРѕР±Р°РІР»РµРЅРѕ РІ capabilities</li>
          </ul>
        </section>

        <section class="block">
          <div class="row-head">
            <h3>рџ”® Daimonion вЂ” РіРѕР»РѕСЃРѕРІРѕР№ Р°СЃСЃРёСЃС‚РµРЅС‚</h3>
            <span class="badge ok">D0+ РіРѕС‚РѕРІ</span>
          </div>
          <p class="hint-text">
            Daimonion (О”О±О№ОјПЊОЅО№ОїОЅ) вЂ” РіРѕР»РѕСЃ-РїРµСЂРІС‹Р№ РјСѓР»СЊС‚РёРјРѕРґР°Р»СЊРЅС‹Р№ Р°СЃСЃРёСЃС‚РµРЅС‚
            РёР· С‚С‘РјРЅРѕР№ Р»РёРЅРµР№РєРё Luna (СЂСЏРґРѕРј СЃ Lucifer / Azazel / Raziel /
            Mephistopheles). STT Рё TTS РёРґСѓС‚ С‡РµСЂРµР· <b>MiniMax</b> (РЅСѓР¶РЅР°
            Р°РєС‚РёРІРЅР°СЏ РїРѕРґРїРёСЃРєР° Ultra Рё MiniMax-РєР»СЋС‡ РІ API Keys).
            Push-to-talk РІ РїР°РЅРµР»Рё Daimonion вЂ” <kbd>Space</kbd>.
          </p>

          <div class="vad-grid">
            <label>
              <span>Energy threshold</span>
              <input type="number" min="0.001" max="0.5" step="0.001"
                bind:value={vad.energy_threshold} />
              <small>0.001вЂ“0.5. РќРёР¶Рµ = С‡СѓРІСЃС‚РІРёС‚РµР»СЊРЅРµРµ.</small>
            </label>
            <label>
              <span>Start hold (frames)</span>
              <input type="number" min="1" max="50" step="1"
                bind:value={vad.start_hold_frames} />
              <small>РЎРєРѕР»СЊРєРѕ С„СЂРµР№РјРѕРІ РїРѕРґСЂСЏРґ РІС‹С€Рµ РїРѕСЂРѕРіР° РґРѕ СЃС‚Р°СЂС‚Р° СЂРµС‡Рё.</small>
            </label>
            <label>
              <span>End hold (frames)</span>
              <input type="number" min="10" max="500" step="1"
                bind:value={vad.end_hold_frames} />
              <small>РўРёС€РёРЅР° (РјСЃ = frames Г— 10) РґРѕ РєРѕРЅС†Р° С„СЂР°Р·С‹.</small>
            </label>
            <label>
              <span>Frame (ms)</span>
              <input type="number" min="5" max="50" step="1"
                bind:value={vad.frame_ms} />
              <small>Р Р°Р·РјРµСЂ С„СЂРµР№РјР° РґР»СЏ VAD-РјР°С‚РµРјР°С‚РёРєРё.</small>
            </label>
          </div>

          <ul class="info-list">
            <li><b>Pipeline</b>: cpal mic в†’ VAD в†’ MiniMax ASR в†’ MiniMax-M3 в†’ MiniMax T2A (speech-02) в†’ cpal</li>
            <li><b>Latency budget</b>: в‰¤ 1.5 СЃ p50, в‰¤ 2.5 СЃ p95 end-to-end</li>
            <li><b>Vision</b>: РјРѕРґРµР»СЊ СЃР°РјР° СЂРµС€Р°РµС‚, РєРѕРіРґР° СЃРјРѕС‚СЂРµС‚СЊ СЌРєСЂР°РЅ (D2+); РЅРµ always-on</li>
            <li><b>Read-only</b>: Daimonion СЃРјРѕС‚СЂРёС‚ Рё РіРѕРІРѕСЂРёС‚, РЅРµ РїСЂР°РІРёС‚ С„Р°Р№Р»С‹</li>
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
            <h3>рџ”ђ Azazel Vault</h3>
            <span class="badge neutral">
              {vaultEntries.length} {vaultEntries.length === 1 ? 'Р·Р°РїРёСЃСЊ' : 'Р·Р°РїРёСЃРµР№'}
            </span>
          </div>
          <p class="hint-text">
            Р›РѕРіРёРЅС‹ Рё РїР°СЂРѕР»Рё РґР»СЏ СЃР°Р№С‚РѕРІ, РЅР° РєРѕС‚РѕСЂС‹Рµ Azazel РґРѕР»Р¶РµРЅ СѓРјРµС‚СЊ Р»РѕРіРёРЅРёС‚СЊСЃСЏ
            (VK, GitHub, Gmail, etc). РҐСЂР°РЅСЏС‚СЃСЏ РІ OS keyring вЂ” <strong>РјРѕРґРµР»СЊ РёС… РЅРёРєРѕРіРґР° РЅРµ
            РІРёРґРёС‚</strong>. РљРѕРіРґР° РјРѕРґРµР»СЊ РІС‹Р·С‹РІР°РµС‚ <code>azazel_run</code> СЃ
            <code>vault_domain="vk.com"</code>, РїР°СЂРѕР»СЊ РїРѕРґСЃС‚Р°РІР»СЏРµС‚СЃСЏ РІ Р±СЂР°СѓР·РµСЂРЅСѓСЋ СЃРµСЃСЃРёСЋ
            СЃРµСЂРІРµСЂРѕРј Azazel.
          </p>

          {#if vaultError}
            <div class="banner banner-error">вљ  {vaultError}</div>
          {/if}
          {#if vaultSavedFlash}
            <div class="banner banner-ok">{vaultSavedFlash}</div>
          {/if}

          <div class="vault-add">
            <h4>+ Р”РѕР±Р°РІРёС‚СЊ credential</h4>
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
                  placeholder="вЂўвЂўвЂўвЂўвЂўвЂўвЂўвЂў"
                  bind:value={vaultDraft.password}
                />
              </label>
              <button class="btn-primary" type="button" on:click={saveVaultDraft}>
                РЎРѕС…СЂР°РЅРёС‚СЊ
              </button>
            </div>
            <p class="hint-text subtle">
              Р”РѕРјРµРЅ РЅРѕСЂРјР°Р»РёР·СѓРµС‚СЃСЏ: <code>https://www.VK.com/login</code> в†’ <code>vk.com</code>.
              Р§СѓРІСЃС‚РІРёС‚РµР»СЊРЅС‹Рµ РґР°РЅРЅС‹Рµ С€РёС„СЂСѓСЋС‚СЃСЏ OS keyring.
            </p>
          </div>

          <h4>РЎРѕС…СЂР°РЅС‘РЅРЅС‹Рµ credentials</h4>
          {#if vaultLoading}
            <p class="hint-text">Р—Р°РіСЂСѓР·РєР°вЂ¦</p>
          {:else if vaultEntries.length === 0}
            <p class="hint-text subtle">
              РџРѕРєР° РїСѓСЃС‚Рѕ. Р”РѕР±Р°РІСЊ РїРµСЂРІСѓСЋ Р·Р°РїРёСЃСЊ РІС‹С€Рµ вЂ” Azazel СЃРјРѕР¶РµС‚ Р»РѕРіРёРЅРёС‚СЊСЃСЏ
              Р°РІС‚РѕРјР°С‚РёС‡РµСЃРєРё.
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
                        <span class="muted">вЂўвЂўвЂўвЂўвЂўвЂўвЂўвЂў (РІ keyring)</span>
                      {:else}
                        <span class="muted">вЂ”</span>
                      {/if}
                    </td>
                    <td class="muted">{e.updated_at}</td>
                    <td>
                      <button
                        class="btn-icon"
                        type="button"
                        title="РЈРґР°Р»РёС‚СЊ"
                        on:click={() => deleteVault(e.domain)}
                      >рџ—‘</button>
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
            <h3>рџ–Ґ Shell-РєРѕРјР°РЅРґС‹</h3>
            <span class="badge neutral">{shellList?.commands.length ?? 0} РєРѕРјР°РЅРґ</span>
          </div>
          <p class="hint-text">
            РўРѕР»СЊРєРѕ РїРµСЂРµС‡РёСЃР»РµРЅРЅС‹Рµ РєРѕРјР°РЅРґС‹ РјРѕРіСѓС‚ РІС‹Р·С‹РІР°С‚СЊСЃСЏ РёР· Р°РіРµРЅС‚Р° / С‡Р°С‚Р°. РџРѕРІРµРґРµРЅРёРµ: argv-only, Р±РµР· <code>sh -c</code> / <code>cmd /c "&lt;string&gt;"</code> вЂ” РЅРёРєР°РєРѕР№ shell-РёРЅСЉРµРєС†РёРё.
            Р¤Р°Р№Р» allow-list: <code>%LOCALAPPDATA%\luna-agent\shell-allowlist.json</code>.
          </p>

          <div class="shell-list">
            {#each shellList?.commands ?? [] as e (e.name)}
              <div class="shell-row" class:empty={(e.subcommand_patterns ?? []).length === 0}>
                <div class="shell-name">{e.name}</div>
                <div class="shell-subs">
                  {#if (e.subcommand_patterns ?? []).length === 0}
                    <span class="shell-empty-tag">Р»СЋР±С‹Рµ subcommand</span>
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
                  title="РЈРґР°Р»РёС‚СЊ РёР· allow-list"
                >вњ•</button>
              </div>
            {/each}
          </div>

          <div class="shell-add">
            <input
              type="text"
              class="shell-input"
              placeholder="РёРјСЏ (powershell, bash, cmd, вЂ¦)"
              bind:value={newCmdName}
              on:keydown={(e) => { if (e.key === 'Enter') addCmd(); }}
            />
            <input
              type="text"
              class="shell-input shell-subs-input"
              placeholder="subcommand (С‡РµСЂРµР· Р·Р°РїСЏС‚СѓСЋ, РѕРїС†РёРѕРЅР°Р»СЊРЅРѕ)"
              bind:value={newCmdSubs}
              on:keydown={(e) => { if (e.key === 'Enter') addCmd(); }}
            />
            <button type="button" class="primary" on:click={addCmd} disabled={!newCmdName.trim()}>+ Р”РѕР±Р°РІРёС‚СЊ</button>
            <button type="button" class="secondary" on:click={resetShellList}>в†є Р”РµС„РѕР»С‚С‹</button>
          </div>
          <p class="shell-hint">
            РџСЂРёРјРµСЂС‹: <code>bash</code>, <code>cmd</code>, <code>powershell</code>, <code>cargo</code>, <code>pytest</code>, <code>node</code>.
            Р•СЃР»Рё РІ РїРѕР»Рµ <em>subcommand</em> СѓРєР°Р·Р°С‚СЊ <code>test, build, run</code> вЂ” РґРѕРїСѓСЃС‚РёРјС‹ С‚РѕР»СЊРєРѕ СЌС‚Рё subcommand-С‹ (РґР»СЏ РєРѕРјР°РЅРґ СЃ СЏРІРЅРѕР№ РїРѕРґРєРѕРјР°РЅРґРЅРѕР№ СЃС‚СЂСѓРєС‚СѓСЂРѕР№). РџСѓСЃС‚Рѕ = Р»СЋР±С‹Рµ.
          </p>
        </section>
      {/if}

      {#if activeSection === 'self_evolution'}
        <section class="block">
          <div class="row-head">
            <h3>рџ§¬ Self-evolution (E0вЂ“E4)</h3>
            <span class="badge neutral">СЌРєСЃРїРµСЂРёРјРµРЅС‚Р°Р»СЊРЅРѕ</span>
          </div>
          <p class="hint-text">
            Luna РјРѕР¶РµС‚ С‡РёС‚Р°С‚СЊ СЃРѕР±СЃС‚РІРµРЅРЅС‹Рµ РёСЃС…РѕРґРЅРёРєРё, РЅР°С…РѕРґРёС‚СЊ РІ РЅРёС… РїСЂРѕР±Р»РµРјС‹,
            СЃРѕСЃС‚Р°РІР»СЏС‚СЊ РїР»Р°РЅ РїСЂР°РІРѕРє, РїСЂРѕРІРµСЂСЏС‚СЊ РµРіРѕ РІ РїРµСЃРѕС‡РЅРёС†Рµ (sandbox) Рё
            Р°С‚РѕРјР°СЂРЅРѕ РѕР±РЅРѕРІР»СЏС‚СЊ СЃРІРѕР№ Р±РёРЅР°СЂСЊ. <b>РўРѕР»СЊРєРѕ СЂСѓС‡РЅРѕР№ Р·Р°РїСѓСЃРє</b>:
            РІС‹ РЅР°Р¶РёРјР°РµС‚Рµ РєРЅРѕРїРєСѓ, РІРёРґРёС‚Рµ РѕС‚С‡С‘С‚ Рё РїРѕРґС‚РІРµСЂР¶РґР°РµС‚Рµ apply.
          </p>
          <ul class="info-list">
            <li><b>Р’СЃРµ СЌС‚Р°РїС‹ РІРєР»СЋС‡РµРЅС‹ РІ С‚РµРєСѓС‰РµР№ СЃР±РѕСЂРєРµ</b> (E0 inspect В· E1 snapshots В· E2 diagnose В· E3 sandbox В· E4 apply/rollback)</li>
            <li><b>Р“РґРµ Р»РµР¶Р°С‚ РґР°РЅРЅС‹Рµ</b>: <code>%LOCALAPPDATA%\com.luna.agent\evolver\</code>
              <ul>
                <li><code>active.json</code> вЂ” С‚РµРєСѓС‰Р°СЏ РІРµСЂСЃРёСЏ</li>
                <li><code>snapshots/&lt;id&gt;/src/</code> вЂ” РїРѕР»РЅС‹Рµ РєРѕРїРёРё РёСЃС…РѕРґРЅРёРєРѕРІ</li>
                <li><code>feedback/&lt;id&gt;.json</code> вЂ” РІР°С€ С„РёРґР±РµРє (РёСЃРїРѕР»СЊР·СѓРµС‚СЃСЏ РІ СЃР»РµРґСѓСЋС‰РµР№ diagnose)</li>
                <li><code>plans/&lt;id&gt;.json</code> вЂ” РїРѕСЃР»РµРґРЅРёРµ РїР»Р°РЅС‹</li>
              </ul>
            </li>
            <li><b>РћРєСЂСѓР¶РµРЅРёРµ</b>: <code>LUNA_SOURCE_ROOT</code> env var РёР»Рё Р°РІС‚РѕРґРµС‚РµРєС‚ РѕС‚ Р±РёРЅР°СЂСЏ</li>
            <li><b>Р—Р°С‰РёС‰С‘РЅРЅС‹Рµ С„Р°Р№Р»С‹</b> (worker РѕС‚РєР°Р¶РµС‚СЃСЏ РёС… С‚СЂРѕРіР°С‚СЊ): <code>Cargo.toml</code>, <code>tauri.conf.json</code>, <code>package.json</code>, <code>capabilities/default.json</code>, <code>vendor/</code>, <code>LICENSE*</code></li>
            <li><b>РўСЂРµР±РѕРІР°РЅРёСЏ</b>: Rust toolchain (cargo), <code>tauri-build.cmd</code> РІ PATH (Windows)</li>
            <li><b>GC</b>: 5 РїРѕСЃР»РµРґРЅРёС… non-important + РІСЃРµ important + active</li>
            <li><b>РћС‚РєСЂС‹С‚СЊ РІРєР»Р°РґРєСѓ</b>: <code>рџ§¬ Self</code> РІ С€Р°РїРєРµ РіР»Р°РІРЅРѕРіРѕ РѕРєРЅР°</li>
          </ul>
          <p class="hint-text">
            <b>Р’РЅРёРјР°РЅРёРµ:</b> apply/rollback РїРµСЂРµСЃРѕР±РёСЂР°РµС‚ Р±РёРЅР°СЂСЊ Рё Р°С‚РѕРјР°СЂРЅРѕ
            РїРѕРґРјРµРЅСЏРµС‚ РµРіРѕ. <b>Р’СЃРµРіРґР° РґРµР»Р°РµС‚СЃСЏ pre-update snapshot</b>, Рє
            РєРѕС‚РѕСЂРѕРјСѓ РјРѕР¶РЅРѕ РѕС‚РєР°С‚РёС‚СЊСЃСЏ. Р’ Windows РїРѕСЃР»Рµ СѓСЃРїРµС€РЅРѕРіРѕ apply
            РЅСѓР¶РЅРѕ РІСЂСѓС‡РЅСѓСЋ РїРµСЂРµР·Р°РїСѓСЃС‚РёС‚СЊ Luna, С‡С‚РѕР±С‹ Р·Р°РіСЂСѓР·РёС‚СЊ РЅРѕРІС‹Р№ .exe.
          </p>
        </section>
      {/if}

      {#if activeSection === 'about'}
        <section class="block">
          <h3>в„№ About Luna Agent</h3>
          <p class="hint-text">Tauri 2 + Svelte РґРµСЃРєС‚РѕРї-Р°РіРµРЅС‚. РЎС‚СЂРёРјРёРЅРі, DuckDuckGo, Р»РѕРєР°Р»СЊРЅС‹Р№ keyring, РіРѕР»РѕСЃ С‡РµСЂРµР· Whisper.</p>
          <ul class="info-list">
            <li><b>Р“РѕР»РѕСЃРѕРІРѕР№ РІРІРѕРґ</b>: рџЋ™ РІ РєРѕРјРїРѕР·РµСЂРµ РёР»Рё <kbd>Ctrl</kbd>+<kbd>Space</kbd></li>
            <li><b>News-Р°РіРµРЅС‚</b>: рџ“° РІ С€Р°РїРєРµ С‡Р°С‚Р° в†’ Р°РІС‚РѕРїРѕРґР±РѕСЂ РїРѕ РёРЅС‚РµСЂРµСЃР°Рј</li>
            <li><b>РљР»РёРєР°Р±РµР»СЊРЅС‹Рµ СЃСЃС‹Р»РєРё</b> РІ РѕС‚РІРµС‚Р°С… в†’ СЃРёСЃС‚РµРјРЅС‹Р№ Р±СЂР°СѓР·РµСЂ</li>
            <li><b>РЎС‚СЂРёРјРёРЅРі</b> РїРѕРєР° РЅРµ РїРѕРґРєР»СЋС‡С‘РЅ вЂ” MiniMax РІРѕР·РІСЂР°С‰Р°РµС‚ РїРѕР»РЅС‹Р№ РѕС‚РІРµС‚</li>
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
