import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

function report(stats) {
  return new Map([['private-id', { type: 'inbound-rtp', kind: 'audio', ...stats }]]);
}
async function flush() {
  for (let index = 0; index < 10; index++) await Promise.resolve();
}

test('media extracts only allowed numbers and missing, reset, silent counters remain unknown', async () => {
  const { readMediaSample, sampleChanges } = await loadTypeScript('src/media-telemetry.ts');
  const empty = readMediaSample(report({ address: 'private', trackIdentifier: 'secret' }), 'audio');
  assert.ok(Object.values(empty).every((value) => value === undefined));
  assert.ok(Object.values(sampleChanges(empty, undefined)).every((value) => value === undefined));
  const previous = readMediaSample(
    report({
      packetsLost: 1,
      packetsReceived: 99,
      totalSamplesReceived: 1000,
      concealedSamples: 100,
    }),
    'audio',
  );
  const next = readMediaSample(
    report({
      packetsLost: 2,
      packetsReceived: 198,
      totalSamplesReceived: 2000,
      concealedSamples: 150,
    }),
    'audio',
  );
  const changes = sampleChanges(next, previous);
  assert.equal(changes.packetLoss, 100);
  assert.equal(changes.concealment, 500);
  assert.equal(sampleChanges(previous, next).packetLoss, undefined);
  assert.equal(sampleChanges(previous, previous).concealment, undefined);
});

test('sampling caps outstanding native calls, ignores late/hidden/paused results and removes listeners on teardown', async () => {
  const timeouts = new Map();
  const listeners = new Map();
  let sequence = 0;
  const document = {
    visibilityState: 'visible',
    addEventListener: (key, value) => listeners.set(key, value),
    removeEventListener: (key) => listeners.delete(key),
  };
  const { MediaTelemetry } = await loadTypeScript('src/media-telemetry.ts', {
    globals: {
      document,
      setInterval: () => 1,
      clearInterval() {},
      setTimeout: (callback) => {
        timeouts.set(++sequence, callback);
        return sequence;
      },
      clearTimeout: (key) => timeouts.delete(key),
    },
  });
  const events = [];
  const pending = [];
  let calls = 0;
  let active = true;
  const sources = Array.from({ length: 10 }, () => ({
    key: {},
    kind: 'audio',
    active: () => active,
    getStats: () => {
      calls++;
      return new Promise((resolve) => pending.push(resolve));
    },
  }));
  const sampler = new MediaTelemetry(
    () => sources,
    (event) => events.push(event),
  );
  sampler.sample();
  await flush();
  assert.equal(calls, 4);
  for (const callback of [...timeouts.values()]) callback();
  sampler.sample();
  await flush();
  assert.equal(calls, 4, 'timeouts cannot accumulate unbounded native calls');
  for (const resolve of pending.splice(0)) resolve(report({ packetsReceived: 500 }));
  await flush();
  assert.equal(events.filter((event) => event.outcome === 'timeout').length, 4);
  assert.equal(events.at(-1).outcome, 'unavailable');
  active = false;
  sampler.sample();
  await flush();
  assert.equal(calls, 4);
  active = true;
  document.visibilityState = 'hidden';
  sampler.sample();
  await flush();
  assert.equal(calls, 4);
  sampler.dispose();
  assert.equal(listeners.size, 0);
  assert.equal(timeouts.size, 0);
});

test('first-frame latency uses presentation callbacks and hidden elements remain unknown', async () => {
  const listeners = new Map();
  const events = [];
  let callback;
  let now = 50;
  let cleared = false;
  const document = {
    visibilityState: 'visible',
    addEventListener: (name, fn) => listeners.set(name, fn),
    removeEventListener: (name) => listeners.delete(name),
  };
  const { observeFirstVideoFrame } = await loadTypeScript('src/media-telemetry.ts', {
    globals: {
      document,
      performance: { now: () => now },
      setTimeout: () => 1,
      clearTimeout: () => {
        cleared = true;
      },
    },
  });
  const video = {
    isConnected: true,
    srcObject: {},
    requestVideoFrameCallback: (fn) => {
      callback = fn;
      return 1;
    },
    cancelVideoFrameCallback() {},
    addEventListener() {},
    removeEventListener() {},
  };
  observeFirstVideoFrame(video, (event) => events.push(event));
  now = 173;
  callback();
  assert.deepEqual(events, [{ name: 'media_first_video_frame', outcome: 'ok', durationMs: 123 }]);
  assert.equal(cleared, true);
  assert.equal(listeners.size, 0);
  document.visibilityState = 'hidden';
  observeFirstVideoFrame(video, (event) => events.push(event));
  assert.equal(events.at(-1).outcome, 'unknown');
});

test('pending calls share bounded sampling while quality events keep their normal cadence', async () => {
  let tick;
  let nativeCalls = 0;
  let callSamples = 0;
  const events = [];
  const calls = { pending: true, sample: () => callSamples++ };
  const { MediaTelemetry } = await loadTypeScript('src/media-telemetry.ts', {
    globals: {
      document: { visibilityState: 'visible', addEventListener() {}, removeEventListener() {} },
      setInterval: (callback) => {
        tick = callback;
        return 1;
      },
      clearInterval() {},
    },
  });
  const sampler = new MediaTelemetry(
    () => [
      {
        key: {},
        kind: 'audio',
        active: () => true,
        getStats: async () => {
          nativeCalls++;
          return report({ totalSamplesReceived: nativeCalls * 100, concealedSamples: 0 });
        },
      },
    ],
    (event) => events.push(event),
    calls,
  );
  for (let index = 0; index < 3; index++) {
    tick();
    await flush();
  }
  assert.equal(nativeCalls, 3);
  assert.equal(callSamples, 3);
  assert.equal(events.length, 0, 'fast readiness observations do not flood the upload queue');
  calls.pending = false;
  for (let index = 0; index < 11; index++) {
    tick();
    await flush();
  }
  assert.equal(nativeCalls, 3);
  tick();
  await flush();
  assert.equal(nativeCalls, 4);
  assert.ok(events.some((event) => event.name === 'media_sample'));
  sampler.dispose();
  const broken = new MediaTelemetry(
    () => {
      throw new Error('retired getter');
    },
    () => {
      throw new Error('retired recorder');
    },
  );
  assert.doesNotThrow(() => broken.sample());
  broken.dispose();
});

test('quality intervals follow native timestamps and reject reset, replaced or ambiguous inbound counters', async () => {
  const { readMediaSample, sampleChanges } = await loadTypeScript('src/media-telemetry.ts');
  const before = {
    identity: 'private-inbound-a',
    timestamp: 1000,
    frames: 100,
    freezeSeconds: 1,
    lost: 1,
    received: 99,
  };
  const after = {
    ...before,
    timestamp: 21000,
    frames: 700,
    freezeSeconds: 1.2,
    lost: 2,
    received: 198,
  };
  const changes = sampleChanges(after, before);
  assert.equal(changes.windowMs, 20000);
  assert.equal(changes.frames, 600);
  assert.equal(changes.packetLoss, 100);
  assert.ok(Math.abs(changes.freezeMs - 200) < 0.001);
  assert.equal(sampleChanges({ ...after, frames: 1 }, before).frames, undefined);
  for (const invalid of [
    { ...after, timestamp: 1000 },
    { ...after, timestamp: 999 },
    { ...after, timestamp: 30 * 60000 + 1001 },
    { ...after, identity: 'private-inbound-b' },
  ]) {
    assert.ok(Object.values(sampleChanges(invalid, before)).every((value) => value === undefined));
  }
  assert.equal(sampleChanges({ ...after, timestamp: undefined }, before).windowMs, undefined);
  const ambiguous = report({ timestamp: 1000, packetsReceived: 500 });
  ambiguous.set('second-private-id', { type: 'inbound-rtp', kind: 'audio', packetsReceived: 900 });
  assert.deepEqual(readMediaSample(ambiguous, 'audio'), {});
});

test('grouped samples retain real cadence, survive callback failure and reset after visibility, pause and source replacement', async () => {
  const listeners = new Map();
  const document = {
    visibilityState: 'visible',
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const { MediaTelemetry } = await loadTypeScript('src/media-telemetry.ts', {
    globals: { document, setInterval: () => 1, clearInterval() {} },
  });
  let timestamp = 1000;
  let active = true;
  let key = {};
  let defer;
  const samples = [];
  const sampler = new MediaTelemetry(
    () => [
      {
        key,
        kind: 'video',
        active: () => active,
        getStats: () =>
          defer ??
          Promise.resolve(
            report({
              kind: 'video',
              timestamp,
              framesDecoded: (timestamp / 1000) * 30,
            }),
          ),
      },
    ],
    () => {},
    undefined,
    (sample) => {
      samples.push(sample);
      if (samples.length === 1) throw new Error('retired local recorder');
    },
  );
  sampler.sample();
  await flush();
  assert.equal(samples[0].windowMs, undefined);
  timestamp = 17000;
  sampler.sample();
  await flush();
  assert.equal(samples[1].windowMs, 16000);
  assert.equal(samples[1].frames, 480);
  document.visibilityState = 'hidden';
  listeners.get('visibilitychange')();
  document.visibilityState = 'visible';
  listeners.get('visibilitychange')();
  timestamp = 33000;
  sampler.sample();
  await flush();
  assert.equal(samples.at(-1).windowMs, undefined);
  active = false;
  sampler.sample();
  active = true;
  timestamp = 49000;
  sampler.sample();
  await flush();
  assert.equal(samples.at(-1).windowMs, undefined);
  let complete;
  defer = new Promise((resolve) => {
    complete = resolve;
  });
  sampler.sample();
  await flush();
  key = {};
  defer = undefined;
  sampler.sample();
  await flush();
  const length = samples.length;
  complete(report({ kind: 'video', timestamp: 65000, framesDecoded: 1950 }));
  await flush();
  assert.equal(samples.length, length, 'late result from retired source never reaches export');
  assert.equal(samples.at(-1).windowMs, undefined);
  sampler.dispose();
});
