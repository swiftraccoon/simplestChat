import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import {
  authenticationVerdict, configuration, databaseEnvironment, generatorArguments, generatorVerdict, measurementResources,
  parseCgroup, parseDatabaseProbe, parseHost, parseMetrics, retentionVerdict, serverProcessTree, sourceAddress,
} from '../account-room-check.mjs';

const environment = {
  GITHUB_ACTIONS: 'true', RUNNER_OS: 'Linux', ACCOUNT_ROOM_E2E: '1', DISPOSABLE_TEST_DATABASE: '1',
  BASE_URL: 'http://127.0.0.1:3119', DATABASE_URL: 'postgres://test_owner:disposable@127.0.0.1:5432/simplestchat_test?sslmode=disable',
  TEST_SERVER_PID: '123', TEST_METRICS_TOKEN: 'a'.repeat(64), SERVER_REVISION: 'b'.repeat(40),
  E2E_ARTIFACTS: '/tmp/account-room-artifacts', RUNNER_TEMP: '/tmp', TEST_GENERATOR_BINARY: '/checkout/target/release/load_test',
};

test('account-room workload is hosted, explicit, loopback and fixed to thirty accounts for twenty minutes', () => {
  const config = configuration(environment, 'linux');
  const args = generatorArguments(config, '/private/accounts.json', 'account-room-12345678-1234-1234-1234-123456789012');
  for (const [name, value] of [['--clients', '30'], ['--duration', '1200'], ['--rooms', '1'], ['--source-addresses', '30'],
    ['--profile', 'browser'], ['--receive-mode', 'balanced'], ['--chat-interval-ms', '30000'], ['--departure', 'explicit-leave']])
    assert.equal(args[args.indexOf(name) + 1], value);
  assert.equal(sourceAddress(0), '127.0.1.2');
  assert.equal(sourceAddress(29), '127.0.1.31');
  assert.throws(() => sourceAddress(30));
  for (const platform of ['darwin', 'win32']) assert.throws(() => configuration(environment, platform));
  for (const field of ['GITHUB_ACTIONS', 'RUNNER_OS', 'ACCOUNT_ROOM_E2E', 'DISPOSABLE_TEST_DATABASE', 'TEST_SERVER_PID', 'TEST_METRICS_TOKEN',
    'SERVER_REVISION', 'RUNNER_TEMP', 'E2E_ARTIFACTS', 'TEST_GENERATOR_BINARY'])
    assert.throws(() => configuration({ ...environment, [field]: '' }, 'linux'), field);
  for (const BASE_URL of ['https://example.com', 'http://127.0.0.2:3119', 'http://user:password@127.0.0.1:3119',
    'http://127.0.0.1:3119/other', 'http://127.0.0.1:3119/?secret=x'])
    assert.throws(() => configuration({ ...environment, BASE_URL }, 'linux'));
  for (const DATABASE_URL of ['postgres://user@public.example/simplestchat_test', 'postgres://user@127.0.0.1/production',
    'postgres://user@127.0.0.1/simplestchat_test?host=public.example'])
    assert.throws(() => configuration({ ...environment, DATABASE_URL }, 'linux'));
});

const accounts = Array.from({ length: 30 }, (_, index) => `00000000-0000-4000-8000-${String(index).padStart(12, '0')}`);
const inventory = accounts.map((senderAccountId, index) => ({ messageId: `message-${index}`, senderAccountId,
  clientMessageId: `client-${index}`, contentSha256: createHash('sha256').update(`content-${index}`).digest('hex') }));
const rows = inventory.map((row, index) => ({ ...row, content: `content-${index}` }));

test('retention verifies the exact acknowledged identities, owners and content, not only row counts', () => {
  assert.deepEqual(retentionVerdict(rows, inventory, accounts), { passed: true, messages: 30, accounts: 30 });
  for (const patch of [{ content: 'changed' }, { senderAccountId: accounts[1] }, { clientMessageId: 'foreign' }, { messageId: 'foreign' }])
    assert.throws(() => retentionVerdict([{ ...rows[0], ...patch }, ...rows.slice(1)], inventory, accounts));
  assert.throws(() => retentionVerdict(rows.slice(1), inventory, accounts));
  assert.throws(() => retentionVerdict([rows[0], rows[0], ...rows.slice(2)], inventory, accounts));
});

test('database client defaults separate the database name and percent-decoded credentials from its URI', () => {
  const env = databaseEnvironment('postgres://test%5Fowner:public%2Dfixture%21@127.0.0.1:5439/simplestchat_test?sslmode=disable', '/fixture/bin');
  assert.equal(env.PGHOST, '127.0.0.1');
  assert.equal(env.PGPORT, '5439');
  assert.equal(env.PGDATABASE, 'simplestchat_test');
  assert.equal(env.PGUSER, 'test_owner');
  assert.equal(env.PGPASSWORD, 'public-fixture!');
  assert.equal(env.PGSSLMODE, 'disable');
  assert.equal(env.PGPASSFILE, '/dev/null');
  assert.equal(databaseEnvironment('postgres://fixture@localhost/simplestchat_test', '/bin').PGPORT, '5432');
  assert.equal(databaseEnvironment('postgres://fixture@[::1]:5432/simplestchat_test', '/bin').PGHOST, '::1');
  assert.ok(!Object.values(env).some(value => value?.includes('postgres://')));
});

test('all thirty real accounts must refresh, renew their existing socket, and cross original expiry', () => {
  const authentication = accounts.map(accountId => ({ accountId, passed: true, originalExpiresAt: 1800000000,
    latestExpiresAt: 1800000800, originalExpiryCrossed: true, httpRefreshes: 1, socketRenewals: 1, failures: 0 }));
  assert.equal(authenticationVerdict(authentication, accounts).passed, true);
  for (const patch of [{ passed: false }, { httpRefreshes: 0 }, { socketRenewals: 0 }, { originalExpiryCrossed: false },
    { latestExpiresAt: 1800000000 }, { failures: 1 }, { accountId: accounts[1] }])
    assert.throws(() => authenticationVerdict([{ ...authentication[0], ...patch }, ...authentication.slice(1)], accounts));
});

test('a full completed generator failure preserves its measurement and failure reasons', () => {
  const summary = { run: { completed: true, passed: false, failureReasons: ['one consumer did not deliver'],
    configuration: { durationSecs: 1200, rampUpSecs: 60, warmupSecs: 30, profile: { name: 'browser' }, maxVideoConsumers: 9, maxAudioConsumers: 29 } },
    measurement: { durationMs: 1200000 }, totalClients: 30, successfulConnections: 30, failedConnections: 0, failedConsumers: 1, validatedConsumers: 1139 };
  const failed = generatorVerdict(summary, { code: 1, signal: null });
  assert.equal(failed.completed, true);
  assert.equal(failed.fullWindow, true, 'CPU, authentication and retained history can still be assessed separately');
  assert.equal(failed.passed, false);
  assert.deepEqual(failed.failureReasons, summary.run.failureReasons);
  assert.equal(generatorVerdict({ ...summary, failedConsumers: 0, validatedConsumers: 1140,
    run: { ...summary.run, passed: true, failureReasons: [] } }, { code: 0, signal: null }).passed, true);
  const incomplete = generatorVerdict({ ...summary, run: { ...summary.run, completed: false } }, { code: 1, signal: null });
  assert.equal(incomplete.fullWindow, false);
  assert.equal(incomplete.passed, false);
});

test('server accounting sums owned native descendants, including children of non-main threads', async () => {
  const stat = (pid, user, pages) => {
    const fields = Array(22).fill('0');
    fields[0] = 'S'; fields[11] = String(user); fields[12] = '10'; fields[19] = String(pid * 10); fields[21] = String(pages);
    return `${pid} (worker with spaces) ${fields.join(' ')}`;
  };
  const files = new Map([
    ['/proc/123/stat', stat(123, 100, 5)], ['/proc/456/stat', stat(456, 200, 7)], ['/proc/789/stat', stat(789, 300, 11)],
    ['/proc/123/task/123/children', '456'], ['/proc/123/task/124/children', '456'],
    ['/proc/456/task/456/children', '789'], ['/proc/789/task/789/children', ''],
  ]);
  const result = await serverProcessTree(123, [100, 4096], {
    readFile: async path => { assert.ok(files.has(path)); return files.get(path); },
    readdir: async path => path === '/proc/123/task' ? ['123', '124'] : [path.split('/')[2]],
  });
  assert.equal(result.childCount, 2);
  assert.equal(result.rssKiB, 92);
  assert.ok(Math.abs(result.cpuSeconds - 6.3) < 1e-9);
  assert.deepEqual(result.processes.map(process => process.pid), [123, 456, 789]);
});

test('database probe timing is separately parsed and incomplete server evidence does not become zero', () => {
  assert.deepEqual(parseDatabaseProbe('{"connections":5,"active":1,"waiting":0}\nTime: 0.321 ms\n'),
    { connections: 5, active: 1, waiting: 0, queryRoundTripMs: 0.321 });
  assert.throws(() => parseDatabaseProbe('{"connections":5}\n'));
  assert.throws(() => parseMetrics('# empty\n'));
  assert.deepEqual(parseMetrics('simplestchat_connections_active 30\nsimplestchat_db_pool_connections{state="used"} 2\n'),
    { simplestchat_connections_active: 30, 'simplestchat_db_pool_connections{state="used"}': 2 });
  assert.deepEqual(parseCgroup('usage_usec 500\nnr_periods 10\nnr_throttled 1\n', '200000 100000\n'),
    { periods: 10, throttledPeriods: 1, quotaCores: 2 });
  assert.equal(parseCgroup('usage_usec 500\nnr_periods 0\nnr_throttled 0\n', 'max 100000').quotaCores, null);
  assert.throws(() => parseCgroup('usage_usec 500', 'max 100000'));
  assert.deepEqual(parseHost('cpu 10 0 20 60 5 2 3 0 0 0\n', 'MemAvailable: 500000 kB\n'),
    { total: 100, idle: 65, steal: 0, availableBytes: 512000000 });
});

function resourceSamples() {
  return Array.from({ length: 2401 }, (_, index) => ({ elapsedMs: index * 500,
    server: { cpuSeconds: index * 0.1, rssKiB: 100000, childCount: 1, processes: [{ pid: 123, startTicks: 1000 }, { pid: 456, startTicks: 1001 }] },
    generator: { cpuSeconds: index * 0.3, rssKiB: 150000 },
    host: { total: index * 200, idle: index * 120, steal: 0, availableBytes: 2 ** 30 },
    cgroup: { periods: index, throttledPeriods: 0, quotaCores: null }, metricsExpected: index % 10 === 0,
    metrics: { 'simplestchat_media_worker_cpu{worker="0"}': 0.4, simplestchat_cpu_saturated: 0, simplestchat_memory_saturated: 0,
      simplestchat_connections_active: 30, simplestchat_participants_active: 30, simplestchat_participants_snapshot_complete: 1, simplestchat_rooms_active: 1,
      'simplestchat_database_errors_total{kind="pool_timeout"}': 0, 'simplestchat_db_pool_connections{state="used"}': 1,
      'simplestchat_db_pool_connections{state="max"}': 10 }, database: { queryRoundTripMs: 0.5, waiting: 0 },
  }));
}

test('resource evidence rejects missing coverage, observed saturation, contention and generator shared-host throttling', () => {
  const samples = resourceSamples();
  const report = measurementResources(samples, 0, 1200000);
  assert.equal(report.passed, true);
  assert.equal(report.server.cpuPercentOfOneCore, 20);
  assert.equal(report.database.p99Ms, 0.5);
  const embedded = resourceSamples();
  for (const row of embedded) {
    row.server.processes = [row.server.processes[0]];
    row.server.childCount = 0;
  }
  assert.equal(measurementResources(embedded, 0, 1200000).passed, true,
    'mediasoup workers are embedded threads, already included in the process CPU/RSS');
  const noWorkerMetric = resourceSamples();
  for (const row of noWorkerMetric) delete row.metrics['simplestchat_media_worker_cpu{worker="0"}'];
  assert.throws(() => measurementResources(noWorkerMetric, 0, 1200000), /worker_cpu/);
  assert.throws(() => measurementResources(samples.slice(20), 0, 1200000));
  for (const patch of [{ 'simplestchat_media_worker_cpu{worker="0"}': 0.8 }, { simplestchat_memory_saturated: 1 }, { simplestchat_participants_active: 29 },
    { 'simplestchat_database_errors_total{kind="pool_timeout"}': 1 }]) {
    const invalid = resourceSamples();
    Object.assign(invalid[100].metrics, patch);
    assert.equal(measurementResources(invalid, 0, 1200000).passed, false);
  }
  const throttled = resourceSamples();
  throttled.at(-1).cgroup.throttledPeriods = 200;
  assert.equal(measurementResources(throttled, 0, 1200000).passed, false);
  assert.throws(() => measurementResources(samples.map(row => ({ ...row, cgroup: null })), 0, 1200000));
  const changedTree = resourceSamples();
  changedTree[100].server.processes[1] = { pid: 789, startTicks: 2000 };
  assert.throws(() => measurementResources(changedTree, 0, 1200000), /process_tree/);
});

test('the hosted account-room experiment is manual only and publishes an explicit credential-free allowlist', async () => {
  const workflow = await readFile(new URL('../../.github/workflows/performance.yml', import.meta.url), 'utf8');
  const job = workflow.split('\n  account-room:\n')[1];
  assert.ok(job, 'manual account-room job exists');
  assert.match(job, /github\.event_name == 'workflow_dispatch' && inputs\.only == 'account-room'/);
  assert.match(job, /postgres:18\.6-bookworm@sha256:/);
  assert.match(job, /build\/with-test-server\.sh node build\/account-room-check\.mjs/);
  assert.match(job, /account-room-report\.json/);
  assert.doesNotMatch(job, /path:.*\*|accounts\.json|generator\.log/);
});
