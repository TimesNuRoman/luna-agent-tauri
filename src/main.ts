// Luna Agent — Tauri entry point
// The app HTML is loaded as a resource and displayed directly

// Tell the app we're running in Tauri
(window as any).__TAURI__ = true;

// Navigate to the bundled Luna Agent HTML
const html = `
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Luna Agent</title>
</head>
<body>
  <div id="luna-loading" style="display:flex;align-items:center;justify-content:center;height:100vh;background:#09090b;color:#C9A0A0;font-family:Inter,system-ui,sans-serif;flex-direction:column;gap:16px;">
    <div style="font-size:24px;font-weight:700;">Luna Agent</div>
    <div style="font-size:13px;color:#9090a0;">Loading...</div>
  </div>
  <script>
    // Redirect to bundled app or load from same page
    window.__TAURI_INTERNALS__ = { platform: 'tauri' };
    window.addEventListener('DOMContentLoaded', function() {
      // Try to load from same origin (dev server)
      fetch(window.location.href)
        .then(function(r) { return r.text(); })
        .then(function(html) {
          document.open();
          document.write(html);
          document.close();
        })
        .catch(function() {
          // Fallback: load from bundled asset
          fetch('/luna-agent.html')
            .then(function(r) { return r.text(); })
            .then(function(html) {
              document.open();
              document.write(html);
              document.close();
            })
            .catch(function() {
              document.getElementById('luna-loading').innerHTML =
                '<div style="color:#ef4444;font-size:14px;">Failed to load app</div>';
            });
        });
    });
  <\/script>
</body>
</html>
`;

// For Tauri, we use an iframe approach to load the Luna Agent HTML
// The HTML is embedded as a data URI or loaded from the filesystem
import { create } from 'lodash';

// Simple approach: redirect to our app
const lunaHtml = document.createElement('div');
lunaHtml.style.cssText = 'position:fixed;inset:0;overflow:auto;';
document.body.appendChild(lunaHtml);

// We'll inject the Luna Agent HTML via Tauri command
import { invoke } from '@tauri-apps/api/tauri';

async function loadLuna() {
  try {
    // Load the bundled HTML from Tauri assets
    const html = await invoke<string>('get_luna_html');
    document.open();
    document.write(html);
    document.close();
  } catch (e) {
    // Fallback: redirect to the dev server or file
    console.warn('Tauri invoke failed, using inline app');
    document.body.innerHTML = '<div style="display:flex;align-items:center;justify-content:center;height:100vh;background:#09090b;color:#C9A0A0;font-family:system-ui,sans-serif;font-size:18px;">Luna Agent</div>';
  }
}

// For development: just show the loading screen
// The actual app is loaded from the HTML file in Tauri
document.body.innerHTML = `
  <div style="position:fixed;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;background:#09090b;color:#C9A0A0;font-family:Inter,system-ui,sans-serif;gap:16px;">
    <svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d="M12 3L9.27 9.27M3 12l6.27-2.73M21 12l-6.27 2.73M12 21L9.27 14.73M16.36 7.64l-2.12 6.36M7.76 16.36l2.12-6.36"/>
    </svg>
    <div style="font-size:20px;font-weight:700;">Luna Agent</div>
    <div style="font-size:13px;color:#9090a0;">Desktop App</div>
    <div id="status" style="font-size:12px;color:#555568;">Initializing...</div>
  </div>
`;

// Load the full app
loadLuna();
