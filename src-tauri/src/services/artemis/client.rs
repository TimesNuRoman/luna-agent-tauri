//! ADB client for Android device management (Phase T0+).
//!
//! Provides ADB connection, device listing, screenshot capture, and input
//! simulation for Android automation.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::state::{DeviceInfo, DeviceStatus};

/// ADB client errors.
#[derive(Debug, thiserror::Error)]
pub enum AdbError {
    #[error("ADB not found: {0}")]
    AdbNotFound(String),
    #[error("Device not found: {0}")]
    DeviceNotFound(String),
    #[error("Command failed: {0}")]
    CommandFailed(String),
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Screenshot decode error: {0}")]
    ScreenshotDecode(String),
}

/// ADB client for Android device operations.
pub struct AdbClient {
    adb_path: PathBuf,
    timeout_secs: u64,
}

impl AdbClient {
    /// Create a new ADB client. Searches PATH for `adb` by default.
    pub fn new(adb_path: Option<String>, timeout_secs: u64) -> Result<Self, AdbError> {
        let path = if let Some(p) = adb_path {
            PathBuf::from(p)
        } else {
            // Search for adb in PATH
            Self::find_adb()?
        };
        Ok(Self {
            adb_path: path,
            timeout_secs,
        })
    }

    /// Find adb in system PATH.
    fn find_adb() -> Result<PathBuf, AdbError> {
        let output = std::process::Command::new("which")
            .arg("adb")
            .output()
            .map_err(|e| AdbError::AdbNotFound(e.to_string()))?;
        
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
        
        // Fallback: check common locations
        let candidates = [
            "/usr/bin/adb",
            "/usr/local/bin/adb",
            "/opt/android-sdk/platform-tools/adb",
            "/home/android-sdk/platform-tools/adb",
            "C:\\Android\\platform-tools\\adb.exe",
        ];
        
        for candidate in &candidates {
            if PathBuf::from(candidate).exists() {
                return Ok(PathBuf::from(candidate));
            }
        }
        
        Err(AdbError::AdbNotFound("ADB not found in PATH or common locations".into()))
    }

    /// Run an ADB command and return output.
    async fn run(&self, args: &[&str]) -> Result<String, AdbError> {
        let output = match timeout(
            Duration::from_secs(self.timeout_secs),
            Command::new(&self.adb_path)
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        ).await {
            Ok(result) => result.map_err(|e| AdbError::CommandFailed(e.to_string()))?,
            Err(_) => return Err(AdbError::Timeout(format!(
                "ADB command timed out after {}s", self.timeout_secs
            ))),
        };
        
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(AdbError::CommandFailed(stderr.to_string()))
        }
    }

    /// List connected devices.
    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>, AdbError> {
        let output = self.run(&["devices", "-l"]).await?;
        let mut devices = Vec::new();
        
        for line in output.lines().skip(1) {
            // Skip header line "List of devices attached"
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            // Format: "serial          device|offline|unauthorized transport_id:1"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let id = parts[0].to_string();
                let status_str = parts[1];
                
                let status = match status_str {
                    "device" => DeviceStatus::Connected,
                    "offline" => DeviceStatus::Disconnected,
                    "unauthorized" => DeviceStatus::Unauthorized,
                    _ => DeviceStatus::Disconnected,
                };
                
                // Get device properties
                let (model, manufacturer, version, width, height) = 
                    self.get_device_properties(&id).await.unwrap_or((
                        "Unknown".into(),
                        "Unknown".into(),
                        "Unknown".into(),
                        1080,
                        2400,
                    ));
                
                let is_emulator = id.starts_with("emulator-") || id.starts_with("localhost:");
                
                devices.push(DeviceInfo {
                    id,
                    model,
                    manufacturer,
                    android_version: version,
                    screen_width: width,
                    screen_height: height,
                    is_emulator,
                    status,
                });
            }
        }
        
        Ok(devices)
    }

    /// Get device properties.
    async fn get_device_properties(&self, device_id: &str) -> Result<(String, String, String, u32, u32), AdbError> {
        let device_id = device_id.to_string();
        
        let model = self.run(&[&format!("-s{}", device_id), "shell", "getprop", "ro.product.model"])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".into());
        
        let manufacturer = self.run(&[&format!("-s{}", device_id), "shell", "getprop", "ro.product.manufacturer"])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".into());
        
        let version = self.run(&[&format!("-s{}", device_id), "shell", "getprop", "ro.build.version.release"])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".into());
        
        // Get screen size
        let wm_output = self.run(&[&format!("-s{}", device_id), "shell", "wm", "size"])
            .await
            .ok();
        
        let (width, height) = if let Some(out) = wm_output {
            parse_wm_size(&out).unwrap_or((1080, 2400))
        } else {
            (1080, 2400)
        };
        
        Ok((model, manufacturer, version, width, height))
    }

    /// Take a screenshot of the device.
    pub async fn screenshot(&self, device_id: &str, output_path: &PathBuf) -> Result<(), AdbError> {
        // First save screenshot to device
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "screencap",
            "-p",
            "/sdcard/screen.png"
        ]).await?;
        
        // Pull to local
        self.run(&[
            &format!("-s{device_id}"),
            "pull",
            "/sdcard/screen.png",
            output_path.to_str().unwrap_or("/tmp/screen.png")
        ]).await?;
        
        // Remove from device
        let _ = self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "rm",
            "/sdcard/screen.png"
        ]).await;
        
        Ok(())
    }

    /// Tap on the screen.
    pub async fn tap(&self, device_id: &str, x: i32, y: i32) -> Result<(), AdbError> {
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "input",
            "tap",
            &x.to_string(),
            &y.to_string()
        ]).await?;
        Ok(())
    }

    /// Swipe on the screen.
    pub async fn swipe(&self, device_id: &str, x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u32) -> Result<(), AdbError> {
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "input",
            "swipe",
            &x1.to_string(),
            &y1.to_string(),
            &x2.to_string(),
            &y2.to_string(),
            &duration_ms.to_string()
        ]).await?;
        Ok(())
    }

    /// Input text.
    pub async fn input_text(&self, device_id: &str, text: &str) -> Result<(), AdbError> {
        // Escape special characters for shell
        let escaped = text
            .replace("\\", "\\\\")
            .replace(" ", "\\ ")
            .replace("(", "\\(")
            .replace(")", "\\)");
        
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "input",
            "text",
            &escaped
        ]).await?;
        Ok(())
    }

    /// Press key code (e.g., BACK, HOME, ENTER).
    pub async fn press_key(&self, device_id: &str, keycode: &str) -> Result<(), AdbError> {
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "input",
            "keyevent",
            keycode
        ]).await?;
        Ok(())
    }

    /// Open an app by package name.
    pub async fn open_app(&self, device_id: &str, package: &str) -> Result<(), AdbError> {
        self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "monkey",
            "-p",
            package,
            "-c",
            "android.intent.category.LAUNCHER",
            "1"
        ]).await?;
        Ok(())
    }

    /// Get current focused app package.
    pub async fn get_current_app(&self, device_id: &str) -> Result<String, AdbError> {
        let output = self.run(&[
            &format!("-s{device_id}"),
            "shell",
            "dumpsys",
            "activity",
            "activities"
        ]).await?;
        
        // Parse the output to find the top activity
        for line in output.lines().rev() {
            if line.contains("mResumedActivity") || line.contains("mFocusedActivity") {
                if let Some(start) = line.find(' ') {
                    let activity = line[start..].trim().split(' ').next().unwrap_or("");
                    if let Some(idx) = activity.find('/') {
                        return Ok(activity[..idx].to_string());
                    }
                }
            }
        }
        
        Ok("unknown".into())
    }

    /// Install an APK.
    pub async fn install_apk(&self, device_id: &str, apk_path: &str) -> Result<(), AdbError> {
        self.run(&[
            &format!("-s{device_id}"),
            "install",
            "-r", // reinstall if exists
            apk_path
        ]).await?;
        Ok(())
    }

    /// Start ADB server.
    pub async fn start_server(&self) -> Result<(), AdbError> {
        self.run(&["start-server"]).await?;
        Ok(())
    }

    /// Connect to a network device.
    pub async fn connect(&self, host: &str) -> Result<(), AdbError> {
        self.run(&["connect", host]).await?;
        Ok(())
    }

    /// Disconnect a network device.
    pub async fn disconnect(&self, host: &str) -> Result<(), AdbError> {
        self.run(&["disconnect", host]).await?;
        Ok(())
    }
}

/// Parse `Physical size: 1080x2400` format.
fn parse_wm_size(output: &str) -> Option<(u32, u32)> {
    for line in output.lines() {
        if line.contains("Physical size:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, part) in parts.iter().enumerate() {
                if part.contains('x') {
                    let dims: Vec<&str> = part.split('x').collect();
                    if dims.len() == 2 {
                        let w = dims[0].parse().ok()?;
                        let h = dims[1].parse().ok()?;
                        return Some((w, h));
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wm_size() {
        let input = "Physical size: 1080x2400";
        assert_eq!(parse_wm_size(input), Some((1080, 2400)));
        
        let input2 = "Override size: 1080x2280";
        assert_eq!(parse_wm_size(input2), Some((1080, 2280)));
        
        let input3 = "Size: 1440x3200";
        assert_eq!(parse_wm_size(input3), None); // Missing "Physical" prefix
    }
}
