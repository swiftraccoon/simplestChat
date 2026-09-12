// Post-run identity correlation only. Scheduled SFU samples are not stall-triggered samples.
import { analyzeMediaDiagnostics, generatorConsumers, mediaReference } from './media-diagnostic-report.mjs';

const MAX_STALLS = 128;
const uint = value => Number.isSafeInteger(value) && value >= 0;
const keys = (value, names) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === names.length && names.every(name => Object.hasOwn(value, name));
const captureStatuses = ['captured', 'busy', 'budget_exhausted', 'timed_out', 'attempt_ended',
  'unavailable', 'snapshot_rejected', 'cancelled'];
const peerStates = ['new', 'connecting', 'connected', 'disconnected', 'failed', 'closed'];
const invalid = () => { throw new Error('invalid_stall_evidence'); };
const triggerKeys = ['consumerOrdinal', 'ssrc', 'isAudio', 'beginBucket', 'endBucket'];
const equalTrigger = (left, right) => keys(right, triggerKeys) && triggerKeys.every(key => left[key] === right[key]);

function stallEvidence(results, identities) {
  const identitiesByOrdinal = new Map(identities.map(identity =>
    [`${identity.generator.clientOrdinal}/${identity.generator.consumerOrdinal}`, identity]));
  const stalls = [], issues = new Set();
  let snapshots = 0;
  for (const [clientIndex, client] of results.entries()) {
    const clientOrdinal = clientIndex + 1;
    const triggers = new Map(), completions = new Map(), captures = new Map();
    for (const event of client.diagnostics.events) {
      if (event.kind === 'receiver-stall-triggered') {
        const detail = event.details, trigger = detail.trigger;
        if (!keys(detail, ['triggerElapsedMs', 'trigger']) || !uint(detail.triggerElapsedMs) ||
            event.elapsedMs < detail.triggerElapsedMs || !keys(trigger, triggerKeys) ||
            !uint(trigger.consumerOrdinal) || !trigger.consumerOrdinal || !uint(trigger.ssrc) || trigger.ssrc > 0xffffffff ||
            typeof trigger.isAudio !== 'boolean' || !uint(trigger.beginBucket) || !uint(trigger.endBucket) ||
            trigger.endBucket - trigger.beginBucket !== 3 || triggers.has(event.attempt)) invalid();
        const identity = identitiesByOrdinal.get(`${clientOrdinal}/${trigger.consumerOrdinal}`);
        const delivery = client.consumerDelivery[trigger.consumerOrdinal - 1];
        if (!identity || identity.generator.attemptOrdinal !== event.attempt || identity.ssrc !== trigger.ssrc ||
            identity.kind !== (trigger.isAudio ? 'audio' : 'video') || delivery.attempt !== event.attempt ||
            delivery.isAudio !== trigger.isAudio || delivery.passed || delivery.skippedShortLived ||
            delivery.longestGapSeconds < 3 || trigger.endBucket > delivery.packetsBySecond.length ||
            !delivery.packetsBySecond.slice(trigger.beginBucket, trigger.endBucket).every(count => count === 0)) invalid();
        triggers.set(event.attempt, { clientOrdinal, attemptOrdinal:event.attempt, consumerOrdinal:trigger.consumerOrdinal,
          trigger:{...trigger, elapsedMs:detail.triggerElapsedMs}, identity, delivery, eventElapsedMs:event.elapsedMs });
      } else if (event.kind === 'receiver-stall-capture') {
        if (!keys(event.details, ['triggerElapsedMs', 'status']) || !uint(event.details.triggerElapsedMs) ||
            event.elapsedMs < event.details.triggerElapsedMs || !captureStatuses.includes(event.details.status) ||
            completions.has(event.attempt)) invalid();
        completions.set(event.attempt, event);
      }
    }
    const stored = Object.hasOwn(client.diagnostics, 'receiverStalls') ? client.diagnostics.receiverStalls : [];
    if (!Array.isArray(stored) || stored.length > 8) invalid();
    snapshots += stored.length;
    if (snapshots > 8) invalid();
    for (const [index, entry] of stored.entries()) {
      if (!keys(entry, ['attempt', 'elapsedMs', 'kind', 'details']) || !uint(entry.attempt) || !entry.attempt ||
          entry.attempt > client.connectionAttempts.length || !uint(entry.elapsedMs) || entry.kind !== 'receiver-stall' ||
          !keys(entry.details, ['triggerElapsedMs', 'trigger', 'snapshot']) ||
          !uint(entry.details.triggerElapsedMs) || entry.elapsedMs < entry.details.triggerElapsedMs ||
          Buffer.byteLength(JSON.stringify(entry)) > 256 * 1024 || captures.has(entry.attempt)) invalid();
      const transports = entry.details.snapshot?.transports;
      if (!Array.isArray(transports) || transports.length !== 1 || transports[0]?.direction !== 'receive' ||
          !peerStates.includes(transports[0].connectionState) || !Array.isArray(transports[0].stats) ||
          transports[0].stats.length > 512 || !transports[0].stats.some(stat => stat?.type === 'transport') ||
          !Array.isArray(transports[0].consumerMappings) || transports[0].consumerMappings.length > 256) invalid();
      captures.set(entry.attempt, {entry, ordinal:index + 1, peer:transports[0]});
    }
    for (const attempt of [...completions.keys(), ...captures.keys()]) {
      if (!triggers.has(attempt)) issues.add('orphan_receiver_capture');
    }
    for (const [attempt, stall] of [...triggers].sort(([left], [right]) => left - right)) {
      const completion = completions.get(attempt), captured = captures.get(attempt);
      const entryIssues = [];
      if (completion && (completion.details.triggerElapsedMs !== stall.trigger.elapsedMs ||
          completion.elapsedMs < stall.eventElapsedMs)) invalid();
      if (captured && (captured.entry.details.triggerElapsedMs !== stall.trigger.elapsedMs ||
          captured.entry.elapsedMs < stall.eventElapsedMs ||
          !equalTrigger(stall.trigger, captured.entry.details.trigger) ||
          !captured.peer.consumerMappings.some(mapping => mapping?.ssrc === stall.trigger.ssrc) ||
          (completion && captured.entry.elapsedMs > completion.elapsedMs))) invalid();
      const status = completion?.details.status ?? 'missing';
      if (status !== 'captured') entryIssues.push('receiver_capture_incomplete');
      if (status === 'captured' && !captured) entryIssues.push('receiver_snapshot_missing');
      if (captured && status !== 'captured') entryIssues.push('receiver_capture_contradiction');
      stalls.push({...stall, receiverCapture:{status, completedElapsedMs:completion?.elapsedMs ?? null,
        snapshotOrdinal:captured?.ordinal ?? null, peerState:captured?.peer.connectionState ?? null}, issues:entryIssues});
    }
  }
  return {stalls, issues};
}

// Producer SSRCs are not consumer SSRCs. Compare all streams only while the
// transport and complete SSRC set remain stable across the scheduled samples.
function producerAccounting(observations) {
  const usable = observations.filter(row => row.status === 'ok' && row.streams.length);
  const result = {movement:usable.length ? 'single_observation' : 'unavailable', packetDelta:null, byteDelta:null};
  if (usable.length < 2) return result;
  if (usable.some(row => !row.transportReference)) return {...result, movement:'identity_unavailable'};
  for (let index = 1; index < usable.length; index++) {
    const before = usable[index - 1], after = usable[index];
    if (before.transportReference !== after.transportReference) return {...result, movement:'reset_or_replaced'};
    const previous = new Map(before.streams.map(stream => [stream.ssrc, stream]));
    if (previous.size !== after.streams.length || after.streams.some(stream => !previous.has(stream.ssrc))) {
      return {...result, movement:'stream_set_changed'};
    }
    if (after.streams.some(stream => {
      const prior = previous.get(stream.ssrc);
      return stream.packetCount < prior.packetCount || stream.rtpBytes < prior.rtpBytes || stream.workerTimestampMs < prior.workerTimestampMs;
    })) return {...result, movement:'reset_or_replaced'};
  }
  const total = (row, field) => row.streams.reduce((sum, stream) => sum + stream[field], 0);
  const first = usable[0], last = usable.at(-1);
  const totals = [total(first, 'packetCount'), total(last, 'packetCount'), total(first, 'rtpBytes'), total(last, 'rtpBytes')];
  if (!totals.every(uint)) return {...result, movement:'counter_overflow'};
  const packetDelta = totals[1] - totals[0], byteDelta = totals[3] - totals[2];
  return {movement:packetDelta > 0 ? 'increased' : totals[1] === 0 ? 'flat_zero' : 'flat_nonzero', packetDelta, byteDelta};
}

/** Keep failure evidence separate from scheduled-sample coverage and workload
 * success. Never infer temporal proximity by subtracting independent clocks.
 */
export function correlateReceiverStalls(samples, generatorResults) {
  const report = {schemaVersion:1, coverage:{available:false, complete:false, scope:'recorded_stall_triggers',
    issues:[], triggersObserved:0, entriesRetained:0}, samples:[], stalls:[],
    interpretation:'Identity correlation with retained scheduled SFU samples, not observations at the stall. Trigger elapsedMs uses the client collector clock; sample elapsedMs uses the runner clock; observedUs and workerTimestampMs retain native clocks. No before/after-trigger arithmetic, loss estimate, causal attribution or continuous-health claim. Empty legacy reports do not establish detector support. At most 128 triggers are retained in client/attempt order; receiver snapshots remain in load_test_results.json.'};
  let evidence;
  try {
    const identities = generatorConsumers(generatorResults, {allowDiagnosticFailures:true});
    evidence = stallEvidence(generatorResults, identities);
  } catch {
    report.coverage.issues = ['generator_stall_evidence_unavailable'];
    return report;
  }
  report.coverage.available = true;
  report.coverage.triggersObserved = evidence.stalls.length;
  const issues = evidence.issues;
  if (generatorResults.some(client => client.diagnostics.failures.length)) issues.add('generator_diagnostics_incomplete');
  if (!evidence.stalls.length) {
    report.coverage.complete = issues.size ? false : null;
    report.coverage.issues = [...issues].sort();
    return report;
  }
  const {report:media, validatedSamples} = analyzeMediaDiagnostics(samples, generatorResults);
  report.samples = media.samples;
  if (!media.coverage.complete) issues.add('scheduled_media_evidence_incomplete');
  if (evidence.stalls.length > MAX_STALLS) issues.add('stall_report_limit');
  const consumerRows = new Map(media.consumers.map(row =>
    [`${row.generator.clientOrdinal}/${row.generator.consumerOrdinal}`, row]));
  for (const stall of evidence.stalls.slice(0, MAX_STALLS)) {
    const row = consumerRows.get(`${stall.clientOrdinal}/${stall.consumerOrdinal}`);
    const entryIssues = new Set(stall.issues);
    if (!row || row.observations.filter(o => o.status === 'ok' && o.stream && o.transportReference).length < 2) {
      entryIssues.add('consumer_counter_pair_missing');
    }
    if (row && ['reset_or_replaced', 'identity_unavailable'].includes(row.nativeAccounting.movement)) {
      entryIssues.add('consumer_counter_comparison_unavailable');
    }
    const observations = [];
    // Existing consumer observations enumerate only schema/namespace-validated
    // native samples. Missing producers are explicit; do not borrow another SSRC.
    for (const observation of row?.observations ?? []) {
      const sample = validatedSamples.find(sample => sample.ordinal === observation.sampleOrdinal);
      const reference = mediaReference(sample.snapshot.correlationSalt, 'producer', stall.identity.producerId);
      const producer = sample.snapshot.entities.find(entity => entity.entityType === 'producer' && entity.reference === reference);
      if (!producer) {
        observations.push({sampleOrdinal:sample.ordinal, status:'not_observed', paused:null, transportReference:null, observedUs:null, streams:[]});
      } else if (producer.kind !== stall.identity.kind) {
        entryIssues.add('producer_identity_mismatch');
        observations.push({sampleOrdinal:sample.ordinal, status:'identity_mismatch', paused:null, transportReference:null, observedUs:null, streams:[]});
      } else {
        observations.push({sampleOrdinal:sample.ordinal, status:producer.status, paused:producer.paused,
          transportReference:producer.transportReference, observedUs:producer.observedUs,
          streams:producer.streams.map(stream => ({...stream}))});
      }
    }
    if (observations.filter(o => o.status === 'ok' && o.streams.length && o.transportReference).length < 2) {
      entryIssues.add('producer_counter_pair_missing');
    }
    const accounting = producerAccounting(observations);
    if (['reset_or_replaced', 'stream_set_changed', 'identity_unavailable', 'counter_overflow'].includes(accounting.movement)) {
      entryIssues.add('producer_counter_comparison_unavailable');
    }
    for (const issue of entryIssues) issues.add(issue);
    report.stalls.push({clientOrdinal:stall.clientOrdinal, attemptOrdinal:stall.attemptOrdinal, consumerOrdinal:stall.consumerOrdinal,
      trigger:stall.trigger, receiverCapture:stall.receiverCapture,
      consumerReference:row?.consumerReference ?? null, producerReference:row?.producerReference ?? null, kind:stall.identity.kind,
      delivery:{passed:stall.delivery.passed, eligibleSeconds:stall.delivery.eligibleSeconds,
        secondsWithPackets:stall.delivery.secondsWithPackets, longestGapSeconds:stall.delivery.longestGapSeconds},
      consumer:{observations:row?.observations ?? [], nativeAccounting:row?.nativeAccounting ?? {movement:'unavailable', packetDelta:null, byteDelta:null}},
      producer:{observations, nativeAccounting:accounting}, issues:[...entryIssues].sort()});
  }
  report.coverage.entriesRetained = report.stalls.length;
  report.coverage.issues = [...issues].sort();
  report.coverage.complete = !issues.size;
  return report;
}
