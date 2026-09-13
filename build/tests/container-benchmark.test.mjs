import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

// Exercise the actual pure validators, never the root/Docker launchers. Fixture
// data travels over stdin: no /run files, network namespaces or daemon calls.
const runner = readFileSync(new URL('../benchmark-container.sh', import.meta.url), 'utf8');
const cleanup = readFileSync(new URL('../cleanup-container-benchmark.sh', import.meta.url), 'utf8');
const extract = (source, expression) => {
  const match = source.match(expression);
  assert.ok(match, `Validator extraction must be updated deliberately: ${expression}`);
  return match[1];
};
const summaryFilter = extract(runner, /'([.]schemaVersion == 2[\s\S]*?)' "\$summary"/);
const countFilter = extract(runner, /zero_counts\(\) \{\s+awk '([\s\S]*?)' "\$1"\s+\}/);
const ownershipFilter = extract(cleanup, /'(\.id == \$id and \.image == \$image[^']*)'/);
const exitFilter = extract(runner, /jq -e '(\.Running == false and \.ExitCode == 0[^']*)'/);
const boundedFunction = extract(runner, /^(bounded\(\) \{[^\n]+\})$/m);
const publicChatGuard = extract(runner, /^(require_public_chat_stopped\(\) \{[\s\S]*?^\})$/m);
const emergencyBlock = extract(cleanup, /(if \[\[ "\$mode" == --emergency \]\]; then\n  retained_result=''[\s\S]*?\nfi)\nfor role/);
const invoke = (command, args, input = '') => {
  const result = spawnSync(command, args, {
    input, encoding: 'utf8', timeout: 2000, maxBuffer: 65536,
    env: { PATH: process.env.PATH, LC_ALL: 'C' },
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, result.stderr);
  return result;
};
const jq = (filter, value, args = []) => invoke('jq', ['-e', ...args, filter], JSON.stringify(value));
const workload = { clients: 10, rooms: 1, duration: 60, rampUp: 5, warmup: 10,
  run: 'a'.repeat(32), server: `sha256:${'b'.repeat(64)}`, generator: `sha256:${'c'.repeat(64)}` };
const expectedConsumers = ({ clients, rooms }) => Array.from({ length: rooms }, (_, index) => {
  const members = Math.floor(clients / rooms) + Number(index < clients % rooms);
  return members * Math.min(members - 1, 4) * 2;
}).reduce((sum, count) => sum + count, 0);
function summary(options = workload) {
  return {
    schemaVersion: 2, totalErrors: 0, failedConnections: 0, failedConsumers: 0,
    validatedConsumers: expectedConsumers(options), skippedShortLivedConsumers: 0,
    run: { completed: true, passed: true,
      configuration: { numClients: options.clients, numRooms: options.rooms, durationSecs: options.duration,
        rampUpSecs: options.rampUp, warmupSecs: options.warmup, runLabel: options.run },
      provenance: { serverRevision: options.server, generatorRevision: options.generator } },
    attemptCoverage: { version: 1, scope: 'stable-publishers', available: true,
      attempts: options.clients, passedAttempts: options.clients, failedAttempts: 0,
      missingCoverageAttempts: 0, skippedShortTailAttempts: 0, requestedChurners: 0, validatedChurners: 0 },
  };
}
function validateSummary(value, options = workload) {
  const args = Object.entries(options).flatMap(([key, item]) =>
    [typeof item === 'number' ? '--argjson' : '--arg', key, String(item)]);
  return jq(summaryFilter, value, args);
}
const set = (object, path, value) => {
  const fields = path.split('.');
  const key = fields.pop();
  const parent = fields.reduce((current, field) => current[field], object);
  if (value === undefined) delete parent[key];
  else parent[key] = value;
};

test('container summary accepts current passing one-room and uneven multi-room coverage', () => {
  for (const [clients, rooms, consumers] of [[4, 1, 24], [10, 1, 80], [10, 4, 32], [30, 4, 240]]) {
    const options = { ...workload, clients, rooms };
    const value = summary(options);
    assert.equal(value.validatedConsumers, consumers);
    const result = validateSummary(value, options);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), 'true');
  }
});

for (const [name, changes] of [
  ['legacy summary schema', [['schemaVersion', 1]]],
  ['missing completion', [['run.completed', undefined]]],
  ['incomplete workload', [['run.completed', false]]],
  ['failed workload', [['run.passed', false]]],
  ['truthy string pass', [['run.passed', 'true']]],
  ['signaling errors', [['totalErrors', 1]]],
  ['failed connections', [['failedConnections', 1]]],
  ['failed consumer', [['failedConsumers', 1]]],
  ['absent attempt coverage', [['attemptCoverage', undefined]]],
  ['wrong coverage version', [['attemptCoverage.version', 2]]],
  ['wrong coverage scope', [['attemptCoverage.scope', 'all-streams']]],
  ['unavailable coverage', [['attemptCoverage.available', false]]],
  ['failed attempt', [['attemptCoverage.failedAttempts', 1]]],
  ['missing attempt', [['attemptCoverage.missingCoverageAttempts', 1]]],
  ['short attempt skipped', [['attemptCoverage.skippedShortTailAttempts', 1]]],
  ['reset attempt counters', [['attemptCoverage.attempts', 0], ['attemptCoverage.passedAttempts', 0]]],
  ['partial successful attempts', [['attemptCoverage.passedAttempts', 9]]],
  ['unexpected churn request', [['attemptCoverage.requestedChurners', 1]]],
  ['unexpected validated churn', [['attemptCoverage.validatedChurners', 1]]],
  ['no received consumers', [['validatedConsumers', 0]]],
  ['incomplete consumer coverage', [['validatedConsumers', 79]]],
  ['double-counted consumers', [['validatedConsumers', 81]]],
  ['skipped consumer', [['skippedShortLivedConsumers', 1]]],
  ['unavailable consumer counter', [['failedConsumers', undefined]]],
  ['string consumer counter', [['validatedConsumers', '80']]],
  ['foreign run label', [['run.configuration.runLabel', 'd'.repeat(32)]]],
  ['foreign server image', [['run.provenance.serverRevision', `sha256:${'d'.repeat(64)}`]]],
  ['foreign generator image', [['run.provenance.generatorRevision', `sha256:${'e'.repeat(64)}`]]],
  ['narrowed client population', [['run.configuration.numClients', 9]]],
  ['changed room distribution', [['run.configuration.numRooms', 2]]],
  ['shortened measurement', [['run.configuration.durationSecs', 30]]],
  ['changed launch ramp', [['run.configuration.rampUpSecs', 10]]],
  ['changed warmup', [['run.configuration.warmupSecs', 11]]],
]) {
  test(`container summary rejects ${name}`, () => {
    const value = summary();
    for (const [path, replacement] of changes) set(value, path, replacement);
    assert.notEqual(validateSummary(value).status, 0, name);
  });
}

const zeroMetrics = [
  'simplestchat_rooms_active 0', 'simplestchat_participants_active 0',
  'simplestchat_connections_active 0', 'simplestchat_participants_snapshot_complete 1',
].join('\n') + '\n';
const counts = input => invoke('awk', [countFilter], input);
test('container cleanup accepts complete zero counts and ignores unrelated counters', () => {
  assert.equal(counts(`# TYPE simplestchat_rooms_active gauge\n${zeroMetrics}simplestchat_connections_total 10\n`).status, 0);
});
test('container cleanup rejects missing, duplicate, malformed or incomplete count evidence', () => {
  const cases = ['', 'not metrics\n'];
  for (const line of zeroMetrics.trim().split('\n')) {
    cases.push(zeroMetrics.replace(`${line}\n`, ''), `${zeroMetrics}${line}\n`);
  }
  for (const value of ['1', '-1', '0.0', 'NaN', 'Inf', '0 extra', '']) {
    cases.push(zeroMetrics.replace('rooms_active 0', `rooms_active ${value}`));
  }
  cases.push(zeroMetrics.replace('snapshot_complete 1', 'snapshot_complete 0'),
    zeroMetrics.replace('snapshot_complete 1', 'snapshot_complete true'),
    `${zeroMetrics}simplestchat_rooms_active 1\n`);
  for (const input of cases) assert.notEqual(counts(input).status, 0, JSON.stringify(input));
});

test('cleanup ownership predicate requires the exact container, image, run label and role', () => {
  const owned = { id: 'd'.repeat(64), image: workload.server, run: workload.run, role: 'server' };
  const args = Object.entries(owned).flatMap(([key, value]) => ['--arg', key, value]);
  assert.equal(jq(ownershipFilter, owned, args).status, 0);
  for (const field of Object.keys(owned)) {
    for (const value of [undefined, null, '', 'foreign']) {
      const candidate = { ...owned }; set(candidate, field, value);
      assert.notEqual(jq(ownershipFilter, candidate, args).status, 0, `${field}/${value}`);
    }
  }
});
test('native exit predicate rejects signals, OOM, errors, restarts and unknown exit evidence', () => {
  const state = { Running: false, ExitCode: 0, OOMKilled: false, Error: '', Restarting: false };
  assert.equal(jq(exitFilter, state).status, 0);
  for (const [field, value] of [['Running', true], ['ExitCode', 1], ['ExitCode', 137],
    ['ExitCode', 143], ['OOMKilled', true], ['Error', 'cleanup failed'], ['Restarting', true]]) {
    assert.notEqual(jq(exitFilter, { ...state, [field]: value }).status, 0);
  }
  for (const field of Object.keys(state)) {
    const missing = { ...state }; delete missing[field];
    assert.notEqual(jq(exitFilter, missing).status, 0, field);
  }
});
test('bounded option validator rejects malformed numbers before arithmetic evaluation', () => {
  const check = value => invoke('bash', ['-c', `${boundedFunction}\nbounded "$@"`, 'fixture', value, '1', '4']);
  for (const value of ['1', '4']) assert.equal(check(value).status, 0);
  for (const value of ['0', '5', '-1', '01', '1.5', 'Infinity', '', '1+1', '$(printf unsafe)']) {
    const result = check(value);
    assert.notEqual(result.status, 0, value);
    assert.equal(result.stdout, '');
  }
});

test('private benchmark permits an idle public project and refuses active or unknown public state', () => {
  const script = `set -eu
fixture_containers=$1
fixture_status=$2
docker_owned() {
  [[ $# == 4 && $1 == ps && $2 == --quiet && $3 == --filter && $4 == label=com.docker.compose.project=simplestchat-public ]] || return 97
  printf '%s' "$fixture_containers"
  return "$fixture_status"
}
${publicChatGuard}
require_public_chat_stopped`;
  for (const [containers, status, permitted] of [
    ['', 0, true], ['a'.repeat(12), 0, false], ['a'.repeat(12) + '\n' + 'b'.repeat(12), 0, false],
    [' ', 0, false], ['unknown output', 0, false], ['', 1, false], ['', 124, false],
    ['a'.repeat(12), 1, false],
  ]) {
    const result = invoke('bash', ['-c', script, 'fixture', containers, String(status)]);
    assert.equal(result.status === 0, permitted, `containers=${JSON.stringify(containers)}, status=${status}`);
    assert.equal(result.stdout, '', 'The guard must not expose container identities');
    if (!permitted) assert.match(result.stderr, /Public chat is running|Cannot verify whether public chat is running/);
  }
});

test('public-project refusal precedes private workload state and never changes public containers', () => {
  const invocation = runner.indexOf('\nrequire_public_chat_stopped\n');
  assert.ok(invocation > runner.indexOf('flock --exclusive --nonblock 9'));
  assert.ok(invocation < runner.indexOf('for image in "$server_image" "$generator_image"'));
  assert.ok(invocation < runner.indexOf('mkdir -m 700 -- "$output"'));
  assert.ok(invocation < runner.indexOf('run_id='));
  assert.match(runner, /docker_owned\(\) \{ timeout --signal=TERM --kill-after=1s 8s docker/);
  assert.doesNotMatch(publicChatGuard, /\b(?:stop|kill|restart|rm|compose|prune)\s+--/);
  assert.equal((publicChatGuard.match(/docker_owned /g) ?? []).length, 1);
});

function emergencyFixture(t, preparation = '') {
  const directory = mkdtempSync(path.join(tmpdir(), 'simplestchat-emergency-evidence.'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const run = () => invoke('bash', ['-c', `set -eu\numask 077\noutput=$1\nrun_id=$2\nmode=--emergency\ncleanup_ok=true\n${preparation}\n${emergencyBlock}\nprintf '%s' "$cleanup_ok"`,
    'fixture', directory, workload.run]);
  return { directory, run, result: path.join(directory, 'result.json') };
}
test('emergency cleanup preserves original result bytes in unique private files before replacement', t => {
  const fixture = emergencyFixture(t);
  const retained = [];
  for (const [phase, launcherExit] of [['summary_validation', 1], ['server_shutdown', 124]]) {
    const original = `${JSON.stringify({ passed: false, phase, launcherExit }, null, 2)}\n`;
    writeFileSync(fixture.result, original, { mode: 0o600 });
    const result = fixture.run();
    assert.equal(result.status, 0, result.stderr); assert.equal(result.stdout, 'true');
    const emergency = JSON.parse(readFileSync(fixture.result, 'utf8'));
    assert.equal(emergency.passed, false); assert.equal(emergency.completed, false);
    assert.match(emergency.retainedResult, /^result-before-emergency\.[a-zA-Z0-9]+$/);
    const filename = path.join(fixture.directory, emergency.retainedResult);
    assert.equal(readFileSync(filename, 'utf8'), original);
    assert.equal(statSync(filename).mode & 0o777, 0o600);
    retained.push({ filename, original });
  }
  assert.notEqual(retained[0].filename, retained[1].filename);
  for (const entry of retained) assert.equal(readFileSync(entry.filename, 'utf8'), entry.original);
});
test('emergency cleanup records absence without inventing retained prior evidence', t => {
  const fixture = emergencyFixture(t);
  assert.equal(fixture.run().status, 0);
  assert.equal(JSON.parse(readFileSync(fixture.result, 'utf8')).retainedResult, null);
  assert.deepEqual(readdirSync(fixture.directory), ['result.json']);
});
test('failed emergency evidence copy keeps the original result and cleanup failure', t => {
  const fixture = emergencyFixture(t, 'cp() { return 1; }');
  const original = '{"passed":false,"phase":"generator","launcherExit":124}\n';
  writeFileSync(fixture.result, original, { mode: 0o600 });
  const result = fixture.run();
  assert.equal(result.status, 0); assert.equal(result.stdout, 'false');
  assert.match(result.stderr, /leaving it unchanged/);
  assert.equal(readFileSync(fixture.result, 'utf8'), original);
});

// These source guards document launch/cleanup invariants; they are deliberately
// not a Docker/systemd integration test or a simulated native workload.
test('container launcher has no public URL, publishing or implicit image pull path', () => {
  assert.match(runner, /--network none/);
  assert.match(runner, /--network "container:\$server_id"/);
  assert.match(runner, /--server ws:\/\/127\.0\.0\.1:3000\/ws/);
  assert.match(runner, /--pull never/);
  assert.match(runner, /--read-only --user 10001:10001 --cap-drop ALL/);
  assert.match(runner, /curl --disable --config - --noproxy '\*'/);
  assert.doesNotMatch(runner, /--publish|--network host|--privileged/);
  assert.ok(runner.indexOf('mv -- "$record.tmp" "$record"') < runner.indexOf('docker_owned create'));
  for (const role of ['server', 'generator']) {
    assert.ok(runner.indexOf(`>"$output/${role}.create-attempted"`) < runner.indexOf(`--name "scbench-$run_id-${role}"`));
  }
});
test('cleanup retains ownership, locking, uncertain-create and failure-preservation guards', () => {
  assert.match(cleanup, /flock --exclusive --timeout 30 9/);
  assert.match(cleanup, /exec 9<>\/run\/simplestchat-bench\/workload\.lock/);
  assert.match(cleanup, /0:700/); assert.match(cleanup, /0:600/);
  assert.match(cleanup, /--filter "name=\^\/\$\{name\}\$"/);
  assert.match(cleanup, /"\$output\/\$role\.create-attempted"/);
  assert.match(cleanup, /! -f "\$output\/\$role\.removed".*cleanup_ok=false/);
  assert.match(cleanup, /passed:false,error:"supervisor_or_launcher_interrupted"/);
  assert.ok(cleanup.indexOf(ownershipFilter) < cleanup.indexOf('stop --time'));
  assert.ok(cleanup.indexOf('final-state.json') < cleanup.indexOf('docker_owned rm'));
  assert.doesNotMatch(cleanup, /prune|rm --force|rm -f|--volumes/);
  assert.match(runner, /"\$status" == 0 && "\$workload_ok" == true && "\$cleanup_ok" == true/);
  assert.match(runner, /load_test_timeout\.json/);
  assert.match(runner, /\[\[ \$\(<"\$output\/generator\.wait\.txt"\) == 0 \]\]/);
});
