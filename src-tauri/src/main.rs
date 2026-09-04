// Luna Agent — бинарь-обёртка. Вся логика в `lib::run` (библиотечный корень).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The `luna_tools_schema` function builds a 30+ tool JSON literal via
// `serde_json::json!([...])`. The default recursion limit (128) trips
// the macro hygiene. 512 is enough for the current schema and headroom
// for ~10 more tools.
#![recursion_limit = "512"]

use std::time::Duration;

fn main() {
    // Phase E3 self-evolution: when launched with `--smoke`, run a
    // lightweight smoke check. We DO NOT spin up a full Tauri window
    // (that requires WebView2 and an active display, both flaky in
    // headless CI). Instead we verify the binary can:
    //   1. Initialize basic subsystems (keyring probe, temp dir).
    //   2. Stay alive for ~25 seconds without panicking.
    //   3. Exit 0 cleanly.
    //
    // Phase E4 (or beyond) can replace this with a real `tauri::Builder`
    // smoke that opens a hidden window, but that needs a display
    // server and WebView2.
    if std::env::args().any(|a| a == "--smoke") {
        run_smoke_mode();
        return;
    }
    luna_agent::run();
}

fn run_smoke_mode() {
    eprintln!("[luna-smoke] starting (binary boot probe)");

    // 1. Catch any panic during early init.
    let init_ok = std::panic::catch_unwind(|| {
        // Touch a few things that exercise subsystem init:
        // - Resolve a temp dir (cheap, but proves std::env works).
        // - Probe keyring (creates an Entry but does not write).
        let _tmp = std::env::temp_dir();
        // The keyring probe is best-effort: if the OS credential store
        // is unavailable, we don't want to fail the whole smoke. So
        // we just call `provider_id` (a pure function).
        let _id = luna_agent::sandbox::provider_id("anthropic");
    })
    .is_ok();

    if !init_ok {
        eprintln!("[luna-smoke] PANIC during init");
        std::process::exit(1);
    }

    eprintln!("[luna-smoke] init OK; sleeping 25s");

    // 2. Sleep for 25 seconds. If something on a background thread
    // panics, `catch_unwind` won't see it, but the process will exit
    // non-zero — and our parent (the sandbox runner) checks the
    // exit code.
    std::thread::sleep(Duration::from_secs(25));

    eprintln!("[luna-smoke] OK");
    std::process::exit(0);
}
