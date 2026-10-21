import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { loadTypeScript } from './native-fixtures.mjs';
import { createLocalPreferences, signOutAndForget } from '../src/local-preferences.ts';
import { createDirectoryEntry } from '../src/room-entry.ts';
import { createDOM } from './ui-fixture.mjs';
import { roomScreenFixture } from './room-screen-fixture.mjs';

async function toastFixture(globals) {
  const { createToasts } = await loadTypeScript('src/toasts.ts', {
    globals: { setTimeout: globals.setTimeout },
  });
  return createToasts(globals.document, globals.toastContainer);
}
async function chromeFixture(globals) {
  const bindings = globals.document ? { document: globals.document } : {};
  const ui = await loadTypeScript('src/ui.ts', { globals: bindings });
  const overrides = Object.fromEntries(
    ['modal', 'el', 'button'].filter((name) => globals[name]).map((name) => [name, globals[name]]),
  );
  const chrome = await loadTypeScript('src/screens/chrome.ts', {
    modules: { '../ui': { ...ui, ...overrides } },
    globals: bindings,
  });
  return {
    ...chrome,
    announceRoomEntry: (name) =>
      chrome.announceRoomEntry(globals.roomScreen, globals.showToast, name),
    placeDiagnosticsButton: (inRoom) =>
      chrome.placeDiagnosticsButton(
        { homeTools: globals.homeTools, roomMoreRepair: globals.roomMoreRepair },
        globals.diagnosticsButton,
        inRoom,
      ),
    setupRoomMenu: () => chrome.setupMenu(globals.roomMoreBtn, globals.roomMoreMenu),
    setupAccountMenu: () => chrome.setupMenu(globals.accountMenuBtn, globals.accountMenu),
  };
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
  const f = await roomScreenFixture();
  f.controls.updateMicButton(false);
  f.controls.updateMicButton(true);
  f.controls.updateCamButton(false);
  f.controls.updateCamButton(true);
  assert.deepEqual(
    f.state.buttonUpdates.map(({ tooltip }) => tooltip),
    ['Turn on mic (M)', 'Turn off mic (M)', 'Turn on camera (V)', 'Turn off camera (V)'],
  );
  f.controller.dispose();
});

test('role badges explain their symbol to pointer and assistive users', async () => {
  const f = await roomScreenFixture();
  const badge = f.controller.getRoleBadgeSpan('moderator');
  assert.equal(badge.textContent, '@');
  assert.equal(badge.title, 'Moderator');
  assert.equal(badge.getAttribute('role'), 'img', 'a labelled span needs a role');
  assert.equal(badge.getAttribute('aria-label'), 'Moderator');
  assert.equal(f.controller.getRoleBadgeSpan('guest'), null);
  assert.equal(f.controller.getRoleBadgeSpan('user'), null);
  f.controller.dispose();
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
  const { showToast } = await toastFixture({
    document,
    toastContainer,
    setTimeout: (callback, delay) => timers.push({ callback, delay }),
  });
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
  const { announceRoomEntry } = await chromeFixture({
    roomScreen: { focus: () => focused++ },
    showToast: (message) => toasts.push(message),
  });
  announceRoomEntry('Design review');
  assert.equal(focused, 1);
  assert.deepEqual(toasts, [
    'Joined Design review. Your camera and microphone stay off until you turn them on.',
  ]);
});

test('the diagnostics entry lives on the home card outside a room and in the More menu inside one', async () => {
  const { document } = createDOM();
  const home = document.createElement('div');
  home.id = 'home-tools';
  const roomTools = document.createElement('div');
  roomTools.id = 'room-more-repair';
  const diagnosticsButton = document.createElement('button');
  document.body.append(home, roomTools);
  const { placeDiagnosticsButton } = await chromeFixture({
    diagnosticsButton,
    homeTools: home,
    roomMoreRepair: roomTools,
  });
  placeDiagnosticsButton(false);
  assert.equal(diagnosticsButton.parentNode, home);
  placeDiagnosticsButton(true);
  assert.equal(diagnosticsButton.parentNode, roomTools);
  placeDiagnosticsButton(false);
  assert.equal(diagnosticsButton.parentNode, home);
});

test('being removed, denied or unable to join shows an owned dialog instead of a native alert', async () => {
  const notices = [];
  const alerts = [];
  for (const [action, reason] of [
    ['kicked', 'Be kind'],
    ['banned', undefined],
  ]) {
    const f = await roomScreenFixture({ globals: { alert: (message) => alerts.push(message) } });
    await f.connect({ localParticipantId: 'local' });
    f.state.events.onModeration(action, 'local', reason);
    await f.settled();
    notices.push(...f.state.notices);
    f.controller.dispose();
  }
  const denied = await roomScreenFixture({ globals: { alert: (message) => alerts.push(message) } });
  await denied.connect();
  denied.state.events.onLobbyDenied('Full today');
  await denied.settled();
  notices.push(...denied.state.notices);
  denied.controller.dispose();
  const failed = await roomScreenFixture({ globals: { alert: (message) => alerts.push(message) } });
  await failed.connect({
    join: async () => {
      throw new Error('Room is full');
    },
  });
  await failed.settled();
  notices.push(...failed.state.notices);
  failed.controller.dispose();
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
  const { showRoomExitNotice } = await chromeFixture({
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
  });
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
  const { showCopyFallback } = await chromeFixture({
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
  });
  showCopyFallback('https://example.test/#room');
  assert.equal(bodies[0].title, 'Copy room link');
  const field = bodies[0].body.querySelector('input');
  assert.equal(field.value, 'https://example.test/#room');
  assert.equal(field.readOnly, true);
  assert.equal(field.focused, true);
  assert.equal(field.selected, true);
});

for (const [label, setup, toggleName, menuName] of [
  ['More', 'setupRoomMenu', 'roomMoreBtn', 'roomMoreMenu'],
  ['account', 'setupAccountMenu', 'accountMenuBtn', 'accountMenu'],
]) {
  test(`the ${label} menu opens on its button, closes on a choice, Escape or a click away, and keeps focus sensible`, async () => {
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
        this.listeners.push({
          type,
          handler,
          capture: capture === true || capture?.capture === true,
        });
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
    const { [setup]: setupMenu } = await chromeFixture({
      [toggleName]: toggle,
      [menuName]: menu,
      document: {
        addEventListener: (type, handler) => type === 'click' && state.documentClick.push(handler),
      },
    });
    setupMenu();
    fire(toggle, 'click');
    assert.equal(menu.hidden, false);
    assert.equal(toggle.attributes['aria-expanded'], 'true');
    assert.equal(state.focused, item, 'opening focuses the first choice');
    let prevented = false;
    fire(item, 'mousedown', {
      button: 0,
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, true, 'primary pointer activation preserves focus until click');
    prevented = false;
    fire(item, 'mousedown', {
      button: 2,
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, false, 'secondary pointer actions retain their native behavior');
    const checkboxLabel = node('checkbox-label', menu);
    const checkbox = node('checkbox', checkboxLabel);
    checkbox.checked = false;
    checkbox.closest = (selector) =>
      selector.includes('input[type="checkbox"]')
        ? checkbox
        : selector === 'label'
          ? checkboxLabel
          : null;
    checkboxLabel.closest = (selector) => (selector === 'label' ? checkboxLabel : null);
    checkboxLabel.querySelector = () => checkbox;
    for (const target of [checkbox, checkboxLabel]) {
      prevented = false;
      fire(target, 'mousedown', {
        button: 0,
        preventDefault() {
          prevented = true;
        },
      });
      // Safari's native pointer focus transition would otherwise blur the first
      // choice to the document before activating a checkbox or its label.
      if (!prevented) fire(item, 'focusout', { relatedTarget: null });
      if (!menu.hidden) {
        fire(target, 'click');
        checkbox.checked = !checkbox.checked;
      }
      assert.equal(
        checkbox.checked,
        target === checkbox,
        'checkbox and label both activate without losing the menu',
      );
      assert.equal(menu.hidden, false);
    }
    const rangeLabel = node('range-label', menu);
    const range = node('range', rangeLabel);
    range.closest = (selector) => (selector === 'label' ? rangeLabel : null);
    rangeLabel.querySelector = () => null;
    prevented = false;
    fire(range, 'mousedown', {
      button: 0,
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, false, 'range controls retain native focus and dragging');
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
}

test('one pinned tile fills the stage, and a tile that is gone cannot stay pinned', async () => {
  const f = await roomScreenFixture({ room: {} });
  f.controls.renderDeferredVideo('alice', 'Alice', undefined, 'offscreen');
  f.controls.renderDeferredVideo('bob', 'Bob', 'screen', 'offscreen');
  const alice = f.controls.tiles.get('alice');
  const screen = f.controls.tiles.get('bob:screen');
  f.controls.setPinnedTile('bob:screen');
  assert.equal(screen.classList.contains('pinned'), true);
  assert.equal(screen.querySelector('.tile-pin').getAttribute('aria-pressed'), 'true');
  assert.equal(alice.querySelector('.tile-pin').getAttribute('aria-pressed'), 'false');
  assert.equal(f.elements.videoGrid.classList.contains('has-pinned'), true);
  assert.equal(screen.style.order, '-3');
  f.controls.setPinnedTile('alice');
  assert.equal(screen.classList.contains('pinned'), false, 'only one');
  assert.equal(alice.classList.contains('pinned'), true);
  f.controls.setPinnedTile('gone');
  assert.equal(alice.classList.contains('pinned'), false);
  assert.equal(f.elements.videoGrid.classList.contains('has-pinned'), false);
  f.controller.dispose();
});

test('following changes tile order while preserving shared screens, a manual pin and uniform dimensions', async () => {
  const f = await roomScreenFixture();
  await f.connect();
  for (const id of ['alice', 'bob', 'alice:screen', 'carol']) {
    f.controls.renderDeferredVideo(
      id.split(':')[0],
      id,
      id.endsWith(':screen') ? 'screen' : undefined,
      'offscreen',
    );
    Object.assign(f.controls.tiles.get(id).style, { width: '160px', height: '90px' });
  }
  f.controls.setPinnedTile('carol');
  f.state.events.onFollowedSpeakerChanged('bob');
  assert.equal(f.controls.tiles.get('carol').style.order, '-3');
  assert.equal(f.controls.tiles.get('alice:screen').style.order, '-2');
  assert.equal(f.controls.tiles.get('bob').style.order, '-1');
  assert.equal(f.controls.tiles.get('alice').style.order, '');
  for (const tile of f.controls.tiles.values())
    assert.deepEqual([tile.style.width, tile.style.height], ['160px', '90px']);
  f.state.events.onFollowedSpeakerChanged(null);
  assert.equal(f.controls.tiles.get('carol').style.order, '-3');
  for (const id of ['alice', 'bob', 'alice:screen'])
    assert.equal(f.controls.tiles.get(id).style.order, '');
  f.controller.dispose();
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
  const { forgetThisDevice } = await loadTypeScript('src/local-preferences.ts');
  forgetThisDevice(localStorage);
  assert.deepEqual([...stored.keys()], ['someone-elses-key']);
});

test('blocked storage keeps optional preferences in memory without interrupting initialization', async () => {
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
    // The storage getter remains lazy, so a blocked access cannot stop initialization.
    const api = createLocalPreferences(() => blocked.localStorage);
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
    let epoch = 0;
    const task = Promise.withResolvers();
    const auth = { isLoggedIn: true, logout: () => task.promise };
    const memoryPreferences = new Map([['displayName', 'Before']]);
    const nameInput = { value: 'Before' };
    let forgotten = 0,
      reloaded = 0;
    const api = {
      signOutAndForget: () =>
        signOutAndForget({
          auth,
          nameInput,
          accountEpoch: () => epoch,
          clearPreferences: () => memoryPreferences.clear(),
          forgetDevice: () => {
            if (outcome === 'blocked-storage') throw new Error('Blocked');
            forgotten++;
          },
          reload: () => reloaded++,
        }),
      next: () => {
        epoch++;
      },
    };
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
    const source = await readFile(new URL('../src/app.ts', import.meta.url), 'utf8');
    assert.match(
      source,
      /logoutBtn\.addEventListener\(\s*'click',\s*\(\) => \{\s*signOutAndForget\(\)/,
    );
  });
}

test('room tiles get a stable tone from the room id, always from the warm palette', async () => {
  const api = await loadTypeScript('src/room-entry.ts');
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
  const { enterRoomFromDirectory } = {
    enterRoomFromDirectory: createDirectoryEntry({
      nameInput,
      getDisplayName: () => auth.displayName,
      requestJoin: (id) => calls.push(['join', id]),
      selectRoom: (id) => calls.push(['select', id]),
      updateJoin() {},
    }),
  };
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
