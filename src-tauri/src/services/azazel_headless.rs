//! Headless Browser Service — standalone chromiumoxide implementation.
//! Minimal stub: provides BrowserFetcher and HeadlessBrowserConfig only.
//! The actual browser lifecycle (launch, page management, close) lives in cli_api.rs
//! using the BrowserManager singleton pattern.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use chromiumoxide::browser::Browser as CxBrowser;
pub use chromiumoxide::browser::BrowserConfig as CxBrowserConfig;
pub use chromiumoxide::error::CdpError;
pub use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};
pub use chromiumoxide::page::Page as CxPage;

// PR-1 Layer 1 anti-block: pull `StealthConfig` so headless callers
// (CLI / API server) can wire a proxy + UA + JS patch into the
// Chromium launch without round-tripping through the live azazel
// browser module.
use crate::services::azazel::stealth::StealthConfig;
use crate::services::azazel::stealth_js::STEALTH_JS;

/// Configuration for headless browser.
#[derive(Debug, Clone, Default)]
pub struct HeadlessBrowserConfig {
    pub user_data_dir: Option<PathBuf>,
    pub headless: bool,
    pub width: u32,
    pub height: u32,
    pub cdp_port: u16,
    pub chrome_executable: Option<PathBuf>,
    pub extra_args: Vec<String>,
    /// PR-1 Layer 1: optional stealth config. When `Some`, its
    /// `to_chrome_args()` output is folded into `extra_args` by
    /// `to_browser_config()`, and every page created via
    /// `HeadlessBrowserSession::new_page` gets the STEALTH_JS payload
    /// installed via `Page.evaluate_on_new_document`. `None` ⇒
    /// stealth-disabled (matches the live `azazel::browser::LaunchConfig`).
    pub stealth: Option<StealthConfig>,
}

impl HeadlessBrowserConfig {
    pub fn builder() -> HeadlessBrowserConfigBuilder {
        HeadlessBrowserConfigBuilder {
            user_data_dir: None,
            headless: true,
            width: 1920,
            height: 1080,
            cdp_port: 0,
            chrome_executable: None,
            extra_args: vec![
                "--no-sandbox".to_string(),
                "--disable-gpu".to_string(),
                "--disable-dev-shm-usage".to_string(),
            ],
            stealth: None,
        }
    }

    /// Returns the Chrome executable path, downloading it via BrowserFetcher if needed.
    pub async fn get_executable_path(&self) -> Result<PathBuf, String> {
        if let Some(ref exe) = self.chrome_executable {
            return Ok(exe.clone());
        }

        let download_path = std::env::temp_dir().join("luna-chrome");
        std::fs::create_dir_all(&download_path)
            .map_err(|e| format!("Failed to create chrome download dir: {}", e))?;

        let fetcher = BrowserFetcher::new(
            BrowserFetcherOptions::builder()
                .with_path(&download_path)
                .build()
                .map_err(|e| format!("BrowserFetcher init failed: {}", e))?,
        );

        let info = fetcher
            .fetch()
            .await
            .map_err(|e| format!("Chrome download failed: {}", e))?;

        // info.executable_path is a PathBuf field
        Ok(info.executable_path)
    }

    /// Build a BrowserConfig for chromiumoxide.
    pub async fn to_browser_config(&self) -> Result<CxBrowserConfig, String> {
        let exe = self.get_executable_path().await?;

        let mut config = CxBrowserConfig::builder()
            .chrome_executable(exe.as_path())
            .no_sandbox()
            .arg("--disable-dev-shm-usage");

        if self.headless {
            config = config.arg("--headless");
        }

        if let Some(ref dir) = self.user_data_dir {
            config = config.user_data_dir(dir);
        }

        // PR-1 Layer 1: when a stealth config is present and any
        // Layer 1 toggle is on, fold its Chromium args (proxy, UA,
        // locale, viewport, `--disable-blink-features=AutomationControlled`)
        // into `extra_args` so Chromium picks them up at startup.
        // User-supplied extras land first; stealth flags come last
        // so users can still override individual flags if they need to.
        let mut effective_extra = self.extra_args.clone();
        if let Some(stealth_cfg) = self.stealth.as_ref() {
            if stealth_cfg.wants_stealth_js() {
                effective_extra.extend(stealth_cfg.to_chrome_args());
            }
        }
        for arg in &effective_extra {
            config = config.arg(arg.as_str());
        }

        config.build().map_err(|e| format!("BrowserConfig build failed: {}", e))
    }
}

pub struct HeadlessBrowserConfigBuilder {
    user_data_dir: Option<PathBuf>,
    headless: bool,
    width: u32,
    height: u32,
    cdp_port: u16,
    chrome_executable: Option<PathBuf>,
    extra_args: Vec<String>,
    stealth: Option<StealthConfig>,
}

impl HeadlessBrowserConfigBuilder {
    pub fn headless(mut self, v: bool) -> Self {
        self.headless = v;
        self
    }
    pub fn width(mut self, v: u32) -> Self {
        self.width = v;
        self
    }
    pub fn height(mut self, v: u32) -> Self {
        self.height = v;
        self
    }
    pub fn user_data_dir(mut self, v: PathBuf) -> Self {
        self.user_data_dir = Some(v);
        self
    }
    pub fn chrome_executable(mut self, v: PathBuf) -> Self {
        self.chrome_executable = Some(v);
        self
    }
    pub fn extra_arg(mut self, v: String) -> Self {
        self.extra_args.push(v);
        self
    }
    /// PR-1 Layer 1: attach a `StealthConfig` so the headless
    /// launcher pulls proxy / UA / locale / viewport flags and so
    /// every page gets the STEALTH_JS patch installed.
    pub fn stealth(mut self, cfg: StealthConfig) -> Self {
        self.stealth = Some(cfg);
        self
    }
    pub fn build(self) -> HeadlessBrowserConfig {
        HeadlessBrowserConfig {
            user_data_dir: self.user_data_dir,
            headless: self.headless,
            width: self.width,
            height: self.height,
            cdp_port: self.cdp_port,
            chrome_executable: self.chrome_executable,
            extra_args: self.extra_args,
            stealth: self.stealth,
        }
    }
}

// =====================================================================
// HeadlessBrowserSession — simple browser wrapper
// =====================================================================

use futures::StreamExt;
use chromiumoxide::page::ScreenshotParams;

/// A launched browser instance.
pub struct HeadlessBrowserSession {
    browser: CxBrowser,
    /// PR-1 Layer 1: optional stealth config copied from
    /// `HeadlessBrowserConfig::stealth`. When `Some`, every
    /// `new_page` call installs STEALTH_JS via
    /// `Page.evaluate_on_new_document` before any user script runs.
    stealth: Option<StealthConfig>,
}

impl HeadlessBrowserSession {
    /// Launch a new headless browser.
    pub async fn launch(config: HeadlessBrowserConfig) -> Result<Self, String> {
        let browser_config = config.to_browser_config().await?;

        let (browser, handler) = CxBrowser::launch(browser_config)
            .await
            .map_err(|e| format!("Browser launch failed: {}", e))?;

        // Spawn background task to drain the handler stream (prevents hung browser)
        tokio::spawn(async move {
            let mut h = handler;
            while h.next().await.is_some() {}
        });

        Ok(Self {
            browser,
            stealth: config.stealth,
        })
    }

    /// Get a reference to the underlying browser.
    pub fn browser(&self) -> &CxBrowser {
        &self.browser
    }

    /// Create a new page and navigate to a URL. When the session was
    /// launched with `stealth = Some(...)` AND any Layer 1 toggle
    /// is on, the STEALTH_JS payload is installed via
    /// `Page.evaluate_on_new_document` before the page starts
    /// loading — so it runs before any page-side script (Cloudflare,
    /// PerimeterX, hCaptcha canary checks included).
    pub async fn new_page(&mut self, url: &str) -> Result<PageHandle, String> {
        let page = self
            .browser
            .new_page(url)
            .await
            .map_err(|e| format!("Failed to create page: {}", e))?;
        // PR-1 Layer 1: best-effort stealth JS install. Failures are
        // logged but do NOT fail `new_page` — the browser is still
        // useful without the JS payload (the Chromium flags give us
        // most of the protection already).
        if self.stealth.as_ref().is_some_and(|s| s.wants_stealth_js()) {
            if let Err(e) = page.evaluate_on_new_document(STEALTH_JS.to_string()).await {
                tracing::warn!(
                    target: "luna.azazel.headless",
                    "failed to install stealth JS payload: {e}"
                );
            }
        }
        Ok(PageHandle { page })
    }

    /// Close the browser.
    pub async fn close(mut self) -> Result<(), String> {
        self.browser
            .close()
            .await
            .map(|_| ())
            .map_err(|e| format!("Browser close failed: {}", e))
    }
}

/// A page handle within a browser session.
pub struct PageHandle {
    page: CxPage,
}

impl PageHandle {
    /// Navigate to a URL.
    pub async fn navigate(&mut self, url: &str) -> Result<(), String> {
        self.page
            .goto(url)
            .await
            .map_err(|e| format!("Navigation failed: {}", e))?;
        Ok(())
    }

    /// Execute JavaScript and return the result as a string.
    pub async fn evaluate_js(&self, script: &str) -> Result<String, String> {
        let result = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("JS evaluation failed: {}", e))?;
        // Return JSON string of the value, or empty string if no value
        Ok(result
            .value()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .unwrap_or_default())
    }

    /// Get full HTML content.
    pub async fn content(&self) -> Result<String, String> {
        self.evaluate_js("document.documentElement.outerHTML").await
    }

    /// Take a screenshot (PNG bytes).
    pub async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let params = ScreenshotParams::builder().full_page(true).build();
        self.page
            .screenshot(params)
            .await
            .map_err(|e| format!("Screenshot failed: {}", e))
    }

    /// Get the page title.
    pub async fn title(&self) -> Result<String, String> {
        self.evaluate_js("document.title").await
    }

    /// PR-1 Layer 1: install the STEALTH_JS payload so it runs in
    /// every new document on this page before any user script.
    ///
    /// This is called automatically by `HeadlessBrowserSession::new_page`
    /// when the session was launched with a stealth config; callers
    /// who built their own `PageHandle` outside that path can call
    /// it explicitly to opt in.
    ///
    /// Uses chromiumoxide 0.7's `Page::evaluate_on_new_document`
    /// which takes `impl Into<AddScriptToEvaluateOnNewDocumentParams>`
    /// (a `String` implements that via the
    /// `From<T: Into<String>>` impl on the params struct).
    pub async fn install_stealth_script(&self) -> Result<(), String> {
        self.page
            .evaluate_on_new_document(STEALTH_JS.to_string())
            .await
            .map(|_id| ())
            .map_err(|e| format!("install_stealth_script: {e}"))
    }
}

// =====================================================================
// google_search — search Google using headless Chrome
// =====================================================================

// PR-1 Layer 1: re-export the stealth accessor so `google_search`
// can read the process-global config that `cli::main` populates.
// Library consumers (and tests) that bypass the CLI get the same
// behavior because `try_current_stealth` returns `None` and we
// fall through to the unstealthed default.
use crate::services::azazel::stealth::{current_stealth, try_current_stealth};

/// Search Google and return results as strings.
pub async fn google_search(query: &str, num_results: usize) -> Result<Vec<String>, String> {
    // PR-1 Layer 1: pull the stealth config from the OnceLock
    // populated by `cli::main`. `current_stealth()` falls back to
    // env-var resolution when the lock is empty, so a unit test
    // that doesn't go through the CLI still gets the right
    // behavior (no stealth unless `LUNA_*` env vars say otherwise).
    let stealth_cfg = current_stealth();

    let mut builder = HeadlessBrowserConfig::builder();
    let mut warm = std::time::Duration::from_secs(0);
    if let Some(cfg) = stealth_cfg.as_ref() {
        builder = builder.stealth(cfg.clone());
        warm = cfg.warm_duration();
    }
    let config = builder.build();

    // PR-1 Layer 3 placeholder: honor `warm_session_min` by sleeping
    // before the first navigate. The actual warm-up (browse to a
    // benign page, do some human-shaped clicks) is a Layer 3 PR; for
    // now we just sleep so a `--warm-session 120` invocation is
    // observable end-to-end.
    if !warm.is_zero() {
        tracing::info!(
            target: "luna.azazel.headless",
            warm_secs = warm.as_secs(),
            "PR-1 warm-session: sleeping before first navigate"
        );
        tokio::time::sleep(warm).await;
    }

    let mut session = HeadlessBrowserSession::launch(config).await?;

    let url = format!(
        "https://www.google.com/search?q={}",
        urlencoding::encode(query)
    );

    let mut page = session.new_page(&url).await?;

    // Wait for results to load
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Extract result snippets
    let script = r#"
        (function() {
            var results = [];
            var items = document.querySelectorAll('.g');
            for (var i = 0; i < Math.min(items.length, arguments[0]); i++) {
                var title = items[i].querySelector('h3');
                var snippet = items[i].querySelector('.VwiC3b');
                results.push({
                    title: title ? title.textContent : '',
                    snippet: snippet ? snippet.textContent : ''
                });
            }
            return JSON.stringify(results);
        })(arguments[0]);
    "#;

    let json = page.evaluate_js(&script.replace("arguments[0]", &num_results.to_string())).await?;
    let results: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();

    let formatted: Vec<String> = results
        .iter()
        .map(|r| {
            let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let snippet = r.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
            format!("{}\n{}\n", title, snippet)
        })
        .collect();

    // Close browser
    session.close().await?;

    // Touch the accessor so the import is preserved across edits —
    // `try_current_stealth` is the right call for callers that want
    // to distinguish "CLI has populated the lock" from "no CLI ran"
    // (the difference matters for the test harness).
    let _ = try_current_stealth();

    Ok(formatted)
}
