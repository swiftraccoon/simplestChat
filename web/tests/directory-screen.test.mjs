import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { createDOM, deferred } from './ui-fixture.mjs';

async function fixture() {
  const dom = createDOM();
  const createElement = dom.document.createElement;
  dom.document.createElement = (tag) => {
    const node = createElement(tag);
    node.dataset = {};
    const classes = new Set();
    node.classList = {
      toggle(name, enabled) {
        if (enabled) classes.add(name);
        else classes.delete(name);
      },
      contains: (name) => classes.has(name),
    };
    node.style = { setProperty() {} };
    node.closest = (selector) => {
      for (let current = node; current; current = current.parentNode)
        if (selector.startsWith('.') && current.className?.split(' ').includes(selector.slice(1)))
          return current;
      return null;
    };
    node.contains = (target) => {
      for (let current = target; current; current = current.parentNode)
        if (current === node) return true;
      return false;
    };
    node.click = () => {
      if (node.disabled) return;
      for (let current = node; current; current = current.parentNode)
        current.emit('click', { target: node });
    };
    const add = node.addEventListener.bind(node);
    node.addEventListener = (type, callback, options = {}) =>
      add(
        type,
        (event) => {
          if (!options.signal?.aborted) callback(event);
        },
        options,
      );
    return node;
  };
  const elements = Object.fromEntries(
    [
      'roomBrowser',
      'roomSearchInput',
      'roomList',
      'roomLoadMore',
      'savedRoomPicker',
      'savedRoomContainer',
      'directoryRailToggle',
    ].map((key) => [key, dom.document.createElement(key === 'roomLoadMore' ? 'button' : 'div')]),
  );
  dom.document.body.append(elements.roomBrowser, elements.savedRoomPicker);
  elements.roomBrowser.append(elements.roomList, elements.roomSearchInput, elements.roomLoadMore);
  elements.savedRoomPicker.append(elements.savedRoomContainer, elements.directoryRailToggle);
  const store = {
    capabilities: { roomDirectory: true },
    accountId: null,
    token: null,
    isLoggedIn: false,
    selectedRoomId: '',
  };
  const calls = [],
    tasks = [],
    joins = [],
    mounts = [],
    favorites = [];
  const preferences = new Map([['simplestchat.directoryRail', 'collapsed']]);
  const timers = new Map();
  let nextTimer = 0;
  const api = {
    rooms: async (params, signal) => {
      calls.push({ params, signal });
      return [];
    },
    savedRooms: async () => ({ rooms: [] }),
  };
  class ApiError extends Error {
    constructor(status) {
      super('API failure');
      this.status = status;
    }
  }
  const { DirectoryScreen, hasIndexableRoomSearchTrigram } = await loadTypeScript(
    'src/screens/directory.ts',
    {
      modules: {
        '../ui': {
          api,
          ApiError,
          safeRasterUrl: () => false,
          el: (tag, text, className) => {
            const node = dom.document.createElement(tag);
            if (text !== undefined) node.textContent = text;
            node.className = className;
            return node;
          },
        },
        '../discovery': {
          createFavoriteButton(id, favorite, options) {
            favorites.push({ id, favorite, options });
            return dom.document.createElement('button');
          },
          mountSavedRooms(host, options) {
            const mount = {
              host,
              options,
              disposed: false,
              refreshes: 0,
              async refresh() {
                this.refreshes++;
              },
              dispose() {
                this.disposed = true;
              },
            };
            mounts.push(mount);
            return mount;
          },
        },
      },
      globals: {
        document: dom.document,
        Element: dom.Node,
        setTimeout: (callback) => {
          const id = ++nextTimer;
          timers.set(id, callback);
          return id;
        },
        clearTimeout: (id) => timers.delete(id),
      },
    },
  );
  const screen = new DirectoryScreen(elements, store, {
    enterRoom: (id) => joins.push(id),
    observe: (task) => tasks.push(task),
    notify() {},
    readPreference: (key) => preferences.get(key) ?? null,
    writePreference: (key, value) => preferences.set(key, value),
    tone: () => '#2f3a2a',
  });
  return {
    ...dom,
    elements,
    store,
    api,
    ApiError,
    calls,
    tasks,
    joins,
    mounts,
    favorites,
    preferences,
    timers,
    screen,
    hasIndexableRoomSearchTrigram,
    tick() {
      const pending = [...timers.values()];
      timers.clear();
      for (const callback of pending) callback();
    },
  };
}
const room = (id = 'public') => ({
  id,
  display_name: '<Public room>',
  topic: 'Topic',
  description: '',
  participant_count: null,
  broadcaster_count: null,
  password_protected: false,
  moderated: false,
  image_url: null,
});

test('the directory rail remembers its choice and stops listening on disposal', async () => {
  const f = await fixture();
  const { savedRoomPicker: rail, directoryRailToggle: toggle } = f.elements;
  assert.equal(rail.classList.contains('collapsed'), true);
  assert.equal(toggle.getAttribute('aria-expanded'), 'false');
  assert.equal(toggle.getAttribute('aria-label'), 'Show favorites and recent rooms');
  toggle.click();
  assert.equal(rail.classList.contains('collapsed'), false);
  assert.equal(toggle.getAttribute('aria-expanded'), 'true');
  assert.equal(toggle.getAttribute('aria-label'), 'Hide favorites and recent rooms');
  assert.equal(f.preferences.get('simplestchat.directoryRail'), 'open');
  toggle.click();
  assert.equal(f.preferences.get('simplestchat.directoryRail'), 'collapsed');
  f.screen.dispose();
  toggle.click();
  assert.equal(f.preferences.get('simplestchat.directoryRail'), 'collapsed');
});

test('a newer directory read owns rendering even when an aborted read completes later', async () => {
  const f = await fixture();
  const first = deferred(),
    second = deferred();
  const pending = [first, second];
  f.api.rooms = (params, signal) => {
    f.calls.push({ params, signal });
    return pending.shift().promise;
  };
  const old = f.screen.load();
  const current = f.screen.load();
  assert.equal(f.calls[0].signal.aborted, true);
  second.resolve([room('new')]);
  await current;
  first.resolve([room('old')]);
  await old;
  const cards = f.elements.roomList.querySelectorAll('.room-card');
  assert.deepEqual(
    cards.map((card) => card.dataset.roomId),
    ['new'],
    f.elements.roomList.textContent,
  );
  assert.match(cards[0].textContent, /\? online · \? broadcasting/);
  assert.match(cards[0].textContent, /<Public room>/);
  assert.equal(cards[0].querySelector('script'), null);
  cards[0].click();
  assert.deepEqual(f.joins, ['new']);
  let prevented = false;
  f.elements.roomBrowser.emit('keydown', {
    target: cards[0],
    key: ' ',
    preventDefault() {
      prevented = true;
    },
  });
  assert.equal(prevented, true);
  assert.deepEqual(f.joins, ['new', 'new']);
  f.screen.dispose();
  cards[0].click();
  assert.deepEqual(f.joins, ['new', 'new']);
});

test('directory results and favorite controls belong to the account that requested them', async () => {
  const f = await fixture();
  f.store.accountId = 'owner';
  f.store.token = 'token';
  f.store.isLoggedIn = true;
  const pending = deferred();
  f.api.rooms = () => pending.promise;
  const read = f.screen.load();
  f.store.accountId = 'replacement';
  pending.resolve([room()]);
  await read;
  assert.equal(f.elements.roomList.children.length, 0);
  f.api.rooms = async () => [room()];
  await f.screen.load();
  assert.equal(f.favorites[0].options.current(), true);
  f.store.accountId = 'third';
  assert.equal(f.favorites[0].options.current(), false);
});

test('search debounce rejects short fragments and cancels an older read', async () => {
  const f = await fixture();
  for (const query of ['ab', 'a-b-c', 'ééé'])
    assert.equal(f.hasIndexableRoomSearchTrigram(query), false);
  for (const query of ['abc', 'A12', '-foo-'])
    assert.equal(f.hasIndexableRoomSearchTrigram(query), true);
  const pending = deferred();
  f.api.rooms = (params, signal) => {
    f.calls.push({ params, signal });
    return pending.promise;
  };
  const read = f.screen.load();
  f.elements.roomSearchInput.value = 'ab';
  f.elements.roomSearchInput.emit('input');
  f.tick();
  assert.equal(f.calls[0].signal.aborted, true);
  assert.match(f.elements.roomList.textContent, /at least 3 consecutive/);
  pending.resolve([room()]);
  await read;
  assert.equal(f.elements.roomList.querySelectorAll('.room-card').length, 0);
  f.api.rooms = async (params) => {
    f.calls.push({ params });
    return [];
  };
  f.elements.roomSearchInput.value = 'first';
  f.elements.roomSearchInput.emit('input');
  f.elements.roomSearchInput.value = 'second';
  f.elements.roomSearchInput.emit('input');
  assert.equal(f.timers.size, 1);
  f.tick();
  await Promise.all(f.tasks);
  assert.equal(f.calls.at(-1).params.get('q'), 'second');
  assert.match(f.elements.roomList.textContent, /No rooms match/);
});

test('pagination appends the next page and a failed directory read offers a working retry', async () => {
  const f = await fixture();
  f.api.rooms = async (params) => {
    f.calls.push({ params });
    return Array.from({ length: 20 }, (_, i) => room(String(i)));
  };
  await f.screen.load();
  assert.equal(f.elements.roomLoadMore.hidden, false);
  f.elements.roomLoadMore.click();
  await Promise.all(f.tasks);
  assert.equal(f.calls.at(-1).params.get('page'), '2');
  assert.equal(f.elements.roomList.querySelectorAll('.room-card').length, 40);
  f.api.rooms = async () => {
    throw new f.ApiError(429);
  };
  await f.screen.load();
  assert.match(f.elements.roomList.textContent, /Too many room searches/);
  assert.equal(f.elements.roomLoadMore.hidden, true);
  f.api.rooms = async () => [room('retry')];
  f.elements.roomList.querySelector('button').click();
  await Promise.all(f.tasks);
  assert.equal(f.elements.roomList.querySelector('.room-card').dataset.roomId, 'retry');
  assert.equal(f.elements.roomList.querySelector('.directory-status'), null);
});

test('disposal cancels search, requests and saved-room ownership including late results', async () => {
  const f = await fixture();
  f.store.accountId = 'first';
  f.store.token = 'token';
  f.screen.refreshSavedRooms();
  assert.equal(f.mounts.length, 1);
  f.mounts[0].options.onEmpty(false);
  assert.equal(f.elements.savedRoomPicker.hidden, false);
  f.store.accountId = 'second';
  f.screen.refreshSavedRooms();
  assert.equal(f.mounts[0].disposed, true);
  assert.equal(f.mounts[0].options.current(), false);
  assert.equal(f.elements.savedRoomPicker.hidden, true);
  const pending = deferred();
  f.api.rooms = (params, signal) => {
    f.calls.push({ params, signal });
    return pending.promise;
  };
  const read = f.screen.load();
  f.elements.roomSearchInput.emit('input');
  f.screen.dispose();
  f.screen.dispose();
  assert.equal(f.timers.size, 0);
  assert.equal(f.calls[0].signal.aborted, true);
  assert.equal(f.mounts[1].disposed, true);
  pending.resolve([room()]);
  await read;
  assert.equal(f.elements.roomList.children.length, 0);
  f.elements.roomSearchInput.emit('input');
  f.elements.roomLoadMore.click();
  assert.equal(f.timers.size, 0);
  assert.equal(f.calls.length, 1);
});
