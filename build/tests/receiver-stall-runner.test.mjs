import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { admitsDiagnosticEvidence, collectReceiverStallDiagnostics, runFinalizers } from '../../load_tests/benchmark-local.mjs';

const generatorSha256 = 'a'.repeat(64);
const options = { purpose: 'diagnostic', diagnosticDetail: 'full', departure: 'abrupt', rampUp: 3, warmup: 10, duration: 15 };
const stopped = () => ({ result: { code: 0, signal: null } });

function summary() {
  return { schemaVersion: 2, passed: false, diagnosticFailures: 1,
    run: { completed: true, startedAt: '2026-09-12T10:00:00Z', finishedAt: '2026-09-12T10:00:28Z',
      provenance: { generatorBinarySha256: generatorSha256 },
      configuration: { diagnostics: true, departure: 'abrupt', rampUpSecs: 3, warmupSecs: 10, durationSecs: 15 } } };
}

async function directoryFixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-stall-runner-test.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

function missingCaptureFixture() {
  const consumerId = '11111111-2222-4333-8444-555555555555';
  const producerId = '66666666-7777-4888-8999-aaaaaaaaaaaa';
  const trigger = { consumerOrdinal: 1, ssrc: 42, isAudio: false, beginBucket: 3, endBucket: 6 };
  return [{ clientId: 'PRIVATE-client', connectionAttempts: [{}],
    consumerDelivery: [{ consumerId, producerId, ssrc: 42, attempt: 1, isAudio: false,
      packetsBySecond: [5, 5, 5, 0, 0, 0, 5, 5], eligibleSeconds: 8, secondsWithPackets: 5,
      longestGapSeconds: 3, passed: false, skippedShortLived: false }],
    diagnostics: { failures: ['PRIVATE receiver capture failed'], receiverStalls: [], snapshots: [], events: [
      { attempt: 1, elapsedMs: 10, kind: 'consumer-created', details: { consumerId, producerId, ssrc: 42, kind: 'video' } },
      { attempt: 1, elapsedMs: 9500, kind: 'receiver-stall-triggered', details: { triggerElapsedMs: 9500, trigger } },
    ] } }];
}

test('completed failed workloads and bounded positive diagnostic failures remain admissible evidence', () => {
  for (const passed of [true, false]) for (const diagnosticFailures of [0, 1, 409600]) {
    const candidate = { ...summary(), passed, diagnosticFailures };
    const before = structuredClone(candidate);
    assert.equal(admitsDiagnosticEvidence(candidate, options, generatorSha256, false), true);
    assert.deepEqual(candidate, before, 'admission must not rewrite workload or diagnostic failure state');
  }
});

test('diagnostic evidence admission excludes disabled modes and every non-false timeout marker', () => {
  for (const disabled of [{ purpose: 'performance' }, { purpose: 'diagnostic', diagnosticDetail: 'capture-only' }]) {
    assert.equal(admitsDiagnosticEvidence(summary(), { ...options, ...disabled }, generatorSha256, false), false);
  }
  for (const timedOut of [true, undefined, null, 0, 1, 'false', {}, []]) {
    assert.equal(admitsDiagnosticEvidence(summary(), options, generatorSha256, timedOut), false);
  }
});

test('incomplete or foreign generator metadata cannot be admitted as diagnostic evidence', () => {
  for (const mutate of [
    value => { value.schemaVersion = 1; },
    value => { delete value.run; },
    value => { value.run.completed = false; },
    value => { value.run.completed = 'true'; },
    value => { delete value.run.provenance; },
    value => { value.run.provenance.generatorBinarySha256 = 'b'.repeat(64); },
    value => { delete value.run.configuration; },
    value => { value.run.configuration.diagnostics = false; },
    value => { value.run.configuration.diagnostics = 'true'; },
    value => { value.run.configuration.departure = 'graceful'; },
    value => { value.run.configuration.rampUpSecs++; },
    value => { value.run.configuration.warmupSecs++; },
    value => { value.run.configuration.durationSecs++; },
  ]) {
    const candidate = summary();
    mutate(candidate);
    assert.equal(admitsDiagnosticEvidence(candidate, options, generatorSha256, false), false);
  }
  for (const candidate of [undefined, null, {}, [], 'PRIVATE']) {
    assert.equal(admitsDiagnosticEvidence(candidate, options, generatorSha256, false), false);
  }
});

test('admission requires a canonical frozen executable hash even when provenance matches malformed input', () => {
  for (const digest of [undefined, null, '', 'a'.repeat(63), 'a'.repeat(65), 'A'.repeat(64), 'g'.repeat(64), 42, {}, []]) {
    const candidate = summary();
    candidate.run.provenance.generatorBinarySha256 = digest;
    assert.equal(admitsDiagnosticEvidence(candidate, options, digest, false), false);
  }
});

test('diagnostic failure counts must be explicit bounded nonnegative integers', () => {
  for (const diagnosticFailures of [undefined, null, -1, 0.5, 409601, Number.MAX_SAFE_INTEGER + 1,
    NaN, Infinity, '0', false, true, {}, []]) {
    assert.equal(admitsDiagnosticEvidence({ ...summary(), diagnosticFailures }, options, generatorSha256, false), false);
  }
});

test('receiver stall collection is inactive outside full diagnostics without inspecting processes or inputs', async () => {
  const unreadable = new Proxy({}, { get: () => assert.fail('disabled collection must not inspect inputs') });
  const persist = () => assert.fail('disabled collection must not persist artifacts');
  for (const disabled of [{ purpose: 'performance' }, { purpose: 'diagnostic', diagnosticDetail: 'capture-only' }]) {
    assert.equal(await collectReceiverStallDiagnostics(disabled, '/unused', unreadable, unreadable,
      unreadable, unreadable, persist), null);
  }
});

test('receiver stall collection waits for both owned processes to stop before deriving or persisting evidence', async () => {
  let writes = 0;
  const persist = async () => { writes++; };
  for (const [server, generator] of [[{}, stopped()], [stopped(), {}], [{}, {}]]) {
    await assert.rejects(collectReceiverStallDiagnostics(options, '/unused', server, generator, [], null, persist),
      /require stopped owned processes/);
  }
  assert.equal(writes, 0);
  for (const result of [{ code: 1, signal: null }, { code: null, signal: 'SIGTERM' }, { code: null, error: 'PRIVATE spawn failure' }]) {
    const report = await collectReceiverStallDiagnostics(options, '/unused', { result }, { result }, [], null, persist);
    assert.equal(report.coverage.available, false, 'failed exited processes still permit reporting missing evidence');
    assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
  }
  await collectReceiverStallDiagnostics(options, '/unused', undefined, undefined, [], null, persist);
  assert.equal(writes, 4, 'an absent child after launch failure must not prevent retaining missing-evidence status');
});

test('offline stall collection persists private missing evidence without making native requests', async t => {
  const directory = await directoryFixture(t);
  const fetch = t.mock.method(globalThis, 'fetch', () => assert.fail('post-run correlation must not request live state'));
  const report = await collectReceiverStallDiagnostics(options, directory, stopped(), stopped(), [], null);
  const path = join(directory, 'receiver-stall-report.json');
  assert.equal(fetch.mock.callCount(), 0);
  assert.deepEqual(JSON.parse(await readFile(path, 'utf8')), report);
  assert.equal((await stat(path)).mode & 0o777, 0o600);
  assert.equal(report.coverage.available, false);
  assert.equal(report.coverage.complete, false);
  assert.deepEqual(report.coverage.issues, ['generator_stall_evidence_unavailable']);
  assert.deepEqual(report.stalls, []);
});

test('post-run persistence retains a failed trigger even when receiver and native evidence are missing', async t => {
  const directory = await directoryFixture(t);
  const results = missingCaptureFixture();
  const before = structuredClone(results);
  const report = await collectReceiverStallDiagnostics(options, directory, stopped(), { result: { code: 1 } }, [], results);
  assert.deepEqual(results, before);
  assert.equal(report.coverage.available, true);
  assert.equal(report.coverage.complete, false);
  assert.equal(report.coverage.triggersObserved, 1);
  assert.equal(report.coverage.entriesRetained, 1);
  assert.equal(report.stalls[0].delivery.passed, false);
  assert.equal(report.stalls[0].receiverCapture.status, 'missing');
  assert.equal(report.stalls[0].receiverCapture.snapshotOrdinal, null);
  assert.equal(report.stalls[0].consumer.nativeAccounting.movement, 'unavailable');
  assert.equal(report.stalls[0].producer.nativeAccounting.movement, 'unavailable');
  assert.ok(report.coverage.issues.includes('generator_diagnostics_incomplete'));
  const retained = await readFile(join(directory, 'receiver-stall-report.json'), 'utf8');
  assert.deepEqual(JSON.parse(retained), report);
  for (const privateValue of ['PRIVATE', results[0].consumerDelivery[0].consumerId, results[0].consumerDelivery[0].producerId]) {
    assert.equal(retained.includes(privateValue), false);
  }
});

test('the actual receiver stall writer refuses to overwrite retained evidence', async t => {
  const directory = await directoryFixture(t);
  const path = join(directory, 'receiver-stall-report.json');
  const original = '{"retained":"original evidence"}\n';
  await writeFile(path, original, { flag: 'wx', mode: 0o600 });
  await assert.rejects(collectReceiverStallDiagnostics(options, directory, stopped(), stopped(), [], null), { code: 'EEXIST' });
  assert.equal(await readFile(path, 'utf8'), original);
});

test('stall report persistence failure preserves the original workload error and all later finalizers', async t => {
  const directory = await directoryFixture(t);
  const path = join(directory, 'receiver-stall-report.json');
  await writeFile(path, 'retained evidence', { flag: 'wx', mode: 0o600 });
  const primary = new Error('original workload failed');
  const events = [], errors = [];
  const run = async () => {
    try { throw primary; }
    finally {
      await runFinalizers(primary, [
        () => { events.push('stall report'); return collectReceiverStallDiagnostics(options, directory, stopped(), stopped(), [], null); },
        () => { events.push('remaining cleanup'); },
        async () => { events.push('remaining artifact'); await writeFile(join(directory, 'later.json'), '{}\n', { flag: 'wx' }); },
      ], error => errors.push(error));
    }
  };
  await assert.rejects(run(), error => error === primary);
  assert.deepEqual(events, ['stall report', 'remaining cleanup', 'remaining artifact']);
  assert.equal(errors.length, 1);
  assert.equal(errors[0].code, 'EEXIST');
  assert.equal(await readFile(path, 'utf8'), 'retained evidence');
  assert.equal(await readFile(join(directory, 'later.json'), 'utf8'), '{}\n');
});

test('stall report persistence failure rejects an otherwise successful run only after remaining finalizers', async t => {
  const directory = await directoryFixture(t);
  await writeFile(join(directory, 'receiver-stall-report.json'), 'retained evidence', { flag: 'wx', mode: 0o600 });
  const events = [], errors = [];
  await assert.rejects(runFinalizers(undefined, [
    () => collectReceiverStallDiagnostics(options, directory, stopped(), stopped(), [], null),
    () => { events.push('remaining cleanup'); },
    async () => { events.push('remaining artifact'); await writeFile(join(directory, 'later.json'), '{}\n', { flag: 'wx' }); },
  ], error => errors.push(error)), error => error === errors[0] && error.code === 'EEXIST');
  assert.deepEqual(events, ['remaining cleanup', 'remaining artifact']);
  assert.equal(errors.length, 1);
  assert.equal(await readFile(join(directory, 'later.json'), 'utf8'), '{}\n');
});
