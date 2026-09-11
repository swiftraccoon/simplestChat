import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadContractModules } from './source-loader.mjs';
import { createDOM, flush } from './ui-fixture.mjs';

async function functionSource(name) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(declaration, `${name} must remain a shared UI action`);
  return declaration.getText(ast);
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

for (const scenario of ['cancel', 'leave', 'save']) {
  test(`camera setup ${scenario} honors the current room before publishing`, async () => {
    const pending = deferred();
    let publishes = 0,
      opens = 0;
    const activeRoom = {
      hasMedia: true,
      videoEnabled: false,
      async toggleVideo() {
        publishes++;
        return true;
      },
    };
    const api = evaluateTypeScript(
      `
      let room = initialRoom;
      let cameraTogglePending = false;
      ${await functionSource('toggleCamera')}
      export { toggleCamera };
      export function leave() { room = null; }
    `,
      {
        globals: {
          initialRoom: activeRoom,
          mediaControls: {
            hasConfiguredSetup: false,
            openSetup() {
              opens++;
              return pending.promise;
            },
          },
          canStartBroadcast() {
            return true;
          },
          updateCamButton() {},
          updateLocalTile() {},
          showToast() {},
        },
      },
    );
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
  const api = evaluateTypeScript(
    `
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
  `,
    {
      globals: {
        initialRoom: {
          hasMedia: true,
          roomSettings: { pushToTalk: true },
          audioEnabled: true,
          muteAudio() {
            muted++;
            this.audioEnabled = false;
          },
        },
        localStorage: { setItem: (...args) => saved.push(args) },
        micModeSelect: { value: 'open' },
        updateMicButton() {},
        updateLocalTile() {},
        showToast() {},
      },
    },
  );
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
  class Element {
    constructor(interactive = false) {
      this.interactive = interactive;
    }
    closest() {
      return this.interactive ? {} : null;
    }
  }
  const api = evaluateTypeScript(
    `${await functionSource('shortcutsBlocked')} export { shortcutsBlocked };`,
    {
      globals: {
        room: {},
        roomScreen: { hidden: false },
        roomRecovering: false,
        document: {
          querySelector() {
            return modal;
          },
        },
        Element,
      },
    },
  );
  const basic = {
    target: new Element(),
    defaultPrevented: false,
    repeat: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
  };
  assert.equal(api.shortcutsBlocked(basic), false);
  for (const flag of ['defaultPrevented', 'repeat', 'ctrlKey', 'metaKey', 'altKey', 'shiftKey']) {
    assert.equal(api.shortcutsBlocked({ ...basic, [flag]: true }), true);
  }
  assert.equal(api.shortcutsBlocked({ ...basic, target: new Element(true) }), true);
  modal = {};
  assert.equal(api.shortcutsBlocked(basic), true);
});

async function captureStoppedUiFixture(initialRoom, mode = 'open', held = false) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  let roomEvents;
  function visit(node) {
    if (ts.isNewExpression(node) && node.expression.getText(ast) === 'RoomClient')
      roomEvents = node.arguments[1];
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.ok(roomEvents && ts.isObjectLiteralExpression(roomEvents));
  const eventSource = ['onLocalMediaChanged', 'onLocalCaptureStopped']
    .map((name) => {
      const property = roomEvents.properties.find((node) => node.name?.getText(ast) === name);
      assert.ok(property, `${name} must be wired to the room`);
      return property.getText(ast);
    })
    .join(',');
  const keyboardSource = ast.statements
    .filter(
      (node) =>
        ts.isExpressionStatement(node) &&
        ["document.addEventListener('keydown'", "document.addEventListener('keyup'"].some(
          (prefix) => node.getText(ast).startsWith(prefix),
        ),
    )
    .map((node) => node.getText(ast))
    .join('\n');
  const functions = await Promise.all(
    [
      'handleLocalCaptureStopped',
      'pttActivate',
      'pttDeactivate',
      'shortcutsBlocked',
      'observeUiTask',
    ].map(functionSource),
  );
  const updates = { mic: [], camera: [], screen: [], tiles: 0, toasts: [] };
  const listeners = new Map();
  class Element {
    closest() {
      return null;
    }
  }
  const api = evaluateTypeScript(
    `
    let room = initialRoom;
    let micMode = initialMode;
    let pttHeld = initialHeld;
    let pttActivation = 0;
    ${functions.join('\n')}
    ${keyboardSource}
    export const events = { ${eventSource} };
    export function state() { return { pttHeld, pttActivation }; }
    export function leave() { room = null; }
  `,
    {
      globals: {
        initialRoom,
        initialMode: mode,
        initialHeld: held,
        Element,
        auth: { userId: null },
        roomScreen: { hidden: false },
        roomRecovering: false,
        document: {
          querySelector() {
            return null;
          },
          addEventListener(type, callback) {
            listeners.set(type, callback);
          },
        },
        canStartBroadcast() {
          return true;
        },
        updateMicButton(value) {
          updates.mic.push(value);
        },
        updateCamButton(value) {
          updates.camera.push(value);
        },
        updateScreenButton(value) {
          updates.screen.push(value);
        },
        updateLocalTile() {
          updates.tiles++;
        },
        showToast(message) {
          updates.toasts.push(message);
        },
      },
    },
  );
  assert.equal(listeners.size, 2);
  return {
    ...api,
    updates,
    stop(kind) {
      api.events.onLocalMediaChanged();
      api.events.onLocalCaptureStopped(kind);
    },
    key(type, key, repeat = false) {
      listeners.get(type)({
        key,
        repeat,
        target: new Element(),
        preventDefault() {
          this.defaultPrevented = true;
        },
      });
    },
  };
}

test('stopped open microphone refreshes controls and requests explicit restart without touching camera or screen', async () => {
  const activeRoom = {
    hasMedia: true,
    audioEnabled: false,
    videoEnabled: true,
    isScreenSharing: true,
  };
  const api = await captureStoppedUiFixture(activeRoom);
  api.stop('audio');
  assert.deepEqual(api.updates, {
    mic: [false],
    camera: [true],
    screen: [true],
    tiles: 1,
    toasts: ['Microphone stopped. Click Unmute (M) to restart.'],
  });
  assert.deepEqual(api.state(), { pttHeld: false, pttActivation: 1 });
  assert.equal(activeRoom.videoEnabled, true);
  assert.equal(activeRoom.isScreenSharing, true);
});

for (const replacement of ['none', 'room', 'account']) {
  test(`detached UI failures retain diagnostics without notifying a replaced ${replacement} context`, async () => {
    const pending = deferred();
    const notifications = [],
      errors = [];
    const auth = { userId: 'account-a' };
    const api = evaluateTypeScript(
      `
      let room = {};
      ${await functionSource('observeUiTask')}
      export { observeUiTask };
      export function replaceRoom() { room = {}; }
    `,
      {
        globals: {
          auth,
          showToast: (message) => notifications.push(message),
          console: { error: (...details) => errors.push(details) },
        },
      },
    );
    api.observeUiTask(pending.promise, 'Could not update the camera');
    if (replacement === 'room') api.replaceRoom();
    if (replacement === 'account') auth.userId = 'account-b';
    const failure = new Error('owned fixture failure');
    pending.reject(failure);
    await flush();
    assert.deepEqual(
      errors,
      [['Could not update the camera']],
      'native failure details are not logged',
    );
    assert.deepEqual(notifications, replacement === 'none' ? ['Could not update the camera'] : []);
  });
}

test('async DOM action observes synchronous and asynchronous failures without deferring the user action', async () => {
  const observed = [];
  const api = evaluateTypeScript(
    `${await functionSource('asyncUiAction')} export { asyncUiAction };`,
    {
      globals: {
        observeUiTask: (task, message) => {
          observed.push({ task, message });
        },
      },
    },
  );
  let calls = 0;
  const syncFailure = new Error('synchronous fixture failure');
  const syncAction = api.asyncUiAction(() => {
    calls++;
    throw syncFailure;
  }, 'Action failed');
  assert.equal(syncAction(), undefined);
  assert.equal(calls, 1, 'device/clipboard action must run during the original user gesture');
  assert.equal(observed[0].message, 'Action failed');
  await assert.rejects(observed[0].task, (error) => error === syncFailure);
  const asyncFailure = new Error('asynchronous fixture failure');
  const asyncAction = api.asyncUiAction(async () => {
    throw asyncFailure;
  }, 'Other action failed');
  assert.equal(asyncAction(), undefined);
  await assert.rejects(observed[1].task, (error) => error === asyncFailure);
});

test('room directory validates response shapes before rendering untrusted fields', async () => {
  const { decodeRoomDirectory } = (await loadContractModules())['./api-validation'];
  const entry = {
    id: 'room',
    display_name: 'Room',
    topic: null,
    participant_count: 0,
    password_protected: false,
    moderated: false,
    broadcaster_count: 0,
    description: '',
    image_url: null,
    secret: false,
  };
  assert.deepEqual(decodeRoomDirectory([entry]), [entry]);
  for (const value of [
    null,
    {},
    [null],
    [{ ...entry, participant_count: -1 }],
    [{ ...entry, participant_count: NaN }],
    [{ ...entry, display_name: {} }],
    [{ ...entry, topic: false }],
    [{ ...entry, image_url: [] }],
    [{ ...entry, broadcaster_count: 1.5 }],
    [{ ...entry, description: null }],
  ]) {
    assert.throws(() => decodeRoomDirectory(value), /Invalid response data/);
  }
});

test('stopped camera preserves held microphone intent and reports explicit camera restart', async () => {
  const activeRoom = {
    hasMedia: true,
    audioEnabled: true,
    videoEnabled: false,
    isScreenSharing: true,
  };
  const api = await captureStoppedUiFixture(activeRoom, 'ptt', true);
  api.stop('video');
  assert.deepEqual(api.updates, {
    mic: [true],
    camera: [false],
    screen: [true],
    tiles: 1,
    toasts: ['Camera stopped. Click Cam On (V) to restart.'],
  });
  assert.deepEqual(api.state(), { pttHeld: true, pttActivation: 0 });
  assert.equal(activeRoom.audioEnabled, true);
  assert.equal(activeRoom.isScreenSharing, true);
});

for (const key of [' ', 't']) {
  test(`stopped PTT ignores pending activation and repeated ${JSON.stringify(key)} until a fresh hold`, async () => {
    const pending = deferred();
    let activations = 0,
      mutes = 0;
    const activeRoom = {
      hasMedia: true,
      audioEnabled: false,
      videoEnabled: true,
      isScreenSharing: false,
      unmuteAudio() {
        activations++;
        return activations === 1 ? pending.promise : Promise.resolve();
      },
      muteAudio() {
        mutes++;
      },
    };
    const api = await captureStoppedUiFixture(activeRoom, 'ptt');
    api.key('keydown', key);
    assert.equal(activations, 1);
    assert.equal(api.state().pttHeld, true);
    api.stop('audio');
    assert.deepEqual(api.state(), { pttHeld: false, pttActivation: 2 });
    assert.deepEqual(api.updates.toasts, [
      'Microphone stopped. Release, then hold Space/T or the microphone button again to restart.',
    ]);
    api.key('keydown', key, true);
    pending.resolve();
    await flush();
    assert.equal(activations, 1, 'a repeated held key must not reopen stopped capture');
    assert.equal(
      api.updates.tiles,
      1,
      'late activation completion cannot refresh the retired intent',
    );
    assert.equal(api.state().pttHeld, false);
    assert.equal(mutes, 0, 'capture stop must not issue new media commands');
    api.key('keyup', key);
    api.key('keydown', key);
    await flush();
    assert.equal(activations, 2, 'an explicit new hold may request capture again');
    assert.equal(api.state().pttHeld, true);
    assert.equal(activeRoom.videoEnabled, true);
    api.key('keyup', key);
    assert.equal(mutes, 1);
    assert.equal(api.state().pttHeld, false);
  });
}

test('capture-stop UI ignores notifications after leaving', async () => {
  const api = await captureStoppedUiFixture({ hasMedia: true }, 'ptt');
  api.leave();
  api.stop('audio');
  api.stop('video');
  assert.deepEqual(api.updates, { mic: [], camera: [], screen: [], tiles: 0, toasts: [] });
  assert.deepEqual(api.state(), { pttHeld: false, pttActivation: 0 });
});

test('scroll-button icon initialization preserves the unread badge for SocialChat startup', async () => {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const initialization = ast.statements
    .filter(
      (node) => ts.isExpressionStatement(node) && node.getText(ast).startsWith('scrollBottomBtn.'),
    )
    .map((node) => node.getText(ast))
    .join('\n');
  assert.ok(initialization.includes('scrollBottomBtn.textContent'));
  const { document } = createDOM();
  const scrollBottomBtn = document.createElement('button');
  scrollBottomBtn.id = 'scroll-bottom-btn';
  const unreadBadge = document.createElement('span');
  unreadBadge.id = 'unread-badge';
  scrollBottomBtn.append(unreadBadge);
  document.body.append(scrollBottomBtn);
  scrollBottomBtn.insertAdjacentHTML = () => {};
  evaluateTypeScript(initialization, {
    globals: { scrollBottomBtn, unreadBadge, icons: { scrollDown: () => '<svg></svg>' } },
  });
  assert.equal(document.getElementById('unread-badge'), unreadBadge);
  assert.equal(unreadBadge.parentNode, scrollBottomBtn);
});
