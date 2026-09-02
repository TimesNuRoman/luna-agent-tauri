// src/lib/daimonionClient.ts
// Typed IPC wrappers for the Daimonion voice assistant (Phase D0+).
//
// All commands go through `__TAURI__.core.invoke`. The audio payload
// is shipped as a base64 data-URI to keep the IPC schema small and
// the VAD/audio-IO loop on the backend free of WAV-header parsing.

import { core, events } from './tauri';

// ---------- Types (mirror services::daimonion::types in Rust) ----------

export type AudioFormat = 'mp3' | 'wav' | 'pcm';

export type VadConfig = {
  energy_threshold: number; // 0.0–1.0
  start_hold_frames: number; // frames above threshold before SpeechStarted
  end_hold_frames: number; // frames below threshold before SpeechPaused
  frame_ms: number; // frame size in ms
};

export type VoiceChatRequest = {
  user_text: string;
  conversation_id?: string | null;
  model?: string | null;
  include_vision?: boolean | null;
  tts_voice_id?: string | null;
  tts_format?: AudioFormat | null;
};

export type TtsResponse = {
  audio_bytes: number[]; // Vec<u8> serialised as a number array
  format: AudioFormat;
  elapsed_ms: number;
  audio_base64: string; // convenience: base64-encoded data
};

export type VoiceChatOutcome = {
  assistant_text: string;
  audio: TtsResponse;
  total_ms: number;
  llm_ms: number;
  tts_ms: number;
};

export type TranscribeResponse = {
  text: string;
  language?: string | null;
  confidence?: number | null;
};

export type CaptureFrameRequest = {
  monitor_id?: number | null;
  max_width?: number | null;
};

export type CapturedFrame = {
  base64: string;
  width: number;
  height: number;
  bytes: number;
};

export type SynthesizeRequest = {
  text: string;
  voice_id?: string | null;
  format?: AudioFormat | null;
};

// ---------- IPC wrappers ----------

export async function daimonionTranscribe(
  audioBase64: string,
  filenameHint = 'audio.wav',
): Promise<TranscribeResponse> {
  return core().invoke<TranscribeResponse>('daimonion_transcribe', {
    audioBase64,
    filenameHint,
  });
}

export async function daimonionChat(req: VoiceChatRequest): Promise<VoiceChatOutcome> {
  return core().invoke<VoiceChatOutcome>('daimonion_chat', { request: req });
}

export async function daimonionCaptureFrame(
  req: CaptureFrameRequest = {},
): Promise<CapturedFrame | null> {
  return core().invoke<CapturedFrame | null>('daimonion_capture_frame', { request: req });
}

export async function daimonionSynthesize(req: SynthesizeRequest): Promise<TtsResponse> {
  return core().invoke<TtsResponse>('daimonion_synthesize', { request: req });
}

// ---------- Live events ----------

/** Subscribe to "daimonion-spoke" events. The payload is the
 *  `VoiceChatOutcome` from the most recent pipeline run. Returns
 *  an unsubscribe function. */
export async function onDaimonionSpoke(
  handler: (outcome: VoiceChatOutcome) => void,
): Promise<() => void> {
  const ev = events();
  return ev.listen<VoiceChatOutcome>('daimonion-spoke', (e) => {
    handler(e.payload);
  });
}

// ---------- Helpers ----------

/** Build a `data:audio/mpeg;base64,...` URI from a TtsResponse for
 *  direct `<audio src=...>` playback. */
export function audioDataUri(audio: TtsResponse): string {
  return `data:audio/${audio.format === 'mp3' ? 'mpeg' : audio.format};base64,${audio.audio_base64}`;
}

/** Build a `data:audio/wav;base64,...` URI from a MediaRecorder
 *  Blob. Used for the push-to-talk path. */
export async function blobToBase64(blob: Blob): Promise<string> {
  const buf = await blob.arrayBuffer();
  let binary = '';
  const bytes = new Uint8Array(buf);
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(
      null,
      Array.from(bytes.subarray(i, i + chunk)),
    );
  }
  return btoa(binary);
}
