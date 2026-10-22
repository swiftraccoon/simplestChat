import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { readLifecycleReport } from '../../load_tests/lifecycle-diagnostic-report.mjs';
import { readLifecycleReportWithGenerator } from '../../load_tests/lifecycle-report-command.mjs';
import { lifecycleFixture } from '../../load_tests/lifecycle-report-fixtures.mjs';

async function fixture(t, complete = true) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat lifecycle command '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, '--PRIVATE log');
  const sample = lifecycleFixture();
  const lines = complete ? sample.lines : sample.lines.filter(line => !line.includes('media_cleanup_finished'));
  await writeFile(path, lines.join('\n') + '\n', { mode: 0o600 });
  const report = await readLifecycleReport(path, sample.timeline, { expectedParticipants: 1 });
  return { directory, path, sample, report };
}

async function executable(f, body) {
  const path = join(f.directory, `generator ${Math.random()}`);
  await writeFile(path, '#!/usr/bin/env node\n' + body + '\n', { mode: 0o700 });
  return path;
}

function server(f, value, assertions = '') {
  return `let input = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', chunk => { input += chunk; }); process.stdin.on('end', () => { ${assertions} process.stdout.write(${JSON.stringify(JSON.stringify(value))}); process.exitCode = ${value.coverage.complete ? 0 : 1}; });`;
}

test('lifecycle command keeps timeline off literal argv and closes the owned stdin pipe', async t => {
  const f = await fixture(t);
  const assertions = `const assert = require('node:assert/strict'); assert.deepEqual(process.argv.slice(2), ${JSON.stringify(['report', 'lifecycle', f.path, '--expected-participants', '1'])}); assert.deepEqual(JSON.parse(input), ${JSON.stringify(f.sample.timeline)});`;
  const binary = await executable(f, server(f, f.report, assertions));
  assert.deepEqual(await readLifecycleReportWithGenerator(binary, f.path, f.sample.timeline, { expectedParticipants: 1 }), f.report);
});

test('incomplete lifecycle reports remain evidence when the reader exits1', async t => {
  const f = await fixture(t, false);
  const binary = await executable(f, server(f, f.report));
  assert.deepEqual(await readLifecycleReportWithGenerator(binary, f.path, f.sample.timeline, { expectedParticipants: 1 }), f.report);
});

test('unknown lifecycle payloads, nested fields and vocabularies never enter artifacts', async t => {
  const f = await fixture(t);
  const sentinel = 'PRIVATE credentials raw log user';
  for (const mutate of [
    value => { value.privatePayload = sentinel; },
    value => { value.coverage.privatePayload = sentinel; },
    value => { value.timeline.privatePayload = sentinel; },
    value => { value.timeline.events[0].privatePayload = sentinel; },
    value => { value.timeline.events[0].at = sentinel; },
    value => { value.timeline.nominalMeasurement.privatePayload = sentinel; },
    value => { value.lifecycleEvents[0].participantId = sentinel; },
    value => { value.lifecycleEvents[0].event = sentinel; },
    value => { value.lifecycleEvents[0].phase = sentinel; },
    value => { value.lifecycleEvents[1].state = sentinel; },
    value => { value.clamps[0].privatePayload = sentinel; },
    value => { value.clamps[0].previouslyObserved.privatePayload = sentinel; },
    value => { value.phases.setup.privatePayload = sentinel; },
    value => { value.totals.privatePayload = sentinel; },
    value => { value.interpretation = sentinel; },
    value => { value.totals.participantsWithCleanupFinished = 0; },
  ]) {
    const report = structuredClone(f.report); mutate(report);
    const binary = await executable(f, server(f, report));
    await assert.rejects(readLifecycleReportWithGenerator(binary, f.path, f.sample.timeline, { expectedParticipants: 1 }), error => {
      assert.equal(error.message, 'lifecycle_report_command_failed'); assert.equal(error.cause, undefined);
      assert.equal(String(error).includes('PRIVATE'), false); return true;
    });
  }
});

test('spawn, child output and timeline serialization failures use fixed errors', async t => {
  const f = await fixture(t);
  for (const body of ["process.stderr.write('PRIVATE credentials'); process.exitCode = 2;", "process.stdout.write('PRIVATE invalid output');", "process.stdout.write('x'.repeat(16 * 1024 * 1024 + 1));"]) {
    const binary = await executable(f, body);
    await assert.rejects(readLifecycleReportWithGenerator(binary, f.path, f.sample.timeline, { expectedParticipants: 1 }), { message: 'lifecycle_report_command_failed' });
  }
  await assert.rejects(readLifecycleReportWithGenerator('/PRIVATE absent generator', f.path, f.sample.timeline, { expectedParticipants: 1 }), { message: 'lifecycle_report_command_failed' });
  await assert.rejects(readLifecycleReportWithGenerator('unused', f.path, { private: 'x'.repeat(65536) }, { expectedParticipants: 1 }), { message: 'lifecycle_report_command_failed' });
});
