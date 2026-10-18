import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush } from './ui-fixture.mjs';

const revision = '8'.repeat(40);
function response(value, options = {}) {
  const counts = { read: 0, cancel: 0, unreadCancel: 0 };
  const chunks = options.chunks ?? [new TextEncoder().encode(JSON.stringify(value))];
  let position = 0;
  const reader = {
    read: async () => {
      counts.read++;
      return position < chunks.length ? { done: false, value: chunks[position++] } : { done: true };
    },
    cancel: async () => {
      counts.cancel++;
    },
  };
  return {
    value: {
      ok: options.ok ?? true,
      headers: new Map([
        ['Content-Type', options.type ?? 'application/json'],
        ...(options.length ? [['Content-Length', options.length]] : []),
      ]),
      body: {
        getReader: () => reader,
        cancel: async () => {
          counts.unreadCancel++;
        },
      },
    },
    counts,
  };
}

async function fixture() {
  let now = 0;
  let inCall = false;
  let reloads = 0;
  const requests = [];
  const available = [];
  const timeouts = new Map();
  const events = new Map();
  const document = {
    hidden: false,
    addEventListener: (name, callback) => events.set(name, callback),
  };
  let sequence = 0;
  const { AppUpdates } = await loadTypeScript('src/app-updates.ts', {
    globals: {
      window: { addEventListener: (name, callback) => events.set(name, callback) },
      document,
      AbortController,
      TextDecoder,
      Date: { now: () => now },
      setTimeout: (callback) => {
        const id = ++sequence;
        timeouts.set(id, callback);
        return id;
      },
      clearTimeout: (id) => timeouts.delete(id),
      fetch: (path, init) => {
        const pending = deferred();
        requests.push({ path, init, pending });
        return pending.promise;
      },
    },
  });
  const updates = new AppUpdates({
    revision,
    inCall: () => inCall,
    available: (value) => available.push(value),
    reload: () => reloads++,
  });
  return {
    updates,
    requests,
    available,
    timeouts,
    events,
    document,
    advance: (value) => {
      now += value;
    },
    setInCall: (value) => {
      inCall = value;
    },
    reloads: () => reloads,
  };
}

test('release and rollback revisions are equality hints with no automatic or in-call reload', async () => {
  const f = await fixture();
  f.setInCall(true);
  const latest = response({ revision: 'f'.repeat(40) });
  f.requests[0].pending.resolve(latest.value);
  await flush();
  assert.deepEqual(f.available, [true]);
  assert.equal(f.reloads(), 0);
  assert.equal(f.updates.reloadWhenIdle(), false);
  f.setInCall(false);
  assert.equal(f.updates.reloadWhenIdle(), true);
  assert.equal(f.reloads(), 1);
  f.advance(60001);
  const request = f.updates.check();
  f.requests[1].pending.resolve(response({ revision: '1'.repeat(40) }).value);
  await request;
  assert.deepEqual(f.available, [true, true], 'a rollback is also an available version');
  f.advance(60001);
  const same = f.updates.check();
  f.requests[2].pending.resolve(response({ revision }).value);
  await same;
  assert.equal(f.available.at(-1), false);
  assert.equal(f.updates.reloadWhenIdle(), false);
  assert.equal(latest.counts.cancel, 1);
  f.updates.dispose();
});

test('version reads omit credentials, reject redirects and cache, and throttle focus/visibility', async () => {
  const f = await fixture();
  const request = f.requests[0];
  assert.equal(request.path, '/app-version.json');
  assert.equal(request.init.credentials, 'omit');
  assert.equal(request.init.redirect, 'error');
  assert.equal(request.init.cache, 'no-store');
  assert.deepEqual(Object.keys(request.init.headers), ['Accept']);
  request.pending.resolve(response({ revision }).value);
  await flush();
  for (let index = 0; index < 20; index++) f.events.get('focus')();
  await flush();
  assert.equal(f.requests.length, 1);
  f.advance(60001);
  f.document.hidden = true;
  await f.updates.check();
  assert.equal(f.requests.length, 1);
  f.updates.dispose();
});

test('invalid headers cancel unread bodies and oversized streamed input cancels the reader', async () => {
  for (const options of [
    { type: 'text/html' },
    { length: '513' },
    { length: 'bad' },
    { ok: false },
  ]) {
    const f = await fixture();
    const invalid = response({ revision: 'f'.repeat(40) }, options);
    f.requests[0].pending.resolve(invalid.value);
    await flush();
    assert.equal(invalid.counts.unreadCancel, 1);
    assert.equal(invalid.counts.read, 0);
    assert.deepEqual(f.available, []);
    assert.equal(f.requests[0].init.signal.aborted, true);
    f.updates.dispose();
  }
  const f = await fixture();
  const large = response(null, { chunks: [new Uint8Array(513)] });
  f.requests[0].pending.resolve(large.value);
  await flush();
  assert.equal(large.counts.read, 1);
  assert.equal(large.counts.cancel, 1);
  assert.deepEqual(f.available, []);
  f.updates.dispose();
});

test('unknown schemas and malformed UTF-8 cannot announce a release', async () => {
  for (const value of [
    null,
    { revision: null },
    { revision: 'bad' },
    { revision, account: 'secret' },
    [revision],
  ]) {
    const f = await fixture();
    f.requests[0].pending.resolve(response(value).value);
    await flush();
    assert.deepEqual(f.available, []);
    f.updates.dispose();
  }
  const f = await fixture();
  f.requests[0].pending.resolve(response(null, { chunks: [new Uint8Array([0xff])] }).value);
  await flush();
  assert.deepEqual(f.available, []);
  f.updates.dispose();
});

test('a timed-out or disposed pending fetch cannot overlap a new check or update retired UI', async () => {
  const f = await fixture();
  for (const timeout of [...f.timeouts.values()]) timeout();
  assert.equal(f.requests[0].init.signal.aborted, true);
  f.advance(120001);
  await f.updates.check();
  assert.equal(f.requests.length, 1, 'abort does not abandon an unresolved native fetch');
  f.updates.dispose();
  const late = response({ revision: 'f'.repeat(40) });
  f.requests[0].pending.resolve(late.value);
  await flush();
  assert.deepEqual(f.available, []);
  assert.equal(f.updates.reloadWhenIdle(), false);
  assert.equal(late.counts.cancel, 1);
  assert.equal(f.timeouts.size, 0);
});
