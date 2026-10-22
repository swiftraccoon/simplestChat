import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createRequire } from 'node:module';
import { Duplex, Writable } from 'node:stream';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const require = createRequire(import.meta.url);
const { run, main } = require('../e2e/browser-tcp.cjs');
const { createChannel } = require('../e2e/browser-impairment-channel.cjs');
const runId = '00000000-0000-4000-8000-000000000001';
const base = { BROWSER_TCP_RUN_ID: runId, E2E_ARTIFACTS: '/owned/private/tcp' };

function fixture({ status = 0, hold = false } = {}) {
  const signals = new EventEmitter();
  const entered = Promise.withResolvers();
  const events = [];
  const frames = [];
  const child = { exitCode: null, signalCode: null };
  const stream = new Duplex({
    read() {},
    write(bytes, _encoding, callback) {
      const frame = JSON.parse(bytes.toString());
      frames.push(frame);
      events.push(frame.phase);
      callback();
      entered.resolve();
      if (!hold)
        queueMicrotask(() =>
          stream.push(
            `${JSON.stringify({
              schemaVersion: 1,
              runId,
              sequence: frame.sequence,
              passed: status === 0,
              status,
            })}\n`,
          ),
        );
    },
    destroy(error, callback) {
      events.push('control-closed');
      callback(error);
    },
  });
  const browser = {};
  const options = {
    env: base,
    signals,
    channel: createChannel({ runId, mode: 'tcp', stream }),
    playwright: {
      chromium: {
        async launchServer() {
          return {
            process: () => child,
            wsEndpoint: () => 'private-endpoint',
            async close() {
              events.push('browser-closed');
              child.exitCode = 0;
            },
            async kill() {
              events.push('browser-killed');
              child.signalCode = 'SIGKILL';
            },
          };
        },
        async connect() {
          return browser;
        },
      },
    },
  };
  return {
    options,
    entered,
    signals,
    frames,
    events,
    assertClosed() {
      assert.equal(stream.closed, true);
      assert.equal(events.filter((value) => value === 'browser-closed').length, 1);
      assert.ok(!events.includes('browser-killed'));
      for (const name of ['data', 'end', 'error', 'close'])
        assert.equal(stream.listenerCount(name), 0);
      for (const name of ['SIGINT', 'SIGTERM']) assert.equal(signals.listenerCount(name), 0);
    },
  };
}

async function suite(runtime) {
  assert.equal(runtime.env.IMPAIRED_BLOCK_UDP, '1');
  assert.equal(runtime.env.WEBRTC_SERVER_TCP, 'true');
  assert.equal(runtime.env.IMPAIRED_PROFILES, 'baseline');
  assert.equal(runtime.env.IMPAIRED_SILENT_AUDIO, '0');
  assert.equal(runtime.env.IMPAIR_SCRIPT, 'rust-owned');
  const browser = await runtime.launchBrowser('chromium', {});
  await runtime.impair('block-udp');
  await runtime.impair('clear', {}, 'baseline');
  await runtime.impair('clear');
  await runtime.impair('unblock-udp');
  await runtime.closeBrowser(browser);
}

test('TCP facade waits for all four acknowledgments and unblocks before browser retirement', async () => {
  const f = fixture();
  const result = await run({ ...f.options, suite });
  assert.equal(result.status, 0);
  assert.equal(result.receipt.runId, runId);
  assert.equal(result.receipt.phasePassed, true);
  assert.equal(result.receipt.browserCleanupPassed, true);
  assert.deepEqual(f.events, [
    'block',
    'baseline',
    'finish',
    'unblock',
    'browser-closed',
    'control-closed',
  ]);
  assert.ok(f.frames.every((frame) => frame.profile === null));
  f.assertClosed();
});

test('a held block is cancelled while the same browser still retires exactly once', async () => {
  const f = fixture({ hold: true });
  const task = run({ ...f.options, suite });
  await f.entered.promise;
  f.signals.emit('SIGTERM');
  f.signals.emit('SIGINT');
  const result = await task;
  assert.equal(result.status, 143);
  assert.equal(result.receipt.cancelled, 143);
  assert.equal(result.receipt.phasePassed, false);
  assert.equal(result.receipt.browserCleanupPassed, true);
  assert.deepEqual(f.events, ['block', 'control-closed', 'browser-closed']);
  f.assertClosed();
});

test('known control failures remain primary over later channel finalization', async () => {
  for (const status of [1, 42, 124, 130, 143, 255]) {
    const f = fixture({ status });
    const result = await run({ ...f.options, suite });
    assert.equal(result.status, status);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    assert.doesNotMatch(JSON.stringify(result.receipt), /private-endpoint|PRIVATE/);
    f.assertClosed();
  }
});

test('incomplete or early cleanup cannot manufacture a positive TCP phase receipt', async () => {
  for (const unblock of [false, true]) {
    const f = fixture();
    const result = await run({
      ...f.options,
      async suite(runtime) {
        await runtime.launchBrowser('chromium', {});
        await runtime.impair('clear');
        if (unblock) await runtime.impair('unblock-udp');
      },
    });
    assert.equal(result.status, 1);
    assert.equal(result.receipt.phasePassed, false);
    assert.equal(result.receipt.browserCleanupPassed, true);
    f.assertClosed();
  }
});

test('conflicting TCP settings refuse before allocating a browser or control owner', async () => {
  for (const patch of [
    { IMPAIRED_BLOCK_UDP: '0' },
    { WEBRTC_SERVER_TCP: 'false' },
    { IMPAIRED_PROFILES: 'lossy' },
    { IMPAIRED_SILENT_AUDIO: '1' },
    { IMPAIR_SCRIPT: 'none' },
    { IMPAIR_LOSS: '5' },
    { CANARY_MODE: '1' },
    { NODE_OPTIONS: '--inspect' },
  ]) {
    let calls = 0;
    await assert.rejects(
      run({
        env: { ...base, ...patch },
        channel: {
          finish() {
            calls++;
          },
        },
        createOwner() {
          calls++;
        },
        suite,
      }),
    );
    assert.equal(calls, 0);
  }
});

test('TCP publication is bounded and follows browser and control closure; CLI diagnostics identify TCP', async () => {
  const f = fixture();
  let stdout = '';
  const output = new Writable({
    write(bytes, _encoding, callback) {
      f.assertClosed();
      stdout += bytes;
      callback();
    },
  });
  assert.equal(await main({ ...f.options, suite, stdout: output }), 0);
  assert.equal(JSON.parse(stdout).phasePassed, true);
  assert.ok(Buffer.byteLength(stdout) <= 16 * 1024);
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL('../e2e/browser-tcp.cjs', import.meta.url)), 'unexpected'],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.equal(result.status, 2);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'Browser ICE-TCP arguments are invalid.\n');
});
