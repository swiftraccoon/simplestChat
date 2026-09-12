import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

// Exercise the actual browser identity check without launching the runner or
// substituting a second implementation of its peer/track preservation rules.
const source = await readFile(new URL('../../web/e2e/community.cjs', import.meta.url), 'utf8');
const start = source.indexOf('async function reconnectMediaIdentity(');
const end = source.indexOf('\nfunction mediaProgressDelta(', start);
assert.ok(start >= 0 && end > start, 'reconnect identity helper must remain available');
const reconnectMediaIdentity = runInNewContext(
  `${source.slice(start, end)}; reconnectMediaIdentity`,
  {},
  { timeout: 1000 },
);

function peer(connectionState, senders = [], receivers = []) {
  return {
    connectionState,
    senders: senders.map(track => ({ track })),
    receivers: receivers.map(track => ({ track })),
    getSenders() { return this.senders; },
    getReceivers() { return this.receivers; },
    getStats() { throw new Error('Identity verification must not request media statistics'); },
  };
}

function fixture() {
  const captureVideo = { kind: 'video', readyState: 'live' };
  const captureAudio = { kind: 'audio', readyState: 'live' };
  const receiveVideo = { kind: 'video', readyState: 'live' };
  const receiveAudio = { kind: 'audio', readyState: 'live' };
  const sending = peer('connected', [captureVideo, captureAudio]);
  const receiving = peer('connected', [], [receiveVideo, receiveAudio]);
  const unused = peer('new');
  const historical = peer('closed', [null], [{ kind: 'video', readyState: 'ended' }]);
  const window = {
    __communityPeers: [sending, receiving, unused, historical],
    __communityCaptureRequests: 2,
  };
  const localTracks = [captureVideo, captureAudio];
  const document = {
    querySelector(selector) {
      assert.equal(selector, '#local-tile video');
      return { srcObject: { getTracks: () => [...localTracks] } };
    },
  };
  const page = {
    async evaluateHandle(callback) {
      return runInNewContext(`(${callback.toString()})()`, { window, document }, { timeout: 1000 });
    },
  };
  return {
    window, sending, receiving, unused, historical, localTracks,
    captureVideo, captureAudio, receiveVideo, receiveAudio,
    observe: () => reconnectMediaIdentity(page),
  };
}

test('reconnect identity rejects missing and invalid initial capture observations', async () => {
  const missing = fixture();
  delete missing.window.__communityCaptureRequests;
  await assert.rejects(missing.observe(), /Capture request observation unavailable/);
  for (const value of [undefined, null, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, '2', {}]) {
    const f = fixture();
    f.window.__communityCaptureRequests = value;
    await assert.rejects(f.observe(), /Capture request observation unavailable/);
  }
});

test('reconnect identity preserves stable native peers including unused New and historical closed directions', async () => {
  const f = fixture();
  const identity = await f.observe();
  for (let check = 0; check < 2; check++) {
    assert.deepEqual(JSON.parse(JSON.stringify(identity.verify())), {
      peers: 4, liveTracks: 4, captureRequests: 2,
    });
  }
  assert.equal(f.unused.connectionState, 'new');
  assert.equal(f.historical.connectionState, 'closed');
  assert.equal(f.historical.receivers[0].track.readyState, 'ended');
});

test('reconnect identity rejects changed native peers, tracks, and capture observations', async () => {
  const cases = [
    ['registry replacement', f => { f.window.__communityPeers = [...f.window.__communityPeers]; }, /replaced or added native peers/],
    ['new peer', f => { f.window.__communityPeers.push(peer('new')); }, /replaced or added native peers/],
    ['peer replacement', f => { f.window.__communityPeers[0] = { ...f.sending }; }, /replaced or added native peers/],
    ['peer removal', f => { f.window.__communityPeers.pop(); }, /replaced or added native peers/],
    ['peer reorder', f => { f.window.__communityPeers.reverse(); }, /replaced or added native peers/],
    ['closed active peer', f => { f.sending.connectionState = 'closed'; }, /closed or failed a retained native peer/],
    ['failed active peer', f => { f.receiving.connectionState = 'failed'; }, /closed or failed a retained native peer/],
    ['sender track replacement', f => { f.sending.senders[0].track = { ...f.captureVideo }; }, /replaced native media tracks/],
    ['receiver track replacement', f => { f.receiving.receivers[0].track = { ...f.receiveVideo }; }, /replaced native media tracks/],
    ['ended capture track', f => { f.captureAudio.readyState = 'ended'; }, /ended or replaced capture\/receive tracks/],
    ['ended receive track', f => { f.receiveAudio.readyState = 'ended'; }, /ended or replaced capture\/receive tracks/],
    ['local tile track replacement', f => { f.localTracks[0] = { ...f.captureVideo }; }, /ended or replaced capture\/receive tracks/],
    ['new capture request', f => { f.window.__communityCaptureRequests++; }, /requested new capture/],
    ['lost capture observation', f => { delete f.window.__communityCaptureRequests; }, /requested new capture/],
  ];
  for (const [name, mutate, expected] of cases) {
    const f = fixture();
    const identity = await f.observe();
    mutate(f);
    assert.throws(() => identity.verify(), expected, name);
  }
});
