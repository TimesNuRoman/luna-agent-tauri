<script lang="ts">
  // Overlay.svelte — compact Daimonion indicator (Phase D3).
  //
  // Renders into the `daimonion-overlay` Tauri window defined in
  // `tauri.conf.json`. The window is transparent, always-on-top,
  // and has no taskbar entry. The user pins it anywhere on the
  // desktop and it stays out of the way until Daimonion has
  // something to say.
  //
  // States (D0 base + D1/D2 hooks):
  //   * `idle`       — translucent pill, "Daimonion"
  //   * `listening`  — pulsing dot, "● слушаю…"
  //   * `thinking`   — spinner, "…думаю…"
  //   * `speaking`   — speaking indicator + playhead
  //   * `error`      — red dot, error message
  //
  // Interaction is intentionally minimal: a single click anywhere
  // on the overlay brings the main Luna window to the foreground.
  // Right-click shows a tiny menu (close, open main window,
  // settings) — but that ships in D3.1, not D0.

  import { onDestroy, onMount } from 'svelte';

  type Status = 'idle' | 'listening' | 'thinking' | 'speaking' | 'error';

  let status: Status = 'idle';
  let message: string = 'Daimonion';
  let pulse: number = 0;
  let raf: number | null = null;
  let unsubscribe: (() => void) | null = null;
  let lastError: string = '';

  function tick() {
    pulse = (pulse + 0.05) % (Math.PI * 2);
    raf = requestAnimationFrame(tick);
  }

  async function onClick() {
    try {
      // Bring the main Luna window to the foreground.
      const tauri = (window as any).__TAURI__;
      if (tauri?.window?.getCurrent) {
        // We're in the overlay window. Use the main window API
        // (Tauri 2 exposes `WebviewWindow::getByLabel`).
        const { WebviewWindow } = await import('@tauri-apps/api/window').catch(() => ({} as any));
        if (WebviewWindow?.getByLabel) {
          const main = WebviewWindow.getByLabel('main');
          if (main) {
            await main.show();
            await main.setFocus();
          }
        }
      }
    } catch (e) {
      // The dev experience inside `npm run dev` (without Tauri)
      // is "no-op, that's fine"; we only care about the
      // production runtime here.
      console.warn('[Overlay] could not focus main window:', e);
    }
  }

  onMount(() => {
    raf = requestAnimationFrame(tick);

    // Listen for daimonion-spoke events that get forwarded from
    // the main window. The events are emitted by the daemon side
    // of the pipeline; the overlay reacts by switching to the
    // "speaking" state and animating a small playhead.
    const tauri = (window as any).__TAURI__;
    if (tauri?.event?.listen) {
      tauri.event
        .listen('daimonion-overlay-state', (e: any) => {
          const payload = e?.payload ?? {};
          if (payload.status) status = payload.status;
          if (payload.message) message = payload.message;
          if (payload.error) lastError = payload.error;
        })
        .then((unsub: () => void) => {
          unsubscribe = unsub;
        })
        .catch(() => {
          // Outside the Tauri webview — fine, the overlay just
          // sits idle.
        });
    }
  });

  onDestroy(() => {
    if (raf !== null) cancelAnimationFrame(raf);
    if (unsubscribe) unsubscribe();
  });
</script>

<button class="overlay overlay-{status}" on:click={onClick} title="click to focus Luna">
  <span class="dot" style:transform="scale({0.7 + 0.3 * Math.abs(Math.sin(pulse))})"></span>
  <span class="msg">
    {#if status === 'listening'}<b>●</b> слушаю…{/if}
    {#if status === 'thinking'}…думаю…{/if}
    {#if status === 'speaking'}▶ говорю{/if}
    {#if status === 'error'}⚠ {lastError || 'error'}{/if}
    {#if status === 'idle'}{message}{/if}
  </span>
</button>

<style>
  :global(html), :global(body) {
    background: transparent !important;
    margin: 0;
    padding: 0;
    overflow: hidden;
  }
  .overlay {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: rgba(20, 20, 20, 0.78);
    color: #fff;
    border: 1px solid rgba(255, 255, 255, 0.12);
    font-family: system-ui, sans-serif;
    font-size: 12px;
    cursor: pointer;
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    user-select: none;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #aaa;
    transition: transform 80ms linear;
  }
  .overlay-idle      .dot { background: #888; }
  .overlay-listening .dot { background: #f44; }
  .overlay-thinking  .dot { background: #fb0; }
  .overlay-speaking  .dot { background: #4f4; }
  .overlay-error     .dot { background: #f33; }
  .msg { line-height: 1; }
</style>
