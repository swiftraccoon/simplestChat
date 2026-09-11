import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import isolation from '../../web/e2e/ice-isolation.cjs';

const { setupReceiveOnlyPeer, receiveOnlyGatheringComplete } = isolation;
const root = fileURLToPath(new URL('../../', import.meta.url));
const runner = path.join(root, 'web/e2e/ice-isolation.cjs');
const plain = value => JSON.parse(JSON.stringify(value));

// Shared by isolated function tests and the fake Playwright process. No sockets,
// media devices or real browser APIs are used, including for timeout scenarios.
function peerEnvironment({ registerPeers = true, gathering = true, failOffer = false } = {}) {
  const calls = [], peers = [];
  const window = { __communityPeers: [], __communityPeerEvents: new WeakMap() };
  const forbidden = name => () => { calls.push(['forbidden', name]); throw new Error('Forbidden ' + name); };
  class Peer {
    connectionState = 'new'; iceConnectionState = 'new'; signalingState = 'stable'; iceGatheringState = 'new';
    localDescription = null; remoteDescription = null;
    currentLocalDescription = null; currentRemoteDescription = null;
    pendingLocalDescription = null; pendingRemoteDescription = null;
    transceivers = []; listeners = new Map(); closed = false;
    constructor(config) {
      this.index = peers.length; peers.push(this); calls.push(['construct', this.index, config]);
      if (registerPeers) window.__communityPeers.push(this);
    }
    addEventListener(type, listener) { this.listeners.set(type, listener); }
    addTransceiver(kind, options) {
      calls.push(['addTransceiver', this.index, kind, options]);
      let direction = options.direction;
      const item = { mid: '0', currentDirection: null, stopped: false,
        sender: { track: null }, receiver: { track: { kind, enabled: true, muted: true, readyState: 'live', id: 'PRIVATE_ISOLATION_TRACK', label: 'PRIVATE_ISOLATION_LABEL' } },
      };
      Object.defineProperty(item, 'direction', { enumerable: true, get: () => direction,
        set: value => { calls.push(['direction', this.index, value]); direction = value; } });
      this.transceivers.push(item); return item;
    }
    description(type) {
      return { type, sdp: ['v=0', 'a=group:BUNDLE 0', 'a=ice-ufrag:PRIVATE_ISOLATION_UFRAG',
        'a=ice-pwd:PRIVATE_ISOLATION_PASSWORD', 'a=fingerprint:sha-256 PRIVATE_ISOLATION_FINGERPRINT',
        'm=video 9 UDP/TLS/RTP/SAVPF 96', 'a=mid:0', 'a=rtpmap:96 VP8/90000',
        'a=' + this.transceivers[0].direction, 'a=msid:PRIVATE_ISOLATION_MSID PRIVATE_ISOLATION_TRACK'].join('\r\n') };
    }
    async createOffer() {
      calls.push(['createOffer', this.index]);
      if (failOffer) throw new Error('owned offer failure');
      return this.description('offer');
    }
    async createAnswer() { calls.push(['createAnswer', this.index]); return this.description('answer'); }
    async setRemoteDescription(description) {
      calls.push(['setRemoteDescription', this.index, description.type]); this.remoteDescription = description;
      this.addTransceiver('video', { direction: 'sendrecv' });
    }
    async setLocalDescription(description) {
      calls.push(['setLocalDescription', this.index, description.type]); this.localDescription = description;
      this.iceGatheringState = gathering ? 'complete' : 'gathering';
      if (gathering) {
        const candidate = 'candidate:PRIVATE_ISOLATION_FOUNDATION 1 UDP 1234 192.0.2.73 41010 typ host ufrag PRIVATE_ISOLATION_UFRAG';
        this.localDescription = { ...description, sdp: description.sdp + '\r\na=' + candidate };
        this.listeners.get('icecandidate')?.({ candidate: { candidate } });
        this.listeners.get('icecandidate')?.({ candidate: null });
      }
      this.listeners.get('icegatheringstatechange')?.({});
    }
    getTransceivers() { calls.push(['getTransceivers', this.index]); return this.transceivers; }
    async getStats() { return new Map([['T01', { type: 'transport', id: 'T01', iceState: 'new', bytesSent: 0, bytesReceived: 0 }]]); }
    close() { calls.push(['close', this.index]); this.closed = true; this.connectionState = 'closed'; this.signalingState = 'closed'; }
  }
  for (const name of ['addTrack', 'removeTrack', 'addIceCandidate', 'restartIce', 'setConfiguration']) Peer.prototype[name] = forbidden(name);
  window.RTCPeerConnection = Peer;
  const sandbox = { window, URL, performance: { now: () => 1000 }, setTimeout, clearTimeout,
    navigator: { mediaDevices: { getUserMedia: forbidden('getUserMedia'), getDisplayMedia: forbidden('getDisplayMedia') } },
    fetch: forbidden('fetch'), XMLHttpRequest: forbidden('XMLHttpRequest'),
  };
  Object.defineProperty(sandbox, 'RTCPeerConnection', { get: () => window.RTCPeerConnection });
  return { sandbox, window, calls, peers };
}

function setup(environment, role) {
  const sandbox = Object.defineProperty({ ...environment.sandbox, role }, 'RTCPeerConnection', {
    get: () => environment.window.RTCPeerConnection,
  });
  return runInNewContext(`(${setupReceiveOnlyPeer.toString()})(role)`, sandbox, { timeout: 1000 });
}

test('receive-only offerer configures only a video receiver and its local offer', async () => {
  const e = peerEnvironment();
  assert.equal(await setup(e, 'offerer'), 0);
  assert.equal(e.window.__iceIsolationPeer, e.peers[0]);
  assert.deepEqual(plain(e.calls), [
    ['construct', 0, { iceServers: [] }], ['addTransceiver', 0, 'video', { direction: 'recvonly' }],
    ['createOffer', 0], ['setLocalDescription', 0, 'offer'],
  ]);
  assert.equal(e.peers[0].closed, false);
});

test('receive-only answerer closes its inactive offer helper before configuring the receiver', async () => {
  const e = peerEnvironment();
  assert.equal(await setup(e, 'answerer'), 0);
  assert.equal(e.peers.length, 2);
  assert.deepEqual(plain(e.calls), [
    ['construct', 0, { iceServers: [] }], ['construct', 1, { iceServers: [] }],
    ['addTransceiver', 1, 'video', { direction: 'sendonly' }], ['createOffer', 1], ['close', 1],
    ['setRemoteDescription', 0, 'offer'], ['addTransceiver', 0, 'video', { direction: 'sendrecv' }],
    ['getTransceivers', 0], ['direction', 0, 'recvonly'], ['createAnswer', 0], ['setLocalDescription', 0, 'answer'],
  ]);
  assert.equal(e.peers[1].closed, true); assert.equal(e.peers[0].closed, false);
  assert.equal(e.peers[1].localDescription, null, 'the helper must never start gathering');
});

test('answerer offer-generation failure still closes the helper without activating either peer', async () => {
  const e = peerEnvironment({ failOffer: true });
  await assert.rejects(setup(e, 'answerer'), /owned offer failure/);
  assert.equal(e.peers[1].closed, true); assert.equal(e.peers[0].closed, false);
  assert.equal(e.calls.some(call => ['setLocalDescription', 'setRemoteDescription', 'forbidden'].includes(call[0])), false);
});

test('invalid isolation roles are rejected before constructing a peer', async () => {
  const e = peerEnvironment();
  await assert.rejects(setup(e, 'publisher'), /Unsupported isolation role/);
  assert.deepEqual(e.calls, []);
});

test('gathering completion requires both complete state and a nonempty candidate event on this peer', () => {
  const e = peerEnvironment(), value = { iceGatheringState: 'gathering' };
  const complete = runInNewContext(`(${receiveOnlyGatheringComplete.toString()})`, e.sandbox);
  assert.ok(!complete());
  e.window.__iceIsolationPeer = value;
  e.window.__communityPeerEvents.set(value, { events: [{ event: 'icecandidate', phase: 'candidate' }] });
  assert.equal(complete(), false);
  value.iceGatheringState = 'complete';
  assert.equal(complete(), true);
  for (const events of [[], [{ event: 'icecandidate', phase: 'complete' }], [{ event: 'icecandidate', phase: 'end-of-generation' }], [{ event: 'other', phase: 'candidate' }]]) {
    e.window.__communityPeerEvents.set(value, { events }); assert.equal(complete(), false);
  }
  e.window.__communityPeerEvents = new WeakMap();
  assert.equal(complete(), false);
});

async function runnerFixture(t, settings = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-ice-runner-fixture.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const tooling = path.join(directory, 'fake playwright'), artifacts = path.join(directory, 'artifacts');
  const eventsFile = path.join(directory, 'events.jsonl');
  await mkdir(tooling); await writeFile(eventsFile, '');
  await writeFile(path.join(tooling, 'package.json'), JSON.stringify({ version: '0.0.0-fixture', main: 'index.cjs' }));
  await writeFile(path.join(tooling, 'index.cjs'), `
    const assert = require('node:assert/strict');
    const fs = require('node:fs');
    const { runInNewContext } = require('node:vm');
    const createEnvironment = (${peerEnvironment.toString()});
    const settings = ${JSON.stringify(settings)};
    const log = (...entry) => fs.appendFileSync(${JSON.stringify(eventsFile)}, JSON.stringify(entry) + '\\n');
    let launched = 0;
    const engine = { async launch() {
      const index = launched++;
      log('launch', index);
      const e = createEnvironment({ registerPeers: false, gathering: !(settings.timeoutFirst && index === 0) });
      let routeHandler;
      const page = {
        async addInitScript(fn, argument) {
          e.sandbox.argument = argument;
          runInNewContext('(' + fn.toString() + ')(argument)', e.sandbox);
        },
        async goto(url) {
          assert.equal(new URL(url).hostname, '127.0.0.1');
          assert.ok(routeHandler, 'navigation must be intercepted before goto');
          let fulfilled = false;
          await routeHandler({ request: () => ({ url: () => url }),
            fulfill(response) { assert.equal(response.contentType, 'text/html'); assert.match(response.body, /<!doctype html>/i); fulfilled = true; },
            abort() { assert.fail('owned loopback document was aborted'); }, continue() { assert.fail('network escape'); },
          });
          assert.equal(fulfilled, true); log('staticDocument', index);
          let aborted = false;
          await routeHandler({ request: () => ({ url: () => 'https://not-contacted.example.test/' }),
            fulfill() { assert.fail('unrelated request must not receive the document'); },
            abort() { aborted = true; }, continue() { assert.fail('network escape'); },
          });
          assert.equal(aborted, true); log('blockedExternalRequest', index);
        },
        async evaluate(fn, argument) {
          if (fn.name === 'collectPeerDiagnostics') {
            log('snapshot', index);
            if (settings.diagnosticsFail && index === 0) throw new Error('PRIVATE_ISOLATION_DIAGNOSTICS');
          }
          e.sandbox.argument = argument;
          return runInNewContext('(' + fn.toString() + ')(argument)', e.sandbox);
        },
        async waitForFunction(fn, argument, options) {
          assert.equal(options.timeout, 15000);
          const complete = runInNewContext('(' + fn.toString() + ')()', e.sandbox);
          if (!complete) throw new Error('PRIVATE_ISOLATION_TIMEOUT');
          log('gathered', index);
        },
      };
      return {
        version() { return 'fixture-browser'; },
        async newContext() { log('context', index); return {
          async route(pattern, callback) { assert.equal(pattern, '**/*'); routeHandler = callback; },
          async newPage() { return page; },
          async close() { log('contextClosed', index); if (settings.cleanupFail && index === 0) throw new Error('PRIVATE_ISOLATION_CLEANUP'); },
        }; },
        async close() {
          log('browserClosed', index);
          assert.ok(e.peers.every(peer => peer.closed), 'page-owned peers must close before browser cleanup');
          assert.equal(e.calls.some(call => call[0] === 'forbidden'), false);
          log('peerCalls', index, e.calls);
        },
      };
    } };
    module.exports = { chromium: engine, firefox: engine };
  `);
  return {
    artifacts,
    run(overrides = {}) {
      const result = spawnSync(process.execPath, [runner], { cwd: root, timeout: 10000, encoding: 'utf8',
        env: { PATH: process.env.PATH, ICE_ISOLATION_E2E: '1', E2E_BROWSER: 'chromium', PLAYWRIGHT_MODULE: tooling, E2E_ARTIFACTS: artifacts, ...overrides },
      });
      assert.equal(result.signal, null, result.stderr); assert.equal(result.error, undefined);
      return result;
    },
    async report() { return JSON.parse(await readFile(path.join(artifacts, 'ice-isolation-results.json'), 'utf8')); },
    async events() { return (await readFile(eventsFile, 'utf8')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line)); },
  };
}

function assertPrivateReport(report) {
  assert.doesNotMatch(JSON.stringify(report), /PRIVATE_ISOLATION_|192\.0\.2\.73|candidate:/);
}

test('isolation CLI rejects absent opt-in and unknown engines without launching', async t => {
  const f = await runnerFixture(t);
  const missing = f.run({ ICE_ISOLATION_E2E: '' });
  assert.equal(missing.status, 1); assert.match(missing.stderr, /Set ICE_ISOLATION_E2E=1/);
  const unknown = f.run({ E2E_BROWSER: 'not-an-engine' });
  assert.equal(unknown.status, 1); assert.match(unknown.stderr, /ICE isolation runner failed/);
  assert.deepEqual(await f.events(), []);
  await assert.rejects(f.report(), { code: 'ENOENT' });
});

test('isolation CLI snapshots both successful roles privately and cleans fresh browsers', async t => {
  const f = await runnerFixture(t), result = f.run();
  assert.equal(result.status, 0, result.stderr);
  const report = await f.report();
  assert.equal(report.complete, true); assert.equal(report.passed, true);
  assert.equal(report.browser, 'chromium'); assert.equal(report.browserVersion, 'fixture-browser');
  assert.deepEqual(report.cases.map(value => [value.name, value.passed, value.peerIndex, value.stage]),
    [['offerer', true, 0, 'complete'], ['answerer', true, 0, 'complete']]);
  assert.ok(report.cases.every(value => value.media[0].iceGatheringState === 'complete'));
  assertPrivateReport(report);
  assert.equal((await stat(f.artifacts)).mode & 0o777, 0o700);
  assert.equal((await stat(path.join(f.artifacts, 'ice-isolation-results.json'))).mode & 0o777, 0o600);
  const events = await f.events();
  for (const name of ['launch', 'context', 'staticDocument', 'blockedExternalRequest', 'snapshot', 'contextClosed', 'browserClosed']) {
    assert.deepEqual(events.filter(event => event[0] === name).map(event => event[1]), [0, 1], name);
  }
});

test('isolation CLI preserves a timed-out first case, snapshots it and still runs the second case', async t => {
  const f = await runnerFixture(t, { timeoutFirst: true }), result = f.run({ E2E_BROWSER: 'firefox' });
  assert.equal(result.status, 1, result.stderr);
  const report = await f.report();
  assert.equal(report.complete, true); assert.equal(report.passed, false); assert.equal(report.browser, 'firefox');
  assert.equal(report.cases[0].stage, 'gathering'); assert.equal(report.cases[0].passed, false);
  assert.match(report.cases[0].error, /within 15000 ms/);
  assert.equal(report.cases[0].media[0].iceGatheringState, 'gathering');
  assert.equal(report.cases[1].passed, true); assertPrivateReport(report);
  assert.equal((await f.events()).filter(event => event[0] === 'browserClosed').length, 2);
});

test('snapshot failure cannot hide the original gathering failure or prevent later cleanup/cases', async t => {
  const f = await runnerFixture(t, { timeoutFirst: true, diagnosticsFail: true }), result = f.run();
  assert.equal(result.status, 1, result.stderr);
  const report = await f.report();
  assert.equal(report.passed, false); assert.match(report.cases[0].error, /within 15000 ms/);
  assert.equal(report.cases[0].diagnosticsError, 'Peer snapshot unavailable');
  assert.equal(report.cases[1].passed, true); assertPrivateReport(report);
  assert.equal((await f.events()).filter(event => event[0] === 'browserClosed').length, 2);
});

test('cleanup failure marks a gathered case failed while still closing the browser and running the next role', async t => {
  const f = await runnerFixture(t, { cleanupFail: true }), result = f.run();
  assert.equal(result.status, 1, result.stderr);
  const report = await f.report();
  assert.equal(report.complete, true); assert.equal(report.passed, false);
  assert.equal(report.cases[0].stage, 'complete'); assert.equal(report.cases[0].passed, false);
  assert.equal(report.cases[0].cleanupError, 'Browser cleanup failed');
  assert.equal(report.cases[1].passed, true); assertPrivateReport(report);
  assert.equal((await f.events()).filter(event => event[0] === 'browserClosed').length, 2);
});
