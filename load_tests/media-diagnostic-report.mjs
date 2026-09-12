// Local-only diagnostic sampling. Native counters and client delivery use different clocks/scopes.
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';

export const MEDIA_BODY_LIMIT = 4 * 1024 * 1024;
export const MEDIA_SAMPLE_LATENESS_MS = 1000;
const GENERATOR_BODY_LIMIT = 32 * 1024 * 1024;
const uint = value => Number.isSafeInteger(value) && value >= 0;
const hex = (value, size) => typeof value === 'string' && new RegExp(`^[a-f0-9]{${size}}$`).test(value);
const uuid = value => typeof value === 'string' && /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(value);
const keys = (value, names) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === names.length && names.every(name => Object.hasOwn(value, name));
const nullable = (value, valid) => value === null || valid(value);
const bool = value => typeof value === 'boolean';
const statuses = ['ok', 'closed', 'timeout', 'error', 'no_streams', 'stream_limit', 'not_collected'];
const failure = code => Object.assign(new Error(code), { code });

export function validateMediaSnapshot(value) {
  const invalid = () => { throw failure('invalid_media_schema'); };
  if (!keys(value, ['schemaVersion', 'correlationSalt', 'sampleId', 'startedUs', 'finishedUs', 'coverage', 'entities']) ||
      value.schemaVersion !== 1 || !hex(value.correlationSalt, 32) || !uint(value.sampleId) || !value.sampleId ||
      !uint(value.startedUs) || !uint(value.finishedUs) || value.finishedUs < value.startedUs ||
      !keys(value.coverage, ['complete', 'registryBusy', 'participantsObserved', 'participantsVisited', 'busyParticipants',
        'participantLimitReached', 'entityLimitReached', 'deadlineReached']) ||
      !['complete', 'registryBusy', 'participantLimitReached', 'entityLimitReached', 'deadlineReached'].every(key => bool(value.coverage[key])) ||
      !['participantsObserved', 'participantsVisited', 'busyParticipants'].every(key => uint(value.coverage[key])) ||
      value.coverage.participantsVisited > 64 || value.coverage.busyParticipants > 64 ||
      value.coverage.participantsVisited + value.coverage.busyParticipants > Math.min(64, value.coverage.participantsObserved) ||
      !Array.isArray(value.entities) || value.entities.length > 1024) invalid();
  const references = new Set();
  for (const entity of value.entities) {
    if (!keys(entity, ['entityType', 'reference', 'transportReference', 'producerReference', 'kind', 'status', 'paused',
      'producerPaused', 'observedUs', 'streams']) || !['consumer', 'producer'].includes(entity.entityType) ||
      !hex(entity.reference, 64) || !nullable(entity.transportReference, v => hex(v, 64)) ||
      !nullable(entity.producerReference, v => hex(v, 64)) || !['audio', 'video'].includes(entity.kind) ||
      !statuses.includes(entity.status) || !nullable(entity.paused, bool) || !nullable(entity.producerPaused, bool) ||
      !uint(entity.observedUs) || entity.observedUs < value.startedUs || entity.observedUs > value.finishedUs ||
      !Array.isArray(entity.streams) || entity.streams.length > 16 || (entity.status === 'ok' && !entity.streams.length)) invalid();
    const reference = `${entity.entityType}/${entity.reference}`;
    if (references.has(reference)) invalid();
    references.add(reference);
    const ssrcs = new Set();
    for (const stream of entity.streams) {
      if (!keys(stream, ['ssrc', 'packetCount', 'rtpBytes', 'workerTimestampMs']) ||
          !Object.values(stream).every(uint) || stream.ssrc > 0xffffffff || ssrcs.has(stream.ssrc)) invalid();
      ssrcs.add(stream.ssrc);
    }
  }
  if (value.coverage.complete && (value.coverage.registryBusy || value.coverage.busyParticipants ||
      value.coverage.participantLimitReached || value.coverage.entityLimitReached || value.coverage.deadlineReached ||
      value.coverage.participantsVisited !== value.coverage.participantsObserved || value.entities.some(entity => entity.status !== 'ok'))) invalid();
  return value;
}

export function mediaReference(salt, kind, id) {
  if (!hex(salt, 32) || !['consumer', 'producer', 'transport'].includes(kind) || !uuid(id)) throw failure('invalid_correlation_identity');
  return createHash('sha256').update('simplestchat-media-v1\0', 'ascii').update(Buffer.from(salt, 'hex'))
    .update(kind, 'ascii').update('\0', 'ascii').update(id, 'ascii').digest('hex');
}

/** Deadline covers fetch, bounded streaming body, decoding and validation. No redirects/retries. */
export async function fetchMediaSnapshot({ origin, token, timeoutMs = 2000 }, { fetch = globalThis.fetch, now = () => performance.now() } = {}) {
  const url = new URL(origin);
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || url.pathname !== '/' || url.search || url.hash ||
      url.username || url.password || !Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 2000) throw failure('invalid_media_target');
  const controller = new AbortController();
  const started = now();
  let timer, reader;
  try {
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => { controller.abort(); reject(failure('media_timeout')); }, timeoutMs);
    });
    const request = (async () => {
      const response = await fetch(`${url.origin}/diagnostics/media`, { headers: { Authorization: `Bearer ${token}` },
        signal: controller.signal, redirect: 'error', cache: 'no-store' });
      if (!response.ok) throw failure(response.status === 404 ? 'media_unavailable' : response.status === 429 ? 'media_busy' :
        [401, 403].includes(response.status) ? 'media_unauthorized' : 'media_http_error');
      const declared = Number(response.headers.get('content-length'));
      if (Number.isFinite(declared) && declared > MEDIA_BODY_LIMIT) throw failure('media_body_limit');
      if (!response.body) throw failure('invalid_media_json');
      reader = response.body.getReader();
      let size = 0;
      const chunks = [];
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MEDIA_BODY_LIMIT) throw failure('media_body_limit');
        chunks.push(value);
      }
      let decoded;
      try { decoded = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks, size))); }
      catch { throw failure('invalid_media_json'); }
      const snapshot = validateMediaSnapshot(decoded);
      if (now() - started >= timeoutMs) throw failure('media_timeout');
      return { status: 'ok', snapshot };
    })();
    return await Promise.race([request, timeout]);
  } catch (error) {
    const codes = ['media_timeout', 'media_unavailable', 'media_busy', 'media_unauthorized', 'media_http_error',
      'media_body_limit', 'invalid_media_json', 'invalid_media_schema'];
    return { status: 'error', issue: codes.includes(error?.code) ? error.code : 'media_request_failed' };
  } finally {
    clearTimeout(timer);
    controller.abort();
    if (reader) reader.cancel().catch(() => {});
  }
}

export function mediaSampleSchedule({ rampUp, warmup, duration }) {
  if (!Number.isInteger(rampUp) || rampUp < 1 || rampUp > 600 || !Number.isInteger(warmup) || warmup < 2 || warmup > 60 ||
      !Number.isInteger(duration) || duration < 3 || duration > 180) throw failure('invalid_media_schedule');
  const start = (rampUp + warmup) * 1000, length = duration * 1000;
  const times = length < 4000 ? [start, start + length / 2] : [start, start + length / 2, start + length - 2000];
  return [...new Set(times)].sort((a, b) => a - b).filter((time, index, sorted) => !index || time - sorted[index - 1] >= 250);
}

/** One sequential poll from the runner's live-generator loop; no background intervals. */
export function createMediaSampler(options, { now, alive, request, persist }) {
  const schedule = mediaSampleSchedule(options);
  const start = (options.rampUp + options.warmup) * 1000;
  const end = (options.rampUp + options.warmup + options.duration) * 1000;
  const window = { windowStartElapsedMs: start, windowEndElapsedMs: end };
  const samples = [];
  let index = 0, running = false, lastStart = -Infinity;
  const save = async sample => {
    samples.push(sample);
    try { await persist(sample); }
    catch { sample.persistenceIssue = 'media_sample_write_failed'; }
  };
  const missed = async reason => {
    const liveness = alive();
    await save({ schemaVersion: 1, ...window, ordinal: index + 1, scheduledElapsedMs: schedule[index++],
      startedElapsedMs: null, finishedElapsedMs: now(), aliveAtStart: liveness, aliveAtEnd: liveness,
      status: 'missed', issue: reason });
  };
  return {
    samples,
    async poll() {
      if (running || index === schedule.length || now() < schedule[index] || now() - lastStart < 250) return;
      const before = alive();
      if (!before.server || !before.generator || now() >= end) { await missed('media_sample_not_live'); return; }
      running = true;
      try {
        const sample = { schemaVersion: 1, ...window, ordinal: index + 1, scheduledElapsedMs: schedule[index++],
          startedElapsedMs: now(), aliveAtStart: before };
        lastStart = sample.startedElapsedMs;
        let result;
        try { result = await request(); }
        catch { result = { status: 'error', issue: 'media_request_failed' }; }
        Object.assign(sample, result, { finishedElapsedMs: now(), aliveAtEnd: alive() });
        if (sample.startedElapsedMs - sample.scheduledElapsedMs > MEDIA_SAMPLE_LATENESS_MS) sample.timingIssue = 'media_sample_late';
        if (!sample.aliveAtEnd.server || !sample.aliveAtEnd.generator || sample.finishedElapsedMs >= end) sample.timingIssue = 'media_sample_crossed_teardown';
        await save(sample);
      } finally { running = false; }
    },
    async finish() {
      if (running) throw failure('media_sampler_still_running');
      while (index < schedule.length) await missed('media_sample_missed');
      return samples;
    },
  };
}

export async function readGeneratorResults(path) {
  let file;
  try {
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const info = await file.stat();
    if (!info.isFile() || info.size > GENERATOR_BODY_LIMIT) throw failure('generator_results_limit');
    // Read one bounded prefix plus an overflow byte, even if the file grows.
    const data = Buffer.alloc(Math.min(info.size + 1, GENERATOR_BODY_LIMIT + 1));
    let size = 0;
    while (size < data.length) {
      const read = await file.read(data, size, data.length - size, null);
      if (!read.bytesRead) break;
      size += read.bytesRead;
    }
    if (size !== info.size) throw failure('generator_results_changed');
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(data.subarray(0, size)));
  } catch { throw failure('generator_results_unavailable'); }
  finally { if (file) await file.close(); }
}

function generatorConsumers(value) {
  if (!Array.isArray(value) || !value.length || value.length > 100) throw failure('invalid_generator_results');
  const consumers = [];
  for (const [clientIndex, client] of value.entries()) {
    if (!client || !Array.isArray(client.consumerDelivery) || client.consumerDelivery.length > 2048 ||
        !Array.isArray(client.connectionAttempts) || client.connectionAttempts.length > 128 ||
        !client.diagnostics || !Array.isArray(client.diagnostics.events) || client.diagnostics.events.length > 4096 ||
        !Array.isArray(client.diagnostics.failures) || client.diagnostics.failures.length) throw failure('invalid_generator_results');
    const created = new Map(), resumed = new Set(), requested = new Set(), received = new Set();
    for (const event of client.diagnostics.events) {
      if (!event || !uint(event.attempt) || !event.attempt || event.attempt > client.connectionAttempts.length ||
          !uint(event.elapsedMs) || typeof event.kind !== 'string' || !event.details || typeof event.details !== 'object') throw failure('invalid_generator_results');
      const detail = event.details;
      if (event.kind === 'consumer-created') {
        if (!uuid(detail.consumerId) || !uuid(detail.producerId) || !uint(detail.ssrc) || detail.ssrc > 0xffffffff ||
            !['audio', 'video'].includes(detail.kind) || created.has(detail.consumerId)) throw failure('invalid_generator_results');
        created.set(detail.consumerId, { attempt: event.attempt, producerId: detail.producerId, ssrc: detail.ssrc, kind: detail.kind });
      } else if (['resume-requested', 'resume-ack'].includes(event.kind)) {
        if (!uuid(detail.consumerId)) throw failure('invalid_generator_results');
        (event.kind === 'resume-ack' ? resumed : requested).add(`${event.attempt}/${detail.consumerId}`);
      } else if (event.kind === 'track-first-rtp') {
        if (!uint(detail.ssrc) || detail.ssrc > 0xffffffff) throw failure('invalid_generator_results');
        received.add(`${event.attempt}/${detail.ssrc}`);
      }
    }
    for (const [consumerIndex, delivery] of client.consumerDelivery.entries()) {
      if (!delivery || !uuid(delivery.consumerId) || !uuid(delivery.producerId) || !uint(delivery.ssrc) || delivery.ssrc > 0xffffffff ||
          !Array.isArray(delivery.packetsBySecond) || delivery.packetsBySecond.length > 3600 || !delivery.packetsBySecond.every(uint) ||
          !['eligibleSeconds', 'secondsWithPackets', 'longestGapSeconds'].every(key => uint(delivery[key])) ||
          delivery.eligibleSeconds > delivery.packetsBySecond.length || delivery.secondsWithPackets > delivery.eligibleSeconds ||
          delivery.longestGapSeconds > delivery.eligibleSeconds || !bool(delivery.passed) || !bool(delivery.skippedShortLived)) throw failure('invalid_generator_results');
      const event = created.get(delivery.consumerId);
      if (!event || event.producerId !== delivery.producerId || event.ssrc !== delivery.ssrc) throw failure('invalid_generator_results');
      const packetCount = delivery.packetsBySecond.reduce((sum, packets) => sum + packets, 0);
      if (!uint(packetCount)) throw failure('invalid_generator_results');
      consumers.push({ id: delivery.consumerId, producerId: delivery.producerId, kind: event.kind, ssrc: delivery.ssrc,
        shortLived: delivery.skippedShortLived, generator: { clientOrdinal: clientIndex + 1, consumerOrdinal: consumerIndex + 1,
          attemptOrdinal: event.attempt, packetsInMeasurement: packetCount, eligibleSeconds: delivery.eligibleSeconds,
          secondsWithPackets: delivery.secondsWithPackets, longestGapSeconds: delivery.longestGapSeconds,
          deliveryPassed: delivery.passed, resumeRequested: requested.has(`${event.attempt}/${delivery.consumerId}`),
          resumeAcknowledged: resumed.has(`${event.attempt}/${delivery.consumerId}`), firstRtpEventObserved: received.has(`${event.attempt}/${delivery.ssrc}`) } });
      if (consumers.length > 16384) throw failure('generator_consumer_limit');
    }
  }
  const ids = new Set(consumers.map(consumer => consumer.id));
  if (!consumers.length || ids.size !== consumers.length) throw failure('invalid_generator_results');
  return consumers;
}

export function correlateMediaDiagnostics(samples, generatorResults) {
  const issues = new Set();
  const report = { schemaVersion: 1, coverage: { available: false, complete: false, issues: [],
    expectedConsumers: 0, matchedConsumers: 0, shortLivedUnobservedConsumers: 0, consumersWithCounterPairs: 0 }, samples: [], consumers: [],
    interpretation: 'Native cumulative RTP accounting and generator measurement buckets have different intervals. No cross-clock arithmetic or packet-loss/peer-receipt inference. Flat counters are observations, not workload failures; sparse samples cannot exclude unobserved resets or state changes.' };
  let consumers;
  try { consumers = generatorConsumers(generatorResults); }
  catch { issues.add('generator_correlation_unavailable'); }
  const valid = [];
  let salt, lastId = 0, lastOrdinal = 0, lastScheduled = -1, lastStarted = -Infinity, lastFinished = -Infinity, windowStart, windowEnd;
  if (!Array.isArray(samples) || samples.length < 2 || samples.length > 3) issues.add('media_sample_count');
  for (const sample of Array.isArray(samples) ? samples.slice(0, 3) : []) {
    const elapsed = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
    if (!sample || sample.schemaVersion !== 1 || !uint(sample.ordinal) || !sample.ordinal || sample.ordinal > 3 ||
        !uint(sample.windowStartElapsedMs) || !uint(sample.windowEndElapsedMs) || sample.windowStartElapsedMs < 3000 ||
        sample.windowEndElapsedMs - sample.windowStartElapsedMs < 3000 || sample.windowEndElapsedMs - sample.windowStartElapsedMs > 180000 ||
        !uint(sample.scheduledElapsedMs) || sample.scheduledElapsedMs < sample.windowStartElapsedMs || sample.scheduledElapsedMs >= sample.windowEndElapsedMs ||
        !elapsed(sample.finishedElapsedMs) || !nullable(sample.startedElapsedMs, elapsed) ||
        !['ok', 'error', 'missed'].includes(sample.status) || !['aliveAtStart', 'aliveAtEnd'].every(key =>
          keys(sample[key], ['server', 'generator']) && bool(sample[key].server) && bool(sample[key].generator))) {
      issues.add('invalid_media_sample'); continue;
    }
    if (sample.ordinal <= lastOrdinal || sample.scheduledElapsedMs <= lastScheduled ||
        (windowStart !== undefined && (sample.windowStartElapsedMs !== windowStart || sample.windowEndElapsedMs !== windowEnd))) {
      issues.add('media_sample_order'); continue;
    }
    windowStart = sample.windowStartElapsedMs; windowEnd = sample.windowEndElapsedMs;
    const length = windowEnd - windowStart;
    const schedule = [...new Set(length < 4000 ? [windowStart, windowStart + length / 2] :
      [windowStart, windowStart + length / 2, windowEnd - 2000])].sort((a, b) => a - b);
    if (sample.scheduledElapsedMs !== schedule[sample.ordinal - 1] || samples.length !== schedule.length) issues.add('media_schedule_mismatch');
    lastOrdinal = sample.ordinal; lastScheduled = sample.scheduledElapsedMs;
    if (sample.status === 'missed' ? sample.startedElapsedMs !== null : sample.startedElapsedMs === null ||
        sample.startedElapsedMs < sample.scheduledElapsedMs || sample.finishedElapsedMs < sample.startedElapsedMs ||
        sample.startedElapsedMs - lastStarted < 250 || sample.startedElapsedMs < lastFinished) {
      issues.add('invalid_media_sample_timing'); continue;
    }
    if (sample.startedElapsedMs !== null) { lastStarted = sample.startedElapsedMs; lastFinished = sample.finishedElapsedMs; }
    report.samples.push({ ordinal: sample.ordinal, status: sample.status, scheduledElapsedMs: sample.scheduledElapsedMs,
      startedElapsedMs: sample.startedElapsedMs, finishedElapsedMs: sample.finishedElapsedMs,
      aliveAtStart: { ...sample.aliveAtStart }, aliveAtEnd: { ...sample.aliveAtEnd } });
    if (sample.status !== 'ok') { issues.add('media_sample_unavailable'); continue; }
    try { validateMediaSnapshot(sample.snapshot); } catch { issues.add('invalid_media_schema'); continue; }
    const snapshot = sample.snapshot;
    if (sample.timingIssue || sample.persistenceIssue || !sample.aliveAtStart?.generator || !sample.aliveAtEnd?.generator ||
        !sample.aliveAtStart?.server || !sample.aliveAtEnd?.server || sample.startedElapsedMs - sample.scheduledElapsedMs > MEDIA_SAMPLE_LATENESS_MS ||
        sample.finishedElapsedMs >= windowEnd || sample.finishedElapsedMs - sample.startedElapsedMs > 2000) issues.add('media_sample_incomplete');
    if (!snapshot.coverage.complete || snapshot.coverage.registryBusy || snapshot.coverage.busyParticipants ||
        snapshot.coverage.participantLimitReached || snapshot.coverage.entityLimitReached || snapshot.coverage.deadlineReached ||
        snapshot.entities.some(entity => entity.status !== 'ok')) issues.add('native_coverage_incomplete');
    if (salt && salt !== snapshot.correlationSalt) { issues.add('media_namespace_changed'); continue; }
    salt = snapshot.correlationSalt;
    if (snapshot.sampleId <= lastId) { issues.add('media_sample_order'); continue; }
    lastId = snapshot.sampleId;
    valid.push(sample);
  }
  if (valid.length < 2) issues.add('insufficient_media_samples');
  report.coverage.available = Boolean(consumers && valid.length);
  if (consumers && salt) for (const consumer of consumers) {
    const reference = mediaReference(salt, 'consumer', consumer.id);
    const producerReference = mediaReference(salt, 'producer', consumer.producerId);
    const observations = [];
    let matched = false;
    for (const sample of valid) {
      const entity = sample.snapshot.entities.find(e => e.entityType === 'consumer' && e.reference === reference);
      if (!entity) { observations.push({ sampleOrdinal: sample.ordinal, status: 'not_observed' }); continue; }
      if (entity.producerReference !== producerReference || entity.kind !== consumer.kind) {
        issues.add('consumer_identity_mismatch'); observations.push({ sampleOrdinal: sample.ordinal, status: 'identity_mismatch' }); continue;
      }
      const stream = entity.streams.find(stream => stream.ssrc === consumer.ssrc);
      matched = true;
      observations.push({ sampleOrdinal: sample.ordinal, status: entity.status, paused: entity.paused,
        producerPaused: entity.producerPaused, transportReference: entity.transportReference,
        observedUs: entity.observedUs, stream: stream ?? null });
      if (!['ok', 'closed', 'no_streams'].includes(entity.status)) issues.add('consumer_observation_incomplete');
      if (entity.status === 'ok' && !stream) issues.add('consumer_stream_missing');
    }
    report.coverage.expectedConsumers++;
    if (matched) report.coverage.matchedConsumers++;
    else if (consumer.shortLived) report.coverage.shortLivedUnobservedConsumers++;
    else issues.add('consumer_not_observed');
    const counted = observations.filter(o => o.status === 'ok' && o.stream);
    if (counted.some(row => !row.transportReference)) issues.add('consumer_identity_unavailable');
    if (counted.length >= 2 && counted.every(row => row.transportReference)) report.coverage.consumersWithCounterPairs++;
    else if (!consumer.shortLived) issues.add('consumer_counter_pair_missing');
    let movement = counted.length ? 'single_observation' : 'unavailable';
    let packetDelta = null, byteDelta = null;
    if (counted.length >= 2) {
      const reset = counted.some((row, index) => index && (row.transportReference !== counted[index - 1].transportReference ||
        row.stream.packetCount < counted[index - 1].stream.packetCount || row.stream.rtpBytes < counted[index - 1].stream.rtpBytes ||
        row.stream.workerTimestampMs < counted[index - 1].stream.workerTimestampMs));
      const first = counted[0], last = counted.at(-1);
      if (reset) movement = 'reset_or_replaced';
      else if (!first.transportReference || !last.transportReference) movement = 'identity_unavailable';
      else {
        packetDelta = last.stream.packetCount - first.stream.packetCount;
        byteDelta = last.stream.rtpBytes - first.stream.rtpBytes;
        movement = packetDelta > 0 ? 'increased' : last.stream.packetCount === 0 ? 'flat_zero' : 'flat_nonzero';
      }
    }
    report.consumers.push({ consumerReference: reference, producerReference, ssrc: consumer.ssrc, kind: consumer.kind,
      generator: consumer.generator, observations, nativeAccounting: { movement, packetDelta, byteDelta } });
  }
  report.coverage.issues = [...issues].sort();
  report.coverage.complete = report.coverage.available && !issues.size && report.coverage.matchedConsumers > 0;
  return report;
}
