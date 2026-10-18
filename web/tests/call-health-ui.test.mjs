import assert from 'node:assert/strict';
import test from 'node:test';
import { createDOM } from './ui-fixture.mjs';
import { loadTypeScript } from './source-loader.mjs';

test('Call health updates retain controls and invoke repair only on explicit actions', async () => {
  const dom = createDOM();
  const calls = [];
  let canRefresh = true;
  const ui = {
    el(tag, text, className) {
      const node = dom.document.createElement(tag);
      if (text) node.textContent = text;
      if (className) node.className = className;
      return node;
    },
    button(label, action, className) {
      const node = ui.el('button', label, className);
      node.addEventListener('click', action);
      return node;
    },
  };
  const { mountCallHealth } = await loadTypeScript('src/call-health-ui.ts', {
    modules: { './ui': ui },
  });
  const host = dom.document.createElement('div');
  dom.document.body.append(host);
  const update = mountCallHealth(host, {
    retryPlayback: () => calls.push('retryPlayback'),
    speaker: () => calls.push('speaker'),
    capture: () => calls.push('capture'),
    refreshIncoming: () => calls.push('refreshIncoming'),
    canRefresh: () => canRefresh,
  });
  const controls = dom.created.filter((node) => node.tagName === 'BUTTON');
  const snapshot = {
    sending: { level: 'idle', label: 'Microphone and camera off', detail: 'Nothing is sent.' },
    receiving: { level: 'problem', label: 'Incoming media unavailable', detail: 'Refresh it.' },
    playback: { level: 'unknown', label: 'Checking', detail: 'Not measured yet.' },
    level: 'problem',
  };
  update(snapshot);
  assert.deepEqual(calls, []);
  assert.match(host.textContent, /Sending.*Receiving.*Playback/);
  assert.match(host.textContent, /reliability sharing off/);
  assert.equal(controls.length, 4);
  assert.equal(controls[0].textContent, 'Refresh incoming media');
  controls[1].click();
  assert.deepEqual(calls, ['retryPlayback']);
  canRefresh = false;
  update({ ...snapshot, receiving: { ...snapshot.receiving, label: 'Refreshing incoming media' } });
  assert.equal(controls[0].disabled, true);
  assert.deepEqual(
    dom.created.filter((node) => node.tagName === 'BUTTON'),
    controls,
    'refreshing status must not replace the focused controls',
  );
  controls[2].click();
  controls[3].click();
  assert.deepEqual(calls, ['retryPlayback', 'speaker', 'capture']);
});
