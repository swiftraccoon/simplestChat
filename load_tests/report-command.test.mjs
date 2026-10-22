import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readDiagnosticReportWithGenerator } from './report-command.mjs';

const report = complete => ({ schemaVersion: 1,
  coverage: { available: true, complete, scope: complete ? 'all_emitted_records' : 'recorded_subset', issues: complete ? [] : ['missing_summary'] },
  records: 1, summary: complete ? { schemaVersion: 1, kind: 'summary', accepted: 1, written: 1, dropped: 0, expired: 0, unfinished: 0, writeFailed: false } : null,
  outcomeCounts: { ok: 1, error: 0, completed: 0, cancelled: 0, timeout: 0, rejected: 0 },
  operations: [{ operation: 'join_room', outcome: 'ok', count: 1, p50Ms: 1, p95Ms: 1, p99Ms: 1 }], stages: [],
  interpretation: 'Instrumented records over the recorder lifetime, including setup and cleanup; not the shared media measurement interval. Subsets are not random samples. Stage durations may overlap and must not be added to operation durations.',
  percentiles: 'nearest-rank from individual elapsedUs values, converted to milliseconds' });

async function executable(t, body) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat report command '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const binary = join(directory, 'owned generator');
  await writeFile(binary, `#!/usr/bin/env node\n${body}\n`, { mode: 0o700 });
  return binary;
}

test('explicit generator command receives literal report argv without a shell', async t => {
  const path = '--private path $(echo SECRET); `false`';
  const expected = report(true);
  const binary = await executable(t, `require('node:assert/strict').deepEqual(process.argv.slice(2), ${JSON.stringify(['report', 'diagnostics', path])}); process.stdout.write(${JSON.stringify(JSON.stringify(expected))});`);
  assert.deepEqual(await readDiagnosticReportWithGenerator(binary, path), expected);
});

test('exit1 preserves incomplete report evidence for the existing coverage judgement', async t => {
  const expected = report(false);
  const binary = await executable(t, `process.stdout.write(${JSON.stringify(JSON.stringify(expected))}); process.stderr.write('PRIVATE credentials'); process.exitCode = 1;`);
  assert.deepEqual(await readDiagnosticReportWithGenerator(binary, '/owned recorder'), expected);
});

test('process and malformed output errors do not retain child output or paths', async t => {
  for (const body of [
    "process.stderr.write('PRIVATE credentials'); process.exitCode = 2;",
    "process.stdout.write('PRIVATE record contents');",
    `process.stdout.write(${JSON.stringify(JSON.stringify(report(true)))}); process.exitCode = 1;`,
    "process.stdout.write('{}');",
    "process.stdout.write('x'.repeat(2 * 1024 * 1024 + 1));",
  ]) {
    const binary = await executable(t, body);
    await assert.rejects(readDiagnosticReportWithGenerator(binary, '/PRIVATE account path'), error => {
      assert.equal(error.message, 'diagnostic_report_command_failed');
      assert.equal(error.cause, undefined);
      assert.equal(String(error).includes('PRIVATE'), false);
      return true;
    });
  }
  await assert.rejects(readDiagnosticReportWithGenerator('/PRIVATE nonexistent executable', '/owned recorder'), { message: 'diagnostic_report_command_failed' });
});

test('unknown keys and vocabulary at every report boundary cannot enter retained artifacts', async t => {
  const sentinel = 'PRIVATE credentials room payload';
  for (const mutate of [
    value => { value.privatePayload = sentinel; },
    value => { value.coverage.privatePayload = sentinel; },
    value => { value.coverage.issues = [sentinel]; value.coverage.complete = false; value.coverage.scope = 'recorded_subset'; },
    value => { value.summary.privatePayload = sentinel; },
    value => { value.outcomeCounts.privatePayload = sentinel; },
    value => { value.operations[0].privatePayload = sentinel; },
    value => { value.operations[0].operation = sentinel; },
    value => { value.operations[0].outcome = sentinel; },
    value => { value.stages = [{ ...value.operations[0], stage: sentinel }]; value.operations = []; },
    value => { value.stages = [{ ...value.operations[0], stage: 'dispatch', privatePayload: sentinel }]; value.operations = []; },
    value => { value.interpretation = sentinel; },
    value => { value.percentiles = sentinel; },
    value => { value.operations[0].count = Number.MAX_SAFE_INTEGER; },
    value => { value.operations[0].p99Ms = Number.MAX_SAFE_INTEGER; },
    value => { value.summary.dropped = -1; },
    value => { value.summary = null; },
    value => { value.summary.accepted = 2; },
    value => { value.records = -1; },
    value => { value.operations[0].p50Ms = -1; },
  ]) {
    const value = report(true); mutate(value);
    const binary = await executable(t, `process.stdout.write(${JSON.stringify(JSON.stringify(value))}); process.exitCode = ${value.coverage.complete ? 0 : 1};`);
    await assert.rejects(readDiagnosticReportWithGenerator(binary, '/PRIVATE recorder'), error => {
      assert.equal(error.message, 'diagnostic_report_command_failed');
      assert.equal(error.cause, undefined);
      assert.equal(String(error).includes('PRIVATE'), false);
      return true;
    });
  }
});
