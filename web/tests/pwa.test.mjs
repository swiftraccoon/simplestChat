import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import { readFile } from 'node:fs/promises';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM, deferred, flush } from './ui-fixture.mjs';

async function fixture(initial = {}) {
  const dom = createDOM();
  const calls = [];
  const store = new Map();
  const state = {
    account: 'account-a',
    token: 'owned-token',
    current: true,
    installed: false,
    permission: 'default',
    enabled: false,
    subscription: null,
    requestPermission: async () => 'granted',
    subscribe: null,
    enable: async () => {},
    openMessages: 0,
    ...initial,
  };
  if (state.owner) store.set('simplestchat.pushOwner', JSON.stringify(state.owner));
  const localStorage = {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, value),
    removeItem: (key) => store.delete(key),
  };
  const window = new EventTarget();
  Object.assign(window, {
    isSecureContext: true,
    PushManager: {},
    Notification: {},
    matchMedia: () => ({ matches: state.installed }),
    location: { href: 'https://example.test/?messages=1' },
    history: {
      state: {},
      replaceState: (_state, _unused, url) => calls.push(['url', String(url)]),
    },
  });
  const subscription = () => ({
    endpoint: 'https://push.example.test/new-endpoint',
    async unsubscribe() {
      calls.push(['unsubscribe']);
      state.subscription = null;
      return true;
    },
  });
  const worker = {
    active: { postMessage: (value) => calls.push(['worker-message', value]) },
    pushManager: {
      getSubscription: async () => state.subscription,
      subscribe: async (options) => {
        calls.push(['subscribe', options]);
        state.subscription = state.subscribe ? await state.subscribe() : subscription();
        return state.subscription;
      },
    },
  };
  const serviceWorker = new EventTarget();
  serviceWorker.register = async (...args) => {
    calls.push(['register', ...args]);
    return worker;
  };
  serviceWorker.ready = Promise.resolve(worker);
  let lock = Promise.resolve();
  const navigator = {
    userAgent: state.ios ? 'iPhone' : 'Fixture browser',
    platform: '',
    maxTouchPoints: 0,
    serviceWorker,
    locks: {
      request: (_name, callback) => {
        const task = lock.catch(() => {}).then(callback);
        lock = task;
        return task;
      },
    },
  };
  const Notification = {
    get permission() {
      return state.permission;
    },
    requestPermission: () => {
      calls.push(['permission']);
      return state.requestPermission();
    },
  };
  const ui = {
    el(tag, text, className) {
      const node = dom.document.createElement(tag);
      if (text !== undefined) node.textContent = text;
      node.className = className ?? '';
      return node;
    },
    button(text, action) {
      const node = this.el('button', text);
      node.addEventListener('click', action);
      return node;
    },
    api: {
      pushStatus: async () => ({ enabled: state.enabled, publicKey: 'public-key' }),
      enablePush: async (_token, body) => {
        calls.push(['enable', body]);
        await state.enable();
        state.enabled = true;
      },
      disablePush: async () => {
        calls.push(['disable']);
        state.enabled = false;
      },
    },
  };
  ui.button = ui.button.bind(ui);
  let nextLease = 0;
  const { PwaControls } = await loadTypeScript('src/pwa.ts', {
    modules: { './ui': ui },
    globals: {
      window,
      navigator,
      Notification,
      localStorage,
      crypto: { randomUUID: () => `lease-${++nextLease}` },
    },
  });
  const pwa = new PwaControls({
    getToken: () => state.token,
    getAccountId: () => state.account,
    openMessages: () => state.openMessages++,
  });
  const container = ui.el('div');
  dom.document.body.append(container);
  pwa.mountAccount(container, () => state.current);
  await flush();
  const toggle = container
    .querySelectorAll('button')
    .find((node) => node.textContent.includes('notifications'));
  const status = container
    .querySelectorAll('p')
    .find((node) => node.getAttribute('role') === 'status');
  return {
    state,
    pwa,
    calls,
    store,
    localStorage,
    window,
    serviceWorker,
    worker,
    subscription,
    toggle,
    status,
    container,
  };
}

test('loading/registering an installable app never requests notification permission or subscribes', async () => {
  const f = await fixture();
  assert.equal(f.toggle.disabled, false);
  assert.ok(!f.calls.some(([kind]) => ['permission', 'subscribe', 'enable'].includes(kind)));
  assert.equal(f.state.openMessages, 1, 'a notification link opens only the authenticated inbox');
  assert.equal(f.calls[0][0], 'register');
  assert.ok(f.calls.some(([kind, url]) => kind === 'url' && !url.includes('messages=')));
});

test('enable requests permission in the button gesture and stores only a nonsecret ownership lease', async () => {
  const f = await fixture();
  f.toggle.click();
  assert.equal(f.calls.at(-1)[0], 'permission', 'permission is requested synchronously');
  await flush();
  assert.equal(f.state.enabled, true);
  assert.equal(f.toggle.textContent, 'Disable notifications');
  assert.deepEqual(f.calls.find(([kind]) => kind === 'subscribe')[1], {
    userVisibleOnly: true,
    applicationServerKey: 'public-key',
  });
  const persisted = [...f.store.values()].join('');
  assert.ok(!persisted.includes('owned-token'));
  assert.ok(!persisted.includes('endpoint'));
  assert.match(persisted, /account-a/);
  f.toggle.click();
  await flush();
  assert.equal(f.state.enabled, false);
  assert.equal(f.state.subscription, null);
  assert.equal(f.store.size, 0);
});

test('denied permission and closing Account during permission never register push', async () => {
  for (const close of [false, true]) {
    const permission = deferred();
    const f = await fixture({ requestPermission: () => permission.promise });
    f.toggle.click();
    if (close) f.state.current = false;
    permission.resolve(close ? 'granted' : 'denied');
    await flush();
    assert.ok(!f.calls.some(([kind]) => ['subscribe', 'enable'].includes(kind)));
    if (!close) assert.match(f.status.textContent, /blocked/);
  }
});

test('late native subscription after identity change is retired and never sent to another account', async () => {
  const native = deferred();
  const f = await fixture({ permission: 'granted', subscribe: () => native.promise });
  f.toggle.click();
  await flush();
  f.state.account = 'account-b';
  f.pwa.accountChanged();
  native.resolve(f.subscription());
  await flush();
  assert.equal(f.state.subscription, null);
  assert.ok(!f.calls.some(([kind]) => kind === 'enable'));
  assert.equal(f.store.size, 0);
});

test('old account logout cannot unsubscribe a newer account ownership lease', async () => {
  const f = await fixture({ owner: { accountId: 'account-a', lease: 'old' } });
  f.state.subscription = f.subscription();
  f.localStorage.setItem(
    'simplestchat.pushOwner',
    JSON.stringify({ accountId: 'account-b', lease: 'new' }),
  );
  f.state.account = null;
  f.state.token = null;
  f.pwa.accountChanged();
  await flush();
  assert.ok(!f.calls.some(([kind]) => kind === 'unsubscribe'));
  assert.equal(f.state.subscription.endpoint, 'https://push.example.test/new-endpoint');
});

test('another account or unowned native endpoint is retired before explicit enable', async () => {
  for (const owner of [null, { accountId: 'other', lease: 'old' }]) {
    const f = await fixture({ permission: 'granted', owner });
    f.state.subscription = f.subscription();
    f.toggle.click();
    await flush();
    const kinds = f.calls.map(([kind]) => kind);
    assert.ok(kinds.indexOf('unsubscribe') < kinds.indexOf('subscribe'));
    assert.equal(f.state.enabled, true);
  }
});

test('iPhone browser explains Home Screen installation without prompting', async () => {
  const f = await fixture({ ios: true, installed: false });
  assert.equal(f.toggle.hidden, true);
  assert.match(f.container.textContent, /Share → Add to Home Screen/);
  assert.ok(!f.calls.some(([kind]) => kind === 'permission'));
});

test('notification click waits for authentication and never opens arbitrary destinations', async () => {
  const f = await fixture({ account: null, token: null });
  assert.equal(f.state.openMessages, 0);
  f.state.account = 'account-a';
  f.state.token = 'owned-token';
  f.pwa.accountChanged();
  assert.equal(f.state.openMessages, 1);
  const event = new Event('message');
  event.data = { type: 'openMessages', url: 'https://untrusted.test' };
  f.serviceWorker.dispatchEvent(event);
  assert.equal(f.state.openMessages, 2);
});

async function workerFixture(windows = []) {
  const handlers = new Map();
  const calls = [];
  const self = {
    addEventListener: (name, handler) => handlers.set(name, handler),
    skipWaiting: async () => {},
    location: { origin: 'https://example.test' },
    clients: {
      claim: async () => {},
      matchAll: async () => windows,
      openWindow: async (url) => calls.push(['open', url]),
    },
    registration: {
      showNotification: async (...args) => calls.push(['show', ...args]),
      getNotifications: async () => [{ close: () => calls.push(['clear']) }],
    },
  };
  vm.runInNewContext(await readFile(new URL('../public/sw.js', import.meta.url), 'utf8'), {
    self,
    URL,
  });
  const fire = async (name, extra = {}) => {
    let done;
    const event = {
      waitUntil: (task) => {
        done = task;
      },
    };
    Object.defineProperties(event, Object.getOwnPropertyDescriptors(extra));
    handlers.get(name)(event);
    await done;
  };
  return { calls, handlers, fire };
}

test('worker displays generic private alerts without inspecting push payloads or caching any requests', async () => {
  const f = await workerFixture();
  await f.fire('push', {
    get data() {
      throw new Error('Private payload must not be read');
    },
  });
  assert.equal(f.calls[0][1], 'simplestChat');
  assert.equal(f.calls[0][2].body, 'New private messages');
  assert.equal(f.handlers.has('fetch'), false);
  await f.fire('message', { data: { type: 'clearNotifications' } });
  assert.deepEqual(f.calls.at(-1), ['clear']);
});

test('worker click focuses the app inbox or opens its fixed same-origin entry point', async () => {
  const focused = [];
  const app = {
    url: 'https://example.test/',
    focus: async () => focused.push('focus'),
    postMessage: (value) => focused.push(value.type),
  };
  const f = await workerFixture([{ url: 'https://example.test/help.html' }, app]);
  await f.fire('notificationclick', { notification: { close() {} } });
  assert.deepEqual(focused, ['focus', 'openMessages']);
  const empty = await workerFixture();
  await empty.fire('notificationclick', { notification: { close() {} } });
  assert.deepEqual(empty.calls, [['open', '/?messages=1']]);
});

test('a registration response after account change removes the old server binding and native subscription', async () => {
  const accepted = deferred();
  const f = await fixture({ permission: 'granted', enable: () => accepted.promise });
  f.toggle.click();
  await flush();
  assert.ok(f.calls.some(([kind]) => kind === 'enable'));
  f.state.account = 'account-b';
  f.pwa.accountChanged();
  accepted.resolve();
  await flush();
  await flush();
  assert.equal(f.state.enabled, false);
  assert.equal(f.state.subscription, null);
  assert.equal(f.store.size, 0);
  assert.ok(f.calls.some(([kind]) => kind === 'disable'));
});

test('disposing a closed tab preserves confirmed push but removes app event handlers', async () => {
  const f = await fixture({ permission: 'granted' });
  f.toggle.click();
  await flush();
  f.pwa.dispose();
  assert.ok(f.state.subscription);
  assert.ok(!f.calls.some(([kind]) => kind === 'unsubscribe'));
  const before = f.state.openMessages;
  const event = new Event('message');
  event.data = { type: 'openMessages' };
  f.serviceWorker.dispatchEvent(event);
  assert.equal(f.state.openMessages, before);
});
