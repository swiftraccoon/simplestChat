import assert from 'node:assert/strict';
import test from 'node:test';
import { createDOM } from './ui-fixture.mjs';
import { loadTypeScript } from './native-fixtures.mjs';
import { roomScreenFixture } from './room-screen-fixture.mjs';

test('update control remains actionable after an in-call refusal and never reloads automatically', async () => {
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
  const { createAppUpdateNotice } = await loadTypeScript('src/app-updates-ui.ts', {
    modules: {
      './ui': {
        el,
        button(label, action) {
          const node = el('button', label);
          node.addEventListener('click', action);
          return node;
        },
      },
    },
  });
  const { renderAppUpdateAvailable } = createAppUpdateNotice(toastContainer, () => {
    if (inCall) return false;
    reloads++;
    return true;
  });
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

test('legacy absolute bandwidth qualities can request evidence but cannot color the local health indicator', async () => {
  const f = await roomScreenFixture();
  await f.connect();
  let samples = 0;
  f.services.callHealth.sample = () => samples++;
  f.state.events.onConnectionQuality('good');
  f.state.events.onConnectionQuality('poor');
  assert.equal(samples, 2);
  f.controller.dispose();
});
