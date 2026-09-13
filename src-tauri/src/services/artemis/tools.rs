//! ARTEMIS tool definitions (Phase T0+).
//!
//! Defines the `android_*` tools available to Luna's AI chat interface.

/// Tool definitions for Android automation.
pub fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "android_run",
            "description": "Dispatch an Android automation task to ARTEMIS, Luna's Android automation agent. ARTEMIS uses ADB to control a connected Android device (phone or emulator), captures screenshots, and executes UI actions (tap, swipe, text input, key presses) to complete user requests. Returns a task_id immediately; the actual run happens asynchronously.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Natural language description of the task to perform on the Android device (e.g., 'Open Settings, go to WiFi, and toggle it off', 'Open Instagram and search for cats')."
                    },
                    "device_id": {
                        "type": "string",
                        "description": "Optional specific device ID (serial from 'adb devices'). If not provided, ARTEMIS will use the first available connected device."
                    }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "android_status",
            "description": "Check the status of a previously dispatched Android automation task. Returns the current state (pending/running/done/error/cancelled) and any result or error message.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task_id returned by android_run."
                    }
                },
                "required": ["task_id"]
            }
        },
        {
            "name": "android_cancel",
            "description": "Cancel a running Android automation task. Returns success even if the task is already completed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task_id of the task to cancel."
                    }
                },
                "required": ["task_id"]
            }
        },
        {
            "name": "android_list_devices",
            "description": "List all connected Android devices (phones and emulators) via ADB. Returns device serial, model, status, and whether it's an emulator.",
            "parameters": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "android_screenshot",
            "description": "Take a screenshot of the specified Android device and return it as a base64-encoded PNG image.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices. Defaults to first connected device."
                    }
                },
                "required": []
            }
        },
        {
            "name": "android_tap",
            "description": "Perform a tap (touch) action at the specified screen coordinates on an Android device.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices."
                    },
                    "x": {
                        "type": "integer",
                        "description": "X coordinate on screen (0-indexed from left)."
                    },
                    "y": {
                        "type": "integer",
                        "description": "Y coordinate on screen (0-indexed from top)."
                    }
                },
                "required": ["x", "y"]
            }
        },
        {
            "name": "android_swipe",
            "description": "Perform a swipe gesture on an Android device.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices."
                    },
                    "x1": {
                        "type": "integer",
                        "description": "Start X coordinate."
                    },
                    "y1": {
                        "type": "integer",
                        "description": "Start Y coordinate."
                    },
                    "x2": {
                        "type": "integer",
                        "description": "End X coordinate."
                    },
                    "y2": {
                        "type": "integer",
                        "description": "End Y coordinate."
                    },
                    "duration_ms": {
                        "type": "integer",
                        "description": "Swipe duration in milliseconds (default: 300).",
                        "default": 300
                    }
                },
                "required": ["x1", "y1", "x2", "y2"]
            }
        },
        {
            "name": "android_input_text",
            "description": "Type text into the focused input field on an Android device.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type."
                    }
                },
                "required": ["text"]
            }
        },
        {
            "name": "android_press_key",
            "description": "Press a hardware or system key on an Android device.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices."
                    },
                    "keycode": {
                        "type": "string",
                        "description": "Keycode to press. Common values: BACK, HOME, ENTER, VOLUME_UP, VOLUME_DOWN, POWER, DELETE."
                    }
                },
                "required": ["keycode"]
            }
        },
        {
            "name": "android_open_app",
            "description": "Launch an app by its package name on an Android device.",
            "parameters": {
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "Device ID from android_list_devices."
                    },
                    "package": {
                        "type": "string",
                        "description": "App package name (e.g., 'com.instagram.android', 'com.twitter.android', 'com.google.android.settings')."
                    }
                },
                "required": ["package"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions_count() {
        let tools = tool_definitions();
        let arr = tools.as_array().unwrap();
        assert!(arr.len() >= 10, "Should have at least 10 tools");
    }

    #[test]
    fn test_required_fields() {
        let tools = tool_definitions();
        let arr = tools.as_array().unwrap();
        
        // android_run should require prompt
        let run_tool = arr.iter().find(|t| t.get("name").and_then(|n| n.as_str()) == Some("android_run")).unwrap();
        let params = run_tool.get("parameters").unwrap();
        let required = params.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("prompt")));
    }
}
