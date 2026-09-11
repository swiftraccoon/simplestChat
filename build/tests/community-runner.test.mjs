import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const runner = path.join(root, 'web/e2e/community.cjs');

test('community steps distinguish successful checks, unsupported simulations, and failures', async () => {
  // Exercise the actual reporter without browser or database side effects.
  const source = await readFile(runner, 'utf8');
  const start = source.indexOf('async function step(');
  const end = source.indexOf('\nasync function client(', start);
  assert.ok(start >= 0 && end > start, 'step reporter must remain available');
  const report = { steps: [] }, saved = [], messages = [];
  const step = runInNewContext(`let activeStep; ${source.slice(start, end)}; step`, {
    report,
    saveReport() { saved.push(JSON.parse(JSON.stringify(report))); },
    console: { log(message) { messages.push(message); } },
  });
  await step('native check', async () => {});
  await step('unsupported simulation', async () => ({ skip: 'No synthetic event delivery' }));
  await assert.rejects(step('broken check', async () => { throw new Error('owned failure'); }), /owned failure/);
  assert.deepEqual(JSON.parse(JSON.stringify(report.steps)), [
    { name: 'native check', passed: true },
    { name: 'unsupported simulation', passed: false, skipped: true, reason: 'No synthetic event delivery' },
    { name: 'broken check', passed: false },
  ]);
  assert.equal(saved.length, 5, 'started checks and their outcomes must be persisted');
  assert.deepEqual(messages, ['PASS native check', 'SKIP unsupported simulation: No synthetic event delivery']);
});

async function fixture(t, toolingSource) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-browser-runner.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const tooling = path.join(directory, 'fake tooling');
  await mkdir(tooling);
  await writeFile(path.join(tooling, 'package.json'), JSON.stringify({ version: '0.0.0-fixture', main: 'index.cjs' }));
  await writeFile(path.join(tooling, 'index.cjs'), toolingSource ?? `module.exports = {
    chromium: { async launch() { throw new Error('owned fixture launch failure'); } },
    firefox: { async launch() { throw new Error('owned fixture launch failure'); } },
  };\n`);
  const artifacts = path.join(directory, 'artifacts');
  const env = {
    PATH: process.env.PATH,
    COMMUNITY_E2E: '1',
    BASE_URL: 'http://127.0.0.1:3119',
    PLAYWRIGHT_MODULE: tooling,
    E2E_ARTIFACTS: artifacts,
  };
  return {
    artifacts,
    run: overrides => spawnSync(process.execPath, [runner], {
      cwd: root, env: { ...env, ...overrides }, encoding: 'utf8', timeout: 10_000,
    }),
  };
}

function pageFailureTooling(diagnosticsFail = false) {
  return `
    const { runInNewContext } = require('node:vm');
    const peer = {
      connectionState: 'new', iceConnectionState: 'new', signalingState: 'stable', iceGatheringState: 'complete',
      localDescription: null,
      remoteDescription: { type: 'offer', sdp: [
        'v=0', 'a=ice-pwd:OWNED_ICE_SECRET', 'm=video 9 UDP/TLS/RTP/SAVPF 96',
        'a=mid:0', 'a=sendonly', 'a=rtpmap:96 VP8/90000',
        'a=candidate:PRIVATE_FOUNDATION 1 udp 1234 192.0.2.73 41010 typ host',
      ].join('\\r\\n') },
      getTransceivers() { return [{ mid: '0', direction: 'recvonly', currentDirection: null, stopped: false,
        receiver: { track: { kind: 'video', enabled: true, muted: true, readyState: 'live', id: 'OWNED_TRACK_SECRET' } },
      }]; },
      async getStats() { return new Map([['T01', { id: 'T01', type: 'transport', iceState: 'new', dtlsState: 'new', bytesReceived: 0, bytesSent: 0 }]]); },
    };
    const page = {
      setDefaultTimeout() {}, on() {}, async addInitScript() {},
      async goto() { throw new Error('owned fixture navigation failure'); },
      async evaluate(fn) {
        if (${diagnosticsFail}) throw new Error('owned fixture diagnostics failure');
        return runInNewContext('(' + fn.toString() + ')()', { window: { __communityPeers: [peer] }, setTimeout, clearTimeout });
      },
      locator() { return { async evaluateAll() {}, async isVisible() { return false; } }; },
      async screenshot() {},
    };
    module.exports = { chromium: { async launch() { return {
      version() { return 'fixture-browser'; },
      async newContext() { return { async newPage() { return page; } }; },
      async close() {},
    }; } } };
  `;
}

test('community runner collects sanitized peer state on failure without replacing the failed outcome', async t => {
  const { artifacts, run } = await fixture(t, pageFailureTooling());
  const result = run({});
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(await readFile(path.join(artifacts, 'community-results.json'), 'utf8'));
  assert.equal(report.passed, false);
  assert.equal(report.complete, false);
  assert.equal(report.error, 'owned fixture navigation failure');
  assert.equal(report.diagnostics.length, 1);
  const peer = report.diagnostics[0].media[0];
  assert.equal(peer.iceGatheringState, 'complete');
  assert.equal(peer.descriptions.remote.media[0].direction, 'sendonly');
  assert.equal(peer.descriptions.remote.media[0].candidates.count, 1);
  assert.equal(peer.transceivers[0].currentDirection, null);
  assert.equal(peer.stats[0].bytesReceived, 0);
  assert.doesNotMatch(JSON.stringify(report.diagnostics), /OWNED_ICE_SECRET|OWNED_TRACK_SECRET|PRIVATE_FOUNDATION|192\.0\.2\.73/);
});

test('community runner preserves the original failure when collecting peer diagnostics fails', async t => {
  const { artifacts, run } = await fixture(t, pageFailureTooling(true));
  const result = run({});
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(await readFile(path.join(artifacts, 'community-results.json'), 'utf8'));
  assert.equal(report.passed, false);
  assert.equal(report.error, 'owned fixture navigation failure');
  assert.deepEqual(report.diagnostics[0].media, { error: 'owned fixture diagnostics failure' });
});

test('community peer snapshots remain failure-only', async () => {
  const source = await readFile(runner, 'utf8');
  const failureStart = source.indexOf('report.failedStep = activeStep;');
  const cleanupStart = source.indexOf('\n  } finally {', failureStart);
  const call = 'item.page.evaluate(collectPeerDiagnostics)';
  assert.ok(failureStart >= 0 && cleanupStart > failureStart);
  assert.equal(source.split(call).length - 1, 1);
  assert.ok(source.slice(failureStart, cleanupStart).includes(call));
});

test('community runner refuses missing disposable-server opt-in before launching', async t => {
  const { artifacts, run } = await fixture(t);
  const result = run({ COMMUNITY_E2E: '' });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Set COMMUNITY_E2E=1/);
  await assert.rejects(readFile(path.join(artifacts, 'community-results.json')), { code: 'ENOENT' });
});

test('community runner rejects unknown engines without silently testing Chromium', async t => {
  const { artifacts, run } = await fixture(t);
  const result = run({ E2E_BROWSER: 'safari' });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unsupported E2E browser/);
  await assert.rejects(readFile(path.join(artifacts, 'community-results.json')), { code: 'ENOENT' });
});

for (const browser of ['chromium', 'firefox']) {
  test(`community runner preserves a failed ${browser} launch report and exits nonzero`, async t => {
    const { artifacts, run } = await fixture(t);
    const result = run({ E2E_BROWSER: browser });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /owned fixture launch failure/);
    const report = JSON.parse(await readFile(path.join(artifacts, 'community-results.json'), 'utf8'));
    assert.equal(report.browser, browser);
    assert.equal(report.browserVersion, null);
    assert.equal(report.playwrightVersion, '0.0.0-fixture');
    assert.equal(report.complete, false);
    assert.equal(report.passed, false);
    assert.equal(report.failedStep, 'launch');
    assert.equal(report.error, 'owned fixture launch failure');
    assert.deepEqual(report.steps, []);
    assert.ok(Date.parse(report.finishedAt) >= Date.parse(report.startedAt));
  });
}
