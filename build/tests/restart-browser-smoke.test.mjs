import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { parseOptions, runCli, runRestartBrowserSmoke } from '../restart-browser-smoke.mjs';

const valid = ['--binary', '/owned/simplestChat', '--browser', 'chromium', '--output', '/owned/fresh'];

test('restart browser options reject external URLs, unknown engines and duplicate/missing options', () => {
  assert.deepEqual(parseOptions(valid), { binary: '/owned/simplestChat', browser: 'chromium', output: '/owned/fresh' });
  for (const args of [[], [...valid, '--url', 'https://fixture.invalid'], [...valid, '--browser', 'firefox'],
    ['--binary', 'relative', ...valid.slice(2)], [...valid.slice(0, 3), 'safari', ...valid.slice(4)],
    [...valid.slice(0, 5)], [...valid.slice(0, 5), 'relative'],
  ]) assert.throws(() => parseOptions(args), /Usage:/u);
});

test('invalid programmatic inputs cannot validate a binary or start resources', async () => {
  let touched = false;
  await assert.rejects(runRestartBrowserSmoke({ binary: '/owned/server', browser: 'unknown', output: '/owned/fresh' }, {
    validateBinary: async () => { touched = true; },
  }), /Usage:/u);
  assert.equal(touched, false);
});

test('fresh output refusal preserves existing artifacts without starting Docker, browsers or servers', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'restart-browser-options.'));
  try {
    let started = false;
    await assert.rejects(runRestartBrowserSmoke({ binary: '/owned/server', browser: 'chromium', output: root }, {
      validateBinary: async () => {}, loadPlaywright: () => { started = true; },
    }), /fresh directory/u);
    assert.equal(started, false);
  } finally { await rm(root, { recursive: true }); }
});

test('browser launch failure still terminates only the owned guest server and retains failure', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'restart-browser-cleanup.'));
  const output = path.join(root, 'attempt');
  const commands = [], requests = [], kills = [];
  const child = new EventEmitter();
  child.pid = 43210;
  child.kill = signal => {
    kills.push(signal);
    queueMicrotask(() => child.emit('exit', 0, null));
    return true;
  };
  try {
    await assert.rejects(runRestartBrowserSmoke({ binary: '/owned/server', browser: 'chromium', output }, {
      validateBinary: async () => {}, reservePorts: async () => ({ http: 45101, media: 45102 }),
      spawn: (...args) => { commands.push(args); return child; },
      fetch: async url => { requests.push(url); return { status: 200, json: async () => ({ status: 'ready' }) }; },
      loadPlaywright: () => ({ chromium: { launchServer: async () => { throw new Error('Fixture launch failure'); } } }),
    }), /Restart browser smoke failed/u);
    assert.equal(commands.length, 1);
    assert.equal(commands[0][0], '/owned/server');
    assert.equal(commands[0][2].env.BIND_ADDR, '127.0.0.1');
    assert.equal(commands[0][2].env.RUN_MIGRATIONS, 'false');
    assert.equal(commands[0][2].env.DATABASE_URL, undefined);
    assert.deepEqual(requests, ['http://127.0.0.1:45101/ready']);
    assert.deepEqual(kills, ['SIGTERM']);
    const report = JSON.parse(await readFile(path.join(output, 'outcome.json'), 'utf8'));
    assert.equal(report.passed, false);
    assert.match(report.failure.message, /Fixture launch failure/u);
    assert.deepEqual(report.servers[0].exit, { code: 0, signal: null });
    assert.equal((await stat(output)).mode & 0o777, 0o700);
    assert.equal((await stat(path.join(output, 'outcome.json'))).mode & 0o777, 0o600);
  } finally { await rm(root, { recursive: true }); }
});

test('CLI reports malformed input before invoking the workload', async () => {
  let invoked = false;
  const sink = { write() {} };
  const result = await runCli({ args: [...valid, '--origin', 'https://fixture.invalid'], stdout: sink, stderr: sink,
    run: async () => { invoked = true; } });
  assert.equal(result, 2);
  assert.equal(invoked, false);
});

test('cancellation during browser launch adopts and closes the late owned browser', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'restart-browser-late-launch.'));
  const output = path.join(root, 'attempt');
  const cancellation = new AbortController();
  const child = new EventEmitter();
  child.pid = 43210;
  child.kill = signal => { assert.equal(signal, 'SIGTERM'); queueMicrotask(() => child.emit('exit', 0, null)); return true; };
  const browserProcess = { exitCode: null, signalCode: null };
  let closed = 0, connected = false;
  const browserServer = {
    process: () => browserProcess,
    close: async () => { closed++; browserProcess.exitCode = 0; },
    kill: async () => { assert.fail('Late launch should close gracefully'); },
  };
  try {
    await assert.rejects(runRestartBrowserSmoke({ binary: '/owned/server', browser: 'chromium', output, signal: cancellation.signal }, {
      validateBinary: async () => {}, reservePorts: async () => ({ http: 45101, media: 45102 }), spawn: () => child,
      fetch: async () => ({ status: 200, json: async () => ({ status: 'ready' }) }),
      loadPlaywright: () => ({ chromium: {
        launchServer: async () => {
          cancellation.abort();
          await new Promise(resolve => setTimeout(resolve, 10));
          return browserServer;
        },
        connect: async () => { connected = true; assert.fail('Cancelled workload must not continue'); },
      } }),
    }), /Restart browser smoke failed/u);
    const report = JSON.parse(await readFile(path.join(output, 'outcome.json'), 'utf8'));
    assert.equal(report.passed, false);
    assert.equal(closed, 1);
    assert.equal(connected, false);
    assert.equal(report.browserCleanup.passed, true);
    assert.equal(report.browserCleanup.exitCode, 0);
    assert.deepEqual(report.servers[0].exit, { code: 0, signal: null });
  } finally { await rm(root, { recursive: true }); }
});
