#!/usr/bin/env node
// Production-shape runs of the synthetic RTP workload inside a Podman Linux VM:
// the server container gets the production CPU quota and memory limit, the
// generator shares its network namespace over loopback, and an optional netem
// sidecar impairs the UDP media path. Reuses the local runner's measurement
// gates and summaries; adds cgroup throttling and per-thread CPU attribution
// that the local runner cannot see.
//
// Usage:
//   node load_tests/benchmark-podman.mjs --server-image localhost/simplestchat-production:dev \
//     --generator-image localhost/simplestchat-loadtest:dev --output results/capacity.<stamp> \
//     --clients 100,200 --scenarios multi-room --subscription-plan ring-v1 --subscription-seed 17 \
//     --workers 2 --cpus 2 --memory 2g --ramp-up 405 --duration 120 [--netem "loss 5% delay 50ms 10ms"]
import { execFile, spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { resourceSummary, verifySubscriptionReport, subscriptionArguments, performanceRunStatus, linuxProcessSample, runFinalizers } from './benchmark-local.mjs';

const exec = promisify(execFile);
const hash = value => createHash('sha256').update(value).digest('hex');
const json = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
const median = values => { const sorted = [...values].sort((a, b) => a - b); const m = Math.floor(sorted.length / 2); return sorted.length % 2 ? sorted[m] : (sorted[m - 1] + sorted[m]) / 2; };
const plannedSubscriptionModes = new Set(['ring-v1', 'hotspot-v1']);
const NETEM_QUEUE_LIMIT = 200000;
const containers = new Set();

async function podman(args, { timeout = 60000, input } = {}) {
  const { stdout } = await exec('podman', args, { timeout, maxBuffer: 64 * 1024 * 1024, input });
  return stdout;
}

/** Parse a dump of every thread's stat file under /proc/1/task into CPU ticks per thread name. */
export function threadTicks(dump) {
  const byName = {};
  for (const line of dump.split('\n')) {
    const close = line.lastIndexOf(')');
    if (close < 0) continue;
    const name = line.slice(line.indexOf('(') + 1, close);
    const fields = line.slice(close + 2).trim().split(/\s+/);
    const ticks = Number(fields[11]) + Number(fields[12]);
    if (!Number.isFinite(ticks)) continue;
    byName[name] = (byName[name] ?? 0) + ticks;
  }
  return byName;
}

/** Parse cgroup v2 `cpu.stat`. */
export function cgroupCpuStat(text) {
  const values = Object.fromEntries(text.split('\n').filter(Boolean).map(line => { const [key, value] = line.trim().split(/\s+/); return [key, Number(value)]; }));
  for (const key of ['usage_usec', 'nr_periods', 'nr_throttled', 'throttled_usec']) {
    if (!Number.isFinite(values[key])) throw new Error(`cpu.stat lacks ${key}`);
  }
  return values;
}

/** Thread-name groups the attribution reports; everything else is `other`. */
export function threadGroup(name) {
  if (name.startsWith('mediasoup-worke')) return 'mediasoupWorkers';
  if (name.startsWith('tokio-runtime-w') || name.startsWith('tokio-rt-worker')) return 'tokioWorkers';
  return 'other';
}

/**
 * CPU seconds per thread group across the window plus cgroup throttling. The
 * per-group figures use first/last sums, so a thread that exits inside the
 * window takes its accumulated time with it; the cgroup usage is exact.
 */
export function serverAttribution(samples, start, end, clockTicks, quotaCpus) {
  const window = samples.filter(s => s.elapsedMs >= start && s.elapsedMs <= end && s.serverDetail);
  if (window.length < 2) throw new Error('Insufficient server detail samples in measurement window');
  const first = window[0], last = window.at(-1);
  const seconds = (last.elapsedMs - first.elapsedMs) / 1000;
  const groups = {};
  for (const [name, ticks] of Object.entries(last.serverDetail.threads)) {
    const group = threadGroup(name);
    groups[group] = (groups[group] ?? 0) + (ticks - (first.serverDetail.threads[name] ?? 0)) / clockTicks;
  }
  const cgroup = {
    usageSeconds: (last.serverDetail.cgroup.usage_usec - first.serverDetail.cgroup.usage_usec) / 1e6,
    periods: last.serverDetail.cgroup.nr_periods - first.serverDetail.cgroup.nr_periods,
    throttledPeriods: last.serverDetail.cgroup.nr_throttled - first.serverDetail.cgroup.nr_throttled,
    throttledSeconds: (last.serverDetail.cgroup.throttled_usec - first.serverDetail.cgroup.throttled_usec) / 1e6,
  };
  cgroup.cpuPercentOfQuota = cgroup.usageSeconds / seconds / quotaCpus * 100;
  cgroup.throttledPeriodFraction = cgroup.periods ? cgroup.throttledPeriods / cgroup.periods : null;
  return { sampledDurationSeconds: seconds, threadCpuSeconds: groups, cgroup };
}

/** The `tc` script a NET_ADMIN sidecar runs inside the server's network namespace. */
export function netemScript(params, scope, udpPort, workers) {
  if (!/^[a-z0-9 .%]+$/i.test(params)) throw new Error('netem parameters may contain only letters, digits, spaces, dots and percent signs');
  let match = 'match ip protocol 17 0xff';
  if (scope === 'udp-downlink') {
    const span = 2 ** Math.ceil(Math.log2(Math.max(workers, 1)));
    if (udpPort % span !== 0) throw new Error(`--udp-port must be a multiple of ${span} for downlink-only impairment of ${workers} workers`);
    match += ` match ip sport ${udpPort} 0x${(0x10000 - span).toString(16)}`;
  } else if (scope !== 'udp-both') throw new Error('--netem-scope must be udp-both or udp-downlink');
  return [
    'set -e',
    'tc qdisc add dev lo root handle 1: prio bands 3 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0',
    `tc qdisc add dev lo parent 1:3 handle 30: netem limit ${NETEM_QUEUE_LIMIT} ${params}`,
    `tc filter add dev lo parent 1: protocol ip prio 1 u32 ${match} flowid 1:3`,
    'tc qdisc show dev lo',
  ].join('\n');
}

export function parseOptions(args) {
  const raw = {};
  const keys = new Set(['server-image', 'generator-image', 'netem-image', 'output', 'clients', 'duration', 'warmup', 'ramp-up', 'repetitions', 'workers', 'cpus', 'memory', 'generator-cpus', 'port', 'udp-port', 'scenarios', 'subscription-plan', 'subscription-seed', 'netem', 'netem-scope', 'max-connections', 'label', 'source-revision', 'source-addresses']);
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i]?.replace(/^--/, '');
    if (!args[i]?.startsWith('--') || !keys.has(key) || args[i + 1] === undefined || raw[key]) throw new Error(`Unknown, duplicate or incomplete option: ${args[i]}`);
    raw[key] = args[i + 1];
  }
  const options = {};
  for (const key of ['server-image', 'generator-image', 'output']) {
    if (!raw[key]) throw new Error(`Required: --${key}`);
    options[key.replace(/-([a-z])/g, (_, l) => l.toUpperCase())] = key === 'output' ? resolve(raw[key]) : raw[key];
  }
  options.netemImage = raw['netem-image'] ?? 'localhost/simplestchat-netem:dev';
  options.subscriptionPlan = raw['subscription-plan'] ?? 'fifo';
  if (options.subscriptionPlan !== 'fifo' && !plannedSubscriptionModes.has(options.subscriptionPlan)) throw new Error('--subscription-plan must be fifo, ring-v1 or hotspot-v1');
  options.subscriptionSeed = null;
  if (plannedSubscriptionModes.has(options.subscriptionPlan)) {
    if (!/^(0|[1-9][0-9]*)$/.test(raw['subscription-seed'] ?? '') || Number(raw['subscription-seed']) > 0xffffffff) throw new Error('--subscription-seed is required for ring-v1 and hotspot-v1 and must be an unsigned 32-bit decimal integer');
    options.subscriptionSeed = Number(raw['subscription-seed']);
  } else if (raw['subscription-seed'] !== undefined) throw new Error('--subscription-seed requires --subscription-plan ring-v1 or hotspot-v1');
  for (const [key, fallback, minimum, maximum] of [['duration', 60, 3, 600], ['warmup', 10, 2, 60], ['ramp-up', 5, 1, 1200], ['repetitions', 1, 1, 5], ['workers', 2, 1, 4], ['port', 3129, 1024, 65535], ['udp-port', 41100, 1024, 65531], ['max-connections', 1000, 2, 10000], ['source-addresses', 1, 1, 60000]]) {
    const value = Number(raw[key] ?? fallback);
    if (!Number.isInteger(value) || value < minimum || value > maximum) throw new Error(`--${key} must be ${minimum}–${maximum}`);
    options[key.replace(/-([a-z])/g, (_, l) => l.toUpperCase())] = value;
  }
  for (const [key, fallback, minimum, maximum] of [['cpus', 2, 0.5, 16], ['generator-cpus', 5, 0.5, 32]]) {
    const value = Number(raw[key] ?? fallback);
    if (!Number.isFinite(value) || value < minimum || value > maximum) throw new Error(`--${key} must be ${minimum}–${maximum}`);
    options[key.replace(/-([a-z])/g, (_, l) => l.toUpperCase())] = value;
  }
  options.memory = raw.memory ?? '2g';
  if (!/^[1-9][0-9]*[mg]$/.test(options.memory)) throw new Error('--memory must be like 512m or 2g');
  options.clients = (raw.clients ?? '100').split(',').map(Number);
  if (!options.clients.length || options.clients.some(v => !Number.isInteger(v) || v < 2 || v > 2000) || new Set(options.clients).size !== options.clients.length) throw new Error('--clients must be unique counts between 2 and 2000');
  const scenarios = (raw.scenarios ?? 'multi-room').split(',');
  if (scenarios.some(s => !['conference', 'multi-room', 'audio', 'webinar'].includes(s)) || new Set(scenarios).size !== scenarios.length) throw new Error('Unknown/duplicate scenario');
  // webinar: one room, exactly one publisher (ceil(clients / 1000)), every
  // other client a viewer, each viewer joining from its own loopback address
  // as distinct viewers would, so the per-address join limits do not shape
  // the ramp of a large one-to-many room.
  // --source-addresses N spreads every other scenario's clients over N loopback
  // addresses too (default 1: the shared address, as every earlier record).
  const WEBINAR_SOURCE_ADDRESSES = 250;
  options.scenarios = options.clients.flatMap(clients => scenarios.map(name => {
    const sourceAddresses = name === 'webinar' ? WEBINAR_SOURCE_ADDRESSES : options.sourceAddresses;
    const spread = sourceAddresses > 1 ? ['--source-addresses', String(sourceAddresses)] : [];
    return { name: `${name}-${clients}`, clients,
      rooms: name === 'multi-room' ? Math.min(4, Math.floor(clients / 2)) : 1, mode: name === 'webinar' ? 'webinar' : 'conference',
      sourceAddresses,
      extra: name === 'audio' ? ['--audio-only', ...spread] : name === 'webinar' ? ['--publish-ratio', '0.001', ...spread] : spread };
  }));
  for (const scenario of options.scenarios) {
    // The server's code-level join limits (30 per IP and 10 per room and IP
    // per minute) apply per loopback source address exactly as locally.
    const perAddress = Math.ceil(scenario.clients / scenario.sourceAddresses);
    const spacing = Math.max(perAddress > 30 ? 60.5 / (30 * scenario.sourceAddresses) : 0, Math.ceil(perAddress / scenario.rooms) > 10 ? 60.5 / (10 * scenario.rooms * scenario.sourceAddresses) : 0);
    const minimumRamp = Math.ceil(spacing * scenario.clients);
    if (options.rampUp < minimumRamp) throw new Error(`${scenario.name} exceeds loopback join admission limits; use --ramp-up ${minimumRamp}`);
  }
  options.netem = raw.netem ?? null;
  options.netemScope = raw['netem-scope'] ?? 'udp-both';
  if (options.netem) netemScript(options.netem, options.netemScope, options.udpPort, options.workers);
  else if (raw['netem-scope']) throw new Error('--netem-scope requires --netem');
  options.label = raw.label ?? null;
  options.sourceRevision = raw['source-revision'] ?? null;
  return options;
}

async function metrics(url, token) {
  const response = await fetch(`${url}/metrics`, { headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(2000) });
  if (!response.ok) throw new Error(`Metrics HTTP ${response.status}`);
  const raw = await response.text();
  const values = Object.fromEntries(raw.split('\n').filter(l => /^simplestchat_\w+ \d/.test(l)).map(l => { const [key, value] = l.split(' '); return [key, Number(value)]; }));
  return { raw, values };
}

async function imageIdentity(image) {
  const [id, digest] = (await podman(['image', 'inspect', '--format', '{{.Id}} {{.Digest}}', image])).trim().split(' ');
  return { image, id, digest };
}

async function containerState(name) {
  try {
    const [status, code] = (await podman(['inspect', '--format', '{{.State.Status}} {{.State.ExitCode}}', name])).trim().split(' ');
    return { status, code: Number(code) };
  } catch { return null; }
}

async function execIn(name, script) {
  return podman(['exec', name, 'sh', '-c', script], { timeout: 10000 });
}

const SERVER_SAMPLER = 'while :; do cat /proc/1/stat; echo @@S; cat /sys/fs/cgroup/cpu.stat; echo @@Y; cat /proc/1/task/*/stat; echo @@E; sleep 0.5; done';
const GENERATOR_SAMPLER = 'while :; do cat /proc/1/stat; echo @@E; sleep 0.5; done';

/**
 * One long-lived exec session per container streams samples every 500 ms;
 * per-sample exec calls cost about a second each through the VM and left
 * the measurement window under-covered.
 */
function startSampler(container, script, onBlock) {
  const child = spawn('podman', ['exec', container, 'sh', '-c', script], { stdio: ['ignore', 'pipe', 'ignore'] });
  let buffer = '';
  child.stdout.on('data', chunk => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('@@E\n')) >= 0) {
      const block = buffer.slice(0, index);
      buffer = buffer.slice(index + 4);
      onBlock(block);
    }
  });
  child.on('error', () => {});
  return { stop: () => { try { child.kill('SIGTERM'); } catch {} } };
}

/** Parse one server sampler block into process, cgroup and thread readings. */
export function parseServerBlock(block, clockTicks, pageSize) {
  const [stat, cpuStat, tasks] = block.split(/@@[SY]\n/);
  return { server: linuxProcessSample(stat, clockTicks, pageSize), serverDetail: { cgroup: cgroupCpuStat(cpuStat), threads: threadTicks(tasks) } };
}

async function stopContainer(name, graceSeconds) {
  if (!containers.has(name)) return null;
  try { await podman(['stop', '-t', String(graceSeconds), name], { timeout: (graceSeconds + 30) * 1000 }); } catch {}
  return containerState(name);
}

async function removeContainer(name) {
  if (!containers.has(name)) return;
  try { await podman(['rm', '-f', name]); } catch {}
  containers.delete(name);
}

async function runOne(options, scenario, repetition, manifest) {
  const name = `${scenario.name}-${repetition}${options.netem ? '-netem' : ''}`;
  const directory = join(options.output, name); await mkdir(directory, { mode: 0o700 });
  const id = randomBytes(4).toString('hex');
  const sfu = `bench-sfu-${id}`, gen = `bench-gen-${id}`;
  const origin = `http://127.0.0.1:${options.port}`;
  const token = randomBytes(32).toString('hex');
  const env = { BIND_ADDR: '0.0.0.0', PORT: String(options.port), ANNOUNCE_IP: '127.0.0.1',
    MEDIA_WORKERS: String(options.workers), WEBRTC_SERVER_PORT_BASE: String(options.udpPort),
    ALLOW_AD_HOC_ROOMS: 'true', ALLOWED_ORIGINS: origin, REGISTRATION_ENABLED: 'false',
    MAX_CONNECTIONS: String(options.maxConnections), MAX_CONNECTIONS_PER_IP: String(Math.min(options.maxConnections, scenario.clients + 16)),
    WS_HANDSHAKES_PER_MINUTE: '600', METRICS_TOKEN: token, RUST_LOG: 'simplestChat=info,mediasoup=warn' };
  const serverSamples = [], generatorSamples = [];
  const startedAt = new Date().toISOString();
  let generatorStarted, generatorExit, serverExit, row, primaryError, clockTicks = 100, serverSampler, generatorSampler;
  try {
    await podman(['run', '-d', '--name', sfu, '--cpus', String(options.cpus), '--memory', options.memory, '--pids-limit', '512',
      '-p', `127.0.0.1:${options.port}:${options.port}/tcp`, ...Object.entries(env).flatMap(([k, v]) => ['-e', `${k}=${v}`]), options.serverImage]);
    containers.add(sfu);
    clockTicks = Number((await execIn(sfu, 'getconf CLK_TCK')).trim()) || 100;
    const pageSize = Number((await execIn(sfu, 'getconf PAGESIZE')).trim()) || 4096;
    let ready = false;
    for (let i = 0; i < 200 && !ready; i++) {
      try { const response = await fetch(`${origin}/health`, { signal: AbortSignal.timeout(500) }); ready = response.ok; } catch {}
      if (!ready) await delay(250);
    }
    if (!ready) throw new Error('Owned server container did not become ready');
    if (options.netem) {
      const script = netemScript(options.netem, options.netemScope, options.udpPort, options.workers);
      const shown = await podman(['run', '--rm', '--network', `container:${sfu}`, '--cap-add', 'NET_ADMIN', options.netemImage, 'sh', '-c', script]);
      await writeFile(join(directory, 'netem.txt'), `${script}\n---\n${shown}`, { flag: 'wx' });
    }
    const before = await metrics(origin, token); await writeFile(join(directory, 'metrics-before.txt'), before.raw, { flag: 'wx' });
    const args = ['--server', `ws://127.0.0.1:${options.port}/ws`, '--clients', String(scenario.clients),
      '--rooms', String(scenario.rooms), '--room', `benchmark-${randomBytes(6).toString('hex')}`,
      '--duration', String(options.duration), '--ramp-up', String(options.rampUp), '--warmup', String(options.warmup),
      '--mode', scenario.mode, '--quality', '480p', '--fps', '30', '--max-audio', '4', '--max-video', '4',
      '--output-dir', '/results', '--run-label', name, '--server-revision', manifest.server.id.slice(0, 16),
      '--generator-revision', manifest.generator.id.slice(0, 16), ...scenario.extra, ...subscriptionArguments(options)];
    await json(join(directory, 'invocation.json'), { startedAt, scenario, repetition, args, netem: options.netem, netemScope: options.netem ? options.netemScope : null,
      serverConfiguration: Object.fromEntries(Object.entries(env).filter(([k]) => k !== 'METRICS_TOKEN')),
      limits: { cpus: options.cpus, memory: options.memory, generatorCpus: options.generatorCpus } });
    const start = performance.now();
    serverSampler = startSampler(sfu, SERVER_SAMPLER, block => {
      const elapsedMs = performance.now() - start;
      try { serverSamples.push({ elapsedMs, ...parseServerBlock(block, clockTicks, pageSize) }); }
      catch { serverSamples.push({ elapsedMs, server: null, serverDetail: null }); }
    });
    await podman(['run', '-d', '--name', gen, '--network', `container:${sfu}`, '--cpus', String(options.generatorCpus), '-e', 'RUST_LOG=warn', options.generatorImage, ...args]);
    containers.add(gen);
    generatorStarted = true;
    generatorSampler = startSampler(gen, GENERATOR_SAMPLER, block => {
      const elapsedMs = performance.now() - start;
      try { generatorSamples.push({ elapsedMs, generator: linuxProcessSample(block, clockTicks, pageSize) }); }
      catch { generatorSamples.push({ elapsedMs, generator: null }); }
    });
    const deadline = (options.rampUp + options.warmup + options.duration + 135) * 1000;
    for (;;) {
      const [generatorState, serverState] = await Promise.all([containerState(gen), containerState(sfu)]);
      const elapsedMs = performance.now() - start;
      if (!generatorState || generatorState.status !== 'running') { generatorExit = generatorState; break; }
      if (!serverState || serverState.status !== 'running') throw new Error('Server container exited during load');
      if (elapsedMs > deadline) throw new Error('Outer generator deadline exceeded');
      await delay(1000);
    }
    generatorSampler.stop();
    await podman(['cp', `${gen}:/results/.`, directory]);
    if (generatorExit?.code !== 0) throw new Error(`Generator failed: ${JSON.stringify(generatorExit)}`);
    const summary = JSON.parse(await readFile(join(directory, 'load_test_summary.json'), 'utf8'));
    if (summary.schemaVersion !== 2 || !summary.run?.completed || !summary.run?.passed || summary.totalErrors || summary.failedConnections || summary.failedConsumers) throw new Error('Incomplete or failing generator report');
    const results = plannedSubscriptionModes.has(options.subscriptionPlan) ? JSON.parse(await readFile(join(directory, 'load_test_results.json'), 'utf8')) : null;
    const subscription = verifySubscriptionReport(summary, results, options, scenario);
    const finish = await metrics(origin, token); await writeFile(join(directory, 'metrics-finish.txt'), finish.raw, { flag: 'wx' });
    let cleaned = false, final;
    const cleanupStart = performance.now();
    for (let i = 0; i < 80 && !cleaned; i++) {
      final = await metrics(origin, token);
      cleaned = ['connections', 'rooms', 'participants'].every(k => final.values[`simplestchat_${k}_active`] === 0);
      if (!cleaned) await delay(500);
    }
    if (!cleaned) throw new Error('Server room/session cleanup did not complete within 40 seconds');
    await writeFile(join(directory, 'metrics-cleanup.txt'), final.raw, { flag: 'wx' });
    const windowStart = (options.rampUp + options.warmup) * 1000;
    const windowEnd = windowStart + options.duration * 1000;
    const serverResources = resourceSummary(serverSamples, 'server', windowStart, windowEnd);
    const generatorResources = resourceSummary(generatorSamples, 'generator', windowStart, windowEnd);
    const attribution = serverAttribution(serverSamples, windowStart, windowEnd, clockTicks, options.cpus);
    row = { purpose: 'production-shape', scenario: scenario.name, variant: 'candidate', repetition, startedAt, netem: options.netem, netemScope: options.netem ? options.netemScope : null,
      workloadPassed: true, ...subscription,
      joinP99Ms: summary.p99ConnectionTimeMs, sendReadyP99Ms: summary.sendMediaReady.p99Ms, receiveReadyP99Ms: summary.receiveMediaReady.p99Ms,
      receivedPacketsPerSecond: summary.measurement.packetsReceived / (summary.measurement.durationMs / 1000),
      keyframesGenerated: summary.keyframesGenerated, keyframesRequested: summary.keyframesRequested,
      bandwidthEstimates: summary.bandwidthEstimates, clientsWithBandwidthEstimate: summary.clientsWithBandwidthEstimate,
      serverCpuPercent: serverResources.cpuPercentOfOneCore, serverPeakRssMiB: serverResources.peakRssMiB,
      serverCpuPercentOfQuota: attribution.cgroup.cpuPercentOfQuota, serverThrottledPeriodFraction: attribution.cgroup.throttledPeriodFraction,
      serverThrottledSeconds: attribution.cgroup.throttledSeconds, serverThreadCpuSeconds: attribution.threadCpuSeconds,
      generatorCpuPercent: generatorResources.cpuPercentOfOneCore, generatorPeakRssMiB: generatorResources.peakRssMiB,
      cleanupMs: performance.now() - cleanupStart, serverResources, generatorResources, serverAttribution: attribution,
      media: { validatedConsumers: summary.validatedConsumers, failedConsumers: summary.failedConsumers, skippedShortLivedConsumers: summary.skippedShortLivedConsumers },
      serverMetricsAtFinish: Object.fromEntries(Object.entries(finish.values).filter(([k]) => /rejected|deaths|queue_full|queue_closed|errors_total|consumers_created/.test(k))) };
    return row;
  } catch (error) {
    primaryError = error;
    await runFinalizers(primaryError, [() => json(join(directory, 'failure.json'), { startedAt, error: error.stack, generatorExit, generatorStarted })]);
    throw error;
  } finally {
    await runFinalizers(primaryError, [() => { serverSampler?.stop(); generatorSampler?.stop(); }, async () => {
      if (generatorStarted && !generatorExit) {
        await stopContainer(gen, 5);
        try { await podman(['cp', `${gen}:/results/.`, directory]); } catch {}
      }
    }, async () => {
      try { await writeFile(join(directory, 'generator.log'), await podman(['logs', gen]), { flag: 'wx' }); } catch {}
    }, async () => {
      serverExit = await stopContainer(sfu, 20);
      if (row) row.serverExit = serverExit && serverExit.status === 'exited' ? { code: serverExit.code, signal: null } : { code: null, signal: null, error: JSON.stringify(serverExit) };
    }, async () => {
      try { await writeFile(join(directory, 'server.log'), await podman(['logs', sfu]), { flag: 'wx' }); } catch {}
    }, () => json(join(directory, 'server-exit.json'), serverExit ?? null),
    () => json(join(directory, 'generator-exit.json'), generatorExit ?? null),
    () => json(join(directory, 'resources.json'), { server: serverSamples, generator: generatorSamples }),
    async () => {
      if (row) {
        Object.assign(row, performanceRunStatus([row]), { completedAt: new Date().toISOString() });
        await json(join(directory, 'result.json'), row);
        console.log(`${row.passed ? 'PASS' : 'FAIL'} ${name}: recvP99=${row.receiveReadyP99Ms}ms serverCPU=${row.serverCpuPercent.toFixed(1)}% (${row.serverCpuPercentOfQuota.toFixed(1)}% of quota, throttled ${(100 * (row.serverThrottledPeriodFraction ?? 0)).toFixed(1)}% of periods) RSS=${row.serverPeakRssMiB.toFixed(1)}MiB; server shutdown ${row.serverShutdownPassed ? 'clean' : 'FAILED'}`);
      }
    }, () => removeContainer(gen), () => removeContainer(sfu)]);
  }
}

async function main() {
  const options = parseOptions(process.argv.slice(2));
  await mkdir(options.output, { recursive: false, mode: 0o700 });
  const info = JSON.parse(await podman(['info', '--format', 'json']));
  const manifest = { schemaVersion: 1, startedAt: new Date().toISOString(), options, label: options.label, sourceRevision: options.sourceRevision,
    environment: { orchestratorPlatform: os.platform(), orchestratorArch: os.arch(), orchestratorCpu: os.cpus()[0]?.model, node: process.version,
      vm: { kernel: info.host?.kernel, arch: info.host?.arch, cpus: info.host?.cpus, memoryBytes: info.host?.memTotal, cgroupVersion: info.host?.cgroupVersion, podman: info.version?.Version } },
    server: await imageIdentity(options.serverImage), generator: await imageIdentity(options.generatorImage),
    orchestratorSha256: hash(await readFile(new URL(import.meta.url))),
    limitations: ['Co-located server and generator inside one Linux VM on Apple silicon: the CPU quota is the production shape, the cores are not the production cores.',
      'Loopback media; impairment applies only when --netem is set and then only to UDP on loopback.',
      'Per-thread CPU uses first/last thread sums, so threads that exit inside the window are excluded; cgroup usage and throttling are exact.',
      'Synthetic RTP without simulcast: forwarding cost and delivery, not browser quality or layer selection.'] };
  await json(join(options.output, 'manifest.json'), manifest);
  const rows = [];
  try {
    for (const scenario of options.scenarios) for (let rep = 1; rep <= options.repetitions; rep++) rows.push(await runOne(options, scenario, rep, manifest));
    const status = performanceRunStatus(rows);
    const capacity = {};
    for (const scenario of new Set(rows.map(r => r.scenario))) {
      const of = field => rows.filter(r => r.scenario === scenario).map(r => r[field]);
      capacity[scenario] = Object.fromEntries(['receiveReadyP99Ms', 'sendReadyP99Ms', 'receivedPacketsPerSecond', 'serverCpuPercentOfQuota', 'serverThrottledPeriodFraction', 'serverPeakRssMiB', 'generatorCpuPercent']
        .map(field => [field, { median: median(of(field).map(v => v ?? 0)), range: [Math.min(...of(field).map(v => v ?? 0)), Math.max(...of(field).map(v => v ?? 0))] }]));
    }
    await json(join(options.output, 'comparison.json'), { purpose: 'production-shape', completed: true, ...status, rows, capacity });
    if (!status.passed) { console.error('Run did not pass workload or server shutdown gates; see per-run reports.'); process.exitCode = 1; }
  } catch (error) {
    await json(join(options.output, 'comparison.json'), { purpose: 'production-shape', completed: false, passed: false, rows, error: error.stack });
    throw error;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, async () => { for (const name of [...containers]) { await stopContainer(name, 5); await removeContainer(name); } process.exit(signal === 'SIGINT' ? 130 : 143); });
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
