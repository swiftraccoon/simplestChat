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

test('toasts announce, mark errors, fade just before removal and dismiss on click', async () => {
  const { document, Node } = createDOM();
  Object.defineProperty(Node.prototype, 'classList', {
    get() {
      return (this._classList ??= classListStub(
        (this.className ?? '').split(/\s+/).filter(Boolean),
      ));
    },
  });
  const timers = [];
  const toastContainer = document.createElement('div');
  document.body.append(toastContainer);
  const { showToast } = evaluateTypeScript(
    `${await functionSource('showToast')} export { showToast };`,
    {
      globals: {
        document,
        toastContainer,
        setTimeout: (callback, delay) => timers.push({ callback, delay }),
      },
    },
  );
  showToast('Room link copied');
  showToast('Could not enable microphone', 8000, 'error');
  const [info, error] = toastContainer.children;
  assert.equal(info.className, 'toast');
  assert.equal(error.className, 'toast toast-error');
  assert.equal(error.getAttribute('role'), 'alert');
  assert.deepEqual(
    timers.map((timer) => timer.delay),
    [2700, 3000, 7700, 8000],
    'the fade starts 300 ms before removal, whatever the lifetime',
  );
  timers[0].callback();
  assert.equal(info.classList.contains('toast-leaving'), true);
  timers[1].callback();
  assert.equal(info.isConnected, false);
  error.click();
  assert.equal(error.isConnected, false, 'a click dismisses the toast');
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
  // Each toggle keeps its icon; only the label span's text changes.
  const toggle = () => {
    const label = { textContent: '' };
    return { label, setAttribute() {}, querySelector: () => label };
  };
  const rosterToggleBtn = toggle();
  const chatToggleBtn = toggle();
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
        isDesktopLayout: () => window.innerWidth > 768,
        isMobilePanelCollapsed: () =>
          window.innerWidth <= 768 && panelPreferences.mobilePanelCollapsed,
        selectSidebarTab() {},
        document: {
          getElementById: (id) => (id === 'sidebar' ? sidebar : null),
          documentElement: { style: { setProperty() {} } },
        },
      },
    },
  );
  applyPanelPreferences();
  assert.equal(rosterToggleBtn.label.textContent, 'People');
  assert.equal(chatToggleBtn.label.textContent, 'Chat');
  assert.equal(roomScreen.classList.contains('mobile-panel-collapsed'), true);
  assert.equal(sidebarCollapseBtn.attributes['aria-expanded'], 'false');
  assert.equal(sidebar.inert, false, 'the tab bar stays usable while collapsed');
  window.innerWidth = 1200;
  applyPanelPreferences();
  assert.equal(roomScreen.classList.contains('mobile-panel-collapsed'), false);
  assert.equal(sidebarCollapseBtn.attributes['aria-expanded'], 'true');
});

test('the desktop layout ends where style.css starts stacking or splitting the room', async () => {
  const css = await readFile(new URL('../src/style.css', import.meta.url), 'utf8');
  const queries = [];
  let matches = false;
  const window = {
    matchMedia(query) {
      queries.push(query);
      return { matches };
    },
  };
  const { isDesktopLayout } = evaluateTypeScript(
    `${await functionSource('isDesktopLayout')} export { isDesktopLayout };`,
    { globals: { window } },
  );
  assert.equal(isDesktopLayout(), true);
  matches = true;
  assert.equal(isDesktopLayout(), false);
  const parts = queries[0].split(',').map((part) => part.trim());
  assert.ok(parts.some((part) => part.includes('orientation: landscape')));
  for (const part of parts)
    assert.ok(css.includes(`@media ${part} {`), `style.css has no @media ${part}`);
});

test('a landscape phone in the classic layout lists people in the sidebar tab', async () => {
  const roomScreen = { classList: classListStub(), style: { setProperty() {} } };
  const button = () => ({ setAttribute() {}, querySelector: () => ({}) });
  const usersTab = { hidden: true, classList: { contains: () => false } };
  let desktop = false;
  const { applyPanelPreferences } = evaluateTypeScript(
    `${await functionSource('applyPanelPreferences')} export { applyPanelPreferences };`,
    {
      globals: {
        window: { innerWidth: 844 },
        panelPreferences: {
          rosterWidth: 220,
          chatWidth: 320,
          rosterCollapsed: false,
          chatCollapsed: false,
          mobilePanelCollapsed: false,
        },
        roomScreen,
        rosterToggleBtn: button(),
        chatToggleBtn: button(),
        sidebarCollapseBtn: button(),
        usersTab,
        getLayout: () => 'classic',
        isDesktopLayout: () => desktop,
        isMobilePanelCollapsed: () => false,
        selectSidebarTab() {},
        document: {
          getElementById: (id) => (id === 'sidebar' ? {} : null),
          documentElement: { style: { setProperty() {} } },
        },
      },
    },
  );
  applyPanelPreferences();
  assert.equal(usersTab.hidden, false, 'the roster column is hidden, so the tab lists people');
  desktop = true;
  applyPanelPreferences();
  assert.equal(usersTab.hidden, true, 'the desktop roster column lists them instead');
});

test('the diagnostics entry lives on the home card outside a room and in the More menu inside one', async () => {
  const { document } = createDOM();
  const home = document.createElement('div');
  home.id = 'home-tools';
  const roomTools = document.createElement('div');
  roomTools.id = 'room-more-repair';
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

test('the More menu opens on its button, closes on a choice, Escape or a click away, and keeps focus sensible', async () => {
  const state = { focused: null, documentClick: [] };
  const node = (name, parent = null) => ({
    name,
    parent,
    hidden: true,
    attributes: {},
    listeners: [],
    setAttribute(key, value) {
      this.attributes[key] = String(value);
    },
    getAttribute(key) {
      return this.attributes[key] ?? null;
    },
    addEventListener(type, handler, capture) {
      this.listeners.push({ type, handler, capture: capture === true });
    },
    focus() {
      state.focused = this;
    },
    contains(other) {
      for (let current = other; current; current = current.parent)
        if (current === this) return true;
      return false;
    },
    closest() {
      return this.name.startsWith('item') ? this : null;
    },
  });
  const wrapper = node('wrapper');
  const toggle = node('toggle', wrapper);
  toggle.parentElement = wrapper;
  const menu = node('menu', wrapper);
  const item = node('item', menu);
  menu.querySelector = () => item;
  const fire = (target, type, event = {}) => {
    const path = [];
    for (let current = target; current; current = current.parent) path.unshift(current);
    const payload = { target, stopPropagation() {}, ...event };
    for (const phase of [true, false])
      for (const current of phase ? path : [...path].reverse())
        for (const entry of current.listeners)
          if (entry.type === type && entry.capture === phase) entry.handler(payload);
    if (type === 'click') for (const handler of state.documentClick) handler(payload);
  };
  const { setupRoomMenu } = evaluateTypeScript(
    `${await functionSource('setupRoomMenu')} export { setupRoomMenu };`,
    {
      globals: {
        document: {
          getElementById: (id) => ({ 'room-more-btn': toggle, 'room-more-menu': menu })[id],
          addEventListener: (type, handler) =>
            type === 'click' && state.documentClick.push(handler),
        },
      },
    },
  );
  setupRoomMenu();
  fire(toggle, 'click');
  assert.equal(menu.hidden, false);
  assert.equal(toggle.attributes['aria-expanded'], 'true');
  assert.equal(state.focused, item, 'opening focuses the first choice');
  fire(item, 'click');
  assert.equal(menu.hidden, true, 'a choice closes the menu');
  assert.equal(state.focused, toggle, 'a dialog the choice opens will return focus to More');
  fire(toggle, 'click');
  fire(item, 'keydown', { key: 'Escape' });
  assert.equal(menu.hidden, true);
  assert.equal(toggle.attributes['aria-expanded'], 'false');
  fire(toggle, 'click');
  fire(node('elsewhere'), 'click');
  assert.equal(menu.hidden, true, 'a click elsewhere closes it');
  fire(toggle, 'click');
  fire(toggle, 'click');
  assert.equal(menu.hidden, true, 'the button toggles it');
});

test('one pinned tile fills the stage, and a tile that is gone cannot stay pinned', async () => {
  const classes = () => {
    const names = new Set();
    return {
      names,
      toggle: (name, on) => (on ? names.add(name) : names.delete(name)),
      contains: (name) => names.has(name),
    };
  };
  const tile = () => {
    const pin = {
      attributes: {},
      setAttribute(key, value) {
        this.attributes[key] = value;
      },
    };
    return { pin, dataset: {}, classList: classes(), querySelector: () => pin };
  };
  const remoteTiles = new Map([
    ['alice', tile()],
    ['bob:screen', tile()],
  ]);
  const videoGrid = { classList: classes() };
  const { setPinnedTile, pinned } = evaluateTypeScript(
    `let pinnedTileKey = null;
    ${await functionSource('setPinnedTile')}
    export { setPinnedTile };
    export const pinned = () => pinnedTileKey;`,
    { globals: { remoteTiles, videoGrid, room: { setPinnedRemoteVideo() {} } } },
  );
  setPinnedTile('bob:screen');
  assert.equal(pinned(), 'bob:screen');
  assert.equal(remoteTiles.get('bob:screen').classList.contains('pinned'), true);
  assert.equal(remoteTiles.get('bob:screen').pin.attributes['aria-pressed'], 'true');
  assert.equal(remoteTiles.get('alice').pin.attributes['aria-pressed'], 'false');
  assert.equal(videoGrid.classList.contains('has-pinned'), true);
  setPinnedTile('alice');
  assert.equal(remoteTiles.get('bob:screen').classList.contains('pinned'), false, 'only one');
  setPinnedTile('gone');
  assert.equal(pinned(), null);
  assert.equal(videoGrid.classList.contains('has-pinned'), false);
});

test('forgetting this device clears every key the app wrote and nothing else', async () => {
  const stored = new Map([
    ['displayName', 'Maya'],
    ['micMode', 'ptt'],
    ['layout', 'classic'],
    ['panelPreferences', '{}'],
    ['reliabilityTelemetry', '1'],
    ['simplestchat.chat.v1.account-a', '{}'],
    ['simplestchat.capturePreferences', '{}'],
    ['someone-elses-key', 'kept'],
  ]);
  const localStorage = {
    get length() {
      return stored.size;
    },
    key: (index) => [...stored.keys()][index] ?? null,
    removeItem: (key) => stored.delete(key),
  };
  const { forgetThisDevice } = evaluateTypeScript(
    `${await functionSource('forgetThisDevice')} export { forgetThisDevice };`,
    { globals: { localStorage } },
  );
  forgetThisDevice();
  assert.deepEqual([...stored.keys()], ['someone-elses-key']);
});

test('blocked storage keeps optional preferences in memory without interrupting initialization', async () => {
  const source = `const memoryPreferences = new Map(); ${await functionSource('readLocalPreference')} ${await functionSource('writeLocalPreference')} export { readLocalPreference, writeLocalPreference };`;
  for (const mode of ['get', 'read', 'write']) {
    const storage = {
      getItem: () => {
        if (mode === 'read') throw new Error('Blocked');
        return 'saved';
      },
      setItem: () => {
        if (mode === 'write') throw new Error('Full');
      },
    };
    const blocked = {
      get localStorage() {
        if (mode === 'get') throw new Error('Blocked');
        return storage;
      },
    };
    // The VM global is accessed lazily, just like a browser's storage getter.
    const api = evaluateTypeScript(source.replaceAll('localStorage.', 'browser.localStorage.'), {
      globals: { browser: blocked },
    });
    assert.equal(
      api.readLocalPreference('layout'),
      mode === 'get' || mode === 'read' ? null : 'saved',
    );
    api.writeLocalPreference('layout', 'modern');
    assert.equal(api.readLocalPreference('layout'), 'modern');
  }
});

for (const outcome of ['success', 'failure', 'replacement', 'blocked-storage']) {
  test(`ordinary sign-out clears saved identity only after owned success: ${outcome}`, async () => {
    const task = Promise.withResolvers();
    const auth = { isLoggedIn: true, logout: () => task.promise };
    const memoryPreferences = new Map([['displayName', 'Before']]);
    const nameInput = { value: 'Before' };
    let forgotten = 0,
      reloaded = 0;
    const api = evaluateTypeScript(
      `let inviteAccountEpoch = 0; ${await functionSource('signOutAndForget')} export {signOutAndForget}; export function next() {inviteAccountEpoch++;}`,
      {
        globals: {
          auth,
          memoryPreferences,
          nameInput,
          forgetThisDevice: () => {
            if (outcome === 'blocked-storage') throw new Error('Blocked');
            forgotten++;
          },
          location: { reload: () => reloaded++ },
        },
      },
    );
    const pending = api.signOutAndForget();
    assert.equal(forgotten, 0);
    assert.equal(nameInput.value, 'Before');
    if (outcome === 'failure') {
      task.reject(new Error('Revoke failed'));
      await assert.rejects(pending, /Revoke failed/);
    } else {
      api.next();
      auth.isLoggedIn = outcome === 'replacement';
      task.resolve();
      if (outcome === 'blocked-storage')
        await assert.rejects(pending, /Signed out.*clear.*site data/i);
      else await pending;
    }
    assert.equal(forgotten, outcome === 'success' ? 1 : 0);
    assert.equal(reloaded, outcome === 'success' ? 1 : 0);
    assert.equal(memoryPreferences.size, ['success', 'blocked-storage'].includes(outcome) ? 0 : 1);
    assert.equal(nameInput.value, ['success', 'blocked-storage'].includes(outcome) ? '' : 'Before');
    const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
    assert.match(source, /logoutBtn.addEventListener\('click', \(\) => \{\s*signOutAndForget\(\)/);
  });
}

test('room tiles get a stable tone from the room id, always from the warm palette', async () => {
  const api = evaluateTypeScript(`${await functionSource('roomTone')} export { roomTone };`, {
    globals: {},
  });
  const ids = ['lobby', 'reading-group', 'design-review', 'quiet-study', 'town-hall', 'x'];
  const tones = ids.map((id) => api.roomTone(id));
  for (const tone of tones) assert.match(tone, /^#[0-9a-f]{6}$/);
  assert.equal(api.roomTone('lobby'), api.roomTone('lobby'));
  assert.ok(new Set(tones).size > 1, 'different rooms spread across the palette');
});

test('a directory tile enters its room once a name is known and only selects it otherwise', async () => {
  const calls = [];
  const nameInput = {
    value: '',
    focused: 0,
    focus() {
      this.focused++;
    },
  };
  const auth = { displayName: '' };
  const { enterRoomFromDirectory } = evaluateTypeScript(
    `${await functionSource('enterRoomFromDirectory')} export { enterRoomFromDirectory };`,
    {
      globals: {
        auth,
        nameInput,
        navigation: {
          requestJoin: (id) => calls.push(['join', id]),
          selectRoom: (id) => calls.push(['select', id]),
        },
        updateJoinBtn() {},
      },
    },
  );
  enterRoomFromDirectory('reading-group');
  assert.deepEqual(calls, [['select', 'reading-group']]);
  assert.equal(nameInput.focused, 1, 'without a name the tile asks the join bar for one');
  nameInput.value = ' Guest ';
  enterRoomFromDirectory('reading-group');
  assert.deepEqual(calls.at(-1), ['join', 'reading-group']);
  auth.displayName = 'Owner';
  nameInput.value = '';
  enterRoomFromDirectory('lobby');
  assert.equal(nameInput.value, 'Owner', 'an account name fills the join bar');
  assert.deepEqual(calls.at(-1), ['join', 'lobby']);
  assert.equal(nameInput.focused, 1, 'a known name needs no focus move');
});
