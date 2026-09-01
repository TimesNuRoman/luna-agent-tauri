<script lang="ts">
  import { onMount } from 'svelte';
  import { getApiKey, setApiKey } from './lib/tauri';
  import { minimaxKeyStatus, refreshMinimaxKeyStatus } from './lib/videomode-store';

  export let onClose: () => void = () => {};

  let key = '';
  let showKey = false;
  let saving = false;
  let error: string | null = null;

  const PROVIDER = 'minimax';

  onMount(async () => {
    try {
      const existing = await getApiKey(PROVIDER);
      if (existing) key = existing;
    } catch {
      /* keyring might not be available; user will type fresh */
    }
  });

  async function save() {
    const trimmed = key.trim();
    if (!trimmed) {
      error = 'Ключ не может быть пустым';
      return;
    }
    saving = true;
    error = null;
    try {
      await setApiKey(PROVIDER, trimmed);
      await refreshMinimaxKeyStatus();
      onClose();
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  function clearKey() {
    if (!confirm('Удалить сохранённый MiniMax API key из системного keyring?')) return;
    // We don't have a delete command; the user can overwrite with empty? Better:
    // just set to empty — the check in call_minimax_vision will reject it.
    // But to actually clear, we'd need a `delete_api_key` command. For now, mark as missing.
    // Hack: save an obviously invalid key so the next call fails fast. Users can re-enter.
    error =
      'Чтобы удалить ключ, перезапишите его новым (или удалите из Credential Manager вручную: ' +
      'ищите запись "luna-agent" / "minimax").';
  }
</script>

<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="apikey-title">
  <div class="modal">
    <h2 id="apikey-title">🔑 API ключи</h2>
    <p>
      MiniMax API ключ хранится в системном keyring (Windows Credential Manager /
      macOS Keychain / Linux Secret Service) и используется для vision-вызовов
      из Video Mode.
    </p>

    <label class="key-row">
      <span>MiniMax API key</span>
      <div class="input-row">
        {#if showKey}
          <input
            type="text"
            bind:value={key}
            placeholder="eyJhbGciOi..."
            autocomplete="off"
            spellcheck="false" />
        {:else}
          <input
            type="password"
            bind:value={key}
            placeholder="eyJhbGciOi..."
            autocomplete="off"
            spellcheck="false" />
        {/if}
        <button
          type="button"
          class="ghost small"
          on:click={() => (showKey = !showKey)}
          title={showKey ? 'Скрыть' : 'Показать'}>
          {showKey ? '🙈' : '👁'}
        </button>
      </div>
    </label>

    <p class="status">
      Статус: <strong class:ok={$minimaxKeyStatus === 'set'} class:bad={$minimaxKeyStatus === 'missing'}>
        {$minimaxKeyStatus === 'set'
          ? 'сохранён'
          : $minimaxKeyStatus === 'missing'
            ? 'не задан'
            : 'проверяется…'}
      </strong>
    </p>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <p class="hint">
      Где взять ключ: <a href="https://api.minimax.chat" target="_blank" rel="noopener">api.minimax.chat</a>
      → раздел API Keys. Нужен доступ к vision-модели MiniMax-M3.
    </p>

    <div class="actions">
      <button class="ghost" on:click={onClose}>Отмена</button>
      <button class="ghost" on:click={clearKey} title="Инструкция по удалению">
        Удалить
      </button>
      <button class="primary" on:click={save} disabled={saving}>
        {saving ? 'Сохраняю…' : 'Сохранить'}
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: #1c1f26;
    color: #e6e8eb;
    border-radius: 12px;
    padding: 24px 28px;
    max-width: 540px;
    width: 100%;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    border: 1px solid #2c313a;
  }
  h2 { margin: 0 0 12px 0; font-size: 18px; }
  p { margin: 8px 0; line-height: 1.45; }
  a { color: #6ea8ff; }

  .key-row { display: flex; flex-direction: column; gap: 6px; margin: 12px 0 4px 0; }
  .key-row > span { font-size: 12px; color: #b6bcc7; }
  .input-row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .input-row input {
    flex: 1;
    background: #0f1217;
    color: #e6e8eb;
    border: 1px solid #2c313a;
    border-radius: 4px;
    padding: 8px 10px;
    font-family: ui-monospace, monospace;
    font-size: 13px;
  }
  .input-row input:focus { outline: 1px solid #4a78c8; }
  button.small {
    padding: 6px 10px;
    font-size: 14px;
  }

  .status { font-size: 13px; color: #b6bcc7; }
  .status .ok { color: #6dd18f; }
  .status .bad { color: #f09090; }

  .error {
    color: #f09090;
    background: #2a1818;
    border: 1px solid #8a3a3a;
    border-radius: 4px;
    padding: 8px 10px;
    font-size: 12px;
  }
  .hint { color: #6c7280; font-size: 12px; }

  .actions {
    display: flex;
    gap: 10px;
    justify-content: flex-end;
    margin-top: 18px;
  }
  button {
    padding: 8px 14px;
    border-radius: 6px;
    border: 1px solid transparent;
    cursor: pointer;
    font-size: 14px;
  }
  button.primary { background: #4a78c8; color: white; border-color: #4a78c8; }
  button.primary:hover:not(:disabled) { background: #5a88d8; }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
  button.ghost { background: transparent; color: #cfd3da; border-color: #3a414b; }
  button.ghost:hover { background: #252932; }
</style>
