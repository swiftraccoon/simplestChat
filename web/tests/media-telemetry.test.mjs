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
