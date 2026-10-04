import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { capabilities as supported } from '../e2e/capabilities-fixture.cjs';
import { evaluateTypeScript } from './source-loader.mjs';

const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
function functionSource(name) {
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(declaration, `${name} must retain a named capability boundary`);
  return declaration.getText(ast);
}

function discoveryFixture() {
  const requests = [],
    effects = [],
    tasks = [];
  const capabilitiesRetry = { disabled: false, hidden: true };
  const serverMode = { hidden: true, textContent: '' };
  const document = { activeElement: null };
  const api = evaluateTypeScript(
    `let capabilities = null;
     let capabilitiesLoading = false;
     function applyCapabilities(value) { capabilities = value; applied(value); }
     ${functionSource('loadCapabilities')}
     export { loadCapabilities };
     export const state = () => ({ capabilities, loading: capabilitiesLoading });`,
    {
      globals: {
        capabilitiesRetry,
        serverMode,
        document,
        nameInput: { focus: () => effects.push('focus-name') },
        api: {
          capabilities() {
            const response = Promise.withResolvers();
            requests.push(response);
            return response.promise;
          },
        },
        applied: (value) => effects.push({ applied: value }),
        navigation: { resumePendingJoin: () => effects.push('resume-join') },
        previewPendingInvite: () => {
          effects.push('preview-invite');
          return Promise.resolve();
        },
        observeUiTask: (task, message) => tasks.push({ task, message }),
      },
    },
  );
  return { ...api, requests, effects, tasks, capabilitiesRetry, serverMode, document };
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
    const joinBtn = { disabled: !disabled };
    const { updateJoinBtn } = evaluateTypeScript(
      `${functionSource('updateJoinBtn')} export { updateJoinBtn };`,
      {
        globals: {
          capabilities: supported,
          navigationPending: false,
          departureInProgress: null,
          room: null,
          signaling: { connected: true },
          nameInput: { value: '  Guest  ' },
          roomInput: { value: '  room_A-2  ' },
          ...overrides,
          joinBtn,
        },
      },
    );
    updateJoinBtn();
    assert.equal(joinBtn.disabled, disabled);
  });
}

function authUiFixture(capabilities, signedIn) {
  const nodes = Object.fromEntries(
    [
      'signInBtn',
      'communityActions',
      'roomBrowser',
      'joinFormDivider',
      'createRoomBtn',
      'authBarGuest',
      'authBarUser',
      'authDisplayName',
    ].map((name) => [name, { hidden: false }]),
  );
  const effects = [];
  const nameInput = { value: '' };
  const { updateAuthUI } = evaluateTypeScript(
    `${functionSource('updateAuthUI')} export { updateAuthUI };`,
    {
      globals: {
        ...nodes,
        capabilities,
        auth: { isLoggedIn: signedIn, displayName: signedIn ? 'Restored account' : null },
        nameInput,
        document: {
          getElementById: (id) => {
            assert.equal(id, 'community-actions');
            return nodes.communityActions;
          },
        },
        loadRoomBrowser: () => {
          effects.push('directory');
          return Promise.resolve();
        },
        observeUiTask() {},
        updateJoinBtn: () => effects.push('join-controls'),
        community: { refresh: () => effects.push('community') },
        participantHovercard: { refresh() {} },
      },
    },
  );
  return { updateAuthUI, nodes, effects, nameInput };
}

for (const signedIn of [false, true]) {
  test(`unknown capabilities keep optional actions hidden for a ${signedIn ? 'restored account' : 'guest'}`, () => {
    const f = authUiFixture(null, signedIn);
    f.updateAuthUI();
    for (const name of [
      'signInBtn',
      'communityActions',
      'roomBrowser',
      'joinFormDivider',
      'createRoomBtn',
    ])
      assert.equal(f.nodes[name].hidden, true, name);
    assert.deepEqual(f.effects, ['join-controls', 'community']);
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
    'joinFormDivider',
    'createRoomBtn',
  ])
    assert.equal(guestServer.nodes[name].hidden, true, name);
  assert.deepEqual(guestServer.effects, ['join-controls', 'community']);

  for (const signedIn of [false, true]) {
    const f = authUiFixture(supported, signedIn);
    f.updateAuthUI();
    for (const name of ['signInBtn', 'communityActions', 'roomBrowser', 'joinFormDivider'])
      assert.equal(f.nodes[name].hidden, false, name);
    assert.equal(f.nodes.createRoomBtn.hidden, !signedIn);
    assert.deepEqual(f.effects, ['directory', 'join-controls', 'community']);
  }

  const creationDisabled = authUiFixture({ ...supported, roomCreation: false }, true);
  creationDisabled.updateAuthUI();
  assert.equal(creationDisabled.nodes.createRoomBtn.hidden, true);
});

function authDialogFixture(capabilities, canDismiss = true) {
  const effects = [],
    notices = [];
  const dialogs = Object.fromEntries(
    ['login', 'register'].map((name) => [
      name,
      {
        hidden: true,
        showModal: () => effects.push(`open-${name}`),
      },
    ]),
  );
  const { openAuthDialog } = evaluateTypeScript(
    `${functionSource('openAuthDialog')} export { openAuthDialog };`,
    {
      globals: {
        capabilities,
        loginModal: dialogs.login,
        registerModal: dialogs.register,
        loginEmail: { focus: () => effects.push('focus-login') },
        registerEmail: { focus: () => effects.push('focus-register') },
        dismissAuth: () => {
          effects.push('dismiss');
          return canDismiss;
        },
        showToast: (...args) => notices.push(args),
      },
    },
  );
  return { openAuthDialog, dialogs, effects, notices };
}

test('unknown or unavailable account support cannot open login or registration', () => {
  for (const capabilities of [null, { ...supported, accounts: false }]) {
    const f = authDialogFixture(capabilities);
    for (const dialog of Object.values(f.dialogs)) {
      f.openAuthDialog(dialog);
      assert.equal(dialog.hidden, true);
    }
    assert.deepEqual(f.effects, []);
    assert.deepEqual(f.notices, []);
  }
});

test('disabled registration reports its limit while keeping supported login available', () => {
  const f = authDialogFixture({ ...supported, passwordRegistration: 'disabled' });
  f.openAuthDialog(f.dialogs.register);
  assert.equal(f.dialogs.register.hidden, true);
  assert.deepEqual(f.effects, []);
  assert.equal(f.notices.length, 1);
  assert.match(f.notices[0][0], /registration is unavailable/i);
  assert.equal(f.notices[0][2], 'error');
  f.openAuthDialog(f.dialogs.login);
  assert.equal(f.dialogs.login.hidden, false);
  assert.deepEqual(f.effects, ['dismiss', 'open-login', 'focus-login']);
});

test('each advertised registration method can open registration and focus its form', () => {
  for (const capabilities of [
    supported,
    { ...supported, passwordRegistration: 'disabled', passkeyRegistration: 'open' },
  ]) {
    const f = authDialogFixture(capabilities);
    f.openAuthDialog(f.dialogs.register);
    assert.equal(f.dialogs.register.hidden, false);
    assert.deepEqual(f.effects, ['dismiss', 'open-register', 'focus-register']);
    assert.deepEqual(f.notices, []);
  }
});

test('known capabilities cannot bypass an auth attempt that refuses dismissal', () => {
  const f = authDialogFixture(supported, false);
  f.openAuthDialog(f.dialogs.login);
  assert.equal(f.dialogs.login.hidden, true);
  assert.deepEqual(f.effects, ['dismiss']);
});
