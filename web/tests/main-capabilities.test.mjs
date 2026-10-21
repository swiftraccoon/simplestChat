import assert from 'node:assert/strict';
import test from 'node:test';
import { capabilities as supported } from '../e2e/capabilities-fixture.cjs';
import { homeScreenFixture } from './home-screen-fixture.mjs';

function discoveryFixture() {
  const f = homeScreenFixture();
  const requests = [],
    effects = [],
    tasks = [];
  f.services.loadCapabilities = () => {
    const response = Promise.withResolvers();
    requests.push(response);
    return response.promise;
  };
  f.services.authScreen.applyCapabilities = (value) => effects.push({ applied: value });
  f.services.navigation.resumePendingJoin = () => effects.push('resume-join');
  f.services.previewPendingInvite = () => {
    effects.push('preview-invite');
    return Promise.resolve();
  };
  const observe = f.services.observeUiTask;
  f.services.observeUiTask = (task, message) => {
    observe(task, message);
    if (message === 'Invitation not reviewed') tasks.push({ task, message });
  };
  f.elements.nameInput.focus = () => effects.push('focus-name');
  return {
    ...f.screen,
    requests,
    effects,
    tasks,
    document: f.document,
    capabilitiesRetry: f.elements.capabilitiesRetry,
    serverMode: f.elements.serverMode,
    state: () => ({
      capabilities: f.store.capabilities,
      loading: f.elements.capabilitiesRetry.disabled,
    }),
  };
}

test('capability discovery issues one read while pending and never reloads a known contract', async () => {
  const f = discoveryFixture();
  const loading = f.loadCapabilities();
  await f.loadCapabilities();
  assert.equal(f.requests.length, 1);
  assert.deepEqual(f.state(), { capabilities: null, loading: true });
  assert.equal(f.capabilitiesRetry.disabled, true);
  assert.equal(f.serverMode.hidden, false);
  assert.match(f.serverMode.textContent, /checking/i);
  assert.deepEqual(f.effects, [], 'pending discovery must not resume joins or invitations');

  f.requests[0].resolve(supported);
  await loading;
  assert.deepEqual(f.state(), { capabilities: supported, loading: false });
  assert.deepEqual(f.effects, [{ applied: supported }, 'resume-join', 'preview-invite']);
  assert.equal(f.capabilitiesRetry.disabled, false);
  assert.equal(f.capabilitiesRetry.hidden, true);
  assert.equal(f.tasks.length, 1);
  await f.tasks[0].task;

  await f.loadCapabilities();
  assert.equal(f.requests.length, 1);
  assert.equal(f.effects.length, 3, 'known capabilities must not replay queued intent');
});

test('failed discovery keeps features unknown and a successful explicit retry restores pending intent', async () => {
  const f = discoveryFixture();
  const first = f.loadCapabilities();
  f.requests[0].reject(new Error('Response contained invalid data'));
  await first;
  assert.deepEqual(f.state(), { capabilities: null, loading: false });
  assert.equal(f.serverMode.hidden, false);
  assert.match(f.serverMode.textContent, /could not be loaded/i);
  assert.equal(f.capabilitiesRetry.hidden, false);
  assert.equal(f.capabilitiesRetry.disabled, false);
  assert.deepEqual(f.effects, []);

  f.document.activeElement = f.capabilitiesRetry;
  const retry = f.loadCapabilities();
  await f.loadCapabilities();
  assert.equal(f.requests.length, 2, 'retry also owns a single read');
  assert.equal(f.capabilitiesRetry.disabled, true);
  f.requests[1].resolve(supported);
  await retry;
  assert.deepEqual(f.effects, [
    { applied: supported },
    'resume-join',
    'preview-invite',
    'focus-name',
  ]);
  assert.equal(f.capabilitiesRetry.hidden, true);
  assert.equal(f.capabilitiesRetry.disabled, false);
});

test('another failed retry stays available without moving keyboard focus or starting account work', async () => {
  const f = discoveryFixture();
  f.document.activeElement = f.capabilitiesRetry;
  for (let attempt = 0; attempt < 2; attempt++) {
    const loading = f.loadCapabilities();
    f.requests[attempt].reject(new Error('Offline'));
    await loading;
    assert.deepEqual(f.state(), { capabilities: null, loading: false });
    assert.equal(f.capabilitiesRetry.hidden, false);
    assert.equal(f.capabilitiesRetry.disabled, false);
  }
  assert.deepEqual(f.effects, []);
  assert.deepEqual(f.tasks, []);
});

for (const [name, overrides, disabled] of [
  ['unknown capabilities', { capabilities: null }, true],
  [
    'advertised guest rooms',
    { capabilities: { ...supported, accounts: false, roomDirectory: false } },
    false,
  ],
  ['advertised saved rooms', { capabilities: { ...supported, adHocRooms: false } }, false],
  [
    'no advertised rooms',
    { capabilities: { ...supported, roomDirectory: false, adHocRooms: false } },
    true,
  ],
  ['closed socket', { signaling: { connected: false } }, true],
  ['pending navigation', { navigationPending: true }, true],
  ['unfinished departure', { departureInProgress: Promise.resolve() }, true],
  ['existing room', { room: {} }, true],
  ['blank display name', { nameInput: { value: '  ' } }, true],
  ['blank room', { roomInput: { value: '  ' } }, true],
  ['invalid room character', { roomInput: { value: 'room/name' } }, true],
  ['oversized room ID', { roomInput: { value: 'a'.repeat(129) } }, true],
  ['maximum room ID', { roomInput: { value: 'a'.repeat(128) } }, false],
]) {
  test(`join availability respects ${name}`, () => {
    const f = homeScreenFixture({ capabilities: supported });
    Object.assign(
      f.store,
      Object.fromEntries(Object.entries(overrides).filter(([key]) => key in f.store)),
    );
    Object.assign(f.services.signaling, overrides.signaling);
    Object.assign(f.elements.nameInput, { value: '  Guest  ' }, overrides.nameInput);
    Object.assign(f.elements.roomInput, { value: '  room_A-2  ' }, overrides.roomInput);
    f.elements.joinBtn.disabled = !disabled;
    f.screen.updateJoinBtn();
    assert.equal(f.elements.joinBtn.disabled, disabled);
    f.screen.dispose();
  });
}

function authUiFixture(capabilities, signedIn) {
  const f = homeScreenFixture({ capabilities });
  const effects = [];
  Object.assign(f.services.auth, {
    isLoggedIn: signedIn,
    displayName: signedIn ? 'Restored account' : null,
    userId: signedIn ? 'restored' : null,
  });
  f.services.directoryScreen.load = () => {
    effects.push('directory');
    return Promise.resolve();
  };
  f.services.community.refresh = () => effects.push('community');
  f.services.community.decorateOwnAvatar = (_node, id, name) =>
    effects.push(`avatar:${id}:${name}`);
  let disabled = false;
  Object.defineProperty(f.elements.joinBtn, 'disabled', {
    get: () => disabled,
    set: (value) => {
      disabled = value;
      effects.push('join-controls');
    },
  });
  return {
    updateAuthUI: f.screen.updateAuthUI,
    nodes: f.elements,
    effects,
    nameInput: f.elements.nameInput,
    authAvatar: f.elements.authAvatar,
  };
}

for (const signedIn of [false, true]) {
  test(`unknown capabilities keep optional actions hidden for a ${signedIn ? 'restored account' : 'guest'}`, () => {
    const f = authUiFixture(null, signedIn);
    f.updateAuthUI();
    for (const name of [
      'signInBtn',
      'communityActions',
      'roomBrowser',
      'directoryToolbar',
      'createRoomBtn',
    ])
      assert.equal(f.nodes[name].hidden, true, name);
    // A restored account decorates its avatar even before the server's features are known.
    assert.deepEqual(
      f.effects,
      signedIn
        ? ['avatar:restored:Restored account', 'join-controls', 'community']
        : ['join-controls', 'community'],
    );
    assert.equal(f.nodes.authBarUser.hidden, !signedIn);
    assert.equal(f.nodes.authBarGuest.hidden, signedIn);
    assert.equal(f.nameInput.value, signedIn ? 'Restored account' : '');
  });
}

test('known capabilities reveal only advertised account and directory actions', () => {
  const guestServer = authUiFixture(
    { ...supported, accounts: false, roomDirectory: false, roomCreation: false },
    false,
  );
  guestServer.updateAuthUI();
  for (const name of [
    'signInBtn',
    'communityActions',
    'roomBrowser',
    'directoryToolbar',
    'createRoomBtn',
  ])
    assert.equal(guestServer.nodes[name].hidden, true, name);
  assert.deepEqual(guestServer.effects, ['join-controls', 'community']);

  for (const signedIn of [false, true]) {
    const f = authUiFixture(supported, signedIn);
    f.updateAuthUI();
    for (const name of ['signInBtn', 'communityActions', 'roomBrowser', 'directoryToolbar'])
      assert.equal(f.nodes[name].hidden, false, name);
    assert.equal(f.nodes.createRoomBtn.hidden, !signedIn);
    assert.deepEqual(
      f.effects,
      signedIn
        ? ['avatar:restored:Restored account', 'directory', 'join-controls', 'community']
        : ['directory', 'join-controls', 'community'],
    );
  }

  const creationDisabled = authUiFixture({ ...supported, roomCreation: false }, true);
  creationDisabled.updateAuthUI();
  assert.equal(creationDisabled.nodes.createRoomBtn.hidden, true);
});

test('home startup is explicit and a retired capability result cannot resume account or room intent', async () => {
  const f = homeScreenFixture();
  const response = Promise.withResolvers();
  let reads = 0;
  let restores = 0;
  let resumed = 0;
  f.services.loadCapabilities = () => {
    reads++;
    return response.promise;
  };
  f.services.auth.tryRestore = async () => {
    restores++;
    return 'unavailable';
  };
  f.services.navigation.resumePendingJoin = () => resumed++;
  assert.equal(reads, 0);
  assert.equal(restores, 0);
  f.screen.start();
  f.screen.start();
  assert.equal(reads, 1);
  assert.equal(restores, 1);
  assert.equal(typeof f.state.restoreHandler, 'function');
  f.screen.dispose();
  f.screen.dispose();
  assert.equal(f.state.restoreHandler, null);
  response.resolve(supported);
  await Promise.all(f.observed.map(({ task }) => task));
  assert.equal(f.store.capabilities, null);
  assert.equal(resumed, 0);
  assert.equal(f.elements.serverMode.textContent, 'Checking server features…');
  f.screen.start();
  assert.equal(reads, 1);
  assert.equal(restores, 1);
  assert.deepEqual(f.errors, []);
});

test('home form events preserve pending join intent and disposal removes their actions', () => {
  const f = homeScreenFixture({ capabilities: supported });
  const effects = [];
  f.services.navigation.resumePendingJoin = () => effects.push('resume');
  f.services.navigation.cancelPendingJoin = () => effects.push('cancel');
  f.elements.joinBtn.addEventListener('click', () => effects.push('join'));
  f.elements.nameInput.value = 'Guest';
  f.elements.roomInput.value = 'room-a';
  f.elements.nameInput.dispatchEvent(new Event('input'));
  assert.deepEqual(effects, ['resume']);
  assert.equal(f.elements.joinBtn.disabled, false);
  f.elements.roomInput.dispatchEvent(new Event('input'));
  assert.deepEqual(effects, ['resume', 'cancel']);
  assert.equal(f.store.roomSelectionVersion, 1);
  const enter = () => Object.assign(new Event('keydown'), { key: 'Enter' });
  f.elements.nameInput.dispatchEvent(enter());
  f.elements.roomInput.dispatchEvent(enter());
  assert.deepEqual(effects, ['resume', 'cancel', 'join', 'join']);
  f.screen.dispose();
  f.elements.roomInput.dispatchEvent(new Event('input'));
  f.elements.nameInput.dispatchEvent(enter());
  assert.equal(f.store.roomSelectionVersion, 1);
  assert.deepEqual(effects, ['resume', 'cancel', 'join', 'join']);
});

test('a verified capability contract reaches auth controls before pending invite or join work', () => {
  const f = homeScreenFixture();
  const applied = [];
  f.services.hasPendingInvite = () => true;
  f.services.authScreen.applyCapabilities = (value, invited) => {
    assert.equal(f.store.capabilities, value);
    applied.push({ value, invited });
  };
  f.screen.applyCapabilities({ ...supported, accounts: false, roomDirectory: false });
  assert.equal(f.elements.serverMode.hidden, false);
  assert.equal(
    f.elements.serverMode.textContent,
    'Guest rooms are available. Enter your name and a room ID to join.',
  );
  f.screen.applyCapabilities({
    ...supported,
    accounts: false,
    roomDirectory: false,
    adHocRooms: false,
  });
  assert.equal(
    f.elements.serverMode.textContent,
    'Rooms are not available on this server yet. Contact its host for help.',
  );
  assert.equal(applied.length, 2);
  assert.equal(
    applied.every(({ invited }) => invited),
    true,
  );
  f.screen.dispose();
});
