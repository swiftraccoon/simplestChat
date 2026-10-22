// Owned, bounded offline generator invocation. Unknown child JSON never enters
// retained artifacts, even when the executable exits with incomplete coverage.
import { execFile } from 'node:child_process';

const LIMIT = 10000;
const phases = ['setup', 'measurement', 'departure', 'post_cleanup', 'shutdown', 'unknown'];
const anchors = ['run_started', 'server_ready', 'generator_started', 'generator_exited', 'cleanup_observed', 'server_stop_requested', 'server_exited'];
const markerNames = ['transport_created', 'transport_closed', 'transport_ice', 'transport_dtls', 'media_cleanup_started', 'media_cleanup_finished', 'explicit_leave_started', 'explicit_leave_finished', 'grace_started', 'grace_cleanup_started'];
const issues = new Set(['cleanup_identity_conflict', 'cleanup_marker_missing', 'duplicate_cleanup_marker', 'duplicate_transport_closed', 'duplicate_transport_created', 'expected_participant_cleanup_missing', 'explicit_leave_marker_missing', 'file_close_failed', 'file_limit', 'file_unavailable', 'file_unreadable', 'grace_cleanup_marker_missing', 'invalid_expected_participants', 'invalid_lifecycle_record', 'invalid_log_control', 'invalid_log_encoding', 'invalid_timeline', 'invalid_workload_timeline', 'line_count_limit', 'line_length_limit', 'log_changed_during_read', 'log_clock_regression', 'missing_lifecycle_markers', 'missing_timeline_anchor', 'participant_cleanup_missing', 'participant_recv_transport_missing', 'record_limit', 'timeline_clock_shift', 'timeline_order', 'transport_closed_missing', 'transport_created_missing', 'transport_dtls_ready_missing', 'transport_ice_ready_missing', 'transport_identity_conflict', 'unknown_record_phase', 'unsafe_file', 'unterminated_line', 'workload_timeline_order']);
const totalKeys = ['participantsObserved', 'transportsCreated', 'transportsClosed', 'transportsOpen', 'openTransportsIceDisconnected', 'openTransportsDtlsFailedOrClosed', 'openTransportsWithDisconnectState', 'iceDisconnectedEvents', 'cleanupGenerationsStarted', 'cleanupGenerationsFinished', 'cleanupGenerationsInProgress', 'participantsWithCleanupFinished', 'explicitLeavesFinished', 'graceCleanupsStarted'];
const interpretation = 'Coverage means expected instrumented lifecycle markers, not comprehensive native capture or successful server exit. Measurement boundaries are nominal wall-clock anchors, not individual media activity. Rows retain log observation order; asynchronous callbacks and formatter ordering can differ from underlying state order. Clamps contain only previously observed aggregate state, never a transport identity or causal attribution. Small timestamp reordering (up to 250ms) is tolerated; larger regressions make coverage incomplete. Explicit leave completion alone does not establish media cleanup.';
const exact = (value, keys) => value !== null && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
const integer = (value, max = LIMIT) => Number.isSafeInteger(value) && value >= 0 && value <= max;
const positive = value => integer(value) && value > 0;
const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= Number.MAX_SAFE_INTEGER;
const at = value => typeof value === 'string' && /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?(?:Z|\+00:00)$/.test(value);
const iso = value => typeof value === 'string' && /^(?:[0-9]{4}|[+-][0-9]{6})-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$/.test(value);
const totals = value => exact(value, totalKeys) && totalKeys.every(key => integer(value[key]));

function validTimeline(value) {
  return exact(value, ['departure', 'events', 'nominalMeasurement']) &&
    [null, 'abrupt', 'explicit-leave'].includes(value.departure) && Array.isArray(value.events) && value.events.length <= anchors.length &&
    value.events.every(row => exact(row, ['event', 'at', 'elapsedMs']) && anchors.includes(row.event) && at(row.at) && finite(row.elapsedMs)) &&
    new Set(value.events.map(row => row.event)).size === value.events.length &&
    (value.nominalMeasurement === null || exact(value.nominalMeasurement, ['startedAt', 'finishedAt']) && iso(value.nominalMeasurement.startedAt) && iso(value.nominalMeasurement.finishedAt));
}

function validMarker(row, index) {
  if (!row || !markerNames.includes(row.event)) return false;
  const keys = ['ordinal', 'at', 'phase', 'event', 'participantOrdinal'];
  if (row.event.startsWith('transport_')) keys.push('transportOrdinal', 'transportType');
  if (['transport_ice', 'transport_dtls'].includes(row.event)) keys.push('state');
  if (row.event.startsWith('media_cleanup_')) keys.push('cleanupOrdinal');
  return exact(row, keys) && row.ordinal === index + 1 && at(row.at) && phases.includes(row.phase) && positive(row.participantOrdinal) &&
    (!keys.includes('transportOrdinal') || positive(row.transportOrdinal) && ['send', 'recv'].includes(row.transportType)) &&
    (!keys.includes('cleanupOrdinal') || positive(row.cleanupOrdinal)) &&
    (row.event !== 'transport_ice' || ['New', 'Connected', 'Completed', 'Disconnected'].includes(row.state)) &&
    (row.event !== 'transport_dtls' || ['New', 'Connecting', 'Connected', 'Failed', 'Closed'].includes(row.state));
}

function validReport(report, expected) {
  if (!exact(report, ['schemaVersion', 'coverage', 'timeline', 'linesRead', 'relevantRecords', 'lifecycleEvents', 'clamps', 'phases', 'totals', 'interpretation']) || report.schemaVersion !== 1 ||
      !exact(report.coverage, ['available', 'complete', 'scope', 'issues']) || typeof report.coverage.available !== 'boolean' || typeof report.coverage.complete !== 'boolean' ||
      !Array.isArray(report.coverage.issues) || !report.coverage.issues.every(issue => issues.has(issue)) || new Set(report.coverage.issues).size !== report.coverage.issues.length ||
      report.coverage.complete !== (report.coverage.available && report.coverage.issues.length === 0) ||
      report.coverage.scope !== (!report.coverage.available ? 'unavailable' : report.coverage.complete ? 'expected_instrumented_markers' : 'observed_subset') ||
      !validTimeline(report.timeline) || !integer(report.linesRead, 100001) || !integer(report.relevantRecords) || report.relevantRecords > report.linesRead ||
      !Array.isArray(report.lifecycleEvents) || !Array.isArray(report.clamps) || report.lifecycleEvents.length + report.clamps.length > report.relevantRecords ||
      !report.lifecycleEvents.every(validMarker) || !report.clamps.every((row, index) => exact(row, ['ordinal', 'at', 'phase', 'signature', 'previouslyObserved']) &&
        row.ordinal === index + 1 && at(row.at) && phases.includes(row.phase) && row.signature === 'start_bitrate_below_minimum_30000_100000' && totals(row.previouslyObserved)) ||
      !exact(report.phases, phases) || !phases.every(phase => exact(report.phases[phase], ['lifecycleEvents', 'clamps']) && integer(report.phases[phase].lifecycleEvents) && integer(report.phases[phase].clamps)) ||
      phases.reduce((sum, phase) => sum + report.phases[phase].lifecycleEvents, 0) !== report.lifecycleEvents.length ||
      phases.reduce((sum, phase) => sum + report.phases[phase].clamps, 0) !== report.clamps.length ||
      !totals(report.totals) || !report.lifecycleEvents.every(row => row.participantOrdinal <= report.totals.participantsObserved) ||
      report.interpretation !== interpretation) return false;
  return !report.coverage.complete || report.lifecycleEvents.length > 0 && report.totals.participantsObserved > 0 &&
    Number.isInteger(expected) && expected >= 1 && expected <= LIMIT && report.totals.participantsWithCleanupFinished >= expected &&
    report.totals.participantsWithCleanupFinished === report.totals.participantsObserved && report.totals.transportsCreated > 0 &&
    report.totals.transportsCreated === report.totals.transportsClosed && report.totals.transportsOpen === 0 &&
    report.totals.cleanupGenerationsStarted > 0 && report.totals.cleanupGenerationsStarted === report.totals.cleanupGenerationsFinished && report.totals.cleanupGenerationsInProgress === 0 &&
    (report.timeline.departure !== 'abrupt' || report.totals.graceCleanupsStarted === report.totals.participantsObserved) &&
    (report.timeline.departure !== 'explicit-leave' || report.totals.explicitLeavesFinished === report.totals.participantsObserved);
}

/** Feed one bounded timeline and close the owned stdin pipe. No raw output,
 * operating-system error or arbitrary report field crosses this boundary.
 */
export async function readLifecycleReportWithGenerator(generator, path, timeline, { expectedParticipants } = {}) {
  let input;
  try { input = JSON.stringify(timeline) ?? 'null'; }
  catch { throw new Error('lifecycle_report_command_failed'); }
  if (Buffer.byteLength(input) > 64 * 1024) throw new Error('lifecycle_report_command_failed');
  const env = Object.fromEntries(['PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT'].filter(key => process.env[key]).map(key => [key, process.env[key]]));
  const expected = typeof expectedParticipants === 'number' ? String(expectedParticipants) : 'NaN';
  const result = await new Promise((resolve, reject) => {
    let inputFailed = false;
    const child = execFile(generator, ['report', 'lifecycle', path, '--expected-participants', expected],
      { env, maxBuffer: 16 * 1024 * 1024, timeout: 30000, killSignal: 'SIGKILL' }, (error, stdout) => {
        if (inputFailed || error && (error.code !== 1 || error.killed || error.signal)) { reject(new Error('lifecycle_report_command_failed')); return; }
        resolve({ stdout, status: error ? 1 : 0 });
      });
    if (!child.stdin) { inputFailed = true; return; }
    child.stdin.on('error', () => { inputFailed = true; });
    try { child.stdin.end(input); }
    catch { inputFailed = true; child.kill('SIGKILL'); }
  }).catch(() => { throw new Error('lifecycle_report_command_failed'); });
  try {
    const report = JSON.parse(result.stdout);
    if (!validReport(report, expectedParticipants) || report.coverage.complete !== (result.status === 0)) throw new Error();
    return report;
  } catch { throw new Error('lifecycle_report_command_failed'); }
}
