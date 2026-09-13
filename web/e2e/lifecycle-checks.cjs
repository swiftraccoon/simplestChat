const assert = require('node:assert/strict');

function configuration(env = process.env) {
  assert.equal(env.LIFECYCLE_E2E, '1', 'Set LIFECYCLE_E2E=1');
  assert.equal(env.DISPOSABLE_TEST_DATABASE, '1', 'Use owned disposable services');
  const base = new URL(env.BASE_URL || 'http://127.0.0.1:3119');
  assert.ok(
    ['http:', 'https:'].includes(base.protocol) &&
      ['127.0.0.1', 'localhost', '[::1]'].includes(base.hostname) &&
      !base.username &&
      !base.password &&
      !base.search &&
      !base.hash &&
      base.pathname === '/',
    'Lifecycle tests require a credential-free loopback origin',
  );
  assert.match(env.TEST_METRICS_TOKEN || '', /^[a-f0-9]{64}$/, 'Use the lifecycle server helper');
  assert.match(env.TEST_SERVER_PID || '', /^[1-9][0-9]*$/, 'Owned server PID is required');
  assert.ok(
    Number.isSafeInteger(Number(env.TEST_SERVER_PID)) &&
      Number(env.TEST_SERVER_PID) > 1 &&
      Number(env.TEST_SERVER_PID) <= 2147483647,
    'Invalid owned server PID',
  );
  const integer = (name, fallback, minimum, maximum) => {
    const value = env[name] ?? String(fallback);
    assert.ok(/^[1-9][0-9]*$/.test(value), `${name} must be an integer`);
    const number = Number(value);
    assert.ok(number >= minimum && number <= maximum, `${name} outside bounded workload range`);
    return number;
  };
  return {
    base: base.origin,
    cycles: integer('LIFECYCLE_CYCLES', 6, 3, 10),
    mediaSeconds: integer('LIFECYCLE_MEDIA_SECONDS', 30, 30, 120),
    serverPid: Number(env.TEST_SERVER_PID),
  };
}

/** Read only required numeric gauges; missing/incomplete snapshots never mean zero. */
function serverCounts(text) {
  assert.ok(typeof text === 'string' && text.length <= 131072, 'Invalid metrics response size');
  const read = (name) => {
    const lines = text.split('\n').filter((line) => new RegExp(`^${name}(?:[ \\t{]|$)`).test(line));
    assert.equal(lines.length, 1, `Missing or duplicate ${name}`);
    const match = new RegExp(`^${name} ([0-9]+)$`).exec(lines[0]);
    assert.ok(match, `Invalid ${name}`);
    const value = Number(match[1]);
    assert.ok(Number.isSafeInteger(value), `Invalid ${name}`);
    return value;
  };
  assert.equal(
    read('simplestchat_participants_snapshot_complete'),
    1,
    'Participant snapshot incomplete',
  );
  return {
    rooms: read('simplestchat_rooms_active'),
    participants: read('simplestchat_participants_active'),
    connections: read('simplestchat_connections_active'),
  };
}

/** Require progress on the same native stream, not a counter reset/new SSRC. */
function mediaProgress(before, after) {
  const flatten = (sample) => {
    assert.ok(Array.isArray(sample?.peers) && sample.peers.length <= 32, 'Invalid peer sample');
    const peers = new Set();
    return sample.peers.flatMap((peer) => {
      assert.ok(
        Number.isSafeInteger(peer.peer) && peer.peer > 0 && !peers.has(peer.peer),
        'Invalid or duplicate peer identity',
      );
      peers.add(peer.peer);
      assert.ok(Array.isArray(peer.inbound) && peer.inbound.length <= 64, 'Invalid inbound sample');
      const streams = new Set();
      return peer.inbound.map((stream) => {
        assert.ok(
          Number.isSafeInteger(stream.stream) && stream.stream > 0 && !streams.has(stream.stream),
          'Invalid or duplicate stream identity',
        );
        streams.add(stream.stream);
        assert.ok(['video', 'audio'].includes(stream.kind), 'Invalid media kind');
        return { ...stream, key: `${peer.peer}:${stream.stream}` };
      });
    });
  };
  const previous = new Map(flatten(before).map((stream) => [stream.key, stream]));
  const deltas = flatten(after).flatMap((stream) => {
    const prior = previous.get(stream.key);
    if (!prior || stream.kind !== prior.kind) return [];
    const field = stream.kind === 'video' ? 'framesDecoded' : 'packetsReceived';
    const first = prior[field];
    const last = stream[field];
    if (!Number.isSafeInteger(first) || !Number.isSafeInteger(last) || first < 0 || last < first)
      return [];
    return [{ kind: stream.kind, delta: last - first }];
  });
  const videoFrames = deltas.filter((d) => d.kind === 'video').reduce((sum, d) => sum + d.delta, 0);
  const audioPackets = deltas
    .filter((d) => d.kind === 'audio')
    .reduce((sum, d) => sum + d.delta, 0);
  assert.ok(videoFrames > 0, 'No decoded video progress on a continuing native stream');
  assert.ok(audioPackets > 0, 'No audio packet progress on a continuing native stream');
  return { videoFrames, audioPackets };
}

function releasedResources(snapshot, expectedOpenSockets = 0) {
  assert.ok(expectedOpenSockets === 0 || expectedOpenSockets === 1);
  for (const field of [
    'openPeers',
    'liveLocalTracks',
    'pendingCaptures',
    'attachedMediaElements',
  ]) {
    assert.equal(snapshot[field], 0, `${field} must return to zero after explicit leave`);
  }
  assert.equal(
    snapshot.openSockets,
    expectedOpenSockets,
    'openSockets must return to the expected startup baseline',
  );
}

/**
 * Hold a real replacement handshake, not a protocol message or forged reply.
 * Playwright awaits the handler before opening the page-side WebSocket.
 * A bounded cancellation closes the owned route without connecting it later.
 */
function handshakeGate(milliseconds = 50000) {
  let armed = false;
  let held = false;
  let resolve;
  let timer;
  let failure = null;
  let disposed = false;
  let intercepted = 0;
  const finish = (connect) => {
    clearTimeout(timer);
    armed = false;
    held = false;
    resolve?.(connect);
    resolve = undefined;
  };
  return {
    arm() {
      assert.ok(!armed && !held && !disposed && !failure, 'Handshake gate cannot be armed');
      armed = true;
      timer = setTimeout(() => {
        failure = 'Owned replacement handshake deadline exceeded';
        finish(false);
      }, milliseconds);
    },
    async handle(route) {
      if (disposed || failure) {
        await route.close({ code: 1001 });
        return;
      }
      if (held) {
        failure = 'Unexpected additional replacement handshake';
        finish(false);
        await route.close({ code: 1001 });
        return;
      }
      if (!armed) {
        route.connectToServer();
        return;
      }
      armed = false;
      held = true;
      intercepted++;
      const connect = await new Promise((done) => {
        resolve = done;
      });
      if (connect && !disposed && !failure) route.connectToServer();
      else await route.close({ code: 1001 });
    },
    snapshot() {
      return { armed, held, intercepted, failure };
    },
    release() {
      assert.ok(held && !failure && !disposed, 'No healthy held handshake to release');
      finish(true);
    },
    dispose() {
      disposed = true;
      finish(false);
    },
  };
}

module.exports = { configuration, serverCounts, mediaProgress, releasedResources, handshakeGate };
