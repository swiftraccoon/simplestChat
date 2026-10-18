#!/usr/bin/env node
// A fixed hosted experiment, never a public origin or a laptop media workload.
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { chmod, mkdir, mkdtemp, open, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { linuxProcessSample, resourceSummary } from '../load_tests/benchmark-local.mjs';

const exec = promisify(execFile);
const CLIENTS = 30;
const DURATION = 1200;
const RAMP = 60;
const WARMUP = 30;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const sha256 = value => createHash('sha256').update(value).digest('hex');
const requireValue = (condition, code) => { if (!condition) throw new Error(code); };

export function configuration(env, platform = process.platform) {
  requireValue(platform === 'linux' && env.GITHUB_ACTIONS === 'true' && env.RUNNER_OS === 'Linux', 'hosted_linux_only');
  requireValue(env.ACCOUNT_ROOM_E2E === '1' && env.DISPOSABLE_TEST_DATABASE === '1', 'explicit_disposable_opt_in_required');
  const base = new URL(env.BASE_URL ?? 'http://127.0.0.1:3119');
  requireValue(base.protocol === 'http:' && base.hostname === '127.0.0.1' && base.port !== '' &&
    !base.username && !base.password && base.pathname === '/' && !base.search && !base.hash, 'owned_loopback_origin_required');
  const database = new URL(env.DATABASE_URL ?? '');
  requireValue(['postgres:', 'postgresql:'].includes(database.protocol) && ['127.0.0.1', 'localhost', '[::1]'].includes(database.hostname) &&
    /^\/[A-Za-z0-9_]+_test$/.test(database.pathname) && (!database.search || database.search === '?sslmode=disable') && !database.hash,
  'disposable_loopback_database_required');
  const serverPid = Number(env.TEST_SERVER_PID);
  requireValue(Number.isSafeInteger(serverPid) && serverPid > 1, 'owned_server_pid_required');
  requireValue(/^[0-9a-f]{64}$/.test(env.TEST_METRICS_TOKEN ?? ''), 'owned_metrics_token_required');
  requireValue(/^[0-9a-f]{40}$/.test(env.SERVER_REVISION ?? ''), 'exact_source_revision_required');
  requireValue(env.E2E_ARTIFACTS?.startsWith('/') && env.RUNNER_TEMP?.startsWith('/'), 'private_absolute_directories_required');
  requireValue(env.TEST_GENERATOR_BINARY?.startsWith('/'), 'explicit_generator_binary_required');
  return { base: base.origin, database: database.href, serverPid, metricsToken: env.TEST_METRICS_TOKEN,
    revision: env.SERVER_REVISION, output: resolve(env.E2E_ARTIFACTS), temporaryRoot: resolve(env.RUNNER_TEMP),
    generator: resolve(env.TEST_GENERATOR_BINARY) };
}

export function sourceAddress(index) {
  requireValue(Number.isInteger(index) && index >= 0 && index < CLIENTS, 'invalid_cohort_index');
  return `127.0.1.${index + 2}`;
}

export function generatorArguments(config, credentials, room) {
  requireValue(/^account-room-[0-9a-f-]{36}$/.test(room), 'invalid_owned_room');
  return ['--server', `${config.base.replace('http:', 'ws:')}/ws`, '--room', room,
    '--clients', String(CLIENTS), '--rooms', '1', '--source-addresses', String(CLIENTS),
    '--ramp-up', String(RAMP), '--warmup', String(WARMUP), '--duration', String(DURATION),
    '--profile', 'browser', '--receive-mode', 'balanced', '--chat-interval-ms', '30000',
    '--diagnostics', '--departure', 'explicit-leave', '--accounts-file', credentials, '--output-dir', join(config.output, 'generator'),
    '--run-label', 'account-room', '--server-revision', `git:${config.revision}`, '--generator-revision', `git:${config.revision}`];
}

export function parseMetrics(raw) {
  const values = {};
  for (const line of raw.split('\n')) {
    if (!line || line.startsWith('#')) continue;
    const match = /^(simplestchat_[a-zA-Z0-9_]+(?:\{[^\n]*\})?) ([0-9.eE+-]+)$/.exec(line);
    if (match && Number.isFinite(Number(match[2]))) values[match[1]] = Number(match[2]);
  }
  requireValue(Number.isFinite(values.simplestchat_connections_active), 'incomplete_server_metrics');
  return values;
}

export function parseHost(stat, memory) {
  const cpu = stat.split('\n').find(line => line.startsWith('cpu '))?.trim().split(/\s+/).slice(1).map(Number);
  requireValue(cpu?.length >= 8 && cpu.every(Number.isFinite), 'invalid_host_cpu_sample');
  const total = cpu.slice(0, 8).reduce((a, b) => a + b, 0);
  const available = Number(/^MemAvailable:\s+(\d+) kB$/m.exec(memory)?.[1]) * 1024;
  requireValue(Number.isFinite(available) && available > 0, 'invalid_host_memory_sample');
  return { total, idle: cpu[3] + cpu[4], steal: cpu[7], availableBytes: available };
}

export function parseDatabaseProbe(raw) {
  const lines = raw.trim().split('\n');
  const elapsed = /^Time: ([0-9.]+) ms(?: .*)?$/.exec(lines.at(-1));
  requireValue(elapsed && Number.isFinite(Number(elapsed[1])), 'missing_database_query_timing');
  const counters = JSON.parse(lines.slice(0, -1).join('\n'));
  requireValue(['connections', 'active', 'waiting'].every(key => Number.isSafeInteger(counters[key]) && counters[key] >= 0), 'invalid_database_probe');
  return { ...counters, queryRoundTripMs: Number(elapsed[1]) };
}

export function retentionVerdict(rows, inventory, accountIds) {
  requireValue(Array.isArray(rows) && Array.isArray(inventory) && inventory.length > 0, 'retained_inventory_missing');
  requireValue(rows.length === inventory.length, 'retained_message_count_mismatch');
  const expected = new Map(inventory.map(row => [row.messageId, row]));
  requireValue(expected.size === inventory.length, 'duplicate_expected_message_id');
  const allowed = new Set(accountIds);
  const seen = new Set();
  for (const row of rows) {
    const original = expected.get(row.messageId);
    requireValue(original && !seen.has(row.messageId) && allowed.has(row.senderAccountId), 'unexpected_retained_message');
    requireValue(row.clientMessageId === original.clientMessageId && row.senderAccountId === original.senderAccountId && sha256(row.content) === original.contentSha256,
      'retained_message_content_mismatch');
    seen.add(row.messageId);
  }
  requireValue(new Set(rows.map(row => row.senderAccountId)).size === CLIENTS, 'incomplete_retained_sender_cohort');
  return { passed: true, messages: rows.length, accounts: CLIENTS };
}

export function authenticationVerdict(authentication, accountIds) {
  requireValue(Array.isArray(authentication) && authentication.length === CLIENTS, 'incomplete_authentication_cohort');
  const expected = new Set(accountIds);
  requireValue(expected.size === CLIENTS && new Set(authentication.map(row => row.accountId)).size === CLIENTS, 'duplicate_authentication_account');
  for (const row of authentication)
    requireValue(expected.has(row.accountId) && row.passed === true && row.originalExpiryCrossed === true && row.httpRefreshes >= 1 && row.socketRenewals >= 1 &&
      Number.isFinite(row.originalExpiresAt) && row.latestExpiresAt > row.originalExpiresAt && row.failures === 0, 'authentication_refresh_or_expiry_failed');
  return { passed: true, accounts: CLIENTS, httpRefreshes: authentication.reduce((sum, row) => sum + row.httpRefreshes, 0),
    socketRenewals: authentication.reduce((sum, row) => sum + row.socketRenewals, 0), originalExpiryCrossed: CLIENTS };
}

/** A complete failing run is evidence; it must not be confused with setup failure. */
export function generatorVerdict(summary, exit) {
  const config = summary.run?.configuration;
  const completed = summary.run?.completed === true;
  const fullWindow = completed && summary.measurement?.durationMs === DURATION * 1000 && config?.durationSecs === DURATION &&
    config.rampUpSecs === RAMP && config.warmupSecs === WARMUP;
  const passed = fullWindow && exit?.code === 0 && exit.signal === null && summary.run.passed === true &&
    summary.totalClients === CLIENTS && summary.successfulConnections === CLIENTS && summary.failedConnections === 0 &&
    summary.failedConsumers === 0 && summary.validatedConsumers === CLIENTS * (CLIENTS - 1 + 9) &&
    config.profile?.name === 'browser' && config.maxVideoConsumers === 9 && config.maxAudioConsumers >= CLIENTS - 1;
  return { completed, fullWindow, passed, failure: passed ? null : 'generator_workload_failed',
    failureReasons: (summary.run?.failureReasons ?? []).slice(0, 100).map(value => String(value).slice(0, 400)),
    totalClients: summary.totalClients, failedConnections: summary.failedConnections, failedConsumers: summary.failedConsumers,
    validatedConsumers: summary.validatedConsumers, chat: summary.chat, signalingLatencies: summary.signalingLatencies };
}

async function assess(work) {
  try { return await work(); }
  catch (error) { return { passed: false, failure: /^[a-z0-9_]+$/.test(error.message ?? '') ? error.message : 'evidence_validation_failed' }; }
}

export function parseCgroup(stat, maximum) {
  const counters = Object.fromEntries(stat.trim().split('\n').map(line => line.trim().split(/\s+/)).map(([key, value]) => [key, Number(value)]));
  const [quota, period] = maximum.trim().split(/\s+/);
  requireValue(Number.isFinite(counters.usage_usec) && Number.isFinite(counters.nr_periods) && Number.isFinite(counters.nr_throttled) &&
    Number(period) > 0 && (quota === 'max' || Number(quota) > 0), 'invalid_cgroup_cpu_sample');
  return { periods: counters.nr_periods, throttledPeriods: counters.nr_throttled,
    quotaCores: quota === 'max' ? null : Number(quota) / Number(period) };
}

/** Linux process CPU/RSS excludes child processes; follow every owned thread's children. */
export async function serverProcessTree(rootPid, units, io = { readFile, readdir }) {
  const pending = [rootPid], observed = new Set(), processes = [];
  while (pending.length) {
    const pid = pending.pop();
    if (observed.has(pid)) continue;
    requireValue(Number.isSafeInteger(pid) && pid > 1 && observed.size < 128, 'invalid_server_process_tree');
    observed.add(pid);
    const stat = await io.readFile(`/proc/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(')') + 2).trim().split(/\s+/);
    const startTicks = Number(fields[19]);
    requireValue(Number.isSafeInteger(startTicks) && startTicks >= 0, 'invalid_server_process_identity');
    processes.push({ pid, startTicks, ...linuxProcessSample(stat, ...units) });
    const tasks = await io.readdir(`/proc/${pid}/task`);
    requireValue(tasks.length > 0 && tasks.length <= 512, 'invalid_server_thread_inventory');
    for (const task of tasks) {
      requireValue(/^[1-9][0-9]*$/.test(task), 'invalid_server_thread_identity');
      const children = (await io.readFile(`/proc/${pid}/task/${task}/children`, 'utf8')).trim();
      for (const child of children ? children.split(/\s+/) : []) pending.push(Number(child));
    }
  }
  processes.sort((left, right) => left.pid - right.pid);
  return { cpuSeconds: processes.reduce((sum, process) => sum + process.cpuSeconds, 0),
    rssKiB: processes.reduce((sum, process) => sum + process.rssKiB, 0),
    processes: processes.map(({ pid, startTicks }) => ({ pid, startTicks })), childCount: processes.length - 1 };
}

export function measurementResources(samples, start, end) {
  const window = samples.filter(s => s.elapsedMs >= start && s.elapsedMs <= end);
  const identities = new Set(window.filter(s => s.server).map(s => JSON.stringify(s.server.processes)));
  requireValue(identities.size === 1 && window.some(s => s.server?.processes?.length > 1), 'unstable_or_missing_native_worker_process_tree');
  const server = resourceSummary(samples, 'server', start, end);
  const generator = resourceSummary(samples, 'generator', start, end);
  server.processes = window.find(s => s.server).server.processes;
  server.childCount = window.find(s => s.server).server.childCount;
  server.scope = 'Rust main process and all owned descendants; RSS sums process resident pages including shared mappings';
  const hosts = window.filter(s => s.host);
  requireValue(hosts.length >= 2 && hosts.length >= window.length * 0.9, 'incomplete_host_resource_coverage');
  const first = hosts[0].host, last = hosts.at(-1).host;
  const total = last.total - first.total;
  requireValue(total > 0, 'invalid_host_cpu_window');
  const busyShare = 1 - (last.idle - first.idle) / total;
  const stealShare = (last.steal - first.steal) / total;
  requireValue(busyShare >= 0 && busyShare <= 1 && stealShare >= 0 && stealShare <= 1, 'invalid_host_cpu_counter_delta');
  const scrapes = window.filter(s => s.metricsExpected);
  const healthy = scrapes.filter(s => s.metrics && s.database);
  requireValue(scrapes.length >= DURATION / 6 && healthy.length >= scrapes.length * 0.9, 'incomplete_database_metrics_coverage');
  requireValue(window.every((row, index) => !index || row.elapsedMs - window[index - 1].elapsedMs <= 1500), 'resource_sampling_stalled');
  const cgroups = window.filter(s => s.cgroup);
  requireValue(cgroups.length >= window.length * 0.9, 'missing_cgroup_throttling_evidence');
  const periods = cgroups.at(-1).cgroup.periods - cgroups[0].cgroup.periods;
  const throttled = cgroups.at(-1).cgroup.throttledPeriods - cgroups[0].cgroup.throttledPeriods;
  requireValue(periods >= 0 && throttled >= 0 && throttled <= periods, 'invalid_cgroup_throttling_window');
  const throttledShare = periods === 0 ? 0 : throttled / periods;
  const peakWorkerCores = Math.max(...healthy.flatMap(s => Object.entries(s.metrics)
    .filter(([key]) => key.startsWith('simplestchat_media_worker_cpu{')).map(([, value]) => value)));
  requireValue(Number.isFinite(peakWorkerCores), 'missing_worker_cpu_evidence');
  const databaseErrors = healthy.some(s => Object.entries(s.metrics).some(([key, value]) => key.startsWith('simplestchat_database_errors_total{') && value !== 0));
  const saturated = healthy.some(s => s.metrics.simplestchat_cpu_saturated !== 0 || s.metrics.simplestchat_memory_saturated !== 0);
  const sorted = healthy.map(s => s.database.queryRoundTripMs).sort((a, b) => a - b);
  requireValue(healthy.every(s => s.metrics['simplestchat_db_pool_connections{state="max"}'] > 0 &&
    Number.isFinite(s.metrics['simplestchat_db_pool_connections{state="used"}'])), 'missing_application_pool_evidence');
  const membership = healthy.filter(s => s.metrics.simplestchat_participants_snapshot_complete === 1);
  requireValue(membership.length >= healthy.length * 0.9, 'incomplete_live_membership_evidence');
  const completeCohort = membership.every(s => s.metrics.simplestchat_connections_active === CLIENTS &&
    s.metrics.simplestchat_participants_active === CLIENTS && s.metrics.simplestchat_rooms_active === 1);
  return { passed: completeCohort && busyShare <= 0.85 && stealShare <= 0.05 && throttledShare <= 0.05 && peakWorkerCores <= 0.7 && !databaseErrors && !saturated,
    server, generator, host: { busyShare, stealShare, minimumAvailableBytes: Math.min(...hosts.map(s => s.host.availableBytes)) },
    cgroup: { throttledShare, quotaCores: cgroups[0].cgroup.quotaCores, scope: 'shared runner cgroup, not separate per-process quotas' },
    peakWorkerCores, databaseErrors, saturated, completeCohort, membershipSamples: membership.length, metricsSamples: healthy.length,
    database: { probe: 'psql query round trip after connection; separate from application pool waits',
      p50Ms: sorted[Math.ceil(sorted.length * 0.5) - 1], p99Ms: sorted[Math.ceil(sorted.length * 0.99) - 1],
      peakPoolUsed: Math.max(...healthy.map(s => s.metrics['simplestchat_db_pool_connections{state="used"}'] ?? 0)),
      maxPoolConnections: Math.max(...healthy.map(s => s.metrics['simplestchat_db_pool_connections{state="max"}'] ?? 0)),
      peakWaitingSessions: Math.max(...healthy.map(s => s.database.waiting)) } };
}

async function request(config, index, path, body, token) {
  const raw = body === undefined ? undefined : JSON.stringify(body);
  return new Promise((resolveRequest, reject) => {
    const req = http.request(`${config.base}${path}`, { method: 'POST', localAddress: sourceAddress(index),
      headers: { Origin: config.base, 'Content-Type': 'application/json', ...(token ? { Authorization: `Bearer ${token}` } : {}) } }, response => {
      const chunks = [];
      let bytes = 0;
      response.on('data', chunk => {
        bytes += chunk.length;
        if (bytes > 65536) req.destroy(new Error('setup_response_too_large'));
        else chunks.push(chunk);
      });
      response.on('end', () => {
        if (response.statusCode !== 200) { reject(new Error(`setup_http_${response.statusCode}`)); return; }
        try { resolveRequest(JSON.parse(Buffer.concat(chunks).toString('utf8'))); }
        catch { reject(new Error('invalid_setup_response')); }
      });
      response.on('error', () => reject(new Error('setup_response_failed')));
    });
    req.setTimeout(15000, () => req.destroy(new Error('setup_request_timeout')));
    req.on('error', () => reject(new Error('setup_request_failed')));
    req.end(raw);
  });
}

async function databaseQuery(config, sql, timing = false) {
  const args = ['-X', '-qAt', '--set=ON_ERROR_STOP=1'];
  if (timing) args.push('--command', '\\timing on');
  args.push('--command', sql);
  const { stdout } = await exec('psql', args, { timeout: 5000, maxBuffer: 1024 * 1024,
    env: { PATH: process.env.PATH, LC_ALL: 'C', PGDATABASE: config.database, PGCONNECT_TIMEOUT: '3', PGOPTIONS: '-c statement_timeout=3000' } });
  return timing ? parseDatabaseProbe(stdout) : JSON.parse(stdout);
}

async function sample(config, child, units, cgroupPath, origin, metricsExpected) {
  const row = { elapsedMs: performance.now() - origin, metricsExpected };
  const values = await Promise.allSettled([
    serverProcessTree(config.serverPid, units), readFile(`/proc/${child.pid}/stat`, 'utf8'),
    Promise.all([readFile('/proc/stat', 'utf8'), readFile('/proc/meminfo', 'utf8')]),
    Promise.all([readFile(join(cgroupPath, 'cpu.stat'), 'utf8'), readFile(join(cgroupPath, 'cpu.max'), 'utf8')]),
  ]);
  row.server = values[0].status === 'fulfilled' ? values[0].value : null;
  row.generator = values[1].status === 'fulfilled' ? linuxProcessSample(values[1].value, ...units) : null;
  row.host = values[2].status === 'fulfilled' ? parseHost(...values[2].value) : null;
  row.cgroup = values[3].status === 'fulfilled' ? parseCgroup(...values[3].value) : null;
  if (metricsExpected) {
    const extra = await Promise.allSettled([
      fetch(`${config.base}/metrics`, { headers: { Authorization: `Bearer ${config.metricsToken}` }, signal: AbortSignal.timeout(2000) })
        .then(response => { requireValue(response.ok, 'metrics_request_failed'); return response.text(); }).then(parseMetrics),
      databaseQuery(config, "SELECT json_build_object('connections',count(*),'active',count(*) FILTER (WHERE state='active' AND pid<>pg_backend_pid()),'waiting',count(*) FILTER (WHERE wait_event_type='Lock')) FROM pg_stat_activity WHERE datname=current_database()", true),
    ]);
    row.metrics = extra[0].status === 'fulfilled' ? extra[0].value : null;
    row.database = extra[1].status === 'fulfilled' ? extra[1].value : null;
  }
  return row;
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return { forced: false, stopped: true };
  child.kill('SIGTERM');
  const deadline = performance.now() + 5000;
  while (child.exitCode === null && child.signalCode === null && performance.now() < deadline) await delay(50);
  const forced = child.exitCode === null && child.signalCode === null;
  if (forced) {
    child.kill('SIGKILL');
    const killDeadline = performance.now() + 2000;
    while (child.exitCode === null && child.signalCode === null && performance.now() < killDeadline) await delay(50);
  }
  return { forced, stopped: child.exitCode !== null || child.signalCode !== null };
}

export async function main(env = process.env) {
  const config = configuration(env);
  process.kill(config.serverPid, 0);
  await mkdir(config.output, { recursive: true, mode: 0o700 });
  const privateDirectory = await mkdtemp(join(config.temporaryRoot, 'account-room-private.'));
  await chmod(privateDirectory, 0o700);
  const report = { schemaVersion: 1, purpose: 'account-room-operating-point', completed: false, passed: false,
    startedAt: new Date().toISOString(), revision: config.revision,
    workload: { clients: CLIENTS, rooms: 1, durationSeconds: DURATION, rampSeconds: RAMP, warmupSeconds: WARMUP,
      receiveMode: 'balanced', chatIntervalMs: 30000, historyRetentionDays: 7, mediaWorkers: 1 },
    setup: { registrationsPerIpPerHour: 100, authenticationRequestsPerIpPerMinute: 300, sourceAddresses: CLIENTS,
      registration: 'real HTTP requests', room: 'one retained, registered-only room' },
    validityLimits: { hostBusyShare: 0.85, hostStealShare: 0.05, sharedCgroupThrottledShare: 0.05,
      mediaWorkerCores: 0.7, generatorRuntimeLagMs: 500, resourceSamplingGapMs: 1500 },
    host: { platform: os.platform(), release: os.release(), arch: os.arch(), cpuModel: os.cpus()[0]?.model,
      logicalCpus: os.cpus().length, totalMemoryBytes: os.totalmem() },
    limitations: ['Co-located hosted Linux generator, server and PostgreSQL; one operating point, not production maximum capacity.',
      'Synthetic RTP forwarding, not thirty browser decoders or real devices, WAN, TLS or TURN.',
      'Database query probe excludes application pool acquisition; chat acknowledgement latency includes persistence and queuing.',
      'Media egress reports confirmed received RTP payload bytes; wire overhead is an explicit estimate.',
      'Setup uses the disposable helper registration/auth budgets; room/media/default admission limits are retained.',
      'The outer server helper records shutdown independently in server-shutdown.json.'],
    phase: 'setup', failure: null };
  let child, log, samplesFile, interrupted = false;
  const interrupt = () => { interrupted = true; if (child) child.kill('SIGTERM'); };
  process.once('SIGINT', interrupt);
  process.once('SIGTERM', interrupt);
  try {
    report.harnessSha256 = sha256(await readFile(new URL(import.meta.url)));
    report.resourceHelperSha256 = sha256(await readFile(new URL('../load_tests/benchmark-local.mjs', import.meta.url)));
    report.cargoLockSha256 = sha256(await readFile(new URL('../Cargo.lock', import.meta.url)));
    report.databaseServerVersion = await databaseQuery(config, "SELECT to_json(current_setting('server_version_num')::integer)");
    requireValue(report.databaseServerVersion === 180006, 'pinned_postgresql_version_required');
    const accounts = [];
    const room = `account-room-${randomUUID()}`;
    let ownerToken;
    report.phase = 'registration';
    for (let index = 0; index < CLIENTS; index++) {
      requireValue(!interrupted, 'experiment_interrupted');
      const email = `account-room-${randomUUID()}@example.test`;
      const password = `${randomBytes(32).toString('base64url')}Aa1!`;
      const registered = await request(config, index, '/api/auth/register', { email, password, display_name: `Account ${index + 1}` });
      requireValue(UUID.test(registered.user?.id ?? '') && typeof registered.token === 'string', 'invalid_registered_account');
      accounts.push({ email, password, accountId: registered.user.id });
      if (index === 0) ownerToken = registered.token;
    }
    report.phase = 'room_creation';
    await request(config, 0, '/api/rooms', { id: room, display_name: 'Thirty-account measurement', require_registration: true,
      guests_allowed: false, secret: true, max_participants: CLIENTS, max_broadcasters: CLIENTS }, ownerToken);
    ownerToken = undefined;
    report.roomId = room;
    report.accountsRegistered = accounts.length;
    const credentials = join(privateDirectory, 'accounts.json');
    await writeFile(credentials, JSON.stringify(accounts), { flag: 'wx', mode: 0o600 });
    await mkdir(join(config.output, 'generator'), { mode: 0o700 });
    const args = generatorArguments(config, credentials, room);
    report.generatorBinarySha256 = sha256(await readFile(config.generator));
    report.serverBinarySha256 = sha256(await readFile(`/proc/${config.serverPid}/exe`));
    report.arguments = args.map(value => value === credentials ? '<private credential cohort>' : value);
    const units = await Promise.all(['CLK_TCK', 'PAGESIZE'].map(unit => exec('getconf', [unit]).then(value => Number(value.stdout.trim()))));
    const cgroup = /^0::(\/.*)$/m.exec(await readFile(`/proc/${config.serverPid}/cgroup`, 'utf8'))?.[1];
    requireValue(cgroup && !cgroup.split('/').includes('..'), 'unified_server_cgroup_required');
    const cgroupPath = join('/sys/fs/cgroup', cgroup);
    log = await open(join(privateDirectory, 'generator.log'), 'wx', 0o600);
    samplesFile = await open(join(config.output, 'resource-samples.ndjson'), 'wx', 0o600);
    const origin = performance.now(), wallOrigin = Date.now();
    report.phase = 'generator';
    child = spawn(config.generator, args, { cwd: process.cwd(), env: { PATH: env.PATH, RUST_LOG: 'error' }, stdio: ['ignore', log.fd, log.fd] });
    let result;
    child.once('error', () => { result = { code: null, signal: null, spawnFailed: true }; });
    child.once('exit', (code, signal) => { result = { code, signal }; });
    const samples = [];
    let tick = 0;
    while (!result && performance.now() - origin < (RAMP + WARMUP + DURATION + 180) * 1000) {
      requireValue(!interrupted, 'experiment_interrupted');
      const row = await sample(config, child, units, cgroupPath, origin, tick++ % 10 === 0);
      samples.push(row);
      await samplesFile.write(`${JSON.stringify(row)}\n`);
      await delay(500);
    }
    report.generatorExit = result ?? { code: null, signal: null, deadlineExceeded: true };
    const summary = JSON.parse(await readFile(join(config.output, 'generator/load_test_summary.json'), 'utf8'));
    report.generator = generatorVerdict(summary, result);
    report.completed = report.generator.completed;
    report.phase = 'authentication';
    report.authentication = await assess(() => authenticationVerdict(summary.authentication, accounts.map(account => account.accountId)));
    const measurementStart = Date.parse(summary.run?.startedAt ?? '') - wallOrigin + (RAMP + WARMUP) * 1000;
    report.phase = 'resource_validation';
    report.resources = await assess(() => {
      requireValue(report.generator.fullWindow, 'full_measurement_window_not_completed');
      const resources = measurementResources(samples, measurementStart, measurementStart + DURATION * 1000);
      const runtimeLag = summary.runtimeLag?.maxMsBySecond?.slice(RAMP + WARMUP, RAMP + WARMUP + DURATION);
      resources.generatorLagCoverage = runtimeLag?.length === DURATION && runtimeLag.every(value => Number.isFinite(value) && value >= 0);
      resources.generatorMaxLagMs = resources.generatorLagCoverage ? Math.max(...runtimeLag) : null;
      resources.passed &&= resources.generatorLagCoverage && resources.generatorMaxLagMs <= 500;
      if (!resources.passed) resources.failure = 'resource_validity_or_headroom_failed';
      return resources;
    });
    report.phase = 'retention_validation';
    report.retention = await assess(async () => {
      const inventory = JSON.parse(await readFile(join(config.output, 'generator/chat_inventory.json'), 'utf8'));
      const rows = await databaseQuery(config, `SELECT COALESCE(json_agg(json_build_object('messageId',id,'senderAccountId',sender_account,'clientMessageId',client_message_id,'content',body->>'content') ORDER BY id),'[]'::json) FROM chat_messages WHERE room_id='${room}' AND expires_at>clock_timestamp()`);
      const retention = retentionVerdict(rows, inventory, accounts.map(account => account.accountId));
      requireValue(retention.messages === summary.chat?.acknowledgements && summary.chat?.passed === true, 'retention_chat_ack_mismatch');
      const retentionDays = await databaseQuery(config, `SELECT to_json(history_retention_days) FROM rooms WHERE id='${room}'`);
      requireValue(retentionDays === 7, 'room_history_retention_not_enabled');
      return retention;
    });
    report.media = { payloadBytesReceived: summary.measurement?.bytesReceived, packetsReceived: summary.measurement?.packetsReceived,
      receivedPayloadMbps: summary.measurement?.bytesReceived * 8 / (DURATION * 1e6),
      estimatedIpv4MediaMbps: (summary.measurement?.bytesReceived + summary.measurement?.packetsReceived * 62) * 8 / (DURATION * 1e6) };
    const verdicts = [report.generator, report.authentication, report.resources, report.retention];
    report.passed = report.completed && verdicts.every(verdict => verdict.passed);
    report.failure = verdicts.find(verdict => !verdict.passed)?.failure ?? null;
    report.phase = 'completed';
  } catch (error) {
    report.failure = /^[a-z0-9_]+$/.test(error.message ?? '') ? error.message : 'orchestration_failed';
    throw error;
  } finally {
    report.generatorCleanup = await stop(child);
    await Promise.allSettled([log?.close(), samplesFile?.close()]);
    try {
      await rm(privateDirectory, { recursive: true, force: true });
      report.privateCredentialsRemoved = true;
    } catch {
      report.privateCredentialsRemoved = false;
      report.failure ??= 'private_credential_cleanup_failed';
    }
    if (report.generatorCleanup.forced || !report.generatorCleanup.stopped || !report.privateCredentialsRemoved)
      report.passed = false;
    process.removeListener('SIGINT', interrupt);
    process.removeListener('SIGTERM', interrupt);
    report.finishedAt = new Date().toISOString();
    await writeFile(join(config.output, 'account-room-report.json'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  }
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().then(report => {
    assert.equal(report.passed, true);
    console.log('Thirty-account operating-point measurement passed.');
  }).catch(() => {
    console.error('Account-room measurement failed; inspect the bounded report artifact.');
    process.exitCode = 1;
  });
}
