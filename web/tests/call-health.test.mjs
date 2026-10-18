import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush } from './ui-fixture.mjs';

const readers = await loadTypeScript('src/media-telemetry.ts');

async function fixture(overrides = {}) {
  let now = 0;
  let sequence = 0;
  const timers = new Map();
  const listeners = new Map();
  const document = {
    visibilityState: 'visible',
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const api = await loadTypeScript('src/call-health.ts', {
    modules: { './media-telemetry': readers },
    globals: {
      document,
      performance: { now: () => now },
      setInterval: () => 1,
      clearInterval() {},
      setTimeout: (callback) => {
        const id = ++sequence;
        timers.set(id, callback);
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  const state = {
    connected: true,
    receiveRecovering: false,
    receiveFailed: false,
    microphone: 'off',
    camera: 'off',
    sharing: false,
    sendState: 'connected',
    receiveState: 'connected',
    expected: 0,
    selected: 0,
    unavailable: false,
    ...overrides,
  };
  const context = { scope: {}, membership: 1, state };
  const sources = [];
  const playback = [];
  const snapshots = [];
  const health = new api.CallHealth({
    context: () => context,
    sources: () => sources,
    playback: () => playback,
    changed: (snapshot) => snapshots.push(snapshot),
  });
  return {
    api,
    health,
    state,
    context,
    sources,
    playback,
    snapshots,
    document,
    listeners,
    timers,
    advance: (value) => {
      now += value;
    },
  };
}

function source(direction = 'receiving', kind = 'audio') {
  let active = true;
  let timestamp = 0;
  let packets = 0;
  let calls = 0;
  return {
    key: {},
    direction,
    kind,
    active: () => active,
    getStats: () => {
      calls++;
      return Promise.resolve(
        new Map([
          [
            'private',
            {
              id: 'native-private-id',
              type: direction === 'sending' ? 'outbound-rtp' : 'inbound-rtp',
              kind,
              timestamp,
              ...(direction === 'sending'
                ? { packetsSent: packets }
                : { packetsReceived: packets }),
              address: 'private-address',
              trackIdentifier: 'private-person',
            },
          ],
        ]),
      );
    },
    advance() {
      timestamp += 2000;
      packets += 1;
    },
    stop() {
      active = false;
    },
    calls: () => calls,
  };
}

test('audio-only current packet progress is good even at tiny bitrate; silence and resets stay unknown', async () => {
  const f = await fixture({ microphone: 'live', expected: 1, selected: 1 });
  const outgoing = source('sending');
  const incoming = source();
  f.sources.push(outgoing, incoming);
  f.health.sample();
  await flush();
  assert.equal(f.health.snapshot().sending.level, 'unknown');
  outgoing.advance();
  incoming.advance();
  f.advance(2000);
  f.health.sample();
  await flush();
  assert.equal(f.health.snapshot().sending.level, 'good');
  assert.equal(f.health.snapshot().receiving.level, 'good');
  const silence = f.api.callHealthChanges(
    { timestamp: 4000, received: 1 },
    { timestamp: 2000, received: 1 },
    'audio',
  );
  assert.equal(silence.level, 'unknown');
  assert.equal(
    f.api.callHealthChanges(
      { timestamp: 1, received: 1 },
      { timestamp: 2000, received: 9 },
      'audio',
    ).level,
    'unknown',
  );
  assert.doesNotMatch(JSON.stringify(f.snapshots), /native-private|private-address|private-person/);
  f.health.dispose();
});

test('stats readers retain fixed counters only and reject ambiguous native streams', async () => {
  const { api, health } = await fixture();
  const entry = { key: {}, direction: 'sending', kind: 'video' };
  const row = {
    type: 'outbound-rtp',
    kind: 'video',
    timestamp: 1000,
    packetsSent: 4,
    framesEncoded: 2,
    address: 'private-address',
    codecId: 'private-codec',
    ssrc: 1234,
  };
  const sample = api.readCallHealthSample(new Map([['id', row]]), entry);
  assert.equal(sample.received, 4);
  assert.equal(sample.frames, 2);
  assert.doesNotMatch(JSON.stringify(sample), /private|1234/);
  assert.deepEqual(
    Object.keys(
      api.readCallHealthSample(
        new Map([
          ['a', row],
          ['b', row],
        ]),
        entry,
      ),
    ),
    [],
  );
  health.dispose();
});

test('outgoing simulcast uses all encoding counters without retaining native reports', async () => {
  const f = await fixture({ camera: 'live' });
  const outgoing = { key: {}, direction: 'sending', kind: 'video', active: () => true };
  let timestamp = 1000;
  let frames = 10;
  let reverse = false;
  const report = () => {
    const rows = ['low-private', 'medium-private', 'high-private'].map((id, index) => [
      id,
      {
        id,
        type: 'outbound-rtp',
        kind: 'video',
        timestamp,
        packetsSent: frames * (index + 1),
        framesEncoded: frames,
        address: 'private-network-address',
        ssrc: 123456,
      },
    ]);
    return new Map(reverse ? rows.reverse() : rows);
  };
  outgoing.getStats = () => Promise.resolve(report());
  f.sources.push(outgoing);
  f.health.sample();
  await flush();
  assert.equal(f.health.snapshot().sending.level, 'unknown');
  timestamp += 2000;
  frames++;
  reverse = true;
  f.advance(2000);
  f.health.sample();
  await flush();
  assert.equal(f.health.snapshot().sending.level, 'good');
  assert.doesNotMatch(JSON.stringify(f.snapshots), /private|123456/);
  f.health.dispose();
});

test('simulcast layer changes and individual resets cannot be hidden by aggregate growth', async () => {
  const { api, health } = await fixture();
  const outgoing = { key: {}, direction: 'sending', kind: 'video' };
  const sample = (timestamp, values) =>
    api.readCallHealthSample(
      new Map(
        values.map(([id, packetsSent, framesEncoded], index) => [
          index,
          { id, type: 'outbound-rtp', kind: 'video', timestamp, packetsSent, framesEncoded },
        ]),
      ),
      outgoing,
    );
  const before = sample(1000, [
    ['a', 10, 10],
    ['b', 20, 20],
    ['c', 30, 30],
  ]);
  for (const values of [
    [
      ['a', 11, 11],
      ['b', 100, 100],
      ['c', 1, 1],
    ],
    [
      ['a', 11, 11],
      ['b', 21, 21],
    ],
    [
      ['a', 11, 11],
      ['b', 21, 21],
      ['new', 31, 31],
    ],
    [
      ['a', 11, 11],
      ['b', 1, 21],
      ['c', 100, 100],
    ],
  ]) {
    const current = sample(3000, values);
    assert.equal(api.callHealthChanges(current, before, 'video').level, 'unknown');
    const next = sample(
      5000,
      values.map(([id, packets, frames]) => [id, packets + 1, frames + 1]),
    );
    assert.equal(api.callHealthChanges(next, current, 'video').level, 'good');
  }
  for (const values of [
    Array.from({ length: 9 }, (_, index) => [`layer-${index}`, 1, 1]),
    [
      ['a', 1, 1],
      ['a', 2, 2],
    ],
    [
      ['a'.repeat(257), 1, 1],
      ['b', 2, 2],
    ],
  ])
    assert.deepEqual(Object.keys(sample(3000, values)), []);
  health.dispose();
});

test('frequent signaling and UI updates cannot repeatedly poll a fast native source', async () => {
  const f = await fixture({ expected: 1, selected: 1 });
  const incoming = source();
  f.sources.push(incoming);
  f.health.sample();
  await flush();
  for (let index = 0; index < 30; index++) {
    f.health.sample();
    await flush();
  }
  assert.equal(incoming.calls(), 1);
  f.advance(2000);
  incoming.advance();
  f.health.sample();
  await flush();
  assert.equal(incoming.calls(), 2);
  f.health.dispose();
});

test('stale, hidden, replaced membership and missing receivers never retain a green result', async () => {
  const f = await fixture({ expected: 1, selected: 1 });
  const incoming = source();
  f.sources.push(incoming);
  f.health.sample();
  await flush();
  incoming.advance();
  f.advance(2000);
  f.health.sample();
  await flush();
  assert.equal(f.health.snapshot().receiving.level, 'good');
  f.advance(15001);
  assert.equal(f.health.snapshot().receiving.level, 'unknown');
  incoming.advance();
  f.health.sample();
  await flush();
  f.context.membership++;
  assert.equal(f.health.snapshot().receiving.level, 'unknown');
  f.state.selected = 2;
  assert.equal(f.health.snapshot().receiving.label, 'Waiting for selected media');
  f.document.visibilityState = 'hidden';
  f.listeners.get('visibilitychange')();
  assert.equal(f.health.snapshot().receiving.level, 'unknown');
  f.health.dispose();
  assert.equal(f.listeners.size, 0);
  assert.equal(f.timers.size, 0);
});

test('four hung native calls remain busy after timeout and cannot overlap or update a later room', async () => {
  const f = await fixture({ expected: 8, selected: 8 });
  const pending = [];
  let calls = 0;
  for (let index = 0; index < 8; index++) {
    const request = deferred();
    pending.push(request);
    f.sources.push({
      key: {},
      direction: 'receiving',
      kind: 'audio',
      active: () => true,
      getStats: () => {
        calls++;
        return request.promise;
      },
    });
  }
  f.health.sample();
  await flush();
  assert.equal(calls, 4);
  for (const callback of [...f.timers.values()]) callback();
  for (let index = 0; index < 20; index++) f.health.sample();
  await flush();
  assert.equal(calls, 4, 'a timeout must not release unresolved native capacity');
  f.context.scope = {};
  for (const request of pending) request.resolve(new Map());
  await flush();
  assert.equal(f.health.snapshot().receiving.level, 'unknown');
  f.health.dispose();
});

test('deliberately off devices, interrupted capture, receive repair and playback are separate evidence', async () => {
  const f = await fixture();
  assert.equal(f.health.snapshot().sending.level, 'idle');
  f.state.microphone = 'interrupted';
  assert.equal(f.health.snapshot().sending.label, 'Microphone interrupted');
  f.state.microphone = 'off';
  f.state.receiveRecovering = true;
  assert.equal(f.health.snapshot().sending.level, 'idle');
  assert.equal(f.health.snapshot().receiving.label, 'Refreshing incoming media');
  const element = {
    isConnected: true,
    srcObject: {},
    tagName: 'AUDIO',
    muted: false,
    volume: 1,
    paused: true,
    error: null,
    readyState: 4,
  };
  f.playback.push(element);
  f.health.playbackResult(element, true);
  assert.equal(f.health.snapshot().playback.label, 'Playback blocked');
  f.health.playbackResult(element, false);
  element.paused = false;
  assert.equal(f.health.snapshot().playback.label, 'Audio playback running');
  element.muted = true;
  assert.equal(f.health.snapshot().playback.level, 'idle');
  assert.equal(f.sources.length, 0, 'local playback state never requests capture or native stats');
  f.health.dispose();
});

test('selected playback waiting or paused video cannot claim a healthy local renderer', async () => {
  const f = await fixture({ expected: 1, selected: 1 });
  assert.equal(f.health.snapshot().playback.label, 'Waiting for playback');
  const element = {
    isConnected: true,
    srcObject: {},
    tagName: 'VIDEO',
    muted: true,
    volume: 1,
    paused: true,
    error: null,
    readyState: 4,
  };
  f.playback.push(element);
  assert.equal(f.health.snapshot().playback.label, 'Video playback paused');
  f.health.playbackResult(element, true);
  assert.equal(f.health.snapshot().playback.label, 'Playback blocked');
  element.paused = false;
  f.health.playbackResult(element, false);
  assert.equal(f.health.snapshot().playback.label, 'Video playback running');
  f.health.dispose();
});

test('loss/concealment/freeze warnings require supported interval evidence', async () => {
  const f = await fixture();
  assert.equal(
    f.api.callHealthChanges(
      { timestamp: 3000, received: 90, lost: 10 },
      { timestamp: 1000, received: 0, lost: 0 },
      'audio',
    ).label,
    'Packet loss observed',
  );
  assert.equal(
    f.api.callHealthChanges(
      { timestamp: 3000, samples: 100, concealed: 20 },
      { timestamp: 1000, samples: 0, concealed: 0 },
      'audio',
    ).label,
    'Audio gaps observed',
  );
  assert.equal(
    f.api.callHealthChanges(
      { timestamp: 3000, frames: 20, freezeSeconds: 1 },
      { timestamp: 1000, frames: 10, freezeSeconds: 0 },
      'video',
    ).label,
    'Video freezes observed',
  );
  assert.equal(
    f.api.callHealthChanges({ timestamp: 3000 }, { timestamp: 1000 }, 'video').level,
    'unknown',
  );
  f.health.dispose();
});
