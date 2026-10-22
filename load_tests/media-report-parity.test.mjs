// Explicit native gate. Ordinary helper checks never compile or select a binary.
import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, open, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { correlateMediaDiagnostics } from './media-diagnostic-report.mjs';
import { correlateReceiverStalls } from './receiver-stall-report.mjs';
import {
  readMediaReportWithGenerator,
  readReceiverStallReportWithGenerator,
} from './media-report-command.mjs';
import { mediaFixture, receiverStallFixture } from './media-report-fixtures.mjs';

const binary = process.env.LOAD_TEST_REPORT_BINARY;
const execute = promisify(execFile);
const wire = (value) => JSON.parse(JSON.stringify(value));
async function owned(t) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat media parity '));
  await chmod(directory, 0o700);
  t.after(() => rm(directory, { recursive: true, force: true }));
  return { directory, path: join(directory, '--PRIVATE results.json') };
}
async function compare(path, fixture, mode = 'media', admitted = true) {
  const samples = wire(fixture.samples);
  const results = wire(fixture.generator);
  await writeFile(path, JSON.stringify(results), { mode: 0o600 });
  const oracle = mode === 'media' ? correlateMediaDiagnostics : correlateReceiverStalls;
  const read =
    mode === 'media' ? readMediaReportWithGenerator : readReceiverStallReportWithGenerator;
  const expected = wire(oracle(samples, admitted ? results : null));
  const actual = await read(binary, path, samples, { admitted });
  assert.deepEqual(actual, expected);
  assert.equal(JSON.stringify(actual).includes('PRIVATE'), false);
  for (const row of results ?? [])
    for (const delivery of row.consumerDelivery ?? []) {
      for (const id of [delivery.consumerId, delivery.producerId])
        if (typeof id === 'string' && id.length > 10) {
          assert.equal(JSON.stringify(actual).includes(id), false);
        }
    }
  return actual;
}

test(
  'native media matches valid, incomplete, counter, schedule and privacy oracle',
  { skip: !binary },
  async (t) => {
    const f = await owned(t);
    const cases = [
      ['complete', () => {}],
      ['diagnostic-failure', (f) => f.diagnostics.failures.push('PRIVATE diagnostic failure')],
      [
        'no-results',
        (f) => {
          f.generator = null;
        },
      ],
      [
        'empty-results',
        (f) => {
          f.generator = [];
        },
      ],
      ['one-sample', (f) => f.samples.splice(1)],
      [
        'no-samples',
        (f) => {
          f.samples = [];
        },
      ],
      ['extra-sample', (f) => f.samples.push(structuredClone(f.samples[2]))],
      [
        'null-samples',
        (f) => {
          f.samples = null;
        },
      ],
      [
        'sample-order',
        (f) => {
          f.samples[1].ordinal = 1;
        },
      ],
      [
        'sample-id-order',
        (f) => {
          f.samples[1].snapshot.sampleId = 1;
        },
      ],
      [
        'mixed-namespace',
        (f) => {
          f.samples[1].snapshot.correlationSalt = 'f'.repeat(32);
        },
      ],
      [
        'schedule-mismatch',
        (f) => {
          f.samples[1].scheduledElapsedMs++;
        },
      ],
      [
        'window-mismatch',
        (f) => {
          f.samples[1].windowEndElapsedMs++;
        },
      ],
      [
        'clock-fraction',
        (f) => {
          f.samples[1].startedElapsedMs += 0.125;
        },
      ],
      [
        'roundtrip-decimal-timing',
        (f) => {
          f.samples[0].startedElapsedMs = 3100.3940620683134;
          f.samples[0].finishedElapsedMs = 3200.3940620683134;
          f.samples[2].startedElapsedMs = 9052.637203851715;
          f.samples[2].finishedElapsedMs = 9093.985276597552;
        },
      ],
      [
        'early-start',
        (f) => {
          f.samples[1].startedElapsedMs = 6999;
        },
      ],
      [
        'late-start',
        (f) => {
          f.samples[1].startedElapsedMs = 8050;
          f.samples[1].finishedElapsedMs = 8100;
        },
      ],
      [
        'finished-before-start',
        (f) => {
          f.samples[1].finishedElapsedMs = 6999;
        },
      ],
      [
        'finished-at-teardown',
        (f) => {
          f.samples[1].finishedElapsedMs = 11000;
        },
      ],
      [
        'dead-generator',
        (f) => {
          f.samples[1].aliveAtEnd.generator = false;
        },
      ],
      [
        'missed-sample',
        (f) => {
          f.samples[1].status = 'missed';
          f.samples[1].startedElapsedMs = null;
        },
      ],
      [
        'error-sample',
        (f) => {
          f.samples[1].status = 'error';
        },
      ],
      [
        'persistence-issue',
        (f) => {
          f.samples[1].persistenceIssue = 'PRIVATE';
        },
      ],
      [
        'empty-issue-marker',
        (f) => {
          f.samples[1].timingIssue = '';
        },
      ],
      [
        'object-issue-marker',
        (f) => {
          f.samples[1].timingIssue = {};
        },
      ],
      [
        'unknown-snapshot-field',
        (f) => {
          f.samples[1].snapshot.private = 'PRIVATE';
        },
      ],
      [
        'native-busy',
        (f) => {
          f.samples[1].snapshot.coverage.complete = false;
          f.samples[1].snapshot.coverage.registryBusy = true;
        },
      ],
      [
        'missing-consumer',
        (f) => {
          f.samples[1].snapshot.entities.splice(0, 1);
        },
      ],
      ['all-consumers-missing', (f) => f.samples.forEach((s) => s.snapshot.entities.splice(0, 1))],
      [
        'short-lived-unobserved',
        (f) => {
          f.delivery.skippedShortLived = true;
          f.samples.forEach((s) => s.snapshot.entities.splice(0, 1));
        },
      ],
      [
        'wrong-producer',
        (f) => {
          f.samples[1].snapshot.entities[0].producerReference = 'f'.repeat(64);
        },
      ],
      [
        'wrong-kind',
        (f) => {
          f.samples[1].snapshot.entities[0].kind = 'audio';
        },
      ],
      [
        'wrong-ssrc',
        (f) => {
          f.samples[1].snapshot.entities[0].streams[0].ssrc++;
        },
      ],
      ['missing-created-event', (f) => f.diagnostics.events.shift()],
      [
        'duplicate-created-event',
        (f) => f.diagnostics.events.push(structuredClone(f.diagnostics.events[0])),
      ],
      [
        'unknown-event-private-details',
        (f) =>
          f.diagnostics.events.push({
            attempt: 1,
            elapsedMs: 1,
            kind: 'PRIVATE',
            details: { sdp: 'PRIVATE' },
          }),
      ],
      [
        'number-shaped-private-object-retains-object-semantics',
        (f) => {
          f.generator[0].ignored = { '$serde_json::private::Number': '1e400' };
          f.samples[0].timingIssue = { '$serde_json::private::Number': '0' };
        },
      ],
      [
        'wrong-event-attempt',
        (f) => {
          f.diagnostics.events[0].attempt = 2;
        },
      ],
      [
        'second-attempt',
        (f) => {
          f.generator[0].connectionAttempts.push({});
          f.delivery.attempt = 2;
          f.diagnostics.events.forEach((e) => {
            e.attempt = 2;
          });
        },
      ],
      [
        'delivery-failure',
        (f) => {
          f.delivery.passed = false;
          f.delivery.packetsBySecond.fill(0);
          f.delivery.secondsWithPackets = 0;
        },
      ],
      [
        'delivery-overflow',
        (f) => {
          f.delivery.packetsBySecond = [Number.MAX_SAFE_INTEGER, 1];
        },
      ],
      ['invalid-diagnostic-reason', (f) => f.diagnostics.failures.push({ private: 'PRIVATE' })],
      ['private-utf16-reason', (f) => f.diagnostics.failures.push('𝄞'.repeat(2049))],
      [
        'invalid-eligible',
        (f) => {
          f.delivery.eligibleSeconds = 9;
        },
      ],
    ];
    for (const status of [
      'closed',
      'no_streams',
      'timeout',
      'error',
      'stream_limit',
      'not_collected',
    ]) {
      cases.push([
        `native-${status}`,
        (f) => {
          const snapshot = f.samples[1].snapshot;
          snapshot.coverage.complete = false;
          snapshot.entities[0].status = status;
          snapshot.entities[0].streams = [];
        },
      ]);
    }
    for (const field of ['packetCount', 'rtpBytes', 'workerTimestampMs'])
      cases.push([
        `counter-reset-${field}`,
        (f) => {
          f.samples[1].snapshot.entities[0].streams[0][field] = 0;
        },
      ]);
    for (const value of [null, 'f'.repeat(64)])
      cases.push([
        `transport-${value}`,
        (f) => {
          f.samples[1].snapshot.entities[0].transportReference = value;
        },
      ]);
    for (const count of [0, 100])
      cases.push([
        `flat-${count}`,
        (f) =>
          f.samples.forEach((s) => {
            Object.assign(s.snapshot.entities[0].streams[0], {
              packetCount: count,
              rtpBytes: count * 100,
            });
          }),
      ]);
    for (const value of [-1, 0.5, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1])
      cases.push([
        `packet-${value}`,
        (f) => {
          f.samples[1].snapshot.entities[0].streams[0].packetCount = value;
        },
      ]);
    for (const [name, mutate] of [
      [
        'snapshot-schema',
        (s) => {
          s.schemaVersion = 3;
        },
      ],
      [
        'snapshot-start-order',
        (s) => {
          s.finishedUs = s.startedUs - 1;
        },
      ],
      [
        'snapshot-zero-id',
        (s) => {
          s.sampleId = 0;
        },
      ],
      [
        'coverage-unknown-key',
        (s) => {
          s.coverage.private = 'PRIVATE';
        },
      ],
      [
        'coverage-invalid-bool',
        (s) => {
          s.coverage.registryBusy = 0;
        },
      ],
      [
        'coverage-inventory-limit',
        (s) => {
          s.coverage.participantsVisited = 65;
        },
      ],
      [
        'coverage-observed-mismatch',
        (s) => {
          s.coverage.participantsObserved = 0;
        },
      ],
      [
        'coverage-complete-busy',
        (s) => {
          s.coverage.busyParticipants = 1;
        },
      ],
      [
        'entity-extra-key',
        (s) => {
          s.entities[0].address = 'PRIVATE';
        },
      ],
      [
        'entity-invalid-kind',
        (s) => {
          s.entities[0].kind = 'PRIVATE';
        },
      ],
      [
        'entity-invalid-reference',
        (s) => {
          s.entities[0].reference = 'PRIVATE';
        },
      ],
      [
        'entity-invalid-paused',
        (s) => {
          s.entities[0].paused = 0;
        },
      ],
      [
        'entity-observed-before',
        (s) => {
          s.entities[0].observedUs = s.startedUs - 1;
        },
      ],
      [
        'entity-observed-after',
        (s) => {
          s.entities[0].observedUs = s.finishedUs + 1;
        },
      ],
      [
        'entity-duplicate',
        (s) => {
          s.entities.push(structuredClone(s.entities[0]));
        },
      ],
      [
        'stream-extra-key',
        (s) => {
          s.entities[0].streams[0].nativeId = 'PRIVATE';
        },
      ],
      [
        'stream-duplicate-ssrc',
        (s) => {
          s.entities[0].streams.push(structuredClone(s.entities[0].streams[0]));
        },
      ],
      [
        'stream-ssrc-overflow',
        (s) => {
          s.entities[0].streams[0].ssrc = 2 ** 32;
        },
      ],
      [
        'stream-bytes-negative',
        (s) => {
          s.entities[0].streams[0].rtpBytes = -1;
        },
      ],
      [
        'stream-clock-fraction',
        (s) => {
          s.entities[0].streams[0].workerTimestampMs = 0.1;
        },
      ],
      [
        'stream-empty-ok',
        (s) => {
          s.entities[0].streams = [];
        },
      ],
    ])
      cases.push([name, (f) => mutate(f.samples[1].snapshot)]);
    for (const [name, field, value] of [
      ['v2-score-low', 'score', -1],
      ['v2-score-high', 'score', 11],
      ['v2-score-fraction', 'score', 0.5],
      ['v2-score-string', 'score', '10'],
      ['v2-consumer-encoding', 'encodingIndex', 0],
    ])
      cases.push([
        name,
        (f) => {
          const snapshot = f.samples[1].snapshot;
          snapshot.schemaVersion = 2;
          for (const entity of snapshot.entities)
            for (const stream of entity.streams) {
              Object.assign(stream, { score: 10, encodingIndex: null });
            }
          snapshot.entities[0].streams[0][field] = value;
        },
      ]);
    for (const [name, mutate] of cases)
      await t.test(name, async () => {
        const fixture = mediaFixture();
        mutate(fixture);
        await compare(f.path, fixture);
      });
    await t.test('owner-rejected-results-stay-unavailable', () =>
      compare(f.path, mediaFixture(), 'media', false),
    );
  },
);

test(
  'native receiver stalls match capture, independent clocks, producer and bounds oracle',
  { skip: !binary },
  async (t) => {
    const f = await owned(t);
    const cases = [
      ['complete', () => {}],
      [
        'legacy',
        (f) => {
          f.diagnostics.events.splice(4);
          delete f.diagnostics.receiverStalls;
        },
      ],
      ['diagnostic-failure', (f) => f.diagnostics.failures.push('PRIVATE')],
      ['missing-trigger', (f) => f.diagnostics.events.splice(4, 1)],
      ['missing-completion', (f) => f.diagnostics.events.pop()],
      [
        'missing-snapshot',
        (f) => {
          f.diagnostics.receiverStalls = [];
        },
      ],
      ['duplicate-trigger', (f) => f.diagnostics.events.push(structuredClone(f.triggerEvent))],
      ['duplicate-completion', (f) => f.diagnostics.events.push(structuredClone(f.completion))],
      [
        'wrong-trigger-ssrc',
        (f) => {
          f.trigger.ssrc++;
          f.triggerEvent.details.trigger.ssrc++;
        },
      ],
      [
        'wrong-trigger-duration',
        (f) => {
          f.triggerEvent.details.trigger.endBucket--;
        },
      ],
      [
        'wrong-trigger-buckets',
        (f) => {
          f.delivery.packetsBySecond[4] = 1;
        },
      ],
      [
        'wrong-trigger-identity',
        (f) => {
          f.triggerEvent.details.trigger.consumerOrdinal = 2;
        },
      ],
      [
        'unknown-trigger-field',
        (f) => {
          f.triggerEvent.details.private = 'PRIVATE';
        },
      ],
      [
        'unknown-capture-field',
        (f) => {
          f.receiverSnapshot.private = 'PRIVATE';
        },
      ],
      [
        'wrong-capture-trigger',
        (f) => {
          f.receiverSnapshot.details.trigger.beginBucket--;
        },
      ],
      [
        'wrong-capture-direction',
        (f) => {
          f.receiverSnapshot.details.snapshot.transports[0].direction = 'send';
        },
      ],
      [
        'missing-transport-stats',
        (f) => {
          f.receiverSnapshot.details.snapshot.transports[0].stats = [];
        },
      ],
      [
        'wrong-capture-mapping',
        (f) => {
          f.receiverSnapshot.details.snapshot.transports[0].consumerMappings[0].ssrc++;
        },
      ],
      [
        'raw-rtc-private-fields',
        (f) => {
          Object.assign(f.receiverSnapshot.details.snapshot.transports[0].stats[0], {
            address: 'PRIVATE',
            candidate: 'PRIVATE',
            sdp: 'PRIVATE',
          });
        },
      ],
      [
        'clock-shift',
        (f) =>
          f.samples.forEach((s) => {
            s.snapshot.startedUs += 5000000;
            s.snapshot.finishedUs += 5000000;
            s.snapshot.entities.forEach((e) => {
              e.observedUs += 5000000;
              e.streams.forEach((stream) => {
                stream.workerTimestampMs += 700000;
              });
            });
          }),
      ],
      ['missing-producer', (f) => f.samples.forEach((s) => s.snapshot.entities.splice(1))],
      [
        'wrong-producer-kind',
        (f) => {
          f.samples[1].snapshot.entities[1].kind = 'audio';
        },
      ],
      [
        'producer-transport-replacement',
        (f) => {
          f.samples[1].snapshot.entities[1].transportReference = 'f'.repeat(64);
        },
      ],
      [
        'producer-missing-identity',
        (f) => {
          f.samples[1].snapshot.entities[1].transportReference = null;
        },
      ],
      [
        'producer-stream-set',
        (f) => {
          f.samples[1].snapshot.entities[1].streams.pop();
        },
      ],
      [
        'producer-aggregate-overflow',
        (f) =>
          f.samples.forEach((s) => {
            s.snapshot.entities[1].streams.forEach((stream) => {
              stream.packetCount = Number.MAX_SAFE_INTEGER;
            });
          }),
      ],
    ];
    for (const status of [
      'busy',
      'budget_exhausted',
      'timed_out',
      'attempt_ended',
      'unavailable',
      'snapshot_rejected',
      'cancelled',
    ]) {
      cases.push([
        `capture-${status}`,
        (f) => {
          f.completion.details.status = status;
          f.diagnostics.receiverStalls = [];
        },
      ]);
    }
    for (const field of ['packetCount', 'rtpBytes', 'workerTimestampMs'])
      cases.push([
        `producer-reset-${field}`,
        (f) => {
          f.samples[1].snapshot.entities[1].streams[0][field] = 0;
        },
      ]);
    for (const version of [1, 2])
      cases.push([
        `schema-${version}`,
        (f) =>
          f.samples.forEach((s) => {
            s.snapshot.schemaVersion = version;
            if (version === 2)
              s.snapshot.entities.forEach((e) =>
                e.streams.forEach((stream, index) => {
                  Object.assign(stream, {
                    score: 10,
                    encodingIndex: e.entityType === 'producer' ? index : null,
                  });
                }),
              );
          }),
      ]);
    for (const delta of [-1, 0, 1])
      cases.push([
        `receiver-snapshot-byte-bound-${delta}`,
        (f) => {
          const snapshot = f.receiverSnapshot.details.snapshot;
          snapshot.private = { fraction: 0.000001, content: '' };
          const size = Buffer.byteLength(JSON.stringify(f.receiverSnapshot));
          snapshot.private.content = 'x'.repeat(256 * 1024 + delta - size);
        },
      ]);
    for (const [name, mutate] of cases)
      await t.test(name, async () => {
        const fixture = receiverStallFixture();
        mutate(fixture);
        await compare(f.path, fixture, 'receiver-stall');
      });
  },
);

test(
  'result file guards and owner admission produce fixed unavailable reports',
  { skip: !binary },
  async (t) => {
    const f = await owned(t);
    const fixture = receiverStallFixture();
    await writeFile(f.path, JSON.stringify(fixture.generator), { mode: 0o600 });
    const linked = join(f.directory, 'linked');
    await symlink(f.path, linked);
    const fifo = join(f.directory, 'fifo');
    await execute('mkfifo', [fifo]);
    const large = join(f.directory, 'large');
    const file = await open(large, 'w');
    await file.truncate(32 * 1024 * 1024 + 1);
    await file.close();
    for (const path of [linked, fifo, large, f.directory, join(f.directory, 'missing')]) {
      for (const [read, oracle] of [
        [readMediaReportWithGenerator, correlateMediaDiagnostics],
        [readReceiverStallReportWithGenerator, correlateReceiverStalls],
      ]) {
        assert.deepEqual(
          await read(binary, path, fixture.samples),
          wire(oracle(fixture.samples, null)),
        );
      }
    }
    const deeplyNested = structuredClone(fixture.generator);
    let nested = deeplyNested[0];
    for (let depth = 0; depth < 130; depth++) nested = nested.ignored = {};
    const deepInput = JSON.stringify(deeplyNested);
    for (const raw of [
      'PRIVATE not JSON',
      '{"private":"\\ud800"}',
      JSON.stringify(fixture.generator).replace('"PRIVATE-client"', '1e400'),
      deepInput,
    ]) {
      await writeFile(f.path, raw);
      assert.deepEqual(
        await readMediaReportWithGenerator(binary, f.path, fixture.samples),
        wire(correlateMediaDiagnostics(fixture.samples, null)),
      );
    }
    // Deliberate representable-JSON boundary: JS ignores this overflow field, while
    // Rust refuses the whole result rather than unifying server JSON features.
    const overflow = JSON.stringify(fixture.generator).replace('"PRIVATE-client"', '1e400');
    assert.equal(
      correlateMediaDiagnostics(fixture.samples, JSON.parse(overflow)).coverage.available,
      true,
    );
    assert.equal(
      correlateMediaDiagnostics(fixture.samples, JSON.parse(deepInput)).coverage.available,
      true,
    );
  },
);

test(
  'media commands reject usage and bounded input without constructing a runtime',
  { skip: !binary },
  async (t) => {
    const f = await owned(t);
    const fixture = mediaFixture();
    await writeFile(f.path, JSON.stringify(fixture.generator), { mode: 0o600 });
    for (const mode of ['media', 'receiver-stall']) {
      const help = await execute(binary, ['report', mode, '--help'], {
        env: { ...process.env, TOKIO_WORKER_THREADS: '0' },
      });
      assert.match(help.stdout, /RESULTS_PATH/);
      await assert.rejects(
        execute(binary, ['report', mode], { env: { ...process.env, TOKIO_WORKER_THREADS: '0' } }),
        { code: 2 },
      );
    }
    for (const input of [
      'x'.repeat(12 * 1024 * 1024 + 1),
      '{"PRIVATE":"\\ud800"}',
      '{"PRIVATE":1e400}',
    ]) {
      const child = spawn(binary, ['report', 'media', f.path], {
        env: { ...process.env, TOKIO_WORKER_THREADS: '0' },
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', (chunk) => {
        stdout += chunk;
      });
      child.stderr.on('data', (chunk) => {
        stderr += chunk;
      });
      child.stdin.on('error', () => {});
      child.stdin.end(input);
      const result = await new Promise((resolve) =>
        child.on('close', (code, signal) => resolve({ code, signal })),
      );
      assert.deepEqual(result, { code: 1, signal: null });
      assert.equal(stderr, '');
      assert.deepEqual(
        JSON.parse(stdout),
        wire(correlateMediaDiagnostics(null, fixture.generator)),
      );
    }
  },
);
