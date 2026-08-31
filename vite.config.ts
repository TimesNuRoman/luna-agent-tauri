import { defineConfig } from 'vite';
import { resolve } from 'path';
import { readFileSync, copyFileSync, mkdirSync } from 'fs';

export default defineConfig({
  root: 'src-tauri',
  base: './',
  build: {
    outDir: '../dist',
    emptyOutDir: true,
    rollupOptions: {
      // Inject Luna Agent HTML directly into the page
      plugins: [
        {
          name: 'luna-agent-html-injector',
          transformIndexHtml(html) {
            // Read the Luna Agent HTML
            const lunaHtml = readFileSync(
              resolve(__dirname, '../luna-agent/index.html'),
              'utf-8'
            );
            // Inject Tauri detection script
            const tauriScript = `<script>
window.__TAURI__ = typeof window.__TAURI__ !== 'undefined';
window.__LUNA_TAURI__ = {
  async invoke(cmd, args) {
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      return await window.__TAURI__.core.invoke(cmd, args);
    }
    throw new Error('Not in Tauri');
  }
};
</script>`;
            // Inject before </head>
            const modifiedHtml = html.replace('</head>', tauriScript + '</head>');
            // Replace the loading div with the actual app
            return modifiedHtml;
          },
        },
      ],
    },
  },
  resolve: {
    alias: {
      '@tauri-apps/api': resolve(__dirname, 'src-tauri/mock-api.js'),
    },
  },
});
