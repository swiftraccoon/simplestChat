import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, chmod, symlink, rm, open } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { readLifecycleReport, LIFECYCLE_LIMITS } from '../../load_tests/lifecycle-diagnostic-report.mjs';

const participantId = '00000000-0000-4000-8000-000000000001';
const transportId = '00000000-0000-4000-8000-000000000002';
const generationId = '00000000-0000-4000-8000-000000000003';
const workerId = '00000000-0000-4000-8000-000000000004';
const base = Date.parse('2026-09-11T12:00:00.000Z');
const at = ms => new Date(base + ms).toISOString();
const marker = (ms, event, fields = '') => `${at(ms)} DEBUG simplestChat::lifecycle: lifecycle event="${event}" participant_id=${participantId}${fields ? ` ${fields}` : ''}`;
const transportFields = `transport_id=${transportId} transport_type="recv"`;
const transport = (ms, event, state) => marker(ms, event, `${transportFields}${state ? ` state=${state}` : ''}`);
const cleanup = (ms, event) => marker(ms, event, `generation=${generationId}`);
const clamp = ms => `${at(ms)} ERROR mediasoup::worker: [id:${workerId}] webrtc::GoogCcNetworkController::ClampConstraints() | start bitrate smaller than min bitrate [starting_rate_:30000, min_data_rate_:100000]`;
const timeline = (departure = 'abrupt') => ({ schemaVersion: 1, departure,
  events: [['run_started', 0], ['server_ready', 500], ['generator_started', 1000], ['generator_exited', 14000],
    ['cleanup_observed', 15000], ['server_stop_requested', 16000], ['server_exited', 17000]]
    .map(([event, elapsedMs]) => ({ event, at: at(elapsedMs), elapsedMs })),
  workload: { startedAt: at(1000).replace('Z', '+00:00'), finishedAt: at(13900).replace('Z', '+00:00'), rampUpSecs: 1, warmupSecs: 2, durationSecs: 8 } });
const fixture = (departure = 'abrupt') => [
  clamp(600), transport(1500, 'transport_created'), transport(2000, 'transport_ice', 'Connected'),
  transport(2500, 'transport_dtls', 'Connected'), clamp(5000), transport(12000, 'transport_ice', 'Disconnected'),
  clamp(12100), marker(12200, departure === 'abrupt' ? 'grace_started' : 'explicit_leave_started'),
  ...(departure === 'abrupt' ? [marker(13000, 'grace_cleanup_started')] : []),
  cleanup(13100, 'media_cleanup_started'), transport(13200, 'transport_closed'), clamp(13300),
  cleanup(13400, 'media_cleanup_finished'), ...(departure === 'explicit-leave' ? [marker(13500, 'explicit_leave_finished')] : []),
  clamp(15500), clamp(16500),
];

async function ownedLog(t, lines = fixture()) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-lifecycle-test-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, 'server.log');
  await writeFile(path, `${lines.join('\n')}\n`, { mode: 0o600, flag: 'wx' });
  return { directory, path };
}

test('realistic ANSI, span prefixes and UTC offsets produce private nominal phase evidence', async t => {
  const lines = fixture().map(line => line.replace(' DEBUG ', ' \u001b[34mDEBUG\u001b[0m dispatch{operation=consume}: ')
    .replace(/^(\S+)/, '\u001b[2m$1\u001b[0m'));
  lines.unshift(`${at(0)} INFO simplestChat::media::transport_manager: legacy-room\u001fprivate-room-secret`);
  const { path } = await ownedLog(t, lines);
  const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  assert.equal(report.coverage.complete, true, report.coverage.issues.join(', '));
  assert.equal(report.coverage.scope, 'expected_instrumented_markers');
  assert.deepEqual(report.clamps.map(row => row.phase), ['setup', 'measurement', 'departure', 'departure', 'post_cleanup', 'shutdown']);
  assert.deepEqual(report.timeline.nominalMeasurement, { startedAt: at(4000), finishedAt: at(12000) });
  assert.equal(report.totals.participantsWithCleanupFinished, 1);
  assert.ok(report.lifecycleEvents.every(row => row.participantOrdinal === 1));
  assert.match(report.interpretation, /never a transport identity or causal attribution/);
  const encoded = JSON.stringify(report);
  for (const privateValue of [participantId, transportId, generationId, workerId, 'private-room-secret', 'dispatch{', '\u001b']) assert.equal(encoded.includes(privateValue), false);
});

test('clamp aggregates use only earlier records and distinguish disconnection from close', async t => {
  const { path } = await ownedLog(t);
  const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  const [before, connected, disconnected, closed] = report.clamps;
  assert.equal(before.previouslyObserved.transportsCreated, 0);
  assert.equal(connected.previouslyObserved.transportsOpen, 1);
  assert.equal(connected.previouslyObserved.openTransportsWithDisconnectState, 0);
  assert.equal(disconnected.previouslyObserved.openTransportsIceDisconnected, 1);
  assert.equal(disconnected.previouslyObserved.transportsClosed, 0);
  assert.equal(closed.previouslyObserved.transportsClosed, 1);
  assert.equal(closed.previouslyObserved.transportsOpen, 0);
  assert.equal(closed.previouslyObserved.openTransportsWithDisconnectState, 0);
  assert.equal(closed.previouslyObserved.iceDisconnectedEvents, 1);
  assert.equal(closed.previouslyObserved.cleanupGenerationsFinished, 0);
});

test('explicit leave completion is required separately from media cleanup', async t => {
  const { path } = await ownedLog(t, fixture('explicit-leave'));
  assert.equal((await readLifecycleReport(path, timeline('explicit-leave'), { expectedParticipants: 1 })).coverage.complete, true);
  await writeFile(path, `${fixture('explicit-leave').filter(line => !line.includes('explicit_leave_finished')).join('\n')}\n`);
  const incomplete = await readLifecycleReport(path, timeline('explicit-leave'), { expectedParticipants: 1 });
  assert.ok(incomplete.coverage.issues.includes('explicit_leave_marker_missing'));
  assert.equal(incomplete.totals.participantsWithCleanupFinished, 1);
});

test('unknown keys, invalid states and control characters cannot enter reports', async t => {
  const bad = [
    transport(2600, 'transport_ice', 'Closed'),
    `${marker(2700, 'grace_started')} credential="private-secret"`,
    marker(2800, 'grace_started').replace(participantId, 'not-a-uuid-private-secret'),
    `${marker(2900, 'grace_started')}\u001fprivate-secret`,
    marker(3000, 'unrecognized_private_event'),
  ];
  const lines = fixture(); lines.splice(4, 0, ...bad);
  const { path } = await ownedLog(t, lines);
  const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  assert.equal(report.coverage.complete, false);
  assert.ok(report.coverage.issues.includes('invalid_lifecycle_record'));
  assert.ok(report.coverage.issues.includes('invalid_log_control'));
  assert.equal(report.lifecycleEvents.length, fixture().filter(line => line.includes('lifecycle:')).length);
  assert.equal(JSON.stringify(report).includes('private-secret'), false);
});

test('missing transport inventory, readiness, close, cleanup and grace cannot be complete', async t => {
  const { path } = await ownedLog(t);
  for (const missing of ['transport_', 'transport_created', 'transport_ice', 'transport_dtls', 'transport_closed', 'media_cleanup_started', 'media_cleanup_finished', 'grace_started', 'grace_cleanup_started']) {
    await writeFile(path, `${fixture().filter(line => !line.includes(`event="${missing}`)).join('\n')}\n`);
    const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
    assert.equal(report.coverage.complete, false, `Missing ${missing} must be incomplete`);
  }
  await writeFile(path, `${fixture().join('\n')}\n`);
  assert.equal((await readLifecycleReport(path, timeline(), { expectedParticipants: 2 })).coverage.complete, false);
});

test('missing and inconsistent timeline anchors retain records without claiming complete phases', async t => {
  const { path } = await ownedLog(t);
  for (const mutate of [
    value => { value.events.pop(); },
    value => { value.workload = null; },
    value => { value.events[3].elapsedMs += 1000; },
    value => { value.workload.startedAt = '2026-02-30T12:00:00Z'; },
    value => { value.events.reverse(); },
  ]) {
    const input = timeline(); mutate(input);
    const report = await readLifecycleReport(path, input, { expectedParticipants: 1 });
    assert.equal(report.coverage.complete, false);
    assert.ok(report.lifecycleEvents.length > 0);
  }
});

test('log timestamp regression beyond formatter jitter is incomplete and does not reorder observations', async t => {
  const { path } = await ownedLog(t);
  const small = fixture(); small.splice(5, 0, clamp(4900));
  await writeFile(path, `${small.join('\n')}\n`);
  assert.equal((await readLifecycleReport(path, timeline(), { expectedParticipants: 1 })).coverage.complete, true);
  const large = fixture(); large.splice(5, 0, clamp(4500));
  await writeFile(path, `${large.join('\n')}\n`);
  const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  assert.ok(report.coverage.issues.includes('log_clock_regression'));
  assert.equal(report.clamps[2].at, at(4500));
  assert.equal(report.clamps[2].previouslyObserved.transportsCreated, 1);
});

test('missing, unprivate, symlinked and truncated logs are incomplete', async t => {
  const { path, directory } = await ownedLog(t);
  const missing = await readLifecycleReport(join(directory, 'missing.log'), timeline(), { expectedParticipants: 1 });
  assert.ok(missing.coverage.issues.includes('file_unavailable'));
  await chmod(path, 0o644);
  assert.ok((await readLifecycleReport(path, timeline(), { expectedParticipants: 1 })).coverage.issues.includes('unsafe_file'));
  await chmod(path, 0o600);
  const link = join(directory, 'linked.log'); await symlink(path, link);
  assert.ok((await readLifecycleReport(link, timeline(), { expectedParticipants: 1 })).coverage.issues.includes('unsafe_file'));
  await writeFile(path, `${fixture().join('\n')}\n${marker(16600, 'grace_started')}`);
  const truncated = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  assert.ok(truncated.coverage.issues.includes('unterminated_line'));
  assert.equal(truncated.lifecycleEvents.length, fixture().filter(line => line.includes('lifecycle:')).length);
});

test('line length, line count, relevant-record and file-size bounds retain a valid prefix', async t => {
  const { path } = await ownedLog(t);
  const prefix = `${fixture().join('\n')}\n`;
  for (const [suffix, issue] of [
    ['x'.repeat(LIFECYCLE_LIMITS.lineBytes + 1), 'line_length_limit'],
    ['ordinary line\n'.repeat(LIFECYCLE_LIMITS.lines + 1), 'line_count_limit'],
    [`${clamp(16600)}\n`.repeat(LIFECYCLE_LIMITS.records + 1), 'record_limit'],
  ]) {
    await writeFile(path, `${prefix}${suffix}`);
    const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
    assert.ok(report.coverage.issues.includes(issue), issue);
    assert.ok(report.lifecycleEvents.length > 0);
    assert.ok(report.relevantRecords <= LIFECYCLE_LIMITS.records);
  }
  await writeFile(path, prefix);
  const file = await open(path, 'r+');
  try { await file.truncate(LIFECYCLE_LIMITS.bytes + 1); } finally { await file.close(); }
  const report = await readLifecycleReport(path, timeline(), { expectedParticipants: 1 });
  assert.ok(report.coverage.issues.includes('file_limit'));
  assert.ok(report.lifecycleEvents.length > 0);
});
