<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import {
    getTelegramStatus,
    setTelegramToken,
    clearTelegramToken,
    setTelegramAllowList,
    startTelegramBot,
    stopTelegramBot,
    type TelegramStatus,
  } from './lib/tauri';

  let status: TelegramStatus | null = null;
  let tokenInput = '';
  let allowListInput = '';
  let busy = false;
  let lastError = '';
  let lastInfo = '';
  let pollHandle: number | null = null;

  async function refresh() {
    try {
      status = await getTelegramStatus();
    } catch (e) {
      lastError = String(e);
    }
  }

  onMount(() => {
    refresh();
    // Light polling so the pill / status reflects dispatcher state.
    pollHandle = window.setInterval(refresh, 3000);
  });
  onDestroy(() => {
    if (pollHandle !== null) clearInterval(pollHandle);
  });

  async function saveToken() {
    busy = true; lastError = ''; lastInfo = '';
    try {
      const t = tokenInput.trim();
      if (!t) return;
      await setTelegramToken(t);
      tokenInput = '';
      lastInfo = 'Token saved to keyring.';
      await refresh();
    } catch (e) { lastError = String(e); }
    finally { busy = false; }
  }

  async function deleteToken() {
    busy = true; lastError = ''; lastInfo = '';
    try {
      await clearTelegramToken();
      lastInfo = 'Token cleared.';
      await refresh();
    } catch (e) { lastError = String(e); }
    finally { busy = false; }
  }

  async function saveAllowList() {
    busy = true; lastError = ''; lastInfo = '';
    try {
      const ids = allowListInput
        .split(/[\s,]+/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
        .map((s) => Number(s))
        .filter((n) => Number.isInteger(n) && n > 0);
      await setTelegramAllowList(ids);
      lastInfo = `Allow list saved (${ids.length} ids).`;
      await refresh();
    } catch (e) { lastError = String(e); }
    finally { busy = false; }
  }

  async function start() {
    busy = true; lastError = ''; lastInfo = '';
    try {
      const username = await startTelegramBot();
      lastInfo = `Bot started: @${username}`;
      await refresh();
    } catch (e) { lastError = String(e); }
    finally { busy = false; }
  }

  async function stop() {
    busy = true; lastError = ''; lastInfo = '';
    try {
      await stopTelegramBot();
      lastInfo = 'Bot stopped.';
      await refresh();
    } catch (e) { lastError = String(e); }
    finally { busy = false; }
  }

  $: statusLabel = status
    ? status.running
      ? `🟢 Running as @${status.bot_username ?? '?'}`
      : status.token_set
        ? '🟡 Token set, stopped'
        : '🔴 No token'
    : '…';
</script>

<div class="tg-section">
  <h3>🤖 Telegram Bot</h3>
  <p class="muted">
    Управляйте агентом с телефона: чат, чтение/правка файлов, поиск,
    shell-команды из allow-list, загрузка файлов. Бот живёт в Luna Agent и
    стартует по кнопке (не автоматически).
  </p>

  <div class="row status-row">
    <span class="status-pill">{statusLabel}</span>
    {#if status}
      <span class="muted small">
        {status.allow_list_size} user(s) in allow-list ·
        last activity {status.last_activity_ms
          ? new Date(status.last_activity_ms).toLocaleTimeString()
          : '—'}
      </span>
    {/if}
  </div>

  {#if lastError}
    <div class="banner err">⚠ {lastError}</div>
  {/if}
  {#if lastInfo}
    <div class="banner ok">✓ {lastInfo}</div>
  {/if}

  <h4>Bot token</h4>
  <p class="muted small">
    Получите у <a href="https://t.me/BotFather" target="_blank" rel="noopener">@BotFather</a>
    → /newbot. Токен хранится в keyring и НЕ отображается в UI.
  </p>
  <div class="row">
    <input
      type="password"
      bind:value={tokenInput}
      placeholder="123456:ABC-DEF…"
      autocomplete="off"
      spellcheck="false"
      disabled={busy}
    />
    <button class="primary" on:click={saveToken} disabled={busy || !tokenInput.trim()}>
      Save
    </button>
    <button class="ghost" on:click={deleteToken} disabled={busy || !status?.token_set}>
      Clear
    </button>
  </div>

  <h4>Allow-list (Telegram user IDs)</h4>
  <p class="muted small">
    Только эти пользователи смогут управлять ботом. ID можно узнать, отправив
    боту <code>/start</code> — в ответе будет ваш ID.
  </p>
  <div class="row">
    <input
      type="text"
      bind:value={allowListInput}
      placeholder="123456789, 987654321"
      disabled={busy}
    />
    <button class="primary" on:click={saveAllowList} disabled={busy || !allowListInput.trim()}>
      Save
    </button>
  </div>

  <h4>Lifecycle</h4>
  <div class="row">
    <button class="primary" on:click={start}
      disabled={busy || !status?.token_set || status.running}>
      ▶ Start
    </button>
    <button class="ghost" on:click={stop} disabled={busy || !status?.running}>
      ■ Stop
    </button>
  </div>

  <h4>Команды в Telegram</h4>
  <pre class="cmd-list">
/start /help /status /whoami
/workspace [path]   /ls [path] -d N
/read &lt;path&gt;       /find &lt;query&gt; [-g glob] [-r] [-c]
/edit &lt;path&gt;       → OLD → NEW → /apply
/revert &lt;edit_id&gt;
/create &lt;name&gt; [template] [--parent path]
/run &lt;cmd&gt; &lt;args...&gt;
/upload           (файл следующим сообщением)
/model [name]     /stop
  </pre>
  <p class="muted small">
    Шел-команды ограничены allow-list (см. секцию «Shell allow-list»).
  </p>
</div>

<style>
  .tg-section h3 { margin: 0 0 6px; font-size: 14px; font-weight: 600; }
  .tg-section h4 { margin: 18px 0 6px; font-size: 12px; font-weight: 600; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.4px; }
  .tg-section p { margin: 0 0 8px; font-size: 12px; }
  .muted { color: var(--text-muted); }
  .small { font-size: 11px; }
  .row { display: flex; align-items: center; gap: 8px; margin: 6px 0; flex-wrap: wrap; }
  .row input {
    flex: 1 1 200px; min-width: 200px;
    background: var(--bg-input); color: var(--text);
    border: 1px solid var(--border); border-radius: 6px;
    padding: 6px 10px; font-family: ui-monospace, monospace; font-size: 12px;
  }
  .status-row { gap: 12px; padding: 6px 0; }
  .status-pill {
    background: var(--bg-input); border: 1px solid var(--border);
    padding: 4px 12px; border-radius: 999px;
    font-size: 12px; font-weight: 500;
  }
  .primary {
    background: var(--accent); color: #1a0d0d; border: 0;
    padding: 6px 14px; font-size: 12px; font-weight: 600;
    border-radius: 6px; cursor: pointer;
  }
  .primary:hover:not(:disabled) { opacity: 0.9; }
  .primary:disabled { opacity: 0.4; cursor: not-allowed; }
  .ghost {
    background: transparent; color: var(--text-muted);
    border: 1px solid var(--border);
    padding: 6px 12px; font-size: 12px; font-weight: 500;
    border-radius: 6px; cursor: pointer;
  }
  .ghost:hover:not(:disabled) { background: var(--bg-hover); color: var(--text); }
  .ghost:disabled { opacity: 0.4; cursor: not-allowed; }
  .banner { padding: 6px 10px; border-radius: 4px; margin: 8px 0; font-size: 12px; }
  .banner.err { background: var(--danger-soft); color: var(--danger); border: 1px solid var(--danger); }
  .banner.ok { background: var(--ok-soft); color: var(--ok); border: 1px solid var(--ok); }
  .cmd-list {
    background: var(--bg-input); border: 1px solid var(--border);
    border-radius: 6px; padding: 10px 14px;
    font-family: ui-monospace, monospace; font-size: 11px;
    color: var(--text-muted); line-height: 1.7;
    white-space: pre; overflow-x: auto;
  }
</style>
