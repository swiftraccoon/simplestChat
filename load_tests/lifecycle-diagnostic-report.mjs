// Offline lifecycle evidence from an owned server; never native causal attribution.
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';

export const LIFECYCLE_LIMITS = Object.freeze({ bytes: 32 * 1024 * 1024, lines: 100000, records: 10000, lineBytes: 16 * 1024 });
const phases = ['setup', 'measurement', 'departure', 'post_cleanup', 'shutdown', 'unknown'];
const anchors = ['run_started', 'server_ready', 'generator_started', 'generator_exited', 'cleanup_observed', 'server_stop_requested', 'server_exited'];
const uuid = value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value);
const exact = (value, names) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === names.length && names.every(name => Object.hasOwn(value, name));
const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= Number.MAX_SAFE_INTEGER;
const integer = value => Number.isSafeInteger(value) && value >= 0;
const events = new Map([
  ['transport_created', ['event', 'participant_id', 'transport_id', 'transport_type']],
  ['transport_closed', ['event', 'participant_id', 'transport_id', 'transport_type']],
  ['transport_ice', ['event', 'participant_id', 'transport_id', 'transport_type', 'state']],
  ['transport_dtls', ['event', 'participant_id', 'transport_id', 'transport_type', 'state']],
  ['media_cleanup_started', ['event', 'participant_id', 'generation']],
  ['media_cleanup_finished', ['event', 'participant_id', 'generation']],
  ...['explicit_leave_started', 'explicit_leave_finished', 'grace_started', 'grace_cleanup_started'].map(name => [name, ['event', 'participant_id']]),
]);

// Strict calendar checking avoids Date.parse silently normalizing invalid dates.
function instant(value) {
  if (typeof value !== 'string') return null;
  const match = /^(\d{4}-\d\d-\d\d)T(\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?(?:Z|\+00:00)$/.exec(value);
  if (!match) return null;
  const whole = Date.parse(`${match[1]}T${match[2]}Z`);
  if (!Number.isFinite(whole) || new Date(whole).toISOString().slice(0, 19) !== `${match[1]}T${match[2]}`) return null;
  const ns = BigInt(whole) * 1000000n + BigInt((match[3] ?? '').padEnd(9, '0'));
  return { at: value, ms: whole + Number(`0.${match[3] ?? '0'}`) * 1000, ns };
}

function timelineView(value, issue) {
  const result = { departure: null, events: [], nominalMeasurement: null };
  const known = new Map();
  let safe = true;
  if (!exact(value, ['schemaVersion', 'departure', 'events', 'workload']) || value.schemaVersion !== 1 ||
      !['abrupt', 'explicit-leave'].includes(value.departure) || !Array.isArray(value.events) || value.events.length > anchors.length) {
    issue('invalid_timeline'); return { output: result, phase: () => 'unknown' };
  }
  result.departure = value.departure;
  let previous;
  for (const row of value.events) {
    const at = instant(row?.at);
    if (!exact(row, ['event', 'at', 'elapsedMs']) || !anchors.includes(row.event) || known.has(row.event) || !at || !finite(row.elapsedMs)) {
      issue('invalid_timeline'); safe = false; continue;
    }
    if (previous && (anchors.indexOf(row.event) <= anchors.indexOf(previous.event) || row.elapsedMs < previous.elapsedMs)) {
      issue('timeline_order'); safe = false;
    }
    if (previous && Math.abs(at.ms - previous.time.ms - (row.elapsedMs - previous.elapsedMs)) > 250) {
      issue('timeline_clock_shift'); safe = false;
    }
    known.set(row.event, at);
    result.events.push({ event: row.event, at: at.at, elapsedMs: row.elapsedMs });
    previous = { ...row, time: at };
  }
  if (anchors.some(name => !known.has(name))) issue('missing_timeline_anchor');
  let start, end;
  const workload = value.workload;
  const workloadStart = instant(workload?.startedAt), workloadFinish = instant(workload?.finishedAt);
  if (!exact(workload, ['startedAt', 'finishedAt', 'rampUpSecs', 'warmupSecs', 'durationSecs']) || !workloadStart || !workloadFinish ||
      workloadFinish.ns < workloadStart.ns || !['rampUpSecs', 'warmupSecs', 'durationSecs'].every(key => integer(workload[key])) ||
      workload.rampUpSecs > 600 || workload.warmupSecs > 60 || workload.durationSecs < 3 || workload.durationSecs > 180) {
    issue('invalid_workload_timeline');
  } else {
    start = workloadStart.ns + BigInt(workload.rampUpSecs + workload.warmupSecs) * 1000000000n;
    end = start + BigInt(workload.durationSecs) * 1000000000n;
    result.nominalMeasurement = { startedAt: new Date(Number(start / 1000000n)).toISOString(), finishedAt: new Date(Number(end / 1000000n)).toISOString() };
    if (workloadFinish.ns < end || (known.has('generator_started') && workloadStart.ms < known.get('generator_started').ms - 250) ||
        (known.has('generator_exited') && workloadFinish.ms > known.get('generator_exited').ms + 250) ||
        (known.has('cleanup_observed') && known.get('cleanup_observed').ns < end)) {
      issue('workload_timeline_order'); safe = false;
    }
  }
  return { output: result, phase: time => {
    if (!safe || !known.has('run_started') || time.ns < known.get('run_started').ns ||
        (known.has('server_exited') && time.ns > known.get('server_exited').ns)) return 'unknown';
    if (known.has('server_stop_requested') && time.ns >= known.get('server_stop_requested').ns) return 'shutdown';
    if (known.has('cleanup_observed') && time.ns >= known.get('cleanup_observed').ns) return 'post_cleanup';
    if (end !== undefined && time.ns >= end) return 'departure';
    if (start !== undefined && time.ns >= start) return 'measurement';
    return start !== undefined ? 'setup' : 'unknown';
  } };
}

function markerFields(text) {
  const fields = {};
  let remaining = text;
  while (remaining) {
    const match = /^([a-z_]+)=(?:"([^"\\]*)"|([^\s"=]+))(?: +|$)/.exec(remaining);
    if (!match || Object.hasOwn(fields, match[1])) return null;
    fields[match[1]] = match[2] ?? match[3];
    remaining = remaining.slice(match[0].length);
  }
  if (!events.has(fields.event) || !exact(fields, events.get(fields.event)) || !uuid(fields.participant_id) ||
      ('transport_id' in fields && (!uuid(fields.transport_id) || !['send', 'recv'].includes(fields.transport_type))) ||
      ('generation' in fields && !uuid(fields.generation)) ||
      (fields.event === 'transport_ice' && !['New', 'Connected', 'Completed', 'Disconnected'].includes(fields.state)) ||
      (fields.event === 'transport_dtls' && !['New', 'Connecting', 'Connected', 'Failed', 'Closed'].includes(fields.state))) return null;
  return fields;
}

/** Coverage concerns expected instrumented markers only. Read after the owned
 * server stops. Errors never include source lines, paths, UUIDs or credentials.
 */
export async function readLifecycleReport(logPath, timeline, { expectedParticipants } = {}) {
  const issues = new Set();
  const issue = code => issues.add(code);
  const view = timelineView(timeline, issue);
  const report = { schemaVersion: 1, coverage: { available: false, complete: false, scope: 'unavailable', issues: [] },
    timeline: view.output, linesRead: 0, relevantRecords: 0, lifecycleEvents: [], clamps: [],
    phases: Object.fromEntries(phases.map(phase => [phase, { lifecycleEvents: 0, clamps: 0 }])), totals: null,
    interpretation: 'Coverage means expected instrumented lifecycle markers, not comprehensive native capture or successful server exit. Measurement boundaries are nominal wall-clock anchors, not individual media activity. Rows retain log observation order; asynchronous callbacks and formatter ordering can differ from underlying state order. Clamps contain only previously observed aggregate state, never a transport identity or causal attribution. Small timestamp reordering (up to 250ms) is tolerated; larger regressions make coverage incomplete. Explicit leave completion alone does not establish media cleanup.' };
  if (!Number.isInteger(expectedParticipants) || expectedParticipants < 1 || expectedParticipants > 10000) issue('invalid_expected_participants');
  const participants = new Map(), transports = new Map(), cleanups = new Map();
  let lastTimestamp, disconnectedEvents = 0;
  const participant = id => {
    if (!participants.has(id)) participants.set(id, { ordinal: participants.size + 1, markers: new Set() });
    return participants.get(id);
  };
  const totals = () => {
    const allTransports = [...transports.values()], allCleanups = [...cleanups.values()];
    const openTransports = allTransports.filter(row => row.created && !row.closed);
    return { participantsObserved: participants.size, transportsCreated: allTransports.filter(row => row.created).length,
      transportsClosed: allTransports.filter(row => row.closed).length, transportsOpen: openTransports.length,
      openTransportsIceDisconnected: openTransports.filter(row => row.ice === 'Disconnected').length,
      openTransportsDtlsFailedOrClosed: openTransports.filter(row => ['Failed', 'Closed'].includes(row.dtls)).length,
      openTransportsWithDisconnectState: openTransports.filter(row => row.ice === 'Disconnected' || ['Failed', 'Closed'].includes(row.dtls)).length,
      iceDisconnectedEvents: disconnectedEvents,
      cleanupGenerationsStarted: allCleanups.filter(row => row.started).length,
      cleanupGenerationsFinished: allCleanups.filter(row => row.finished).length,
      cleanupGenerationsInProgress: allCleanups.filter(row => row.started && !row.finished).length,
      participantsWithCleanupFinished: new Set(allCleanups.filter(row => row.started && row.finished).map(row => row.participantId)).size,
      explicitLeavesFinished: [...participants.values()].filter(row => row.markers.has('explicit_leave_finished')).length,
      graceCleanupsStarted: [...participants.values()].filter(row => row.markers.has('grace_cleanup_started')).length };
  };
  const consume = buffer => {
    report.linesRead++;
    if (report.linesRead > LIFECYCLE_LIMITS.lines) { issue('line_count_limit'); return false; }
    if (buffer.length > LIFECYCLE_LIMITS.lineBytes) { issue('line_length_limit'); return false; }
    let line;
    // Strip only the SGR color sequences emitted by the tracing formatter.
    // eslint-disable-next-line no-control-regex
    try { line = new TextDecoder('utf-8', { fatal: true }).decode(buffer).replace(/\x1b\[[0-9;]*m/g, ''); }
    catch { issue('invalid_log_encoding'); return true; }
    const lifecycle = line.includes('simplestChat::lifecycle');
    const clamp = line.includes('start bitrate smaller than min bitrate') || line.includes('ClampConstraints()');
    if (!lifecycle && !clamp) return true;
    // Legacy INFO lines contain namespace separators; only our marker fields
    // have the strict UUID-only contract. Never echo either kind of raw line.
    // eslint-disable-next-line no-control-regex
    if (/[\x00-\x1f\x7f-\x9f\u2028\u2029\u202a-\u202e\u2066-\u2069]/.test(line)) { issue('invalid_log_control'); return true; }
    if (report.relevantRecords >= LIFECYCLE_LIMITS.records) { issue('record_limit'); return false; }
    report.relevantRecords++;
    const match = lifecycle
      ? /^(\S+) +DEBUG +(?:.* )?simplestChat::lifecycle: lifecycle +(.*)$/.exec(line)
      : /^(\S+) +ERROR +(?:.* )?mediasoup::worker: (?:\[id:([0-9a-f-]+)\] )?webrtc::GoogCcNetworkController::ClampConstraints\(\) \| start bitrate smaller than min bitrate \[starting_rate_:30000, min_data_rate_:100000\]$/.exec(line);
    const time = instant(match?.[1]);
    const fields = lifecycle && match ? markerFields(match[2]) : null;
    if (!match || !time || (lifecycle && !fields) || (!lifecycle && match[2] && !uuid(match[2]))) { issue('invalid_lifecycle_record'); return true; }
    if (lastTimestamp !== undefined && lastTimestamp - time.ns > 250000000n) issue('log_clock_regression');
    if (lastTimestamp === undefined || time.ns > lastTimestamp) lastTimestamp = time.ns;
    const phase = view.phase(time);
    if (phase === 'unknown') issue('unknown_record_phase');
    if (!lifecycle) {
      report.clamps.push({ ordinal: report.clamps.length + 1, at: time.at, phase,
        signature: 'start_bitrate_below_minimum_30000_100000', previouslyObserved: totals() });
      report.phases[phase].clamps++;
      return true;
    }
    const owner = participant(fields.participant_id);
    const row = { ordinal: report.lifecycleEvents.length + 1, at: time.at, phase, event: fields.event, participantOrdinal: owner.ordinal };
    if (fields.transport_id) {
      if (!transports.has(fields.transport_id)) transports.set(fields.transport_id, { ordinal: transports.size + 1,
        participantId: fields.participant_id, type: fields.transport_type, created: false, closed: false,
        iceReadyObserved: false, dtlsReadyObserved: false, ice: null, dtls: null });
      const transport = transports.get(fields.transport_id);
      if (transport.participantId !== fields.participant_id || transport.type !== fields.transport_type) { issue('transport_identity_conflict'); return true; }
      row.transportOrdinal = transport.ordinal; row.transportType = transport.type;
      if (fields.event === 'transport_created') { if (transport.created) issue('duplicate_transport_created'); transport.created = true; }
      if (fields.event === 'transport_closed') { if (transport.closed) issue('duplicate_transport_closed'); transport.closed = true; }
      if (fields.state) {
        row.state = fields.state;
        transport[fields.event === 'transport_ice' ? 'ice' : 'dtls'] = fields.state;
        if (fields.event === 'transport_ice' && ['Connected', 'Completed'].includes(fields.state)) transport.iceReadyObserved = true;
        if (fields.event === 'transport_dtls' && fields.state === 'Connected') transport.dtlsReadyObserved = true;
        if (fields.event === 'transport_ice' && fields.state === 'Disconnected') disconnectedEvents++;
      }
    } else if (fields.generation) {
      if (!cleanups.has(fields.generation)) cleanups.set(fields.generation, { ordinal: cleanups.size + 1, participantId: fields.participant_id, started: false, finished: false });
      const cleanup = cleanups.get(fields.generation);
      if (cleanup.participantId !== fields.participant_id) { issue('cleanup_identity_conflict'); return true; }
      const key = fields.event === 'media_cleanup_started' ? 'started' : 'finished';
      if (cleanup[key]) issue('duplicate_cleanup_marker');
      cleanup[key] = true;
      row.cleanupOrdinal = cleanup.ordinal;
    } else owner.markers.add(fields.event);
    report.lifecycleEvents.push(row);
    report.phases[phase].lifecycleEvents++;
    return true;
  };
  let file;
  try {
    file = await open(logPath, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const info = await file.stat();
    if (!info.isFile() || (info.mode & 0o077)) issue('unsafe_file');
    else {
      report.coverage.available = true;
      if (info.size > LIFECYCLE_LIMITS.bytes) issue('file_limit');
      const buffer = Buffer.alloc(64 * 1024);
      let pending = Buffer.alloc(0), bytes = 0, halted = false;
      while (!halted && bytes < LIFECYCLE_LIMITS.bytes) {
        const read = await file.read(buffer, 0, Math.min(buffer.length, LIFECYCLE_LIMITS.bytes - bytes), null);
        if (!read.bytesRead) break;
        bytes += read.bytesRead;
        pending = Buffer.concat([pending, buffer.subarray(0, read.bytesRead)]);
        let newline;
        while ((newline = pending.indexOf(10)) !== -1) {
          if (!consume(pending.subarray(0, newline))) { halted = true; break; }
          pending = pending.subarray(newline + 1);
        }
        if (!halted && pending.length > LIFECYCLE_LIMITS.lineBytes) { issue('line_length_limit'); halted = true; }
      }
      if (!halted && pending.length) issue('unterminated_line');
      const after = await file.stat();
      if (after.size > LIFECYCLE_LIMITS.bytes) issue('file_limit');
      if (after.size !== info.size || after.mtimeMs !== info.mtimeMs || after.ctimeMs !== info.ctimeMs) issue('log_changed_during_read');
    }
  } catch (error) { issue(error?.code === 'ENOENT' ? 'file_unavailable' : error?.code === 'ELOOP' ? 'unsafe_file' : 'file_unreadable'); }
  finally { if (file) await file.close().catch(() => issue('file_close_failed')); }
  report.totals = totals();
  if (!report.lifecycleEvents.length) issue('missing_lifecycle_markers');
  if (report.totals.participantsWithCleanupFinished < expectedParticipants) issue('expected_participant_cleanup_missing');
  for (const transport of transports.values()) {
    if (!transport.created) issue('transport_created_missing');
    if (!transport.closed) issue('transport_closed_missing');
    // Readiness coverage is cumulative: a later disconnect/close must retain
    // evidence that setup reached readiness, without rewriting latest state.
    if (transport.created && !transport.iceReadyObserved) issue('transport_ice_ready_missing');
    if (transport.created && !transport.dtlsReadyObserved) issue('transport_dtls_ready_missing');
  }
  for (const cleanup of cleanups.values()) if (!cleanup.started || !cleanup.finished) issue('cleanup_marker_missing');
  for (const [id, owner] of participants) {
    if (![...transports.values()].some(row => row.participantId === id && row.type === 'recv' && row.created)) issue('participant_recv_transport_missing');
    if (![...cleanups.values()].some(row => row.participantId === id && row.started && row.finished)) issue('participant_cleanup_missing');
    if (view.output.departure === 'explicit-leave' && (!owner.markers.has('explicit_leave_started') || !owner.markers.has('explicit_leave_finished'))) issue('explicit_leave_marker_missing');
    if (view.output.departure === 'abrupt' && (!owner.markers.has('grace_started') || !owner.markers.has('grace_cleanup_started'))) issue('grace_cleanup_marker_missing');
  }
  report.coverage.issues = [...issues].sort();
  report.coverage.complete = report.coverage.available && !issues.size;
  report.coverage.scope = !report.coverage.available ? 'unavailable' : report.coverage.complete ? 'expected_instrumented_markers' : 'observed_subset';
  return report;
}
