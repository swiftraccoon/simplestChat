import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { evaluateTypeScript } from './source-loader.mjs';
import { createDOM } from './ui-fixture.mjs';

async function functionSource(name) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
  assert.ok(declaration, `${name} must remain a shared UI action`);
  return declaration.getText(ast);
}

function deferred() {
  let resolve;
  const promise = new Promise(yes => { resolve = yes; });
  return { promise, resolve };
}

for (const scenario of ['cancel', 'leave', 'save']) {
  test(`camera setup ${scenario} honors the current room before publishing`, async () => {
    const pending = deferred();
    let publishes = 0, opens = 0;
    const activeRoom = { hasMedia: true, videoEnabled: false, async toggleVideo() { publishes++; return true; } };
    const api = evaluateTypeScript(`
      let room = initialRoom;
      let cameraTogglePending = false;
      ${await functionSource('toggleCamera')}
      export { toggleCamera };
      export function leave() { room = null; }
    `, { globals: {
      initialRoom: activeRoom,
      mediaControls: { hasConfiguredSetup: false, openSetup() { opens++; return pending.promise; } },
      canStartBroadcast() { return true; },
      updateCamButton() {}, updateLocalTile() {}, showToast() {},
    } });
    const activation = api.toggleCamera();
    await api.toggleCamera();
    assert.equal(opens, 1, 'repeated activation must not open another dialog');
    assert.equal(publishes, 0);
    if (scenario === 'leave') api.leave();
    pending.resolve(scenario !== 'cancel');
    await activation;
    assert.equal(publishes, scenario === 'save' ? 1 : 0);
  });
}

test('room-required PTT does not overwrite the personal microphone mode', async () => {
  const saved = [];
  let muted = 0;
  const api = evaluateTypeScript(`
    type MicMode = 'open' | 'ptt';
    let personalMicMode: MicMode = 'open';
    let micMode: MicMode = 'open';
    let pttHeld = false;
    let pttActivation = 0;
    let room = initialRoom;
    ${await functionSource('setMicMode')}
    export { setMicMode };
    export function modes() { return [micMode, personalMicMode]; }
    export function leave() { room = null; setMicMode(personalMicMode, false); }
  `, { globals: {
    initialRoom: { hasMedia: true, roomSettings: { pushToTalk: true }, audioEnabled: true, muteAudio() { muted++; this.audioEnabled = false; } },
    localStorage: { setItem: (...args) => saved.push(args) },
    micModeSelect: { value: 'open' }, updateMicButton() {}, updateLocalTile() {}, showToast() {},
  } });
  api.setMicMode('ptt', false);
  assert.deepEqual(api.modes(), ['ptt', 'open']);
  assert.equal(muted, 1);
  api.setMicMode('open');
  assert.deepEqual(api.modes(), ['ptt', 'open']);
  assert.equal(saved.length, 0);
  api.leave();
  assert.deepEqual(api.modes(), ['open', 'open']);
});

test('media shortcuts are blocked in dialogs, interactive controls, and modified/repeated key presses', async () => {
  let modal = null;
  class Element { constructor(interactive = false) { this.interactive = interactive; } closest() { return this.interactive ? {} : null; } }
  const api = evaluateTypeScript(`${await functionSource('shortcutsBlocked')} export { shortcutsBlocked };`, { globals: {
    room: {}, roomScreen: { hidden: false }, roomRecovering: false,
    document: { querySelector() { return modal; } }, Element,
  } });
  const basic = { target: new Element(), defaultPrevented: false, repeat: false, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false };
  assert.equal(api.shortcutsBlocked(basic), false);
  for (const flag of ['defaultPrevented', 'repeat', 'ctrlKey', 'metaKey', 'altKey', 'shiftKey']) {
    assert.equal(api.shortcutsBlocked({ ...basic, [flag]: true }), true);
  }
  assert.equal(api.shortcutsBlocked({ ...basic, target: new Element(true) }), true);
  modal = {};
  assert.equal(api.shortcutsBlocked(basic), true);
});

test('scroll-button icon initialization preserves the unread badge for SocialChat startup', async () => {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const initialization = ast.statements.filter(node => ts.isExpressionStatement(node) &&
    node.getText(ast).startsWith('scrollBottomBtn.')).map(node => node.getText(ast)).join('\n');
  assert.ok(initialization.includes('scrollBottomBtn.textContent'));
  const { document } = createDOM();
  const scrollBottomBtn = document.createElement('button'); scrollBottomBtn.id = 'scroll-bottom-btn';
  const unreadBadge = document.createElement('span'); unreadBadge.id = 'unread-badge';
  scrollBottomBtn.append(unreadBadge); document.body.append(scrollBottomBtn);
  scrollBottomBtn.insertAdjacentHTML = () => {};
  evaluateTypeScript(initialization, { globals: { scrollBottomBtn, unreadBadge, icons: { scrollDown: () => '<svg></svg>' } } });
  assert.equal(document.getElementById('unread-badge'), unreadBadge);
  assert.equal(unreadBadge.parentNode, scrollBottomBtn);
});
