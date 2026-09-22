import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

const require = createRequire(import.meta.url);
const { installRelayPolicy, selectedRelayPaths, waitForRelayPaths } = require('../../web/e2e/turn-relay.cjs');

test('relay policy keeps the native constructor, credentials and other configuration intact', () => {
  let received;
  class NativePeer {
    constructor(...args) { received = args; }
  }
  const window = { RTCPeerConnection: NativePeer };
  runInNewContext(`(${installRelayPolicy})()`, { window });
  const configuration = { iceTransportPolicy: 'all', iceServers: [{ urls: 'turn:fixture.invalid', username: 'test', credential: 'private' }] };
  const extra = {};
  const peer = new window.RTCPeerConnection(configuration, extra);
  assert.ok(peer instanceof NativePeer);
  assert.equal(received[0].iceTransportPolicy, 'relay');
  assert.equal(received[0].iceServers, configuration.iceServers);
  assert.equal(received[1], extra);
  assert.equal(configuration.iceTransportPolicy, 'all', 'the caller retains its original configuration');
});

function fixture({ selectedTransport = true } = {}) {
  const entries = [
    { type: 'candidate-pair', id: 'selected', state: 'succeeded', nominated: true, localCandidateId: 'local', bytesSent: 240, bytesReceived: 360 },
    { type: 'local-candidate', id: 'local', candidateType: 'relay', address: '192.0.2.9', usernameFragment: 'private' },
    { type: 'candidate-pair', id: 'unused', state: 'succeeded', nominated: false, localCandidateId: 'host', bytesSent: 1, bytesReceived: 2 },
    { type: 'local-candidate', id: 'host', candidateType: 'host' },
  ];
  if (selectedTransport) entries.push({ type: 'transport', id: 'transport', selectedCandidatePairId: 'selected' });
  const peer = {
    connectionState: 'connected', remoteDescription: {},
    getConfiguration: () => ({ iceTransportPolicy: 'relay', iceServers: [{ credential: 'private' }] }),
    getStats: async () => new Map(entries.map(entry => [entry.id, entry])),
  };
  const window = { __communityPeers: [peer, { connectionState: 'closed' }, { connectionState: 'new' }] };
  return { entries, peer, window, run: () => runInNewContext(`(${selectedRelayPaths})()`, { window }) };
}

for (const selectedTransport of [true, false]) {
  test(`native relay check follows ${selectedTransport ? 'transport selection' : 'the unique nominated fallback'} without leaking credentials`, async () => {
    const f = fixture({ selectedTransport });
    assert.deepEqual(JSON.parse(JSON.stringify(await f.run())), [{ candidateId: 'local', candidateType: 'relay', bytesSent: 240, bytesReceived: 360 }]);
  });
}

for (const mutation of ['host', 'absent-pair', 'absent-candidate', 'checking', 'not-nominated', 'disconnected', 'policy', 'empty', 'ambiguous']) {
  test(`native relay check rejects ${mutation} even if unused relay candidates exist`, async () => {
    const f = fixture({ selectedTransport: mutation !== 'ambiguous' });
    switch (mutation) {
      case 'host': f.entries[0].localCandidateId = 'host'; break;
      case 'absent-pair': f.entries.pop().selectedCandidatePairId = 'missing'; f.entries.shift(); break;
      case 'absent-candidate': f.entries[0].localCandidateId = 'missing'; break;
      case 'checking': f.entries[0].state = 'in-progress'; break;
      case 'not-nominated': f.entries[0].nominated = false; break;
      case 'disconnected': f.peer.connectionState = 'disconnected'; break;
      case 'policy': f.peer.getConfiguration = () => ({ iceTransportPolicy: 'all' }); break;
      case 'empty': f.window.__communityPeers = []; break;
      case 'ambiguous': f.entries[2].nominated = true; break;
      default: assert.fail('Unknown fixture mutation');
    }
    await assert.rejects(f.run());
  });
}

for (const counter of ['bytesSent', 'bytesReceived']) {
  test(`relay proof requires a real positive ${counter} counter`, async () => {
    for (const value of [undefined, null, 0, -1, 0.1, Infinity, '12']) {
      const f = fixture();
      f.entries[0][counter] = value;
      await assert.rejects(f.run(), /no bidirectional traffic/);
    }
  });
}

test('relay startup awaits nomination and rejects retained allocations until both paths change', async () => {
  let elapsed = 0, calls = 0;
  const wait = runInNewContext(`(${waitForRelayPaths})`, { performance: { now: () => elapsed }, selectedRelayPaths });
  const page = {
    async evaluate(callback) {
      assert.equal(callback, selectedRelayPaths);
      await Promise.resolve();
      calls++;
      if (calls === 1) throw new Error('Nomination pending');
      return [{ candidateId: 'new-send' }, { candidateId: calls === 2 ? 'old-recv' : 'new-recv' }];
    },
    async waitForTimeout(milliseconds) { elapsed += milliseconds; },
  };
  const paths = await wait(page, [{ candidateId: 'old-send' }, { candidateId: 'old-recv' }]);
  assert.equal(calls, 3);
  assert.equal(elapsed, 400);
  assert.equal(paths[1].candidateId, 'new-recv');
});

for (const unavailable of [true, false]) {
  test(`relay startup fails within a bound when allocations stay ${unavailable ? 'unavailable' : 'unchanged'}`, async () => {
    let elapsed = 0;
    const wait = runInNewContext(`(${waitForRelayPaths})`, { performance: { now: () => elapsed }, selectedRelayPaths });
    const page = {
      async evaluate() {
        if (unavailable) throw new Error('Relay unavailable');
        return [{ candidateId: 'old' }];
      },
      async waitForTimeout(milliseconds) { elapsed += milliseconds; },
    };
    await assert.rejects(wait(page, [{ candidateId: 'old' }]), /TURN relay paths did not become ready/);
    assert.equal(elapsed, 10000);
  });
}
