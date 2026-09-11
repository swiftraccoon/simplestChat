import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM } from './ui-fixture.mjs';

async function fixture(selected = 0) {
  const { document, Node } = createDOM();
  let focused = null;
  Node.prototype.focus = function () {
    focused = this;
  };
  Node.prototype.dispatchEvent = function (event) {
    this.emit(event.type, { detail: event.detail });
    return true;
  };
  Node.prototype.querySelectorAll = function (selector) {
    const matches = (node) =>
      selector === '[role="tabpanel"]'
        ? node.getAttribute('role') === 'tabpanel'
        : selector === '[data-settings-tab]'
          ? node.getAttribute('data-settings-tab') !== null
          : selector === '[data-dialog-close]'
            ? node.getAttribute('data-dialog-close') !== null
            : selector === '.settings-dialog-body' && node.className === 'settings-dialog-body';
    return this.children.flatMap((child) => [
      ...(matches(child) ? [child] : []),
      ...child.querySelectorAll(selector),
    ]);
  };
  const dialog = document.createElement('dialog');
  const nav = document.createElement('div');
  const body = document.createElement('div');
  body.className = 'settings-dialog-body';
  body.scrollTop = 42;
  const tabs = [],
    panels = [],
    changes = [];
  for (let index = 0; index < 3; index++) {
    const tab = document.createElement('button');
    tab.setAttribute('data-settings-tab', '');
    tab.setAttribute('aria-controls', `panel-${index}`);
    tab.setAttribute('aria-selected', String(selected === index));
    const panel = document.createElement('section');
    panel.id = `panel-${index}`;
    panel.setAttribute('role', 'tabpanel');
    nav.append(tab);
    body.append(panel);
    tabs.push(tab);
    panels.push(panel);
  }
  const closeButtons = [document.createElement('button'), document.createElement('button')];
  for (const button of closeButtons) button.setAttribute('data-dialog-close', '');
  dialog.append(nav, body, ...closeButtons);
  document.body.append(dialog);
  dialog.addEventListener('settings-tab-change', (event) => changes.push(event.detail));
  const api = await loadTypeScript('src/settings-dialog.ts', { globals: { CustomEvent } });
  const key = (tab, value, extras = {}) => {
    let prevented = false;
    tab.emit('keydown', {
      key: value,
      preventDefault() {
        prevented = true;
      },
      ...extras,
    });
    return prevented;
  };
  return { ...api, dialog, tabs, panels, body, closeButtons, changes, key, focused: () => focused };
}

function assertSelected(fixture, selected) {
  assert.deepEqual(
    fixture.tabs.map((tab) => tab.getAttribute('aria-selected')),
    fixture.tabs.map((_, index) => String(index === selected)),
  );
  assert.deepEqual(
    fixture.tabs.map((tab) => tab.tabIndex),
    fixture.tabs.map((_, index) => (index === selected ? 0 : -1)),
  );
  assert.deepEqual(
    fixture.panels.map((panel) => panel.hidden),
    fixture.panels.map((_, index) => index !== selected),
  );
}

test('settings initialize the selected tab without opening the dialog or taking focus', async () => {
  const f = await fixture(1);
  f.configureSettingsDialog(f.dialog);
  assertSelected(f, 1);
  assert.equal(f.dialog.open, false);
  assert.equal(f.focused(), null);
  assert.deepEqual(f.changes, ['panel-1']);
});

test('click activates one panel, resets scrolling, and emits only actual tab changes', async () => {
  const f = await fixture();
  f.configureSettingsDialog(f.dialog);
  f.body.scrollTop = 200;
  f.tabs[2].click();
  assertSelected(f, 2);
  assert.equal(f.focused(), f.tabs[2]);
  assert.equal(f.body.scrollTop, 0);
  f.tabs[2].click();
  assert.deepEqual(f.changes, ['panel-0', 'panel-2']);
});

test('arrow keys wrap and Home/End activate and focus the correct tab', async () => {
  const f = await fixture();
  f.configureSettingsDialog(f.dialog);
  for (const [from, key, to] of [
    [0, 'ArrowLeft', 2],
    [2, 'ArrowRight', 0],
    [0, 'End', 2],
    [2, 'Home', 0],
    [0, 'ArrowRight', 1],
  ]) {
    assert.equal(f.key(f.tabs[from], key), true);
    assertSelected(f, to);
    assert.equal(f.focused(), f.tabs[to]);
  }
  assert.equal(f.key(f.tabs[1], 'Tab'), false);
  assert.equal(f.key(f.tabs[1], 'ArrowRight', { ctrlKey: true }), false);
  assertSelected(f, 1);
});

test('keyboard navigation skips unavailable tabs', async () => {
  const f = await fixture();
  f.tabs[1].disabled = true;
  f.configureSettingsDialog(f.dialog);
  f.key(f.tabs[0], 'ArrowRight');
  assertSelected(f, 2);
  f.tabs[1].disabled = false;
  f.tabs[1].hidden = true;
  f.key(f.tabs[2], 'ArrowLeft');
  assertSelected(f, 0);
});

test('both close controls and Escape use caller cleanup exactly once after repeated configuration', async () => {
  const f = await fixture();
  let dismissals = 0,
    prevented = false;
  const dismiss = () => {
    dismissals++;
  };
  f.configureSettingsDialog(f.dialog, dismiss);
  f.configureSettingsDialog(f.dialog, dismiss);
  f.dialog.showModal();
  f.closeButtons[0].click();
  f.closeButtons[1].click();
  f.dialog.emit('cancel', {
    preventDefault() {
      prevented = true;
    },
  });
  assert.equal(dismissals, 3);
  assert.equal(prevented, true);
  assert.equal(f.dialog.open, true, 'the caller controls when cleanup permits closing');
  assert.deepEqual(f.changes, ['panel-0']);
});

test('backdrop closes only for clicks outside the dialog rectangle, never inside content', async () => {
  const f = await fixture();
  f.configureSettingsDialog(f.dialog);
  f.dialog.showModal();
  for (const event of [
    { clientX: 100, clientY: 100 },
    { clientX: 10, clientY: 10 },
    { clientX: 0, clientY: 0, target: f.body },
  ]) {
    f.dialog.emit('click', event);
    assert.equal(f.dialog.open, true);
  }
  f.dialog.emit('click', { clientX: 0, clientY: 100 });
  assert.equal(f.dialog.open, false);
});

test('tab state and callbacks stay scoped to each dialog', async () => {
  const first = await fixture(),
    second = await fixture();
  first.configureSettingsDialog(first.dialog);
  second.configureSettingsDialog(second.dialog);
  second.tabs[2].click();
  assertSelected(first, 0);
  assertSelected(second, 2);
  assert.deepEqual(first.changes, ['panel-0']);
});

test('room settings retain every existing control with semantic panels and no submit side effects', async () => {
  const html = await readFile(new URL('../index.html', import.meta.url), 'utf8');
  const dialog = html.match(/<dialog\b[^>]*\bid="room-settings-modal"[\s\S]*?<\/dialog>/)?.[0];
  assert.ok(dialog);
  assert.match(dialog, /aria-labelledby="room-settings-title"/);
  assert.match(dialog, /id="room-settings-close"[^>]*data-dialog-close[^>]*aria-label=/);
  assert.match(dialog, /Changes apply immediately/);
  const expected = [
    'topic',
    'password',
    'guests',
    'guests-broadcast',
    'require-reg',
    'invite-only',
    'secret',
    'lobby',
    'video',
    'chat',
    'screen',
    'ptt',
    'max-broadcasters',
    'max-participants',
    'moderated',
  ]
    .map((id) => `rs-${id}`)
    .sort();
  assert.deepEqual(
    [...dialog.matchAll(/id="(rs-[^"]+)"/g)].map((match) => match[1]).sort(),
    expected,
  );
  for (const section of ['basics', 'access', 'participation']) {
    assert.match(dialog, new RegExp(`role="tab"[^>]*aria-controls="room-settings-${section}"`));
    assert.match(
      dialog,
      new RegExp(
        `id="room-settings-${section}"\\s+role="tabpanel"\\s+aria-labelledby="room-settings-${section}-tab"`,
      ),
    );
  }
  for (const button of dialog.matchAll(/<button\b[^>]*>/g))
    assert.match(button[0], /type="button"/);
  assert.doesNotMatch(dialog, /0 = unlimited|class="modal-overlay"/);
});
