//! Anti-block fingerprint-spoofing JS payload (Layer 1).
//!
//! This string is injected into every page via
//! `Page.addScriptToEvaluateOnNewDocument` (chromiumoxide 0.7) so the
//! patches are applied **before** any page script runs. The payload is
//! taken **verbatim** from plan §C.3 — do not edit the body of
//! `STEALTH_JS` without re-reading the plan's "Tested against" list
//! (Cloudflare v2, PerimeterX human-check, hCaptcha enterprise).
//!
//! Why a single string and not a structured set of patches? Two
//! reasons:
//!   1. The plan tested this exact payload as one unit. Splitting it
//!      risks subtle ordering bugs (e.g. webdriver must be patched
//!      before any check that reads it).
//!   2. CDP injects the script into every frame, including cross-
//!      origin iframes. A single `(() => { ... })()` IIFE keeps the
//!      scope sealed.
use std::sync::OnceLock;

/// The full Layer 1 stealth payload. Patched against:
/// - Cloudflare v2 (basic challenge cleared)
/// - PerimeterX human-check (cleared)
/// - hCaptcha enterprise (cleared on first request)
///
/// NOT patched against Yandex SmartCaptcha with active challenge
/// (see plan §D.1 — needs Layer 3 behavioral sim).
pub const STEALTH_JS: &str = r#"(() => {
  // 1. navigator.webdriver = false (kills the #1 Cloudflare/PerimeterX check)
  Object.defineProperty(Navigator.prototype, 'webdriver', {
    get: () => false,
    configurable: true
  });

  // 2. window.chrome stub (Cloudflare checks window.chrome.runtime)
  if (!window.chrome) {
    window.chrome = {
      runtime: {
        PlatformOs: { MAC: 'mac', WIN: 'win', ANDROID: 'android', CROS: 'cros', LINUX: 'linux', OPENBSD: 'openbsd' },
        PlatformArch: { ARM: 'arm', X86_32: 'x86-32', X86_64: 'x86-64' },
        RequestUpdateCheckStatus: { THROTTLED: 'throttled', NO_UPDATE: 'no_update', UPDATE_AVAILABLE: 'update_available' },
        OnInstalledReason: { INSTALL: 'install', UPDATE: 'update', CHROME_UPDATE: 'chrome_update', SHARED_MODULE_UPDATE: 'shared_module_update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
        connect: () => {},
        sendMessage: () => {}
      },
      loadTimes: () => ({}),
      csi: () => ({}),
      app: { isInstalled: false, InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' }, RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' } }
    };
  }

  // 3. plugins / languages (Cloudflare v2, hCaptcha enterprise)
  Object.defineProperty(navigator, 'plugins', {
    get: () => {
      const arr = [
        { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer',  filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
        { name: 'Native Client',      filename: 'internal-nacl-plugin', description: '' }
      ];
      arr.length = 3;
      return arr;
    },
    configurable: true
  });
  Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'], configurable: true });

  // 4. WebGL vendor/renderer (PerimeterX, FingerprintJS)
  const getParameter = WebGLRenderingContext.prototype.getParameter;
  WebGLRenderingContext.prototype.getParameter = function(p) {
    if (p === 37445) return 'Intel Inc.';          // UNMASKED_VENDOR_WEBGL
    if (p === 37446) return 'Intel Iris OpenGL Engine'; // UNMASKED_RENDERER_WEBGL
    return getParameter.call(this, p);
  };

  // 5. permissions query (DataDome, Cloudflare) — 'notifications' default-deny is the giveaway
  const origQuery = navigator.permissions && navigator.permissions.query;
  if (origQuery) {
    navigator.permissions.query = (params) =>
      params.name === 'notifications'
        ? Promise.resolve({ state: Notification.permission, onchange: null })
        : origQuery.call(navigator.permissions, params);
  }

  // 6. iframe contentWindow — disable headless detection via toString leak
  // (already covered by webdriver=false in 99% of cases; skip unless fingerprintjs reports it)
})();"#;

/// Lazy accessor for tests that want to assert on size / structure
/// without copying the constant. The `OnceLock` ensures a single
/// allocation across the process.
pub fn stealth_js() -> &'static str {
    // The `OnceLock` is here so we can replace STEALTH_JS in tests
    // (e.g. to inject a counter patch) without touching every call
    // site. In practice the const is used directly because CDP
    // requires `&'static str`.
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| STEALTH_JS.to_string()).as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_iife_sealed() {
        let js = STEALTH_JS;
        assert!(js.starts_with("(() => {"), "must be an IIFE");
        assert!(js.trim_end().ends_with("})();"), "must end with IIFE close");
    }

    #[test]
    fn payload_patches_webdriver() {
        // Sanity-check that the four core patches are present so a
        // future "let's tidy this up" doesn't silently drop one.
        assert!(STEALTH_JS.contains("'webdriver'"), "must patch webdriver");
        assert!(STEALTH_JS.contains("window.chrome"), "must stub window.chrome");
        assert!(STEALTH_JS.contains("'plugins'"), "must patch plugins");
        assert!(STEALTH_JS.contains("'languages'"), "must patch languages");
        assert!(STEALTH_JS.contains("UNMASKED_VENDOR_WEBGL") || STEALTH_JS.contains("37445"),
                "must patch WebGL vendor");
        assert!(STEALTH_JS.contains("permissions.query"), "must patch permissions");
    }

    #[test]
    fn payload_is_static_str() {
        // The CDP API takes `impl Into<String>`; this just makes sure
        // nobody accidentally swaps the const for a `String`.
        let _: &str = STEALTH_JS;
    }

    #[test]
    fn size_is_under_4kb() {
        // Sanity-bound: 4KB is well under CDP's frame-injection
        // limit and small enough to inspect by eye in a debugger.
        assert!(STEALTH_JS.len() < 4096, "payload is {} bytes", STEALTH_JS.len());
    }
}