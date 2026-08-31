// Luna Agent — Tauri entry point
// The Tauri API is available globally via window.__TAURI__
// No bundler imports needed

// Try to import Tauri API for type checking, but gracefully fall back
// The actual Tauri API is injected at runtime by the Tauri runtime
declare global {
  interface Window {
    __TAURI__: any;
  }
}

// Log startup
console.log('[Luna Agent] Desktop app initialized');
