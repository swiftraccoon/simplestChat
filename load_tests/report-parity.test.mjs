// Explicit native parity gate. Ordinary helper checks keep the pure-JS oracle
// without building or implicitly selecting a native media generator.
import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { readDiagnosticReport } from './diagnostic-report.mjs';
import { readDiagnosticReportWithGenerator } from './report-command.mjs';

const execute = promisify(execFile);
const binary = process.env.LOAD_TEST_REPORT_BINARY;
const enabled = Boolean(binary);
const record = overrides => ({ schemaVersion: 1, kind: 'operation', operationId: 1, connectionId: 1,
  operation: 'join_room', stage: null, outcome: 'ok', startedUs: 10, elapsedUs: 1000, ...overrides });
const summary = (count, overrides) => ({ schemaVersion: 1, kind: 'summary', accepted: count,
  written: count, dropped: 0, expired: 0, unfinished: 0, writeFailed: false, ...overrides });
const lines = records => records.map(value => `${JSON.stringify(value)}\n`).join('');

async function command(path, maxRecords) {
  const args = ['report', 'diagnostics', path, ...(maxRecords === undefined ? [] : ['--max-records', String(maxRecords)])];
  let stdout, status = 0;
  try { ({ stdout } = await execute(binary, args, { maxBuffer: 2 * 1024 * 1024, timeout: 30000, killSignal: 'SIGKILL' })); }
  catch (error) {
    if (error.code !== 1 || error.killed || error.signal) throw new Error('native report parity command failed');
    stdout = error.stdout; status = 1;
  }
  const report = JSON.parse(stdout);
  assert.equal(status, report.coverage.complete ? 0 : 1);
  return report;
}

test('native diagnostics command matches the JS oracle across schema, numeric and bounded-reader cases', { skip: !enabled }, async t => {
  assert.equal(isAbsolute(binary), true, 'LOAD_TEST_REPORT_BINARY must select an explicit absolute executable');
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat report parity '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, '--PRIVATE recorder.jsonl');
  const cases = [
    ['complete', lines([record(), summary(1)])],
    ['server-close-consumer', lines([record({ operation: 'close_consumer' }), summary(1)])],
    ['empty', ''], ['prefix', lines([record()])],
    ['unterminated', lines([record()]) + JSON.stringify(summary(1))],
    ['after-summary', lines([record(), summary(1), record()])],
    ['empty-summary', lines([summary(0)])],
    ['line-limit', `${'x'.repeat(1025)}\n`],
    ['file-limit', 'x'.repeat(2051), 1],
    ['record-limit', lines([record(), record(), record(), summary(3)]), 2],
    ['invalid-utf8', Buffer.from([0xff, 10])],
    ['invalid-json', '{"secret":"PRIVATE"\n'],
    ['utf8-bom', `\ufeff${JSON.stringify(record())}\n\ufeff${JSON.stringify(summary(1))}\n`],
    ['crlf', lines([record(), summary(1)]).replaceAll('\n', '\r\n')],
    ['blank-after-summary', lines([record(), summary(1)]) + '\n'],
    ['blank-line', '\n'],
    ['null', 'null\n'], ['array', '[]\n'],
    ['scalar-overflow', '1e400\n'],
    ['lone-surrogate-value', '{"private":"\\ud800"}\n'],
    ['lone-surrogate-key', '{"\\ud800":1}\n'],
    ['summary-surrogate-key', '{"kind":"summary","\\ud800":1}\n'],
    ['nested-array', '['.repeat(200) + '0' + ']'.repeat(200) + '\n'],
  ];
  for (const patch of [{ operation: 'PRIVATE' }, { kind: 'stage', stage: 'PRIVATE' }, { outcome: 'PRIVATE' },
    { privatePayload: 'PRIVATE' }, { stage: 'room_lock_wait' }, { kind: 'stage', stage: null },
    { operationId: 0 }, { connectionId: -1 }, { connectionId: null },
    { operationId: Number.MAX_SAFE_INTEGER }, { startedUs: 0.5 }, { elapsedUs: Number.MAX_SAFE_INTEGER + 1 },
    { elapsedUs: null }, { schemaVersion: 2 }]) cases.push(['record-field', lines([record(patch), summary(1)])]);
  for (const patch of [{ accepted: 100001 }, { written: 2 }, { dropped: -1 }, { expired: 0.1 },
    { unfinished: Number.MAX_SAFE_INTEGER + 1 }, { unfinished: undefined }, { writeFailed: 'false' },
    { secret: 'PRIVATE' }, { schemaVersion: 2 }, { dropped: 1 }, { expired: 1 }, { unfinished: 1 },
    { accepted: 2 }, { written: 0 }, { writeFailed: true }]) cases.push(['summary-field', lines([record(), summary(1, patch)])]);
  for (const token of ['-0', '1.0', '1e0', '1.0000000000000001', '9007199254740991',
    '9007199254740991.49', '9007199254740991.5', '1e400', '-1e400', '0.5', '0.0000000000000000001']) {
    cases.push(['raw-number', lines([record(), summary(1)]).replace('"elapsedUs":1000', `"elapsedUs":${token}`)]);
  }
  cases.push(['duplicate-last', lines([record(), summary(1)]).replace('"operation":"join_room"', '"operation":"PRIVATE","operation":"join_room"')]);
  cases.push(['duplicate-invalid-last', lines([record(), summary(1)]).replace('"operation":"join_room"', '"operation":"join_room","operation":"PRIVATE"')]);
  cases.push(['escaped-string', lines([record(), summary(1)]).replace('join_room', 'join\\u005froom')]);
  for (const [name, content, maxRecords] of cases) await t.test(name, async () => {
    await writeFile(path, content, { mode: 0o600 });
    const options = maxRecords === undefined ? {} : { maxRecords };
    // Compare the artifact boundary: JSON.stringify normalizes JS -0 to 0.
    const expected = JSON.parse(JSON.stringify(await readDiagnosticReport(path, options)));
    const actual = await command(path, maxRecords);
    assert.deepEqual(actual, expected);
    if (maxRecords === undefined) {
      assert.deepEqual(await readDiagnosticReportWithGenerator(binary, path), expected);
    }
    assert.equal(JSON.stringify(actual).includes('PRIVATE'), false);
  });
  // Real operational adapter must retain native complete and incomplete reports.
  await writeFile(path, lines([record(), summary(1)]));
  assert.deepEqual(await readDiagnosticReportWithGenerator(binary, path), await readDiagnosticReport(path));
  await writeFile(path, lines([record()]));
  assert.deepEqual(await readDiagnosticReportWithGenerator(binary, path), await readDiagnosticReport(path));
});

test('native no-follow/private-file refusal and group percentile order match the JS oracle', { skip: !enabled }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat report boundary parity '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, 'records');
  await writeFile(path, lines([record(), summary(1)]), { mode: 0o600 });
  const link = join(directory, 'link'); await symlink(path, link);
  const folder = join(directory, 'folder'); await mkdir(folder, { mode: 0o700 });
  for (const refused of [link, folder, join(directory, 'absent')]) assert.deepEqual(await command(refused), await readDiagnosticReport(refused));
  await chmod(path, 0o644);
  assert.deepEqual(await command(path), await readDiagnosticReport(path));
  await chmod(path, 0o600);
  const operations = ['join_room', 'leave_room', 'reconnect', 'router_capabilities', 'create_send_transport', 'create_recv_transport', 'connect_transport', 'produce', 'consume', 'resume_consumer', 'pause_consumer', 'close_consumer', 'pause_producer', 'resume_producer', 'close_producer', 'restart_ice', 'set_preferred_layers', 'chat', 'private_message', 'room_action', 'socket_write', 'shutdown_notification'];
  const stages = ['room_creation_lock_wait', 'room_lock_wait', 'room_policy_lookup', 'room_password_permit_wait', 'room_password_dispatch', 'room_password_work', 'room_media_setup', 'room_membership_commit', 'session_lock_wait', 'media_create_transport', 'media_connect_transport', 'media_produce', 'media_consume', 'media_resume', 'outbound_queue_wait', 'socket_write', 'dispatch'];
  const outcomes = ['ok', 'error', 'completed', 'cancelled', 'timeout', 'rejected'];
  const records = [];
  for (const operation of operations) for (const outcome of outcomes) {
    records.push(record({ operation, outcome }));
    for (const stage of stages) records.push(record({ operation, outcome, kind: 'stage', stage }));
  }
  for (let index = 1; index <= 199; index++) records.push(record({ elapsedUs: index === 199 ? Number.MAX_SAFE_INTEGER : index }));
  await writeFile(path, lines([...records.reverse(), summary(records.length)]));
  const expected = await readDiagnosticReport(path);
  assert.deepEqual(await command(path), expected);
  assert.deepEqual(await readDiagnosticReportWithGenerator(binary, path), expected);
});

test('offline CLI documents coverage exits, rejects invalid argv and bypasses runtime construction', { skip: !enabled }, async () => {
  // A Tokio runtime would reject this setting. Offline reports must dispatch
  // synchronously before tracing or media workload initialization.
  const env = { ...process.env, TOKIO_WORKER_THREADS: '0' };
  const usage = 'Usage: load_test report diagnostics PATH [--max-records 1..100000]\n';
  const help = await execute(binary, ['report', '--help'], { env });
  assert.equal(help.stdout, usage);
  assert.equal(help.stderr, '');
  for (const args of [[], ['other', 'PRIVATE path'], ['diagnostics'], ['diagnostics', 'PRIVATE path', '--max-records', '0'], ['diagnostics', 'PRIVATE path', '--max-records', '100001']]) {
    await assert.rejects(execute(binary, ['report', ...args], { env }), error => {
      assert.equal(error.code, 2);
      assert.equal(error.stdout, '');
      assert.equal(error.stderr, usage);
      return true;
    });
  }
  const missing = join(tmpdir(), `simplestchat missing report ${process.pid}`, 'PRIVATE path');
  await assert.rejects(execute(binary, ['report', 'diagnostics', missing], { env }), error => {
    assert.equal(error.code, 1);
    assert.equal(error.stderr, '');
    const report = JSON.parse(error.stdout);
    assert.deepEqual(report.coverage.issues, ['file_unavailable', 'missing_summary']);
    assert.equal(error.stdout.includes('PRIVATE'), false);
    return true;
  });
});
