import { accountUpdatesFixture } from './account-updates-fixture.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { uiFixture, deferred, flush } from './ui-fixture.mjs';

const defaults = () => ({
  allowPrivateMessages: true,
  sounds: false,
  largeText: false,
  timestamps: 'time',
  ignored: [],
});
const peer = { id: '11111111-1111-4111-8111-111111111111', name: 'Peer' };
function backend() {
  return {
    accounts: new Map([
      ['Bearer token-a', defaults()],
      ['Bearer token-b', defaults()],
    ]),
    requests: [],
    hold: null,
    status: 200,
  };
}
async function fixture(t, server = backend()) {
  const state = { account: 'account-a', token: 'token-a', notices: [] };
  const f = await uiFixture({
    fetch: async (url, options) => {
      assert.equal(url, '/api/auth/preferences');
      const owner = options.headers.Authorization;
      const saved = server.accounts.get(owner);
      const body = options.body ? JSON.parse(options.body) : null;
      server.requests.push({ owner, method: options.method, body });
      if (options.method === 'PATCH') {
        for (const key of ['allowPrivateMessages', 'sounds', 'largeText', 'timestamps'])
          if (Object.hasOwn(body, key)) saved[key] = body[key];
        if (body.ignore)
          saved.ignored = [
            ...saved.ignored.filter((person) => person.id !== body.ignore.id),
            body.ignore,
          ];
        if (body.unignore)
          saved.ignored = saved.ignored.filter((person) => person.id !== body.unignore);
      }
      const response = structuredClone(saved);
      if (server.hold) await server.hold.promise;
      return {
        ok: server.status === 200,
        status: server.status,
        json: async () => response,
        text: async () => 'Unavailable',
      };
    },
  });
  const events = new Map(),
    timers = new Map();
  const sheet = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': f.ui, './settings-sheet.css': {} },
    globals: { document: f.document, setTimeout: () => 1, clearTimeout() {} },
  });
  const document = {
    ...f.document,
    hidden: false,
    addEventListener: (name, fn) => events.set(name, fn),
  };
  const updates = accountUpdatesFixture();
  const { AccountPreferences } = await loadTypeScript('src/account-preferences.ts', {
    modules: { './account-updates': updates.module, './ui': f.ui, './settings-sheet': sheet },
    globals: {
      document,
      window: { addEventListener: (name, fn) => events.set(name, fn) },
      localStorage: { setItem() {} },
      crypto: { randomUUID: () => 'change-marker' },
      setInterval: (fn) => {
        timers.set(1, fn);
        return 1;
      },
      clearInterval: (id) => timers.delete(id),
    },
  });
  const preferences = new AccountPreferences({
    getAccountId: () => state.account,
    getToken: () => state.token,
    notify: (message) => state.notices.push(message),
  });
  t.after(() => preferences.dispose());
  return {
    ...f,
    updates,
    preferences,
    state,
    server,
    events,
    timers,
    preferencesDocument: document,
  };
}

test('a stale tab changes one field without undoing another tab’s ignore or PM opt-out', async (t) => {
  const server = backend();
  const first = await fixture(t, server),
    second = await fixture(t, server);
  await Promise.all([first.preferences.refresh(), second.preferences.refresh()]);
  await first.preferences.change({ ignore: peer });
  await first.preferences.change({ allowPrivateMessages: false });
  await second.preferences.change({ timestamps: 'seconds' });
  const saved = server.accounts.get('Bearer token-a');
  assert.deepEqual(saved.ignored, [peer]);
  assert.equal(saved.allowPrivateMessages, false);
  assert.equal(saved.timestamps, 'seconds');
  assert.deepEqual(
    server.requests.filter((request) => request.method === 'PATCH').map((request) => request.body),
    [{ ignore: peer }, { allowPrivateMessages: false }, { timestamps: 'seconds' }],
  );
  await first.preferences.refresh();
  assert.deepEqual(first.preferences.snapshot, second.preferences.snapshot);
});

test('pending explicit choices serialize and preserve each independent field', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const hold = deferred();
  f.server.hold = hold;
  const first = f.preferences.change({ ignore: peer });
  const second = f.preferences.change({ sounds: true });
  await flush();
  assert.equal(f.server.requests.filter((request) => request.method === 'PATCH').length, 1);
  f.server.hold = null;
  hold.resolve();
  await Promise.all([first, second]);
  assert.equal(f.preferences.snapshot.sounds, true);
  assert.deepEqual(f.preferences.snapshot.ignored, [peer]);
});

test('an unknown mutation retains restrictive intent and stops queued writes until explicit reconciliation', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  f.server.status = 503;
  const first = f.preferences.change({ ignore: peer });
  const second = f.preferences.change({ timestamps: 'seconds' });
  await assert.rejects(first, f.ui.ApiOutcomeUnknownError);
  await assert.rejects(second, /Refresh privacy/);
  assert.deepEqual(f.preferences.snapshot.ignored, [peer]);
  const requests = f.server.requests.length;
  await f.preferences.refresh();
  assert.equal(
    f.server.requests.length,
    requests,
    'polling does not resolve or replay an uncertain write',
  );
  f.server.status = 200;
  await f.preferences.refresh(true);
  await f.preferences.change({ unignore: peer.id });
  assert.deepEqual(f.preferences.snapshot.ignored, []);
});

test('an old read cannot overwrite a newer confirmed preference change', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const hold = deferred();
  f.server.hold = hold;
  const read = f.preferences.refresh();
  f.server.hold = null;
  await f.preferences.change({ allowPrivateMessages: false });
  hold.resolve();
  await read;
  assert.equal(f.preferences.snapshot.allowPrivateMessages, false);
});

test('explicit recovery sends a fresh read while a pre-mutation read is still pending', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const hold = deferred();
  f.server.hold = hold;
  const oldRead = f.preferences.refresh();
  f.server.hold = null;
  try {
    f.server.status = 503;
    await assert.rejects(
      f.preferences.change({ allowPrivateMessages: false }),
      f.ui.ApiOutcomeUnknownError,
    );
    f.server.status = 200;
    const reads = f.server.requests.filter(({ method }) => method === 'GET').length;
    await f.preferences.refresh(true);
    assert.equal(
      f.server.requests.filter(({ method }) => method === 'GET').length,
      reads + 1,
      'explicit recovery must synchronize fresh server state before clearing uncertainty',
    );
    await f.preferences.change({ timestamps: 'seconds' });
  } finally {
    hold.resolve();
    await oldRead;
  }
  assert.equal(f.preferences.snapshot.allowPrivateMessages, false);
  assert.equal(f.preferences.snapshot.timestamps, 'seconds');
});

test('account replacement retires in-flight reads and queued writes without borrowing the new token', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const hold = deferred();
  f.server.hold = hold;
  const first = f.preferences.change({ ignore: peer });
  const queued = f.preferences.change({ sounds: true });
  const firstRejected = assert.rejects(first, f.ui.ApiOutcomeUnknownError);
  const queuedRejected = assert.rejects(queued, /account changed/);
  await flush();
  f.state.account = 'account-b';
  f.state.token = 'token-b';
  f.server.hold = null;
  await f.preferences.refresh();
  hold.resolve();
  await Promise.all([firstRejected, queuedRejected]);
  assert.deepEqual(f.preferences.snapshot, defaults());
  assert.equal(f.server.requests.filter((request) => request.method === 'PATCH').length, 1);
  assert.equal(f.server.requests.at(-1).owner, 'Bearer token-b');
});

test('privacy controls work without a room and react to a refreshed account snapshot', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const host = f.ui.el('section');
  f.document.body.append(host);
  f.preferences.mountPrivacy(host, () => true);
  await flush();
  const allow = host.querySelector('input');
  allow.checked = false;
  allow.emit('change');
  await flush();
  assert.equal(f.preferences.snapshot.allowPrivateMessages, false);
  const conversation = f.ui.el('section');
  f.document.body.append(conversation);
  f.preferences.mountConversation(conversation, peer.id, peer.name, () => true);
  await flush();
  conversation.querySelector('button').click();
  await flush();
  assert.deepEqual(f.preferences.snapshot.ignored, [peer]);
  assert.match(host.textContent, /Peer/);
  assert.match(conversation.textContent, /Unignore/);
  assert.deepEqual(f.server.requests.at(-1).body, { ignore: peer });
});

test('visible polling, focus and storage hints reconcile other devices without sending another mutation', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  const saved = f.server.accounts.get('Bearer token-a');
  saved.allowPrivateMessages = false;
  saved.ignored = [peer];
  f.timers.get(1)();
  await flush();
  assert.equal(f.preferences.snapshot.allowPrivateMessages, false);
  assert.deepEqual(f.preferences.snapshot.ignored, [peer]);
  const reads = f.server.requests.length;
  f.preferencesDocument.hidden = true;
  f.timers.get(1)();
  f.events.get('focus')();
  await flush();
  assert.equal(f.server.requests.length, reads);
  f.preferencesDocument.hidden = false;
  saved.timestamps = 'seconds';
  f.events.get('storage')({ key: 'an-unrelated-setting' });
  await flush();
  assert.equal(f.server.requests.length, reads);
  f.events.get('storage')({ key: 'simplestchat.preferences.changed' });
  await flush();
  assert.equal(f.preferences.snapshot.timestamps, 'seconds');
  assert.ok(f.server.requests.every(({ method }) => method === 'GET'));
  f.preferences.dispose();
  assert.equal(f.timers.size, 0);
  f.events.get('focus')();
  await flush();
  assert.equal(f.server.requests.length, reads + 1);
});

test('a clean live preference snapshot skips passive focus reads but preserves hints and explicit reconciliation', async (t) => {
  const f = await fixture(t);
  f.updates.live.add('account-a');
  f.events.get('focus')();
  await flush();
  assert.equal(
    f.server.requests.length,
    1,
    'a healthy feed does not substitute for an unknown snapshot',
  );
  f.events.get('focus')();
  f.events.get('visibilitychange')();
  await flush();
  assert.equal(f.server.requests.length, 1);
  f.events.get('storage')({ key: 'simplestchat.preferences.changed' });
  await flush();
  assert.equal(
    f.server.requests.length,
    2,
    'credential-free cross-tab hints remain authoritative invalidations',
  );
  await f.preferences.refresh(true);
  assert.equal(f.server.requests.length, 3, 'explicit Refresh remains available while live');
  f.updates.emit({ account: 'account-a', scope: 'ready' });
  await flush();
  assert.equal(f.server.requests.length, 4, 'reconnect ready always reconciles the scope');
  f.updates.live.clear();
  f.events.get('visibilitychange')();
  await flush();
  assert.equal(f.server.requests.length, 5, 'a visible document repairs before stream readiness');
});

test('a failed preference read is repaired on focus even with a healthy account feed', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  f.updates.live.add('account-a');
  f.server.status = 503;
  await assert.rejects(f.preferences.refresh());
  f.server.status = 200;
  f.server.accounts.get('Bearer token-a').sounds = true;
  f.events.get('focus')();
  await flush();
  assert.equal(f.server.requests.length, 3);
  assert.equal(f.preferences.snapshot.sounds, true);
});

test('live invalidations queue behind an in-flight read and save without cross-account refreshes', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  f.updates.live.add('account-a');
  const count = () => f.server.requests.filter(({ method }) => method === 'GET').length;
  const baseline = count();
  for (const timer of f.timers.values()) timer();
  await flush();
  assert.equal(count(), baseline, 'healthy stream suppresses periodic polling');
  f.updates.emit({ account: 'account-b', scope: 'preferences' });
  await flush();
  assert.equal(count(), baseline);
  const hold = deferred();
  f.server.hold = hold;
  const pending = f.preferences.refresh();
  f.server.accounts.get('Bearer token-a').sounds = true;
  f.updates.emit({ account: 'account-a', scope: 'preferences' });
  f.server.hold = null;
  hold.resolve();
  await pending;
  await flush();
  assert.equal(f.preferences.snapshot.sounds, true);
  assert.equal(count(), baseline + 2, 'invalidation during an old read produces one fresh read');
  const saving = deferred();
  f.server.hold = saving;
  const write = f.preferences.change({ timestamps: 'seconds' });
  await flush();
  f.server.accounts.get('Bearer token-a').allowPrivateMessages = false;
  f.updates.emit({ account: 'account-a', scope: 'preferences' });
  f.server.hold = null;
  saving.resolve();
  await write;
  await flush();
  assert.equal(f.preferences.snapshot.allowPrivateMessages, false);
  assert.equal(f.preferences.snapshot.timestamps, 'seconds');
});

test('live preferences cannot clear an uncertain write without an explicit authoritative refresh', async (t) => {
  const f = await fixture(t);
  await f.preferences.refresh();
  f.server.status = 503;
  await assert.rejects(f.preferences.change({ sounds: true }), f.ui.ApiOutcomeUnknownError);
  const baseline = f.server.requests.length;
  f.server.status = 200;
  f.updates.emit({ account: 'account-a', scope: 'ready' });
  await flush();
  assert.equal(f.server.requests.length, baseline);
  await f.preferences.refresh(true);
  assert.equal(f.preferences.snapshot.sounds, true);
});
