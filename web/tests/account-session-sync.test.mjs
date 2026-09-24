import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const key = 'simplestchat-account-change-v1';
const marker = (number) => number.toString(16).padStart(32, '0');
const flush = async () => {
  for (let index = 0; index < 20; index++) await Promise.resolve();
};
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

async function fixture(t, { storageBlocked = false, channelBlocked = false, reconcile } = {}) {
  const events = new Map();
  const values = new Map();
  const writes = [];
  const broadcasts = [];
  const channels = [];
  let checks = 0;
  let invalidations = 0;
  let now = 0;
  let canCheck = true;
  let random = 0;
  const storage = {
    getItem: (name) => values.get(name) ?? null,
    setItem: (name, value) => {
      writes.push([name, value]);
      values.set(name, value);
    },
  };
  const window = {
    get localStorage() {
      if (storageBlocked) throw new Error('unavailable');
      return storage;
    },
    addEventListener: (name, listener) => events.set(name, listener),
    removeEventListener: (name) => events.delete(name),
  };
  const document = {
    visibilityState: 'visible',
    addEventListener: (name, listener) => events.set(name, listener),
    removeEventListener: (name) => events.delete(name),
  };
  class Channel {
    closed = false;
    onmessage = null;
    constructor(name) {
      if (channelBlocked) throw new Error('unavailable');
      assert.equal(name, key);
      channels.push(this);
    }
    postMessage(value) {
      broadcasts.push(value);
    }
    close() {
      this.closed = true;
    }
  }
  const { AccountSessionSync } = await loadTypeScript('src/account-session-sync.ts', {
    globals: {
      window,
      document,
      BroadcastChannel: Channel,
      performance: { now: () => now },
      queueMicrotask,
      crypto: {
        getRandomValues: (bytes) => {
          bytes[15] = ++random;
          return bytes;
        },
      },
    },
  });
  const sync = new AccountSessionSync({
    invalidate: () => invalidations++,
    reconcile: async () => {
      checks++;
      await reconcile?.();
    },
    canCheck: () => canCheck,
  });
  t.after(() => sync.dispose());
  return {
    sync,
    values,
    writes,
    broadcasts,
    channels,
    events,
    storage,
    document,
    checks: () => checks,
    invalidations: () => invalidations,
    advance: (value) => {
      now += value;
    },
    canCheck: (value) => {
      canCheck = value;
    },
    hint: (revision) => channels[0].onmessage?.({ data: { version: 1, revision } }),
  };
}

test('publication contains only a random revision and cannot reconcile its own account', async (t) => {
  const f = await fixture(t);
  f.sync.publish();
  assert.deepEqual(f.writes, [[key, marker(1)]]);
  assert.deepEqual(f.broadcasts, [{ version: 1, revision: marker(1) }]);
  f.hint(marker(1));
  await flush();
  assert.equal(f.checks(), 0);
  assert.equal(f.invalidations(), 0);
});

test('duplicate broadcast/storage events coalesce, and new hints fence pending reconciliation immediately', async (t) => {
  const first = deferred();
  let calls = 0;
  const f = await fixture(t, { reconcile: () => (++calls === 1 ? first.promise : undefined) });
  f.hint(marker(1));
  f.events.get('storage')({ key, storageArea: f.storage, newValue: marker(1) });
  await flush();
  assert.equal(f.checks(), 1);
  assert.equal(f.invalidations(), 1);
  for (const number of [2, 3, 4]) f.hint(marker(number));
  assert.equal(f.invalidations(), 4, 'stale async work is fenced before it can settle');
  assert.equal(f.checks(), 1, 'one authoritative check is active');
  first.resolve();
  await flush();
  assert.equal(f.checks(), 2, 'a burst requires one follow-up check');
});

test('unknown fields, nonmarkers and session-storage events do not trigger requests', async (t) => {
  const f = await fixture(t);
  for (const data of [
    null,
    'secret',
    {},
    { version: 2, revision: marker(1) },
    { version: 1, revision: marker(1), identity: 'private-account' },
    { version: 1, revision: 'invalid' },
    { version: 1, revision: `${marker(1)}\n` },
  ])
    f.channels[0].onmessage({ data });
  f.events.get('storage')({ key, storageArea: {}, newValue: marker(1) });
  f.events.get('storage')({ key: 'other', storageArea: f.storage, newValue: marker(1) });
  await flush();
  assert.equal(f.checks(), 0);
  assert.equal(f.invalidations(), 0);
});

test('storage fallback and focus/page restoration recover missed changes without periodic polling', async (t) => {
  const f = await fixture(t, { channelBlocked: true });
  f.sync.publish();
  assert.equal(f.writes.length, 1);
  f.values.set(key, marker(2));
  f.events.get('focus')();
  await flush();
  assert.equal(f.checks(), 1);
  f.events.get('pageshow')();
  await flush();
  assert.equal(f.checks(), 1);
  f.document.visibilityState = 'hidden';
  f.values.set(key, marker(3));
  f.events.get('visibilitychange')();
  assert.equal(f.checks(), 1);
  f.document.visibilityState = 'visible';
  f.events.get('visibilitychange')();
  await flush();
  assert.equal(f.checks(), 2);
});

test('blocked storage uses bounded focus checks without interrupting a local authentication intent', async (t) => {
  const f = await fixture(t, { storageBlocked: true, channelBlocked: true });
  f.canCheck(false);
  f.events.get('focus')();
  await flush();
  assert.equal(f.checks(), 0);
  f.canCheck(true);
  f.events.get('focus')();
  await flush();
  f.events.get('pageshow')();
  await flush();
  assert.equal(f.checks(), 1);
  f.advance(2000);
  f.events.get('focus')();
  await flush();
  assert.equal(f.checks(), 2);
  assert.doesNotThrow(() => f.sync.publish());
});

test('failed reconciliation remains bounded and disposal removes listeners and fences pending work', async (t) => {
  const f = await fixture(t, { reconcile: () => Promise.reject(new Error('unavailable')) });
  f.hint(marker(1));
  await flush();
  assert.equal(f.checks(), 1);
  f.sync.dispose();
  assert.equal(f.events.size, 0);
  assert.equal(f.channels[0].closed, true);
  assert.equal(f.channels[0].onmessage, null);
  assert.equal(f.invalidations(), 2);
  f.sync.publish();
  assert.equal(f.writes.length, 0);
  assert.doesNotThrow(() => f.sync.dispose());
});

test('recent marker memory stays bounded across many account changes', async (t) => {
  const f = await fixture(t);
  for (let number = 1; number <= 100; number++) f.hint(marker(number));
  await flush();
  assert.equal(f.checks(), 1);
  assert.equal(f.sync.seen.size, 32);
  assert.equal(f.sync.seen.has(marker(100)), true);
});
