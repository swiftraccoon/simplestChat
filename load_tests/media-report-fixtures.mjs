// Credential-free retained evidence for actual native/JavaScript oracle parity.
import { mediaReference, correlateMediaDiagnostics } from './media-diagnostic-report.mjs';
import { correlateReceiverStalls } from './receiver-stall-report.mjs';

const salt = '0123456789abcdef0123456789abcdef';
const consumerId = '11111111-2222-4333-8444-555555555555';
const producerId = '66666666-7777-4888-8999-aaaaaaaaaaaa';
const transportId = 'bbbbbbbb-cccc-4ddd-8eee-ffffffffffff';
const producerTransportId = '12345678-1234-4234-8234-123456789abc';

export function receiverStallFixture() {
  const trigger = { consumerOrdinal: 1, ssrc: 42, isAudio: false, beginBucket: 3, endBucket: 6 };
  const triggerEvent = {
    attempt: 1,
    elapsedMs: 9500,
    kind: 'receiver-stall-triggered',
    details: { triggerElapsedMs: 9500, trigger: structuredClone(trigger) },
  };
  const completion = {
    attempt: 1,
    elapsedMs: 9510,
    kind: 'receiver-stall-capture',
    details: { triggerElapsedMs: 9500, status: 'captured' },
  };
  const receiverSnapshot = {
    attempt: 1,
    elapsedMs: 9510,
    kind: 'receiver-stall',
    details: {
      triggerElapsedMs: 9500,
      trigger: structuredClone(trigger),
      snapshot: {
        transports: [
          {
            direction: 'receive',
            connectionState: 'connected',
            stats: [{ type: 'transport' }],
            consumerMappings: [{ ssrc: 42 }],
          },
        ],
      },
    },
  };
  const delivery = {
    consumerId,
    producerId,
    ssrc: 42,
    attempt: 1,
    isAudio: false,
    packetsBySecond: [5, 5, 5, 0, 0, 0, 5, 5],
    eligibleSeconds: 8,
    secondsWithPackets: 5,
    longestGapSeconds: 3,
    passed: false,
    skippedShortLived: false,
  };
  const generator = [
    {
      clientId: 'PRIVATE-client',
      connectionAttempts: [{}],
      consumerDelivery: [delivery],
      diagnostics: {
        failures: [],
        snapshots: [],
        receiverStalls: [receiverSnapshot],
        events: [
          {
            attempt: 1,
            elapsedMs: 10,
            kind: 'consumer-created',
            details: { consumerId, producerId, ssrc: 42, kind: 'video' },
          },
          { attempt: 1, elapsedMs: 20, kind: 'resume-requested', details: { consumerId } },
          { attempt: 1, elapsedMs: 30, kind: 'resume-ack', details: { consumerId } },
          { attempt: 1, elapsedMs: 40, kind: 'track-first-rtp', details: { ssrc: 42 } },
          triggerEvent,
          completion,
        ],
      },
    },
  ];
  const samples = [3000, 7000, 9000].map((scheduledElapsedMs, index) => {
    const ordinal = index + 1;
    return {
      schemaVersion: 1,
      ordinal,
      windowStartElapsedMs: 3000,
      windowEndElapsedMs: 11000,
      scheduledElapsedMs,
      startedElapsedMs: scheduledElapsedMs + 10,
      finishedElapsedMs: scheduledElapsedMs + 110,
      aliveAtStart: { server: true, generator: true },
      aliveAtEnd: { server: true, generator: true },
      status: 'ok',
      snapshot: {
        schemaVersion: 1,
        correlationSalt: salt,
        sampleId: ordinal,
        startedUs: ordinal * 100000,
        finishedUs: ordinal * 100000 + 100,
        coverage: {
          complete: true,
          registryBusy: false,
          participantsObserved: 1,
          participantsVisited: 1,
          busyParticipants: 0,
          participantLimitReached: false,
          entityLimitReached: false,
          deadlineReached: false,
        },
        entities: [
          {
            entityType: 'consumer',
            reference: mediaReference(salt, 'consumer', consumerId),
            transportReference: mediaReference(salt, 'transport', transportId),
            producerReference: mediaReference(salt, 'producer', producerId),
            kind: 'video',
            status: 'ok',
            paused: false,
            producerPaused: false,
            observedUs: ordinal * 100000 + 50,
            streams: [
              {
                ssrc: 42,
                packetCount: ordinal * 10,
                rtpBytes: ordinal * 1000,
                workerTimestampMs: ordinal * 1000,
              },
            ],
          },
          {
            entityType: 'producer',
            reference: mediaReference(salt, 'producer', producerId),
            transportReference: mediaReference(salt, 'transport', producerTransportId),
            producerReference: null,
            kind: 'video',
            status: 'ok',
            paused: false,
            producerPaused: null,
            observedUs: ordinal * 100000 + 60,
            streams: [
              {
                ssrc: 7,
                packetCount: ordinal * 20,
                rtpBytes: ordinal * 2000,
                workerTimestampMs: ordinal * 1000,
              },
              {
                ssrc: 8,
                packetCount: ordinal * 30,
                rtpBytes: ordinal * 3000,
                workerTimestampMs: ordinal * 1000,
              },
            ],
          },
        ],
      },
    };
  });
  return {
    generator,
    samples,
    trigger,
    triggerEvent,
    completion,
    receiverSnapshot,
    delivery,
    diagnostics: generator[0].diagnostics,
    report: () => correlateReceiverStalls(samples, generator),
  };
}

export function mediaFixture() {
  const fixture = receiverStallFixture();
  fixture.diagnostics.events = fixture.diagnostics.events.slice(0, 4);
  fixture.diagnostics.receiverStalls = [];
  Object.assign(fixture.delivery, {
    packetsBySecond: Array(8).fill(5),
    secondsWithPackets: 8,
    longestGapSeconds: 0,
    passed: true,
  });
  fixture.report = () => correlateMediaDiagnostics(fixture.samples, fixture.generator);
  return fixture;
}
