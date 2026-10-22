// Invoke an explicitly selected, already-built generator's offline reader.
// This adapter never builds a binary or starts an application workload.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const executeFile = promisify(execFile);
const MAX_RECORDS = 100000;
const operations = new Set(['join_room', 'leave_room', 'reconnect', 'router_capabilities',
  'create_send_transport', 'create_recv_transport', 'connect_transport', 'produce', 'consume',
  'resume_consumer', 'pause_consumer', 'close_consumer', 'pause_producer', 'resume_producer', 'close_producer',
  'restart_ice', 'set_preferred_layers', 'chat', 'private_message', 'room_action', 'socket_write',
  'shutdown_notification']);
const stages = new Set(['room_creation_lock_wait', 'room_lock_wait', 'room_policy_lookup',
  'room_password_permit_wait', 'room_password_dispatch', 'room_password_work', 'room_media_setup',
  'room_membership_commit', 'session_lock_wait', 'media_create_transport', 'media_connect_transport',
  'media_produce', 'media_consume', 'media_resume', 'outbound_queue_wait', 'socket_write', 'dispatch']);
const outcomes = ['ok', 'error', 'completed', 'cancelled', 'timeout', 'rejected'];
const issues = new Set(['records_after_summary', 'line_limit', 'invalid_json', 'invalid_summary',
  'invalid_record', 'record_limit', 'file_unavailable', 'unsafe_file', 'file_unreadable', 'file_limit',
  'unterminated_line', 'file_close_failed', 'missing_summary', 'count_mismatch', 'write_failed',
  'records_dropped', 'records_expired', 'unfinished_timers', 'no_records']);
const interpretation = 'Instrumented records over the recorder lifetime, including setup and cleanup; not the shared media measurement interval. Subsets are not random samples. Stage durations may overlap and must not be added to operation durations.';
const percentiles = 'nearest-rank from individual elapsedUs values, converted to milliseconds';
const exact = (value, keys) => value !== null && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
const integer = value => Number.isSafeInteger(value) && value >= 0;
const count = value => integer(value) && value <= MAX_RECORDS;

function validCoverage(value) {
  return exact(value, ['available', 'complete', 'scope', 'issues']) &&
    typeof value.available === 'boolean' && typeof value.complete === 'boolean' &&
    value.scope === (!value.available ? 'unavailable' : value.complete ? 'all_emitted_records' : 'recorded_subset') &&
    Array.isArray(value.issues) && value.issues.every(issue => issues.has(issue)) &&
    new Set(value.issues).size === value.issues.length &&
    value.complete === (value.available && value.issues.length === 0);
}

function validSummary(value) {
  return value === null || exact(value, ['schemaVersion', 'kind', 'accepted', 'written', 'dropped', 'expired', 'unfinished', 'writeFailed']) &&
    value.schemaVersion === 1 && value.kind === 'summary' && count(value.accepted) && count(value.written) &&
    value.written <= value.accepted && integer(value.dropped) && integer(value.expired) &&
    integer(value.unfinished) && typeof value.writeFailed === 'boolean';
}

function validRows(rows, stageRows) {
  if (!Array.isArray(rows) || rows.length > MAX_RECORDS) return false;
  const keys = ['operation', 'outcome', 'count', 'p50Ms', 'p95Ms', 'p99Ms', ...(stageRows ? ['stage'] : [])];
  const seen = new Set();
  for (const row of rows) {
    if (!exact(row, keys) || !operations.has(row.operation) || !outcomes.includes(row.outcome) ||
        (stageRows && !stages.has(row.stage)) || !count(row.count) || row.count === 0 ||
        !['p50Ms', 'p95Ms', 'p99Ms'].every(key => typeof row[key] === 'number' &&
          Number.isFinite(row[key]) && row[key] >= 0 && row[key] <= Number.MAX_SAFE_INTEGER / 1000) ||
        row.p50Ms > row.p95Ms || row.p95Ms > row.p99Ms) return false;
    const key = `${row.operation}/${row.stage ?? ''}/${row.outcome}`;
    if (seen.has(key)) return false;
    seen.add(key);
  }
  return true;
}

/** The explicit executable is trusted to run, but its JSON still crosses the
 * fixed-report boundary before it can enter a retained benchmark artifact.
 */
function validReport(report) {
  return exact(report, ['schemaVersion', 'coverage', 'records', 'summary', 'outcomeCounts', 'operations', 'stages', 'interpretation', 'percentiles']) &&
    report.schemaVersion === 1 && validCoverage(report.coverage) && count(report.records) &&
    validSummary(report.summary) && exact(report.outcomeCounts, outcomes) &&
    outcomes.every(outcome => count(report.outcomeCounts[outcome])) &&
    outcomes.reduce((sum, outcome) => sum + report.outcomeCounts[outcome], 0) === report.records &&
    validRows(report.operations, false) && validRows(report.stages, true) &&
    [...report.operations, ...report.stages].reduce((sum, row) => sum + row.count, 0) === report.records &&
    (!report.coverage.complete || report.records > 0 && report.summary !== null &&
      report.summary.accepted === report.records && report.summary.written === report.records &&
      !report.summary.dropped && !report.summary.expired && !report.summary.unfinished && !report.summary.writeFailed) &&
    report.interpretation === interpretation && report.percentiles === percentiles;
}

const privateEnvironment = () => Object.fromEntries(
  ['PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT'].filter(key => process.env[key]).map(key => [key, process.env[key]]),
);

/** Exit 1 retains an incomplete coverage report; all other process/format
 * failures use a fixed label rather than exposing paths or child output.
 */
export async function readDiagnosticReportWithGenerator(generator, path, { execute = executeFile } = {}) {
  let stdout, status = 0;
  try {
    ({ stdout } = await execute(generator, ['report', 'diagnostics', path], {
      env: privateEnvironment(), maxBuffer: 2 * 1024 * 1024, timeout: 30000, killSignal: 'SIGKILL',
    }));
  } catch (error) {
    if (error.code !== 1 || error.killed || error.signal) throw new Error('diagnostic_report_command_failed');
    stdout = error.stdout;
    status = 1;
  }
  try {
    const report = JSON.parse(stdout);
    if (!validReport(report) ||
        report.coverage.complete !== (status === 0)) throw new Error();
    return report;
  } catch {
    throw new Error('diagnostic_report_command_failed');
  }
}
