import assert from 'node:assert/strict';
import test from 'node:test';
import { correlateMediaDiagnostics, generatorConsumers, mediaReference } from '../../load_tests/media-diagnostic-report.mjs';

const salt = '0123456789abcdef0123456789abcdef';
const consumerId = '11111111-2222-4333-8444-555555555555';
const producerId = '66666666-7777-4888-8999-aaaaaaaaaaaa';
const transportId = 'bbbbbbbb-cccc-4ddd-8eee-ffffffffffff';

function fixture({ kind = 'audio', attempt = 1 } = {}) {
  const delivery = { consumerId, producerId, ssrc: 42, packetsBySecond: [5, 5, 5],
    eligibleSeconds: 3, secondsWithPackets: 3, longestGapSeconds: 0, passed: true, skippedShortLived: false };
  const generator = [{ clientId: 'PRIVATE-client', connectionAttempts: Array.from({ length: attempt }, () => ({})),
    consumerDelivery: [delivery], diagnostics: { failures: [], events: [
      { attempt, elapsedMs: 10, kind: 'consumer-created', details: { consumerId, producerId, ssrc: 42, kind } },
      { attempt, elapsedMs: 20, kind: 'resume-requested', details: { consumerId } },
      { attempt, elapsedMs: 30, kind: 'resume-ack', details: { consumerId } },
      { attempt, elapsedMs: 40, kind: 'track-first-rtp', details: { ssrc: 42 } },
    ] } }];
  const samples = [3000, 4500].map((scheduledElapsedMs, index) => {
    const ordinal = index + 1;
    return { schemaVersion: 1, ordinal, windowStartElapsedMs: 3000, windowEndElapsedMs: 6000,
      scheduledElapsedMs, startedElapsedMs: scheduledElapsedMs + 10, finishedElapsedMs: scheduledElapsedMs + 110,
      aliveAtStart: { server: true, generator: true }, aliveAtEnd: { server: true, generator: true }, status: 'ok',
      snapshot: { schemaVersion: 1, correlationSalt: salt, sampleId: ordinal, startedUs: ordinal * 100000,
        finishedUs: ordinal * 100000 + 100,
        coverage: { complete: true, registryBusy: false, participantsObserved: 1, participantsVisited: 1,
          busyParticipants: 0, participantLimitReached: false, entityLimitReached: false, deadlineReached: false },
        entities: [{ entityType: 'consumer', reference: mediaReference(salt, 'consumer', consumerId),
          transportReference: mediaReference(salt, 'transport', transportId), producerReference: mediaReference(salt, 'producer', producerId),
          kind, status: 'ok', paused: false, producerPaused: false, observedUs: ordinal * 100000 + 50,
          streams: [{ ssrc: 42, packetCount: ordinal * 10, rtpBytes: ordinal * 1000, workerTimestampMs: ordinal * 1000 }] }] } };
  });
  return { generator, delivery, samples, report: () => correlateMediaDiagnostics(samples, generator) };
}

function assertUnavailable(report) {
  assert.equal(report.coverage.available, false);
  assert.equal(report.coverage.complete, false);
  assert.ok(report.coverage.issues.includes('generator_correlation_unavailable'));
  assert.deepEqual(report.consumers, []);
  assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
}

test('legacy delivery metadata remains optional and does not imply per-attempt workload coverage', () => {
  const legacy = fixture().report();
  assert.equal(legacy.coverage.complete, true);
  assert.equal(legacy.consumers[0].generator.attemptOrdinal, 1);
  for (const metadata of [{ attempt: 1 }, { isAudio: true }, { attempt: 1, isAudio: true }]) {
    const f = fixture();
    Object.assign(f.delivery, metadata);
    assert.deepEqual(f.report(), legacy, 'additive identity metadata must not invent new coverage guarantees');
  }
});

test('delivery identity accepts matching audio/video metadata and one-based attempt boundaries', () => {
  for (const kind of ['audio', 'video']) for (const attempt of [1, 2, 128]) {
    const f = fixture({ kind, attempt });
    Object.assign(f.delivery, { attempt, isAudio: kind === 'audio' });
    const report = f.report();
    assert.equal(report.coverage.complete, true);
    assert.equal(report.consumers[0].kind, kind);
    assert.equal(report.consumers[0].generator.attemptOrdinal, attempt);
    assert.equal(report.consumers[0].generator.resumeAcknowledged, true);
  }
});

test('present attempt metadata rejects invalid types, bounds, and immutable creation mismatches', () => {
  for (const attempt of [undefined, null, 0, -1, 0.5, 'PRIVATE', true, {}, [], NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 3, 2]) {
    const f = fixture();
    f.generator[0].connectionAttempts.push({});
    f.delivery.attempt = attempt;
    assertUnavailable(f.report());
  }
});

test('present isAudio metadata rejects coercible values and creation-kind mismatches', () => {
  for (const isAudio of [undefined, null, 'PRIVATE', 'true', 1, 0, {}, [], false]) {
    const f = fixture();
    f.delivery.isAudio = isAudio;
    assertUnavailable(f.report());
  }
  const video = fixture({ kind: 'video' });
  video.delivery.isAudio = true;
  assertUnavailable(video.report());
});

test('valid additive delivery metadata cannot relax sparse native sample coverage', () => {
  const f = fixture();
  f.samples[0].snapshot.entities = [];
  f.samples[0].snapshot.coverage.participantsObserved = 0;
  f.samples[0].snapshot.coverage.participantsVisited = 0;
  const legacy = f.report();
  assert.equal(legacy.coverage.complete, false);
  assert.ok(legacy.coverage.issues.includes('consumer_counter_pair_missing'));
  Object.assign(f.delivery, { attempt: 1, isAudio: true });
  assert.deepEqual(f.report(), legacy);
});

test('diagnostic failures retain sanitized identity and native counter evidence with incomplete coverage', () => {
  const f = fixture();
  const complete = f.report();
  f.generator[0].diagnostics.failures.push('PRIVATE receiver-stall native detail');
  const report = f.report();
  assert.equal(report.coverage.available, true);
  assert.equal(report.coverage.complete, false);
  assert.deepEqual(report.coverage.issues, ['generator_diagnostics_incomplete']);
  assert.equal(report.coverage.expectedConsumers, 1);
  assert.equal(report.coverage.matchedConsumers, 1);
  assert.equal(report.coverage.consumersWithCounterPairs, 1);
  assert.deepEqual(report.samples, complete.samples);
  assert.deepEqual(report.consumers, complete.consumers);
  assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
});

test('identity extraction requires explicit opt-in to structurally valid diagnostic failures', () => {
  const f = fixture();
  const identities = generatorConsumers(f.generator);
  f.generator[0].diagnostics.failures.push('PRIVATE');
  for (const options of [undefined, {}, { allowDiagnosticFailures: false }, { allowDiagnosticFailures: 'true' }]) {
    assert.throws(() => generatorConsumers(f.generator, options), { message: 'invalid_generator_results' });
  }
  assert.deepEqual(generatorConsumers(f.generator, { allowDiagnosticFailures: true }), identities);
  assert.equal(JSON.stringify(identities).includes('PRIVATE'), false);
});

test('diagnostic failures have bounded string-only shape even when incomplete evidence is admitted', () => {
  for (const failures of [undefined, null, 'PRIVATE', {}, [null], [undefined], [42], [true], [{}], [[]],
    ['PRIVATE', { private: 'PRIVATE' }], ['PRIVATE'.repeat(586)], Array(4097).fill('PRIVATE')]) {
    const f = fixture();
    f.generator[0].diagnostics.failures = failures;
    assert.throws(() => generatorConsumers(f.generator, { allowDiagnosticFailures: true }), { message: 'invalid_generator_results' });
    assertUnavailable(f.report());
  }
  for (const failures of [['P'.repeat(4096)], Array(4096).fill('PRIVATE')]) {
    const f = fixture();
    f.generator[0].diagnostics.failures = failures;
    assert.equal(generatorConsumers(f.generator, { allowDiagnosticFailures: true }).length, 1);
    const report = f.report();
    assert.equal(report.coverage.available, true);
    assert.equal(report.coverage.complete, false);
    assert.deepEqual(report.coverage.issues, ['generator_diagnostics_incomplete']);
    assert.equal(JSON.stringify(report).includes('PRIVATE'), false);
  }
});

test('failed delivery remains failed while available diagnostic evidence is retained', () => {
  const f = fixture();
  Object.assign(f.delivery, { packetsBySecond: [0, 0, 0], secondsWithPackets: 0, longestGapSeconds: 3, passed: false });
  const failedReceive = f.report();
  assert.equal(failedReceive.coverage.complete, true, 'native coverage does not excuse the failed workload');
  assert.equal(failedReceive.consumers[0].generator.deliveryPassed, false);
  assert.equal(failedReceive.consumers[0].generator.packetsInMeasurement, 0);
  assert.equal(failedReceive.consumers[0].generator.longestGapSeconds, 3);
  assert.equal(failedReceive.consumers[0].nativeAccounting.movement, 'increased');
  f.generator[0].diagnostics.failures.push('PRIVATE stalled receiver capture timeout');
  const incomplete = f.report();
  assert.equal(incomplete.coverage.available, true);
  assert.equal(incomplete.coverage.complete, false);
  assert.deepEqual(incomplete.coverage.issues, ['generator_diagnostics_incomplete']);
  assert.deepEqual(incomplete.consumers, failedReceive.consumers);
  assert.equal(JSON.stringify(incomplete).includes('PRIVATE'), false);
});
