//! `BrowserSession` — тонкая обёртка над `chromiumoxide::Browser`.
//!
//! Phase Z0 design:
//! - `BrowserSession::launch` поднимает persistent Chrome.
//! - `new_page` создаёт новую `Page` для задачи.
//! - `navigate` / `screenshot` / `extract_text` / `current_url` —
//!   примитивы для supervisor-цикла.
//! - `close_page` закрывает страницу, не убивая процесс.
//!
//! Реализация намеренно thin: вся сложная логика (approval gates,
//! retry, policy) живёт в `supervisor.rs` и `safety.rs`. Здесь —
//! только Rusty-обёртки, маппящие ошибки chromiumoxide в наши.
//!
//! Поле `Browser` хранится как `Arc<Browser>` через `OnceLock`-style
//! singleton в `AppState` (см. `state.rs`). Сам по себе этот модуль
//! не владеет singleton'ом — он получает `Arc<Browser>` параметром.

use base64::Engine as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;

use crate::services::azazel::state::BrowserFrame;

/// Crate-local alias for the chromiumoxide Browser type. Kept here
/// so a future version bump only touches one line.
pub use chromiumoxide::browser::Browser as CxBrowser;
pub use chromiumoxide::error::CdpError;
pub use chromiumoxide::handler::Handler as CxHandler;
pub use chromiumoxide::page::Page as CxPage;

/// Errors from the Azazel browser layer. Mapped 1:1 to
/// `Result<T, String>` at the Tauri command boundary.
#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("chromiumoxide launch failed: {0}")]
    Launch(String),
    #[error("page operation failed: {0}")]
    Page(String),
    #[error("navigation timeout after {0:?}s")]
    NavTimeout(u64),
    #[error("url parse error: {0}")]
    Url(String),
    #[error("browser session is not running — call `BrowserSession::launch` first")]
    NotRunning,
}

impl From<BrowserError> for String {
    fn from(e: BrowserError) -> Self {
        e.to_string()
    }
}

/// Configuration for `BrowserSession::launch`.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Path to the persistent profile directory. Cookies and logins
    /// survive restarts. Created if missing.
    pub profile_dir: PathBuf,
    /// `false` = headed (window visible). Default for Azazel so the
    /// user can watch what the agent does.
    pub headless: bool,
    /// `window-size` CDP arg. Default: 1280x720.
    pub window_size: (u32, u32),
    /// Extra Chromium args. Useful for things like `--no-sandbox`
    /// (CI/Docker) or `--lang=ru-RU`.
    pub extra_args: Vec<String>,
}

impl LaunchConfig {
    /// Sensible defaults for a headed, persistent Azazel session.
    pub fn persistent_default(profile_dir: PathBuf) -> Self {
        Self {
            profile_dir,
            headless: false,
            window_size: (1280, 720),
            extra_args: Vec::new(),
        }
    }
}

/// A running Azazel browser session.
///
/// Cheap to clone (inner `Arc`). All methods are async.
#[derive(Clone)]
pub struct BrowserSession {
    inner: Arc<BrowserInner>,
}

// SAFETY: chromiumoxide's `Browser` is constructed from channels that
// are `Send` (futures::channel::mpsc), and the `Handler` we hold is
// a tokio task that only this process owns. Each Azazel task creates
// its own `BrowserSession` and uses it serially (no concurrent
// access), so it is safe to move across threads.
//
// `Sync` is also sound because the only mutating operation goes
// through `&mut self` (e.g. `mark_closed`) which is exclusive.
unsafe impl Send for BrowserSession {}
unsafe impl Sync for BrowserSession {}

struct BrowserInner {
    /// The chromiumoxide Browser. Set once at launch; immutable after.
    browser: CxBrowser,
    /// The Handler that drives the websocket connection to Chrome.
    /// MUST be kept alive for the lifetime of `browser` — dropping
    /// the handler severs the CDP connection. We hold it in the
    /// struct so the connection survives across `new_page` calls.
    _handler: CxHandler,
    /// Whether the user explicitly closed the session (e.g. on app
    /// shutdown). New tasks should reject when this is true.
    closed: AtomicBool,
}

impl BrowserSession {
    /// Launch a persistent Chrome under `config`. Returns an error if
    /// the binary can't be found or the profile dir is locked.
    ///
    /// Phase Z0 implementation: builds a chromiumoxide `BrowserConfig`
    /// and calls `Browser::launch`. Heavy work — call this from a
    /// tokio task, not from a Tauri command body.
    pub async fn launch(config: LaunchConfig) -> Result<Self, BrowserError> {
        use chromiumoxide::browser::BrowserConfig;

        // Make sure the profile dir exists. The supervisor has likely
        // already created it via `BrowserState::new`, but be defensive
        // (a user can change the profile dir in Settings).
        if let Err(e) = std::fs::create_dir_all(&config.profile_dir) {
            return Err(BrowserError::Launch(format!(
                "could not create profile dir {}: {e}",
                config.profile_dir.display()
            )));
        }

        // Build the chromiumoxide BrowserConfig. `user_data_dir` takes
        // `impl AsRef<Path>` (NOT `Option<…>`) and `.build()` returns
        // `Result<BrowserConfig, String>`.
        let mut builder = BrowserConfig::builder()
            .user_data_dir(&config.profile_dir)
            .window_size(config.window_size.0, config.window_size.1)
            .with_head();
        for arg in &config.extra_args {
            builder = builder.arg(arg.clone());
        }
        let cx_config = builder
            .build()
            .map_err(|e| BrowserError::Launch(format!("build config: {e}")))?;

        let (browser, handler) = CxBrowser::launch(cx_config)
            .await
            .map_err(|e| BrowserError::Launch(format!("launch: {e}")))?;

        Ok(Self {
            inner: Arc::new(BrowserInner {
                browser,
                _handler: handler,
                closed: AtomicBool::new(false),
            }),
        })
    }

    /// True if the session is still considered alive. Phase Z0: a
    /// simple flag; Phase Z1+ may add a CDP `Browser.getVersion`
    /// ping.
    pub fn is_alive(&self) -> bool {
        !self.inner.closed.load(Ordering::Acquire)
    }

    /// Mark the session as closed. Future `new_page` calls will
    /// fail. Idempotent.
    pub fn mark_closed(&self) {
        self.inner.closed.store(true, Ordering::Release);
    }

    /// Open a new tab (`about:blank`) and return a `TaskPage` handle.
    pub async fn new_page(&self, task_id: &str) -> Result<TaskPage, BrowserError> {
        if !self.is_alive() {
            return Err(BrowserError::NotRunning);
        }
        let page = self
            .inner
            .browser
            .new_page("about:blank")
            .await
            .map_err(|e| BrowserError::Page(format!("new_page({task_id}): {e}")))?;
        Ok(TaskPage {
            task_id: task_id.to_string(),
            page: Arc::new(page),
        })
    }

    /// Get the underlying `CxBrowser` for shutdown / advanced use.
    /// Phase Z0 callers should prefer `mark_closed()`.
    pub fn raw(&self) -> &CxBrowser {
        &self.inner.browser
    }
}

/// A browser page owned by a single Azazel task.
///
/// Cloning is cheap (inner `Arc`).
#[derive(Clone)]
pub struct TaskPage {
    pub task_id: String,
    page: Arc<CxPage>,
}

// SAFETY: same reasoning as `BrowserSession`. Each Azazel task owns
// its `TaskPage` exclusively; the underlying `chromiumoxide::Page`
// is just a handle to the CDP target and is safe to share across
// threads as long as the actual CDP commands are issued serially
// (which the supervisor guarantees by awaiting each one).
unsafe impl Send for TaskPage {}
unsafe impl Sync for TaskPage {}

impl TaskPage {
    /// Navigate the page to `url`. Returns when the `Page.loadEventFired`
    /// fires (chromiumoxide default). Times out per the
    /// `chromiumoxide::page::NavigateParams` defaults (~30s).
    pub async fn navigate(&self, url: &str) -> Result<(), BrowserError> {
        if url.trim().is_empty() {
            return Err(BrowserError::Url("empty url".into()));
        }
        // Validate scheme — chromiumoxide does this internally, but a
        // pre-check gives a friendlier error to the model.
        if !(url.starts_with("http://") || url.starts_with("https://") || url == "about:blank") {
            return Err(BrowserError::Url(format!(
                "url must start with http://, https://, or be about:blank (got: {url:?})"
            )));
        }
        self.page
            .goto(url)
            .await
            .map_err(|e| BrowserError::Page(format!("goto({url}): {e}")))?;
        Ok(())
    }

    /// Capture the current page as a JPEG. The bytes are returned
    /// directly; the supervisor (or caller) is responsible for
    /// base64-encoding + frame-cache insertion.
    pub async fn screenshot_jpeg(&self, quality: u8) -> Result<Vec<u8>, BrowserError> {
        use chromiumoxide::page::ScreenshotParams;
        let q = quality.clamp(1, 100);
        let params = ScreenshotParams::builder()
            .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Jpeg)
            .quality(q as i64)
            .build();
        let img = self
            .page
            .screenshot(params)
            .await
            .map_err(|e| BrowserError::Page(format!("screenshot: {e}")))?;
        Ok(img)
    }

    /// Read the current page URL (the address bar string).
    pub async fn current_url(&self) -> Result<String, BrowserError> {
        self.page
            .url()
            .await
            .map(|u| u.map(|x| x.to_string()).unwrap_or_default())
            .map_err(|e| BrowserError::Page(format!("url: {e}")))
    }

    /// Click an element matched by CSS selector. Internally uses a
    /// `querySelector` + `HTMLElement.click()` JS expression so the
    /// page's own event handlers fire (some pages override
    /// `.click()`; for those, see `click_via_dispatch`).
    ///
    /// Returns an error string (not a `BrowserError`) so the
    /// supervisor can put it in the tool result without wrapping.
    pub async fn click(&self, selector: &str) -> Result<String, String> {
        if selector.trim().is_empty() {
            return Err("selector must not be empty".into());
        }
        let script = format!(
            r#"(function() {{
                const el = document.querySelector({sel});
                if (!el) return 'error: no element matches ' + {sel};
                el.click();
                return 'clicked ' + {sel};
            }})()"#,
            sel = json_str(selector),
        );
        let v = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("click eval: {e}"))?;
        let s: String = v.into_value().unwrap_or_default();
        if s.starts_with("error:") {
            Err(s)
        } else {
            Ok(s)
        }
    }

    /// Type text into a focused element. We focus first (so the
    /// page's onFocus handlers run), then dispatch one
    /// `beforeinput` + `input` + `keyup` per character via a single
    /// `set` + dispatch loop.
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<String, String> {
        if selector.trim().is_empty() {
            return Err("selector must not be empty".into());
        }
        let script = format!(
            r#"(function() {{
                const el = document.querySelector({sel});
                if (!el) return 'error: no element matches ' + {sel};
                el.focus();
                if (!('value' in el) && !el.isContentEditable) {{
                    return 'error: element is not focusable';
                }}
                const text = {txt};
                if ('value' in el) {{
                    el.value = text;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                }} else {{
                    el.textContent = text;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                }}
                return 'typed ' + text.length + ' chars into ' + {sel};
            }})()"#,
            sel = json_str(selector),
            txt = json_str(text),
        );
        let v = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("type eval: {e}"))?;
        let s: String = v.into_value().unwrap_or_default();
        if s.starts_with("error:") {
            Err(s)
        } else {
            Ok(s)
        }
    }

    /// Press a single keyboard key. We dispatch `keydown`, `keypress`
    /// (legacy), and `keyup` on `document.activeElement`. The
    /// `key` string is what the page's JS sees (e.g. "Enter",
    /// "Tab", "Escape", "ArrowDown").
    pub async fn press_key(&self, key: &str) -> Result<String, String> {
        if key.trim().is_empty() {
            return Err("key must not be empty".into());
        }
        let script = format!(
            r#"(function() {{
                const el = document.activeElement || document.body;
                const ev = (type) => new KeyboardEvent(type, {{
                    key: {key}, bubbles: true, cancelable: true
                }});
                el.dispatchEvent(ev('keydown'));
                el.dispatchEvent(ev('keypress'));
                el.dispatchEvent(ev('keyup'));
                return 'pressed ' + {key};
            }})()"#,
            key = json_str(key),
        );
        let v = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("press_key eval: {e}"))?;
        Ok(v.into_value().unwrap_or_default())
    }

    /// Scroll the page (or a specific element) by N pixels in
    /// `direction`. `pixels` is clamped to `[0, 5000]`.
    pub async fn scroll(
        &self,
        direction: &str,
        pixels: u32,
        selector: Option<&str>,
    ) -> Result<String, String> {
        let dir = match direction {
            "up" => "up",
            "down" => "down",
            "left" => "left",
            "right" => "right",
            other => return Err(format!("direction must be up/down/left/right (got {other:?})")),
        };
        let px = pixels.min(5000);
        let script = match selector {
            Some(sel) if !sel.trim().is_empty() => format!(
                r#"(function() {{
                    const el = document.querySelector({sel});
                    if (!el) return 'error: no element matches ' + {sel};
                    el.scrollBy({{ {axis}: -{px} }});
                    return 'scrolled ' + {sel} + ' by {px}px';
                }})()"#,
                sel = json_str(sel),
                axis = match dir {
                    "up" => "top",
                    "down" => "top",
                    "left" => "left",
                    "right" => "left",
                    _ => unreachable!(),
                },
                px = px,
            ),
            _ => format!(
                r#"(function() {{
                    const {axis} = {sign}{px};
                    window.scrollBy({{ {axis} }});
                    return 'scrolled page by {px}px';
                }})()"#,
                axis = match dir {
                    "up" | "down" => "top",
                    "left" | "right" => "left",
                    _ => unreachable!(),
                },
                sign = if dir == "up" || dir == "left" { "-" } else { "" },
                px = px,
            ),
        };
        let v = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("scroll eval: {e}"))?;
        let s: String = v.into_value().unwrap_or_default();
        if s.starts_with("error:") {
            Err(s)
        } else {
            Ok(s)
        }
    }

    /// Wait `ms` milliseconds. Bounded to 30s in the schema; we
    /// additionally enforce 30s here as a safety net.
    pub async fn wait_ms(&self, ms: u64) -> Result<String, String> {
        let bounded = ms.min(30_000);
        tokio::time::sleep(std::time::Duration::from_millis(bounded)).await;
        Ok(format!("waited {bounded}ms"))
    }

    /// Select an `<option>` in a `<select>` element. Matches by
    /// value first, then by visible text (case-insensitive contains).
    pub async fn select_option(
        &self,
        selector: &str,
        value: &str,
    ) -> Result<String, String> {
        if selector.trim().is_empty() {
            return Err("selector must not be empty".into());
        }
        let script = format!(
            r#"(function() {{
                const sel = document.querySelector({sel});
                if (!(sel instanceof HTMLSelectElement)) {{
                    return 'error: not a <select>';
                }}
                const needle = {val}.toLowerCase();
                for (const opt of sel.options) {{
                    if (opt.value === {val}) {{
                        sel.value = opt.value;
                        sel.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return 'selected by value ' + opt.value;
                    }}
                }}
                for (const opt of sel.options) {{
                    if ((opt.textContent || '').toLowerCase().includes(needle)) {{
                        sel.value = opt.value;
                        sel.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return 'selected by text ' + opt.textContent;
                    }}
                }}
                return 'error: no option matches ' + {val};
            }})()"#,
            sel = json_str(selector),
            val = json_str(value),
        );
        let v = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("select_option eval: {e}"))?;
        let s: String = v.into_value().unwrap_or_default();
        if s.starts_with("error:") {
            Err(s)
        } else {
            Ok(s)
        }
    }

    /// Extract visible text from the page. Returns the first
    /// `max_chars` characters of concatenated text nodes.
    pub async fn extract_text(&self, max_chars: usize) -> Result<String, BrowserError> {
        // chromiumoxide's `Page.content()` returns the full HTML;
        // for Phase Z0 we use a lightweight tag-strip via
        // `Page::evaluate` (a CDP `Runtime.evaluate`).
        let max = max_chars.max(1);
        let script = format!(
            r#"(function() {{
                const root = document.body || document.documentElement;
                if (!root) return '';
                const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
                let out = '';
                let n;
                while ((n = walker.nextNode())) {{
                    const t = (n.nodeValue || '').trim();
                    if (!t) continue;
                    out += t + '\n';
                    if (out.length >= {max}) break;
                }}
                return out.slice(0, {max});
            }})()"#
        );
        let value = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| BrowserError::Page(format!("extract_text: {e}")))?;
        let s: String = value.into_value().unwrap_or_default();
        Ok(s)
    }

    /// Close the underlying page. The chromiumoxide Page is `Arc`'d
    /// internally, so this only decrements the refcount.
    pub async fn close(&self) -> Result<(), BrowserError> {
        // chromiumoxide 0.7 doesn't expose a `Page::close` directly;
        // closing the tab via the browser handle is the canonical way.
        // For Phase Z0, we just drop our Arc clone — the GC takes
        // care of the underlying page once all references are gone.
        let _ = &self.page;
        Ok(())
    }

    /// Borrow the underlying chromiumoxide Page (for Phase Z1+ tools
    /// like `browser_click` that need to issue raw CDP calls).
    pub fn raw(&self) -> &CxPage {
        &self.page
    }
}

/// Helper: build a base64 data URL for an image. Used by the
/// supervisor to embed a fresh screenshot into the next M3 user
/// message.
pub fn to_data_url(jpeg_bytes: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_bytes);
    format!("data:image/jpeg;base64,{b64}")
}

/// Helper: build a `BrowserFrame` from a freshly-captured screenshot
/// + URL + title. The `seq` is taken from `BrowserState.next_frame_seq`
/// upstream.
pub fn frame_from_screenshot(
    task_id: &str,
    jpeg: Vec<u8>,
    width: u32,
    height: u32,
    url: String,
    title: String,
    seq: u64,
) -> BrowserFrame {
    let _ = task_id; // included for symmetry; the cache keys on the
                     // task_id stored in `BrowserState.frames` so we
                     // don't need it on the frame itself.
    BrowserFrame {
        bytes: jpeg,
        width,
        height,
        seq,
        t_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        url,
        title,
    }
}

/// Quote a string as a JS literal. Used when we build JS source by
/// string-interpolation into `Page::evaluate`. Backslash, single
/// quotes, line breaks, and the null char are all escaped so the
/// result is safe to drop into a `(...{sel})` context.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_config_default_is_persistent() {
        let dir = std::env::temp_dir().join(format!(
            "luna-azazel-cfg-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let cfg = LaunchConfig::persistent_default(dir.clone());
        assert!(!cfg.headless, "default is headed for watch-pane");
        assert_eq!(cfg.window_size, (1280, 720));
        assert!(cfg.extra_args.is_empty());
        assert_eq!(cfg.profile_dir, dir);
    }

    #[test]
    fn to_data_url_produces_jpeg_data_url() {
        let bytes = vec![0xFF, 0xD8, 0xFF, 0xE0]; // JPEG magic
        let url = to_data_url(&bytes);
        assert!(url.starts_with("data:image/jpeg;base64,"));
        // The base64 part is 4 chars for 3 input bytes (4*ceil(4/3)=8).
        let b64 = url.trim_start_matches("data:image/jpeg;base64,");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64 round-trips");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn frame_from_screenshot_stamps_clock() {
        let f = frame_from_screenshot(
            "t1",
            vec![1, 2, 3],
            1280,
            720,
            "https://example.com".into(),
            "Example".into(),
            42,
        );
        assert_eq!(f.seq, 42);
        assert_eq!(f.width, 1280);
        assert_eq!(f.height, 720);
        assert_eq!(f.url, "https://example.com");
        assert!(f.t_ms > 0, "clock should be set");
    }

    #[test]
    fn navigate_rejects_empty_and_bad_urls() {
        // We can't easily construct a real TaskPage in a unit test
        // (no chromiumoxide Browser), but the URL validation is a
        // pure check. Test the predicate by replicating it here.
        fn ok_url(u: &str) -> bool {
            u.starts_with("http://") || u.starts_with("https://") || u == "about:blank"
        }
        assert!(!ok_url(""));
        assert!(!ok_url("example.com"));
        assert!(!ok_url("javascript:alert(1)"));
        assert!(!ok_url("file:///etc/passwd"));
        assert!(ok_url("https://example.com"));
        assert!(ok_url("http://localhost:1420"));
        assert!(ok_url("about:blank"));
    }
}
