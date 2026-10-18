import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from '@typescript/typescript6';
import { createDOM } from './ui-fixture.mjs';
import { evaluateTypeScript } from './source-loader.mjs';

const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
function declaration(name) {
  const value = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(value);
  return value.getText(ast);
}

test('update control remains actionable after an in-call refusal and never reloads automatically', () => {
  const dom = createDOM();
  let inCall = true;
  let reloads = 0;
  const el = (tag, text, className) => {
    const node = dom.document.createElement(tag);
    if (text) node.textContent = text;
    if (className) node.className = className;
    return node;
  };
  const toastContainer = el('div');
  dom.document.body.append(toastContainer);
  const { renderAppUpdateAvailable } = evaluateTypeScript(
    `let appUpdateNotice = null; ${declaration('renderAppUpdateAvailable')}; export { renderAppUpdateAvailable };`,
    {
      globals: {
        el,
        toastContainer,
        button(label, action) {
          const node = el('button', label);
          node.addEventListener('click', action);
          return node;
        },
        appUpdates: {
          reloadWhenIdle() {
            if (inCall) return false;
            reloads++;
            return true;
          },
        },
      },
    },
  );
  renderAppUpdateAvailable(true);
  const notice = toastContainer.firstChild;
  const action = notice.querySelector('button');
  assert.equal(reloads, 0);
  action.click();
  assert.match(notice.textContent, /Leave the call when you are ready/);
  assert.equal(notice.isConnected, true, 'blocked update keeps the persistent control');
  renderAppUpdateAvailable(true);
  assert.equal(toastContainer.children.length, 1);
  inCall = false;
  assert.equal(reloads, 0, 'leaving alone cannot reload');
  action.click();
  assert.equal(reloads, 1);
  renderAppUpdateAvailable(false);
  assert.equal(toastContainer.children.length, 0);
});

test('legacy absolute bandwidth qualities can request evidence but cannot color the local health indicator', () => {
  let samples = 0;
  const { renderConnectionQuality } = evaluateTypeScript(
    `${declaration('renderConnectionQuality')}; export { renderConnectionQuality };`,
    { globals: { callHealth: { sample: () => samples++ } } },
  );
  renderConnectionQuality('good');
  renderConnectionQuality('poor');
  assert.equal(samples, 2);
});
