import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { runCli, runDiagnosticsSmoke } from '../diagnostics-smoke.mjs';

// Only disposable fixture files and fake child events; no sockets or processes.
const record = overrides => ({ schemaVersion: 1, kind: 'operation', operationId: 1, connectionId: 1,
  operation: 'join_room', stage: null, outcome: 'ok', startedUs: 1, elapsedUs: 100, ...overrides });
const summary = (count, overrides) => ({ schemaVersion: 1, kind: 'summary', accepted: count, written: count,
  dropped: 0, expired: 0, unfinished: 0, writeFailed: false, ...overrides });
const defaultRecords = () => [record(), record({ kind: 'stage', stage: 'room_lock_wait', outcome: 'completed' })];

async function fixture(t, options = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-diagnostics-smoke-unit.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const state = { commands: [], reads: 0, created: 0, stopped: false };
  const child = new EventEmitter();
  child.pid = 12345;
  const originalEnvironment = { BIND_ADDR: '127.0.0.1', PORT: '41111', ALLOW_AD_HOC_ROOMS: 'true' };
  const dependencies = {
    createDirectory: async () => { state.created++; return directory; },
    spawn: (binary, args, init) => { state.commands.push({ binary, args, init }); return child; },
    run: async (runOptions, { spawn }) => {
      state.runOptions = runOptions;
      spawn(runOptions.binary, [], { cwd: '/owned/checkout', env: originalEnvironment, stdio: ['ignore', 'pipe', 'pipe'] });
      if (!options.missing) {
        const records = options.records ?? defaultRecords();
        const contents = options.raw ?? [...records, ...(options.noSummary ? [] : [summary(records.length, options.summary)])]
          .map(value => JSON.stringify(value) + '\n').join('');
        await writeFile(state.commands[0].init.env.DIAGNOSTICS_PATH, contents, { mode: 0o600, flag: 'wx' });
      }
      if (!options.alive) {
        state.stopped = true;
        child.emit('exit', options.exitCode ?? 0, options.exitSignal ?? null);
      }
      if (options.failure) throw options.failure;
      return { ready: true, joined: true, serverRestarting: true, closeCode: 1001,
        exitCode: 0, shutdownMs: 10, ...options.shutdown };
    },
  };
  const run = overrides => runDiagnosticsSmoke({ binary: '/owned/checkout/server', ...overrides }, dependencies);
  return { directory, state, dependencies, originalEnvironment, run };
}

test('diagnostic smoke wraps only spawn environment and validates real parser output after owned exit', async t => {
  const f = await fixture(t);
  const signal = new AbortController().signal;
  const result = await f.run({ signal });
  assert.equal(result.passed, true);
  assert.equal(result.records, 2);
  assert.deepEqual(result.evidence, { joinRoom: true, roomLockWait: true });
  assert.deepEqual(result.serverExit, { code: 0, signal: null });
  assert.equal(result.serverShutdownPassed, true);
  assert.equal(f.state.commands.length, 1);
  assert.equal(f.state.runOptions.signal, signal);
  assert.deepEqual(f.state.commands[0], { binary: '/owned/checkout/server', args: [], init: {
    cwd: '/owned/checkout', stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...f.originalEnvironment, DIAGNOSTICS_PATH: join(f.directory, 'server-diagnostics.jsonl') },
  } });
  assert.equal('DIAGNOSTICS_PATH' in f.originalEnvironment, false);
  assert.equal((await stat(f.directory)).mode & 0o777, 0o700);
  for (const file of ['server-diagnostics.jsonl', 'server-diagnostics-report.json', 'diagnostics-smoke.json']) {
    assert.equal((await stat(join(f.directory, file))).mode & 0o777, 0o600);
  }
  const saved = JSON.parse(await readFile(join(f.directory, 'diagnostics-smoke.json'), 'utf8'));
  assert.deepEqual(saved, result);
  const report = JSON.parse(await readFile(join(f.directory, 'server-diagnostics-report.json'), 'utf8'));
  assert.equal(report.coverage.complete, true);
});

for (const [name, options] of [
  ['no recorder support', { missing: true }], ['truncated recording', { noSummary: true }],
  ['lost records', { summary: { dropped: 1 } }], ['unfinished timers', { summary: { unfinished: 1 } }],
  ['wrong schema', { raw: '{"private":"do not echo"}\n' }],
  ['missing lock stage', { records: [record()] }],
  ['failed join record', { records: [record({ outcome: 'error' }), ...defaultRecords().slice(1)] }],
  ['nonzero exit', { exitCode: 2 }], ['signal-only exit', { exitSignal: 'SIGTERM' }],
  ['inconsistent shutdown result', { shutdown: { exitCode: 2 } }],
]) {
  test(`diagnostic smoke rejects ${name} and preserves its result artifacts`, async t => {
    const f = await fixture(t, options);
    await assert.rejects(f.run(), error => {
      assert.equal(error.message, 'Diagnostics smoke failed');
      assert.equal(error.result.passed, false);
      assert.equal(error.result.artifactDirectory, f.directory);
      assert.equal(JSON.stringify(error.result).includes('do not echo'), false);
      return true;
    });
    assert.equal(JSON.parse(await readFile(join(f.directory, 'diagnostics-smoke.json'), 'utf8')).passed, false);
  });
}

test('original shutdown failure remains the cause while valid recorder output and bounded logs survive', async t => {
  const failure = new Error('private original failure');
  failure.serverOutput = 'private server content\n'.repeat(3000);
  const f = await fixture(t, { failure });
  await assert.rejects(f.run(), error => {
    assert.equal(error.cause, failure);
    assert.equal(error.result.workloadPassed, false);
    assert.equal(error.result.diagnosticCoverageComplete, true);
    assert.equal(error.result.passed, false);
    return true;
  });
  assert.equal((await stat(join(f.directory, 'server.log'))).size, 32768);
  assert.equal((await stat(join(f.directory, 'server.log'))).mode & 0o777, 0o600);
});

test('an uncleaned child prevents racing its writer and does not replace the original cleanup failure', async t => {
  const failure = new Error('owned cleanup failed');
  const f = await fixture(t, { alive: true, failure });
  f.dependencies.readReport = async () => assert.fail('must not read a live writer');
  await assert.rejects(f.run(), error => {
    assert.equal(error.cause, failure);
    assert.deepEqual(error.result.artifactIssues, ['server_not_stopped']);
    assert.equal(error.result.diagnosticCoverageComplete, false);
    assert.equal(error.result.serverShutdownPassed, false);
    return true;
  });
});

test('report and artifact I/O failures do not mask the original smoke failure', async t => {
  const failure = new Error('original owned smoke failure');
  failure.serverOutput = 'private';
  const f = await fixture(t, { failure });
  f.dependencies.readReport = async () => { throw new Error('private read failure'); };
  f.dependencies.save = async () => { throw new Error('private save failure'); };
  f.dependencies.saveLog = async () => { throw new Error('private log failure'); };
  await assert.rejects(f.run(), error => {
    assert.equal(error.cause, failure);
    assert.deepEqual(error.result.artifactIssues, ['diagnostic_read_failed', 'server_log_write_failed', 'smoke_result_write_failed']);
    assert.equal(JSON.stringify(error.result).includes('private'), false);
    return true;
  });
});

test('invalid binary inputs and CLI arguments fail before creating artifacts or invoking the smoke', async t => {
  const f = await fixture(t);
  for (const binary of [undefined, '', 'relative/server', 'https://example.test/server']) await assert.rejects(f.run({ binary }), /Usage:/);
  assert.equal(f.state.created, 0);
  assert.equal(f.state.commands.length, 0);
  let output = '';
  const stream = { write: text => { output += text; } };
  for (const args of [[], ['--url', 'http://127.0.0.1'], ['--binary', 'relative'], ['--binary', '/owned/server', '--extra']]) {
    assert.equal(await runCli({ args, stdout: stream, stderr: stream, run: async () => assert.fail('must not run') }), 2);
  }
  assert.equal(await runCli({ args: ['--help'], stdout: stream, run: async () => assert.fail('help must not run') }), 0);
  assert.match(output, /Usage:/);
});

test('CLI reports only bounded status and artifact path, never exception or server contents', async t => {
  const f = await fixture(t, { failure: Object.assign(new Error('PRIVATE error'), { serverOutput: 'PRIVATE log' }) });
  let output = '';
  const stream = { write: text => { output += text; } };
  const listeners = ['SIGINT', 'SIGTERM'].map(signal => process.listenerCount(signal));
  assert.equal(await runCli({ args: ['--binary', '/owned/server'], stdout: stream, stderr: stream, run: f.run }), 1);
  assert.match(output, /FAIL diagnostics smoke/);
  assert.ok(output.includes(f.directory));
  assert.equal(output.includes('PRIVATE'), false);
  assert.deepEqual(['SIGINT', 'SIGTERM'].map(signal => process.listenerCount(signal)), listeners);
  output = '';
  assert.equal(await runCli({ args: ['--binary', '/owned/server'], stdout: stream, stderr: stream,
    run: async () => ({ passed: true, records: 11, artifactDirectory: '/owned/private-results' }) }), 0);
  assert.match(output, /PASS diagnostics smoke: 11 records/);
  assert.equal(await runCli({ args: ['--binary', '/owned/server'], stdout: stream, stderr: stream,
    run: async () => { throw new Error('PRIVATE setup'); } }), 1);
  assert.equal(output.includes('PRIVATE'), false);
});
