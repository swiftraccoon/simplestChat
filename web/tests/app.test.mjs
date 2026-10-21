import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';

test('importing the app does not require browser globals or mount its shell', async () => {
  assert.equal('document' in globalThis, false);
  assert.equal('window' in globalThis, false);
  const { startApp } = await loadTypeScript('src/app.ts');
  assert.equal(typeof startApp, 'function');
  assert.equal('document' in globalThis, false);
  assert.equal('window' in globalThis, false);
});
