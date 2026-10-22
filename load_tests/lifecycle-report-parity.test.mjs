// Explicit native gate; ordinary helpers never compile or select a generator.
import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, open, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { readLifecycleReport, LIFECYCLE_LIMITS } from './lifecycle-diagnostic-report.mjs';
import { readLifecycleReportWithGenerator } from './lifecycle-report-command.mjs';
import { lifecycleFixture } from './lifecycle-report-fixtures.mjs';

const binary = process.env.LOAD_TEST_REPORT_BINARY;
const execute = promisify(execFile);
const wire = value => JSON.parse(JSON.stringify(value));
async function owned(t) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat lifecycle parity '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  return { directory, path: join(directory, '--PRIVATE server.log') };
}
async function compare(path, timeline, expectedParticipants = 1) {
  const expected = wire(await readLifecycleReport(path, timeline, { expectedParticipants }));
  const actual = await readLifecycleReportWithGenerator(binary, path, timeline, { expectedParticipants });
  assert.deepEqual(actual, expected);
  for (const id of lifecycleFixture().ids) assert.equal(JSON.stringify(actual).includes(id), false);
  assert.equal(JSON.stringify(actual).includes('PRIVATE'), false);
  return actual;
}

test('native lifecycle reader matches calendar, phases, markers and private aggregate oracle', { skip: !binary }, async t => {
  const f = await owned(t);
  const cases = [];
  for (const departure of ['abrupt', 'explicit-leave']) cases.push({ name: departure, ...lifecycleFixture({ departure }) });
  const ansi = lifecycleFixture();
  ansi.lines = ansi.lines.map(line => line.replace(' DEBUG ', ' \u001b[34mDEBUG\u001b[0m dispatch{operation=consume}: ').replace(/^(\S+)/, '\u001b[2m$1\u001b[0m'));
  ansi.lines.unshift(`${ansi.at(0)} INFO simplestchat::media::transport_manager: ignored\u001fPRIVATE legacy`);
  cases.push({ name: 'ansi-and-private-legacy', ...ansi });
  for (const missing of ['transport_created', 'transport_ice', 'transport_dtls', 'transport_closed', 'media_cleanup_started', 'media_cleanup_finished', 'grace_started', 'grace_cleanup_started']) {
    const sample = lifecycleFixture(); sample.lines = sample.lines.filter(line => !line.includes(`event="${missing}`));
    cases.push({ name: `missing-${missing}`, ...sample });
  }
  for (const [name, mutate] of [
    ['invalid-state', sample => sample.lines.splice(4, 0, sample.transport(2600, 'transport_ice', 'Closed'))],
    ['unknown-field', sample => sample.lines.splice(4, 0, sample.marker(2600, 'grace_started') + ' credential="PRIVATE"')],
    ['unknown-event', sample => sample.lines.splice(4, 0, sample.marker(2600, 'PRIVATE_event'))],
    ['control', sample => sample.lines.splice(4, 0, sample.marker(2600, 'grace_started') + '\u001fPRIVATE')],
    ['unicode-control', sample => sample.lines.splice(4, 0, sample.marker(2600, 'grace_started') + '\u2029PRIVATE')],
    ['duplicate-created', sample => sample.lines.splice(4, 0, sample.transport(2600, 'transport_created'))],
    ['transport-owner-conflict', sample => sample.lines.splice(4, 0, sample.transport(2600, 'transport_created').replace(sample.ids[0], sample.ids[3]))],
    ['transport-kind-conflict', sample => sample.lines.splice(4, 0, sample.transport(2600, 'transport_created').replace('"recv"', '"send"'))],
    ['cleanup-owner-conflict', sample => sample.lines.push(sample.cleanup(16600, 'media_cleanup_finished').replace(sample.ids[0], sample.ids[3]))],
    ['duplicate-cleanup', sample => sample.lines.push(sample.cleanup(16600, 'media_cleanup_finished'))],
    ['small-regression', sample => sample.lines.splice(5, 0, sample.clamp(4900))],
    ['250ms-regression', sample => sample.lines.splice(5, 0, sample.clamp(4750))],
    ['250ms-plus-ns-regression', sample => sample.lines.splice(5, 0, sample.clamp(4750).replace('.750Z', '.749999999Z'))],
    ['large-regression', sample => sample.lines.splice(5, 0, sample.clamp(4500))],
    ['missing-anchor', sample => sample.timeline.events.pop()],
    ['invalid-workload', sample => { sample.timeline.workload = null; }],
    ['calendar-normalization', sample => { sample.timeline.workload.startedAt = '2026-02-30T12:00:00Z'; }],
    ['leap-second', sample => { sample.timeline.events[0].at = '2026-09-11T12:00:60Z'; }],
    ['non-utc', sample => { sample.timeline.events[0].at = '2026-09-11T12:00:00+01:00'; }],
    ['timeline-order', sample => sample.timeline.events.reverse()],
    ['timeline-duplicate', sample => { sample.timeline.events[1].event = 'run_started'; }],
    ['timeline-250ms-shift', sample => { sample.timeline.events[3].elapsedMs += 250; }],
    ['timeline-over250ms-shift', sample => { sample.timeline.events[3].elapsedMs += 250.001; }],
    ['nanosecond-timestamp', sample => { sample.timeline.events[0].at = sample.at(0).replace('.000Z', '.000000001Z'); }],
    ['extended-nominal-year', sample => { sample.timeline.workload = { startedAt: '9999-12-31T23:59:59.9Z', finishedAt: '9999-12-31T23:59:59.99Z', rampUpSecs: 0, warmupSecs: 0, durationSecs: 3 }; }],
  ]) {
    const sample = lifecycleFixture(); mutate(sample); cases.push({ name, ...sample });
  }
  for (const base of [Date.parse('0000-01-01T00:00:00Z'), Date.parse('1969-12-31T23:59:00Z'), Date.parse('9999-12-31T23:59:00Z')]) cases.push({ name: 'calendar-range', ...lifecycleFixture({ base }) });
  for (const { name, lines, timeline } of cases) await t.test(name, async () => {
    await writeFile(f.path, lines.join('\n') + '\n', { mode: 0o600 });
    await compare(f.path, timeline);
  });
  const sample = lifecycleFixture();
  await writeFile(f.path, sample.lines.join('\n') + '\n');
  for (const expected of [0, -1, 0.5, 2, 10001, Infinity, NaN]) await t.test('expected-participant-boundary', () => compare(f.path, sample.timeline, expected));
});

test('native lifecycle private/nonblocking file and bounded-prefix behavior matches oracle', { skip: !binary }, async t => {
  const f = await owned(t), sample = lifecycleFixture();
  const prefix = sample.lines.join('\n') + '\n';
  await writeFile(f.path, prefix, { mode: 0o600 });
  const link = join(f.directory, 'linked'); await symlink(f.path, link);
  for (const path of [link, f.directory, join(f.directory, 'absent')]) await compare(path, sample.timeline);
  await chmod(f.path, 0o644); await compare(f.path, sample.timeline); await chmod(f.path, 0o600);
  const fifo = join(f.directory, 'fifo'); await execute('mkfifo', [fifo]); await compare(fifo, sample.timeline);
  for (const [name, suffix] of [
    ['unterminated', sample.marker(16600, 'grace_started')],
    ['long-line', 'x'.repeat(LIFECYCLE_LIMITS.lineBytes + 1)],
    ['line-count', 'ordinary line\n'.repeat(LIFECYCLE_LIMITS.lines + 1)],
    ['record-count', (sample.clamp(16600) + '\n').repeat(LIFECYCLE_LIMITS.records + 1)],
  ]) await t.test(name, async () => { await writeFile(f.path, prefix + suffix); await compare(f.path, sample.timeline); });
  await writeFile(f.path, Buffer.concat([Buffer.from(prefix), Buffer.from([0xff, 10])])); await compare(f.path, sample.timeline);
  await writeFile(f.path, prefix);
  const file = await open(f.path, 'r+'); try { await file.truncate(LIFECYCLE_LIMITS.bytes + 1); } finally { await file.close(); }
  await compare(f.path, sample.timeline);
});

function rawCommand(path, input, { hold = false } = {}) {
  return new Promise((resolve, reject) => {
    let stdout = '', stderr = '', timedOut = false;
    const child = spawn(binary, ['report', 'lifecycle', path, '--expected-participants', '1'], { env: { ...process.env, TOKIO_WORKER_THREADS: '0' }, stdio: ['pipe', 'pipe', 'pipe'] });
    const timer = setTimeout(() => { timedOut = true; child.kill('SIGKILL'); }, 9000);
    child.stdin.on('error', () => {});
    child.stdout.on('data', chunk => { stdout += chunk; if (Buffer.byteLength(stdout) > 16 * 1024 * 1024) child.kill('SIGKILL'); });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.once('error', () => { clearTimeout(timer); reject(new Error('native lifecycle probe spawn failed')); });
    child.once('close', (code, signal) => { clearTimeout(timer); resolve({ code, signal, stdout, stderr, timedOut }); });
    if (!hold) child.stdin.end(input);
  });
}

test('native lifecycle stdin is bounded, retires a held pipe and never constructs Tokio', { skip: !binary }, async t => {
  const f = await owned(t), sample = lifecycleFixture();
  await writeFile(f.path, sample.lines.join('\n') + '\n', { mode: 0o600 });
  for (const [input, options] of [['x'.repeat(65537), {}], ['', { hold: true }]]) {
    const result = await rawCommand(f.path, input, options);
    assert.equal(result.code, 1); assert.equal(result.signal, null); assert.equal(result.timedOut, false); assert.equal(result.stderr, '');
    const report = JSON.parse(result.stdout); assert.equal(report.coverage.complete, false); assert.ok(report.coverage.issues.includes('invalid_timeline'));
    assert.equal(result.stdout.includes('PRIVATE'), false);
  }
  const usage = 'Usage: load_test report lifecycle LOG_PATH --expected-participants N < TIMELINE.json\n';
  const result = await execute(binary, ['report', 'lifecycle', '--help'], { env: { ...process.env, TOKIO_WORKER_THREADS: '0' } });
  assert.equal(result.stdout, usage); assert.equal(result.stderr, '');
  await assert.rejects(execute(binary, ['report', 'lifecycle', 'PRIVATE path']), error => { assert.equal(error.code, 2); assert.equal(error.stdout, ''); assert.equal(error.stderr, usage); return true; });
});
