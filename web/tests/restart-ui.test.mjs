import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { createDOM } from './ui-fixture.mjs';

async function fixture() {
  const { document } = createDOM();
  const toastContainer = document.createElement('div');
  document.body.append(toastContainer);
  const timers = [];
  const { createToasts } = await loadTypeScript('src/toasts.ts', {
    globals: { setTimeout: (callback, duration) => timers.push({ callback, duration }) },
  });
  const { showActionToast } = createToasts(document, toastContainer);
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
  const source = (
    await Promise.all(
      (await readdir(new URL('../src/', import.meta.url)))
        .filter((name) => name.endsWith('.css'))
        .map((name) => readFile(new URL(`../src/${name}`, import.meta.url), 'utf8')),
    )
  ).join('\n');
  const declaration = source.match(/\.toast-action\s*\{([^}]+)\}/)?.[1];
  assert.ok(declaration);
  assert.match(declaration, /animation:\s*toastIn\s+200ms\s+ease;/);
  assert.doesNotMatch(declaration, /toastOut/);
});
