import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

test('development serves auth and room APIs alongside WebSocket signaling', async () => {
  const { default: config } = await loadTypeScript('vite.config.ts', {
    modules: { vite: { defineConfig: value => value } },
  });
  assert.deepEqual(JSON.parse(JSON.stringify(config.server.proxy)), {
    '/ws': { target: 'ws://127.0.0.1:3000', ws: true },
    '/api': { target: 'http://127.0.0.1:3000' },
  });
});
