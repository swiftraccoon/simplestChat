import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

// Exercise the actual browser checks without launching the runner or
// substituting a second implementation of its media preservation rules.
const source = await readFile(new URL('../../web/e2e/community.cjs', import.meta.url), 'utf8');
const start = source.indexOf('async function reconnectMediaIdentity(');
const end = source.indexOf('\nasync function signalingReconnect(', start);
assert.ok(start >= 0 && end > start, 'reconnect identity helper must remain available');
function helpers(clock = performance) {
  return runInNewContext(
    `${source.slice(start, end)}; ({ reconnectMediaIdentity, mediaProgressDelta, waitForMediaStart, advancingReconnectMedia })`,
    { performance: clock },
    { timeout: 1000 },
  );
}
const { reconnectMediaIdentity, mediaProgressDelta, advancingReconnectMedia } = helpers();

test('initial media statistics wait awaits asynchronous samples and retries missing or unready counters', async () => {
  let elapsed = 0;
  let calls = 0;
  const { waitForMediaStart } = helpers({ now: () => elapsed });
  const state = {
    async sample() {
      await Promise.resolve();
      calls++;
      if (calls === 1) throw new Error('Fresh receiver has no statistics yet');
      return { audioPackets: calls > 2 ? 10 : 0, audioSamplesTime: 0.6, audioEnergy: 0.2 };
    },
  };
  await waitForMediaStart(
    { async waitForTimeout(milliseconds) { elapsed += milliseconds; } },
    { async evaluate(callback) { return callback(state); } },
  );
  assert.equal(calls, 3);
  assert.equal(elapsed, 400);
});

for (const missing of [false, true]) {
  test(`initial media statistics wait fails within its deadline when counters are ${missing ? 'missing' : 'unready'}`, async () => {
    let elapsed = 0;
    const { waitForMediaStart } = helpers({ now: () => elapsed });
    const failure = new Error('Statistics unavailable');
    await assert.rejects(waitForMediaStart(
      { async waitForTimeout(milliseconds) { elapsed += milliseconds; } },
      { async evaluate() {
        if (missing) throw failure;
        return { audioPackets: 10, audioSamplesTime: 0, audioEnergy: 0 };
      } },
    ), error => {
      assert.match(error.message, /did not expose decoded audio statistics/);
      assert.equal(error.cause, missing ? failure : undefined);
      return true;
    });
    assert.equal(elapsed, 10000);
  });
}

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
  const receiveAudio = { id: 'owned-audio', kind: 'audio', readyState: 'live' };
  const sending = peer('connected', [captureVideo, captureAudio]);
  const receiving = peer('connected', [], [receiveVideo, receiveAudio]);
  const unused = peer('new');
  const historical = peer('closed', [null], [{ kind: 'video', readyState: 'ended' }]);
  const window = {
    __communityPeers: [sending, receiving, unused, historical],
    __communityCaptureRequests: 2,
  };
  const localTracks = [captureVideo, captureAudio];
  const audio = {
    paused: false, muted: false, volume: 1, readyState: 4, currentTime: 36141.478,
    srcObject: { getAudioTracks: () => [receiveAudio] },
  };
  const document = {
    querySelector(selector) {
      assert.equal(selector, '#local-tile video');
      return { srcObject: { getTracks: () => [...localTracks] } };
    },
    querySelectorAll(selector) {
      assert.equal(selector, '.video-tile:not(.local) audio');
      return [audio];
    },
  };
  const page = {
    async evaluateHandle(callback) {
      return runInNewContext(`(${callback.toString()})()`, { window, document, performance }, { timeout: 1000 });
    },
  };
  return {
    window, sending, receiving, unused, historical, localTracks, audio,
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

function samplingFixture() {
  const f = fixture();
  const audioStats = {
    id: 'owned-inbound', type: 'inbound-rtp', kind: 'audio', trackIdentifier: f.receiveAudio.id,
    packetsReceived: 50, totalSamplesDuration: 1.29, totalAudioEnergy: 0.23, playoutId: 'owned-playout',
  };
  const playoutStats = { id: 'owned-playout', type: 'media-playout', kind: 'audio', totalSamplesDuration: 1.32 };
  const stats = new Map([
    ['owned-inbound', audioStats],
    ['owned-playout', playoutStats],
    ['owned-video', { type: 'inbound-rtp', kind: 'video', framesDecoded: 5 }],
    ['other-audio', { ...audioStats, id: 'other-inbound', trackIdentifier: 'other-track', packetsReceived: 9000 }],
    ['other-playout', { ...playoutStats, id: 'other-playout', totalSamplesDuration: 9000 }],
  ]);
  f.sending.getStats = f.unused.getStats = async () => new Map();
  f.receiving.getStats = async () => stats;
  return { ...f, audioStats, playoutStats, stats };
}

test('reconnect audio sampling follows the playing track and its linked playout report', async () => {
  const f = samplingFixture();
  const identity = await f.observe();
  const sample = await identity.sample();
  assert.equal(sample.framesDecoded, 5);
  assert.equal(sample.audioPackets, 50);
  assert.equal(sample.audioSamplesTime, 1.29);
  assert.equal(sample.audioEnergy, 0.23);
  assert.equal(sample.audioTime, 36141.478);
  assert.equal(sample.audioReportId, 'owned-inbound');
  assert.equal(sample.audioPlayoutId, 'owned-playout');
  assert.equal(sample.audioPlayoutTime, 1.32);
  delete f.audioStats.playoutId;
  const fallback = await identity.sample();
  assert.equal(fallback.audioPlayoutId, null);
  assert.equal(fallback.audioPlayoutTime, null);
});

test('reconnect audio sampling rejects inaudible playback and missing or invalid evidence', async () => {
  const cases = [
    ['paused element', f => { f.audio.paused = true; }, /audible owned/],
    ['muted element', f => { f.audio.muted = true; }, /audible owned/],
    ['silent element', f => { f.audio.volume = 0; }, /audible owned/],
    ['ended track', f => { f.receiveAudio.readyState = 'ended'; }, /audible owned/],
    ['wrong track', f => { f.audioStats.trackIdentifier = 'unrelated'; }, /one matching/],
    ['missing linked playout', f => { f.stats.delete('owned-playout'); }, /Linked native audio/],
    ['wrong linked playout type', f => { f.playoutStats.type = 'codec'; }, /Linked native audio/],
    ['missing samples', f => { delete f.audioStats.totalSamplesDuration; }, /counters unavailable/],
    ['invalid energy', f => { f.audioStats.totalAudioEnergy = NaN; }, /counters unavailable/],
    ['negative packets', f => { f.audioStats.packetsReceived = -1; }, /counters unavailable/],
    ['invalid playout', f => { f.playoutStats.totalSamplesDuration = Infinity; }, /counters unavailable/],
  ];
  for (const [name, mutate, expected] of cases) {
    const f = samplingFixture();
    const identity = await f.observe();
    mutate(f);
    await assert.rejects(identity.sample(), expected, name);
  }
});

function progressSamples() {
  return [
    { framesDecoded: 5, audioPackets: 50, audioReportId: 'owned-inbound', audioSamplesTime: 1.29,
      audioEnergy: 0.23, audioTime: 36141.478, audioPlayoutId: 'owned-playout', audioPlayoutTime: 1.32, sampledAtMs: 0 },
    { framesDecoded: 9, audioPackets: 60, audioReportId: 'owned-inbound', audioSamplesTime: 1.49,
      audioEnergy: 0.28, audioTime: 36141.478, audioPlayoutId: 'owned-playout', audioPlayoutTime: 1.52, sampledAtMs: 200 },
  ];
}

async function observeProgress(samples, ensureGap) {
  let index = 0;
  return advancingReconnectMedia(
    { async waitForTimeout(milliseconds) { assert.equal(milliseconds, 200); } },
    { async evaluate(callback) { return callback({ sample: () => samples[index++] }); } },
    0,
    ensureGap,
  );
}

test('reconnect progress accepts decoded signal and native playout with a frozen HTML clock', async () => {
  let gapChecks = 0;
  const result = await observeProgress(progressSamples(), async () => { gapChecks++; });
  assert.equal(result.audioSeconds, 0);
  assert.ok(result.audioSamplesSeconds > 0.05);
  assert.ok(result.audioEnergy > 0);
  assert.ok(result.audioPlayoutSeconds > 0.05);
  assert.equal(gapChecks, 4, 'both samples must be bracketed by outage observations');
});

test('reconnect progress rejects real stalls even when the HTML clock advances', async () => {
  for (const field of ['framesDecoded', 'audioPackets', 'audioSamplesTime', 'audioEnergy', 'audioPlayoutTime']) {
    const samples = progressSamples();
    samples[1].audioTime += 0.2;
    samples[1][field] = samples[0][field];
    await assert.rejects(observeProgress(samples), error => {
      assert.match(error.message, /No measured decoded video and audible audio progress/);
      assert.equal(error.cause.mediaProgress.length, 2);
      return true;
    }, field);
  }
});

test('reconnect progress requires an advancing HTML clock when native playout is unsupported', async () => {
  const samples = progressSamples().map(sample => ({ ...sample, audioPlayoutId: null, audioPlayoutTime: null }));
  await assert.rejects(observeProgress(samples), /No measured decoded video and audible audio progress/);
  samples[1].audioTime += 0.2;
  const result = await observeProgress(samples);
  assert.ok(result.audioSeconds > 0.05);
  assert.equal(result.audioPlayoutSeconds, null);
});

test('reconnect progress rejects replacement counters instead of treating them as advancement', () => {
  for (const field of ['audioReportId', 'audioPlayoutId']) {
    const [before, after] = progressSamples();
    after[field] = 'replacement';
    assert.throws(() => mediaProgressDelta(before, after), /statistics changed identity/);
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
