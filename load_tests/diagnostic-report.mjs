// Offline summaries of the owned server's bounded, privacy-filtered JSONL recorder.
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';

export const DIAGNOSTIC_LIMITS = Object.freeze({ records: 100000, lineBytes: 1024 });
const operations = new Set(['join_room', 'leave_room', 'reconnect', 'router_capabilities',
  'create_send_transport', 'create_recv_transport', 'connect_transport', 'produce', 'consume',
  'resume_consumer', 'pause_consumer', 'pause_producer', 'resume_producer', 'close_producer',
  'restart_ice', 'set_preferred_layers', 'chat', 'private_message', 'room_action', 'socket_write',
  'shutdown_notification']);
const stages = new Set(['room_creation_lock_wait', 'room_lock_wait', 'room_policy_lookup',
  'room_password_permit_wait', 'room_password_dispatch', 'room_password_work', 'room_media_setup',
  'room_membership_commit', 'session_lock_wait', 'media_create_transport', 'media_connect_transport',
  'media_produce', 'media_consume', 'media_resume', 'outbound_queue_wait', 'socket_write', 'dispatch']);
const outcomes = ['ok', 'error', 'completed', 'cancelled', 'timeout', 'rejected'];
const recordKeys = ['schemaVersion', 'kind', 'operationId', 'connectionId', 'operation', 'stage', 'outcome', 'startedUs', 'elapsedUs'];
const summaryKeys = ['schemaVersion', 'kind', 'accepted', 'written', 'dropped', 'expired', 'unfinished', 'writeFailed'];
const integer = value => Number.isSafeInteger(value) && value >= 0;
const exactKeys = (value, keys) => value && typeof value === 'object' && !Array.isArray(value) &&
  Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
const validRecord = value => exactKeys(value, recordKeys) && value.schemaVersion === 1 &&
  (value.kind === 'operation' ? value.stage === null : value.kind === 'stage' && stages.has(value.stage)) &&
  integer(value.operationId) && value.operationId > 0 &&
  (value.connectionId === null || (integer(value.connectionId) && value.connectionId > 0)) &&
  operations.has(value.operation) && outcomes.includes(value.outcome) &&
  integer(value.startedUs) && integer(value.elapsedUs);
const validSummary = value => exactKeys(value, summaryKeys) && value.schemaVersion === 1 && value.kind === 'summary' &&
  ['accepted', 'written', 'dropped', 'expired', 'unfinished'].every(key => integer(value[key])) &&
  value.accepted <= DIAGNOSTIC_LIMITS.records && value.written <= value.accepted && typeof value.writeFailed === 'boolean';

/** Read only after the owned server stops. Error labels never contain file or record contents.
 * Groups use raw elapsed durations and nearest-rank percentiles, never summed nested stages.
 */
export async function readDiagnosticReport(path, { maxRecords = DIAGNOSTIC_LIMITS.records } = {}) {
  if (!Number.isInteger(maxRecords) || maxRecords < 1 || maxRecords > DIAGNOSTIC_LIMITS.records) {
    throw new Error('Invalid diagnostic record bound');
  }
  const report = { schemaVersion: 1, coverage: { available: false, complete: false, scope: 'unavailable', issues: [] },
    records: 0, summary: null, outcomeCounts: Object.fromEntries(outcomes.map(value => [value, 0])),
    operations: [], stages: [],
    interpretation: 'Instrumented records over the recorder lifetime, including setup and cleanup; not the shared media measurement interval. Subsets are not random samples. Stage durations may overlap and must not be added to operation durations.',
    percentiles: 'nearest-rank from individual elapsedUs values, converted to milliseconds' };
  const issue = code => { if (!report.coverage.issues.includes(code)) report.coverage.issues.push(code); };
  const groups = new Map();
  let file;
  let terminal = false;
  const consume = line => {
    if (terminal) { issue('records_after_summary'); return false; }
    if (line.length > DIAGNOSTIC_LIMITS.lineBytes) { issue('line_limit'); return false; }
    let record;
    try { record = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line)); }
    catch { issue('invalid_json'); return false; }
    if (record?.kind === 'summary') {
      if (!validSummary(record)) { issue('invalid_summary'); return false; }
      report.summary = record;
      terminal = true;
      return true;
    }
    if (!validRecord(record)) { issue('invalid_record'); return false; }
    if (report.records === maxRecords) { issue('record_limit'); return false; }
    report.records++;
    report.outcomeCounts[record.outcome]++;
    const key = `${record.kind}/${record.operation}/${record.stage ?? ''}/${record.outcome}`;
    if (!groups.has(key)) groups.set(key, { kind: record.kind, operation: record.operation,
      stage: record.stage, outcome: record.outcome, values: [] });
    groups.get(key).values.push(record.elapsedUs);
    return true;
  };
  try {
    // Do not follow a substituted symlink or wait on a special file. The recorder
    // creates a private regular file; enforce that same boundary when reading it.
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    const info = await file.stat();
    if (!info.isFile() || (info.mode & 0o077)) { issue('unsafe_file'); }
    else if (info.size > (maxRecords + 1) * (DIAGNOSTIC_LIMITS.lineBytes + 1)) { issue('file_limit'); }
    else {
      report.coverage.available = true;
      const buffer = Buffer.alloc(64 * 1024);
      let pending = Buffer.alloc(0), bytesRead = 0, halted = false;
      while (!halted) {
        const read = await file.read(buffer, 0, buffer.length, null);
        if (!read.bytesRead) break;
        bytesRead += read.bytesRead;
        if (bytesRead > (maxRecords + 1) * (DIAGNOSTIC_LIMITS.lineBytes + 1)) { issue('file_limit'); break; }
        pending = Buffer.concat([pending, buffer.subarray(0, read.bytesRead)]);
        let newline;
        while ((newline = pending.indexOf(10)) !== -1) {
          if (!consume(pending.subarray(0, newline))) { halted = true; break; }
          pending = pending.subarray(newline + 1);
        }
        if (!halted && pending.length > DIAGNOSTIC_LIMITS.lineBytes) { issue('line_limit'); halted = true; }
      }
      if (!halted && pending.length) issue('unterminated_line');
    }
  } catch (error) {
    issue(error.code === 'ENOENT' ? 'file_unavailable' : error.code === 'ELOOP' ? 'unsafe_file' : 'file_unreadable');
  } finally {
    if (file) await file.close().catch(() => issue('file_close_failed'));
  }
  if (!report.summary) issue('missing_summary');
  else {
    if (report.summary.written !== report.records || report.summary.accepted !== report.summary.written) issue('count_mismatch');
    if (report.summary.writeFailed) issue('write_failed');
    if (report.summary.dropped) issue('records_dropped');
    if (report.summary.expired) issue('records_expired');
    if (report.summary.unfinished) issue('unfinished_timers');
  }
  if (!report.records && report.coverage.available) issue('no_records');
  report.coverage.complete = report.coverage.available && !report.coverage.issues.length;
  report.coverage.scope = !report.coverage.available ? 'unavailable' : report.coverage.complete ? 'all_emitted_records' : 'recorded_subset';
  for (const [, group] of [...groups].sort(([left], [right]) => left.localeCompare(right))) {
    const values = group.values.sort((left, right) => left - right);
    const percentile = fraction => values[Math.ceil(values.length * fraction) - 1] / 1000;
    const row = { operation: group.operation, ...(group.kind === 'stage' ? { stage: group.stage } : {}),
      outcome: group.outcome, count: values.length, p50Ms: percentile(0.5), p95Ms: percentile(0.95), p99Ms: percentile(0.99) };
    report[group.kind === 'stage' ? 'stages' : 'operations'].push(row);
  }
  return report;
}
