<script lang="ts">
  import { acceptConsent } from './lib/videomode-store';

  export let onAccept: () => void = () => {};
  export let onDecline: () => void = () => {};

  function handleAccept() {
    acceptConsent();
    onAccept();
  }
  function handleDecline() {
    onDecline();
  }

  // F17: Esc is promised in the consent copy ("Нажмите Esc или кнопку Stop").
  // Bind it on the window so it works regardless of which button has focus.
  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      handleDecline();
    }
  }
</script>

<svelte:window on:keydown={onWindowKey} />

<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="consent-title">
  <div class="modal">
    <h2 id="consent-title">🎥 Video Mode — согласие на захват экрана</h2>
    <p>
      Luna Agent будет видеть <strong>всё, что сейчас на экране</strong> выбранного
      монитора — окна, игры, чаты, уведомления.
    </p>
    <ul>
      <li>Кадры <strong>не сохраняются на диск</strong> и не покидают память, кроме как
        при отправке в MiniMax для анализа.</li>
      <li>Чтобы получить подсказку, кадр отправляется в vision-модель MiniMax. Содержимое
        экрана увидит внешний сервис.</li>
      <li>В строке статуса будет постоянный визуальный индикатор «Luna смотрит экран».</li>
      <li>Нажмите <kbd>Esc</kbd> или кнопку <strong>Stop</strong> в любой момент — захват
        прекратится за &lt; 1 сек.</li>
    </ul>
    <p class="warn">
      Не используйте Video Mode на экранах с чувствительными данными
      (банки, пароли, медицинские записи), пока не доверяете провайдеру MiniMax.
    </p>
    <div class="actions">
      <button class="ghost" on:click={handleDecline}>Отмена</button>
      <button class="primary" on:click={handleAccept}>Я понимаю, включить</button>
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
    background: var(--bg-elevated);
    color: var(--text);
    border-radius: 12px;
    padding: 24px 28px;
    max-width: 540px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    border: 1px solid var(--border);
  }
  h2 {
    margin: 0 0 12px 0;
    font-size: 18px;
  }
  p { margin: 8px 0; line-height: 1.45; }
  ul { margin: 8px 0 8px 18px; padding: 0; line-height: 1.45; }
  li { margin: 4px 0; }
  kbd {
    background: var(--border);
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    padding: 1px 6px;
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  .warn {
    color: var(--warn);
    font-size: 13px;
    background: var(--warn-soft);
    border-left: 3px solid var(--warn);
    padding: 8px 10px;
    border-radius: 4px;
  }
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
  button.primary {
    background: var(--danger);
    color: var(--text-inverse);
    border-color: var(--danger);
  }
  button.primary:hover {
    background: var(--danger-strong);
  }
  button.ghost {
    background: transparent;
    color: var(--text);
    border-color: var(--border);
  }
  button.ghost:hover {
    background: var(--bg-hover);
  }

</style>
