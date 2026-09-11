import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { cpuSeconds, resourceSummary, comparison, parseOptions, command, captureArguments, finishCapture, diagnosticPolicy, verifyExecutable, sourceTreeIdentity, identity, serverRevisionLabel } from './benchmark-local.mjs';

const exec = promisify(execFile);

async function sourceFixture(t) {
  const temporary = await mkdtemp(join(tmpdir(), 'simplestchat-source-identity.'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const root = join(temporary, 'checkout with spaces');
  await mkdir(root);
  const git = async args => (await exec('git', ['-C', root, '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test', ...args], {
    env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' },
  })).stdout.trim();
  const write = async (file, content) => {
    await mkdir(dirname(join(root, file)), { recursive: true });
    await writeFile(join(root, file), content);
  };
  await git(['init', '--initial-branch=main']);
  for (const [file, content] of Object.entries({
    '.gitignore': 'CLAUDE.md\nCLAUDE.local.md\nsrc/ignored.rs\n.cargo/config.toml\n/results/\n/target/\n',
    'Cargo.toml': '[package]\nname="fixture"\nversion="0.1.0"\n',
    'Cargo.lock': '# lock fixture\n', 'rust-toolchain.toml': '[toolchain]\nchannel="fixture"\n',
    'src/lib.rs': 'pub fn fixture() {}\n', 'src/deleted.rs': '// original\n',
    'vendor/native.cc': '// vendored native fixture\n', 'build/pip-constraints.txt': '# pinned fixture\n',
  })) await write(file, content);
  await git(['add', '--', '.']);
  await git(['commit', '-m', 'Initial fixture']);
  const binary = join(root, 'server');
  await writeFile(binary, 'binary fixture');
  return { root, binary, temporary, git, write };
}

test('records immediate spawn errors and fast nonzero exits', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-benchmark-unit.'));
  t.after(() => rm(directory, { recursive: true }));
  const missing = await command(join(directory, 'missing'), [], directory, {}, join(directory, 'missing.log'));
  assert.match((await missing.completion).error, /ENOENT/);
  const failed = await command(process.execPath, ['-e', 'process.exit(23)'], directory, {}, join(directory, 'failed.log'));
  assert.equal((await failed.completion).code, 23);
});

test('parses Linux and macOS process CPU time', () => {
  assert.equal(cpuSeconds('0:00.02'), 0.02);
  assert.equal(cpuSeconds('01:02:03'), 3723);
  assert.equal(cpuSeconds('1-02:03:04'), 93784);
  assert.throws(() => cpuSeconds('broken'));
});

test('CPU uses measured process deltas within the shared window', () => {
  const samples = [0, 1000, 2000, 3000].map(elapsedMs => ({ elapsedMs, server: { cpuSeconds: elapsedMs / 2000, rssKiB: 2048 } }));
  assert.deepEqual(resourceSummary(samples, 'server', 1000, 2000), { samples: 2, sampledDurationSeconds: 1, cpuSeconds: 0.5, cpuPercentOfOneCore: 50, peakRssMiB: 2, medianRssMiB: 2 });
  assert.throws(() => resourceSummary(samples, 'server', 1500, 1700));
});

test('comparison refuses unmatched runs rather than reporting false success', () => {
  assert.throws(() => comparison([{ scenario: 'conference-10', variant: 'baseline', joinP99Ms: 1 }]));
  assert.throws(() => comparison([{ purpose: 'diagnostic' }]), /not performance/);
});

test('bounded local orchestrator rejects unknown options and excessive load', () => {
  const required = ['--baseline-root', '/tmp/b', '--baseline-bin', '/tmp/b/server', '--candidate-root', '/tmp/c', '--candidate-bin', '/tmp/c/server', '--generator', '/tmp/generator', '--output', '/tmp/results'];
  assert.deepEqual(parseOptions(required).clients, [10]);
  assert.equal(parseOptions(required).purpose, 'performance');
  assert.equal(parseOptions([...required, '--purpose', 'diagnostic']).purpose, 'diagnostic');
  assert.throws(() => parseOptions([...required, '--purpose', 'unknown']), /purpose/);
  assert.throws(() => parseOptions([...required, '--capture-interface', 'lo0']), /diagnostic/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--capture-interface', 'en0']), /lo\/lo0/);
  assert.throws(() => parseOptions([...required, '--diagnostic-detail', 'capture-only']), /diagnostic/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'capture-only']), /capture-interface/);
  assert.throws(() => parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'unknown']), /diagnostic-detail/);
  const quiet = parseOptions([...required, '--purpose', 'diagnostic', '--diagnostic-detail', 'capture-only', '--capture-interface', 'lo0']);
  assert.deepEqual(diagnosticPolicy(quiet), { generatorArgs: [], requireSnapshots: false, serverLog: 'error', generatorLog: 'error' });
  assert.throws(() => parseOptions(['baseline-root', ...required.slice(1)]));
  for (const flags of [['--server', 'ws://example.com'], ['--clients', '1000'], ['--duration', '3600'], ['--clients', '10,10'], ['--port', '0'], ['--scenarios', 'unknown']]) assert.throws(() => parseOptions([...required, ...flags]));
  assert.throws(() => parseOptions([...required, '--clients', '50']), /join admission/);
  assert.throws(() => parseOptions([...required, '--clients', '50', '--scenarios', 'multi-room']), /join admission/);
  assert.throws(() => parseOptions([...required, '--scenarios', 'churn']), /join budget/);
  assert.equal(parseOptions([...required, '--clients', '30', '--scenarios', 'multi-room']).scenarios[0].rooms, 4);
  assert.equal(parseOptions([...required, '--clients', '50', '--ramp-up', '303']).clients[0], 50);
  assert.equal(parseOptions([...required, '--clients', '50', '--scenarios', 'multi-room', '--ramp-up', '101']).clients[0], 50);
});

test('optional capture is header-limited and scoped to owned loopback media ports', () => {
  const args = captureArguments({ purpose: 'diagnostic', captureInterface: 'lo0', udpPort: 41100, workers: 2 }, '/tmp/probe');
  assert.deepEqual(args, ['-i', 'lo0', '-p', '-nn', '-s', '64', '-B', '4096', '-U', '-c', '500000',
    '-w', '/tmp/probe/media-headers.pcap', 'udp and host 127.0.0.1 and portrange 41100-41101']);
  assert.throws(() => captureArguments({ purpose: 'performance', captureInterface: 'lo0' }, '/tmp/probe'));
  assert.throws(() => captureArguments({ purpose: 'diagnostic', captureInterface: 'en0' }, '/tmp/probe'));
  const quiet = captureArguments({ purpose: 'diagnostic', diagnosticDetail: 'capture-only', captureInterface: 'lo0', udpPort: 41100, workers: 1 }, '/tmp/probe');
  assert.equal(quiet[quiet.indexOf('-c') + 1], '2000000');
});

test('full diagnostics remain opt-in and quiet mode works with the original generator', () => {
  assert.deepEqual(diagnosticPolicy({ purpose: 'performance' }), { generatorArgs: [], requireSnapshots: false, serverLog: 'error', generatorLog: 'error' });
  const full = diagnosticPolicy({ purpose: 'diagnostic', diagnosticDetail: 'full' });
  assert.deepEqual(full.generatorArgs, ['--diagnostics']);
  assert.equal(full.requireSnapshots, true);
  assert.equal(full.generatorLog, 'warn,load_test=info');
});

test('server and generator executables must match their recorded hashes', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'simplestchat-binary-unit.'));
  t.after(() => rm(directory, { recursive: true }));
  const binary = join(directory, 'fixture');
  const expected = createHash('sha256').update('original').digest('hex');
  await writeFile(binary, 'original');
  await verifyExecutable(binary, expected, 'Generator');
  await verifyExecutable(binary, expected, 'candidate server');
  await writeFile(binary, 'changed');
  await assert.rejects(verifyExecutable(binary, expected, 'Generator'), /Generator binary changed/);
  await assert.rejects(verifyExecutable(binary, expected, 'candidate server'), /candidate server binary changed/);
  await assert.rejects(verifyExecutable(join(directory, 'missing'), expected, 'baseline server'), /ENOENT/);
});

test('source identity includes new Rust inputs and is unchanged by staging identical contents', async t => {
  const f = await sourceFixture(t);
  const before = await identity(f.root, f.binary);
  await f.write('src/readiness.rs', '// new first-party input\n');
  const untracked = await identity(f.root, f.binary);
  assert.equal(untracked.revision, before.revision);
  assert.equal(untracked.trackedDiffSha256, before.trackedDiffSha256, 'legacy diff cannot see untracked files');
  assert.notEqual(untracked.sourceTreeSha256, before.sourceTreeSha256);
  assert.equal(untracked.sourceTreeFiles, before.sourceTreeFiles + 1);
  await f.git(['add', '--', 'src/readiness.rs']);
  const staged = await identity(f.root, f.binary);
  assert.equal(staged.sourceTreeSha256, untracked.sourceTreeSha256);
  assert.notEqual(staged.trackedDiffSha256, untracked.trackedDiffSha256);
  await f.write('src/readiness.rs', '// changed after staging\n');
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, staged.sourceTreeSha256);
  await f.write('src/readiness.rs', '// new first-party input\n');
  await f.git(['commit', '-m', 'Track new input']);
  assert.equal((await sourceTreeIdentity(f.root)).sourceTreeSha256, staged.sourceTreeSha256, 'tree identity is independent of commit bookkeeping');
});

test('deleted source paths change identity consistently before staging and after commit', async t => {
  const f = await sourceFixture(t);
  const before = await sourceTreeIdentity(f.root);
  await rm(join(f.root, 'src/deleted.rs'));
  const deleted = await sourceTreeIdentity(f.root);
  assert.notEqual(deleted.sourceTreeSha256, before.sourceTreeSha256);
  assert.deepEqual(deleted.sourceTreeMissingPaths, ['src/deleted.rs']);
  assert.equal(deleted.sourceTreeFiles, before.sourceTreeFiles - 1);
  await f.git(['add', '-u']);
  assert.deepEqual(await sourceTreeIdentity(f.root), deleted, 'staging a deletion must not change its identity');
  await f.git(['commit', '-m', 'Remove input']);
  const committed = await sourceTreeIdentity(f.root);
  assert.equal(committed.sourceTreeSha256, deleted.sourceTreeSha256);
  assert.deepEqual(committed.sourceTreeMissingPaths, []);
});

test('ignored, private and generated paths do not enter the scoped source fingerprint', async t => {
  const f = await sourceFixture(t);
  const before = await sourceTreeIdentity(f.root);
  for (const file of ['CLAUDE.md', 'src/CLAUDE.md', 'vendor/CLAUDE.local.md', 'src/ignored.rs', '.cargo/config.toml', 'results/report.json', 'target/generated.rs', 'README.md']) await f.write(file, 'private or out-of-scope fixture\n');
  assert.deepEqual(await sourceTreeIdentity(f.root), before);
  await f.write('build.rs', '// build script\n');
  const buildScript = await sourceTreeIdentity(f.root);
  assert.notEqual(buildScript.sourceTreeSha256, before.sourceTreeSha256);
  await f.write('.cargo/config', '[build]\n');
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, buildScript.sourceTreeSha256);
});

test('source fingerprints include stable path boundaries and executable modes, not timestamps or index order', async t => {
  const f = await sourceFixture(t);
  await f.write('src/a.rs', 'one');
  await f.write('src/line\nbreak.rs', 'two');
  const original = await sourceTreeIdentity(f.root);
  await f.git(['add', '--', 'src/line\nbreak.rs', 'src/a.rs']);
  assert.deepEqual(await sourceTreeIdentity(f.root), original);
  await f.write('src/a.rs', 'one');
  assert.deepEqual(await sourceTreeIdentity(f.root), original, 'rewriting the same bytes does not change identity');
  await f.write('src/a.rs', 'two');
  await f.write('src/line\nbreak.rs', 'one');
  const swapped = await sourceTreeIdentity(f.root);
  assert.notEqual(swapped.sourceTreeSha256, original.sourceTreeSha256, 'file paths and content boundaries matter');
  await chmod(join(f.root, 'src/a.rs'), 0o755);
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, swapped.sourceTreeSha256);
});

test('symlink identity records its target without reading outside the source scope', async t => {
  const f = await sourceFixture(t);
  const outside = join(f.temporary, 'outside-input');
  await writeFile(outside, 'outside version one');
  await symlink(outside, join(f.root, 'src/linked.rs'));
  const original = await sourceTreeIdentity(f.root);
  await writeFile(outside, 'outside version two');
  assert.deepEqual(await sourceTreeIdentity(f.root), original, 'external target contents are deliberately outside the fingerprint');
  await rm(join(f.root, 'src/linked.rs'));
  await symlink(`${outside}-different`, join(f.root, 'src/linked.rs'));
  assert.notEqual((await sourceTreeIdentity(f.root)).sourceTreeSha256, original.sourceTreeSha256);
});

test('server provenance retains legacy diff hashes and labels Git, source and binary independently', async t => {
  const f = await sourceFixture(t);
  await f.write('src/lib.rs', 'pub fn changed() {}\n');
  const recorded = await identity(f.root, f.binary);
  const legacyDiff = await f.git(['diff', 'HEAD', '--', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'src', 'vendor', 'build/pip-constraints.txt']);
  assert.equal(recorded.trackedDiffSha256, createHash('sha256').update(legacyDiff).digest('hex'));
  assert.equal(serverRevisionLabel(recorded), `git:${recorded.revision};source:sha256:${recorded.sourceTreeSha256};binary:sha256:${recorded.binarySha256}`);
  assert.match(recorded.sourceTreeSha256, /^[a-f0-9]{64}$/);
  await writeFile(f.binary, 'different frozen executable');
  const changedBinary = await identity(f.root, f.binary);
  assert.equal(changedBinary.sourceTreeSha256, recorded.sourceTreeSha256);
  assert.equal(changedBinary.revision, recorded.revision);
  assert.notEqual(changedBinary.binarySha256, recorded.binarySha256);
  assert.notEqual(serverRevisionLabel(changedBinary), serverRevisionLabel(recorded));
});

test('capture drains after generation before termination and skips drain if already finished', async () => {
  const events = [];
  let complete;
  const capture = { completion: new Promise(resolve => { complete = resolve; }),
    kill(signal) {
      events.push(signal);
      capture.result = { code: 0 };
      complete(capture.result);
    } };
  await finishCapture(capture, async () => { events.push('drain'); });
  assert.deepEqual(events, ['drain', 'SIGTERM']);
  await finishCapture(capture, async () => { assert.fail('completed capture must not drain again'); });
});

test('late capture failures and unflushed termination cannot become diagnostic success', async () => {
  await finishCapture({ result: { code: 0 } });
  for (const result of [{ code: 2 }, { error: 'ENOENT' }, { code: null, signal: 'SIGKILL' }, { code: null, signal: 'SIGTERM' }]) {
    await assert.rejects(finishCapture({ result }), /did not finish cleanly/);
  }
  let complete;
  const capture = { completion: new Promise(resolve => { complete = resolve; }),
    kill() { queueMicrotask(() => { capture.result = { code: 2 }; complete(capture.result); }); } };
  await assert.rejects(finishCapture(capture, async () => {}), /did not finish cleanly/);
  const failedDuringDrain = {};
  await assert.rejects(finishCapture(failedDuringDrain, async () => {
    failedDuringDrain.result = { code: 2 };
  }), /did not finish cleanly/);
  await assert.rejects(finishCapture({}, async () => { throw new Error('drain failed'); }), /drain failed/);
});
