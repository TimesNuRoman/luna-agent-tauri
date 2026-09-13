//! ARTEMIS supervisor — vision-action loop for Android automation.

use crate::services::agent::minimax_client::{MinimaxClient, MinimaxMessage, MinimaxRequest};
use crate::services::artemis::client::{AdbClient, AdbError};
use crate::services::artemis::state::{ArtemisState, TaskStatus};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_STEPS: u32 = 50;
const VISION_MODEL: &str = "MiniMax-M3";

#[derive(Debug)]
pub enum ArtemisError {
    Adb(AdbError),
    Vision(String),
    NoDevice,
    MaxStepsReached,
    TaskNotFound,
}

impl From<AdbError> for ArtemisError {
    fn from(e: AdbError) -> Self { Self::Adb(e) }
}

impl std::fmt::Display for ArtemisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adb(e) => write!(f, "ADB error: {}", e),
            Self::Vision(e) => write!(f, "Vision error: {}", e),
            Self::NoDevice => write!(f, "No Android device connected"),
            Self::MaxStepsReached => write!(f, "Max steps ({}) reached", MAX_STEPS),
            Self::TaskNotFound => write!(f, "Task not found in state"),
        }
    }
}

impl std::error::Error for ArtemisError {}

/// Result of a successful ARTEMIS task run.
#[derive(Debug)]
pub struct ArtemisResult {
    pub result: String,
    pub steps: u32,
    pub cost_usd: f64,
}

/// Runs the ARTEMIS vision-action loop for a single task.
pub async fn run_artemis_loop(
    task_id: String,
    goal: String,
    device_serial: Option<String>,
    api_key: String,
    state: Arc<ArtemisState>,
) -> ArtemisResult {
    let client = match MinimaxClient::new(api_key, VISION_MODEL.into()) {
        Ok(c) => c,
        Err(e) => {
            if let Some(task_arc) = state.get_task(&task_id).await {
                let mut g = task_arc.lock().await;
                g.status = TaskStatus::Error;
                g.error = Some(format!("Failed to create client: {}", e));
            }
            return ArtemisResult { result: "Client error".to_string(), steps: 0, cost_usd: 0.0 };
        }
    };
    
    let adb = match AdbClient::new(None, 30) {
        Ok(a) => a,
        Err(e) => {
            if let Some(task_arc) = state.get_task(&task_id).await {
                let mut g = task_arc.lock().await;
                g.status = TaskStatus::Error;
                g.error = Some(format!("Failed to create ADB client: {}", e));
            }
            return ArtemisResult { result: "ADB client error".to_string(), steps: 0, cost_usd: 0.0 };
        }
    };

    // Find a device
    let serial = match device_serial {
        Some(s) => s,
        None => {
            match adb.list_devices().await {
                Ok(devices) if !devices.is_empty() => devices[0].id.clone(),
                _ => {
                    if let Some(task_arc) = state.get_task(&task_id).await {
                        let mut g = task_arc.lock().await;
                        g.status = TaskStatus::Error;
                        g.error = Some("No Android device connected".to_string());
                    }
                    return ArtemisResult { result: "No device".to_string(), steps: 0, cost_usd: 0.0 };
                }
            }
        }
    };

    // Mark running
    if let Some(task_arc) = state.get_task(&task_id).await {
        let mut g = task_arc.lock().await;
        g.status = TaskStatus::Running;
    }

    let system_prompt = "You are an expert Android automation agent.\n\nYou will receive a screenshot description and a task goal.\nAnalyze the current state and decide the next action.\n\nRespond with exactly ONE action per message:\n- TAP <x> <y> — tap at coordinates\n- SWIPE <x1> <y1> <x2> <y2> — swipe\n- TEXT <text> — input text\n- PRESS <key> — press key (home, back, enter, volume_up, etc.)\n- DONE <summary> — task is complete\n\nExample: TAP 540 960\nExample: SWIPE 540 960 540 1200\nExample: TEXT hello world\nExample: PRESS home\nExample: DONE Opened Settings, WiFi is enabled";

    let mut messages = vec![
        MinimaxMessage::system(system_prompt),
        MinimaxMessage::user_text(format!("TASK: {}\n\nAnalyze the screen and take the next action.", goal)),
    ];

    let mut steps: u32 = 0;
    let mut total_cost: f64 = 0.0;

    loop {
        if steps >= MAX_STEPS {
            if let Some(task_arc) = state.get_task(&task_id).await {
                let mut g = task_arc.lock().await;
                g.status = TaskStatus::Error;
                g.error = Some("Max steps reached".to_string());
            }
            return ArtemisResult { result: "Max steps".to_string(), steps, cost_usd: total_cost };
        }

        // Capture screenshot
        let temp_path = PathBuf::from("/tmp/artemis_screenshot.png");
        let screenshot_result = adb.screenshot(&serial, &temp_path).await;
        let screenshot_desc = match &screenshot_result {
            Ok(()) => "Screenshot captured".to_string(),
            Err(e) => format!("Screenshot failed: {}", e),
        };

        let vision_prompt = format!(
            "{}\n\nTask: {}\n\nWhat action should you take?",
            screenshot_desc,
            goal
        );

        messages.push(MinimaxMessage::user_text(vision_prompt));

        let req = MinimaxRequest {
            model: VISION_MODEL.into(),
            messages: messages.clone(),
            tools: vec![],
            max_tokens: 256,
            temperature: None,
        };

        let response = match client.chat(req).await {
            Ok(resp) => {
                total_cost += estimate_cost(resp.input_tokens, resp.output_tokens);
                resp.content
            }
            Err(e) => {
                if let Some(task_arc) = state.get_task(&task_id).await {
                    let mut g = task_arc.lock().await;
                    g.status = TaskStatus::Error;
                    g.error = Some(format!("Vision error: {}", e));
                }
                return ArtemisResult { result: format!("Vision error: {}", e), steps, cost_usd: total_cost };
            }
        };

        tracing::info!(target: "artemis", "Step {}: {}", steps, response.trim().chars().take(100).collect::<String>());

        // Parse action
        let first_line = response.trim().lines().next().unwrap_or("").trim().to_uppercase();
        let parts: Vec<&str> = first_line.split_whitespace().collect();

        if parts.is_empty() {
            steps += 1;
            continue;
        }

        match parts[0] {
            "DONE" => {
                let summary = parts[1..].join(" ");
                if let Some(task_arc) = state.get_task(&task_id).await {
                    let mut g = task_arc.lock().await;
                    g.status = TaskStatus::Done;
                    g.result = Some(summary.clone());
                }
                return ArtemisResult { result: summary, steps, cost_usd: total_cost };
            }
            "TAP" if parts.len() >= 3 => {
                if let (Ok(x), Ok(y)) = (parts[1].parse::<i32>(), parts[2].parse::<i32>()) {
                    if let Err(e) = adb.tap(&serial, x, y).await {
                        tracing::warn!(target: "artemis", "TAP failed: {}", e);
                    }
                }
            }
            "SWIPE" if parts.len() >= 5 => {
                if let (Ok(x1), Ok(y1), Ok(x2), Ok(y2)) = (
                    parts[1].parse::<i32>(), parts[2].parse::<i32>(),
                    parts[3].parse::<i32>(), parts[4].parse::<i32>(),
                ) {
                    if let Err(e) = adb.swipe(&serial, x1, y1, x2, y2, 300).await {
                        tracing::warn!(target: "artemis", "SWIPE failed: {}", e);
                    }
                }
            }
            "TEXT" if parts.len() >= 2 => {
                let text = parts[1..].join(" ");
                if let Err(e) = adb.input_text(&serial, &text).await {
                    tracing::warn!(target: "artemis", "TEXT failed: {}", e);
                }
            }
            "PRESS" if parts.len() >= 2 => {
                let key = parts[1..].join(" ");
                if let Err(e) = adb.press_key(&serial, &key).await {
                    tracing::warn!(target: "artemis", "PRESS failed: {}", e);
                }
            }
            _ => {
                tracing::warn!(target: "artemis", "Unknown action: {}", first_line);
            }
        }

        messages.push(MinimaxMessage::Assistant { content: Some(response), tool_calls: vec![] });
        steps += 1;
    }
}

fn estimate_cost(input_tokens: u64, output_tokens: u64) -> f64 {
    const INPUT_COST: f64 = 0.00015 / 1000.0;
    const OUTPUT_COST: f64 = 0.0006 / 1000.0;
    (input_tokens as f64 * INPUT_COST) + (output_tokens as f64 * OUTPUT_COST)
}
