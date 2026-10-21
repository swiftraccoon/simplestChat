import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript, loadContractModules } from './native-fixtures.mjs';
import { createUiTasks, asyncUiAction as actionAdapter } from '../src/ui-tasks.ts';
import { createDOM, deferred, flush } from './ui-fixture.mjs';
import { roomScreenFixture } from './room-screen-fixture.mjs';

test('concurrent home and leave actions share cleanup until it finishes', async () => {
  const f = await roomScreenFixture();
  let cleanup = deferred();
  let departures = 0;
  const owner = () =>
    f.makeRoom({
      leave() {
        departures++;
        return cleanup.promise;
      },
    });
  f.store.room = owner();
  const first = f.controller.leaveCurrentRoom();
  assert.equal(f.controller.leaveCurrentRoom(), first);
  assert.equal(departures, 1);
  cleanup.resolve();
  await first;
  cleanup = deferred();
  f.store.room = owner();
  const next = f.controller.leaveCurrentRoom();
  assert.equal(departures, 2);
  cleanup.reject(new Error('Cleanup failed'));
  await assert.rejects(next, /Cleanup failed/);
  cleanup = deferred();
  f.store.room = owner();
  const retry = f.controller.leaveCurrentRoom();
  assert.equal(departures, 3, 'failed cleanup must not latch future departures');
  cleanup.resolve();
  await retry;
});

test('departure UI announces a named peer without a media tile and keeps recovery cleanup silent', async () => {
  const f = await roomScreenFixture();
  let gridUpdates = 0;
  f.elements.videoGrid._dataset = new Proxy(
    {},
    {
      set(target, key, value) {
        if (key === 'count') gridUpdates++;
        target[key] = value;
        return true;
      },
    },
  );
  f.controller.handleParticipantLeft('no-media', 'Nickname');
  f.controller.handleParticipantLeft('recovery-cleanup');
  assert.deepEqual(f.state.system, ['Nickname left']);
  assert.deepEqual(f.state.calls, [
    ['detach', 'no-media'],
    ['detach', 'recovery-cleanup'],
  ]);
  assert.equal(gridUpdates, 2);
});

async function cameraUiFixture() {
  const pending = deferred();
  const state = { pending, captures: 0, updates: [], toasts: [] };
  const f = await roomScreenFixture();
  await f.connect({
    hasMedia: true,
    videoEnabled: false,
    toggleVideo() {
      state.captures++;
      return state.pending.promise.then((enabled) => {
        this.videoEnabled = enabled;
        return enabled;
      });
    },
  });
  f.services.setButtonContent = (button, _icon, tooltip) => {
    if (button === f.elements.camBtn)
      state.updates.push(['camera', tooltip.startsWith('Turn off')]);
  };
  f.elements.videoGrid._dataset = new Proxy(
    {},
    {
      set(target, key, value) {
        if (key === 'count') state.updates.push(['tile']);
        target[key] = value;
        return true;
      },
    },
  );
  f.services.showToast = (message) => state.toasts.push(message);
  return {
    ...f,
    state,
    activeRoom: f.store.room,
    toggleCamera: f.controls.toggleCamera,
    replaceRoom(next) {
      f.store.room = next;
    },
    async recovering(value) {
      const events = f.state.events;
      events.onRecoveryState(value ? 'reconnecting' : 'connected');
      state.updates.length = 0;
      state.toasts.length = 0;
    },
  };
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
  await f.recovering(true);
  await f.toggleCamera();
  await f.recovering(false);
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

for (const change of ['leave', 'replacement'])
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

for (const action of ['toggleMicrophone', 'toggleCamera', 'toggleScreenShare', 'pttActivate'])
  for (const outcome of ['resolve', 'reject']) {
    test(`${action} ${outcome} cannot update a fresh membership on the same room client`, async () => {
      const pending = deferred();
      const updates = [];
      const f = await roomScreenFixture({
        room: {
          hasMedia: true,
          toggleAudio: () => pending.promise,
          toggleVideo: () => pending.promise,
          startScreenShare: () => pending.promise,
          unmuteAudio: () => pending.promise,
          muteAudio: () => updates.push('mute'),
        },
      });
      f.services.setButtonContent = () => updates.push('button');
      f.services.showToast = () => updates.push('toast');
      const activation = f.controls[action]();
      f.store.room.membershipVersion++;
      if (outcome === 'reject') pending.reject(new Error('Retired capture'));
      else pending.resolve(true);
      if (action === 'toggleScreenShare' && outcome === 'reject')
        await assert.rejects(activation, /Retired capture/);
      else await activation;
      assert.deepEqual(updates, []);
    });
  }

test('successful recovery displays fresh-session media guidance and keeps the resumed-session fallback', async () => {
  const f = await roomScreenFixture();
  await f.connect({ localParticipantId: 'local' });
  let joinedUpdates = 0,
    localUpdates = 0;
  f.services.callHealth.sample = () => joinedUpdates++;
  f.elements.videoGrid._dataset = new Proxy(
    {},
    {
      set(target, key, value) {
        if (key === 'count') localUpdates++;
        target[key] = value;
        return true;
      },
    },
  );
  const guidance =
    'Room rejoined. Your microphone, camera, and screen sharing are off; turn them on when you are ready.';
  f.state.events.onRecoveryState('connected', guidance);
  assert.equal(f.controller.recovering, false);
  assert.equal(f.elements.connectionStatus.textContent, 'Connected');
  assert.equal(f.elements.connectionStatus.className, 'status connected');
  assert.deepEqual(f.state.toasts, [[guidance]]);
  f.state.events.onRecoveryState('connected');
  assert.deepEqual(f.state.toasts, [[guidance], ['Room connection restored']]);
  assert.equal(joinedUpdates, 2);
  assert.equal(localUpdates, 2);
});

test('restored lobby signaling does not expose joined-room or media controls', async () => {
  const f = await roomScreenFixture();
  await f.connect({ localParticipantId: null });
  f.state.events.onRecoveryState('connected', 'Connection restored. Waiting for room admission.');
  assert.equal(f.controller.recovering, false);
  assert.deepEqual(f.state.toasts, [['Connection restored. Waiting for room admission.']]);
  assert.deepEqual(f.state.buttonUpdates, []);
});

test('failed recovery actions persist and cannot retry or leave a replacement room', async () => {
  let retries = 0,
    leaves = 0;
  const f = await roomScreenFixture();
  await f.connect({ retryRecovery: () => retries++, leave: async () => leaves++ });
  f.state.events.onRecoveryState('failed', 'Retry after maintenance');
  const first = f.state.notices[0];
  assert.equal(first.duration, 0);
  assert.equal(first.node.id, 'room-recovery-notice');
  first.actions[0].action();
  assert.equal(retries, 1);
  f.state.events.onRecoveryState('failed', 'Still unavailable');
  assert.equal(first.node.isConnected, false, 'a newer notice retires the previous one');
  f.store.room = f.makeRoom({ retryRecovery: () => retries++ });
  first.actions[0].action();
  first.actions[1].action();
  assert.equal(retries, 1);
  assert.equal(leaves, 0);
});

test('room-required PTT does not overwrite the personal microphone mode', async () => {
  let muted = 0;
  const f = await roomScreenFixture({
    room: {
      hasMedia: true,
      audioEnabled: true,
      roomSettings: { pushToTalk: true },
      muteAudio() {
        muted++;
        this.audioEnabled = false;
      },
    },
  });
  const row = f.controller.buildTalkRow();
  const pressed = () =>
    row.querySelectorAll('button').find((button) => button.getAttribute('aria-pressed') === 'true')
      ?.textContent;
  f.controls.setMicMode('ptt', false);
  assert.equal(pressed(), 'Hold to talk');
  assert.equal(muted, 1);
  f.controls.setMicMode('open');
  assert.equal(pressed(), 'Hold to talk');
  assert.equal(f.state.preferences.length, 0);
  await f.controller.leaveCurrentRoom();
  assert.equal(pressed(), 'Open mic');
});

test('media shortcuts are blocked in dialogs, interactive controls, and modified/repeated key presses', async () => {
  const f = await roomScreenFixture({ room: {} });
  const target = new f.Node('div');
  const basic = {
    target,
    defaultPrevented: false,
    repeat: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
  };
  assert.equal(f.controller.shortcutsBlocked(basic), false);
  for (const flag of ['defaultPrevented', 'repeat', 'ctrlKey', 'metaKey', 'altKey', 'shiftKey'])
    assert.equal(f.controller.shortcutsBlocked({ ...basic, [flag]: true }), true);
  target.interactive = true;
  assert.equal(f.controller.shortcutsBlocked(basic), true);
  target.interactive = false;
  const dialog = f.document.createElement('dialog');
  f.document.body.append(dialog);
  dialog.showModal();
  assert.equal(f.controller.shortcutsBlocked(basic), true);
});

async function captureStoppedUiFixture(initialRoom, mode = 'open', held = false) {
  const f = await roomScreenFixture({ mode });
  await f.connect(initialRoom);
  if (held) {
    f.key('keydown', 't');
    await flush();
  }
  f.state.buttonUpdates.length = 0;
  return {
    ...f,
    stop(kind) {
      f.state.events.onLocalMediaChanged();
      f.state.events.onLocalCaptureStopped(kind);
    },
  };
}

test('stopped open microphone refreshes controls and requests explicit restart without touching camera or screen', async () => {
  const f = await captureStoppedUiFixture({
    hasMedia: true,
    audioEnabled: false,
    videoEnabled: true,
    isScreenSharing: true,
  });
  f.stop('audio');
  assert.deepEqual(
    f.state.buttonUpdates.map((update) => update.button),
    [f.elements.micBtn, f.elements.camBtn, f.elements.screenBtn],
  );
  assert.equal(f.elements.micBtn.classList.contains('active'), false);
  assert.equal(f.elements.camBtn.classList.contains('active'), true);
  assert.equal(f.elements.screenBtn.classList.contains('active'), true);
  assert.deepEqual(f.state.toasts, [
    ['Microphone stopped. Press M or the mic button to turn it back on.'],
  ]);
  assert.equal(f.store.room.videoEnabled, true);
  assert.equal(f.store.room.isScreenSharing, true);
});

test('stopped camera preserves held microphone intent and reports explicit camera restart', async () => {
  const f = await captureStoppedUiFixture(
    {
      hasMedia: true,
      audioEnabled: true,
      videoEnabled: false,
      isScreenSharing: true,
      unmuteAudio: async () => {},
    },
    'ptt',
    true,
  );
  f.stop('video');
  assert.equal(f.elements.micBtn.classList.contains('ptt-active'), true);
  assert.equal(f.elements.camBtn.classList.contains('active'), false);
  assert.equal(f.elements.screenBtn.classList.contains('active'), true);
  assert.deepEqual(f.state.toasts, [
    ['Camera stopped. Press V or the camera button to turn it back on.'],
  ]);
  assert.equal(f.store.room.audioEnabled, true);
  assert.equal(f.store.room.isScreenSharing, true);
});

for (const key of [' ', 't'])
  test(`stopped PTT ignores pending activation and repeated ${JSON.stringify(key)} until a fresh hold`, async () => {
    const pending = deferred();
    let activations = 0,
      mutes = 0;
    const f = await captureStoppedUiFixture(
      {
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
      },
      'ptt',
    );
    f.key('keydown', key);
    assert.equal(activations, 1);
    f.stop('audio');
    const before = f.state.buttonUpdates.length;
    assert.deepEqual(f.state.toasts, [
      ['Microphone stopped. Release, then hold Space/T or the microphone button again to restart.'],
    ]);
    f.key('keydown', key, true);
    pending.resolve();
    await flush();
    assert.equal(activations, 1, 'a repeated held key must not reopen stopped capture');
    assert.equal(
      f.state.buttonUpdates.length,
      before,
      'late activation completion cannot refresh the retired intent',
    );
    assert.equal(mutes, 0, 'capture stop must not issue new media commands');
    f.key('keyup', key);
    f.key('keydown', key);
    await flush();
    assert.equal(activations, 2, 'an explicit new hold may request capture again');
    assert.equal(f.store.room.videoEnabled, true);
    f.key('keyup', key);
    assert.equal(mutes, 1);
  });

test('capture-stop UI ignores notifications after leaving', async () => {
  const f = await captureStoppedUiFixture({ hasMedia: true }, 'ptt');
  f.store.room = null;
  f.stop('audio');
  f.stop('video');
  assert.deepEqual(f.state.buttonUpdates, []);
  assert.deepEqual(f.state.toasts, []);
});

for (const change of ['leave', 'replacement', 'membership'])
  test(`a password prompt retired by ${change} cannot retry a stale or replacement room`, async () => {
    const f = await roomScreenFixture();
    let joins = 0,
      leaves = 0;
    const owner = f.makeRoom({
      join: async () => {
        joins++;
        throw new f.RoomPasswordRequiredError();
      },
      leave: async () => leaves++,
    });
    f.store.room = owner;
    const joining = f.controller.joinRoomWithPassword(owner, 'private-room', 'Guest');
    await flush();
    const field = f.document.getElementById('join-room-password');
    assert.ok(field);
    if (change === 'membership') owner.membershipVersion++;
    else
      f.store.room =
        change === 'leave'
          ? null
          : f.makeRoom({
              join() {
                throw new Error('Must not join replacement');
              },
            });
    field.value = 'retired-password';
    field.parentNode.emit('submit', { preventDefault() {} });
    assert.equal(await joining, null);
    assert.equal(joins, 1);
    assert.equal(leaves, 0);
    assert.equal(field.value, '');
  });

test('cancelling an owned password prompt disposes its room membership', async () => {
  const f = await roomScreenFixture();
  let leaves = 0;
  const owner = f.makeRoom({
    join: async () => {
      throw new f.RoomPasswordRequiredError();
    },
    leave: async () => leaves++,
  });
  f.store.room = owner;
  const joining = f.controller.joinRoomWithPassword(owner, 'private-room', 'Guest');
  await flush();
  f.document.querySelector('dialog').close();
  assert.equal(await joining, null);
  assert.equal(leaves, 1);
});

async function speakingHighlightFixture() {
  const timers = [];
  const f = await roomScreenFixture({
    globals: {
      setTimeout: (callback, delay) => {
        timers.push({ callback, delay, cancelled: false });
        return timers.length;
      },
      clearTimeout: (handle) => {
        if (handle) timers[handle - 1].cancelled = true;
      },
    },
  });
  await f.connect();
  const tiles = new Map(),
    rows = new Map();
  for (const name of ['alice', 'bob']) {
    const tile = new f.Node('div'),
      row = new f.Node('li');
    tiles.set(name, tile);
    rows.set(name, row);
    f.controls.tiles.set(name, tile);
  }
  f.elements.participantList.querySelector = (selector) =>
    rows.get(/"([^"]+)"/.exec(selector)[1]) ?? null;
  return { ...f, events: f.state.events, tiles, rows, timers };
}

test('speaking highlights clear when the server reports silence', async () => {
  const { events, tiles, rows, timers } = await speakingHighlightFixture();
  events.onActiveSpeaker('alice');
  events.onAudioLevels([{ participantId: 'alice', volume: -20 }]);
  assert.ok(tiles.get('alice').classList.contains('speaking'));
  assert.ok(tiles.get('alice').classList.contains('dominant-speaker'));
  assert.ok(rows.get('alice').classList.contains('speaking'));
  events.onAudioLevels([]);
  assert.equal(tiles.get('alice').className, '');
  assert.equal(rows.get('alice').className, '');
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
  assert.equal(tiles.get('bob').className, '');
  assert.equal(rows.get('bob').className, '');
});

test('signaling recovery captures the public replay boundary before subsequent UI updates', async () => {
  const f = await roomScreenFixture({ mode: 'ptt' });
  await f.connect({
    hasMedia: true,
    unmuteAudio: async () => {},
    muteAudio: () => f.state.calls.push('ptt'),
  });
  f.key('keydown', 't');
  await flush();
  f.state.calls.length = 0;
  f.services.socialChat.participantsChanged = () => f.state.calls.push('settings');
  f.state.events.onRecoveryState('reconnecting');
  assert.deepEqual(f.state.calls, ['capture', 'retire', 'ptt', 'settings']);
});

for (const replacement of ['none', 'room', 'account']) {
  test(`detached UI failures retain diagnostics without notifying a replaced ${replacement} context`, async () => {
    const pending = deferred();
    const notifications = [],
      errors = [];
    const auth = { userId: 'account-a' };
    let room = {};
    const { observeUiTask } = createUiTasks({
      owner: () => ({ room, accountId: auth.userId }),
      notify: (message) => notifications.push(message),
      report: (message) => errors.push([message]),
    });
    const api = {
      observeUiTask,
      replaceRoom: () => {
        room = {};
      },
    };
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
  const api = {
    asyncUiAction: (action, message) =>
      actionAdapter(action, message, (task, message) => observed.push({ task, message })),
  };
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

test('scroll-button icon initialization preserves the unread badge for SocialChat startup', async () => {
  const { initializeUnreadButton } = await loadTypeScript('src/screens/chrome.ts');
  const { document } = createDOM();
  const scrollBottomBtn = document.createElement('button');
  scrollBottomBtn.id = 'scroll-bottom-btn';
  const unreadBadge = document.createElement('span');
  unreadBadge.id = 'unread-badge';
  scrollBottomBtn.append(unreadBadge);
  document.body.append(scrollBottomBtn);
  scrollBottomBtn.insertAdjacentHTML = () => {};
  initializeUnreadButton(scrollBottomBtn, unreadBadge);
  assert.equal(document.getElementById('unread-badge'), unreadBadge);
  assert.equal(unreadBadge.parentNode, scrollBottomBtn);
});
