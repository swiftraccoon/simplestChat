import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript } from './source-loader.mjs';
import { createDOM } from './ui-fixture.mjs';

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

function classListStub(initial = []) {
  const classes = new Set(initial);
  return {
    classes,
    toggle(name, force) {
      const on = force ?? !classes.has(name);
      if (on) classes.add(name);
      else classes.delete(name);
      return on;
    },
    contains: (name) => classes.has(name),
    add: (...names) => names.forEach((name) => classes.add(name)),
    remove: (...names) => names.forEach((name) => classes.delete(name)),
  };
}

test('microphone and camera tooltips describe turning a device on, never unmuting something that was never on', async () => {
  const labels = [];
  const api = evaluateTypeScript(
    `let micMode = 'open';
     ${await functionSource('updateMicButton')}
     ${await functionSource('updateCamButton')}
     export { updateMicButton, updateCamButton };`,
    {
      globals: {
        micBtn: { classList: classListStub(), querySelector: () => null, appendChild() {} },
        camBtn: { classList: classListStub() },
        icons: { micOn: () => '', micOff: () => '', camOn: () => '', camOff: () => '' },
        setButtonContent: (_button, _icon, tooltip) => labels.push(tooltip),
        document: { createElement: () => ({}) },
      },
    },
  );
  api.updateMicButton(false);
  api.updateMicButton(true);
  api.updateCamButton(false);
  api.updateCamButton(true);
  assert.deepEqual(labels, [
    'Turn on mic (M)',
    'Turn off mic (M)',
    'Turn on camera (V)',
    'Turn off camera (V)',
  ]);
});

test('role badges explain their symbol to pointer and assistive users', async () => {
  const { document } = createDOM();
  const { getRoleBadgeSpan } = evaluateTypeScript(
    `const ROLE_SYMBOLS: Record<string, string> = { owner: '~', admin: '&', moderator: '@', member: '+' };
     const ROLE_NAMES: Record<string, string> = { owner: 'Owner', admin: 'Admin', moderator: 'Moderator', member: 'Member' };
     ${await functionSource('getRoleBadgeSpan')}
     export { getRoleBadgeSpan };`,
    { globals: { document } },
  );
  const badge = getRoleBadgeSpan('moderator');
  assert.equal(badge.textContent, '@');
  assert.equal(badge.title, 'Moderator');
  assert.equal(badge.getAttribute('role'), 'img', 'a labelled span needs a role');
  assert.equal(badge.getAttribute('aria-label'), 'Moderator');
  assert.equal(getRoleBadgeSpan('guest'), null);
});

test('toasts announce, mark errors, and dismiss on click', async () => {
  const { document, Node } = createDOM();
  Object.defineProperty(Node.prototype, 'style', {
    get() {
      return (this._style ??= {
        setProperty(name, value) {
          this[name] = value;
        },
      });
    },
  });
  const toastContainer = document.createElement('div');
  document.body.append(toastContainer);
  const { showToast } = evaluateTypeScript(
    `${await functionSource('showToast')} export { showToast };`,
    { globals: { document, toastContainer, setTimeout: () => 0 } },
  );
  showToast('Room link copied');
  showToast('Could not enable microphone', 8000, 'error');
  const [info, error] = toastContainer.children;
  assert.equal(info.className, 'toast');
  assert.equal(error.className, 'toast toast-error');
  assert.equal(error.getAttribute('role'), 'alert');
  assert.equal(info.style['--toast-life'], '3000ms', 'the fade-out follows the lifetime');
  assert.equal(error.style['--toast-life'], '8000ms');
  info.click();
  assert.equal(info.isConnected, false, 'a click dismisses the toast');
  assert.equal(error.isConnected, true);
});

test('entering a room moves focus into it and states that media stays off', async () => {
  const toasts = [];
  let focused = 0;
  const { announceRoomEntry } = evaluateTypeScript(
    `${await functionSource('announceRoomEntry')} export { announceRoomEntry };`,
    {
      globals: {
        roomScreen: { focus: () => focused++ },
        showToast: (message) => toasts.push(message),
      },
    },
  );
  announceRoomEntry('Design review');
  assert.equal(focused, 1);
  assert.deepEqual(toasts, [
    'Joined Design review. Your camera and microphone stay off until you turn them on.',
  ]);
});

test('phone layouts collapse the chat panel to its tab bar without touching desktop widths', async () => {
  const roomScreen = { classList: classListStub(), style: { setProperty() {} } };
  const rosterToggleBtn = { setAttribute() {} };
  const chatToggleBtn = { setAttribute() {} };
  const sidebarCollapseBtn = {
    attributes: {},
    setAttribute(k, v) {
      this.attributes[k] = v;
    },
  };
  const sidebar = {};
  const window = { innerWidth: 375 };
  const panelPreferences = {
    rosterWidth: 220,
    chatWidth: 320,
    rosterCollapsed: false,
    chatCollapsed: false,
    mobilePanelCollapsed: true,
  };
  const { applyPanelPreferences } = evaluateTypeScript(
    `${await functionSource('applyPanelPreferences')} export { applyPanelPreferences };`,
    {
      globals: {
        window,
        panelPreferences,
        roomScreen,
        rosterToggleBtn,
        chatToggleBtn,
        sidebarCollapseBtn,
        usersTab: null,
        getLayout: () => 'classic',
        selectSidebarTab() {},
        document: { getElementById: (id) => (id === 'sidebar' ? sidebar : null) },
      },
    },
  );
  applyPanelPreferences();
  assert.equal(roomScreen.classList.contains('mobile-panel-collapsed'), true);
  assert.equal(sidebarCollapseBtn.attributes['aria-expanded'], 'false');
  assert.equal(sidebar.inert, false, 'the tab bar stays usable while collapsed');
  window.innerWidth = 1200;
  applyPanelPreferences();
  assert.equal(roomScreen.classList.contains('mobile-panel-collapsed'), false);
  assert.equal(sidebarCollapseBtn.attributes['aria-expanded'], 'true');
});

test('the diagnostics entry lives on the home card outside a room and in the room tools inside one', async () => {
  const { document } = createDOM();
  const home = document.createElement('div');
  home.id = 'home-tools';
  const roomTools = document.createElement('div');
  roomTools.id = 'room-tools-right';
  const diagnosticsButton = document.createElement('button');
  document.body.append(home, roomTools);
  const { placeDiagnosticsButton } = evaluateTypeScript(
    `${await functionSource('placeDiagnosticsButton')} export { placeDiagnosticsButton };`,
    { globals: { document, diagnosticsButton } },
  );
  placeDiagnosticsButton(false);
  assert.equal(diagnosticsButton.parentNode, home);
  placeDiagnosticsButton(true);
  assert.equal(diagnosticsButton.parentNode, roomTools);
  placeDiagnosticsButton(false);
  assert.equal(diagnosticsButton.parentNode, home);
});

test('room settings show whether a password is set and can remove it explicitly', async () => {
  const fields = Object.fromEntries(
    [
      'rsModerated',
      'rsLobby',
      'rsScreen',
      'rsChat',
      'rsGuests',
      'rsGuestsBroadcast',
      'rsRequireReg',
      'rsInviteOnly',
      'rsSecret',
      'rsVideo',
      'rsPtt',
      'rsMaxBroadcasters',
      'rsMaxParticipants',
      'rsTopic',
      'rsPassword',
    ].map((name) => [name, { checked: false, value: '' }]),
  );
  const rsPasswordRemove = { hidden: true };
  const rsPasswordHint = { textContent: '' };
  const patches = [];
  const settings = {
    moderated: false,
    lobbyEnabled: false,
    allowScreenSharing: true,
    allowChat: true,
    guestsAllowed: true,
    guestsCanBroadcast: true,
    requireRegistration: false,
    inviteOnly: false,
    secret: false,
    allowVideo: true,
    pushToTalk: false,
    passwordProtected: true,
  };
  const api = evaluateTypeScript(
    `let roomSettingsPending = false;
     ${await functionSource('populateRoomSettingsModal')}
     ${await functionSource('removeRoomPassword')}
     export { populateRoomSettingsModal, removeRoomPassword };`,
    {
      globals: {
        ...fields,
        rsPasswordRemove,
        rsPasswordHint,
        room: { roomSettings: settings },
        applyRoomSetting: async (change) => {
          await change({ updateRoomSettings: async (patch) => patches.push(patch) });
        },
      },
    },
  );
  api.populateRoomSettingsModal();
  assert.equal(rsPasswordRemove.hidden, false);
  assert.match(rsPasswordHint.textContent, /password is set/i);
  settings.passwordProtected = false;
  api.populateRoomSettingsModal();
  assert.equal(rsPasswordRemove.hidden, true);
  assert.match(rsPasswordHint.textContent, /no password/i);
  await api.removeRoomPassword();
  assert.deepEqual(patches, [{ password: null }]);
});

test('being removed, denied or unable to join shows an owned dialog instead of a native alert', async () => {
  const notices = [];
  const alerts = [];
  const api = evaluateTypeScript(
    `let localTextMuted = false;
     export const events = { ${await roomEventSource(['onModeration', 'onLobbyDenied'])} };
     ${await functionSource('reportJoinFailure')}
     export { reportJoinFailure };`,
    {
      globals: {
        room: { localParticipantId: 'local', getParticipants: () => new Map() },
        alert: (message) => alerts.push(message),
        observeUiTask() {},
        leaveCurrentRoom: async () => {},
        applyRoomSettingsToUI() {},
        showToast() {},
        updateJoinBtn() {},
        joinBtn: { textContent: '' },
        console: { error() {} },
        showRoomExitNotice: (title, message) => notices.push({ title, message }),
      },
    },
  );
  api.events.onModeration('kicked', 'local', 'Be kind');
  api.events.onModeration('banned', 'local');
  api.events.onLobbyDenied('Full today');
  api.reportJoinFailure(new Error('Room is full'));
  assert.deepEqual(alerts, []);
  assert.deepEqual(notices, [
    { title: 'Removed from the room', message: 'You were kicked from this room. Reason: Be kind' },
    { title: 'Removed from the room', message: 'You were banned from this room.' },
    { title: 'Entry declined', message: 'A moderator declined your request to enter. Full today' },
    { title: 'Could not join', message: 'Room is full' },
  ]);
});

test('the exit notice is an owned dialog with a single way back home', async () => {
  const { document } = createDOM();
  const views = [];
  const { showRoomExitNotice } = evaluateTypeScript(
    `${await functionSource('showRoomExitNotice')} export { showRoomExitNotice };`,
    {
      globals: {
        document,
        modal: (title) => {
          const dialog = document.createElement('dialog');
          const body = document.createElement('div');
          dialog.append(body);
          const view = { title, dialog, body, close: () => dialog.close() };
          views.push(view);
          return view;
        },
        el: (tag, text, className) => {
          const node = document.createElement(tag);
          if (text !== undefined) node.textContent = text;
          if (className) node.className = className;
          return node;
        },
        button: (text, action, className) => {
          const node = document.createElement('button');
          node.textContent = text;
          node.className = className ?? '';
          node.addEventListener('click', action);
          return node;
        },
      },
    },
  );
  showRoomExitNotice('Removed from the room', 'You were kicked from this room.');
  assert.equal(views.length, 1);
  assert.equal(views[0].title, 'Removed from the room');
  assert.match(views[0].body.textContent, /You were kicked from this room\./);
  const back = views[0].body.querySelectorAll('button')[0];
  assert.equal(back.textContent, 'Back to home');
  views[0].dialog.open = true;
  back.click();
  assert.equal(views[0].dialog.open, false);
});

test('copying a room link without clipboard access offers the link for manual copying', async () => {
  const { document, Node } = createDOM();
  Node.prototype.focus = function () {
    this.focused = true;
  };
  Node.prototype.select = function () {
    this.selected = true;
  };
  const bodies = [];
  const { showCopyFallback } = evaluateTypeScript(
    `${await functionSource('showCopyFallback')} export { showCopyFallback };`,
    {
      globals: {
        document,
        modal: (title) => {
          const body = document.createElement('div');
          bodies.push({ title, body });
          return { dialog: document.createElement('dialog'), body, close() {} };
        },
        el: (tag, text, className) => {
          const node = document.createElement(tag);
          if (text !== undefined) node.textContent = text;
          if (className) node.className = className;
          return node;
        },
      },
    },
  );
  showCopyFallback('https://example.test/#room');
  assert.equal(bodies[0].title, 'Copy room link');
  const field = bodies[0].body.querySelector('input');
  assert.equal(field.value, 'https://example.test/#room');
  assert.equal(field.readOnly, true);
  assert.equal(field.focused, true);
  assert.equal(field.selected, true);
});

test('a room ID is suggested from the display name until the ID is edited by hand', async () => {
  const { suggestRoomId } = evaluateTypeScript(
    `${await functionSource('suggestRoomId')} export { suggestRoomId };`,
  );
  assert.equal(suggestRoomId('Design Review'), 'design-review');
  assert.equal(suggestRoomId('  Café — Crème!! '), 'cafe-creme');
  assert.equal(suggestRoomId('___'), '');
  assert.equal(suggestRoomId('a'.repeat(200)).length, 64);
});
