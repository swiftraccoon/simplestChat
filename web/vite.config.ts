import { defineConfig } from 'vite';

export default defineConfig({
  server: {
    proxy: {
      '/ws': {
        target: 'ws://127.0.0.1:3000',
        ws: true,
      },
      '/api': {
        target: 'http://127.0.0.1:3000',
      },
    },
  },
  build: {
    outDir: 'dist',
    // Preserve the previous Vite 6 output target across the Vite 8 upgrade.
    target: ['chrome87', 'edge88', 'firefox78', 'safari14'],
  },
});
