//! ARTEMIS state management (Phase T0+).
//!
//! Manages Android device connections, task registry, and session state
//! for the ARTEMIS Android automation agent.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// ARTEMIS task status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Error,
    Cancelled,
}

/// A single ARTEMIS automation task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtemisTask {
    pub id: String,
    pub prompt: String,
    pub device_id: Option<String>,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub cost_usd: f64,
}

impl ArtemisTask {
    pub fn new(id: String, prompt: String, device_id: Option<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            id,
            prompt,
            device_id,
            status: TaskStatus::Pending,
            result: None,
            error: None,
            created_at_ms: now,
            finished_at_ms: None,
            cost_usd: 0.0,
        }
    }
}

/// Connected Android device information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub model: String,
    pub manufacturer: String,
    pub android_version: String,
    pub screen_width: u32,
    pub screen_height: u32,
    pub is_emulator: bool,
    pub status: DeviceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    Connected,
    Disconnected,
    Busy,
    Unauthorized,
}

/// ARTEMIS agent state, held in `AppState`.
#[derive(Default)]
pub struct ArtemisState {
    /// Device registry: device_id -> DeviceInfo
    pub devices: Mutex<HashMap<String, DeviceInfo>>,
    /// Task registry: task_id -> ArtemisTask (using tokio Mutex for async compatibility)
    pub tasks: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<ArtemisTask>>>>,
    /// Approval queue for high-risk actions
    pub pending_approvals: Arc<Mutex<Vec<PendingApproval>>>,
    /// Cost tracking
    pub total_cost_usd: Mutex<f64>,
    /// ADB path override (None = auto-detect)
    pub adb_path: Mutex<Option<String>>,
}

/// A pending approval request for a high-risk action.
#[derive(Debug)]
pub struct PendingApproval {
    pub task_id: String,
    pub action: String,
    pub description: String,
    pub created_at_ms: u64,
    #[allow(dead_code)]
    pub response_tx: Option<tokio::sync::oneshot::Sender<bool>>,
}

impl Default for PendingApproval {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            action: String::new(),
            description: String::new(),
            created_at_ms: 0,
            response_tx: None,
        }
    }
}

impl ArtemisState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a connected device.
    pub fn register_device(&self, device: DeviceInfo) {
        self.devices.lock().insert(device.id.clone(), device);
    }

    /// Unregister a device.
    pub fn unregister_device(&self, device_id: &str) {
        self.devices.lock().remove(device_id);
    }

    /// Get all connected devices.
    pub fn get_devices(&self) -> Vec<DeviceInfo> {
        self.devices.lock().values().cloned().collect()
    }

    /// Register a new task.
    pub async fn register_task(self: &Arc<Self>, task: ArtemisTask) -> Arc<tokio::sync::Mutex<ArtemisTask>> {
        let arc = Arc::new(tokio::sync::Mutex::new(task));
        let mut tasks = self.tasks.lock().await;
        tasks.insert(arc.lock().await.id.clone(), arc.clone());
        arc
    }

    /// Get a task by ID.
    pub async fn get_task(self: &Arc<Self>, task_id: &str) -> Option<Arc<tokio::sync::Mutex<ArtemisTask>>> {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).cloned()
    }

    /// Remove a task.
    pub async fn remove_task(self: &Arc<Self>, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        tasks.remove(task_id);
    }

    /// Add cost to total.
    pub fn add_cost(&self, cost_usd: f64) {
        let mut total = self.total_cost_usd.lock();
        *total += cost_usd;
    }

    /// Get total cost.
    pub fn total_cost(&self) -> f64 {
        *self.total_cost_usd.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_creation() {
        let task = ArtemisTask::new(
            "test-1".into(),
            "Open Settings and check WiFi".into(),
            Some("device-1".into()),
        );
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.result.is_none());
    }

    #[test]
    fn test_device_registry() {
        let state = ArtemisState::new();
        let device = DeviceInfo {
            id: "emu-5554".into(),
            model: "Pixel 7".into(),
            manufacturer: "Google".into(),
            android_version: "14".into(),
            screen_width: 1080,
            screen_height: 2400,
            is_emulator: true,
            status: DeviceStatus::Connected,
        };
        state.register_device(device.clone());
        let devices = state.get_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].model, "Pixel 7");
    }

    #[test]
    fn test_cost_tracking() {
        let state = ArtemisState::new();
        state.add_cost(0.05);
        state.add_cost(0.03);
        assert!((state.total_cost() - 0.08).abs() < 0.001);
    }
}
