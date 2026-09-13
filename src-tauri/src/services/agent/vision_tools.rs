//! Phase 1: Visual Grounding — screen capture + vision analysis for the agent.
//!
//! This module adds a `vision_grounding` tool that the supervisor can call
//! to capture the current screen and send it to the MiniMax vision model
//! for grounding. This enables the agent to "see" the current screen state
//! and make decisions based on visual context.
//!
//! ## Usage
//! The supervisor calls `vision_grounding` with a question/prompt, and
//! this returns the vision model's analysis of the current screen.
//!
//! ## Implementation
//! - Uses `CaptureState` from `services::vision` for screen capture
//! - Calls `call_minimax_vision` for vision analysis
//! - Integrates with the goal-based monitoring from Video Mode

use crate::services::vision::{self, CaptureState, CaptureOptions, SingleFrame};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Request payload for vision grounding.
#[derive(Debug, Deserialize)]
pub struct VisionGroundingRequest {
    /// The question/prompt to ask about the screen.
    pub query: String,
    /// Optional monitor ID (defaults to primary).
    pub monitor_id: Option<u32>,
    /// Optional max response tokens.
    pub max_tokens: Option<u32>,
}

/// Response from vision grounding.
#[derive(Debug, Clone, Serialize)]
pub struct VisionGroundingResponse {
    /// The vision model's analysis.
    pub analysis: String,
    /// Frame metadata.
    pub frame: FrameInfo,
    /// Whether the capture was successful.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub monitor_id: u32,
    pub seq: u64,
    pub t_ms: u128,
}

/// Execute a vision grounding request.
/// 
/// Captures the current screen and sends it to the vision model for analysis.
/// The agent uses this to ground its responses in the current visual context.
pub async fn execute_vision_grounding(
    request: &VisionGroundingRequest,
    capture_state: Option<&Arc<CaptureState>>,
) -> VisionGroundingResponse {
    // Try to get the latest frame from the capture loop first
    if let Some(state) = capture_state {
        if let Some(frame) = vision::peek_latest_frame(state) {
            let analysis = match call_vision_for_grounding(
                &frame,
                &request.query,
                request.max_tokens,
            ).await {
                Ok(a) => a,
                Err(e) => format!("Vision analysis failed: {}", e),
            };
            
            return VisionGroundingResponse {
                success: true,
                analysis,
                frame: FrameInfo {
                    width: frame.width,
                    height: frame.height,
                    monitor_id: frame.monitor_id,
                    seq: frame.seq,
                    t_ms: frame.t_ms,
                },
                error: None,
            };
        }
    }
    
    // Fall back to a single-shot capture if no capture loop is running
    let opts = CaptureOptions {
        monitor_id: request.monitor_id,
        fps: None,
        max_width: Some(1280), // Default to reasonable size for grounding
    };
    
    match vision::capture_single_frame(opts) {
        Ok(frame) => {
            let analysis = match call_vision_for_grounding(
                &frame,
                &request.query,
                request.max_tokens,
            ).await {
                Ok(a) => a,
                Err(e) => format!("Vision analysis failed: {}", e),
            };
            
            VisionGroundingResponse {
                success: true,
                analysis,
                frame: FrameInfo {
                    width: frame.width,
                    height: frame.height,
                    monitor_id: frame.monitor_id,
                    seq: frame.seq,
                    t_ms: frame.t_ms,
                },
                error: None,
            }
        }
        Err(e) => VisionGroundingResponse {
            success: false,
            analysis: String::new(),
            frame: FrameInfo {
                width: 0,
                height: 0,
                monitor_id: 0,
                seq: 0,
                t_ms: 0,
            },
            error: Some(e),
        },
    }
}

/// Internal helper to call MiniMax vision for grounding analysis.
async fn call_vision_for_grounding(
    frame: &SingleFrame,
    query: &str,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    let system = "Ты визуальный ассистент. Проанализируй изображение экрана и \
        ответь на вопрос пользователя. Отвечай кратко и по существу. \
        Если на экране нет релевантной информации, так и скажи.";
    
    let user_text = format!(
        "Вопрос: {}\n\nПроанализируй, что показано на экране (монитор {}, {}x{}).",
        query,
        frame.monitor_id,
        frame.width,
        frame.height
    );
    
    let req = vision::VisionRequest {
        system: system.to_string(),
        user_text,
        image_base64: frame.base64.clone(),
        max_tokens: max_tokens.or(Some(300)),
    };
    
    vision::call_minimax_vision(req).await
}

/// Build the vision grounding tool definition for the supervisor.
pub fn vision_grounding_tool() -> super::minimax_client::MinimaxTool {
    super::minimax_client::MinimaxTool {
        kind: "function".into(),
        function: super::minimax_client::MinimaxToolFunction {
            name: "vision_grounding".into(),
            description: "Capture the current screen and analyze it with vision AI. \
                Use this when you need to see what's on the user's screen to answer \
                a question or make a decision. Returns the vision model's analysis \
                of the screen content.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { 
                        "type": "string", 
                        "description": "The question or analysis request about the screen content."
                    },
                    "monitor_id": { 
                        "type": "integer", 
                        "description": "Optional monitor ID (0 = primary).",
                        "default": 0
                    },
                    "max_tokens": { 
                        "type": "integer", 
                        "description": "Maximum response length.",
                        "default": 300
                    }
                },
                "required": ["query"]
            }),
        },
    }
}
