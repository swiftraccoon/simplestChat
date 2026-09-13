import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import test from 'node:test';
import { compileFunction } from 'node:vm';

const require = createRequire(import.meta.url);
const source = await readFile(new URL('../e2e/lifecycle-checks.cjs', import.meta.url), 'utf8');
const {
  configuration,
  serverCounts,
  mediaProgress,
  releasedResources,
} = require('../e2e/lifecycle-checks.cjs');
const allowed = {
  LIFECYCLE_E2E: '1',
  DISPOSABLE_TEST_DATABASE: '1',
  TEST_METRICS_TOKEN: 'a'.repeat(64),
  TEST_SERVER_PID: '12345',
};

test('lifecycle configuration requires explicit disposable-service ownership and a metrics credential', () => {
  for (const key of Object.keys(allowed)) {
    const env = { ...allowed };
    delete env[key];
    assert.throws(() => configuration(env), undefined, `missing ${key}`);
    assert.throws(() => configuration({ ...allowed, [key]: '' }), undefined, `empty ${key}`);
  }
  for (const token of ['a'.repeat(63), 'a'.repeat(65), 'g'.repeat(64), 'A'.repeat(64)]) {
    assert.throws(() => configuration({ ...allowed, TEST_METRICS_TOKEN: token }), /server helper/);
  }
  for (const pid of [
    '0',
    '-1',
    '01',
    '1.5',
    '123x',
    '2147483648',
    '9007199254740992',
    '9'.repeat(400),
  ]) {
    assert.throws(() => configuration({ ...allowed, TEST_SERVER_PID: pid }), undefined, pid);
  }
});

test('lifecycle configuration accepts only credential-free loopback origins', () => {
  for (const origin of [
    'http://127.0.0.1:3119',
    'http://localhost:3119',
    'http://[::1]:3119',
    'https://localhost:3119',
  ]) {
    assert.equal(configuration({ ...allowed, BASE_URL: origin }).base, origin);
  }
  for (const origin of [
    'https://example.test',
    'http://192.168.1.2',
    'http://0.0.0.0',
    'http://localhost.example.test',
    'http://user:secret@localhost',
    'http://localhost/room',
    'http://localhost/?room=private',
    'http://localhost/#room',
    'file://localhost/',
    'ws://localhost:3119',
    'invalid-url',
  ]) {
    assert.throws(() => configuration({ ...allowed, BASE_URL: origin }), undefined, origin);
  }
});

test('lifecycle workloads have bounded defaults and reject invalid or oversized overrides', () => {
  assert.deepEqual(configuration(allowed), {
    base: 'http://127.0.0.1:3119',
    cycles: 6,
    mediaSeconds: 30,
    serverPid: 12345,
  });
  for (const [key, minimum, maximum, field] of [
    ['LIFECYCLE_CYCLES', 3, 10, 'cycles'],
    ['LIFECYCLE_MEDIA_SECONDS', 30, 120, 'mediaSeconds'],
  ]) {
    for (const value of [minimum, maximum])
      assert.equal(configuration({ ...allowed, [key]: String(value) })[field], value);
    for (const value of [
      '',
      '0',
      '-1',
      '1.5',
      ' 6',
      '6 ',
      '06',
      '1e2',
      String(minimum - 1),
      String(maximum + 1),
      '9'.repeat(400),
    ]) {
      assert.throws(
        () => configuration({ ...allowed, [key]: value }),
        undefined,
        `${key}=${value}`,
      );
    }
  }
});

const gauges = {
  simplestchat_participants_snapshot_complete: 1,
  simplestchat_rooms_active: 2,
  simplestchat_participants_active: 3,
  simplestchat_connections_active: 4,
};
const metrics = (values = gauges) =>
  Object.entries(values)
    .map(([key, value]) => `${key} ${value}`)
    .join('\n');

test('server lifecycle counts read only a complete, unique required-gauge snapshot', () => {
  assert.deepEqual(serverCounts(`${metrics()}\n# HELP unrelated ignored\nunrelated 100\n`), {
    rooms: 2,
    participants: 3,
    connections: 4,
  });
  assert.deepEqual(
    serverCounts(
      metrics({
        ...gauges,
        simplestchat_rooms_active: 0,
        simplestchat_participants_active: 0,
        simplestchat_connections_active: 0,
      }),
    ),
    {
      rooms: 0,
      participants: 0,
      connections: 0,
    },
  );
  for (const value of [undefined, null, {}, '', 'x'.repeat(131073)]) {
    assert.throws(() => serverCounts(value));
  }
});

for (const name of Object.keys(gauges)) {
  test(`server lifecycle counts reject missing, duplicate, and malformed ${name}`, () => {
    const missing = { ...gauges };
    delete missing[name];
    assert.throws(() => serverCounts(metrics(missing)), undefined, 'missing is not zero');
    assert.throws(
      () => serverCounts(`${metrics()}\n${name} ${gauges[name]}`),
      undefined,
      'duplicate',
    );
    assert.throws(
      () => serverCounts(`${metrics()}\n${name} NaN`),
      undefined,
      'malformed duplicate',
    );
    for (const value of ['NaN', '+Inf', '-1', '1.5', '9007199254740992', '1 unexpected']) {
      assert.throws(() => serverCounts(metrics({ ...gauges, [name]: value })), undefined, value);
    }
  });
}

test('server lifecycle counts reject explicitly incomplete participant snapshots', () => {
  for (const value of [0, 2]) {
    assert.throws(
      () =>
        serverCounts(metrics({ ...gauges, simplestchat_participants_snapshot_complete: value })),
      /incomplete/,
    );
  }
});

function sample(video = 20, audio = 30) {
  return {
    peers: [
      {
        peer: 1,
        inbound: [
          { stream: 1, kind: 'video', framesDecoded: video },
          { stream: 2, kind: 'audio', packetsReceived: audio },
        ],
      },
    ],
  };
}

test('media progress reports decoded video and audio packets only on continuing native identities', () => {
  const before = sample();
  const after = sample(25, 37);
  after.peers[0].inbound.push({ stream: 3, kind: 'video', framesDecoded: 10000 });
  after.peers.push({ peer: 2, inbound: [{ stream: 2, kind: 'audio', packetsReceived: 10000 }] });
  assert.deepEqual(mediaProgress(before, after), { videoFrames: 5, audioPackets: 7 });
  after.peers[0].inbound.reverse();
  assert.deepEqual(mediaProgress(before, after), { videoFrames: 5, audioPackets: 7 });
});

for (const kind of ['video', 'audio']) {
  test(`${kind} progress rejects counter resets, no progress, missing counters, and replacement streams`, () => {
    const field = kind === 'video' ? 'framesDecoded' : 'packetsReceived';
    const index = kind === 'video' ? 0 : 1;
    for (const value of [
      -1,
      0,
      kind === 'video' ? 20 : 30,
      undefined,
      NaN,
      Infinity,
      1.5,
      '100',
      Number.MAX_SAFE_INTEGER + 1,
    ]) {
      const after = sample(25, 37);
      after.peers[0].inbound[index][field] = value;
      assert.throws(() => mediaProgress(sample(), after), undefined, `${field}=${value}`);
    }
    for (const mutation of ['stream', 'peer', 'kind', 'removed']) {
      const after = sample(25, 37);
      if (mutation === 'stream') after.peers[0].inbound[index].stream = 50;
      if (mutation === 'peer') after.peers[0].peer = 50;
      if (mutation === 'kind') after.peers[0].inbound[index].kind = 'unknown';
      if (mutation === 'removed') after.peers[0].inbound.splice(index, 1);
      assert.throws(() => mediaProgress(sample(), after), undefined, mutation);
    }
    for (const value of [-1, undefined, NaN, Infinity, 1.5, '20', Number.MAX_SAFE_INTEGER + 1]) {
      const before = sample();
      before.peers[0].inbound[index][field] = value;
      assert.throws(
        () => mediaProgress(before, sample(25, 37)),
        undefined,
        `invalid prior ${field}`,
      );
    }
  });
}

for (const side of ['before', 'after']) {
  test(`media progress rejects duplicate native identities in ${side} instead of inflating deltas`, () => {
    for (const duplicate of ['stream', 'peer']) {
      const before = sample();
      const after = sample(25, 37);
      const target = side === 'before' ? before : after;
      if (duplicate === 'stream') target.peers[0].inbound.push({ ...target.peers[0].inbound[0] });
      else target.peers.push(structuredClone(target.peers[0]));
      assert.throws(() => mediaProgress(before, after), undefined, duplicate);
    }
  });
}

const released = {
  openPeers: 0,
  liveLocalTracks: 0,
  pendingCaptures: 0,
  attachedMediaElements: 0,
  openSockets: 0,
};

test('explicit leave requires every observed resource to return to zero', () => {
  assert.doesNotThrow(() => releasedResources(released));
  for (const field of Object.keys(released)) {
    for (const value of [1, -1, undefined, null, '0', NaN]) {
      assert.throws(() => releasedResources({ ...released, [field]: value }), new RegExp(field));
    }
  }
});

test('between-cycle cleanup permits only the intentional single join-screen socket', () => {
  const startup = { ...released, openSockets: 1 };
  assert.doesNotThrow(() => releasedResources(startup, 1));
  assert.throws(() => releasedResources(startup), /openSockets/);
  assert.throws(() => releasedResources(released, 1), /openSockets/);
  for (const field of Object.keys(released).filter((field) => field !== 'openSockets')) {
    assert.throws(() => releasedResources({ ...startup, [field]: 1 }, 1), new RegExp(field));
  }
  assert.throws(() => releasedResources({ ...startup, openSockets: 2 }, 1), /openSockets/);
});

test('cleanup rejects invalid expected signaling baselines instead of accepting leaked sockets', () => {
  for (const expected of [-1, 2, 1.5, null, '0', '1', NaN, Infinity]) {
    assert.throws(() => releasedResources({ ...released, openSockets: expected }, expected));
  }
});

function gateFixture() {
  const timers = new Map();
  let nextTimer = 0;
  const module = { exports: {} };
  compileFunction(source, ['require', 'module', 'exports', 'setTimeout', 'clearTimeout'])(
    require,
    module,
    module.exports,
    (callback, milliseconds) => {
      assert.equal(milliseconds, 50000);
      const id = ++nextTimer;
      timers.set(id, callback);
      return id;
    },
    (id) => timers.delete(id),
  );
  return {
    gate: module.exports.handshakeGate(),
    timers,
    expire() {
      assert.equal(timers.size, 1, 'exactly one owned deadline');
      const callback = timers.values().next().value;
      callback();
    },
  };
}

function routeFixture() {
  const calls = [];
  return {
    calls,
    route: {
      connectToServer(...args) {
        assert.equal(args.length, 0, 'connect the actual route without rewritten options');
        calls.push('connect');
        return { send: () => assert.fail('must not forge upstream messages') };
      },
      async close(options) {
        assert.deepEqual(options, { code: 1001 });
        calls.push('close');
      },
      send: () => assert.fail('must not forge page-side messages'),
      onMessage: () => assert.fail('must not intercept or rewrite signaling payloads'),
    },
  };
}

test('unarmed handshake routes connect normally without payload handling', async () => {
  const { gate, timers } = gateFixture();
  const { route, calls } = routeFixture();
  await gate.handle(route);
  assert.deepEqual(calls, ['connect']);
  assert.equal(timers.size, 0);
  assert.deepEqual(gate.snapshot(), { armed: false, held: false, intercepted: 0, failure: null });
  gate.dispose();
});

test('armed handshake gate holds the real connect until explicit release', async () => {
  const { gate, timers } = gateFixture();
  const { route, calls } = routeFixture();
  gate.arm();
  assert.throws(() => gate.arm(), /cannot be armed/);
  const pending = gate.handle(route);
  await Promise.resolve();
  assert.deepEqual(calls, []);
  assert.deepEqual(gate.snapshot(), { armed: false, held: true, intercepted: 1, failure: null });
  assert.equal(timers.size, 1);
  assert.throws(() => gate.arm(), /cannot be armed/);
  gate.release();
  await pending;
  assert.deepEqual(calls, ['connect']);
  assert.equal(timers.size, 0);
  assert.throws(() => gate.release(), /No healthy held handshake/);
  gate.dispose();
});

for (const operation of ['dispose', 'deadline']) {
  for (const stage of ['armed', 'held']) {
    test(`${operation} while ${stage} closes owned routes and never connects later`, async () => {
      const f = gateFixture();
      const first = routeFixture();
      f.gate.arm();
      const pending = stage === 'held' ? f.gate.handle(first.route) : undefined;
      if (operation === 'dispose') f.gate.dispose();
      else f.expire();
      if (pending) await pending;
      else await f.gate.handle(first.route);
      assert.deepEqual(first.calls, ['close']);
      assert.equal(f.timers.size, 0);
      assert.equal(f.gate.snapshot().held, false);
      assert.equal(f.gate.snapshot().armed, false);
      if (operation === 'deadline') assert.match(f.gate.snapshot().failure, /deadline exceeded/);
      assert.throws(() => f.gate.release(), /No healthy held handshake/);
      assert.throws(() => f.gate.arm(), /cannot be armed/);
      const later = routeFixture();
      await f.gate.handle(later.route);
      assert.deepEqual(later.calls, ['close']);
      f.gate.dispose();
    });
  }
}

test('disposing immediately after release prevents the pending route from connecting', async () => {
  const { gate } = gateFixture();
  const { route, calls } = routeFixture();
  gate.arm();
  const pending = gate.handle(route);
  gate.release();
  gate.dispose();
  await pending;
  assert.deepEqual(calls, ['close']);
});

test('an unexpected concurrent replacement handshake fails closed for both routes', async () => {
  const { gate, timers } = gateFixture();
  const first = routeFixture();
  const second = routeFixture();
  gate.arm();
  const pending = gate.handle(first.route);
  await gate.handle(second.route);
  await pending;
  assert.deepEqual(first.calls, ['close']);
  assert.deepEqual(second.calls, ['close']);
  assert.equal(timers.size, 0);
  assert.ok(gate.snapshot().failure);
  assert.throws(() => gate.release(), /No healthy held handshake/);
  gate.dispose();
});
