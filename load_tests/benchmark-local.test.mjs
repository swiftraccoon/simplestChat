import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cpuSeconds, resourceSummary, comparison, parseOptions, command, captureArguments, finishCapture, diagnosticPolicy, verifyExecutable } from './benchmark-local.mjs';

test('records immediate spawn errors and fast nonzero exits', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-benchmark-unit.'));
  t.after(() => rm(directory, { recursive: true }));
  const missing = await command(join(directory, 'missing'), [], directory, {}, join(directory, 'missing.log'));
  assert.match((await missing.completion).error, /ENOENT/);
  const failed = await command(process.execPath, ['-e', 'process.exit(23)'], directory, {}, join(directory, 'failed.log'));
  assert.equal((await failed.completion).code, 23);
});

test('parses Linux and macOS process CPU time', () => {
  assert.equal(cpuSeconds('0:00.02'), 0.02);
  assert.equal(cpuSeconds('01:02:03'), 3723);
  assert.equal(cpuSeconds('1-02:03:04'), 93784);
  assert.throws(() => cpuSeconds('broken'));
});

test('CPU uses measured process deltas within the shared window', () => {
  const samples = [0, 1000, 2000, 3000].map(elapsedMs => ({ elapsedMs, server: { cpuSeconds: elapsedMs / 2000, rssKiB: 2048 } }));
  assert.deepEqual(resourceSummary(samples, 'server', 1000, 2000), { samples: 2, sampledDurationSeconds: 1, cpuSeconds: 0.5, cpuPercentOfOneCore: 50, peakRssMiB: 2, medianRssMiB: 2 });
  assert.throws(() => resourceSummary(samples, 'server', 1500, 1700));
});

test('comparison refuses unmatched runs rather than reporting false success', () => {
  assert.throws(() => comparison([{ scenario: 'conference-10', variant: 'baseline', joinP99Ms: 1 }]));
  assert.throws(() => comparison([{ purpose: 'diagnostic' }]), /not performance/);
});

test('bounded local orchestrator rejects unknown options and excessive load', () => {
  const required = ['--baseline-root', '/tmp/b', '--baseline-bin', '/tmp/b/server', '--candidate-root', '/tmp/c', '--candidate-bin', '/tmp/c/server', '--generator', '/tmp/generator', '--output', '/tmp/results'];
  assert.deepEqual(parseOptions(required).clients, [10]);
  assert.equal(parseOptions(required).purpose, 'performance');
  assert.equal(parseOptions([...required, '--purpose', 'diagnostic']).purpose, 'diagnostic');
  assert.throws(() => parseOptions([...required, '--purpose', 'unknown']), /purpose/);
  assert.throws(() => parseOptions([...required, '--capture-interface', 'lo0']), /diagnostic/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--capture-interface', 'en0']), /lo\/lo0/);
  assert.throws(() => parseOptions([...required, '--diagnostic-detail', 'capture-only']), /diagnostic/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'capture-only']), /capture-interface/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'unknown']), /diagnostic-detail/);
  const quiet = parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'capture-only', '--capture-interface', 'lo0']);
  assert.deepEqual(diagnosticPolicy(quiet), { generatorArgs: [], requireSnapshots: false, serverLog: 'error', generatorLog: 'error' });
  assert.throws(() => parseOptions(['baseline-root', ...required.slice(1)]));
  for (const flags of [['--server', 'ws://example.com'], ['--clients', '1000'], ['--duration', '3600'], ['--clients', '10,10'], ['--port', '0'], ['--scenarios', 'unknown']]) assert.throws(() => parseOptions([...required, ...flags]));
  assert.throws(() => parseOptions([...required, '--clients', '50']), /join admission/);
  assert.throws(() => parseOptions([...required, '--clients', '50', '--scenarios', 'multi-room']), /join admission/);
  assert.throws(() => parseOptions([...required, '--scenarios', 'churn']), /join budget/);
  assert.equal(parseOptions([...required, '--clients', '30', '--scenarios', 'multi-room']).scenarios[0].rooms, 4);
  assert.equal(parseOptions([...required, '--clients', '50', '--ramp-up', '303']).clients[0], 50);
  assert.equal(parseOptions([...required, '--clients', '50', '--scenarios', 'multi-room', '--ramp-up', '101']).clients[0], 50);
});

test('optional capture is header-limited and scoped to owned loopback media ports', () => {
  const args = captureArguments({ purpose: 'diagnostic', captureInterface: 'lo0', udpPort: 41100, workers: 2 }, '/tmp/probe');
  assert.deepEqual(args, ['-i', 'lo0', '-p', '-nn', '-s', '64', '-B', '4096', '-U', '-c', '500000',
    '-w', '/tmp/probe/media-headers.pcap', 'udp and host 127.0.0.1 and portrange 41100-41101']);
  assert.throws(() => captureArguments({ purpose: 'performance', captureInterface: 'lo0' }, '/tmp/probe'));
  assert.throws(() => captureArguments({ purpose: 'diagnostic', captureInterface: 'en0' }, '/tmp/probe'));
  const quiet = captureArguments({ purpose: 'diagnostic', diagnosticDetail: 'capture-only', captureInterface: 'lo0', udpPort: 41100, workers: 1 }, '/tmp/probe');
  assert.equal(quiet[quiet.indexOf('-c') + 1], '2000000');
});

test('full diagnostics remain opt-in and quiet mode works with the original generator', () => {
  assert.deepEqual(diagnosticPolicy({ purpose: 'performance' }), { generatorArgs: [], requireSnapshots: false, serverLog: 'error', generatorLog: 'error' });
  const full = diagnosticPolicy({ purpose: 'diagnostic', diagnosticDetail: 'full' });
  assert.deepEqual(full.generatorArgs, ['--diagnostics']);
  assert.equal(full.requireSnapshots, true);
  assert.equal(full.generatorLog, 'warn,load_test=info');
});

test('server and generator executables must match their recorded hashes', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-binary-unit.'));
  t.after(() => rm(directory, { recursive: true }));
  const binary = join(directory, 'fixture');
  const expected = createHash('sha256').update('original').digest('hex');
  await writeFile(binary, 'original');
  await verifyExecutable(binary, expected, 'Generator');
  await verifyExecutable(binary, expected, 'candidate server');
  await writeFile(binary, 'changed');
  await assert.rejects(verifyExecutable(binary, expected, 'Generator'), /Generator binary changed/);
  await assert.rejects(verifyExecutable(binary, expected, 'candidate server'), /candidate server binary changed/);
  await assert.rejects(verifyExecutable(join(directory, 'missing'), expected, 'baseline server'), /ENOENT/);
});

test('capture drains after generation before termination and skips drain if already finished', async () => {
  const events = [];
  let complete;
  const capture = { completion: new Promise(resolve => { complete = resolve; }),
    kill(signal) {
      events.push(signal);
      capture.result = { code: 0 };
      complete(capture.result);
    } };
  await finishCapture(capture, async () => { events.push('drain'); });
  assert.deepEqual(events, ['drain', 'SIGTERM']);
  await finishCapture(capture, async () => { assert.fail('completed capture must not drain again'); });
});

test('late capture failures and unflushed termination cannot become diagnostic success', async () => {
  await finishCapture({ result: { code: 0 } });
  for (const result of [{ code: 2 }, { error: 'ENOENT' }, { code: null, signal: 'SIGKILL' }, { code: null, signal: 'SIGTERM' }]) {
    await assert.rejects(finishCapture({ result }), /did not finish cleanly/);
  }
  let complete;
  const capture = { completion: new Promise(resolve => { complete = resolve; }),
    kill() { queueMicrotask(() => { capture.result = { code: 2 }; complete(capture.result); }); } };
  await assert.rejects(finishCapture(capture, async () => {}), /did not finish cleanly/);
  const failedDuringDrain = {};
  await assert.rejects(finishCapture(failedDuringDrain, async () => {
    failedDuringDrain.result = { code: 2 };
  }), /did not finish cleanly/);
  await assert.rejects(finishCapture({}, async () => { throw new Error('drain failed'); }), /drain failed/);
});
