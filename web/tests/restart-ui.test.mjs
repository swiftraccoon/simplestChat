import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { evaluateTypeScript } from './source-loader.mjs';
import { createDOM } from './ui-fixture.mjs';

async function fixture() {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const start = source.indexOf('function showActionToast(');
  const end = source.indexOf('// --- Moderation Context Menu ---', start);
  assert.ok(start >= 0 && end > start, 'exercise the production action-notice implementation');
  const { document } = createDOM();
  const toastContainer = document.createElement('div');
  document.body.append(toastContainer);
  const timers = [];
  const { showActionToast } = evaluateTypeScript(
    `${source.slice(start, end)}\nexport { showActionToast };`,
    {
      globals: {
        document,
        toastContainer,
        setTimeout: (callback, duration) => timers.push({ callback, duration }),
      },
    },
  );
  return { showActionToast, toastContainer, timers };
}

test('persistent recovery actions remain until used and never render server text as HTML', async () => {
  const f = await fixture();
  let retries = 0;
  const notice = f.showActionToast(
    '<untrusted restart reason>',
    [{ label: 'Retry connection', action: () => retries++ }],
    0,
  );
  assert.equal(notice.isConnected, true);
  assert.equal(notice.children[0].textContent, '<untrusted restart reason>');
  assert.deepEqual(f.timers, []);
  notice.querySelector('button').click();
  assert.equal(retries, 1);
  assert.equal(notice.isConnected, false);
});

test('ordinary action notices retain their bounded controller-owned lifetime', async () => {
  const f = await fixture();
  const notice = f.showActionToast('Ordinary action', [], 15000);
  assert.equal(f.timers.length, 1);
  assert.equal(f.timers[0].duration, 15000);
  f.timers[0].callback();
  assert.equal(notice.isConnected, false);
});

test('action CSS does not hide a persistent retry before its controller dismisses it', async () => {
  const source = await readFile(new URL('../src/style.css', import.meta.url), 'utf8');
  const declaration = source.match(/\.toast-action\s*\{([^}]+)\}/)?.[1];
  assert.ok(declaration);
  assert.match(declaration, /animation:\s*toastIn\s+200ms\s+ease;/);
  assert.doesNotMatch(declaration, /toastOut/);
});
