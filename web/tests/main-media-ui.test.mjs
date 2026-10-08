import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadContractModules, loadTypeScript } from './source-loader.mjs';
import { createDOM, flush } from './ui-fixture.mjs';

const { mediaErrorMessage } = await loadTypeScript('src/media-controls.ts', {
  modules: {
    './media': {},
    './media-controls.css': {},
    './settings-dialog': {},
    './settings-dialog.css': {},
    './audio-output': {},
  },
});

async function functionSource(name) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(declaration, `${name} must remain a shared UI action`);
  return declaration.getText(ast);
}

async function roomEventSource(names) {
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
  return names
    .map((name) => {
      const property = roomEvents.properties.find((node) => node.name?.getText(ast) === name);
      assert.ok(property, `${name} must be wired to the room`);
      return property.getText(ast);
    })
    .join(',');
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test('concurrent home and leave actions share cleanup until it finishes', async () => {
  let cleanup = deferred();
  let departures = 0;
  const api = evaluateTypeScript(
    `let departureInProgress = null;
     ${await functionSource('leaveCurrentRoom')}
     export { leaveCurrentRoom };`,
    {
      globals: {
        updateJoinBtn() {},
        navigation: { resumePendingJoin() {} },
        leaveRoomAndShowHome() {
          departures++;
          return cleanup.promise;
        },
      },
    },
  );
  const first = api.leaveCurrentRoom();
  assert.equal(api.leaveCurrentRoom(), first);
  assert.equal(departures, 1);
  cleanup.resolve();
  await first;
  cleanup = deferred();
  const next = api.leaveCurrentRoom();
  assert.equal(departures, 2);
  cleanup.reject(new Error('Cleanup failed'));
  await assert.rejects(next, /Cleanup failed/);
  cleanup = deferred();
  const retry = api.leaveCurrentRoom();
  assert.equal(departures, 3, 'failed cleanup must not latch future departures');
  cleanup.resolve();
  await retry;
});

test('departure UI announces a named peer without a media tile and keeps recovery cleanup silent', async () => {
  const messages = [];
  const detached = [];
  let gridUpdates = 0;
  const { handleParticipantLeft } = evaluateTypeScript(
    `${await functionSource('handleParticipantLeft')} export { handleParticipantLeft };`,
    {
      globals: {
        mediaControls: { detachParticipant: (id) => detached.push(id) },
        lobbyWaiters: new Map([['no-media', 'Nickname']]),
        remoteTiles: new Map(),
        stopObservingTileSize() {},
        updateVideoGridCount: () => gridUpdates++,
        appendSystemMessage: (text) => messages.push(text),
      },
    },
  );
  handleParticipantLeft('no-media', 'Nickname');
  handleParticipantLeft('recovery-cleanup');
  assert.deepEqual(messages, ['Nickname left']);
  assert.deepEqual(detached, ['no-media', 'recovery-cleanup']);
  assert.equal(gridUpdates, 2);
});

async function cameraUiFixture() {
  const state = { pending: deferred(), captures: 0, updates: [], toasts: [] };
  const activeRoom = {
    hasMedia: true,
    membershipVersion: 1,
    videoEnabled: false,
    role: 'member',
    roomSettings: {},
    toggleVideo() {
      state.captures++;
      return state.pending.promise;
    },
  };
  const api = evaluateTypeScript(
    `let room = initialRoom;
     let cameraTogglePending = false;
     let roomRecovering = false;
     ${await functionSource('canStartBroadcast')}
     ${await functionSource('toggleCamera')}
     export { toggleCamera };
     export function replaceRoom(next) { room = next; }
     export function recovering(value) { roomRecovering = value; }`,
    {
      globals: {
        initialRoom: activeRoom,
        updateCamButton: (enabled) => state.updates.push(['camera', enabled]),
        updateLocalTile: () => state.updates.push(['tile']),
        showToast: (message) => state.toasts.push(message),
        mediaErrorMessage,
      },
    },
  );
  return { ...api, state, activeRoom };
}

test('explicit camera activation captures in the click turn without setup or duplicate pending capture', async () => {
  const f = await cameraUiFixture();
  assert.equal(f.state.captures, 0, 'initializing the action never captures');
  const activation = f.toggleCamera();
  assert.equal(
    f.state.captures,
    1,
    'camera access begins before the first await without a settings prerequisite',
  );
  await f.toggleCamera();
  assert.equal(f.state.captures, 1);
  assert.deepEqual(f.state.updates, []);
  f.state.pending.resolve(true);
  await activation;
  assert.deepEqual(f.state.updates, [['camera', true], ['tile']]);
});

test('camera activation preserves room readiness, recovery and broadcasting permissions', async () => {
  const f = await cameraUiFixture();
  f.activeRoom.hasMedia = false;
  await f.toggleCamera();
  f.activeRoom.hasMedia = true;
  f.recovering(true);
  await f.toggleCamera();
  f.recovering(false);
  f.activeRoom.roomSettings = { allowVideo: false };
  await f.toggleCamera();
  f.activeRoom.role = 'guest';
  f.activeRoom.roomSettings = { guestsCanBroadcast: false };
  await f.toggleCamera();
  f.activeRoom.roomSettings = { moderated: true };
  await f.toggleCamera();
  assert.equal(f.state.captures, 0);
  assert.deepEqual(f.state.updates, []);

  f.activeRoom.videoEnabled = true;
  f.state.pending.resolve(false);
  await f.toggleCamera();
  assert.equal(f.state.captures, 1, 'turning an active camera off remains available');
  assert.deepEqual(f.state.updates, [['camera', false], ['tile']]);
});

test('a refused camera request reports the current state and permits a new explicit gesture', async () => {
  const f = await cameraUiFixture();
  const activation = f.toggleCamera();
  f.state.pending.reject(new Error('Camera access denied'));
  await activation;
  assert.deepEqual(f.state.updates, [['camera', false]]);
  assert.deepEqual(f.state.toasts, ['Camera access denied']);
  f.state.pending = deferred();
  const retry = f.toggleCamera();
  assert.equal(f.state.captures, 2, 'a failed request does not latch later explicit actions');
  f.state.pending.resolve(true);
  await retry;
});

for (const change of ['leave', 'replacement']) {
  for (const outcome of ['resolve', 'reject']) {
    test(`camera ${outcome} after ${change} cannot update another room`, async () => {
      const f = await cameraUiFixture();
      const activation = f.toggleCamera();
      f.replaceRoom(change === 'leave' ? null : { ...f.activeRoom, membershipVersion: 2 });
      if (outcome === 'reject') f.state.pending.reject(new Error('Retired camera permission'));
      else f.state.pending.resolve(true);
      await activation;
      assert.equal(f.state.captures, 1);
      assert.deepEqual(f.state.updates, []);
      assert.deepEqual(f.state.toasts, []);
    });
  }
}

for (const action of ['toggleMicrophone', 'toggleCamera', 'toggleScreenShare', 'pttActivate']) {
  for (const outcome of ['resolve', 'reject']) {
    test(`${action} ${outcome} cannot update a fresh membership on the same room client`, async () => {
      const pending = deferred();
      const updates = [];
      const activeRoom = {
        membershipVersion: 1,
        hasMedia: true,
        audioEnabled: false,
        videoEnabled: false,
        isScreenSharing: false,
        toggleAudio: () => pending.promise,
        toggleVideo: () => pending.promise,
        startScreenShare: () => pending.promise,
        unmuteAudio: () => pending.promise,
        muteAudio: () => updates.push('mute'),
      };
      const api = evaluateTypeScript(
        `
        let room = initialRoom;
        let micMode = 'open';
        let microphoneTogglePending = false;
        let cameraTogglePending = false;
        let pttHeld = false;
        let pttActivation = 0;
        ${await functionSource(action)}
        export { ${action} as activate };
      `,
        {
          globals: {
            initialRoom: activeRoom,
            canStartBroadcast: () => true,
            updateMicButton: () => updates.push('microphone'),
            updateCamButton: () => updates.push('camera'),
            updateScreenButton: () => updates.push('screen'),
            updateLocalTile: () => updates.push('tile'),
            showToast: () => updates.push('toast'),
          },
        },
      );
      const activation = api.activate();
      activeRoom.membershipVersion++;
      if (outcome === 'reject') pending.reject(new Error('Retired capture'));
      else pending.resolve(true);
      if (action === 'toggleScreenShare' && outcome === 'reject')
        await assert.rejects(activation, /Retired capture/);
      else await activation;
      assert.deepEqual(updates, []);
    });
  }
}

test('successful recovery displays fresh-session media guidance and keeps the resumed-session fallback', async () => {
  const connectionStatus = {};
  const toasts = [];
  let joinedUpdates = 0,
    localUpdates = 0;
  const api = evaluateTypeScript(
    `
    let roomRecovering = true;
    export const events = { ${await roomEventSource(['onRecoveryState'])} };
    export function recovering() { return roomRecovering; }
  `,
    {
      globals: {
        connectionStatus,
        document: createDOM().document,
        room: { localParticipantId: 'local' },
        applyJoinedRoomUI: () => joinedUpdates++,
        updateLocalTile: () => localUpdates++,
        showToast: (message) => toasts.push(message),
      },
    },
  );
  const guidance =
    'Room rejoined. Your microphone, camera, and screen sharing are off; turn them on when you are ready.';
  api.events.onRecoveryState('connected', guidance);
  assert.equal(api.recovering(), false);
  assert.deepEqual(connectionStatus, { textContent: 'Connected', className: 'status connected' });
  assert.deepEqual(toasts, [guidance]);
  api.events.onRecoveryState('connected');
  assert.deepEqual(toasts, [guidance, 'Room connection restored']);
  assert.equal(joinedUpdates, 2);
  assert.equal(localUpdates, 2);
});

test('restored lobby signaling does not expose joined-room or media controls', async () => {
  const changes = [];
  const api = evaluateTypeScript(
    `let roomRecovering = true;
     export const events = { ${await roomEventSource(['onRecoveryState'])} };
     export function recovering() { return roomRecovering; }`,
    {
      globals: {
        document: createDOM().document,
        connectionStatus: {},
        room: { localParticipantId: null },
        applyJoinedRoomUI: () => changes.push('joined'),
        updateLocalTile: () => changes.push('media'),
        showToast: (message) => changes.push(message),
      },
    },
  );
  api.events.onRecoveryState('connected', 'Connection restored. Waiting for room admission.');
  assert.equal(api.recovering(), false);
  assert.deepEqual(changes, ['Connection restored. Waiting for room admission.']);
});

test('failed recovery actions persist and cannot retry or leave a replacement room', async () => {
  const { document } = createDOM();
  const notices = [];
  let retries = 0;
  let leaves = 0;
  const initialRoom = { retryRecovery: () => retries++ };
  const api = evaluateTypeScript(
    `let roomRecovering = false;
     let room = initialRoom;
     export const events = { ${await roomEventSource(['onRecoveryState'])} };
     export function replaceRoom(next) { room = next; }`,
    {
      globals: {
        document,
        initialRoom,
        connectionStatus: {},
        pttDeactivate() {},
        applyRoomSettingsToUI() {},
        retireRoomSettingsAction() {},
        observeUiTask() {},
        leaveCurrentRoom: () => leaves++,
        showActionToast(message, actions, duration) {
          const node = document.createElement('div');
          document.body.append(node);
          notices.push({ message, actions, duration, node });
          return node;
        },
      },
    },
  );
  api.events.onRecoveryState('failed', 'Retry after maintenance');
  const first = notices[0];
  assert.equal(first.duration, 0);
  assert.equal(first.node.id, 'room-recovery-notice');
  first.actions[0].action();
  assert.equal(retries, 1);
  api.events.onRecoveryState('failed', 'Still unavailable');
  assert.equal(first.node.isConnected, false, 'a newer notice retires the previous one');
  api.replaceRoom({ retryRecovery: () => retries++ });
  first.actions[0].action();
  first.actions[1].action();
  assert.equal(retries, 1);
  assert.equal(leaves, 0);
});

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
  const eventSource = await roomEventSource(['onLocalMediaChanged', 'onLocalCaptureStopped']);
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
    toasts: ['Microphone stopped. Press M or the mic button to turn it back on.'],
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
    name_style: { color: null, style: 'accent' },
    topic_style: { color: null, style: 'accent' },
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
    toasts: ['Camera stopped. Press V or the camera button to turn it back on.'],
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

for (const change of ['leave', 'replacement', 'membership']) {
  test(`a password prompt retired by ${change} cannot retry a stale or replacement room`, async () => {
    const password = deferred();
    class RoomPasswordRequiredError extends Error {}
    let joins = 0;
    let leaves = 0;
    const owner = {
      membershipVersion: 1,
      join: async () => {
        joins++;
        throw new RoomPasswordRequiredError();
      },
    };
    const api = evaluateTypeScript(
      `let room = owner; ${await functionSource('joinRoomWithPassword')} export { joinRoomWithPassword }; export function replace(next) { room = next; }`,
      {
        globals: {
          owner,
          RoomPasswordRequiredError,
          requestRoomPassword: () => password.promise,
          leaveCurrentRoom: async () => {
            leaves++;
          },
        },
      },
    );
    const joining = api.joinRoomWithPassword(owner, 'private-room', 'Guest');
    await flush();
    if (change === 'membership') owner.membershipVersion++;
    else
      api.replace(
        change === 'leave'
          ? null
          : {
              join() {
                throw new Error('Must not join replacement');
              },
            },
      );
    password.resolve('retired-password');
    assert.equal(await joining, null);
    assert.equal(joins, 1);
    assert.equal(leaves, 0);
  });
}

test('cancelling an owned password prompt disposes its room membership', async () => {
  class RoomPasswordRequiredError extends Error {}
  let leaves = 0;
  const owner = {
    membershipVersion: 1,
    join: async () => {
      throw new RoomPasswordRequiredError();
    },
  };
  const api = evaluateTypeScript(
    `let room = owner; ${await functionSource('joinRoomWithPassword')} export { joinRoomWithPassword };`,
    {
      globals: {
        owner,
        RoomPasswordRequiredError,
        requestRoomPassword: async () => null,
        leaveCurrentRoom: async () => {
          leaves++;
        },
      },
    },
  );
  assert.equal(await api.joinRoomWithPassword(owner, 'private-room', 'Guest'), null);
  assert.equal(leaves, 1);
});

test('room limits distinguish blank, invalid and complete numeric values', async () => {
  const status = { textContent: '' };
  const { roomLimit } = evaluateTypeScript(`export ${await functionSource('roomLimit')}`, {
    globals: { roomSettingsStatus: status },
  });
  const input = (value, number, valid = true, bad = false) => ({
    value,
    valueAsNumber: number,
    validity: { badInput: bad },
    checkValidity: () => valid,
    reportValidity() {},
    focus() {},
  });
  assert.equal(roomLimit(input('', NaN)), null);
  assert.equal(roomLimit(input('3e1', 30)), 30);
  assert.equal(roomLimit(input('0', 0)), 0);
  for (const field of [
    input('', NaN, false, true),
    input('2.5', 2.5),
    input('-1', -1),
    input('4294967296', 4294967296),
    input('2', 2, false),
  ]) {
    assert.equal(roomLimit(field), undefined);
  }
});

async function speakingHighlightFixture() {
  class FakeElement {
    constructor() {
      this.classes = new Set();
      this.classList = {
        add: (name) => this.classes.add(name),
        remove: (name) => this.classes.delete(name),
        contains: (name) => this.classes.has(name),
      };
    }
  }
  const tiles = new Map([
    ['alice', new FakeElement()],
    ['bob', new FakeElement()],
  ]);
  const rows = new Map([
    ['alice', new FakeElement()],
    ['bob', new FakeElement()],
  ]);
  const timers = [];
  const api = evaluateTypeScript(
    `
    const AUDIO_LEVEL_THRESHOLD = -50;
    const SPEAKING_HIGHLIGHT_TIMEOUT_MS = 2000;
    const currentlySpeaking = new Set<HTMLElement>();
    let currentDominantTile: HTMLElement | null = null;
    let speakingHighlightTimer: ReturnType<typeof setTimeout> | null = null;
    ${await functionSource('clearSpeakingHighlights')}
    export const events = { ${await roomEventSource(['onActiveSpeaker', 'onAudioLevels'])} };
    `,
    {
      globals: {
        remoteTiles: tiles,
        participantList: {
          querySelector: (selector) => rows.get(/"([^"]+)"/.exec(selector)[1]) ?? null,
        },
        document: { getElementById: () => null },
        CSS: { escape: (value) => value },
        setTimeout: (callback, delay) => {
          timers.push({ callback, delay, cancelled: false });
          return timers.length;
        },
        clearTimeout: (handle) => {
          if (handle) timers[handle - 1].cancelled = true;
        },
      },
    },
  );
  return { events: api.events, tiles, rows, timers };
}

test('speaking highlights clear when the server reports silence', async () => {
  const { events, tiles, rows, timers } = await speakingHighlightFixture();
  events.onActiveSpeaker('alice');
  events.onAudioLevels([{ participantId: 'alice', volume: -20 }]);
  assert.ok(tiles.get('alice').classes.has('speaking'));
  assert.ok(tiles.get('alice').classes.has('dominant-speaker'));
  assert.ok(rows.get('alice').classes.has('speaking'));

  events.onAudioLevels([]);

  assert.deepEqual([...tiles.get('alice').classes], []);
  assert.deepEqual([...rows.get('alice').classes], []);
  assert.ok(
    timers.every((timer) => timer.cancelled),
    'silence cancels the expiry timer',
  );
});

test('speaking highlights expire when audio level reports stop arriving', async () => {
  const { events, tiles, rows, timers } = await speakingHighlightFixture();
  events.onActiveSpeaker('bob');
  events.onAudioLevels([{ participantId: 'bob', volume: -30 }]);
  events.onAudioLevels([{ participantId: 'bob', volume: -30 }]);
  const pending = timers.filter((timer) => !timer.cancelled);
  assert.equal(pending.length, 1, 'each report re-arms one expiry timer');
  assert.equal(pending[0].delay, 2000);

  pending[0].callback();

  assert.deepEqual([...tiles.get('bob').classes], []);
  assert.deepEqual([...rows.get('bob').classes], []);
});

test('signaling recovery captures the public replay boundary before subsequent UI updates', async () => {
  const calls = [];
  const api = evaluateTypeScript(
    `let roomRecovering = false;
     export const events = { ${await roomEventSource(['onRecoveryState'])} };`,
    {
      globals: {
        document: createDOM().document,
        connectionStatus: {},
        socialChat: { prepareSnapshotRecovery: () => calls.push('capture') },
        retireRoomSettingsAction: () => calls.push('retire'),
        pttDeactivate: () => calls.push('ptt'),
        applyRoomSettingsToUI: () => calls.push('settings'),
      },
    },
  );
  api.events.onRecoveryState('reconnecting');
  assert.deepEqual(calls, ['capture', 'retire', 'ptt', 'settings']);
});
