import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { cpuSeconds, resourceSummary, comparison, parseOptions, command, captureArguments, finishCapture, diagnosticPolicy, createLifecycleTimeline, lifecycleWorkload, serverDiagnosticEnvironment, diagnosticRunStatus, performanceRunStatus, runFinalizers, collectServerDiagnostics, verifyExecutable, generatorSourceIdentity, sourceTreeIdentity, identity, serverRevisionLabel, stop, SERVER_SHUTDOWN_GRACE_MS } from './benchmark-local.mjs';
import { DIAGNOSTIC_LIMITS, readDiagnosticReport } from './diagnostic-report.mjs';
import { MEDIA_BODY_LIMIT, MEDIA_SAMPLE_LATENESS_MS, validateMediaSnapshot, mediaReference, fetchMediaSnapshot,
  mediaSampleSchedule, createMediaSampler, readGeneratorResults, correlateMediaDiagnostics } from './media-diagnostic-report.mjs';

const exec = promisify(execFile);

const diagnosticRecord = overrides => ({ schemaVersion: 1, kind: 'operation', operationId: 1,
  connectionId: 1, operation: 'join_room', stage: null, outcome: 'ok', startedUs: 10, elapsedUs: 1000, ...overrides });
const diagnosticSummary = (count, overrides) => ({ schemaVersion: 1, kind: 'summary', accepted: count,
  written: count, dropped: 0, expired: 0, unfinished: 0, writeFailed: false, ...overrides });
const jsonLines = records => records.map(record => JSON.stringify(record) + '\n').join('');
async function diagnosticFixture(t, content, mode = 0o600) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-diagnostic-unit.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, 'records.jsonl');
  await writeFile(path, content, { mode });
  return { directory, path };
}

test('server recorder is disabled in performance and capture-only runs, independently of inherited configuration', () => {
  for (const options of [{ purpose: 'performance' }, { purpose: 'diagnostic', diagnosticDetail: 'capture-only' }]) {
    assert.deepEqual(serverDiagnosticEnvironment(options, '/private/output'), {});
  }
  const options = { purpose: 'diagnostic', diagnosticDetail: 'full', rampUp: 5, warmup: 10, duration: 60 };
  assert.deepEqual(serverDiagnosticEnvironment(options, '/private/output'), {
    DIAGNOSTICS_PATH: '/private/output/server-diagnostics.jsonl', DIAGNOSTICS_MAX_RECORDS: '10000', DIAGNOSTICS_DURATION_SECS: '300',
    MEDIA_DIAGNOSTICS_ENABLED: 'true',
  });
  assert.equal(serverDiagnosticEnvironment({ ...options, rampUp: 600, warmup: 60, duration: 180 }, '/private/output').DIAGNOSTICS_DURATION_SECS, '1040');
});

test('server diagnostic reading requires a stopped owned process and never occurs in disabled modes', async () => {
  let reads = 0;
  const read = async path => { reads++; assert.equal(path, '/owned/server-diagnostics.jsonl'); return { fixture: true }; };
  const running = {};
  for (const options of [{ purpose: 'performance' }, { purpose: 'diagnostic', diagnosticDetail: 'capture-only' }]) {
    assert.equal(await collectServerDiagnostics(options, '/owned', running, read), null);
  }
  const full = { purpose: 'diagnostic', diagnosticDetail: 'full' };
  await assert.rejects(collectServerDiagnostics(full, '/owned', running, read), /after the owned server stops/);
  assert.equal(reads, 0);
  assert.deepEqual(await collectServerDiagnostics(full, '/owned', { result: { code: 0 } }, read), { fixture: true });
  assert.equal(reads, 1);
});

test('diagnostic coverage failure remains distinct from successful media workload', () => {
  const row = { workloadPassed: true, serverExit: { code: 0, signal: null }, diagnosticCoverage: { requested: true, complete: false },
    lifecycleDiagnosticCoverage: { requested: false, complete: null },
    mediaDiagnosticCoverage: { requested: false, complete: null } };
  assert.deepEqual(diagnosticRunStatus([row]), { workloadPassed: true, diagnosticCoverageComplete: false, mediaDiagnosticCoverageComplete: true, lifecycleDiagnosticCoverageComplete: true, serverShutdownPassed: true, passed: false });
  assert.deepEqual(diagnosticRunStatus([{ ...row, diagnosticCoverage: { requested: false, complete: null } }]),
    { workloadPassed: true, diagnosticCoverageComplete: true, mediaDiagnosticCoverageComplete: true, lifecycleDiagnosticCoverageComplete: true, serverShutdownPassed: true, passed: true });
  assert.equal(diagnosticRunStatus([{ workloadPassed: true }]).passed, false);
  assert.equal(diagnosticRunStatus([]).passed, false);
  assert.equal(diagnosticRunStatus([{ ...row, workloadPassed: false, diagnosticCoverage: { requested: true, complete: true } }]).passed, false);
});

test('complete diagnostic coverage and workload success cannot hide a failed owned server shutdown', () => {
  const row = { workloadPassed: true, diagnosticCoverage: { requested: true, complete: true }, mediaDiagnosticCoverage: { requested: true, complete: true }, lifecycleDiagnosticCoverage: { requested: true, complete: true } };
  for (const serverExit of [undefined, null, { code: 1, signal: null }, { code: null, signal: 'SIGKILL' },
    { code: null, signal: 'SIGTERM' }, { code: null, error: 'ENOENT' }, { code: 0, signal: 'SIGTERM' }]) {
    assert.deepEqual(diagnosticRunStatus([{ ...row, serverExit }]),
      { workloadPassed: true, diagnosticCoverageComplete: true, mediaDiagnosticCoverageComplete: true, lifecycleDiagnosticCoverageComplete: true, serverShutdownPassed: false, passed: false });
    assert.equal(diagnosticRunStatus([{ ...row, serverExit, diagnosticCoverage: { requested: false } }]).passed, false);
  }
  assert.deepEqual(diagnosticRunStatus([{ ...row, serverExit: { code: 0, signal: null } }]),
    { workloadPassed: true, diagnosticCoverageComplete: true, mediaDiagnosticCoverageComplete: true, lifecycleDiagnosticCoverageComplete: true, serverShutdownPassed: true, passed: true });
});

test('performance workload success requires a separately verified owned server exit', () => {
  for (const serverExit of [undefined, null, {}, { code: 1, signal: null }, { code: null, signal: 'SIGKILL' },
    { code: null, signal: 'SIGTERM' }, { code: null, error: 'ENOENT' }, { code: 0, signal: 'SIGTERM' },
    { code: 0, signal: null, error: 'shutdown failed' }]) {
    assert.deepEqual(performanceRunStatus([{ workloadPassed: true, serverExit }]),
      { workloadPassed: true, serverShutdownPassed: false, passed: false });
  }
  const serverExit = { code: 0, signal: null };
  assert.deepEqual(performanceRunStatus([{ workloadPassed: true, serverExit }]),
    { workloadPassed: true, serverShutdownPassed: true, passed: true });
  assert.deepEqual(performanceRunStatus([{ workloadPassed: false, serverExit }]),
    { workloadPassed: false, serverShutdownPassed: true, passed: false });
  assert.deepEqual(performanceRunStatus([]), { workloadPassed: false, serverShutdownPassed: false, passed: false });
});

test('performance comparisons cannot aggregate a failed shutdown even when timings match', () => {
  const fields = Object.fromEntries(['joinP99Ms', 'sendReadyP99Ms', 'receiveReadyP99Ms', 'receivedPacketsPerSecond',
    'serverCpuPercent', 'serverPeakRssMiB', 'generatorCpuPercent', 'generatorPeakRssMiB'].map(field => [field, 1]));
  const rows = ['baseline', 'candidate'].map(variant => ({ scenario: 'conference-10', variant, workloadPassed: true,
    serverExit: { code: 0, signal: null }, ...fields }));
  assert.equal(comparison(rows)['conference-10'].serverCpuPercent.delta, 0);
  for (const serverExit of [undefined, { code: 1, signal: null }, { code: null, signal: 'SIGKILL' }]) {
    assert.throws(() => comparison([rows[0], { ...rows[1], serverExit }]), /clean server shutdowns/);
  }
});

test('performance publication follows completed server shutdown rather than successful workload alone', async () => {
  const events = [];
  let complete;
  const server = { completion: new Promise(resolve => { complete = resolve; }),
    kill(signal) { events.push(signal); } };
  const row = { workloadPassed: true };
  await runFinalizers(undefined, [
    () => stop(server, SERVER_SHUTDOWN_GRACE_MS, async () => {
      assert.equal(performanceRunStatus([row]).passed, false);
      events.push('server exit');
      server.result = { code: 0, signal: null };
      complete(server.result);
    }),
    () => {
      row.serverExit = server.result;
      Object.assign(row, performanceRunStatus([row]));
      events.push('publish');
    },
  ]);
  assert.deepEqual(events, ['SIGTERM', 'server exit', 'publish']);
  assert.equal(row.workloadPassed, true);
  assert.equal(row.serverShutdownPassed, true);
  assert.equal(row.passed, true);
});

test('failed-run teardown and artifact errors cannot replace the original workload failure or skip later cleanup', async () => {
  const primary = new Error('original workload failed');
  const teardown = new Error('generator teardown failed');
  const artifact = new Error('artifact write failed');
  const events = [], errors = [];
  const failingRun = async () => {
    try { throw primary; }
    finally {
      await runFinalizers(primary, [
        () => { events.push('generator'); throw teardown; },
        () => { events.push('server'); },
        () => { events.push('artifact'); throw artifact; },
        () => { events.push('resources'); },
      ], error => errors.push(error));
    }
  };
  await assert.rejects(failingRun(), error => error === primary);
  assert.deepEqual(events, ['generator', 'server', 'artifact', 'resources']);
  assert.deepEqual(errors, [teardown, artifact]);
});

test('finalization failure rejects an otherwise successful run after attempting every remaining step', async () => {
  const failure = new Error('artifact write failed');
  let laterStep = false;
  await assert.rejects(runFinalizers(undefined, [() => { throw failure; }, () => { laterStep = true; }], () => {}),
    error => error === failure);
  assert.equal(laterStep, true);
});

test('a failing cleanup error reporter cannot skip later finalizers or replace the primary failure', async () => {
  const workload = new Error('original workload failed');
  const teardown = new Error('teardown failed');
  const reporter = new Error('error output failed');
  for (const primary of [undefined, workload]) {
    let laterStep = false;
    const finish = () => runFinalizers(primary, [
      () => { throw teardown; }, () => { laterStep = true; },
    ], () => { throw reporter; });
    const run = async () => {
      if (!primary) return finish();
      try { throw primary; }
      finally { await finish(); }
    };
    await assert.rejects(run(), error => error === (primary ?? teardown));
    assert.equal(laterStep, true);
  }
});

test('owned server receives a bounded 20s graceful shutdown while other children retain 5s', async () => {
  assert.equal(SERVER_SHUTDOWN_GRACE_MS, 20000);
  for (const grace of [undefined, SERVER_SHUTDOWN_GRACE_MS]) {
    const events = [];
    let complete;
    const child = { completion: new Promise(resolve => { complete = resolve; }),
      kill(signal) { events.push(signal); } };
    await stop(child, grace, async milliseconds => {
      events.push(milliseconds);
      child.result = { code: 0, signal: null };
      complete(child.result);
    });
    assert.deepEqual(events, ['SIGTERM', grace ?? 5000]);
    assert.deepEqual(child.result, { code: 0, signal: null });
    await stop(child, grace, async () => assert.fail('completed child must not be stopped again'));
  }
});

test('expired server shutdown grace forces SIGKILL and preserves the failing exit outcome', async () => {
  const events = [];
  let complete;
  const server = { completion: new Promise(resolve => { complete = resolve; }),
    kill(signal) {
      events.push(signal);
      if (signal === 'SIGKILL') { server.result = { code: null, signal }; complete(server.result); }
    } };
  await stop(server, SERVER_SHUTDOWN_GRACE_MS, async milliseconds => { events.push(milliseconds); });
  assert.deepEqual(events, ['SIGTERM', 20000, 'SIGKILL']);
  const status = diagnosticRunStatus([{ workloadPassed: true, serverExit: server.result,
    diagnosticCoverage: { requested: true, complete: true } }]);
  assert.equal(status.workloadPassed, true);
  assert.equal(status.diagnosticCoverageComplete, true);
  assert.equal(status.serverShutdownPassed, false);
  assert.equal(status.passed, false);
});

test('diagnostic percentiles use raw samples, keep outcomes separate and never add nested stages', async t => {
  const records = Array.from({ length: 100 }, (_, index) => diagnosticRecord({ operationId: index + 1, elapsedUs: (index + 1) * 1000 }));
  records.push(diagnosticRecord({ kind: 'stage', stage: 'room_lock_wait', elapsedUs: 2500 }));
  for (const outcome of ['error', 'completed', 'cancelled', 'timeout', 'rejected']) records.push(diagnosticRecord({ outcome, elapsedUs: 900000 }));
  const f = await diagnosticFixture(t, jsonLines([...records, diagnosticSummary(records.length)]));
  const report = await readDiagnosticReport(f.path);
  assert.deepEqual(report.coverage, { available: true, complete: true, scope: 'all_emitted_records', issues: [] });
  assert.deepEqual(report.operations.find(row => row.outcome === 'ok'), {
    operation: 'join_room', outcome: 'ok', count: 100, p50Ms: 50, p95Ms: 95, p99Ms: 99,
  });
  assert.deepEqual(report.stages, [{ operation: 'join_room', stage: 'room_lock_wait', outcome: 'ok', count: 1, p50Ms: 2.5, p95Ms: 2.5, p99Ms: 2.5 }]);
  assert.deepEqual(report.outcomeCounts, { ok: 101, error: 1, completed: 1, cancelled: 1, timeout: 1, rejected: 1 });
  assert.equal(JSON.stringify(report).includes('connectionId'), false);
  assert.equal(JSON.stringify(report).includes('operationId'), false);
});

test('missing and partial recorder output cannot establish complete coverage', async t => {
  const f = await diagnosticFixture(t, jsonLines([diagnosticRecord()]));
  const missing = await readDiagnosticReport(join(f.directory, 'absent'));
  assert.deepEqual(missing.coverage, { available: false, complete: false, scope: 'unavailable', issues: ['file_unavailable', 'missing_summary'] });
  const prefix = await readDiagnosticReport(f.path);
  assert.equal(prefix.coverage.complete, false);
  assert.equal(prefix.coverage.scope, 'recorded_subset');
  assert.deepEqual(prefix.coverage.issues, ['missing_summary']);
  assert.equal(prefix.operations[0].count, 1);
  await writeFile(f.path, jsonLines([diagnosticRecord()]) + JSON.stringify(diagnosticSummary(1)));
  const truncated = await readDiagnosticReport(f.path);
  assert.deepEqual(truncated.coverage.issues, ['unterminated_line', 'missing_summary']);
});

test('recorder limits and write failures are explicit subset coverage, not random sampling', async t => {
  const f = await diagnosticFixture(t, '');
  for (const [patch, expected] of [[{ dropped: 1 }, 'records_dropped'], [{ expired: 1 }, 'records_expired'],
    [{ unfinished: 1 }, 'unfinished_timers'],
    [{ writeFailed: true }, 'write_failed'], [{ accepted: 2 }, 'count_mismatch'], [{ written: 0 }, 'count_mismatch']]) {
    await writeFile(f.path, jsonLines([diagnosticRecord(), diagnosticSummary(1, patch)]));
    const report = await readDiagnosticReport(f.path);
    assert.equal(report.coverage.complete, false);
    assert.equal(report.coverage.scope, 'recorded_subset');
    assert.ok(report.coverage.issues.includes(expected));
    assert.equal(report.records, 1);
  }
});

test('diagnostic decoder rejects unknown vocabulary, private payloads and invalid numeric boundaries without echoing values', async t => {
  const f = await diagnosticFixture(t, '');
  const sentinel = 'PRIVATE room user credentials sdp 192.0.2.77';
  for (const patch of [{ operation: sentinel }, { kind: 'stage', stage: sentinel }, { outcome: sentinel },
    { privatePayload: sentinel }, { stage: 'room_lock_wait' }, { kind: 'stage', stage: null },
    { operationId: 0 }, { connectionId: -1 }, { startedUs: 0.5 }, { elapsedUs: Number.MAX_SAFE_INTEGER + 1 },
    { elapsedUs: null }, { schemaVersion: 2 }]) {
    await writeFile(f.path, jsonLines([diagnosticRecord(patch), diagnosticSummary(1)]));
    const report = await readDiagnosticReport(f.path);
    assert.equal(report.coverage.complete, false);
    assert.ok(report.coverage.issues.includes('invalid_record'));
    assert.equal(report.records, 0);
    assert.equal(JSON.stringify(report).includes(sentinel), false);
  }
  for (const value of [null, [], { secret: sentinel }]) {
    await writeFile(f.path, jsonLines([value]));
    const report = await readDiagnosticReport(f.path);
    assert.ok(report.coverage.issues.includes('invalid_record'));
    assert.equal(JSON.stringify(report).includes(sentinel), false);
  }
  await writeFile(f.path, `{"secret":"${sentinel}"\n`);
  const malformed = await readDiagnosticReport(f.path);
  assert.ok(malformed.coverage.issues.includes('invalid_json'));
  assert.equal(JSON.stringify(malformed).includes(sentinel), false);
});

test('diagnostic IDs allow nullable connection and safe positive maximum without being exported', async t => {
  const f = await diagnosticFixture(t, jsonLines([diagnosticRecord({ connectionId: null,
    operationId: Number.MAX_SAFE_INTEGER, startedUs: 0, elapsedUs: 0 }), diagnosticSummary(1)]));
  const report = await readDiagnosticReport(f.path);
  assert.equal(report.coverage.complete, true);
  assert.equal(report.operations[0].p99Ms, 0);
});

test('diagnostic summary is terminal, exact-schema and count-checked', async t => {
  const f = await diagnosticFixture(t, '');
  for (const patch of [{ accepted: 100001 }, { written: 2 }, { dropped: -1 }, { expired: 0.1 },
    { unfinished: -1 }, { unfinished: 0.1 }, { unfinished: Number.MAX_SAFE_INTEGER + 1 }, { unfinished: undefined },
    { writeFailed: 'false' }, { secret: 'PRIVATE' }, { schemaVersion: 2 }]) {
    await writeFile(f.path, jsonLines([diagnosticRecord(), diagnosticSummary(1, patch)]));
    const report = await readDiagnosticReport(f.path);
    assert.ok(report.coverage.issues.includes('invalid_summary'));
    assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
  }
  await writeFile(f.path, jsonLines([diagnosticRecord(), diagnosticSummary(1), diagnosticRecord()]));
  assert.ok((await readDiagnosticReport(f.path)).coverage.issues.includes('records_after_summary'));
  await writeFile(f.path, jsonLines([diagnosticSummary(0)]));
  assert.ok((await readDiagnosticReport(f.path)).coverage.issues.includes('no_records'));
});

test('diagnostic input has bounded lines, records and bytes and strict UTF-8', async t => {
  const f = await diagnosticFixture(t, 'x'.repeat(DIAGNOSTIC_LIMITS.lineBytes + 1) + '\n');
  assert.ok((await readDiagnosticReport(f.path)).coverage.issues.includes('line_limit'));
  await writeFile(f.path, jsonLines([diagnosticRecord(), diagnosticRecord(), diagnosticRecord(), diagnosticSummary(3)]));
  const limited = await readDiagnosticReport(f.path, { maxRecords: 2 });
  assert.ok(limited.coverage.issues.includes('record_limit'));
  assert.equal(limited.records, 2);
  await writeFile(f.path, 'x'.repeat(2 * (DIAGNOSTIC_LIMITS.lineBytes + 1) + 1));
  assert.ok((await readDiagnosticReport(f.path, { maxRecords: 1 })).coverage.issues.includes('file_limit'));
  await writeFile(f.path, Buffer.from([0xff, 10]));
  assert.ok((await readDiagnosticReport(f.path)).coverage.issues.includes('invalid_json'));
  for (const maxRecords of [0, -1, 1.5, 100001]) await assert.rejects(readDiagnosticReport(f.path, { maxRecords }), /Invalid diagnostic record bound/);
});

test('diagnostic reader refuses symlinks, directories and nonprivate files', async t => {
  const f = await diagnosticFixture(t, jsonLines([diagnosticRecord(), diagnosticSummary(1)]));
  const link = join(f.directory, 'linked.jsonl');
  await symlink(f.path, link);
  assert.ok((await readDiagnosticReport(link)).coverage.issues.includes('unsafe_file'));
  assert.ok((await readDiagnosticReport(f.directory)).coverage.issues.includes('unsafe_file'));
  await chmod(f.path, 0o644);
  assert.ok((await readDiagnosticReport(f.path)).coverage.issues.includes('unsafe_file'));
});

async function sourceFixture(t) {
  const temporary = await mkdtemp(join(tmpdir(), 'simplestchat-source-identity.'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const root = join(temporary, 'checkout with spaces');
  await mkdir(root);
  const git = async args => (await exec('git', ['-C', root, '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', ...args], {
    env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' },
  })).stdout.trim();
  const write = async (file, content) => {
    await mkdir(dirname(join(root, file)), { recursive: true });
    await writeFile(join(root, file), content);
  };
  await git(['init', '--initial-branch=main']);
  for (const [file, content] of Object.entries({
    '.gitignore': 'CLAUDE.md\nCLAUDE.local.md\nsrc/ignored.rs\n.cargo/config.toml\n/results/\n/target/\n',
    'Cargo.toml': '[package]\nname="fixture"\nversion="0.1.0"\n',
    'Cargo.lock': '# lock fixture\n', 'rust-toolchain.toml': '[toolchain]\nchannel="fixture"\n',
    'src/lib.rs': 'pub fn fixture() {}\n', 'src/deleted.rs': '// original\n',
    'vendor/native.cc': '// vendored native fixture\n', 'build/pip-constraints.txt': '# pinned fixture\n',
  })) await write(file, content);
  await git(['add', '--', '.']);
  await git(['commit', '-m', 'Initial fixture']);
  const binary = join(root, 'server');
  await writeFile(binary, 'binary fixture');
  return { root, binary, temporary, git, write };
}

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
  assert.deepEqual(full.generatorArgs, ['--diagnostics', '--departure', 'abrupt']);
  assert.equal(full.requireSnapshots, true);
  assert.equal(full.serverLog, 'warn,simplestChat::media::transport_manager=info,simplestChat::lifecycle=debug');
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

test('generator source identity includes subscription scheduling contents and requires every declared input', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-generator-source.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const paths = ['load_tests/bin/load_test.rs', 'load_tests/clients/metrics.rs',
    'load_tests/clients/measurement.rs', 'load_tests/clients/media_generator.rs',
    'load_tests/clients/webrtc_client.rs', 'load_tests/clients/subscriptions.rs'];
  const contents = paths.map(file => `// owned source fixture: ${file}\n`);
  for (const [index, file] of paths.entries()) {
    await mkdir(dirname(join(directory, file)), { recursive: true });
    await writeFile(join(directory, file), contents[index]);
  }
  const original = await generatorSourceIdentity(directory);
  assert.equal(original, `sha256:${createHash('sha256').update(contents.join('')).digest('hex')}`);
  const subscriptions = join(directory, 'load_tests/clients/subscriptions.rs');
  await writeFile(subscriptions, '// changed subscription dispatch policy\n');
  const changed = await generatorSourceIdentity(directory);
  assert.notEqual(changed, original, 'runtime subscription changes must alter generator provenance');
  await writeFile(join(directory, 'load_tests/README.md'), 'documentation only\n');
  assert.equal(await generatorSourceIdentity(directory), changed, 'documentation is outside runtime source scope');
  await rm(subscriptions);
  await assert.rejects(generatorSourceIdentity(directory), /ENOENT/);
});

test('source identity includes new Rust inputs and is unchanged by staging identical contents', async t => {
  const f = await sourceFixture(t);
  const before = await identity(f.root, f.binary);
  await f.write('src/readiness.rs', '// new first-party input\n');
  const untracked = await identity(f.root, f.binary);
  assert.equal(untracked.revision, before.revision);
  assert.equal(untracked.trackedDiffSha256, before.trackedDiffSha256, 'legacy diff cannot see untracked files');
  assert.notEqual(untracked.sourceTreeSha256, before.sourceTreeSha256);
  assert.equal(untracked.sourceTreeFiles, before.sourceTreeFiles + 1);
  await f.git(['add', '--', 'src/readiness.rs']);
  const staged = await identity(f.root, f.binary);
  assert.equal(staged.sourceTreeSha256, untracked.sourceTreeSha256);
  assert.notEqual(staged.trackedDiffSha256, untracked.trackedDiffSha256);
  await f.write('src/readiness.rs', '// changed after staging\n');
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, staged.sourceTreeSha256);
  await f.write('src/readiness.rs', '// new first-party input\n');
  await f.git(['commit', '-m', 'Track new input']);
  assert.equal((await sourceTreeIdentity(f.root)).sourceTreeSha256, staged.sourceTreeSha256, 'tree identity is independent of commit bookkeeping');
});

test('deleted source paths change identity consistently before staging and after commit', async t => {
  const f = await sourceFixture(t);
  const before = await sourceTreeIdentity(f.root);
  await rm(join(f.root, 'src/deleted.rs'));
  const deleted = await sourceTreeIdentity(f.root);
  assert.notEqual(deleted.sourceTreeSha256, before.sourceTreeSha256);
  assert.deepEqual(deleted.sourceTreeMissingPaths, ['src/deleted.rs']);
  assert.equal(deleted.sourceTreeFiles, before.sourceTreeFiles - 1);
  await f.git(['add', '-u']);
  assert.deepEqual(await sourceTreeIdentity(f.root), deleted, 'staging a deletion must not change its identity');
  await f.git(['commit', '-m', 'Remove input']);
  const committed = await sourceTreeIdentity(f.root);
  assert.equal(committed.sourceTreeSha256, deleted.sourceTreeSha256);
  assert.deepEqual(committed.sourceTreeMissingPaths, []);
});

test('ignored, private and generated paths do not enter the scoped source fingerprint', async t => {
  const f = await sourceFixture(t);
  const before = await sourceTreeIdentity(f.root);
  for (const file of ['CLAUDE.md', 'src/CLAUDE.md', 'vendor/CLAUDE.local.md', 'src/ignored.rs', '.cargo/config.toml', 'results/report.json', 'target/generated.rs', 'README.md']) await f.write(file, 'private or out-of-scope fixture\n');
  assert.deepEqual(await sourceTreeIdentity(f.root), before);
  await f.write('build.rs', '// build script\n');
  const buildScript = await sourceTreeIdentity(f.root);
  assert.notEqual(buildScript.sourceTreeSha256, before.sourceTreeSha256);
  await f.write('.cargo/config', '[build]\n');
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, buildScript.sourceTreeSha256);
});

test('source fingerprints include stable path boundaries and executable modes, not timestamps or index order', async t => {
  const f = await sourceFixture(t);
  await f.write('src/a.rs', 'one');
  await f.write('src/line\nbreak.rs', 'two');
  const original = await sourceTreeIdentity(f.root);
  await f.git(['add', '--', 'src/line\nbreak.rs', 'src/a.rs']);
  assert.deepEqual(await sourceTreeIdentity(f.root), original);
  await f.write('src/a.rs', 'one');
  assert.deepEqual(await sourceTreeIdentity(f.root), original, 'rewriting the same bytes does not change identity');
  await f.write('src/a.rs', 'two');
  await f.write('src/line\nbreak.rs', 'one');
  const swapped = await sourceTreeIdentity(f.root);
  assert.notEqual(swapped.sourceTreeSha256, original.sourceTreeSha256, 'file paths and content boundaries matter');
  await chmod(join(f.root, 'src/a.rs'), 0o755);
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, swapped.sourceTreeSha256);
});

test('symlink identity records its target without reading outside the source scope', async t => {
  const f = await sourceFixture(t);
  const outside = join(f.temporary, 'outside-input');
  await writeFile(outside, 'outside version one');
  await symlink(outside, join(f.root, 'src/linked.rs'));
  const original = await sourceTreeIdentity(f.root);
  await writeFile(outside, 'outside version two');
  assert.deepEqual(await sourceTreeIdentity(f.root), original, 'external target contents are deliberately outside the fingerprint');
  await rm(join(f.root, 'src/linked.rs'));
  await symlink(`${outside}-different`, join(f.root, 'src/linked.rs'));
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, original.sourceTreeSha256);
});

test('server provenance retains legacy diff hashes and labels Git, source and binary independently', async t => {
  const f = await sourceFixture(t);
  await f.write('src/lib.rs', 'pub fn changed() {}\n');
  const recorded = await identity(f.root, f.binary);
  const legacyDiff = await f.git(['diff', 'HEAD', '--', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'src', 'vendor', 'build/pip-constraints.txt']);
  assert.equal(recorded.trackedDiffSha256, createHash('sha256').update(legacyDiff).digest('hex'));
  assert.equal(serverRevisionLabel(recorded), `git:${recorded.revision};source:sha256:${recorded.sourceTreeSha256};binary:sha256:${recorded.binarySha256}`);
  assert.match(recorded.sourceTreeSha256, /^[a-f0-9]{64}$/);
  await writeFile(f.binary, 'different frozen executable');
  const changedBinary = await identity(f.root, f.binary);
  assert.equal(changedBinary.sourceTreeSha256, recorded.sourceTreeSha256);
  assert.equal(changedBinary.revision, recorded.revision);
  assert.notEqual(changedBinary.binarySha256, recorded.binarySha256);
  assert.notEqual(serverRevisionLabel(changedBinary), serverRevisionLabel(recorded));
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

const mediaSalt = '0123456789abcdef0123456789abcdef';
const consumerId = '11111111-2222-4333-8444-555555555555';
const producerId = '66666666-7777-4888-8999-aaaaaaaaaaaa';
const transportId = 'bbbbbbbb-cccc-4ddd-8eee-ffffffffffff';
const mediaOptions = { rampUp: 1, warmup: 2, duration: 8 };
const live = () => ({ server: true, generator: true });
function mediaSnapshot(index = 1, packets = index * 10) {
  return { schemaVersion: 1, correlationSalt: mediaSalt, sampleId: index, startedUs: index * 100000,
    finishedUs: index * 100000 + 100,
    coverage: { complete: true, registryBusy: false, participantsObserved: 1, participantsVisited: 1,
      busyParticipants: 0, participantLimitReached: false, entityLimitReached: false, deadlineReached: false },
    entities: [{ entityType: 'consumer', reference: mediaReference(mediaSalt, 'consumer', consumerId),
      transportReference: mediaReference(mediaSalt, 'transport', transportId), producerReference: mediaReference(mediaSalt, 'producer', producerId),
      kind: 'audio', status: 'ok', paused: false, producerPaused: false, observedUs: index * 100000 + 50,
      streams: [{ ssrc: 42, packetCount: packets, rtpBytes: packets * 100, workerTimestampMs: index * 1000 }] }] };
}
function mediaSamples() {
  return mediaSampleSchedule(mediaOptions).map((time, index) => ({ schemaVersion: 1, ordinal: index + 1,
    windowStartElapsedMs: 3000, windowEndElapsedMs: 11000, scheduledElapsedMs: time,
    startedElapsedMs: time + 10, finishedElapsedMs: time + 110, aliveAtStart: live(), aliveAtEnd: live(),
    status: 'ok', snapshot: mediaSnapshot(index + 1) }));
}
function generatorMedia() {
  return [{ clientId: 'PRIVATE-client', roomId: 'PRIVATE-room', errors: ['PRIVATE-error'], connectionAttempts: [{}],
    consumerDelivery: [{ consumerId, producerId, ssrc: 42, packetsBySecond: [5, 5, 5, 5, 5, 5, 5, 5],
      eligibleSeconds: 8, secondsWithPackets: 8, longestGapSeconds: 0, passed: true, skippedShortLived: false }],
    diagnostics: { failures: [], snapshots: [{ privateSdp: 'PRIVATE-sdp' }], events: [
      { attempt: 1, elapsedMs: 10, kind: 'consumer-created', details: { consumerId, producerId, ssrc: 42, kind: 'audio' } },
      { attempt: 1, elapsedMs: 20, kind: 'resume-requested', details: { consumerId } },
      { attempt: 1, elapsedMs: 30, kind: 'resume-ack', details: { consumerId } },
      { attempt: 1, elapsedMs: 40, kind: 'track-first-rtp', details: { ssrc: 42, transportId, address: '192.0.2.77' } },
    ] } }];
}

test('media references hash the exact domain, salt bytes, entity kind and canonical UUID', () => {
  const expected = createHash('sha256').update(Buffer.concat([Buffer.from('simplestchat-media-v1\0'),
    Buffer.from(mediaSalt, 'hex'), Buffer.from(`consumer\0${consumerId}`)])).digest('hex');
  assert.equal(mediaReference(mediaSalt, 'consumer', consumerId), expected);
  assert.notEqual(mediaReference(mediaSalt, 'producer', consumerId), expected);
  for (const args of [[mediaSalt.toUpperCase(), 'consumer', consumerId], [mediaSalt, 'PRIVATE', consumerId],
    [mediaSalt, 'consumer', producerId.toUpperCase()], [mediaSalt, 'consumer', 'not-a-uuid']]) {
    assert.throws(() => mediaReference(...args), /invalid_correlation_identity/);
  }
});

test('native media decoder enforces exact private-safe schema and bounded counters and inventories', () => {
  assert.equal(validateMediaSnapshot(mediaSnapshot()).entities[0].streams[0].ssrc, 42);
  const mutations = [v => { v.secret = 'PRIVATE'; }, v => { v.entities[0].address = '192.0.2.77'; },
    v => { v.entities[0].status = 'PRIVATE'; }, v => { v.entities[0].reference = consumerId; },
    v => { v.entities[0].streams[0].packetCount = Number.MAX_SAFE_INTEGER + 1; },
    v => { v.entities[0].streams[0].ssrc = 2 ** 32; }, v => { v.entities[0].streams[0].rtpBytes = -1; },
    v => { v.entities[0].streams[0].workerTimestampMs = 0.1; },
    v => { v.entities[0].streams = Array(17).fill(v.entities[0].streams[0]); },
    v => { v.entities = Array(1025).fill(v.entities[0]); }, v => { v.entities.push(v.entities[0]); },
    v => { v.entities[0].streams.push(v.entities[0].streams[0]); },
    v => { v.entities[0].observedUs = v.startedUs - 1; }, v => { v.entities[0].observedUs = v.finishedUs + 1; },
    v => { v.entities[0].streams = []; }, v => { v.coverage.participantsVisited = 65; },
    v => { v.coverage.busyParticipants = 1; }, v => { v.coverage.participantsObserved = 0; },
    v => { v.coverage.participantsObserved = 2; }, v => { v.coverage.registryBusy = true; },
    v => { v.coverage.deadlineReached = true; }, v => { v.sampleId = 0; }];
  for (const mutate of mutations) {
    const value = mediaSnapshot(); mutate(value);
    assert.throws(() => validateMediaSnapshot(value), error => error.message === 'invalid_media_schema');
  }
  const empty = mediaSnapshot(); empty.entities = []; empty.coverage.participantsObserved = 0; empty.coverage.participantsVisited = 0;
  assert.equal(validateMediaSnapshot(empty).coverage.complete, true);
  const closed = mediaSnapshot(); closed.coverage.complete = false; closed.entities[0].status = 'closed'; closed.entities[0].streams = [];
  assert.equal(validateMediaSnapshot(closed).entities[0].status, 'closed');
});

test('media fetch targets only loopback with auth, no redirects, no retries and no raw error output', async () => {
  let calls = 0;
  const result = await fetchMediaSnapshot({ origin: 'http://127.0.0.1:3119', token: 'PRIVATE' }, { fetch: async (url, options) => {
    calls++; assert.equal(url, 'http://127.0.0.1:3119/diagnostics/media');
    assert.equal(options.headers.Authorization, 'Bearer PRIVATE'); assert.equal(options.redirect, 'error'); assert.equal(options.cache, 'no-store');
    return Response.json(mediaSnapshot());
  } });
  assert.equal(result.status, 'ok'); assert.equal(calls, 1); assert.equal(JSON.stringify(result).includes('PRIVATE'), false);
  for (const [status, issue] of [[404, 'media_unavailable'], [401, 'media_unauthorized'], [429, 'media_busy'], [503, 'media_http_error']]) {
    assert.deepEqual(await fetchMediaSnapshot({ origin: 'http://127.0.0.1:3119', token: 'PRIVATE' },
      { fetch: async () => new Response('PRIVATE raw body', { status }) }), { status: 'error', issue });
  }
  for (const origin of ['https://127.0.0.1', 'http://localhost', 'http://192.0.2.77', 'http://127.0.0.1/private', 'http://secret@127.0.0.1']) {
    await assert.rejects(fetchMediaSnapshot({ origin, token: '' }, { fetch: () => assert.fail('invalid target must not fetch') }), /invalid_media_target/);
  }
  assert.deepEqual(await fetchMediaSnapshot({ origin: 'http://127.0.0.1', token: '' }, { fetch: async () => { throw new Error('PRIVATE'); } }),
    { status: 'error', issue: 'media_request_failed' });
});

test('media fetch bounds content length, streaming bytes, JSON/UTF-8 and the entire request deadline', async () => {
  const options = { origin: 'http://127.0.0.1', token: '' };
  for (const response of [new Response('x', { headers: { 'content-length': String(MEDIA_BODY_LIMIT + 1) } }),
    new Response(new Uint8Array(MEDIA_BODY_LIMIT + 1))]) {
    assert.deepEqual(await fetchMediaSnapshot(options, { fetch: async () => response }), { status: 'error', issue: 'media_body_limit' });
  }
  for (const body of ['PRIVATE', new Uint8Array([255])]) {
    assert.deepEqual(await fetchMediaSnapshot(options, { fetch: async () => new Response(body) }), { status: 'error', issue: 'invalid_media_json' });
  }
  assert.deepEqual(await fetchMediaSnapshot(options, { fetch: async () => Response.json({ secret: 'PRIVATE' }) }),
    { status: 'error', issue: 'invalid_media_schema' });
  for (const fetch of [() => new Promise(() => {}), async () => new Response(new ReadableStream({ start() {} }))]) {
    assert.deepEqual(await fetchMediaSnapshot({ ...options, timeoutMs: 5 }, { fetch }), { status: 'error', issue: 'media_timeout' });
  }
  let clock = 0;
  assert.deepEqual(await fetchMediaSnapshot(options, { now: () => clock++ ? 2001 : 0, fetch: async () => Response.json(mediaSnapshot()) }),
    { status: 'error', issue: 'media_timeout' });
});

test('media schedule is pre-armed, deduplicates short windows and captures serially while live', async () => {
  assert.deepEqual(mediaSampleSchedule(mediaOptions), [3000, 7000, 9000]);
  assert.deepEqual(mediaSampleSchedule({ ...mediaOptions, duration: 3 }), [3000, 4500]);
  assert.deepEqual(mediaSampleSchedule({ ...mediaOptions, duration: 4 }), [3000, 5000]);
  let clock = 0, calls = 0, release;
  const persisted = [];
  const sampler = createMediaSampler(mediaOptions, { now: () => clock, alive: live,
    request: async () => { calls++; await new Promise(resolve => { release = resolve; }); return { status: 'ok', snapshot: mediaSnapshot(calls) }; },
    persist: async sample => { persisted.push(structuredClone(sample)); } });
  await sampler.poll(); assert.equal(calls, 0);
  clock = 3000; const pending = sampler.poll();
  await sampler.poll(); assert.equal(calls, 1);
  await assert.rejects(sampler.finish(), /still_running/);
  clock = 3100; release(); await pending;
  const samples = await sampler.finish();
  assert.deepEqual(samples.map(sample => sample.status), ['ok', 'missed', 'missed']);
  assert.equal(persisted.length, 3); assert.equal(samples[0].startedElapsedMs, 3000); assert.equal(samples[0].finishedElapsedMs, 3100);
  assert.equal(samples[1].issue, 'media_sample_missed');
});

test('media sampling marks late/dead/teardown observations and preserves earlier evidence on later failures', async () => {
  let clock = 3000, isLive = true, calls = 0;
  const saved = [];
  const sampler = createMediaSampler(mediaOptions, { now: () => clock, alive: () => ({ server: true, generator: isLive }),
    request: async () => { if (++calls === 2) throw new Error('PRIVATE'); return { status: 'ok', snapshot: mediaSnapshot(calls) }; },
    persist: async sample => { saved.push(structuredClone(sample)); if (sample.ordinal === 2) throw new Error('PRIVATE'); } });
  await sampler.poll(); clock = 7000 + MEDIA_SAMPLE_LATENESS_MS + 1; await sampler.poll();
  isLive = false; clock = 9000; await sampler.poll();
  const samples = await sampler.finish();
  assert.equal(samples[0].status, 'ok'); assert.equal(samples[1].issue, 'media_request_failed');
  assert.equal(samples[1].timingIssue, 'media_sample_late'); assert.equal(samples[1].persistenceIssue, 'media_sample_write_failed');
  assert.equal(samples[2].issue, 'media_sample_not_live'); assert.equal(calls, 2);
  assert.equal(JSON.stringify(samples).includes('PRIVATE'), false); assert.equal(saved.length, 3);
  clock = 3000;
  const crossed = createMediaSampler(mediaOptions, { now: () => clock, alive: live,
    request: async () => { clock = 11000; return { status: 'ok', snapshot: mediaSnapshot() }; }, persist: async () => {} });
  await crossed.poll(); assert.equal(crossed.samples[0].timingIssue, 'media_sample_crossed_teardown');
});

test('media correlation preserves native counters vs measured receipt and exports no raw identity or private details', () => {
  const report = correlateMediaDiagnostics(mediaSamples(), generatorMedia());
  assert.equal(report.coverage.complete, true); assert.equal(report.coverage.consumersWithCounterPairs, 1);
  assert.deepEqual(report.consumers[0].nativeAccounting, { movement: 'increased', packetDelta: 20, byteDelta: 2000 });
  assert.equal(report.consumers[0].generator.packetsInMeasurement, 40);
  assert.equal(report.consumers[0].generator.resumeAcknowledged, true); assert.equal(report.consumers[0].generator.firstRtpEventObserved, true);
  for (const privateValue of ['PRIVATE', consumerId, producerId, transportId, '192.0.2.77']) assert.equal(JSON.stringify(report).includes(privateValue), false);
  assert.equal(report.samples.length, 3);
  for (const count of [0, 100]) {
    const samples = mediaSamples(); for (const sample of samples) { sample.snapshot.entities[0].streams[0].packetCount = count;
      sample.snapshot.entities[0].streams[0].rtpBytes = count * 100; sample.snapshot.entities[0].paused = true; }
    const flat = correlateMediaDiagnostics(samples, generatorMedia());
    assert.equal(flat.coverage.complete, true); assert.equal(flat.consumers[0].nativeAccounting.movement, count ? 'flat_nonzero' : 'flat_zero');
    assert.equal(flat.consumers[0].generator.packetsInMeasurement, 40, 'different intervals do not imply a contradiction');
  }
  const failedReceive = generatorMedia(); failedReceive[0].consumerDelivery[0].packetsBySecond.fill(0);
  failedReceive[0].consumerDelivery[0].passed = false; failedReceive[0].consumerDelivery[0].secondsWithPackets = 0;
  const reportWithFailedReceive = correlateMediaDiagnostics(mediaSamples(), failedReceive);
  assert.equal(reportWithFailedReceive.coverage.complete, true); assert.equal(reportWithFailedReceive.consumers[0].generator.deliveryPassed, false);
});

test('media correlation never treats missing/partial consumers as zero or single observations as a stable counter pair', () => {
  for (const status of ['closed', 'no_streams', 'timeout', 'error', 'stream_limit', 'not_collected']) {
    const samples = mediaSamples(); samples[1].snapshot.entities[0].status = status; samples[1].snapshot.entities[0].streams = [];
    samples[1].snapshot.coverage.complete = false;
    const report = correlateMediaDiagnostics(samples, generatorMedia());
    assert.equal(report.coverage.complete, false); assert.ok(report.coverage.issues.includes('native_coverage_incomplete'));
    assert.equal(report.consumers[0].observations[1].stream, null);
  }
  const samples = mediaSamples(); samples[1].snapshot.entities = []; samples[2].snapshot.entities = [];
  const single = correlateMediaDiagnostics(samples, generatorMedia());
  assert.equal(single.coverage.complete, false); assert.ok(single.coverage.issues.includes('consumer_counter_pair_missing'));
  assert.equal(single.consumers[0].nativeAccounting.movement, 'single_observation'); assert.equal(single.consumers[0].nativeAccounting.packetDelta, null);
  assert.deepEqual(single.consumers[0].observations[1], { sampleOrdinal: 2, status: 'not_observed' });
  samples[0].snapshot.entities = [];
  const absent = correlateMediaDiagnostics(samples, generatorMedia());
  assert.equal(absent.coverage.complete, false); assert.equal(absent.consumers[0].nativeAccounting.movement, 'unavailable');
  const short = generatorMedia(); short[0].consumerDelivery[0].skippedShortLived = true;
  assert.equal(correlateMediaDiagnostics(samples, short).coverage.shortLivedUnobservedConsumers, 1);
});

test('native counter reset, transport replacement and missing identity cannot invent growth', () => {
  for (const mutate of [e => { e.streams[0].packetCount = 0; }, e => { e.streams[0].rtpBytes = 0; },
    e => { e.streams[0].workerTimestampMs = 0; }, e => { e.transportReference = 'f'.repeat(64); }]) {
    const samples = mediaSamples(); mutate(samples[1].snapshot.entities[0]);
    assert.deepEqual(correlateMediaDiagnostics(samples, generatorMedia()).consumers[0].nativeAccounting,
      { movement: 'reset_or_replaced', packetDelta: null, byteDelta: null });
  }
  for (const all of [false, true]) {
    const samples = mediaSamples(); for (const sample of all ? samples : [samples[1]]) sample.snapshot.entities[0].transportReference = null;
    const report = correlateMediaDiagnostics(samples, generatorMedia());
    assert.equal(report.coverage.complete, false); assert.equal(report.coverage.consumersWithCounterPairs, 0);
    assert.ok(report.coverage.issues.includes('consumer_identity_unavailable')); assert.equal(report.consumers[0].nativeAccounting.packetDelta, null);
  }
});

test('media report validates schedule/order/liveness and rejects mixed namespaces without echoing malformed fields', () => {
  const mutations = [s => { s[1].ordinal = 1; }, s => { s[1].ordinal = 'PRIVATE'; },
    s => { s[1].scheduledElapsedMs = 7100; }, s => { s[1].startedElapsedMs = 6999; },
    s => { s[1].startedElapsedMs = 8050; s[1].finishedElapsedMs = 8100; },
    s => { s[1].finishedElapsedMs = 6999; }, s => { s[1].finishedElapsedMs = 11000; },
    s => { s[1].aliveAtEnd.generator = false; }, s => { s[1].windowEndElapsedMs++; },
    s => { s[1].snapshot.sampleId = 1; }, s => { s[1].snapshot.correlationSalt = 'f'.repeat(32); },
    s => { s[1].snapshot.entities[0].producerReference = 'f'.repeat(64); },
    s => { s[1].snapshot.entities[0].streams[0].ssrc++; }, s => { s[1].persistenceIssue = 'PRIVATE'; }];
  for (const mutate of mutations) {
    const samples = mediaSamples(); mutate(samples); const report = correlateMediaDiagnostics(samples, generatorMedia());
    assert.equal(report.coverage.complete, false); assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
  }
  assert.equal(correlateMediaDiagnostics(mediaSamples().slice(0, 1), generatorMedia()).coverage.complete, false);
  for (const generator of [undefined, [], [{ secret: 'PRIVATE' }]]) {
    const report = correlateMediaDiagnostics(mediaSamples(), generator); assert.equal(report.coverage.available, false);
    assert.ok(report.coverage.issues.includes('generator_correlation_unavailable')); assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
  }
});

test('generator evidence is per attempt, requires consumer creation and bounded valid measurement fields', () => {
  const valid = generatorMedia(); valid[0].connectionAttempts.push({});
  for (const event of valid[0].diagnostics.events) event.attempt = 2;
  assert.equal(correlateMediaDiagnostics(mediaSamples(), valid).consumers[0].generator.attemptOrdinal, 2);
  for (const mutate of [v => { v[0].diagnostics.events.shift(); }, v => { v[0].diagnostics.failures.push('PRIVATE'); },
    v => { v[0].consumerDelivery[0].packetsBySecond = [Number.MAX_SAFE_INTEGER, 1]; },
    v => { v[0].diagnostics.events[0].attempt = 2; }, v => { v[0].consumerDelivery[0].ssrc++; },
    v => { v[0].consumerDelivery[0].eligibleSeconds = 9; }]) {
    const generator = generatorMedia(); mutate(generator);
    assert.equal(correlateMediaDiagnostics(mediaSamples(), generator).coverage.available, false);
  }
});

test('generator result reader accepts ordinary completed files but rejects absent, nonregular and linked inputs', async t => {
  const f = await diagnosticFixture(t, JSON.stringify(generatorMedia()));
  assert.deepEqual(await readGeneratorResults(f.path), generatorMedia());
  const linked = join(f.directory, 'link'); await symlink(f.path, linked);
  for (const path of [linked, f.directory, join(f.directory, 'missing')]) await assert.rejects(readGeneratorResults(path), /generator_results_unavailable/);
  await writeFile(f.path, 'PRIVATE invalid JSON'); await assert.rejects(readGeneratorResults(f.path), /generator_results_unavailable/);
});

test('media correlation coverage is an independent gate, not a replacement for workload, recorder or shutdown status', () => {
  const row = { workloadPassed: true, serverExit: { code: 0, signal: null }, diagnosticCoverage: { requested: true, complete: true },
    lifecycleDiagnosticCoverage: { requested: true, complete: true },
    mediaDiagnosticCoverage: { requested: true, complete: false } };
  assert.deepEqual(diagnosticRunStatus([row]), { workloadPassed: true, diagnosticCoverageComplete: true,
    mediaDiagnosticCoverageComplete: false, lifecycleDiagnosticCoverageComplete: true, serverShutdownPassed: true, passed: false });
  assert.equal(diagnosticRunStatus([{ ...row, mediaDiagnosticCoverage: { requested: true, complete: true } }]).passed, true);
  assert.equal(diagnosticRunStatus([{ ...row, mediaDiagnosticCoverage: undefined }]).passed, false);
});

test('lifecycle evidence is independently required when requested', () => {
  const row = { workloadPassed: true, serverExit: { code: 0, signal: null }, diagnosticCoverage: { requested: true, complete: true },
    mediaDiagnosticCoverage: { requested: true, complete: true } };
  for (const lifecycleDiagnosticCoverage of [undefined, { requested: true, complete: false }, { requested: true }]) {
    const result = diagnosticRunStatus([{ ...row, lifecycleDiagnosticCoverage }]);
    assert.equal(result.workloadPassed, true);
    assert.equal(result.lifecycleDiagnosticCoverageComplete, false);
    assert.equal(result.passed, false);
  }
  assert.equal(diagnosticRunStatus([{ ...row, lifecycleDiagnosticCoverage: { requested: false, complete: null } }]).passed, true);
  assert.equal(diagnosticRunStatus([{ ...row, lifecycleDiagnosticCoverage: { requested: true, complete: true } }]).passed, true);
});

test('departure mode is opt-in diagnostic configuration and cannot alter performance or capture-only workloads', () => {
  const required = ['--baseline-root', '.', '--baseline-bin', './server', '--candidate-root', '.', '--candidate-bin', './server', '--generator', './generator', '--output', './output'];
  assert.equal(parseOptions(required).departure, 'abrupt');
  for (const departure of ['abrupt', 'explicit-leave']) {
    assert.throws(() => parseOptions([...required, '--departure', departure]), /full diagnostic/);
    assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'capture-only', '--capture-interface', 'lo0', '--departure', departure]), /full diagnostic/);
    const full = parseOptions([...required, '--purpose', 'diagnostic', '--departure', departure]);
    assert.equal(full.departure, departure);
    assert.deepEqual(diagnosticPolicy(full).generatorArgs, ['--diagnostics', '--departure', departure]);
  }
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--departure', 'unknown']), /departure/);
});

test('lifecycle timeline retains independent wall and monotonic clocks without inventing generator boundaries', () => {
  let elapsed = 100;
  let timestamp = '2026-09-12T00:00:00.000Z';
  const { timeline, mark } = createLifecycleTimeline('explicit-leave', { now: () => elapsed, wall: () => timestamp });
  mark('run_started');
  elapsed = 600;
  timestamp = '2026-09-11T23:59:59.000Z';
  mark('server_ready');
  assert.deepEqual(timeline, { schemaVersion: 1, departure: 'explicit-leave', workload: null, events: [
    { event: 'run_started', at: '2026-09-12T00:00:00.000Z', elapsedMs: 0 },
    { event: 'server_ready', at: '2026-09-11T23:59:59.000Z', elapsedMs: 500 },
  ] });
});

test('completed failing workloads retain lifecycle boundaries but mismatched provenance or configuration cannot supply them', () => {
  const options = { departure: 'abrupt', rampUp: 3, warmup: 10, duration: 8 };
  const run = { completed: true, passed: false, startedAt: '2026-09-12T00:00:00.000Z', finishedAt: '2026-09-12T00:00:21.020Z',
    configuration: { diagnostics: true, departure: 'abrupt', rampUpSecs: 3, warmupSecs: 10, durationSecs: 8 },
    provenance: { generatorBinarySha256: 'frozen' } };
  const summary = { schemaVersion: 2, run };
  assert.deepEqual(lifecycleWorkload(summary, options, 'frozen'), { startedAt: run.startedAt, finishedAt: run.finishedAt,
    rampUpSecs: 3, warmupSecs: 10, durationSecs: 8 });
  for (const candidate of [null, { ...summary, schemaVersion: 1 }, { ...summary, run: { ...run, completed: false } },
    { ...summary, run: { ...run, provenance: {} } }, { ...summary, run: { ...run, configuration: { ...run.configuration, durationSecs: 9 } } },
    { ...summary, run: { ...run, configuration: { ...run.configuration, departure: 'explicit-leave' } } }]) {
    assert.equal(lifecycleWorkload(candidate, options, 'frozen'), null);
  }
  assert.equal(lifecycleWorkload(summary, options, 'different'), null);
});
