import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { runCli, runDiagnosticsSmoke } from '../diagnostics-smoke.mjs';
import { readDiagnosticReport } from '../../load_tests/diagnostic-report.mjs';

// Only disposable fixture files and fake child events; no sockets or processes.
const record = overrides => ({ schemaVersion: 1, kind: 'operation', operationId: 1, connectionId: 1,
  operation: 'join_room', stage: null, outcome: 'ok', startedUs: 1, elapsedUs: 100, ...overrides });
const summary = (count, overrides) => ({ schemaVersion: 1, kind: 'summary', accepted: count, written: count,
  dropped: 0, expired: 0, unfinished: 0, writeFailed: false, ...overrides });
const defaultRecords = () => [record(), record({ kind: 'stage', stage: 'room_lock_wait', outcome: 'completed' })];

async function fixture(t, options = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-diagnostics-smoke-unit.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const state = { commands: [], reportCommands: [], reads: 0, created: 0, stopped: false };
  const child = new EventEmitter();
  child.pid = 12345;
  const originalEnvironment = { BIND_ADDR: '127.0.0.1', PORT: '41111', ALLOW_AD_HOC_ROOMS: 'true' };
  const dependencies = {
    createDirectory: async () => { state.created++; return directory; },
    spawn: (binary, args, init) => { state.commands.push({ binary, args, init }); return child; },
    executeReport: async (binary, args, init) => {
      assert.equal(state.stopped, true, 'read only after the owned recorder stops');
      state.reads++;
      state.reportCommands.push({ binary, args, init });
      if (options.reportFailure) throw options.reportFailure;
      const report = await readDiagnosticReport(args[2]);
      options.mutateReport?.(report);
      const stdout = JSON.stringify(report);
      if (!report.coverage.complete) throw Object.assign(new Error('private incomplete report'), { code: 1, stdout });
      return { stdout };
    },
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
  const run = overrides => runDiagnosticsSmoke({ binary: '/owned/checkout/server',
    generator: '/owned/checkout/generator $(literal)', ...overrides }, dependencies);
  return { directory, state, dependencies, originalEnvironment, run };
}

test('diagnostic smoke uses the explicit generator adapter after owned exit and preserves the JS oracle', async t => {
  const f = await fixture(t);
  const signal = new AbortController().signal;
  const result = await f.run({ signal });
  assert.equal(result.passed, true);
  assert.equal(result.records, 2);
  assert.deepEqual(result.evidence, { joinRoom: true, roomLockWait: true });
  assert.deepEqual(result.serverExit, { code: 0, signal: null });
  assert.equal(result.serverShutdownPassed, true);
  assert.equal(f.state.commands.length, 1);
  assert.equal(f.state.reads, 1);
  assert.equal(f.state.reportCommands.length, 1);
  const command = f.state.reportCommands[0];
  assert.equal(command.binary, '/owned/checkout/generator $(literal)');
  assert.deepEqual(command.args, ['report', 'diagnostics', join(f.directory, 'server-diagnostics.jsonl')]);
  assert.equal(command.init.timeout, 30000);
  assert.equal(command.init.maxBuffer, 2 * 1024 * 1024);
  assert.equal(command.init.killSignal, 'SIGKILL');
  assert.ok(Object.keys(command.init.env).every(key => ['PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT'].includes(key)));
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
      assert.deepEqual(error.result.artifactIssues, []);
      if (options.noSummary) assert.ok(error.result.diagnosticCoverage.issues.includes('missing_summary'));
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
  assert.equal(f.state.reportCommands.length, 0);
});

test('generator failures and private unexpected report fields cannot enter smoke artifacts', async t => {
  for (const options of [
    { reportFailure: Object.assign(new Error('PRIVATE command output'), { code: 2, stdout: 'PRIVATE', stderr: 'PRIVATE' }) },
    { mutateReport: report => { report.privatePayload = 'PRIVATE'; } },
    { mutateReport: report => { report.stages[0].privatePayload = 'PRIVATE'; } },
  ]) {
    const f = await fixture(t, options);
    await assert.rejects(f.run(), error => {
      assert.deepEqual(error.result.artifactIssues, ['diagnostic_read_failed']);
      assert.equal(JSON.stringify(error.result).includes('PRIVATE'), false);
      return true;
    });
    await assert.rejects(stat(join(f.directory, 'server-diagnostics-report.json')), { code: 'ENOENT' });
    const saved = await readFile(join(f.directory, 'diagnostics-smoke.json'), 'utf8');
    assert.equal(saved.includes('PRIVATE'), false);
    assert.equal(JSON.parse(saved).passed, false);
  }
});

test('cancellation before setup or report admission never starts the next owned phase', async t => {
  const before = await fixture(t);
  const cancelled = new AbortController();
  cancelled.abort();
  await assert.rejects(before.run({ signal: cancelled.signal }), /Diagnostics smoke cancelled/);
  assert.equal(before.state.created, 0);
  const after = await fixture(t);
  const controller = new AbortController();
  const run = after.dependencies.run;
  after.dependencies.run = async (...args) => { const result = await run(...args); controller.abort(); return result; };
  await assert.rejects(after.run({ signal: controller.signal }));
  assert.equal(after.state.reads, 0);
  const result = JSON.parse(await readFile(join(after.directory, 'diagnostics-smoke.json'), 'utf8'));
  assert.equal(result.passed, false);
  assert.ok(result.artifactIssues.includes('smoke_cancelled'));
});

test('report or result-publication cancellation cannot retain success or return CLI zero', async t => {
  for (const phase of ['report', 'publication']) {
    const f = await fixture(t);
    let sawSuccessfulPublication = false;
    if (phase === 'report') {
      const execute = f.dependencies.executeReport;
      f.dependencies.executeReport = async (...args) => {
        const result = await execute(...args);
        process.emit('SIGTERM');
        return result;
      };
    } else {
      f.dependencies.save = async (path, value) => {
        await writeFile(path, `${JSON.stringify(value)}\n`, { flag: 'wx', mode: 0o600 });
        if (path === join(f.directory, 'diagnostics-smoke.json')) {
          sawSuccessfulPublication = value.passed;
          process.emit('SIGTERM');
        }
      };
    }
    let output = '';
    const stream = { write: text => { output += text; } };
    assert.equal(await runCli({ args: ['--binary', '/owned/server', '--generator', '/owned/generator'],
      stdout: stream, stderr: stream, run: f.run }), 1);
    const result = JSON.parse(await readFile(join(f.directory, 'diagnostics-smoke.json'), 'utf8'));
    assert.equal(result.passed, false);
    assert.ok(result.artifactIssues.includes('smoke_cancelled'));
    assert.equal(output.includes('PRIVATE'), false);
    assert.equal((await stat(join(f.directory, 'diagnostics-smoke.json'))).mode & 0o777, 0o600);
    if (phase === 'publication') assert.equal(sawSuccessfulPublication, true);
  }
});

test('publication cancellation preserves the original smoke failure cause', async t => {
  const failure = new Error('PRIVATE original failure');
  const f = await fixture(t, { failure });
  const controller = new AbortController();
  f.dependencies.save = async (path, value) => {
    await writeFile(path, `${JSON.stringify(value)}\n`, { flag: 'wx', mode: 0o600 });
    if (path === join(f.directory, 'diagnostics-smoke.json')) controller.abort();
  };
  await assert.rejects(f.run({ signal: controller.signal }), error => {
    assert.equal(error.cause, failure);
    assert.equal(error.result.passed, false);
    return true;
  });
});

test('cancelled result replacement never follows a final symlink and rewrite failure stays failed', async t => {
  for (const replacement of ['symlink', 'directory']) {
    const f = await fixture(t);
    const controller = new AbortController();
    const unrelated = join(f.directory, 'unrelated-private-file');
    await writeFile(unrelated, 'PRIVATE unchanged', { flag: 'wx', mode: 0o600 });
    f.dependencies.save = async (path, value) => {
      await writeFile(path, `${JSON.stringify(value)}\n`, { flag: 'wx', mode: 0o600 });
      if (path !== join(f.directory, 'diagnostics-smoke.json')) return;
      await rm(path);
      if (replacement === 'symlink') await symlink(unrelated, path);
      else await mkdir(path);
      controller.abort();
    };
    await assert.rejects(f.run({ signal: controller.signal }), error => {
      assert.equal(error.result.passed, false);
      assert.equal(error.result.artifactIssues.includes('smoke_result_write_failed'), replacement === 'directory');
      return true;
    });
    assert.equal(await readFile(unrelated, 'utf8'), 'PRIVATE unchanged');
    if (replacement === 'symlink') {
      assert.equal(JSON.parse(await readFile(join(f.directory, 'diagnostics-smoke.json'), 'utf8')).passed, false);
    }
  }
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

test('invalid binary or generator inputs and CLI arguments fail before artifacts or smoke', async t => {
  const f = await fixture(t);
  for (const binary of [undefined, '', 'relative/server', 'https://example.test/server']) await assert.rejects(f.run({ binary }), /Usage:/);
  for (const generator of [undefined, '', 'relative/generator', 'https://example.test/generator']) await assert.rejects(f.run({ generator }), /Usage:/);
  assert.equal(f.state.created, 0);
  assert.equal(f.state.commands.length, 0);
  let output = '';
  const stream = { write: text => { output += text; } };
  for (const args of [[], ['--url', 'http://127.0.0.1'], ['--binary', 'relative'],
    ['--binary', '/owned/server'], ['--binary', '/owned/server', '--extra'],
    ['--binary', '/owned/server', '--generator', 'relative/generator'],
    ['--binary', '/owned/server', '--unknown', '/owned/generator']]) {
    assert.equal(await runCli({ args, stdout: stream, stderr: stream, run: async () => assert.fail('must not run') }), 2);
  }
  assert.equal(await runCli({ args: ['--help'], stdout: stream, run: async () => assert.fail('help must not run') }), 0);
  assert.match(output, /Usage:/);
  let selected;
  assert.equal(await runCli({ args: ['--binary', '/owned/server', '--generator', '/owned/generator'], stdout: stream,
    run: async options => { selected = options; return { passed: true, records: 1, artifactDirectory: '/owned/artifacts' }; } }), 0);
  assert.equal(selected.binary, '/owned/server');
  assert.equal(selected.generator, '/owned/generator');
});

test('CLI reports only bounded status and artifact path, never exception or server contents', async t => {
  const f = await fixture(t, { failure: Object.assign(new Error('PRIVATE error'), { serverOutput: 'PRIVATE log' }) });
  let output = '';
  const stream = { write: text => { output += text; } };
  const listeners = ['SIGINT', 'SIGTERM'].map(signal => process.listenerCount(signal));
  const args = ['--binary', '/owned/server', '--generator', '/owned/generator'];
  assert.equal(await runCli({ args, stdout: stream, stderr: stream, run: f.run }), 1);
  assert.match(output, /FAIL diagnostics smoke/);
  assert.ok(output.includes(f.directory));
  assert.equal(output.includes('PRIVATE'), false);
  assert.deepEqual(['SIGINT', 'SIGTERM'].map(signal => process.listenerCount(signal)), listeners);
  output = '';
  assert.equal(await runCli({ args, stdout: stream, stderr: stream,
    run: async () => ({ passed: true, records: 11, artifactDirectory: '/owned/private-results' }) }), 0);
  assert.match(output, /PASS diagnostics smoke: 11 records/);
  assert.equal(await runCli({ args, stdout: stream, stderr: stream,
    run: async () => { throw new Error('PRIVATE setup'); } }), 1);
  assert.equal(await runCli({ args, stdout: stream, stderr: stream,
    run: async () => { process.emit('SIGTERM'); return { passed: true, records: 1 }; } }), 1);
  assert.equal(output.includes('PRIVATE'), false);
});
