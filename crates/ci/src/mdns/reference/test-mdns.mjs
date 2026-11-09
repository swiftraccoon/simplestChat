import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { isIPv4 } from 'node:net';
import { networkInterfaces, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execute = promisify(execFile);
const stages = new Set(['arguments', 'create_connection', 'set_dispatch_queue', 'register_record', 'callback', 'callback_timeout']);
const predicate = '((process CONTAINS[c] "WebKit" AND (eventMessage CONTAINS[c] "NetworkMDNSRegister" OR eventMessage CONTAINS[c] "DNSServiceRegisterRecord")) OR (process == "sandboxd" AND eventMessage CONTAINS[c] "WebKit" AND (eventMessage CONTAINS[c] "mDNS" OR eventMessage CONTAINS[c] "network")))';

async function command(binary, args, timeout = 5000) {
  try {
    const { stdout } = await execute(binary, args, {
      timeout, maxBuffer: 512 * 1024, encoding: 'utf8',
      env: { PATH: process.env.PATH, LC_ALL: 'C' },
    });
    return { exitCode: 0, timedOut: false, stdout };
  } catch (error) {
    // Command diagnostics may contain host names, paths or addresses. Retain
    // only the bounded stdout in memory and fixed/numeric outcome fields.
    return {
      exitCode: Number.isSafeInteger(error.code) ? error.code : null,
      timedOut: error.killed === true,
      stdout: typeof error.stdout === 'string' ? error.stdout : '',
    };
  }
}

export function summarizeLogs(output) {
  const counts = { registrationCallbacks: 0, callbackErrors: {}, createConnectionErrors: {}, registerRecordErrors: {}, sandboxDenials: 0 };
  for (const line of output.split('\n')) {
    let message;
    try { message = JSON.parse(line).eventMessage; } catch { continue; }
    if (typeof message !== 'string') continue;
    const callback = message.match(/registerMDNSNameCallback with error (-?\d+)/);
    if (callback) {
      counts.registrationCallbacks++;
      const code = Number(callback[1]);
      if (Number.isSafeInteger(code)) counts.callbackErrors[code] = (counts.callbackErrors[code] ?? 0) + 1;
    }
    for (const [pattern, field] of [
      [/DNSServiceCreateConnection error (-?\d+)/, 'createConnectionErrors'],
      [/DNSServiceRegisterRecord error (-?\d+)/, 'registerRecordErrors'],
    ]) {
      const match = message.match(pattern);
      const code = match ? Number(match[1]) : NaN;
      if (Number.isSafeInteger(code)) counts[field][code] = (counts[field][code] ?? 0) + 1;
    }
    if (/WebKit/i.test(message) && /mDNS|network/i.test(message) && /deny|denied/i.test(message)) counts.sandboxDenials++;
  }
  return counts;
}

export async function systemSnapshot(run = command) {
  // Both reads precede the owned registration. Raw unified logs never leave
  // this function; only fixed categories and numeric error counts are saved.
  // macOS 26 uses the .reloaded service label. Read the signed system plist so
  // the same diagnostic also works on macOS versions using the original label.
  const plist = await run('/usr/bin/plutil', ['-extract', 'Label', 'raw', '-o', '-', '/System/Library/LaunchDaemons/com.apple.mDNSResponder.plist']);
  const label = plist.stdout.trim();
  const knownLabel = ['com.apple.mDNSResponder', 'com.apple.mDNSResponder.reloaded'].includes(label);
  const launch = knownLabel && plist.exitCode === 0
    ? await run('/bin/launchctl', ['print', `system/${label}`])
    : { exitCode: plist.exitCode, timedOut: plist.timedOut, stdout: '' };
  const logs = await run('/usr/bin/log', ['show', '--last', '2m', '--style', 'ndjson', '--info', '--debug', '--predicate', predicate]);
  const state = launch.stdout.match(/^\s*state = (running|waiting|exited|not running)\s*$/m)?.[1] ?? 'unavailable';
  return {
    responder: { readable: knownLabel && launch.exitCode === 0, exitCode: launch.exitCode, timedOut: launch.timedOut, state },
    logs: { readable: logs.exitCode === 0, exitCode: logs.exitCode, timedOut: logs.timedOut, ...summarizeLogs(logs.stdout) },
  };
}

export function interfaceFacts(address, interfaces) {
  if (!isIPv4(address ?? '')) throw new Error('assigned_ipv4_required');
  const entries = Object.values(interfaces).flatMap(value => value ?? []).filter(value => value.family === 'IPv4' || value.family === 4);
  const selected = entries.find(value => value.address === address && value.internal === false);
  if (!selected) throw new Error('assigned_non_loopback_ipv4_required');
  return { ipv4Addresses: entries.length, nonLoopbackIpv4Addresses: entries.filter(value => !value.internal).length, selectedAddressAssigned: true };
}

export function parseNativeProbe(result) {
  let value;
  try { value = JSON.parse(result.stdout); } catch { return { passed: false, stage: 'invalid_report', exitCode: result.exitCode, timedOut: result.timedOut }; }
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || !stages.has(value.stage) || !Number.isSafeInteger(value.error)
    || !['callbackObserved', 'timedOut', 'deallocated'].every(key => typeof value[key] === 'boolean')) {
    return { passed: false, stage: 'invalid_report', exitCode: result.exitCode, timedOut: result.timedOut };
  }
  return {
    passed: result.exitCode === 0 && !result.timedOut && value.stage === 'callback' && value.error === 0 && value.callbackObserved && value.deallocated && !value.timedOut,
    stage: value.stage, error: value.error, callbackObserved: value.callbackObserved,
    timedOut: value.timedOut || result.timedOut, deallocated: value.deallocated, exitCode: result.exitCode,
  };
}

export async function nativeProbe(address, run = command) {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-mdns-'));
  try {
    const binary = join(directory, 'probe');
    const compiled = await run('/usr/bin/cc', ['-std=c11', '-fblocks', '-Wall', '-Wextra', '-Werror', fileURLToPath(new URL('./test-mdns.c', import.meta.url)), '-o', binary], 15_000);
    if (compiled.exitCode !== 0) return { passed: false, stage: 'compile', exitCode: compiled.exitCode, timedOut: compiled.timedOut };
    return parseNativeProbe(await run(binary, [address, randomUUID()], 8000));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

export async function runCli({ args = process.argv.slice(2), env = process.env, platform = process.platform, interfaces = networkInterfaces, snapshot = systemSnapshot, probe = nativeProbe, stdout = process.stdout, stderr = process.stderr } = {}) {
  if (env.MDNS_DIAGNOSTIC_E2E !== '1' || platform !== 'darwin'
    || args.length !== 1 || !['preflight', 'snapshot'].includes(args[0]) || !env.E2E_ARTIFACTS) {
    stderr.write('mDNS diagnostic requires macOS, explicit opt-in, artifacts directory and preflight or snapshot mode.\n');
    return 2;
  }
  const report = { schemaVersion: 1, phase: args[0], startedAt: new Date().toISOString(), registrationDeadlineMs: 5000 };
  try {
    report.interfaces = interfaceFacts(env.TEST_ANNOUNCE_IP, interfaces());
    report.system = await snapshot();
    if (args[0] === 'preflight') report.native = await probe(env.TEST_ANNOUNCE_IP);
    report.passed = report.system.responder.readable && report.system.logs.readable && (report.native?.passed ?? true);
  } catch {
    report.passed = false;
    report.stage = 'diagnostic_failed';
  }
  report.finishedAt = new Date().toISOString();
  try {
    await mkdir(env.E2E_ARTIFACTS, { recursive: true, mode: 0o700 });
    await writeFile(join(env.E2E_ARTIFACTS, `mdns-${args[0]}-results.json`), `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600, flag: 'wx' });
  } catch {
    stderr.write('Cannot save the mDNS diagnostic without replacing existing evidence.\n');
    return 1;
  }
  stdout.write(`${JSON.stringify(report)}\n`);
  return report.passed ? 0 : 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await runCli();
}
