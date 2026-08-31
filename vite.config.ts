import { defineConfig } from 'vite';

export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: ['esnext', 'chrome100', 'safari15'],
    minify: 'esbuild',
    outDir: 'dist',
    assetsDir: 'assets',
  },
  appType: 'custom',
});