import { spawn as spawnCommand } from 'node:child_process';
import { chmod, mkdir, mkdtemp, realpath, writeFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runShutdownSmoke } from './shutdown-smoke.mjs';
import { readDiagnosticReport } from '../load_tests/diagnostic-report.mjs';

const usage = 'Usage: node build/diagnostics-smoke.mjs --binary /absolute/path/to/simplestChat';
const validBinary = binary => typeof binary === 'string' && isAbsolute(binary);
const saveJson = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx', mode: 0o600 });

async function privateDirectory() {
  const root = fileURLToPath(new URL('../results/', import.meta.url));
  await mkdir(root, { recursive: true, mode: 0o700 });
  const directory = await mkdtemp(join(root, 'diagnostics-smoke.'));
  await chmod(directory, 0o700);
  return directory;
}

/** Reuse the owned guest-only shutdown workflow, adding only a private recorder path.
 * Original smoke failures remain the cause even if diagnostic preservation also fails.
 */
export async function runDiagnosticsSmoke({ binary, signal } = {}, dependencies = {}) {
  if (!validBinary(binary)) throw new Error(usage);
  const { run = runShutdownSmoke, spawn = spawnCommand, createDirectory = privateDirectory,
    readReport = readDiagnosticReport, save = saveJson, saveLog = writeFile } = dependencies;
  const directory = await createDirectory();
  const path = join(directory, 'server-diagnostics.jsonl');
  let serverStopped = true, serverExit = null, shutdown = null, smokeFailure, report = null;
  const artifactIssues = [];
  try {
    shutdown = await run({ binary, signal }, { spawn: (file, args, options) => {
      const child = spawn(file, args, { ...options, env: { ...options.env, DIAGNOSTICS_PATH: path } });
      serverStopped = false;
      child.once('exit', (code, signal) => { serverStopped = true; serverExit = { code, signal }; });
      child.once('error', () => {
        if (!child.pid) { serverStopped = true; serverExit = { code: null, signal: null, error: 'spawn_failed' }; }
      });
      return child;
    } });
  } catch (error) { smokeFailure = error; }

  // The shared smoke owns all cleanup. Do not race a writer if its cleanup failed.
  if (!serverStopped) artifactIssues.push('server_not_stopped');
  else {
    try { report = await readReport(path); }
    catch { artifactIssues.push('diagnostic_read_failed'); }
  }
  if (report) {
    try { await save(join(directory, 'server-diagnostics-report.json'), report); }
    catch { artifactIssues.push('diagnostic_report_write_failed'); }
  }
  if (typeof smokeFailure?.serverOutput === 'string') {
    try {
      await saveLog(join(directory, 'server.log'), Buffer.from(smokeFailure.serverOutput).subarray(-32768), { flag: 'wx', mode: 0o600 });
    } catch { artifactIssues.push('server_log_write_failed'); }
  }
  const workloadPassed = !smokeFailure && shutdown?.ready === true && shutdown.joined === true &&
    shutdown.serverRestarting === true && shutdown.closeCode === 1001 && shutdown.exitCode === 0;
  const serverShutdownPassed = serverExit?.code === 0 && serverExit.signal === null;
  const diagnosticCoverageComplete = report?.coverage.complete === true;
  const evidence = {
    joinRoom: report?.operations.some(row => row.operation === 'join_room' && row.outcome === 'ok' && row.count > 0) ?? false,
    roomLockWait: report?.stages.some(row => row.operation === 'join_room' && row.stage === 'room_lock_wait' &&
      ['ok', 'completed'].includes(row.outcome) && row.count > 0) ?? false,
  };
  const result = { schemaVersion: 1, passed: workloadPassed && serverShutdownPassed && diagnosticCoverageComplete &&
      evidence.joinRoom && evidence.roomLockWait && !artifactIssues.length,
    workloadPassed, serverShutdownPassed, diagnosticCoverageComplete, evidence,
    records: report?.records ?? 0, serverExit, shutdownMs: shutdown?.shutdownMs ?? null,
    diagnosticCoverage: report?.coverage ?? null, artifactIssues, artifactDirectory: directory };
  try { await save(join(directory, 'diagnostics-smoke.json'), result); }
  catch { result.passed = false; artifactIssues.push('smoke_result_write_failed'); }
  if (!result.passed) {
    const error = new Error('Diagnostics smoke failed', { cause: smokeFailure });
    error.result = result;
    throw error;
  }
  return result;
}

export async function runCli({ args = [], stdout = process.stdout, stderr = process.stderr, run = runDiagnosticsSmoke } = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) { stdout.write(`${usage}\n`); return 0; }
  if (args.length !== 2 || args[0] !== '--binary' || !validBinary(args[1])) { stderr.write(`${usage}\n`); return 2; }
  const cancellation = new AbortController();
  const cancel = () => cancellation.abort();
  process.once('SIGINT', cancel);
  process.once('SIGTERM', cancel);
  try {
    const result = await run({ binary: args[1], signal: cancellation.signal });
    if (result.passed !== true) {
      const error = new Error('Diagnostics smoke failed'); error.result = result; throw error;
    }
    stdout.write(`PASS diagnostics smoke: ${result.records} records; join and lock timing verified. Private artifacts: ${result.artifactDirectory}\n`);
    return 0;
  } catch (error) {
    const result = error.result;
    stderr.write(result
      ? `FAIL diagnostics smoke: workload=${result.workloadPassed}, coverage=${result.diagnosticCoverageComplete}, shutdown=${result.serverShutdownPassed}, join=${result.evidence?.joinRoom}, lock=${result.evidence?.roomLockWait}; ${result.records} records. Private artifacts: ${result.artifactDirectory}\n`
      : 'FAIL diagnostics smoke: setup failed before artifacts were available.\n');
    return 1;
  } finally {
    process.removeListener('SIGINT', cancel);
    process.removeListener('SIGTERM', cancel);
  }
}

const entryFile = process.argv[1] ? await realpath(process.argv[1]).catch(() => undefined) : undefined;
if (entryFile && import.meta.url === pathToFileURL(entryFile).href) {
  process.exitCode = await runCli({ args: process.argv.slice(2) });
}
