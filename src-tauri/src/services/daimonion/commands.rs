// =====================================================================
// Daimonion (voice-first multimodal agent) — Tauri commands (Phase D0+)
// =====================================================================
//
// Three commands back the `Daimonion.svelte` panel:
//
//   * `daimonion_transcribe`  — STT only. Frontend hands us a
//     base64-encoded audio blob (MediaRecorder output); we call
//     MiniMax ASR and return the transcript text. Used by the
//     push-to-talk path: user records audio in the browser, the
//     blob is shipped to us, we hand back the text the LLM should
//     process.
//
//   * `daimonion_chat`        — voice pipeline: text in, text +
//     audio out. Frontend hands us the transcribed user text (or
//     a text input), we run the LLM, then TTS, and return both
//     the assistant text and a base64 data-URI audio payload that
//     the frontend plays through an HTML5 `<audio>` element.
//
//   * `daimonion_capture_frame` — single screen capture via the
//     vision service. Used by the supervisor in D2 when the
//     model's reply contains a `<capture/>` marker. The vision
//     gate (D2) throttles these so the model can't ask for a
//     frame on every token.
//
// All three return `Result<T, String>` so the frontend gets a clean
// error path. We never panic on bad input — empty text, missing
// key, network 5xx — all become user-facing strings.

// `TranscribeResponse` and `SynthesizeRequest` mirror the
// frontend-side types in `daimonionClient.ts`. They are also
// returned by the corresponding Tauri commands but the Rust side
// is the only consumer right now. The `serialize` derive on the
// IPC output makes the fields "used" for serde, but a few
// helpers in this file are not (yet) called from lib.rs —
// silencing here keeps the build log readable.
#![allow(dead_code)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use super::asr::AsrClient;
use super::pipeline::{DaimonionConfig, LivePipeline, VoiceChatOutcome, VoicePipeline};
use super::tts::{self, TtsClient};
use super::types::{AudioFormat, TtsRequest, TtsResponse, VoiceChatRequest};
use super::vision::CapturedFrame;
use crate::{get_minimax_api_key, AppState};

/// Result of `daimonion_transcribe` — the recognised text plus a
/// optional detected language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResponse {
    pub text: String,
    pub language: Option<String>,
    pub confidence: Option<f32>,
}

#[tauri::command]
pub async fn daimonion_transcribe(
    audio_base64: String,
    filename_hint: Option<String>,
) -> Result<TranscribeResponse, String> {
    let key = get_minimax_api_key()?
        .ok_or_else(|| "MiniMax API key not set.".to_string())?;
    let client = AsrClient::from_env(key).map_err(|e| e.to_string())?;
    let hint = filename_hint.unwrap_or_else(|| "audio.wav".to_string());
    let transcript = client
        .transcribe_base64(&audio_base64, &hint)
        .await
        .map_err(|e| e.to_string())?;
    Ok(TranscribeResponse {
        text: transcript.text,
        language: transcript.language,
        confidence: transcript.confidence,
    })
}

#[tauri::command]
pub async fn daimonion_chat(
    request: VoiceChatRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<VoiceChatOutcome, String> {
    // Pull the system prompt from the persona registry. Falls back
    // to a one-liner if the persona isn't loaded (rare in practice
    // — `Daimonion.toml` ships in the personas dir). Use the injected
    // `state` (canonical accessor) rather than `app.state::<TaskDeps>()`
    // — both reach the same registry but `state.personas` doesn't
    // need the cast and won't go stale if the deps type ever moves.
    let system_prompt = state
        .personas
        .read_system_prompt("daimonion")
        .unwrap_or_else(|_| {
            "You are Daimonion, Luna's voice-first assistant. Reply briefly and conversationally."
                .to_string()
        });

    let key = get_minimax_api_key()?
        .ok_or_else(|| "MiniMax API key not set.".to_string())?;

    let cfg = DaimonionConfig {
        system_prompt,
        model: request
            .model
            .clone()
            .unwrap_or_else(|| "MiniMax-M3".to_string()),
        voice_id: request
            .tts_voice_id
            .clone()
            .unwrap_or_else(|| tts::DEFAULT_VOICE_ID.to_string()),
        format: request.tts_format.unwrap_or(AudioFormat::Mp3),
        max_tokens: 256,
        detect_capture_marker: true,
    };

    let asr = AsrClient::from_env(key.clone()).map_err(|e| e.to_string())?;
    let tts = TtsClient::from_env(key).map_err(|e| e.to_string())?;
    let pipeline = LivePipeline::new(cfg, asr, tts).map_err(|e| e.to_string())?;

    // Build a dynamic dispatch so tests can swap in a mock. In D0
    // we always use the live path; D1+ will route through Arc<dyn
    // VoicePipeline> for the VAD loop. For now, monomorphise.
    let outcome = super::pipeline::run_voice_chat(Arc::new(pipeline), request)
        .await
        .map_err(|e| e.to_string())?;

    // Fire-and-forget UI event so the chat log gets a system note
    // when Daimonion speaks. The frontend listens for this and can
    // scroll / play a chime.
    let _ = app.emit("daimonion-spoke", &outcome);

    Ok(outcome)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureFrameRequest {
    /// `None` → primary monitor.
    pub monitor_id: Option<u32>,
    /// Cap the JPEG width. Default 1280.
    pub max_width: Option<u32>,
}

#[tauri::command]
pub async fn daimonion_capture_frame(
    request: CaptureFrameRequest,
    state: State<'_, AppState>,
) -> Result<Option<CapturedFrame>, String> {
    // The vision gate is a sync lock; we briefly hold it to do the
    // capture. xcap + JPEG encoding are CPU-bound but fast (~30 ms
    // for a 1280px-wide frame on a modern CPU), so the lock is
    // uncontended in practice.
    let res = {
        let mut gate = state.daimonion_vision.lock();
        gate.try_capture(request.monitor_id, request.max_width)
            .map_err(|e| e.to_string())?
    };
    Ok(res)
}

/// TTS-only command — synthesise arbitrary text to a data-URI
/// without running the LLM. Useful for UI chimes, "Daimonion is
/// thinking..." fillers, and integration tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeRequest {
    pub text: String,
    pub voice_id: Option<String>,
    pub format: Option<AudioFormat>,
}

#[tauri::command]
pub async fn daimonion_synthesize(
    request: SynthesizeRequest,
) -> Result<TtsResponse, String> {
    let key = get_minimax_api_key()?
        .ok_or_else(|| "MiniMax API key not set.".to_string())?;
    let client = TtsClient::from_env(key).map_err(|e| e.to_string())?;
    let tts_req = TtsRequest {
        text: request.text,
        voice_id: request.voice_id,
        format: request.format,
        model: None,
        speed: None,
    };
    client.synthesize(&tts_req).await.map_err(|e| e.to_string())
}
