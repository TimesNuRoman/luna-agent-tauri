//! Luna Agent CLI binary
//!
//! This binary provides a command-line interface and HTTP API server for Luna Agent.
//! It can run independently from the Tauri GUI.

use clap::Parser;
use std::sync::Arc;

mod cli_api;

/// PR-1 Layer 1 anti-block options surfaced as CLI flags. These map
/// 1:1 to the env vars consumed by `StealthConfig::from_env` (see
/// `services::azazel::stealth`) — the CLI flags win when both are
/// set, so operators can override a service-unit env file without
/// rewriting it.
#[derive(Debug, Clone, Default)]
struct StealthCliOpts {
    proxy: Option<String>,
    profile: Option<String>,
    warm_session_secs: Option<u64>,
}

impl StealthCliOpts {
    /// Build a `StealthConfig` by layering the CLI overrides on top of
    /// whatever `from_env()` returned. Returns `None` only when the
    /// kill switch (`LUNA_STEALTH_OFF=1`) is honored AND the CLI
    /// didn't re-enable anything.
    fn resolve(&self) -> Option<crate::services::azazel::stealth::StealthConfig> {
        let mut cfg = match crate::services::azazel::stealth::StealthConfig::from_env() {
            Some(c) => c,
            None => crate::services::azazel::stealth::StealthConfig::default(),
        };

        if let Some(url) = &self.proxy {
            cfg.proxy = Some(crate::services::azazel::stealth::ProxyConfig::from_url(url));
        }

        if let Some(profile) = &self.profile {
            let p = crate::services::azazel::stealth::StealthProfile::parse(profile);
            cfg.tls_client = Some(p.to_tls_client());
            if matches!(
                p,
                crate::services::azazel::stealth::StealthProfile::None
            ) {
                cfg.hide_webdriver = false;
                cfg.spoof_chrome_runtime = false;
                cfg.spoof_plugins = false;
                cfg.spoof_webgl = false;
            }
        }

        if let Some(secs) = self.warm_session_secs {
            // CLI exposes seconds; `warm_session_min` is minutes. Round
            // down so users who pass e.g. `--warm-session 45` get
            // 0 minutes (the rounding is intentional — anything under
            // 60 seconds is "essentially off" for warm-up purposes).
            cfg.warm_session_min = (secs / 60) as u32;
        }

        // If every Layer 1 toggle is off AND no proxy is set, treat
        // this as "no stealth" so we don't keep the JS installer
        // around for nothing.
        if !cfg.wants_stealth_js() && cfg.proxy.is_none() {
            return None;
        }

        Some(cfg)
    }
}

#[derive(Parser, Debug)]
#[command(name = "luna")]
#[command(about = "Luna Agent - AI coding assistant", long_about = None)]
struct Cli {
    /// Run the HTTP API server
    #[arg(long, default_value = "127.0.0.1:8080")]
    api_addr: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    // --- PR-1 Layer 1 anti-block flags --------------------------------
    //
    // All three are optional. When omitted, the corresponding env
    // var is consulted instead (see `StealthConfig::from_env`). The
    // three flags cover the three knobs the user is most likely to
    // want to override at the command line without editing a
    // service unit file:
    //
    //   --proxy              : residential proxy URL (auth embedded)
    //   --stealth-profile    : UA / TLS fingerprint profile
    //   --warm-session       : seconds to warm before first navigate
    #[arg(
        long,
        value_name = "URL",
        help = "Residential proxy URL, e.g. http://user:pass@p.webshare.io:80"
    )]
    proxy: Option<String>,

    #[arg(
        long,
        value_name = "PROFILE",
        help = "Stealth profile: chrome_120 | firefox_117 | edge_120 | none"
    )]
    stealth_profile: Option<String>,

    #[arg(
        long,
        value_name = "SECONDS",
        help = "Seconds to warm the session before the first navigate (0 = off)"
    )]
    warm_session: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(if cli.verbose {
                    tracing::Level::DEBUG.into()
                } else {
                    tracing::Level::INFO.into()
                }),
        )
        .init();

    tracing::info!("Starting Luna Agent CLI");

    // PR-1 Layer 1: resolve stealth config from CLI flags + env vars
    // and publish it to the process-global `OnceLock` so
    // `google_search` (and any future entry point) can read it
    // without re-walking the env on every call. First writer wins —
    // if the test suite also calls `init_stealth`, the CLI's value
    // is set first by virtue of running before the test body.
    let stealth_opts = StealthCliOpts {
        proxy: cli.proxy.clone(),
        profile: cli.stealth_profile.clone(),
        warm_session_secs: cli.warm_session,
    };
    if let Some(cfg) = stealth_opts.resolve() {
        let stored = crate::services::azazel::stealth::init_stealth(cfg);
        tracing::info!(
            target: "luna.azazel",
            wants_stealth_js = stored.wants_stealth_js(),
            proxy = stored.proxy.is_some(),
            warm_min = stored.warm_session_min,
            "PR-1 stealth config initialised"
        );
    } else {
        tracing::info!(
            target: "luna.azazel",
            "PR-1 stealth disabled (no proxy, no JS payload)"
        );
    }

    // Initialize core state
    let core_state = Arc::new(luna_core::CoreState::new());
    let api_state = cli_api::ApiState::new(core_state);

    // Start HTTP API server
    tracing::info!("Starting HTTP API server on {}", cli.api_addr);
    cli_api::start_server(&cli.api_addr, api_state).await?;

    Ok(())
}
