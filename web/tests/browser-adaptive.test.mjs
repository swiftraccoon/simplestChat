import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createRequire } from 'node:module';
import { Duplex, Writable } from 'node:stream';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const require = createRequire(import.meta.url);
const { run, main } = require('../e2e/browser-adaptive.cjs');
const { createChannel } = require('../e2e/browser-impairment-channel.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
const phases = [
  ['clear', {}, 'baseline'],
  ['apply', { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10' }, 'lossy'],
  ['apply', { IMPAIR_RATE_KBIT: '400', IMPAIR_DELAY_MS: '30' }, 'constrained'],
  ['apply', { IMPAIR_RATE_KBIT: '150', IMPAIR_DELAY_MS: '30' }, 'severe'],
  ['clear', {}, 'recovery'],
  [
    'apply',
    { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10', IMPAIR_DIRECTION: 'both' },
    'lossyJoin',
  ],
  ['clear', {}],
];
function fixture({ ackStatus = 0, holdAck = false, finalFailure } = {}) {
  const signals = new EventEmitter();
  const entered = Promise.withResolvers();
  const state = { closed: 0, killed: 0, frames: [], events: [] };
  const stream = new Duplex({
    read() {},
    write(bytes, _encoding, callback) {
      const request = JSON.parse(bytes.toString());
      state.frames.push(request);
      entered.resolve();
      callback();
      if (holdAck) return;
      queueMicrotask(() => {
        stream.push(
          `${JSON.stringify({ schemaVersion: 1, runId, sequence: request.sequence, passed: ackStatus === 0, status: ackStatus })}\n`,
        );
      });
    },
    destroy(error, callback) {
      state.events.push('control-closed');
      callback(error);
    },
  });
  const channel = createChannel({ runId, stream });
  const browser = {};
  const child = { exitCode: null, signalCode: null };
  const server = {
    process: () => child,
    wsEndpoint: () => 'private-owned-endpoint',
    async close() {
      state.closed++;
      state.events.push('browser-closed');
      child.exitCode = 0;
    },
    async kill() {
      state.killed++;
      child.signalCode = 'SIGKILL';
    },
  };
  return {
    state,
    stream,
    signals,
    entered,
    options: {
      env: {
        BROWSER_ADAPTIVE_RUN_ID: runId,
        E2E_ARTIFACTS: '/owned/private/adaptive',
        PATH: '/owned/tools',
        HOME: '/owned/home',
        TMPDIR: '/owned/tmp',
        PLAYWRIGHT_BROWSERS_PATH: '/owned/browser-cache',
      },
      signals,
      channel: {
        ...channel,
        async finish() {
          await channel.finish();
          if (finalFailure) throw finalFailure;
        },
      },
      playwright: {
        chromium: {
          async launchServer() {
            return server;
          },
          async connect() {
            return browser;
          },
        },
      },
    },
    assertClosed() {
      assert.equal(state.closed, 1);
      assert.equal(state.killed, 0);
      assert.equal(stream.closed, true);
      for (const event of ['SIGINT', 'SIGTERM']) assert.equal(signals.listenerCount(event), 0);
      for (const event of ['data', 'end', 'error', 'close'])
        assert.equal(stream.listenerCount(event), 0);
    },
  };
}
async function suite(runtime) {
  assert.equal(runtime.env.IMPAIRED_SILENT_AUDIO, '0');
  assert.equal(runtime.env.IMPAIR_SCRIPT, 'rust-owned');
  assert.equal(
    runtime.env.IMPAIRED_PROFILES,
    'baseline,lossy,constrained,severe,recovery,lossyJoin',
  );
  const browser = await runtime.launchBrowser('chromium', { headless: true });
  for (const values of phases) await runtime.impair(...values);
  await runtime.closeBrowser(browser);
}

test('adaptive facade reuses the owner and requires all seven awaited acknowledgements', async () => {
  const f = fixture();
  const result = await run({ ...f.options, suite });
  assert.equal(result.status, 0);
  assert.equal(result.receipt.runId, runId);
  assert.equal(result.receipt.phasePassed, true);
  assert.equal(result.receipt.browserCleanupPassed, true);
  assert.deepEqual(
    f.state.frames.map(({ phase }) => phase),
    ['baseline', 'lossy', 'constrained', 'severe', 'recovery', 'lossyJoin', 'finish'],
  );
  assert.deepEqual(f.state.events, ['browser-closed', 'control-closed']);
  f.assertClosed();
});

test('known control failures survive browser and channel finalization without raw diagnostics', async () => {
  for (const code of [1, 42, 124, 130, 143, 255]) {
    const f = fixture({ ackStatus: code, finalFailure: new Error('PRIVATE_FINAL_FAILURE') });
    const result = await run({ ...f.options, suite });
    assert.equal(result.status, code);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.doesNotMatch(JSON.stringify(result.receipt), /PRIVATE_|endpoint/);
    f.assertClosed();
  }
});

test('an early finish or final channel failure cannot publish a positive phase result', async () => {
  for (const early of [false, true]) {
    const f = fixture({ finalFailure: new Error('PRIVATE_CHANNEL_CLOSE') });
    const result = await run({
      ...f.options,
      suite: early
        ? async (runtime) => {
            await runtime.launchBrowser('chromium', {});
            await runtime.impair('clear');
          }
        : suite,
    });
    assert.equal(result.status, 1);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    f.assertClosed();
  }
});

test('cooperative signals abort pending control while the same browser owner still closes', async () => {
  const f = fixture({ holdAck: true });
  const task = run({ ...f.options, suite });
  await f.entered.promise;
  f.signals.emit('SIGTERM');
  f.signals.emit('SIGINT');
  const result = await task;
  assert.equal(result.status, 143);
  assert.equal(result.receipt.cancelled, 143);
  assert.equal(result.receipt.phasePassed, false);
  assert.equal(result.receipt.browserCleanupPassed, true);
  f.assertClosed();
});

test('stdout receives only the final bounded owner receipt after browser and channel retirement', async () => {
  const f = fixture();
  let stdout = '';
  const output = new Writable({
    write(bytes, _encoding, callback) {
      f.assertClosed();
      stdout += bytes.toString();
      callback();
    },
  });
  assert.equal(await main({ ...f.options, suite, stdout: output }), 0);
  const receipt = JSON.parse(stdout);
  assert.equal(receipt.phasePassed, true);
  assert.equal(receipt.browserCleanupPassed, true);
  assert.ok(Buffer.byteLength(stdout) <= 16 * 1024);
  assert.deepEqual(Object.keys(receipt), [
    'schemaVersion',
    'runId',
    'completed',
    'phasePassed',
    'browserCleanupPassed',
    'cancelled',
    'browserCount',
    'records',
  ]);
});

test('adaptive CLI and failure diagnostics identify the correct journey', async () => {
  const f = fixture({ ackStatus: 42 });
  let stderr = '';
  const output = () =>
    new Writable({
      write(_bytes, _encoding, callback) {
        callback();
      },
    });
  const errors = new Writable({
    write(bytes, _encoding, callback) {
      stderr += bytes.toString();
      callback();
    },
  });
  assert.equal(await main({ ...f.options, suite, stdout: output(), stderr: errors }), 42);
  assert.equal(stderr, 'Browser adaptive failed.\n');
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL('../e2e/browser-adaptive.cjs', import.meta.url)), 'unexpected'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.equal(result.status, 2);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'Browser adaptive arguments are invalid.\n');
  f.assertClosed();
});
