// Owned offline correlation. Only the complete fixed report schema crosses the
// child-process boundary; raw results, private fields and process errors do not.
import { execFile } from 'node:child_process';

const MAX_INPUT = 12 * 1024 * 1024;
const MAX_OUTPUT = 128 * 1024 * 1024;
const MAX_CONSUMERS = 16384;
const exact = (value, keys) =>
  value &&
  typeof value === 'object' &&
  !Array.isArray(value) &&
  Object.keys(value).length === keys.length &&
  keys.every((key) => Object.hasOwn(value, key));
const uint = (value, max = Number.MAX_SAFE_INTEGER) =>
  Number.isSafeInteger(value) && value >= 0 && value <= max;
const elapsed = (value) => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const bool = (value) => typeof value === 'boolean';
const nullable = (value, valid) => value === null || valid(value);
const ordinal = (value, max) => uint(value, max) && value > 0;
const reference = (value) => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const kind = (value) => ['audio', 'video'].includes(value);
const entityStatuses = [
  'ok',
  'closed',
  'timeout',
  'error',
  'no_streams',
  'stream_limit',
  'not_collected',
];
const movements = [
  'unavailable',
  'single_observation',
  'increased',
  'flat_zero',
  'flat_nonzero',
  'reset_or_replaced',
  'identity_unavailable',
];
const mediaIssues = [
  'generator_diagnostics_incomplete',
  'generator_correlation_unavailable',
  'media_sample_count',
  'invalid_media_sample',
  'media_sample_order',
  'media_schedule_mismatch',
  'invalid_media_sample_timing',
  'media_sample_unavailable',
  'invalid_media_schema',
  'media_sample_incomplete',
  'native_coverage_incomplete',
  'media_namespace_changed',
  'insufficient_media_samples',
  'consumer_identity_mismatch',
  'consumer_observation_incomplete',
  'consumer_stream_missing',
  'consumer_not_observed',
  'consumer_identity_unavailable',
  'consumer_counter_pair_missing',
];
const stallIssues = [
  'generator_stall_evidence_unavailable',
  'orphan_receiver_capture',
  'generator_diagnostics_incomplete',
  'scheduled_media_evidence_incomplete',
  'stall_report_limit',
  'receiver_capture_incomplete',
  'receiver_snapshot_missing',
  'receiver_capture_contradiction',
  'consumer_counter_pair_missing',
  'consumer_counter_comparison_unavailable',
  'producer_identity_mismatch',
  'producer_counter_pair_missing',
  'producer_counter_comparison_unavailable',
];
const mediaInterpretation =
  'Native cumulative RTP accounting and generator measurement buckets have different intervals. ' +
  'No cross-clock arithmetic or packet-loss/peer-receipt inference. ' +
  'Flat counters are observations, not workload failures; ' +
  'sparse samples cannot exclude unobserved resets or state changes.';
const stallInterpretation =
  'Identity correlation with retained scheduled SFU samples, not observations at the stall. ' +
  'Trigger elapsedMs uses the client collector clock; sample elapsedMs uses the runner clock; ' +
  'observedUs and workerTimestampMs retain native clocks. No before/after-trigger arithmetic, ' +
  'loss estimate, causal attribution or continuous-health claim. ' +
  'Empty legacy reports do not establish detector support. ' +
  'At most 128 triggers are retained in client/attempt order; ' +
  'receiver snapshots remain in load_test_results.json.';
const issueList = (value, vocabulary) =>
  Array.isArray(value) &&
  value.length <= vocabulary.length &&
  value.every((issue, index) => vocabulary.includes(issue) && (!index || value[index - 1] < issue));

function samples(value) {
  return (
    Array.isArray(value) &&
    value.length <= 3 &&
    value.every(
      (row) =>
        exact(row, [
          'ordinal',
          'status',
          'scheduledElapsedMs',
          'startedElapsedMs',
          'finishedElapsedMs',
          'aliveAtStart',
          'aliveAtEnd',
        ]) &&
        ordinal(row.ordinal, 3) &&
        ['ok', 'error', 'missed'].includes(row.status) &&
        uint(row.scheduledElapsedMs) &&
        nullable(row.startedElapsedMs, elapsed) &&
        elapsed(row.finishedElapsedMs) &&
        ['aliveAtStart', 'aliveAtEnd'].every(
          (key) =>
            exact(row[key], ['server', 'generator']) &&
            bool(row[key].server) &&
            bool(row[key].generator),
        ),
    )
  );
}

function stream(value) {
  return (
    (exact(value, ['ssrc', 'packetCount', 'rtpBytes', 'workerTimestampMs']) ||
      (exact(value, [
        'ssrc',
        'packetCount',
        'rtpBytes',
        'workerTimestampMs',
        'score',
        'encodingIndex',
      ]) &&
        uint(value.score, 10) &&
        nullable(value.encodingIndex, (index) => uint(index, 255)))) &&
    uint(value.ssrc, 0xffffffff) &&
    ['packetCount', 'rtpBytes', 'workerTimestampMs'].every((key) => uint(value[key]))
  );
}

function consumerObservations(value) {
  return (
    Array.isArray(value) &&
    value.length <= 3 &&
    value.every(
      (row) =>
        ordinal(row.sampleOrdinal, 3) &&
        ((exact(row, ['sampleOrdinal', 'status']) &&
          ['not_observed', 'identity_mismatch'].includes(row.status)) ||
          (exact(row, [
            'sampleOrdinal',
            'status',
            'paused',
            'producerPaused',
            'transportReference',
            'observedUs',
            'stream',
          ]) &&
            entityStatuses.includes(row.status) &&
            nullable(row.paused, bool) &&
            nullable(row.producerPaused, bool) &&
            nullable(row.transportReference, reference) &&
            uint(row.observedUs) &&
            nullable(row.stream, stream))),
    )
  );
}

function accounting(value, producer = false) {
  return (
    exact(value, ['movement', 'packetDelta', 'byteDelta']) &&
    [...movements, ...(producer ? ['stream_set_changed', 'counter_overflow'] : [])].includes(
      value.movement,
    ) &&
    nullable(value.packetDelta, uint) &&
    nullable(value.byteDelta, uint)
  );
}

function generator(value) {
  return (
    exact(value, [
      'clientOrdinal',
      'consumerOrdinal',
      'attemptOrdinal',
      'packetsInMeasurement',
      'eligibleSeconds',
      'secondsWithPackets',
      'longestGapSeconds',
      'deliveryPassed',
      'resumeRequested',
      'resumeAcknowledged',
      'firstRtpEventObserved',
    ]) &&
    ordinal(value.clientOrdinal, 100) &&
    ordinal(value.consumerOrdinal, 2048) &&
    ordinal(value.attemptOrdinal, 128) &&
    uint(value.packetsInMeasurement) &&
    ['eligibleSeconds', 'secondsWithPackets', 'longestGapSeconds'].every((key) =>
      uint(value[key], 3600),
    ) &&
    ['deliveryPassed', 'resumeRequested', 'resumeAcknowledged', 'firstRtpEventObserved'].every(
      (key) => bool(value[key]),
    )
  );
}

function mediaReport(value) {
  const counts = [
    'expectedConsumers',
    'matchedConsumers',
    'shortLivedUnobservedConsumers',
    'consumersWithCounterPairs',
  ];
  return (
    exact(value, ['schemaVersion', 'coverage', 'samples', 'consumers', 'interpretation']) &&
    value.schemaVersion === 1 &&
    exact(value.coverage, ['available', 'complete', 'issues', ...counts]) &&
    bool(value.coverage.available) &&
    bool(value.coverage.complete) &&
    issueList(value.coverage.issues, mediaIssues) &&
    counts.every((key) => uint(value.coverage[key], MAX_CONSUMERS)) &&
    samples(value.samples) &&
    Array.isArray(value.consumers) &&
    value.consumers.length <= MAX_CONSUMERS &&
    value.consumers.length === value.coverage.expectedConsumers &&
    value.consumers.every(
      (row) =>
        exact(row, [
          'consumerReference',
          'producerReference',
          'ssrc',
          'kind',
          'generator',
          'observations',
          'nativeAccounting',
        ]) &&
        reference(row.consumerReference) &&
        reference(row.producerReference) &&
        uint(row.ssrc, 0xffffffff) &&
        kind(row.kind) &&
        generator(row.generator) &&
        consumerObservations(row.observations) &&
        accounting(row.nativeAccounting),
    ) &&
    value.interpretation === mediaInterpretation &&
    (!value.coverage.complete ||
      (value.coverage.available &&
        value.coverage.issues.length === 0 &&
        value.coverage.matchedConsumers > 0))
  );
}

function producerObservations(value) {
  return (
    Array.isArray(value) &&
    value.length <= 3 &&
    value.every(
      (row) =>
        exact(row, [
          'sampleOrdinal',
          'status',
          'paused',
          'transportReference',
          'observedUs',
          'streams',
        ]) &&
        ordinal(row.sampleOrdinal, 3) &&
        [...entityStatuses, 'not_observed', 'identity_mismatch'].includes(row.status) &&
        nullable(row.paused, bool) &&
        nullable(row.transportReference, reference) &&
        nullable(row.observedUs, uint) &&
        Array.isArray(row.streams) &&
        row.streams.length <= 16 &&
        row.streams.every(stream),
    )
  );
}

function stallReport(value) {
  if (
    !exact(value, ['schemaVersion', 'coverage', 'samples', 'stalls', 'interpretation']) ||
    value.schemaVersion !== 1 ||
    !exact(value.coverage, [
      'available',
      'complete',
      'scope',
      'issues',
      'triggersObserved',
      'entriesRetained',
    ]) ||
    !bool(value.coverage.available) ||
    !nullable(value.coverage.complete, bool) ||
    value.coverage.scope !== 'recorded_stall_triggers' ||
    !issueList(value.coverage.issues, stallIssues) ||
    !uint(value.coverage.triggersObserved, 12800) ||
    !uint(value.coverage.entriesRetained, 128) ||
    !samples(value.samples) ||
    !Array.isArray(value.stalls) ||
    value.stalls.length !== value.coverage.entriesRetained ||
    value.interpretation !== stallInterpretation
  )
    return false;
  for (const row of value.stalls) {
    if (
      !exact(row, [
        'clientOrdinal',
        'attemptOrdinal',
        'consumerOrdinal',
        'trigger',
        'receiverCapture',
        'consumerReference',
        'producerReference',
        'kind',
        'delivery',
        'consumer',
        'producer',
        'issues',
      ]) ||
      !ordinal(row.clientOrdinal, 100) ||
      !ordinal(row.attemptOrdinal, 128) ||
      !ordinal(row.consumerOrdinal, 2048) ||
      !kind(row.kind) ||
      !nullable(row.consumerReference, reference) ||
      !nullable(row.producerReference, reference) ||
      !issueList(row.issues, stallIssues) ||
      !exact(row.trigger, [
        'consumerOrdinal',
        'ssrc',
        'isAudio',
        'beginBucket',
        'endBucket',
        'elapsedMs',
      ]) ||
      row.trigger.consumerOrdinal !== row.consumerOrdinal ||
      !uint(row.trigger.ssrc, 0xffffffff) ||
      !bool(row.trigger.isAudio) ||
      !uint(row.trigger.beginBucket, 3600) ||
      !uint(row.trigger.endBucket, 3600) ||
      !uint(row.trigger.elapsedMs) ||
      !exact(row.receiverCapture, [
        'status',
        'completedElapsedMs',
        'snapshotOrdinal',
        'peerState',
      ]) ||
      ![
        'captured',
        'busy',
        'budget_exhausted',
        'timed_out',
        'attempt_ended',
        'unavailable',
        'snapshot_rejected',
        'cancelled',
        'missing',
      ].includes(row.receiverCapture.status) ||
      !nullable(row.receiverCapture.completedElapsedMs, uint) ||
      !nullable(
        row.receiverCapture.snapshotOrdinal,
        (ordinal) => uint(ordinal, 8) && ordinal > 0,
      ) ||
      !nullable(row.receiverCapture.peerState, (state) =>
        ['new', 'connecting', 'connected', 'disconnected', 'failed', 'closed'].includes(state),
      ) ||
      !exact(row.delivery, [
        'passed',
        'eligibleSeconds',
        'secondsWithPackets',
        'longestGapSeconds',
      ]) ||
      !bool(row.delivery.passed) ||
      !['eligibleSeconds', 'secondsWithPackets', 'longestGapSeconds'].every((key) =>
        uint(row.delivery[key], 3600),
      ) ||
      !exact(row.consumer, ['observations', 'nativeAccounting']) ||
      !consumerObservations(row.consumer.observations) ||
      !accounting(row.consumer.nativeAccounting) ||
      !exact(row.producer, ['observations', 'nativeAccounting']) ||
      !producerObservations(row.producer.observations) ||
      !accounting(row.producer.nativeAccounting, true)
    )
      return false;
  }
  return (
    value.coverage.complete !== true ||
    (value.coverage.available && value.coverage.issues.length === 0)
  );
}

async function read(generator, mode, path, samples, admitted) {
  const failure = () => new Error('media_report_command_failed');
  let input;
  try {
    input = JSON.stringify(samples) ?? 'null';
  } catch {
    throw failure();
  }
  if (Buffer.byteLength(input) > MAX_INPUT || typeof admitted !== 'boolean') throw failure();
  const env = Object.fromEntries(
    ['PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT']
      .filter((key) => process.env[key])
      .map((key) => [key, process.env[key]]),
  );
  const args = ['report', mode, path, ...(admitted ? [] : ['--results-unavailable'])];
  const result = await new Promise((resolve, reject) => {
    let inputFailed = false;
    const child = execFile(
      generator,
      args,
      { env, maxBuffer: MAX_OUTPUT, timeout: 30000, killSignal: 'SIGKILL' },
      (error, stdout) => {
        if (inputFailed || (error && (error.code !== 1 || error.killed || error.signal))) {
          reject(failure());
          return;
        }
        resolve({ stdout, status: error ? 1 : 0 });
      },
    );
    if (!child.stdin) {
      inputFailed = true;
      return;
    }
    child.stdin.on('error', () => {
      inputFailed = true;
    });
    try {
      child.stdin.end(input);
    } catch {
      inputFailed = true;
      child.kill('SIGKILL');
    }
  }).catch(() => {
    throw failure();
  });
  try {
    const report = JSON.parse(result.stdout);
    if (
      !(mode === 'media' ? mediaReport(report) : stallReport(report)) ||
      (report.coverage.complete === true) !== (result.status === 0)
    )
      throw failure();
    return report;
  } catch {
    throw failure();
  }
}

/** Correlate stopped workload evidence using the frozen selected generator.
 * `admitted:false` prevents re-reading a results artifact rejected by its owner.
 * Exit1 retains an incomplete report; failures expose only a fixed error code.
 */
export const readMediaReportWithGenerator = (generator, path, samples, { admitted = true } = {}) =>
  read(generator, 'media', path, samples, admitted);

/** Receiver evidence is a separate report; empty legacy evidence has null
 * completeness and remains available without claiming detector support.
 */
export const readReceiverStallReportWithGenerator = (
  generator,
  path,
  samples,
  { admitted = true } = {},
) => read(generator, 'receiver-stall', path, samples, admitted);
