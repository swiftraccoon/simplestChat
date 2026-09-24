import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function fixture(t, cryptoApi = globalThis.crypto) {
  const requests = [];
  const intervals = new Map();
  const timeouts = new Map();
  const stored = new Map();
  let sequence = 0;
  let now = 0;
  const api = await loadTypeScript('src/telemetry.ts', {
    globals: {
      crypto: cryptoApi,
      navigator: { userAgent: 'Secret profile Firefox/150.0' },
      performance: { now: () => now },
      localStorage: {
        getItem: (key) => stored.get(key),
        setItem: (key, value) => stored.set(key, value),
      },
      setInterval: (callback) => {
        intervals.set(++sequence, callback);
        return sequence;
      },
      clearInterval: (key) => intervals.delete(key),
      setTimeout: (callback) => {
        timeouts.set(++sequence, callback);
        return sequence;
      },
      clearTimeout: (key) => timeouts.delete(key),
      fetch: async (url, options) => {
        requests.push({ url, options });
        return { ok: false, status: 429 };
      },
    },
  });
  const telemetry = new api.ClientTelemetry('development');
  t.after(() => telemetry.dispose());
  return {
    api,
    telemetry,
    requests,
    stored,
    intervals,
    timeouts,
    advance: (ms) => {
      now += ms;
    },
  };
}

test('diagnostics initialize without secure-context randomUUID and survive unavailable randomness', async (t) => {
  const insecure = await fixture(t, {
    getRandomValues: (bytes) => bytes.fill(42),
  });
  assert.match(JSON.parse(insecure.telemetry.summary()).localReportReference, /^[a-f0-9]{32}$/);
  const unavailable = await fixture(t, {
    getRandomValues: () => {
      throw new Error('Unavailable browser API');
    },
  });
  unavailable.telemetry.record({ name: 'connection', outcome: 'started' });
  const summary = JSON.parse(unavailable.telemetry.summary());
  assert.equal(summary.localReportReference, 'unavailable');
  assert.equal(summary.events.length, 1);
});

test('network reporting is opt-in and projects only fixed fields without IDs, secrets, referrers or cookies', async (t) => {
  const f = await fixture(t);
  f.telemetry.record({ name: 'password_login', outcome: 'ok', password: 'secret' });
  await f.telemetry.flush();
  assert.equal(f.requests.length, 0);
  f.telemetry.setSharing(true);
  f.telemetry.record({
    name: 'password_login',
    outcome: 'ok',
    durationMs: 10.7,
    attempt: 42,
    url: 'https://secret.test',
    message: 'secret',
    token: 'secret',
  });
  await f.telemetry.flush();
  const request = f.requests[0];
  assert.equal(request.url, '/api/telemetry');
  assert.equal(request.options.credentials, 'omit');
  assert.equal(request.options.referrerPolicy, 'no-referrer');
  assert.deepEqual(JSON.parse(request.options.body), {
    version: 1,
    browser: 'firefox',
    events: [{ name: 'password_login', outcome: 'ok', durationMs: 11 }],
  });
  assert.equal(f.telemetry.summary().includes('secret'), false);
  assert.equal(JSON.parse(f.telemetry.summary()).events.at(-1).attempt, 42);
  await f.telemetry.flush();
  assert.equal(f.requests.length, 1, 'server rejection is dropped, never retried');
  assert.equal(JSON.parse(f.telemetry.summary()).undeliveredEvents, 1);
  assert.equal(f.timeouts.size, 0);
});

test('burst, queue, history and batch bounds retain explicit loss; opting out discards queued uploads', async (t) => {
  const f = await fixture(t);
  f.telemetry.setSharing(true);
  for (let index = 0; index < 1000; index++)
    f.telemetry.record({ name: 'js_error', outcome: 'error' });
  assert.equal(JSON.parse(f.telemetry.summary()).droppedEvents, 992);
  for (const name of [
    'connection',
    'room_join',
    'chat_send',
    'reconnect',
    'media_sample',
    'password_login',
    'password_register',
    'media_rtt',
    'auth_restore',
  ]) {
    for (let index = 0; index < 8; index++)
      f.telemetry.record({ name, outcome: 'ok', value: Infinity });
  }
  const summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.events.length, 80);
  assert.equal(summary.pendingEvents, 64);
  assert.equal(summary.droppedEvents, 1008);
  await f.telemetry.flush();
  assert.equal(JSON.parse(f.requests[0].options.body).events.length, 16);
  f.telemetry.setSharing(false);
  await f.telemetry.flush();
  assert.equal(f.requests.length, 1);
  assert.equal(JSON.parse(f.telemetry.summary()).pendingEvents, 0);
});

test('support references rotate locally and measurements expose cancellation uncertainty without raw errors', async (t) => {
  const f = await fixture(t);
  const reference = JSON.parse(f.telemetry.summary()).localReportReference;
  f.advance(30 * 60000);
  await assert.rejects(
    f.telemetry.measure('passkey_login_ceremony', async () => {
      throw new DOMException('private credential detail', 'NotAllowedError');
    }),
  );
  const summary = JSON.parse(f.telemetry.summary());
  assert.notEqual(summary.localReportReference, reference);
  assert.equal(summary.events.at(-1).outcome, 'cancelled_or_timeout');
  assert.equal(f.telemetry.summary().includes('private credential detail'), false);
});

test('call outcomes use local correlation shared with measurements and omit it from uploads', async (t) => {
  const f = await fixture(t);
  f.telemetry.setSharing(true);
  const attempt = f.telemetry.nextAttemptId();
  f.telemetry.record({ name: 'call_join', outcome: 'started', attempt });
  f.telemetry.record({
    name: 'call_join',
    outcome: 'audio_playback_ready',
    attempt,
    durationMs: 2100,
  });
  await f.telemetry.measure('chat_send', async () => {});
  const events = JSON.parse(f.telemetry.summary()).events;
  assert.equal(events[0].attempt, events[1].attempt);
  assert.notEqual(events[1].attempt, events[2].attempt);
  await f.telemetry.flush();
  assert.ok(JSON.parse(f.requests[0].options.body).events.every((event) => !('attempt' in event)));
});
