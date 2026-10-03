import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const revision = 'a'.repeat(40);
const base = 'b'.repeat(40);
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
const dispatcher = String.raw`
tool="$1"
shift
printf '%s\t' "$tool" "$DOCKER_HOST" "$@" >> "$CI_FIXTURE_LOG"
printf '\n' >> "$CI_FIXTURE_LOG"
case "$tool" in
  git)
    case "$1" in
      status) [[ -z "$CI_FIXTURE_DIRTY" ]] || printf ' M source.js\n' ;;
      merge-base)
        if [[ "$2" == --is-ancestor ]]; then [[ -z "$CI_FIXTURE_NONANCESTOR" ]]; exit; fi
        printf '%s\n' bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ;;
      rev-parse)
        if [[ "$3" == HEAD ]]; then
          if [[ -n "$CI_FIXTURE_CHANGED" && -f "$CI_FIXTURE_LOG.act-seen" ]]; then
            printf '%s\n' cccccccccccccccccccccccccccccccccccccccc
          else printf '%s\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; fi
        elif [[ -n "$CI_FIXTURE_SAME_BASE" ]]; then printf '%s\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
        else printf '%s\n' bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; fi ;;
      *) exit 93 ;;
    esac ;;
  curl)
    [[ -z "$CI_FIXTURE_ENGINE_DOWN" ]] || exit 7
    if [[ "$*" == */_ping ]]; then echo OK
    elif [[ "$*" == */networks ]]; then echo '[{"Name":"bridge","IPAM":{"Config":[{"Gateway":"10.88.0.1"}]}}]'
    elif [[ -n "$CI_FIXTURE_BUSY" ]]; then echo '[{"Id":"existing"}]'
    else echo '[]'; fi ;;
  act)
    : > "$CI_FIXTURE_LOG.act-seen"
    echo 'fixture workflow result'
    if [[ -z "$CI_FIXTURE_NO_RECEIPT" ]]; then
      python3 - "$@" <<'PY'
import hashlib,json,os,pathlib,sys
args=sys.argv[1:]
event_path=pathlib.Path(args[args.index('--eventpath')+1])
event=json.loads(event_path.read_text())
identity={'runId':next(value.split('=',1)[1] for value in args if value.startswith('LOCAL_CI_RUN_ID=')), 'revision':event['after'], 'base':event['before']}
if os.environ.get('CI_FIXTURE_STALE_RECEIPT'): identity['runId']='f'*32
evidence=event_path.parent/'checks'
(evidence/'receipts').mkdir()
check='browser-fixture'
(evidence/'receipts'/f'{check}.json').write_text(json.dumps({'schema':1,'status':'passed','check':check,**identity}))
workflow_hashes={name:hashlib.sha256(pathlib.Path('.github/workflows',name).read_bytes()).hexdigest() for name in ['ci.yml','security.yml','codeql.yml']}
(evidence/'required.json').write_text(json.dumps({'schema':1,'status':'passed','gates':['fixture'],'checks':[check], 'workflows':workflow_hashes, **identity}))
PY
    fi
    exit "$CI_FIXTURE_ACT_EXIT" ;;
  uname)
    if [[ "$1" == -m ]]; then echo "$CI_FIXTURE_ARCH"; else echo "$CI_FIXTURE_OS"; fi ;;
  podman)
    [[ "$1" == machine ]] || exit 94
    case "$2" in
      inspect)
        if [[ "$*" == *'{{.State}}'* ]]; then
          if [[ -n "$CI_FIXTURE_STOPPED" ]]; then echo stopped; else echo running; fi
        elif [[ "$*" == *'{{.ConnectionInfo.PodmanSocket.Path}}'* ]]; then echo /owned-ci/socket; fi ;;
      list) if [[ -n "$CI_FIXTURE_OTHER_VM" ]]; then echo 'other-project true'; else echo 'simplestchat-ci false'; fi ;;
      init|start|stop) : ;;
      ssh) [[ -z "$CI_FIXTURE_QEMU_FAIL" ]] || exit 69 ;;
      *) exit 95 ;;
    esac ;;
  *) exit 96 ;;
esac
exit 0
`;

async function fixture(t) {
  await mkdir(path.join(root, 'results'), { recursive: true });
  const temporary = await mkdtemp(path.join(root, 'results/ci-local-test.'));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const checkout = path.join(temporary, 'checkout with spaces');
  const bin = path.join(temporary, 'fake-bin');
  const tool = path.join(temporary, 'tool.sh');
  const log = path.join(temporary, 'calls.jsonl');
  await Promise.all([mkdir(path.join(checkout, 'build'), { recursive: true }), mkdir(path.join(checkout, '.github/workflows'), { recursive: true }), mkdir(bin), writeFile(tool, dispatcher)]);
  for (const name of ['ci.yml', 'security.yml', 'codeql.yml']) await writeFile(path.join(checkout, '.github/workflows', name), `name: ${name}\n`);
  await copyFile(new URL('../ci-local.sh', import.meta.url), path.join(checkout, 'build/ci-local.sh'));
  for (const name of ['git', 'act', 'curl', 'podman', 'uname']) {
    const executable = path.join(bin, name);
    await writeFile(executable, `#!/bin/sh\nexec /bin/bash ${quote(tool)} ${quote(name)} "$@"\n`);
    await chmod(executable, 0o755);
  }
  return {
    checkout,
    async run(args = [], overrides = {}) {
      await writeFile(log, '');
      const result = spawnSync('/bin/bash', [path.join(checkout, 'build/ci-local.sh'), ...args], {
        cwd: checkout,
        env: { PATH: `${bin}:${process.env.PATH}`, CI_FIXTURE_LOG: log, CI_FIXTURE_OS: 'Darwin', CI_FIXTURE_ARCH: 'arm64', CI_FIXTURE_ACT_EXIT: '0', LC_ALL: 'C', ...overrides },
        encoding: 'utf8', timeout: 15_000,
      });
      assert.equal(result.error, undefined, result.stderr);
      const calls = (await readFile(log, 'utf8')).split('\n').filter(Boolean).map(line => {
        const [tool, dockerHost, ...args] = line.split('\t');
        args.pop();
        return { tool, dockerHost, args };
      });
      const output = result.stdout.match(/Local CI evidence: (.+)/)?.[1];
      let summary;
      try { summary = JSON.parse(await readFile(path.join(output, 'summary.json'), 'utf8')); } catch { /* preflight may refuse before evidence exists */ }
      return { ...result, calls, output, summary };
    },
  };
}

test('all runs native ARM suites with bounded resources and explicit AMD64 production coverage', async t => {
  const f = await fixture(t);
  const result = await f.run();
  assert.equal(result.status, 0, result.stderr);
  const act = result.calls.find(call => call.tool === 'act');
  const value = key => act.args[act.args.indexOf(key) + 1];
  assert.equal(value('--job'), 'required');
  assert.equal(value('--workflows'), '.github/workflows/ci.yml');
  assert.equal(value('--container-architecture'), '');
  assert.ok(act.args.includes('ubuntu-24.04=docker.io/catthehacker/ubuntu@sha256:84e94c96278dd26b8feb71226a521d259f8160cf6b1dd08e51fd42be43a82e56'));
  assert.equal(act.args.filter(value => value === '--platform').length, 1);
  assert.equal(value('--container-daemon-socket'), '-');
  assert.equal(value('--network'), 'bridge');
  assert.equal(value('--concurrent-jobs'), '1');
  assert.match(value('--container-options'), /--cpus=3 --memory=8g/);
  assert.match(value('--container-options'), /--privileged/);
  assert.match(value('--container-options'), /--cgroupns=private/);
  assert.match(value('--container-options'), /host\.docker\.internal:10\.88\.0\.1/);
  assert.ok(act.args.includes('--rm'));
  assert.ok(act.args.includes('--use-new-action-cache=true'));
  for (const option of ['--env-file', '--secret-file', '--var-file', '--input-file']) assert.equal(value(option), '/dev/null');
  assert.equal(act.dockerHost, 'unix:///owned-ci/socket');
  const event = JSON.parse(await readFile(value('--eventpath'), 'utf8'));
  assert.equal(event.before, base);
  assert.equal(event.after, revision);
  assert.equal(event.local_ci, true);
  assert.equal(Object.hasOwn(event, 'local_ci_parallel'), false);
  assert.equal(result.summary.completeLocalGate, true);
  assert.equal(result.summary.revision, revision);
  assert.equal(result.summary.base, base);
  assert.equal(result.summary.hostedOnly.length, 2);
  assert.equal(result.summary.runnerPlatform, 'linux/arm64');
  assert.equal(result.summary.productionPlatform, 'linux/amd64');
  for (const target of ['nativeSecurityPlatform', 'nativeCodeqlPlatform']) assert.equal(result.summary[target], 'linux/arm64');
  const qemu = result.calls.findIndex(call => call.tool === 'podman' && call.args[1] === 'ssh');
  const empty = result.calls.findIndex(call => call.tool === 'curl' && call.args.includes('http://localhost/containers/json?all=1'));
  assert.ok(qemu > empty);
});

test('a selected job can filter its matrix and cannot claim a complete gate', async t => {
  const f = await fixture(t);
  const result = await f.run(['browser', '--matrix', 'group:accounts'], { CI_FIXTURE_DIRTY: '1' });
  assert.equal(result.status, 0, result.stderr);
  const args = result.calls.find(call => call.tool === 'act').args;
  assert.equal(args[args.indexOf('--job') + 1], 'browser');
  assert.equal(args[args.indexOf('--matrix') + 1], 'group:accounts');
  assert.equal(result.summary.completeLocalGate, false);
});

test('full CI refuses filtered coverage, dirty source, bad base and unsupported concurrency before any engine action', async t => {
  for (const [args, env] of [
    [['all', '--matrix', 'group:accounts'], {}],
    [[], { CI_FIXTURE_DIRTY: '1' }],
    [[], { CI_FIXTURE_SAME_BASE: '1' }],
    [[], { CI_FIXTURE_NONANCESTOR: '1' }],
    [['all', '--jobs', '1'], {}],
    [['all', '--codeql', '/single-isa-bundle'], {}],
    [['all', '--job', 'web'], {}],
    [['release-security'], {}],
  ]) {
    const f = await fixture(t);
    const result = await f.run(args, env);
    assert.notEqual(result.status, 0);
    assert.ok(!result.calls.some(call => ['act', 'podman', 'curl'].includes(call.tool)));
  }
});

test('the owned VM is stopped only when this invocation started it, including workflow failures', async t => {
  const f = await fixture(t);
  const result = await f.run([], { CI_FIXTURE_STOPPED: '1', CI_FIXTURE_ACT_EXIT: '37' });
  assert.equal(result.status, 37, result.stderr);
  assert.equal(result.summary.status, 'failed');
  assert.equal(result.summary.completeLocalGate, false);
  const commands = result.calls.filter(call => call.tool === 'podman').map(call => call.args.slice(0, 2).join(' '));
  assert.equal(commands.filter(command => command === 'machine start').length, 1);
  assert.equal(commands.filter(command => command === 'machine stop').length, 1);
});

test('other VMs and existing containers are left untouched', async t => {
  for (const env of [{ CI_FIXTURE_STOPPED: '1', CI_FIXTURE_OTHER_VM: '1' }, { CI_FIXTURE_BUSY: '1' }]) {
    const f = await fixture(t);
    const result = await f.run([], env);
    assert.notEqual(result.status, 0);
    assert.ok(!result.calls.some(call => call.tool === 'act'));
    assert.ok(!result.calls.some(call => call.tool === 'podman' && ['start', 'stop', 'ssh'].includes(call.args[1])));
  }
});

test('explicit engines require disposable intent and a local Unix socket', async t => {
  for (const [args, env] of [
    [[], { DOCKER_HOST: 'unix:///shared/socket' }],
    [['all', '--disposable-engine'], { DOCKER_HOST: 'tcp://example.test:2375' }],
    [[], { CI_FIXTURE_OS: 'Linux' }],
  ]) {
    const f = await fixture(t);
    const result = await f.run(args, env);
    assert.notEqual(result.status, 0);
    assert.ok(!result.calls.some(call => ['act', 'curl'].includes(call.tool)));
  }
  const f = await fixture(t);
  const result = await f.run(['all', '--disposable-engine'], { DOCKER_HOST: 'unix:///owned/socket', CI_FIXTURE_OS: 'Linux' });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(!result.calls.some(call => call.tool === 'podman'));
  assert.equal(result.summary.runnerPlatform, 'linux/amd64');
  for (const target of ['productionPlatform', 'nativeSecurityPlatform', 'nativeCodeqlPlatform']) assert.equal(result.summary[target], 'linux/amd64');
});

test('failed owned-VM emulator selection stops before workflow execution', async t => {
  const f = await fixture(t);
  const result = await f.run([], { CI_FIXTURE_QEMU_FAIL: '1' });
  assert.equal(result.status, 69);
  assert.equal(result.summary.completeLocalGate, false);
  assert.ok(!result.calls.some(call => call.tool === 'act'));
});

test('only the actual runner ISA needs a pinned read-only CodeQL cache', async t => {
  const f = await fixture(t);
  const pin = JSON.parse(await readFile(new URL('../../security/codeql-toolchain.json', import.meta.url), 'utf8'));
  await mkdir(path.join(f.checkout, 'security'));
  await writeFile(path.join(f.checkout, 'security/codeql-toolchain.json'), JSON.stringify(pin));
  for (const platform of ['linux-x86_64', 'linux-aarch64']) {
    const bundle = pin.bundles[platform];
    const directory = path.join(f.checkout, 'target/codeql-tools', bundle.sha256);
    await mkdir(path.join(directory, 'codeql'), { recursive: true });
    await writeFile(path.join(directory, 'receipt.json'), JSON.stringify({ archiveSha256: bundle.sha256, archiveBytes: bundle.bytes }));
    await writeFile(path.join(directory, 'codeql/codeql'), '#!/bin/sh\nexit 0\n');
    await chmod(path.join(directory, 'codeql/codeql'), 0o755);
    const result = await f.run();
    assert.equal(result.status, 0, result.stderr);
    const args = result.calls.find(call => call.tool === 'act').args;
    const options = args[args.indexOf('--container-options') + 1];
    assert.equal(options.includes('target/codeql-tools'), platform === 'linux-aarch64');
    if (platform === 'linux-aarch64') assert.match(options.replaceAll('\\,', ','), /codeql-tools,readonly/);
    assert.ok(!args.some(value => value.startsWith('LOCAL_CODEQL_BINARY=')));
    await rm(directory, { recursive: true });
  }
});

test('changing source during a successful workflow invalidates the result', async t => {
  const f = await fixture(t);
  const result = await f.run([], { CI_FIXTURE_CHANGED: '1' });
  assert.notEqual(result.status, 0);
  assert.equal(result.summary.completeLocalGate, false);
  assert.match(result.stderr, /candidate changed during CI/);
});

test('exit zero without actual aggregate evidence or with another run receipt cannot pass', async t => {
  for (const env of [{ CI_FIXTURE_NO_RECEIPT: '1' }, { CI_FIXTURE_STALE_RECEIPT: '1' }]) {
    const f = await fixture(t);
    const result = await f.run([], env);
    assert.notEqual(result.status, 0);
    assert.equal(result.summary.completeLocalGate, false);
  }
});

test('existing evidence is neither replaced nor given a new summary', async t => {
  const f = await fixture(t);
  const existing = path.join(f.checkout, 'existing');
  await mkdir(existing);
  await writeFile(path.join(existing, 'summary.json'), 'keep this evidence');
  const result = await f.run(['all', '--output', existing]);
  assert.notEqual(result.status, 0);
  assert.equal(await readFile(path.join(existing, 'summary.json'), 'utf8'), 'keep this evidence');
});

test('helper entrypoints reject ordinary host invocation before starting tools or sockets', () => {
  for (const helper of ['ci-local-docker.sh', 'ci-local-postgres.sh']) {
    const result = spawnSync('/bin/bash', [path.join(root, 'build', helper)], {
      env: { PATH: process.env.PATH }, encoding: 'utf8', timeout: 5000,
    });
    assert.equal(result.status, 2);
    assert.match(result.stderr, /only for build\/ci-local.sh runners/);
  }
});
