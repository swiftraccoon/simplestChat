import assert from 'node:assert/strict';
import test from 'node:test';
import { mediaReference } from '../../load_tests/media-diagnostic-report.mjs';
import { correlateReceiverStalls } from '../../load_tests/receiver-stall-report.mjs';

const salt = '0123456789abcdef0123456789abcdef';
const consumerId = '11111111-2222-4333-8444-555555555555';
const producerId = '66666666-7777-4888-8999-aaaaaaaaaaaa';
const transportId = 'bbbbbbbb-cccc-4ddd-8eee-ffffffffffff';
const producerTransportId = '12345678-1234-4234-8234-123456789abc';

function fixture() {
  const trigger = { consumerOrdinal: 1, ssrc: 42, isAudio: false, beginBucket: 3, endBucket: 6 };
  const triggerEvent = { attempt: 1, elapsedMs: 9500, kind: 'receiver-stall-triggered',
    details: { triggerElapsedMs: 9500, trigger: structuredClone(trigger) } };
  const completion = { attempt: 1, elapsedMs: 9510, kind: 'receiver-stall-capture',
    details: { triggerElapsedMs: 9500, status: 'captured' } };
  const receiverSnapshot = { attempt: 1, elapsedMs: 9510, kind: 'receiver-stall',
    details: { triggerElapsedMs: 9500, trigger: structuredClone(trigger), snapshot: { transports: [{
      direction: 'receive', connectionState: 'connected', stats: [{ type: 'transport' }], consumerMappings: [{ ssrc: 42 }],
    }] } } };
  const delivery = { consumerId, producerId, ssrc: 42, attempt: 1, isAudio: false,
    packetsBySecond: [5, 5, 5, 0, 0, 0, 5, 5], eligibleSeconds: 8, secondsWithPackets: 5,
    longestGapSeconds: 3, passed: false, skippedShortLived: false };
  const generator = [{ clientId: 'PRIVATE-client', connectionAttempts: [{}], consumerDelivery: [delivery],
    diagnostics: { failures: [], snapshots: [], receiverStalls: [receiverSnapshot], events: [
      { attempt: 1, elapsedMs: 10, kind: 'consumer-created', details: { consumerId, producerId, ssrc: 42, kind: 'video' } },
      { attempt: 1, elapsedMs: 20, kind: 'resume-requested', details: { consumerId } },
      { attempt: 1, elapsedMs: 30, kind: 'resume-ack', details: { consumerId } },
      { attempt: 1, elapsedMs: 40, kind: 'track-first-rtp', details: { ssrc: 42 } },
      triggerEvent, completion,
    ] } }];
  const samples = [3000, 7000, 9000].map((scheduledElapsedMs, index) => {
    const ordinal = index + 1;
    return { schemaVersion: 1, ordinal, windowStartElapsedMs: 3000, windowEndElapsedMs: 11000,
      scheduledElapsedMs, startedElapsedMs: scheduledElapsedMs + 10, finishedElapsedMs: scheduledElapsedMs + 110,
      aliveAtStart: { server: true, generator: true }, aliveAtEnd: { server: true, generator: true }, status: 'ok',
      snapshot: { schemaVersion: 1, correlationSalt: salt, sampleId: ordinal, startedUs: ordinal * 100000,
        finishedUs: ordinal * 100000 + 100,
        coverage: { complete: true, registryBusy: false, participantsObserved: 1, participantsVisited: 1,
          busyParticipants: 0, participantLimitReached: false, entityLimitReached: false, deadlineReached: false },
        entities: [{ entityType: 'consumer', reference: mediaReference(salt, 'consumer', consumerId),
          transportReference: mediaReference(salt, 'transport', transportId), producerReference: mediaReference(salt, 'producer', producerId),
          kind: 'video', status: 'ok', paused: false, producerPaused: false, observedUs: ordinal * 100000 + 50,
          streams: [{ ssrc: 42, packetCount: ordinal * 10, rtpBytes: ordinal * 1000, workerTimestampMs: ordinal * 1000 }] },
        { entityType: 'producer', reference: mediaReference(salt, 'producer', producerId),
          transportReference: mediaReference(salt, 'transport', producerTransportId), producerReference: null,
          kind: 'video', status: 'ok', paused: false, producerPaused: null, observedUs: ordinal * 100000 + 60,
          streams: [{ ssrc: 7, packetCount: ordinal * 20, rtpBytes: ordinal * 2000, workerTimestampMs: ordinal * 1000 },
            { ssrc: 8, packetCount: ordinal * 30, rtpBytes: ordinal * 3000, workerTimestampMs: ordinal * 1000 }] }] } };
  });
  return { generator, samples, trigger, triggerEvent, completion, receiverSnapshot, delivery,
    diagnostics: generator[0].diagnostics, report: () => correlateReceiverStalls(samples, generator) };
}

function assertPrivate(report) {
  const serialized = JSON.stringify(report);
  for (const value of ['PRIVATE', consumerId, producerId, transportId, producerTransportId, '192.0.2.77']) {
    assert.equal(serialized.includes(value), false, `report must not expose ${value}`);
  }
}

function assertUnavailable(report) {
  assert.equal(report.coverage.available, false);
  assert.equal(report.coverage.complete, false);
  assert.ok(report.coverage.issues.length > 0);
  assert.deepEqual(report.stalls, []);
  assertPrivate(report);
}

function manyStalls(count, { captured = false, perClient = 128 } = {}) {
  const f = fixture();
  const clients = [];
  for (let index = 0; index < count; index++) {
    const clientIndex = Math.floor(index / perClient);
    const attempt = index % perClient + 1;
    const client = clients[clientIndex] ??= { clientId: 'PRIVATE', connectionAttempts: [], consumerDelivery: [],
      diagnostics: { failures: [], snapshots: [], receiverStalls: [], events: [] } };
    const id = `00000000-0000-4000-8000-${String(index + 1).padStart(12, '0')}`;
    client.connectionAttempts.push({});
    client.consumerDelivery.push({ ...structuredClone(f.delivery), consumerId: id, attempt });
    client.diagnostics.events.push({ attempt, elapsedMs: 10, kind: 'consumer-created',
      details: { consumerId: id, producerId, ssrc: 42, kind: 'video' } });
    const trigger = structuredClone(f.triggerEvent);
    trigger.attempt = attempt;
    trigger.details.trigger.consumerOrdinal = attempt;
    const completion = structuredClone(f.completion);
    completion.attempt = attempt;
    if (!captured) completion.details.status = 'busy';
    client.diagnostics.events.push(trigger, completion);
    if (captured) {
      const snapshot = structuredClone(f.receiverSnapshot);
      snapshot.attempt = attempt;
      snapshot.details.trigger.consumerOrdinal = attempt;
      client.diagnostics.receiverStalls.push(snapshot);
    }
  }
  return correlateReceiverStalls(f.samples, clients);
}

test('legacy and healthy evidence without recorded triggers never claims stall coverage', () => {
  for (const legacy of [false, true]) {
    const f = fixture();
    f.diagnostics.events = f.diagnostics.events.slice(0, 4);
    f.diagnostics.receiverStalls = [];
    if (legacy) delete f.diagnostics.receiverStalls;
    Object.assign(f.delivery, { packetsBySecond: Array(8).fill(5), secondsWithPackets: 8, longestGapSeconds: 0, passed: true });
    const report = f.report();
    assert.equal(report.schemaVersion, 1);
    assert.equal(report.coverage.available, true);
    assert.equal(report.coverage.complete, null);
    assert.equal(report.coverage.scope, 'recorded_stall_triggers');
    assert.equal(report.coverage.triggersObserved, 0);
    assert.equal(report.coverage.entriesRetained, 0);
    assert.deepEqual(report.coverage.issues, []);
    assert.deepEqual(report.stalls, []);
    assert.deepEqual(report.samples, [], 'no stall correlation is performed; scheduled coverage has its own report');
    assertPrivate(report);
  }
});

test('receiver stall correlation preserves failure and separates receiver, consumer, and producer evidence', () => {
  const f = fixture();
  const report = f.report();
  assert.equal(report.coverage.available, true);
  assert.equal(report.coverage.complete, true);
  assert.equal(report.coverage.triggersObserved, 1);
  assert.equal(report.coverage.entriesRetained, 1);
  assert.deepEqual(report.coverage.issues, []);
  const stall = report.stalls[0];
  assert.equal(stall.clientOrdinal, 1);
  assert.equal(stall.attemptOrdinal, 1);
  assert.equal(stall.consumerOrdinal, 1);
  assert.deepEqual(stall.trigger, { ...f.trigger, elapsedMs: 9500 });
  assert.deepEqual(stall.receiverCapture, { status: 'captured', completedElapsedMs: 9510, snapshotOrdinal: 1, peerState: 'connected' });
  assert.equal(stall.consumerReference, mediaReference(salt, 'consumer', consumerId));
  assert.equal(stall.producerReference, mediaReference(salt, 'producer', producerId));
  assert.equal(stall.kind, 'video');
  assert.deepEqual(stall.delivery, { passed: false, eligibleSeconds: 8, secondsWithPackets: 5, longestGapSeconds: 3 });
  assert.deepEqual(stall.consumer.nativeAccounting, { movement: 'increased', packetDelta: 20, byteDelta: 2000 });
  assert.deepEqual(stall.producer.nativeAccounting, { movement: 'increased', packetDelta: 100, byteDelta: 10000 });
  assert.deepEqual(stall.producer.observations.map(row => row.streams.map(stream => stream.ssrc)), [[7, 8], [7, 8], [7, 8]]);
  assert.equal(stall.consumer.observations[0].stream.ssrc, 42, 'rewritten consumer SSRC must not select producer streams');
  assert.deepEqual(stall.issues, []);
  assertPrivate(report);
});

test('capture timeout retains delivery and server evidence while keeping coverage incomplete', () => {
  const f = fixture();
  f.completion.details.status = 'timed_out';
  f.completion.elapsedMs = 11500;
  f.diagnostics.receiverStalls = [];
  f.diagnostics.failures.push('PRIVATE timed-out native capture');
  const report = f.report();
  assert.equal(report.coverage.available, true);
  assert.equal(report.coverage.complete, false);
  assert.ok(report.coverage.issues.length > 0);
  assert.equal(report.stalls.length, 1);
  assert.deepEqual(report.stalls[0].receiverCapture, { status: 'timed_out', completedElapsedMs: 11500, snapshotOrdinal: null, peerState: null });
  assert.equal(report.stalls[0].delivery.passed, false);
  assert.equal(report.stalls[0].consumer.nativeAccounting.movement, 'increased');
  assert.equal(report.stalls[0].producer.nativeAccounting.movement, 'increased');
  assertPrivate(report);
});

test('trigger identity and three recorded empty seconds cannot be forged or ambiguously duplicated', () => {
  const mutations = [
    f => { f.diagnostics.events.push(structuredClone(f.triggerEvent)); },
    f => { f.generator[0].connectionAttempts.push({}); f.triggerEvent.attempt = 2; },
    f => { f.triggerEvent.details.trigger.consumerOrdinal = 2; },
    f => { f.triggerEvent.details.trigger.ssrc++; },
    f => { f.triggerEvent.details.trigger.isAudio = true; },
    f => { f.triggerEvent.details.trigger.beginBucket = -1; },
    f => { f.triggerEvent.details.trigger.endBucket = 7; },
    f => { f.triggerEvent.details.trigger.beginBucket = 7; f.triggerEvent.details.trigger.endBucket = 10; },
    f => { f.triggerEvent.details.triggerElapsedMs++; },
    f => { f.triggerEvent.elapsedMs = 9505; f.receiverSnapshot.elapsedMs = 9504; },
    f => { f.delivery.packetsBySecond[4] = 1; },
    f => { f.receiverSnapshot.details.trigger.ssrc++; },
    f => { f.receiverSnapshot.details.trigger.beginBucket++; },
    f => { f.receiverSnapshot.details.snapshot.transports = 'PRIVATE'; },
  ];
  for (const mutate of mutations) {
    const f = fixture();
    mutate(f);
    assertUnavailable(f.report());
  }
});

test('orphan or absent completion and receiver snapshot records never imply complete capture evidence', () => {
  const missing = fixture();
  missing.diagnostics.events.pop();
  missing.diagnostics.receiverStalls = [];
  const missingReport = missing.report();
  assert.equal(missingReport.coverage.available, true);
  assert.equal(missingReport.coverage.complete, false);
  assert.deepEqual(missingReport.stalls[0].receiverCapture, { status: 'missing', completedElapsedMs: null, snapshotOrdinal: null, peerState: null });
  for (const keepSnapshot of [false, true]) {
    const f = fixture();
    f.diagnostics.events = f.diagnostics.events.filter(event => event.kind !== 'receiver-stall-triggered');
    if (!keepSnapshot) f.diagnostics.receiverStalls = [];
    const report = f.report();
    assert.equal(report.coverage.complete, false);
    assert.ok(report.coverage.issues.length > 0);
    assert.equal(report.stalls.length, 0);
    assertPrivate(report);
  }
});

test('missing producers and single observations remain distinct from flat native counters', () => {
  for (const retainedSamples of [0, 1]) {
    const f = fixture();
    for (const sample of f.samples.slice(retainedSamples)) sample.snapshot.entities.pop();
    const report = f.report();
    assert.equal(report.coverage.available, true);
    assert.equal(report.coverage.complete, false);
    assert.equal(report.stalls[0].producer.nativeAccounting.movement, retainedSamples ? 'single_observation' : 'unavailable');
    assert.equal(report.stalls[0].producer.nativeAccounting.packetDelta, null);
    assert.equal(report.stalls[0].producer.nativeAccounting.byteDelta, null);
    assert.equal(report.stalls[0].consumer.nativeAccounting.movement, 'increased');
    assertPrivate(report);
  }
  for (const count of [0, 100]) {
    const f = fixture();
    for (const sample of f.samples) for (const stream of sample.snapshot.entities[1].streams) {
      stream.packetCount = count;
      stream.rtpBytes = count * 100;
    }
    const report = f.report();
    assert.equal(report.coverage.complete, true);
    assert.deepEqual(report.stalls[0].producer.nativeAccounting, {
      movement: count ? 'flat_nonzero' : 'flat_zero', packetDelta: 0, byteDelta: 0,
    });
    assert.equal(report.stalls[0].delivery.passed, false);
  }
});

test('producer resets, replacement and changing stream sets cannot invent aggregate growth', () => {
  const cases = [
    { movement: 'reset_or_replaced', mutate: entity => { entity.streams[0].packetCount = 0; } },
    { movement: 'reset_or_replaced', mutate: entity => { entity.streams[0].rtpBytes = 0; } },
    { movement: 'reset_or_replaced', mutate: entity => { entity.streams[0].workerTimestampMs = 0; } },
    { movement: 'reset_or_replaced', mutate: entity => { entity.transportReference = 'f'.repeat(64); } },
    { movement: 'stream_set_changed', mutate: entity => { entity.streams.pop(); } },
    { movement: 'identity_unavailable', mutate: entity => { entity.transportReference = null; } },
  ];
  for (const { movement, mutate } of cases) {
    const f = fixture();
    mutate(f.samples[1].snapshot.entities[1]);
    const report = f.report();
    assert.equal(report.coverage.complete, false);
    assert.deepEqual(report.stalls[0].producer.nativeAccounting, { movement, packetDelta: null, byteDelta: null });
    assertPrivate(report);
  }
});

test('consumer resets and transport replacement keep stall correlation incomplete despite scheduled counter pairs', () => {
  for (const mutate of [
    entity => { entity.streams[0].packetCount = 0; },
    entity => { entity.streams[0].rtpBytes = 0; },
    entity => { entity.streams[0].workerTimestampMs = 0; },
    entity => { entity.transportReference = 'f'.repeat(64); },
  ]) {
    const f = fixture();
    mutate(f.samples[1].snapshot.entities[0]);
    const report = f.report();
    assert.equal(report.coverage.available, true);
    assert.equal(report.coverage.complete, false);
    assert.ok(report.stalls[0].issues.includes('consumer_counter_comparison_unavailable'));
    assert.deepEqual(report.stalls[0].consumer.nativeAccounting, { movement: 'reset_or_replaced', packetDelta: null, byteDelta: null });
    assertPrivate(report);
  }
});

test('malformed earlier duplicate ordinals cannot supply producer observations for a validated sample', () => {
  const f = fixture();
  const invalidSample = structuredClone(f.samples[0]);
  invalidSample.schemaVersion = 99;
  invalidSample.snapshot.entities[1].streams[0].packetCount = 999999;
  f.samples.splice(0, 0, invalidSample);
  f.samples.pop();
  const report = f.report();
  assert.equal(report.coverage.available, true);
  assert.equal(report.coverage.complete, false);
  assert.equal(report.stalls[0].producer.observations.length, 2);
  assert.deepEqual(report.stalls[0].producer.observations.map(row => row.streams[0].packetCount), [20, 40]);
  assert.deepEqual(report.stalls[0].producer.nativeAccounting, { movement: 'increased', packetDelta: 50, byteDelta: 5000 });
  assertPrivate(report);
});

test('producer aggregate counters reject overflow without discarding individual safe stream observations', () => {
  for (const field of ['packetCount', 'rtpBytes']) {
    const f = fixture();
    for (const sample of f.samples) for (const stream of sample.snapshot.entities[1].streams) stream[field] = Number.MAX_SAFE_INTEGER;
    const report = f.report();
    assert.equal(report.coverage.available, true);
    assert.equal(report.coverage.complete, false);
    assert.deepEqual(report.stalls[0].producer.nativeAccounting, { movement: 'counter_overflow', packetDelta: null, byteDelta: null });
    assert.equal(report.stalls[0].producer.observations[0].streams[0][field], Number.MAX_SAFE_INTEGER);
    assertPrivate(report);
  }
});

test('stall output and receiver snapshot admission retain their independent global bounds', () => {
  const limited = manyStalls(130);
  assert.equal(limited.coverage.available, true);
  assert.equal(limited.coverage.complete, false);
  assert.equal(limited.coverage.triggersObserved, 130);
  assert.equal(limited.coverage.entriesRetained, 128);
  assert.equal(limited.stalls.length, 128);
  assert.ok(limited.coverage.issues.includes('stall_report_limit'));
  assertPrivate(limited);
  const atLimit = manyStalls(8, { captured: true, perClient: 4 });
  assert.equal(atLimit.coverage.available, true);
  assert.equal(atLimit.stalls.length, 8);
  assertUnavailable(manyStalls(9, { captured: true, perClient: 5 }));
});

test('mixed server namespaces and partial native snapshots cannot establish complete stall correlation', () => {
  for (const mutate of [
    f => { f.samples[1].snapshot.correlationSalt = 'f'.repeat(32); },
    f => { f.samples[1].snapshot.coverage.complete = false; f.samples[1].snapshot.coverage.deadlineReached = true; },
    f => { f.samples[1].snapshot.entities[1].status = 'timeout'; f.samples[1].snapshot.entities[1].streams = []; f.samples[1].snapshot.coverage.complete = false; },
  ]) {
    const f = fixture();
    mutate(f);
    const report = f.report();
    assert.equal(report.coverage.complete, false);
    assert.ok(report.coverage.issues.length > 0);
    assertPrivate(report);
  }
});

test('raw RTC fields are not copied and independent clock shifts do not alter native accounting', () => {
  const f = fixture();
  const before = f.report();
  const transport = f.receiverSnapshot.details.snapshot.transports[0];
  transport.stats[0].PRIVATE = { address: '192.0.2.77', sdp: 'PRIVATE SDP credential' };
  f.diagnostics.snapshots.push({ attempt: 1, elapsedMs: 10000, kind: 'pre-close', details: { PRIVATE: 'PRIVATE' } });
  for (const event of [f.triggerEvent, f.completion]) {
    event.elapsedMs += 8000000000;
    event.details.triggerElapsedMs += 8000000000;
  }
  f.receiverSnapshot.elapsedMs += 8000000000;
  f.receiverSnapshot.details.triggerElapsedMs += 8000000000;
  for (const sample of f.samples) {
    sample.snapshot.startedUs += 9000000000;
    sample.snapshot.finishedUs += 9000000000;
    for (const entity of sample.snapshot.entities) entity.observedUs += 9000000000;
  }
  const report = f.report();
  assert.equal(report.coverage.complete, true);
  assert.equal(report.stalls[0].trigger.elapsedMs, 8000009500);
  assert.equal(report.stalls[0].receiverCapture.completedElapsedMs, 8000009510);
  assert.deepEqual(report.stalls[0].consumer.nativeAccounting, before.stalls[0].consumer.nativeAccounting);
  assert.deepEqual(report.stalls[0].producer.nativeAccounting, before.stalls[0].producer.nativeAccounting);
  assert.deepEqual(report.samples, before.samples);
  assertPrivate(report);
});
