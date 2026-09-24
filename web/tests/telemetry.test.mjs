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
  assert.equal(summary.events.length, 64, 'routine quality does not occupy lifecycle history');
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

function media(key, values = {}) {
  return {
    key,
    kind: 'video',
    windowMs: 15000,
    frames: 450,
    freezeMs: 0,
    concealment: undefined,
    packetLoss: 37,
    rttMs: 27,
    ...values,
  };
}

test('quality intervals have named units and local stream ordinals without changing anonymous scalar uploads', async (t) => {
  const f = await fixture(t);
  f.advance(15000);
  f.telemetry.setSharing(true);
  const source = { id: 'private-native-id', email: 'private@test.invalid' };
  f.telemetry.recordMediaSample(media(source, { rawStats: 'secret' }));
  f.telemetry.recordMediaSample(media(source, { windowMs: 20000 }));
  f.telemetry.recordMediaSample(media({}, { kind: 'audio', windowMs: undefined }));
  f.telemetry.record({ name: 'media_packet_loss', outcome: 'ok', value: 37 });
  const summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.version, 2);
  assert.equal(summary.events.length, 0);
  assert.deepEqual(summary.mediaSamples[0], {
    atMs: 15000,
    stream: 1,
    kind: 'video',
    windowMs: 15000,
    decodedFrames: 450,
    decodedFps: 30,
    videoFreezeMs: 0,
    packetLossPercent: 0.37,
    rttMs: 27,
  });
  assert.equal(summary.mediaSamples[1].stream, 1);
  assert.equal(summary.mediaSamples[1].windowMs, null, 'interval cannot precede report start');
  assert.deepEqual(summary.mediaSamples[2], {
    atMs: 15000,
    stream: 2,
    kind: 'audio',
    windowMs: null,
    audioConcealmentPercent: null,
    packetLossPercent: null,
    rttMs: 27,
  });
  assert.equal(/private|secret|rawStats/.test(f.telemetry.summary()), false);
  await f.telemetry.flush();
  assert.deepEqual(JSON.parse(f.requests[0].options.body), {
    version: 1,
    browser: 'firefox',
    events: [{ name: 'media_packet_loss', outcome: 'ok', value: 37 }],
  });
});

test('routine quality cannot evict milestones or errors and each history exposes its own truncation window', async (t) => {
  const f = await fixture(t);
  f.telemetry.record({ name: 'call_join', outcome: 'video_ready', attempt: 1 });
  f.telemetry.record({ name: 'media_sample', outcome: 'timeout' });
  f.telemetry.record({ name: 'media_rtt', outcome: 'error' });
  const source = {};
  for (let index = 1; index <= 40; index++) {
    f.advance(15000);
    f.telemetry.recordMediaSample(media(source));
    f.telemetry.record({ name: 'media_sample', outcome: 'ok' });
    f.telemetry.record({ name: 'media_video_progress', outcome: 'ok', value: 450 });
    f.telemetry.record({ name: 'media_packet_loss', outcome: 'unknown' });
  }
  let summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.events.length, 3);
  assert.equal(summary.events[0].outcome, 'video_ready');
  assert.deepEqual(summary.retention.events, {
    capacity: 80,
    evicted: 0,
    oldestAtMs: 0,
    newestAtMs: 0,
  });
  assert.equal(summary.mediaSamples.length, 32);
  assert.deepEqual(summary.retention.mediaSamples, {
    capacity: 32,
    evicted: 8,
    oldestAtMs: 135000,
    newestAtMs: 600000,
  });
  // A distinct lifecycle overflow reports its own eviction rather than silently truncating.
  for (let index = 0; index < 85; index++) {
    for (const tick of f.intervals.values()) tick();
    f.advance(1);
    f.telemetry.record({ name: 'js_error', outcome: 'error' });
  }
  summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.events.length, 80);
  assert.equal(summary.retention.events.evicted, 8);
  assert.equal(summary.retention.events.oldestAtMs, 600006);
  assert.equal(summary.retention.mediaSamples.evicted, 8);
});

test('report rotation resets histories and stream ordinals even when only exporting an idle preview', async (t) => {
  const f = await fixture(t);
  f.advance(15000);
  const first = {};
  const second = {};
  f.telemetry.recordMediaSample(media(first));
  f.telemetry.recordMediaSample(media(second));
  f.telemetry.record({ name: 'call_join', outcome: 'video_ready' });
  const before = JSON.parse(f.telemetry.summary());
  f.advance(30 * 60000);
  const rotated = JSON.parse(f.telemetry.summary());
  assert.notEqual(rotated.localReportReference, before.localReportReference);
  assert.equal(rotated.snapshotAtMs, 0);
  assert.deepEqual(rotated.events, []);
  assert.deepEqual(rotated.mediaSamples, []);
  assert.equal(rotated.retention.events.oldestAtMs, null);
  assert.equal(rotated.retention.mediaSamples.evicted, 0);
  f.telemetry.recordMediaSample(media(second));
  let sample = JSON.parse(f.telemetry.summary()).mediaSamples[0];
  assert.equal(sample.stream, 1);
  assert.equal(sample.windowMs, null, 'a result spanning the previous report has no interval');
  f.advance(15000);
  f.telemetry.recordMediaSample(media(second));
  sample = JSON.parse(f.telemetry.summary()).mediaSamples.at(-1);
  assert.equal(sample.windowMs, 15000);
  assert.equal(sample.decodedFps, 30);
});

test('stream identities stay bounded and local values reject nonfinite, reset and impossible measurements', async (t) => {
  const f = await fixture(t);
  f.advance(15000);
  for (let index = 0; index < 66; index++) f.telemetry.recordMediaSample(media({}));
  let summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.retention.unidentifiedStreamSamples, 2);
  assert.equal(summary.mediaSamples.at(-3).stream, 64);
  assert.equal(summary.mediaSamples.at(-1).stream, null);
  f.telemetry.recordMediaSample(
    media({}, { frames: Infinity, freezeMs: 16000, packetLoss: -1, rttMs: NaN }),
  );
  summary = JSON.parse(f.telemetry.summary());
  assert.equal(summary.mediaSamples.at(-1).decodedFrames, null);
  assert.equal(summary.mediaSamples.at(-1).decodedFps, null);
  assert.equal(summary.mediaSamples.at(-1).videoFreezeMs, null);
  assert.equal(summary.mediaSamples.at(-1).packetLossPercent, null);
  assert.equal(summary.mediaSamples.at(-1).rttMs, null);
  f.telemetry.dispose();
  f.telemetry.recordMediaSample(media({}));
  assert.deepEqual(JSON.parse(f.telemetry.summary()).mediaSamples, []);
});
