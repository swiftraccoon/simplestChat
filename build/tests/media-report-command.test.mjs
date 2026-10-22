import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import {
  readMediaReportWithGenerator,
  readReceiverStallReportWithGenerator,
} from '../../load_tests/media-report-command.mjs';
import { mediaFixture, receiverStallFixture } from '../../load_tests/media-report-fixtures.mjs';

async function executable(
  t,
  report,
  mode,
  samples,
  { admitted = true, status = report.coverage.complete === true ? 0 : 1, body } = {},
) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat media command '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, '--PRIVATE results.json');
  const binary = join(directory, 'generator file');
  const source =
    body ??
    `const assert = require('node:assert/strict'); let input = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', chunk => input += chunk); process.stdin.on('end', () => {
    assert.deepEqual(process.argv.slice(2), ${JSON.stringify(['report', mode, path, ...(admitted ? [] : ['--results-unavailable'])])});
    assert.deepEqual(JSON.parse(input), ${JSON.stringify(samples)});
    assert.equal(process.env.PRIVATE_TEST_TOKEN, undefined);
    process.stdout.write(${JSON.stringify(JSON.stringify(report))}); process.exitCode = ${status};
  });`;
  await writeFile(binary, '#!/usr/bin/env node\n' + source, { mode: 0o700 });
  return { binary, path };
}

test('media reader passes literal results path, closes bounded stdin and retains complete reports', async (t) => {
  const f = mediaFixture();
  const report = f.report();
  const child = await executable(t, report, 'media', f.samples);
  assert.deepEqual(await readMediaReportWithGenerator(child.binary, child.path, f.samples), report);
});

test('receiver reader keeps incomplete and unsupported legacy evidence distinct', async (t) => {
  for (const legacy of [false, true]) {
    const f = receiverStallFixture();
    if (legacy) {
      f.diagnostics.events.splice(4);
      delete f.diagnostics.receiverStalls;
    } else f.completion.details.status = 'timed_out';
    const report = f.report();
    assert.equal(report.coverage.complete, legacy ? null : false);
    const child = await executable(t, report, 'receiver-stall', f.samples, { admitted: false });
    assert.deepEqual(
      await readReceiverStallReportWithGenerator(child.binary, child.path, f.samples, {
        admitted: false,
      }),
      report,
    );
  }
});

test('unknown child report fields, nested payloads and private vocabulary are refused', async (t) => {
  for (const [mode, make, read, mutations] of [
    [
      'media',
      mediaFixture,
      readMediaReportWithGenerator,
      [
        (r) => {
          r.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.coverage.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.coverage.issues.push('PRIVATE');
        },
        (r) => {
          r.samples[0].privatePayload = 'PRIVATE';
        },
        (r) => {
          r.samples[0].aliveAtEnd.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.samples[0].status = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].privatePayload = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].generator.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].observations[0].privatePayload = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].observations[0].stream.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].nativeAccounting.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].consumerReference = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].nativeAccounting.movement = 'PRIVATE';
        },
        (r) => {
          r.consumers[0].generator.packetsInMeasurement = Number.MAX_SAFE_INTEGER + 1;
        },
      ],
    ],
    [
      'receiver-stall',
      receiverStallFixture,
      readReceiverStallReportWithGenerator,
      [
        (r) => {
          r.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.coverage.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].trigger.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].receiverCapture.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].delivery.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].consumer.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].producer.privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].producer.observations[0].streams[0].privatePayload = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].receiverCapture.peerState = 'PRIVATE';
        },
        (r) => {
          r.stalls[0].issues.push('PRIVATE');
        },
        (r) => {
          r.interpretation = 'PRIVATE';
        },
      ],
    ],
  ]) {
    for (const mutate of mutations) {
      const f = make();
      const report = f.report();
      mutate(report);
      const child = await executable(t, report, mode, f.samples);
      await assert.rejects(read(child.binary, child.path, f.samples), (error) => {
        assert.equal(error.message, 'media_report_command_failed');
        assert.equal(error.cause, undefined);
        assert.equal(String(error).includes('PRIVATE'), false);
        return true;
      });
    }
  }
});

test('child status, spawn failures and input serialization never expose raw errors', async (t) => {
  const f = mediaFixture();
  const report = f.report();
  for (const body of [
    "process.stderr.write('PRIVATE credentials'); process.exitCode = 2;",
    "process.stdout.write('PRIVATE malformed JSON');",
  ]) {
    const child = await executable(t, report, 'media', f.samples, { body });
    await assert.rejects(readMediaReportWithGenerator(child.binary, child.path, f.samples), {
      message: 'media_report_command_failed',
    });
  }
  const wrongStatus = await executable(t, report, 'media', f.samples, { status: 1 });
  await assert.rejects(
    readMediaReportWithGenerator(wrongStatus.binary, wrongStatus.path, f.samples),
    { message: 'media_report_command_failed' },
  );
  await assert.rejects(
    readMediaReportWithGenerator('/PRIVATE missing', '/PRIVATE missing', f.samples),
    { message: 'media_report_command_failed' },
  );
  const circular = {};
  circular.private = circular;
  for (const samples of [circular, { private: 'x'.repeat(12 * 1024 * 1024) }]) {
    await assert.rejects(readMediaReportWithGenerator('unused', 'unused', samples), {
      message: 'media_report_command_failed',
    });
  }
});
