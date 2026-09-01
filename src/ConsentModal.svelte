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
</script>

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
    background: #1c1f26;
    color: #e6e8eb;
    border-radius: 12px;
    padding: 24px 28px;
    max-width: 540px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    border: 1px solid #2c313a;
  }
  h2 {
    margin: 0 0 12px 0;
    font-size: 18px;
  }
  p { margin: 8px 0; line-height: 1.45; }
  ul { margin: 8px 0 8px 18px; padding: 0; line-height: 1.45; }
  li { margin: 4px 0; }
  kbd {
    background: #2c313a;
    border: 1px solid #3a414b;
    border-radius: 4px;
    padding: 1px 6px;
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  .warn {
    color: #f5b56b;
    font-size: 13px;
    background: #2a2018;
    border-left: 3px solid #f5b56b;
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
    background: #c34c4c;
    color: white;
    border-color: #c34c4c;
  }
  button.primary:hover {
    background: #d75a5a;
  }
  button.ghost {
    background: transparent;
    color: #cfd3da;
    border-color: #3a414b;
  }
  button.ghost:hover {
    background: #252932;
  }
</style>
