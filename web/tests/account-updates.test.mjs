import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { deferred, flush } from './ui-fixture.mjs';

const peer = '11111111-1111-4111-8111-111111111111';
const { AccountEventDecoder } = await loadTypeScript('src/account-updates.ts');

test('SSE accepts fragmented CRLF, comments and multiline JSON without partial delivery', () => {
  const events = [];
  const parser = new AccountEventDecoder((value) => events.push(value));
  const wire =
    ': heartbeat\r\ndata: {"type":\r\ndata: "ready"}\r\n\r\ndata: {"type":"change","scope":"inbox","peerId":"' +
    peer +
    '"}\n\ndata: {"type":"ready"}';
  for (const character of wire) parser.push(character);
  assert.deepEqual(events, [{ type: 'ready' }, { type: 'change', scope: 'inbox', peerId: peer }]);
});

test('SSE bounds partial lines and cumulative event data and rejects malformed scopes and peers', () => {
  for (const wire of [
    'x'.repeat(8193),
    'data: ' + 'x'.repeat(4100) + '\n' + 'data: ' + 'x'.repeat(4100),
    'data: null\n\n',
    'data: []\n\n',
    'data: {}\n\n',
    'data: {"type":"change","scope":"password"}\n\n',
    'data: {"type":"change","scope":"inbox","peerId":"bad"}\n\n',
  ]) {
    assert.throws(() =>
      new AccountEventDecoder(() => assert.fail('invalid event delivered')).push(wire),
    );
  }
});

async function fixture(t) {
  const state = {
    account: 'account-a',
    token: 'token-a',
    hidden: false,
    requests: [],
    events: [],
    now: 1000,
  };
  const timers = new Map(),
    handlers = new Map();
  let id = 0;
  const { AccountUpdates, onAccountUpdate, accountUpdatesLive } = await loadTypeScript(
    'src/account-updates.ts',
    {
      globals: {
        document: {
          get hidden() {
            return state.hidden;
          },
          addEventListener: (name, fn) => handlers.set(name, fn),
        },
        window: { addEventListener: (name, fn) => handlers.set(name, fn) },
        fetch: (url, options) => {
          const headers = deferred();
          state.requests.push({ url, options, headers });
          return headers.promise;
        },
        setInterval: () => ++id,
        clearInterval() {},
        setTimeout: (fn, ms) => {
          const key = ++id;
          timers.set(key, { fn, ms });
          return key;
        },
        clearTimeout: (key) => timers.delete(key),
        Date: class extends Date {
          static now() {
            return state.now;
          }
        },
      },
    },
  );
  onAccountUpdate((event) => state.events.push(event));
  // A failed view must not suppress other views or tear down the stream.
  onAccountUpdate(() => {
    throw new Error('view retired');
  });
  const updates = new AccountUpdates({
    getToken: () => state.token,
    getAccountId: () => state.account,
  });
  t.after(() => updates.dispose());
  function respond(index, status = 200, extraHeaders = {}, cancel = () => {}) {
    let source;
    const body = new ReadableStream({
      start(controller) {
        source = controller;
      },
      cancel,
    });
    const headers = new Headers({ 'Content-Type': 'text/event-stream', ...extraHeaders });
    state.requests[index].headers.resolve({ status, ok: status === 200, headers, body });
    return {
      send: (value) => source.enqueue(new TextEncoder().encode(value)),
      close: () => source.close(),
    };
  }
  return { state, timers, handlers, updates, respond, accountUpdatesLive };
}

test('authenticated header stream resynchronizes on every ready and never replays mutations', async (t) => {
  const f = await fixture(t);
  const first = f.respond(0);
  first.send('data: {"type":"ready"}\n\n');
  await flush();
  assert.equal(f.accountUpdatesLive('account-a'), true);
  assert.equal(f.state.requests[0].url, '/api/auth/updates');
  assert.equal(f.state.requests[0].options.headers.Authorization, 'Bearer token-a');
  assert.equal(f.state.requests[0].options.redirect, 'error');
  first.send('data: {"type":"change","scope":"inbox","peerId":"' + peer + '"}\n\n');
  await flush();
  first.close();
  await flush();
  assert.equal(f.accountUpdatesLive('account-a'), false);
  const retry = [...f.timers.values()][0];
  assert.ok(retry.ms >= 800 && retry.ms <= 1200);
  f.timers.clear();
  retry.fn();
  const second = f.respond(1);
  second.send('data: {"type":"ready"}\n\n');
  await flush();
  assert.deepEqual(
    f.state.events.map((event) => event.scope),
    ['ready', 'inbox', 'ready'],
  );
  assert.ok(f.state.requests.every(({ options }) => !options.body && !options.method));
});

test('retired account headers and buffered old-account events cannot update the replacement account', async (t) => {
  const f = await fixture(t);
  f.state.account = 'account-b';
  f.state.token = 'token-b';
  f.updates.synchronize();
  assert.equal(f.state.requests[0].options.signal.aborted, true);
  f.respond(0).send('data: {"type":"ready"}\n\n');
  const active = f.respond(1);
  active.send('data: {"type":"ready"}\n\n');
  await flush();
  assert.deepEqual(f.state.events, [{ account: 'account-b', scope: 'ready' }]);
  assert.equal(f.accountUpdatesLive('account-a'), false);
  f.state.token = 'rotated';
  f.handlers.get('online')();
  assert.equal(f.accountUpdatesLive('account-b'), false);
  active.send('data: {"type":"change","scope":"inbox"}\n\n');
  await flush();
  assert.equal(f.state.requests.length, 3);
  assert.deepEqual(f.state.events, [{ account: 'account-b', scope: 'ready' }]);
  f.respond(2).send('data: {"type":"ready"}\n\n');
  await flush();
  assert.equal(f.accountUpdatesLive('account-b'), true);
});

test('online replaces a stalled ready stream without waiting for its read or watchdog', async (t) => {
  const f = await fixture(t);
  const first = f.respond(0);
  first.send('data: {"type":"ready"}\n\n');
  await flush();
  const oldWatchdog = [...f.timers.values()][0];
  assert.equal(oldWatchdog.ms, 45000);
  assert.equal(f.accountUpdatesLive('account-a'), true);

  f.handlers.get('online')();
  assert.equal(f.state.requests[0].options.signal.aborted, true);
  assert.equal(f.state.requests.length, 2);
  assert.equal(f.accountUpdatesLive('account-a'), false);
  f.handlers.get('online')();
  assert.equal(f.state.requests.length, 2, 'fresh pending headers retain their owner');
  assert.equal(f.state.requests[1].options.signal.aborted, false);

  const second = f.respond(1);
  second.send('data: {"type":"ready"}\n\n');
  await flush();
  assert.equal(f.accountUpdatesLive('account-a'), true);
  oldWatchdog.fn();
  first.send('data: {"type":"change","scope":"contacts"}\n\n');
  await flush();
  assert.deepEqual(f.state.events, [
    { account: 'account-a', scope: 'ready' },
    { account: 'account-a', scope: 'ready' },
  ]);
  assert.equal(f.accountUpdatesLive('account-a'), true);
  assert.equal(f.state.requests[1].options.signal.aborted, false);
  assert.equal(f.timers.size, 1, 'late retirement schedules no retry beside the fresh stream');
  assert.equal([...f.timers.values()][0].ms, 45000);
});

test('hidden and disposed clients abort reads and cannot reconnect on late completion', async (t) => {
  const f = await fixture(t);
  const active = f.respond(0);
  active.send('data: {"type":"ready"}\n\n');
  await flush();
  f.state.hidden = true;
  f.handlers.get('visibilitychange')();
  assert.equal(f.state.requests[0].options.signal.aborted, true);
  assert.equal(f.accountUpdatesLive('account-a'), false);
  f.handlers.get('online')();
  assert.equal(f.state.requests.length, 1, 'an online event does not connect a hidden page');
  active.send('data: {"type":"change","scope":"contacts"}\n\n');
  await flush();
  assert.equal(f.state.events.length, 1);
  f.state.hidden = false;
  f.handlers.get('visibilitychange')();
  assert.equal(f.state.requests.length, 2);
  f.updates.dispose();
  f.handlers.get('online')();
  assert.equal(f.state.requests.length, 2, 'a retired listener cannot restore its owner');
  f.respond(1);
  await flush();
  assert.equal(f.timers.size, 0);
});

test('authentication denial stops retry until credentials change; rate limits respect bounded Retry-After', async (t) => {
  const f = await fixture(t);
  f.respond(0, 401);
  await flush();
  assert.equal(f.timers.size, 0);
  f.updates.synchronize();
  assert.equal(f.state.requests.length, 1);
  f.state.token = 'fresh';
  f.updates.synchronize();
  f.respond(1, 429, { 'Retry-After': '120' });
  await flush();
  assert.equal([...f.timers.values()][0].ms, 120000);
});

test('online preserves pending authentication denial and server-directed retry ownership', async (t) => {
  for (const status of [401, 403, 429]) {
    const f = await fixture(t);
    const cancelled = deferred();
    f.respond(0, status, { 'Retry-After': '120' }, () => cancelled.promise);
    await flush();
    f.handlers.get('online')();
    f.handlers.get('online')();
    assert.equal(f.state.requests.length, 1, `pending ${status} remains authoritative`);
    assert.equal(f.state.requests[0].options.signal.aborted, false);
    cancelled.resolve();
    await flush();
    const retry = [...f.timers.entries()];
    f.handlers.get('online')();
    assert.equal(f.state.requests.length, 1, `settled ${status} is not bypassed`);
    assert.deepEqual([...f.timers.entries()], retry);
    assert.equal(retry.length, status === 429 ? 1 : 0);
    if (status === 429) assert.equal(retry[0][1].ms, 120000);
  }
});

test('online keeps an ordinary reconnect backoff until its original timer fires', async (t) => {
  const f = await fixture(t);
  f.respond(0, 503);
  await flush();
  const retry = [...f.timers.entries()][0];
  assert.ok(retry[1].ms >= 800 && retry[1].ms <= 1200);
  f.handlers.get('online')();
  assert.equal(f.state.requests.length, 1);
  assert.deepEqual([...f.timers.entries()], [retry]);
  f.timers.delete(retry[0]);
  retry[1].fn();
  assert.equal(f.state.requests.length, 2);
});

test('heartbeat cannot extend the initial readiness deadline and half-open streams time out', async (t) => {
  const f = await fixture(t);
  const active = f.respond(0);
  active.send(': hi\n\n');
  await flush();
  f.state.now += 9000;
  active.send(': hi\n\n');
  await flush();
  const deadline = [...f.timers.values()][0];
  assert.equal(deadline.ms, 1000);
  deadline.fn();
  assert.equal(f.state.requests[0].options.signal.aborted, true);
  active.close();
  await flush();
  assert.equal(f.accountUpdatesLive('account-a'), false);
});
