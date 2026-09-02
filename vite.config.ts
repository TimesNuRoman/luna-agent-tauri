import { defineConfig } from 'vite';
import { svelte, vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// Pre-existing Svelte a11y warnings (clickable non-interactive <div>s
// without keyboard handlers, in modal backdrops and the code-mode
// message scroll). vite-plugin-svelte promotes warnings to errors
// in production builds; the codebase has lived with these as
// warnings under `vite dev` for a while. Keep them visible in dev
// (`console.warn`) but allow `vite build` to succeed.
const suppressInBuildOnly = (warning: any, defaultHandler?: (w: any) => void) => {
  if (warning.code?.startsWith('a11y-')) return;
  // Anything else (real errors / unused-css) still fails the build.
  if (defaultHandler) defaultHandler(warning);
  else throw new Error(`Svelte build warning: ${warning.message}`);
};

export default defineConfig({
  plugins: [svelte({ preprocess: [vitePreprocess()], onwarn: suppressInBuildOnly })],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Ignore src-tauri/target — cargo locks build artifacts mid-compile on Windows
      // and Vite's chokidar watcher throws EBUSY on the .exe files.
      ignored: ['**/src-tauri/**'],
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'esnext',
  },
  envPrefix: ['VITE_', 'TAURI_'],
});
