#!/usr/bin/env node
// Bounded A/B measurements of two owned local server processes, never a remote URL.
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash, randomBytes } from 'node:crypto';
import { readFile, writeFile, mkdir, stat, lstat, readlink, open } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import os from 'node:os';
import net from 'node:net';
import dgram from 'node:dgram';
import { setTimeout as delay } from 'node:timers/promises';
import { readDiagnosticReport } from './diagnostic-report.mjs';
import { createMediaSampler, fetchMediaSnapshot, readGeneratorResults, correlateMediaDiagnostics } from './media-diagnostic-report.mjs';
import { readLifecycleReport } from './lifecycle-diagnostic-report.mjs';

const exec = promisify(execFile);
const hash = value => createHash('sha256').update(value).digest('hex');
const median = values => { const sorted = [...values].sort((a, b) => a - b); const m = Math.floor(sorted.length / 2); return sorted.length % 2 ? sorted[m] : (sorted[m - 1] + sorted[m]) / 2; };
const children = new Set();
const cleanEnv = () => Object.fromEntries(['PATH', 'TMPDIR', 'LANG', 'SYSTEMROOT'].filter(k => process.env[k]).map(k => [k, process.env[k]]));
const json = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });

export function cpuSeconds(value) {
  const [days, clock] = value.includes('-') ? value.split('-') : ['0', value];
  const parts = clock.split(':').map(Number);
  if (parts.length < 2 || parts.length > 3 || parts.some(v => !Number.isFinite(v))) throw new Error(`Invalid ps CPU time: ${value}`);
  return Number(days) * 86400 + parts.reverse().reduce((sum, part, i) => sum + part * 60 ** i, 0);
}

export function resourceSummary(samples, role, start, end) {
  const window = samples.filter(s => s.elapsedMs >= start && s.elapsedMs <= end && s[role]);
  if (window.length < 2) throw new Error(`Insufficient ${role} resource samples in measurement window`);
  const first = window[0], last = window.at(-1);
  const cpu = last[role].cpuSeconds - first[role].cpuSeconds;
  const seconds = (last.elapsedMs - first.elapsedMs) / 1000;
  return { samples: window.length, sampledDurationSeconds: seconds, cpuSeconds: cpu,
    cpuPercentOfOneCore: cpu / seconds * 100,
    peakRssMiB: Math.max(...window.map(s => s[role].rssKiB)) / 1024,
    medianRssMiB: median(window.map(s => s[role].rssKiB)) / 1024 };
}

export function comparison(rows) {
  if (rows.some(row => row.purpose === 'diagnostic')) throw new Error('Diagnostic runs are not performance comparisons');
  if (!performanceRunStatus(rows).passed) throw new Error('Performance comparison requires passing workloads and clean server shutdowns');
  const fields = ['joinP99Ms', 'sendReadyP99Ms', 'receiveReadyP99Ms', 'receivedPacketsPerSecond', 'serverCpuPercent', 'serverPeakRssMiB', 'generatorCpuPercent', 'generatorPeakRssMiB'];
  const result = {};
  for (const scenario of new Set(rows.map(r => r.scenario))) {
    result[scenario] = {};
    for (const field of fields) {
      const baseline = rows.filter(r => r.scenario === scenario && r.variant === 'baseline').map(r => r[field]);
      const candidate = rows.filter(r => r.scenario === scenario && r.variant === 'candidate').map(r => r[field]);
      if (!baseline.length || baseline.length !== candidate.length || [...baseline, ...candidate].some(v => !Number.isFinite(v))) throw new Error(`Incomplete comparison: ${scenario}/${field}`);
      const before = median(baseline), after = median(candidate);
      result[scenario][field] = { baselineMedian: before, candidateMedian: after, delta: after - before,
        deltaPercent: before ? (after - before) / before * 100 : null,
        baselineRange: [Math.min(...baseline), Math.max(...baseline)], candidateRange: [Math.min(...candidate), Math.max(...candidate)] };
    }
  }
  return result;
}

async function availablePorts(port, udpPort, workers) {
  const tcp = net.createServer();
  await new Promise((yes, no) => { tcp.once('error', no); tcp.listen(port, '127.0.0.1', yes); });
  await new Promise(yes => tcp.close(yes));
  for (let i = 0; i < workers; i++) {
    const udp = dgram.createSocket('udp4');
    // Match the server worker's wildcard UDP bind, not just the loopback target.
    try { await new Promise((yes, no) => { udp.once('error', no); udp.bind(udpPort + i, '0.0.0.0', yes); }); }
    finally { udp.close(); }
  }
}

export async function command(binary, args, cwd, env, logPath) {
  const log = await open(logPath, 'wx', 0o600);
  let child;
  try {
    child = spawn(binary, args, { cwd, env, stdio: ['ignore', log.fd, log.fd] });
    children.add(child);
    // No await before attaching listeners: ENOENT/fast exit can arrive immediately.
    child.completion = new Promise(resolve => {
      child.once('error', error => resolve({ code: null, error: error.message }));
      child.once('exit', (code, signal) => resolve({ code, signal }));
    }).then(result => { children.delete(child); child.result = result; return result; });
  } finally { await log.close(); }
  return child;
}

export const SERVER_SHUTDOWN_GRACE_MS = 20000;

export async function stop(child, graceMs = 5000, wait = milliseconds => delay(milliseconds, undefined, { ref: false })) {
  if (!child || child.result) return;
  child.kill('SIGTERM');
  await Promise.race([child.completion, wait(graceMs)]);
  if (!child.result) { child.kill('SIGKILL'); await child.completion; }
}

/** Attempt every owned cleanup/artifact step without replacing a workload failure. */
export async function runFinalizers(primaryError, steps, reportError = () => console.error('Run finalization failed; inspect retained artifacts.')) {
  let firstError;
  for (const step of steps) {
    try { await step(); }
    catch (error) {
      firstError ??= error;
      // Reporting is best effort too: a broken output stream must not prevent
      // stopping the remaining owned children or replace the original failure.
      try { reportError(error); } catch {}
    }
  }
  if (!primaryError && firstError) throw firstError;
}

export async function finishCapture(capture, drain = () => delay(2000)) {
  if (!capture) return;
  // Let buffered packet headers reach tcpdump after the generator exits. TERM
  // flushes tcpdump's output, but does not drain unread kernel capture buffers.
  // This bounded grace is outside the measurement window; timestamps still
  // determine actual coverage, especially when the packet cap was reached.
  if (!capture.result) await drain();
  await stop(capture);
  // tcpdump handles TERM by flushing and exiting zero. A signal-only exit or
  // forced KILL does not establish a successfully finalized capture.
  if (capture.result?.code !== 0) throw new Error(`Packet capture did not finish cleanly: ${JSON.stringify(capture.result)}`);
}

async function metrics(url, token) {
  const response = await fetch(`${url}/metrics`, { headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(2000) });
  if (!response.ok) throw new Error(`Metrics HTTP ${response.status}`);
  const raw = await response.text();
  const values = Object.fromEntries(raw.split('\n').filter(l => /^simplestchat_\w+ \d/.test(l)).map(l => { const [key, value] = l.split(' '); return [key, Number(value)]; }));
  return { raw, values };
}

async function sample(child) {
  if (!child || child.result) return null;
  try {
    const { stdout } = await exec('ps', ['-p', String(child.pid), '-o', 'rss=,time='], { timeout: 2000, env: { ...cleanEnv(), LC_ALL: 'C' } });
    const [rss, cpu] = stdout.trim().split(/\s+/);
    if (!rss || !cpu) return null;
    return { rssKiB: Number(rss), cpuSeconds: cpuSeconds(cpu) };
  } catch { return null; }
}

export function captureArguments(options, directory) {
  if (options.purpose !== 'diagnostic' || !['lo', 'lo0'].includes(options.captureInterface)) {
    throw new Error('Packet capture requires diagnostic mode and a loopback interface (lo/lo0)');
  }
  const lastPort = options.udpPort + options.workers - 1;
  const packetLimit = options.diagnosticDetail === 'capture-only' ? '2000000' : '500000';
  return ['-i', options.captureInterface, '-p', '-nn', '-s', '64', '-B', '4096', '-U', '-c', packetLimit,
    '-w', join(directory, 'media-headers.pcap'),
    `udp and host 127.0.0.1 and portrange ${options.udpPort}-${lastPort}`];
}

export function diagnosticPolicy(options) {
  const full = options.purpose === 'diagnostic' && options.diagnosticDetail !== 'capture-only';
  return { generatorArgs: full ? ['--diagnostics', '--departure', options.departure ?? 'abrupt'] : [], requireSnapshots: full,
    serverLog: full ? 'warn,simplestChat::media::transport_manager=info,simplestChat::lifecycle=debug' : 'error',
    generatorLog: full ? 'warn,load_test=info' : 'error' };
}

/** Runner wall-clock anchors, with monotonic offsets to expose clock changes.
 * Generator-reported boundaries remain separate from process launch/exit.
 */
export function createLifecycleTimeline(departure, { now = () => performance.now(), wall = () => new Date().toISOString() } = {}) {
  const start = now();
  const timeline = { schemaVersion: 1, departure, events: [], workload: null };
  return { timeline, mark: event => {
    timeline.events.push({ event, at: wall(), elapsedMs: now() - start });
  } };
}

/** A failing completed workload still supplies useful nominal time boundaries.
 * Never substitute metadata from a different executable or workload configuration.
 */
export function lifecycleWorkload(summary, options, generatorSha256) {
  const run = summary?.run;
  const config = run?.configuration;
  if (summary?.schemaVersion !== 2 || run?.completed !== true || config?.diagnostics !== true ||
      run?.provenance?.generatorBinarySha256 !== generatorSha256 || config.departure !== options.departure ||
      config.rampUpSecs !== options.rampUp || config.warmupSecs !== options.warmup || config.durationSecs !== options.duration) return null;
  return { startedAt: run.startedAt, finishedAt: run.finishedAt,
    rampUpSecs: config.rampUpSecs, warmupSecs: config.warmupSecs, durationSecs: config.durationSecs };
}

export function serverDiagnosticEnvironment(options, directory) {
  if (!diagnosticPolicy(options).requireSnapshots) return {};
  return { DIAGNOSTICS_PATH: join(directory, 'server-diagnostics.jsonl'), DIAGNOSTICS_MAX_RECORDS: '10000', MEDIA_DIAGNOSTICS_ENABLED: 'true',
    // Include bounded startup, generator deadline and cleanup, not just steady load.
    DIAGNOSTICS_DURATION_SECS: String(Math.max(300, options.rampUp + options.warmup + options.duration + 200)) };
}

export function diagnosticRunStatus(rows) {
  const { workloadPassed, serverShutdownPassed } = performanceRunStatus(rows);
  const diagnosticCoverageComplete = rows.length > 0 && rows.every(row =>
    row.diagnosticCoverage?.requested === false || row.diagnosticCoverage?.complete === true);
  const mediaDiagnosticCoverageComplete = rows.length > 0 && rows.every(row =>
    row.mediaDiagnosticCoverage?.requested === false || row.mediaDiagnosticCoverage?.complete === true);
  const lifecycleDiagnosticCoverageComplete = rows.length > 0 && rows.every(row =>
    row.lifecycleDiagnosticCoverage?.requested === false || row.lifecycleDiagnosticCoverage?.complete === true);
  return { workloadPassed, diagnosticCoverageComplete, mediaDiagnosticCoverageComplete, lifecycleDiagnosticCoverageComplete, serverShutdownPassed,
    passed: workloadPassed && diagnosticCoverageComplete && mediaDiagnosticCoverageComplete && lifecycleDiagnosticCoverageComplete && serverShutdownPassed };
}

export function performanceRunStatus(rows) {
  const workloadPassed = rows.length > 0 && rows.every(row => row.workloadPassed === true);
  const serverShutdownPassed = rows.length > 0 && rows.every(row =>
    row.serverExit?.code === 0 && !row.serverExit.signal && !row.serverExit.error);
  return { workloadPassed, serverShutdownPassed, passed: workloadPassed && serverShutdownPassed };
}

export async function collectServerDiagnostics(options, directory, server, readReport = readDiagnosticReport) {
  if (!diagnosticPolicy(options).requireSnapshots) return null;
  if (server && !server.result) throw new Error('Server diagnostics must be read after the owned server stops');
  return readReport(join(directory, 'server-diagnostics.jsonl'));
}

export async function verifyExecutable(binary, expectedSha256, role) {
  if (hash(await readFile(binary)) !== expectedSha256) {
    throw new Error(`${role} binary changed after the comparison started`);
  }
}

/** Fingerprint the generator's explicit runtime source inputs, not its build environment. */
export async function generatorSourceIdentity(root) {
  const sources = await Promise.all(['load_tests/bin/load_test.rs', 'load_tests/clients/metrics.rs',
    'load_tests/clients/measurement.rs', 'load_tests/clients/media_generator.rs',
    'load_tests/clients/webrtc_client.rs', 'load_tests/clients/subscriptions.rs']
    .map(file => readFile(join(root, file))));
  return `sha256:${hash(Buffer.concat(sources))}`;
}

const trackedSourcePaths = ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'src', 'vendor', 'build/pip-constraints.txt'];
const sourcePaths = [...trackedSourcePaths, 'build.rs', '.cargo/config', '.cargo/config.toml'];
const pathOrder = (left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right));

/** Hash versioned and nonignored untracked source inputs, not the build environment.
 * Missing tracked inputs are omitted from the tree and listed separately; this
 * makes a deletion's tree hash identical before and after git add/commit.
 * Symlinks are hashed as links, never followed into private/outside files.
 */
export async function sourceTreeIdentity(root) {
  const gitPaths = async args => (await exec('git', ['-C', root, ...args, '--', ...sourcePaths], { maxBuffer: 8 * 1024 * 1024 })).stdout.split('\0').filter(Boolean);
  const [working, committed] = await Promise.all([
    gitPaths(['ls-files', '--cached', '--others', '--exclude-standard', '-z']),
    gitPaths(['ls-tree', '-r', '--name-only', '-z', 'HEAD']),
  ]);
  const paths = [...new Set([...working, ...committed])]
    .filter(file => !['CLAUDE.md', 'CLAUDE.local.md'].includes(file.split('/').at(-1)))
    .sort(pathOrder);
  const entries = [], missing = [];
  for (const file of paths) {
    let info;
    try { info = await lstat(join(root, file)); }
    catch (error) {
      if (error.code === 'ENOENT') { missing.push(file); continue; }
      throw error;
    }
    if (info.isSymbolicLink()) entries.push([file, 'symlink', hash(await readlink(join(root, file)))]);
    else if (info.isFile()) entries.push([file, 'file', Boolean(info.mode & 0o111), hash(await readFile(join(root, file)))]);
    else throw new Error(`Unsupported build input type: ${file}`);
  }
  return {
    sourceTreeSha256: hash(JSON.stringify({ format: 'simplestchat-source-tree-v1', entries })),
    sourceTreeFiles: entries.length,
    sourceTreeMissingPaths: missing,
    sourceTreeScope: sourcePaths,
  };
}

export function serverRevisionLabel(server) {
  return `git:${server.revision};source:sha256:${server.sourceTreeSha256};binary:sha256:${server.binarySha256}`;
}

export async function identity(root, binary) {
  const git = async args => (await exec('git', ['-C', root, ...args])).stdout.trim();
  return { root, binary, revision: await git(['rev-parse', 'HEAD']),
    // Retain the existing field and exact diff scope for older report readers.
    trackedDiffSha256: hash(await git(['diff', 'HEAD', '--', ...trackedSourcePaths])),
    ...await sourceTreeIdentity(root),
    cargoLockSha256: hash(await readFile(join(root, 'Cargo.lock'))),
    binarySha256: hash(await readFile(binary)), binaryBytes: (await stat(binary)).size };
}

async function runOne(options, variant, scenario, repetition, manifest) {
  const name = `${scenario.name}-${repetition}-${variant}`;
  const directory = join(options.output, name); await mkdir(directory, { mode: 0o700 });
  const identity = manifest.servers[variant];
  const origin = `http://127.0.0.1:${options.port}`;
  const token = randomBytes(32).toString('hex');
  const diagnostics = diagnosticPolicy(options);
  const env = { ...cleanEnv(), BIND_ADDR: '127.0.0.1', PORT: String(options.port), ANNOUNCE_IP: '127.0.0.1',
    MEDIA_WORKERS: String(options.workers), WEBRTC_SERVER_PORT_BASE: String(options.udpPort),
    ALLOW_AD_HOC_ROOMS: 'true', ALLOWED_ORIGINS: origin, REGISTRATION_ENABLED: 'false',
    MAX_CONNECTIONS_PER_IP: '128', WS_HANDSHAKES_PER_MINUTE: '600', METRICS_TOKEN: token,
    RUST_LOG: diagnostics.serverLog, ...serverDiagnosticEnvironment(options, directory) };
  let server, generator, capture, diagnosticRow, performanceRow, mediaSampler, primaryError;
  const lifecycle = diagnostics.requireSnapshots ? createLifecycleTimeline(options.departure) : null;
  let lifecycleTimelineSaved = false;
  const samples = [];
  const startedAt = new Date().toISOString();
  lifecycle?.mark('run_started');
  try {
    await Promise.all([
      verifyExecutable(options.generator, manifest.generator.binarySha256, 'Generator'),
      verifyExecutable(identity.binary, identity.binarySha256, `${variant} server`),
    ]);
    await availablePorts(options.port, options.udpPort, options.workers);
    server = await command(identity.binary, [], identity.root, env, join(directory, 'server.log'));
    if (lifecycle) void server.completion.then(() => lifecycle.mark('server_exited'));
    let ready = false;
    for (let i = 0; i < 100 && !server.result; i++) {
      try { const response = await fetch(`${origin}/health`, { signal: AbortSignal.timeout(500) }); ready = response.ok; } catch {}
      if (ready) break;
      await delay(100);
    }
    if (!ready || server.result) throw new Error('Owned server did not become ready');
    lifecycle?.mark('server_ready');
    const before = await metrics(origin, token); await writeFile(join(directory, 'metrics-before.txt'), before.raw, { flag: 'wx' });
    if (options.captureInterface) {
      const captureLog = join(directory, 'capture.log');
      const captureArgs = captureArguments(options, directory);
      await json(join(directory, 'capture-invocation.json'), { command: 'tcpdump', args: captureArgs,
        limitations: `First ${captureArgs[captureArgs.indexOf('-c') + 1]} matching packets only; 64-byte snapshots, not full payloads. Check capture.log for kernel drops.` });
      // Start only after our server owns the selected media port. Never elevate.
      capture = await command('tcpdump', captureArgs, options.candidateRoot, cleanEnv(), captureLog);
      let listening = false;
      for (let i = 0; i < 100 && !capture.result; i++) {
        listening = /listening on /.test(await readFile(captureLog, 'utf8'));
        if (listening) break;
        await delay(20);
      }
      if (!listening || capture.result) throw new Error('Loopback capture unavailable; see capture.log (no privileges were requested)');
    }
    const args = ['--server', origin.replace('http:', 'ws:') + '/ws', '--clients', String(scenario.clients),
      '--rooms', String(scenario.rooms), '--room', `benchmark-${randomBytes(6).toString('hex')}`,
      '--duration', String(options.duration), '--ramp-up', String(options.rampUp), '--warmup', String(options.warmup),
      '--mode', scenario.mode, '--quality', '480p', '--fps', '30', '--max-audio', '4', '--max-video', '4',
      '--output-dir', directory, '--run-label', name, '--server-revision', serverRevisionLabel(identity),
      '--generator-revision', manifest.generator.sourceIdentity, ...scenario.extra,
      ...diagnostics.generatorArgs];
    await json(join(directory, 'invocation.json'), { startedAt, variant, scenario, repetition, args, serverConfiguration: Object.fromEntries(Object.entries(env).filter(([k]) => !['METRICS_TOKEN', 'PATH', 'TMPDIR'].includes(k))) });
    const start = performance.now();
    lifecycle?.mark('generator_started');
    generator = await command(options.generator, args, options.candidateRoot,
      { ...cleanEnv(), RUST_LOG: diagnostics.generatorLog }, join(directory, 'generator.log'));
    if (lifecycle) void generator.completion.then(() => lifecycle.mark('generator_exited'));
    if (diagnostics.requireSnapshots) mediaSampler = createMediaSampler(options, {
      now: () => performance.now() - start,
      alive: () => ({ server: Boolean(server && !server.result), generator: Boolean(generator && !generator.result) }),
      request: () => fetchMediaSnapshot({ origin, token }),
      persist: sample => writeFile(join(directory, `server-media-sample-${String(sample.ordinal).padStart(2, '0')}.json`),
        `${JSON.stringify(sample, null, 2)}\n`, { flag: 'wx', mode: 0o600 }),
    });
    const deadline = (options.rampUp + options.warmup + options.duration + 135) * 1000;
    while (!generator.result) {
      if (server.result) throw new Error('Server exited during load');
      if (capture?.result && capture.result.code !== 0) throw new Error(`Packet capture failed: ${JSON.stringify(capture.result)}`);
      if (options.purpose !== 'diagnostic') {
        const [serverSample, generatorSample] = await Promise.all([sample(server), sample(generator)]);
        samples.push({ elapsedMs: performance.now() - start, server: serverSample, generator: generatorSample });
      }
      if (mediaSampler) await mediaSampler.poll();
      if (performance.now() - start > deadline) throw new Error('Outer generator deadline exceeded');
      await delay(500);
    }
    await finishCapture(capture);
    if (generator.result.code !== 0) throw new Error(`Generator failed: ${JSON.stringify(generator.result)}`);
    const summary = JSON.parse(await readFile(join(directory, 'load_test_summary.json'), 'utf8'));
    if (lifecycle) lifecycle.timeline.workload = lifecycleWorkload(summary, options, manifest.generator.binarySha256);
    const timeoutMarker = await stat(join(directory, 'load_test_timeout.json')).then(() => true, error => { if (error.code === 'ENOENT') return false; throw error; });
    if (timeoutMarker || summary.schemaVersion !== 2 || !summary.run?.completed || !summary.run?.passed || summary.totalErrors || summary.failedConnections || summary.failedConsumers) throw new Error('Incomplete or failing generator report');
    if (diagnostics.requireSnapshots && (summary.run.configuration?.diagnostics !== true || summary.diagnosticFailures !== 0)) throw new Error('Missing or failing generator diagnostics');
    if (diagnostics.requireSnapshots && summary.run.configuration?.departure !== options.departure) throw new Error('Generator departure mode does not match the requested diagnostic run');
    if (summary.run.provenance?.generatorBinarySha256 !== manifest.generator.binarySha256) throw new Error('Generator report does not match the frozen executable');
    const finish = await metrics(origin, token); await writeFile(join(directory, 'metrics-finish.txt'), finish.raw, { flag: 'wx' });
    let cleaned = false;
    let final;
    const cleanupStart = performance.now();
    for (let i = 0; i < 80 && !server.result; i++) {
      final = await metrics(origin, token);
      cleaned = ['connections', 'rooms', 'participants'].every(k => final.values[`simplestchat_${k}_active`] === 0);
      if (cleaned) break;
      await delay(500);
    }
    if (!cleaned || server.result) throw new Error('Server room/session cleanup did not complete within40 seconds');
    lifecycle?.mark('cleanup_observed');
    await writeFile(join(directory, 'metrics-cleanup.txt'), final.raw, { flag: 'wx' });
    if (options.purpose === 'diagnostic') {
      diagnosticRow = { purpose: 'diagnostic', diagnosticDetail: options.diagnosticDetail, departure: options.departure, scenario: scenario.name, variant, repetition, startedAt,
        completedAt: new Date().toISOString(), cleanupMs: performance.now() - cleanupStart,
        workloadPassed: true,
        media: { validatedConsumers: summary.validatedConsumers, failedConsumers: summary.failedConsumers,
          skippedShortLivedConsumers: summary.skippedShortLivedConsumers } };
      // The finally block adds recorder coverage only after stopping the server.
      return diagnosticRow;
    }
    const windowStart = (options.rampUp + options.warmup) * 1000;
    const windowEnd = windowStart + options.duration * 1000;
    const serverResources = resourceSummary(samples, 'server', windowStart, windowEnd);
    const generatorResources = resourceSummary(samples, 'generator', windowStart, windowEnd);
    performanceRow = { purpose: 'performance', scenario: scenario.name, variant, repetition, startedAt,
      workloadPassed: true,
      joinP99Ms: summary.p99ConnectionTimeMs, sendReadyP99Ms: summary.sendMediaReady.p99Ms,
      receiveReadyP99Ms: summary.receiveMediaReady.p99Ms,
      receivedPacketsPerSecond: summary.measurement.packetsReceived / (summary.measurement.durationMs / 1000),
      serverCpuPercent: serverResources.cpuPercentOfOneCore, serverPeakRssMiB: serverResources.peakRssMiB,
      generatorCpuPercent: generatorResources.cpuPercentOfOneCore, generatorPeakRssMiB: generatorResources.peakRssMiB,
      cleanupMs: performance.now() - cleanupStart, serverResources, generatorResources,
      media: { validatedConsumers: summary.validatedConsumers, failedConsumers: summary.failedConsumers, skippedShortLivedConsumers: summary.skippedShortLivedConsumers } };
    // Final result and success output require the owned server's exit below.
    return performanceRow;
  } catch (error) {
    primaryError = error;
    await runFinalizers(primaryError, [() => json(join(directory, 'failure.json'),
      { startedAt, error: error.stack, server: server?.result, generator: generator?.result })]);
    throw error;
  } finally {
    await runFinalizers(primaryError, [async () => {
      // Failure must not discard the owned server's last observable state.
      if (server && !server.result) {
        try {
          const last = await metrics(origin, token);
          await writeFile(join(directory, 'metrics-stop.txt'), last.raw, { flag: 'wx' });
        } catch (error) {
          console.error(`Final metrics unavailable for ${name}: ${error.message}`);
        }
      }
    }, () => stop(generator), () => stop(capture), async () => {
      if (server && !server.result) lifecycle?.mark('server_stop_requested');
      await stop(server, SERVER_SHUTDOWN_GRACE_MS);
    }, async () => {
      if (lifecycle) {
        if (!lifecycle.timeline.workload) {
          try {
            const summary = JSON.parse(await readFile(join(directory, 'load_test_summary.json'), 'utf8'));
            lifecycle.timeline.workload = lifecycleWorkload(summary, options, manifest.generator.binarySha256);
          } catch { /* Keep missing boundaries explicit without replacing the original failure. */ }
        }
        await writeFile(join(directory, 'lifecycle-timeline.json'), `${JSON.stringify(lifecycle.timeline, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
        lifecycleTimelineSaved = true;
      }
    },
    () => json(join(directory, 'server-exit.json'), server?.result ?? null),
    () => json(join(directory, 'generator-exit.json'), generator?.result ?? null), async () => {
      let mediaReport;
      let lifecycleReport;
      if (lifecycle) {
        try { lifecycleReport = await readLifecycleReport(join(directory, 'server.log'), lifecycle.timeline, { expectedParticipants: scenario.clients }); }
        catch { lifecycleReport = { schemaVersion: 1, coverage: { available: false, complete: false, issues: ['lifecycle_report_failed'] } }; }
        if (!lifecycleTimelineSaved) {
          lifecycleReport.coverage.complete = false;
          lifecycleReport.coverage.issues.push('timeline_write_failed');
        }
        try { await writeFile(join(directory, 'server-lifecycle-report.json'), `${JSON.stringify(lifecycleReport, null, 2)}\n`, { flag: 'wx', mode: 0o600 }); }
        catch {
          lifecycleReport.coverage.complete = false;
          lifecycleReport.coverage.issues.push('lifecycle_report_write_failed');
          console.error('Server lifecycle report could not be saved; coverage is incomplete.');
        }
      }
      if (diagnostics.requireSnapshots) {
        let mediaSamples = [], generatorResults;
        try { if (mediaSampler) mediaSamples = await mediaSampler.finish(); } catch {}
        try {
          const generatorSummary = JSON.parse(await readFile(join(directory, 'load_test_summary.json'), 'utf8'));
          const timedOut = await stat(join(directory, 'load_test_timeout.json')).then(() => true, error => {
            if (error.code === 'ENOENT') return false;
            throw error;
          });
          if (generatorSummary.schemaVersion === 2 && generatorSummary.run?.completed === true &&
              generatorSummary.run.configuration?.diagnostics === true && generatorSummary.diagnosticFailures === 0 && !timedOut &&
              generatorSummary.run.provenance?.generatorBinarySha256 === manifest.generator.binarySha256) {
            generatorResults = await readGeneratorResults(join(directory, 'load_test_results.json'));
          }
        } catch {}
        try { mediaReport = correlateMediaDiagnostics(mediaSamples, generatorResults); }
        catch { mediaReport = { schemaVersion: 1, coverage: { available: false, complete: false, issues: ['media_report_failed'] }, samples: [], consumers: [] }; }
        try {
          await writeFile(join(directory, 'server-media-report.json'), `${JSON.stringify(mediaReport, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
        } catch {
          mediaReport.coverage.complete = false;
          mediaReport.coverage.issues.push('media_report_write_failed');
          console.error('Server media diagnostic report could not be saved; coverage is incomplete.');
        }
      }
      const diagnosticReport = await collectServerDiagnostics(options, directory, server);
      if (diagnosticReport) {
        await json(join(directory, 'server-diagnostics-report.json'), diagnosticReport);
      }
      if (diagnosticRow) {
        diagnosticRow.serverExit = server?.result ?? null;
        diagnosticRow.diagnosticCoverage = diagnosticReport
          ? { requested: true, ...diagnosticReport.coverage, report: 'server-diagnostics-report.json' }
          : { requested: false, complete: null, scope: 'not_requested' };
        diagnosticRow.mediaDiagnosticCoverage = mediaReport
          ? { requested: true, ...mediaReport.coverage, report: 'server-media-report.json' }
          : { requested: false, complete: null };
        diagnosticRow.lifecycleDiagnosticCoverage = lifecycleReport
          ? { requested: true, ...lifecycleReport.coverage, report: 'server-lifecycle-report.json' }
          : { requested: false, complete: null };
        const status = diagnosticRunStatus([diagnosticRow]);
        diagnosticRow.serverShutdownPassed = status.serverShutdownPassed;
        diagnosticRow.passed = status.passed;
        await json(join(directory, 'result.json'), diagnosticRow);
        console.log(`PASS workload ${name}: ${diagnosticRow.media.validatedConsumers} validated consumers; server diagnostics ${diagnosticReport ? diagnosticReport.coverage.complete ? 'complete' : 'INCOMPLETE' : 'not requested'}; media correlation ${mediaReport ? mediaReport.coverage.complete ? 'complete' : 'INCOMPLETE' : 'not requested'}; lifecycle ${lifecycleReport ? lifecycleReport.coverage.complete ? 'complete' : 'INCOMPLETE' : 'not requested'}; server shutdown ${status.serverShutdownPassed ? 'clean' : 'FAILED'}; no performance comparison`);
      }
      if (performanceRow) {
        performanceRow.serverExit = server?.result ?? null;
        Object.assign(performanceRow, performanceRunStatus([performanceRow]), { completedAt: new Date().toISOString() });
        await json(join(directory, 'result.json'), performanceRow);
        console.log(`${performanceRow.passed ? 'PASS' : 'FAIL'} ${name}: joinP99=${performanceRow.joinP99Ms}ms serverCPU=${performanceRow.serverCpuPercent.toFixed(1)}% RSS=${performanceRow.serverPeakRssMiB.toFixed(1)}MiB; server shutdown ${performanceRow.serverShutdownPassed ? 'clean' : 'FAILED'}`);
      }
    }, async () => {
      if (capture) await json(join(directory, 'capture-exit.json'), capture.result);
    }, () => json(join(directory, 'resources.json'), samples)]);
  }
}

export function parseOptions(args) {
  const raw = {};
  const keys = new Set(['baseline-root', 'baseline-bin', 'candidate-root', 'candidate-bin', 'generator', 'generator-source-root', 'output', 'clients', 'duration', 'warmup', 'ramp-up', 'repetitions', 'workers', 'port', 'udp-port', 'scenarios', 'purpose', 'capture-interface', 'diagnostic-detail', 'departure']);
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i]?.replace(/^--/, '');
    if (!args[i]?.startsWith('--') || !keys.has(key) || !args[i + 1] || args[i + 1].startsWith('--') || raw[key]) throw new Error(`Unknown, duplicate or incomplete option: ${args[i]}`);
    raw[key] = args[i + 1];
  }
  const options = {};
  for (const key of ['baseline-root', 'baseline-bin', 'candidate-root', 'candidate-bin', 'generator', 'output']) {
    if (!raw[key]) throw new Error(`Required: --${key}`);
    options[key.replace(/-([a-z])/g, (_, l) => l.toUpperCase())] = resolve(raw[key]);
  }
  options.generatorSourceRoot = resolve(raw['generator-source-root'] ?? options.candidateRoot);
  options.purpose = raw.purpose ?? 'performance';
  if (!['performance', 'diagnostic'].includes(options.purpose)) throw new Error('--purpose must be performance or diagnostic');
  options.captureInterface = raw['capture-interface'];
  if (options.captureInterface && (options.purpose !== 'diagnostic' || !['lo', 'lo0'].includes(options.captureInterface))) {
    throw new Error('--capture-interface requires --purpose diagnostic and lo/lo0');
  }
  options.diagnosticDetail = raw['diagnostic-detail'] ?? 'full';
  if (!['full', 'capture-only'].includes(options.diagnosticDetail) ||
      (raw['diagnostic-detail'] && options.purpose !== 'diagnostic') ||
      (options.diagnosticDetail === 'capture-only' && !options.captureInterface)) {
    throw new Error('--diagnostic-detail must be full or capture-only in diagnostic mode; capture-only requires --capture-interface');
  }
  options.departure = raw.departure ?? 'abrupt';
  if (!['abrupt', 'explicit-leave'].includes(options.departure) ||
      (raw.departure && (options.purpose !== 'diagnostic' || options.diagnosticDetail !== 'full'))) {
    throw new Error('--departure must be abrupt or explicit-leave and requires full diagnostic mode');
  }
  for (const [key, fallback, minimum, maximum] of [['duration', 60, 3, 180], ['warmup', 10, 2, 60], ['ramp-up', 5, 1, 600], ['repetitions', 3, 1, 5], ['workers', 1, 1, 4], ['port', 3129, 1024, 65535], ['udp-port', 41100, 1024, 65531]]) {
    const value = Number(raw[key] ?? fallback);
    if (!Number.isInteger(value) || value < minimum || value > maximum) throw new Error(`--${key} must be ${minimum}–${maximum}`);
    options[key.replace(/-([a-z])/g, (_, l) => l.toUpperCase())] = value;
  }
  options.clients = (raw.clients ?? '10').split(',').map(Number);
  if (!options.clients.length || options.clients.some(v => !Number.isInteger(v) || v < 2 || v > 100) || new Set(options.clients).size !== options.clients.length) throw new Error('--clients must be unique counts between2 and100');
  const scenarios = (raw.scenarios ?? 'conference').split(',');
  if (scenarios.some(s => !['conference', 'multi-room', 'webinar', 'audio', 'churn'].includes(s)) || new Set(scenarios).size !== scenarios.length) throw new Error('Unknown/duplicate scenario');
  options.scenarios = options.clients.flatMap(clients => scenarios.map(name => ({ name: `${name}-${clients}`, clients,
    rooms: name === 'multi-room' ? Math.min(4, Math.floor(clients / 2)) : 1,
    mode: name === 'webinar' ? 'webinar' : 'conference', extra: name === 'audio' ? ['--audio-only'] : name === 'churn' ? ['--churn-rate', String(Math.ceil(clients / 5) / options.duration)] : [] })));
  for (const scenario of options.scenarios) {
    // Preserve server admission safeguards: at most30 joins/IP and10 joins/room/IP
    // in each60-second window. Raising WS upgrade limits does not raise these.
    const spacing = Math.max(scenario.clients > 30 ? 60.5 / 30 : 0,
      Math.ceil(scenario.clients / scenario.rooms) > 10 ? 60.5 / (10 * scenario.rooms) : 0);
    const minimumRamp = Math.ceil(spacing * scenario.clients);
    if (options.rampUp < minimumRamp) throw new Error(`${scenario.name} exceeds loopback join admission limits; use --ramp-up ${minimumRamp} or a smaller/multi-room workload`);
    if (scenario.extra.includes('--churn-rate') && scenario.clients >= 10) throw new Error('Use fewer than10 local clients for churn; reconnects also consume the room/IP join budget');
  }
  return options;
}

async function main() {
  const options = parseOptions(process.argv.slice(2));
  await mkdir(options.output, { recursive: false, mode: 0o700 });
  const manifest = { schemaVersion: 1, startedAt: new Date().toISOString(), options,
    environment: { platform: os.platform(), release: os.release(), arch: os.arch(), cpu: os.cpus()[0]?.model, logicalCpus: os.cpus().length, memoryBytes: os.totalmem(), node: process.version, loadAverageBefore: os.loadavg() },
    servers: { baseline: await identity(options.baselineRoot, options.baselineBin), candidate: await identity(options.candidateRoot, options.candidateBin) },
    generator: { binary: options.generator, binarySha256: hash(await readFile(options.generator)), sourceRoot: options.generatorSourceRoot, sourceIdentity: await generatorSourceIdentity(options.generatorSourceRoot) },
    orchestratorSha256: hash(await readFile(new URL(import.meta.url))),
    diagnosticReporterSha256: hash(await readFile(new URL('./diagnostic-report.mjs', import.meta.url))),
    mediaDiagnosticReporterSha256: hash(await readFile(new URL('./media-diagnostic-report.mjs', import.meta.url))),
    lifecycleDiagnosticReporterSha256: hash(await readFile(new URL('./lifecycle-diagnostic-report.mjs', import.meta.url))),
    limitations: ['Co-located server/generator: not production capacity.', 'Source-tree hashes cover the listed versioned and nonignored untracked inputs, not ignored/ancestor Cargo configuration, environment flags, toolchains or external native libraries; record those build inputs separately.', 'Symlinks are identified by their link target, not external target contents. Source snapshots and independently frozen binary hashes do not attest that a binary was built from that snapshot.', 'Diagnostic runs skip resource sampling and never produce performance comparisons; full diagnostics change logging, while capture-only retains error-only logs.', 'ps sampled every~500ms in performance mode; RSS is sampled peak and CPU is total user+system for each process, including in-process media workers.', 'CPU window excludes first/last partial sampling intervals. Process launch/hash overhead causes a small offset from the generator clock; no child-process resource attribution.', 'Synthetic queued RTP is not confirmed egress; receive counts do not measure loss without expected fan-out.', 'Latency/CPU differences are descriptive, not an established regression budget.'] };
  await json(join(options.output, 'manifest.json'), manifest);
  const rows = [];
  try {
    for (const scenario of options.scenarios) for (let rep = 1; rep <= options.repetitions; rep++) {
      for (const variant of rep % 2 ? ['baseline', 'candidate'] : ['candidate', 'baseline']) rows.push(await runOne(options, variant, scenario, rep, manifest));
    }
    const status = options.purpose === 'diagnostic' ? diagnosticRunStatus(rows) : performanceRunStatus(rows);
    await json(join(options.output, 'comparison.json'), { purpose: options.purpose, completed: true, ...status, rows,
      comparison: options.purpose === 'diagnostic' || !status.passed ? null : comparison(rows) });
    if (!status.passed) {
      console.error('Run did not pass workload, diagnostic coverage or server shutdown gates; see per-run reports.');
      process.exitCode = 1;
    }
  } catch (error) {
    await json(join(options.output, 'comparison.json'), { purpose: options.purpose, completed: false, passed: false, rows, error: error.stack });
    throw error;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, async () => { for (const child of children) await stop(child); process.exit(signal === 'SIGINT' ? 130 : 143); });
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
