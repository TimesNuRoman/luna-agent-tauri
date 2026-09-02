<script lang="ts">
  // Daimonion.svelte — voice-first multimodal panel (Phase D0+).
  //
  // UX (D0):
  //   * "Push to talk" — hold Space (or click the mic button) to record
  //     audio via MediaRecorder. On release: transcribe (STT) → chat
  //     (LLM + TTS) → auto-play the assistant's reply.
  //   * Text input fallback — type a message, get text + audio reply.
  //
  // UX (D1+):
  //   * VAD-driven always-on listening (no button needed).
  //   * Vision indicator when Daimonion asked for a screen frame.
  //
  // The component owns the recorder lifecycle and the audio element.
  // All heavy lifting is on the backend; this file is just glue.

  import { onDestroy, onMount } from 'svelte';
  import {
    audioDataUri,
    blobToBase64,
    daimonionCaptureFrame,
    daimonionChat,
    daimonionSynthesize,
    daimonionTranscribe,
    onDaimonionSpoke,
    type VoiceChatOutcome,
  } from './lib/daimonionClient';

  type Status = 'idle' | 'recording' | 'thinking' | 'speaking' | 'error';

  let status: Status = 'idle';
  let errorMsg: string = '';
  let lastOutcome: VoiceChatOutcome | null = null;
  let textInput: string = '';
  let history: { role: 'user' | 'assistant'; text: string }[] = [];
  let audioEl: HTMLAudioElement | null = null;
  let mediaRecorder: MediaRecorder | null = null;
  let recorderChunks: BlobPart[] = [];
  let isPushToTalking = false;
  let unsubscribeSpoke: (() => void) | null = null;

  // --- Push-to-talk lifecycle ---

  async function startRecording(): Promise<void> {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { sampleRate: 16000, channelCount: 1, echoCancellation: true },
      });
      // Prefer opus for small payloads; webm is the browser default.
      const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
        ? 'audio/webm;codecs=opus'
        : 'audio/webm';
      mediaRecorder = new MediaRecorder(stream, { mimeType });
      recorderChunks = [];
      mediaRecorder.ondataavailable = (e) => {
        if (e.data.size > 0) recorderChunks.push(e.data);
      };
      mediaRecorder.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        void handleRecordingComplete();
      };
      mediaRecorder.start();
      status = 'recording';
      errorMsg = '';
    } catch (e) {
      status = 'error';
      errorMsg = e instanceof Error ? e.message : String(e);
    }
  }

  function stopRecording(): void {
    if (mediaRecorder && mediaRecorder.state === 'recording') {
      mediaRecorder.stop();
    }
  }

  async function handleRecordingComplete(): Promise<void> {
    const blob = new Blob(recorderChunks, { type: mediaRecorder?.mimeType ?? 'audio/webm' });
    try {
      status = 'thinking';
      const b64 = await blobToBase64(blob);
      const transcript = await daimonionTranscribe(b64, 'audio.webm');
      await runChat(transcript.text);
    } catch (e) {
      status = 'error';
      errorMsg = e instanceof Error ? e.message : String(e);
    }
  }

  // --- Text input path ---

  async function sendText(): Promise<void> {
    const text = textInput.trim();
    if (!text) return;
    textInput = '';
    await runChat(text);
  }

  async function runChat(userText: string): Promise<void> {
    history = [...history, { role: 'user', text: userText }];
    try {
      status = 'thinking';
      const outcome = await daimonionChat({ user_text: userText });
      lastOutcome = outcome;
      history = [...history, { role: 'assistant', text: outcome.assistant_text }];
      await playOutcome(outcome);
    } catch (e) {
      status = 'error';
      errorMsg = e instanceof Error ? e.message : String(e);
    }
  }

  async function playOutcome(outcome: VoiceChatOutcome): Promise<void> {
    if (!audioEl) return;
    audioEl.src = audioDataUri(outcome.audio);
    status = 'speaking';
    try {
      await audioEl.play();
    } catch (e) {
      // Autoplay can be blocked; surface the error but keep the
      // text reply visible. The user can click the audio element
      // to play it manually.
      errorMsg = `audio playback blocked: ${e instanceof Error ? e.message : String(e)}`;
      status = 'idle';
    }
  }

  // --- Space-bar push-to-talk (also handled by mouse / touch) ---

  function onKeyDown(e: KeyboardEvent): void {
    if (e.code === 'Space' && !isPushToTalking && document.activeElement?.tagName !== 'INPUT' && document.activeElement?.tagName !== 'TEXTAREA') {
      e.preventDefault();
      isPushToTalking = true;
      void startRecording();
    }
  }
  function onKeyUp(e: KeyboardEvent): void {
    if (e.code === 'Space' && isPushToTalking) {
      e.preventDefault();
      isPushToTalking = false;
      stopRecording();
    }
  }

  // --- Manual capture (D2 preview) ---

  async function captureNow(): Promise<void> {
    try {
      const frame = await daimonionCaptureFrame({});
      errorMsg = frame
        ? `captured ${frame.width}x${frame.height} (${(frame.bytes / 1024).toFixed(1)} KB)`
        : 'throttled';
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    }
  }

  // --- TTS chime (TTS-only path) ---

  async function playChime(): Promise<void> {
    try {
      const tts = await daimonionSynthesize({ text: 'Я здесь.', format: 'mp3' });
      if (audioEl) {
        audioEl.src = audioDataUri(tts);
        await audioEl.play();
      }
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    }
  }

  // --- Lifecycle ---

  onMount(() => {
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    unsubscribeSpoke = onDaimonionSpoke((outcome) => {
      lastOutcome = outcome;
    });
  });

  onDestroy(() => {
    window.removeEventListener('keydown', onKeyDown);
    window.removeEventListener('keyup', onKeyUp);
    if (unsubscribeSpoke) unsubscribeSpoke();
    if (mediaRecorder && mediaRecorder.state === 'recording') {
      mediaRecorder.stop();
    }
  });
</script>

<div class="daimonion-panel">
  <header>
    <h2>Daimonion <span class="subtitle">— внутренний голос</span></h2>
    <div class="status status-{status}">
      {#if status === 'idle'}готов{/if}
      {#if status === 'recording'}● слушаю…{/if}
      {#if status === 'thinking'}…думаю…{/if}
      {#if status === 'speaking'}▶ говорю{/if}
      {#if status === 'error'}⚠ {errorMsg}{/if}
    </div>
  </header>

  <div class="controls">
    <button
      type="button"
      class="mic"
      on:mousedown={startRecording}
      on:mouseup={stopRecording}
      on:mouseleave={() => isPushToTalking && stopRecording()}
      on:touchstart|preventDefault={startRecording}
      on:touchend|preventDefault={stopRecording}
      disabled={status === 'thinking' || status === 'speaking'}
    >
      {status === 'recording' ? '● отпусти' : '🎙 push-to-talk (Space)'}
    </button>
    <button type="button" class="ghost" on:click={captureNow} title="Capture one screen frame (D2 preview)">📸 capture</button>
    <button type="button" class="ghost" on:click={playChime} title="TTS smoke test">🔔 chime</button>
  </div>

  <form class="text-input" on:submit|preventDefault={sendText}>
    <input
      type="text"
      placeholder="…или напиши здесь"
      bind:value={textInput}
      disabled={status === 'thinking' || status === 'speaking'}
    />
    <button type="submit" disabled={!textInput.trim() || status === 'thinking' || status === 'speaking'}>→</button>
  </form>

  <div class="history" aria-live="polite">
    {#each history.slice(-12) as turn, i (i)}
      <div class="turn turn-{turn.role}">
        <span class="role">{turn.role === 'user' ? 'ты' : 'Daimonion'}:</span>
        <span class="text">{turn.text}</span>
      </div>
    {/each}
  </div>

  <audio bind:this={audioEl} preload="auto" />

  {#if lastOutcome}
    <footer>
      <span>last: {lastOutcome.total_ms}ms (llm {lastOutcome.llm_ms}ms, tts {lastOutcome.tts_ms}ms)</span>
    </footer>
  {/if}
</div>

<style>
  .daimonion-panel {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    height: 100%;
    box-sizing: border-box;
    font-family: system-ui, sans-serif;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }
  h2 {
    margin: 0;
    font-size: 18px;
  }
  .subtitle {
    font-weight: 400;
    color: #888;
    font-size: 13px;
  }
  .status {
    font-size: 12px;
    padding: 2px 8px;
    border-radius: 12px;
    background: #f0f0f0;
  }
  .status-recording { background: #fee; color: #c33; }
  .status-thinking  { background: #ffe; color: #886; }
  .status-speaking  { background: #efe; color: #383; }
  .status-error     { background: #fdd; color: #800; }
  .controls { display: flex; gap: 8px; }
  button {
    border: 1px solid #ccc;
    background: #fff;
    padding: 6px 12px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .mic { flex: 1; }
  .ghost { background: #f6f6f6; }
  .text-input {
    display: flex;
    gap: 6px;
  }
  .text-input input {
    flex: 1;
    padding: 6px 10px;
    border: 1px solid #ccc;
    border-radius: 6px;
  }
  .history {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    background: #fafafa;
    border-radius: 6px;
    min-height: 100px;
  }
  .turn { display: flex; gap: 6px; font-size: 13px; }
  .turn-user .role { color: #06c; }
  .turn-assistant .role { color: #c60; }
  .role { font-weight: 600; min-width: 64px; }
  footer { font-size: 11px; color: #888; }
</style>
