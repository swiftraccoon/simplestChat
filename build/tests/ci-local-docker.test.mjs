import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

async function fixture(t, options = {}) {
  await mkdir(path.join(root, 'results'), { recursive: true });
  const directory = await mkdtemp(path.join(root, 'results/ci-d.'));
  const bin = path.join(directory, 'bin');
  const temporary = path.join(directory, 'tmp');
  const journal = path.join(directory, 'systemd/journal');
  const journalLogs = path.join(directory, 'journal-logs');
  const config = path.join(directory, 'systemd/journald.conf.d/local-ci.conf');
  const callsFile = path.join(directory, 'calls.jsonl');
  const githubEnv = path.join(directory, 'github-env');
  const helper = path.join(directory, 'helper.sh');
  await Promise.all([mkdir(bin), mkdir(temporary), writeFile(callsFile, ''), writeFile(githubEnv, '')]);
  t.after(async () => {
    try {
      const pid = Number(await readFile(path.join(temporary, 'local-ci-docker/journald.pid'), 'utf8'));
      process.kill(pid, 'SIGTERM');
    } catch (error) {
      if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error;
    }
    await rm(directory, { recursive: true, force: true });
  });
  if (!options.noContainer) await writeFile(path.join(directory, 'container'), 'fixture');
  // Execute the real shell flow with fixed system paths redirected into this
  // fixture. No daemon, privilege escalation, cgroup or host path is touched.
  let source = await readFile(path.join(root, 'build/ci-local-docker.sh'), 'utf8');
  for (const [original, replacement] of [
    ['/lib/systemd/systemd-journald', path.join(bin, 'journald')],
    ['/usr/local/bin', bin],
    ['/usr/local/lib/docker/cli-plugins', path.join(directory, 'plugins')],
    ['/run/systemd', path.join(directory, 'systemd')],
    ['/run/log/journal', journalLogs],
    ['/var/run/docker.sock', path.join(directory, 'docker.sock')],
    ['/run/local-ci-docker', path.join(directory, 'docker-state')],
    ['/sys/fs/cgroup', path.join(directory, 'cgroup')],
    ['/.dockerenv', path.join(directory, 'container')],
    ['/run/.containerenv', path.join(directory, 'container-other')],
  ]) source = source.replaceAll(original, replacement);
  const dispatcher = path.join(directory, 'tool.cjs');
  await writeFile(dispatcher, `
    const fs = require('node:fs');
    const net = require('node:net');
    const path = require('node:path');
    const [tool, ...args] = process.argv.slice(2);
    const settings = ${JSON.stringify(options)};
    fs.appendFileSync(${JSON.stringify(callsFile)}, JSON.stringify({tool, args, env: process.env}) + '\\n');
    if (tool === 'uname') console.log(args[0] === '-m' ? 'aarch64' : 'Linux');
    else if (tool === 'dockerd') {
      if (args[0] === '--version') console.log(settings.wrongDocker ? 'wrong daemon' : 'Docker version 29.8.2, build 8af9fe3');
    } else if (tool === 'journald') {
      if (settings.journal === 'exit') process.exit(55);
      if (settings.journal === 'stall') setInterval(() => {}, 1000);
      else {
        fs.mkdirSync(${JSON.stringify(journal)}, {recursive: true});
        net.createServer().listen(path.join(${JSON.stringify(journal)}, 'socket'));
      }
    } else if (tool === 'docker') {
      if (args.includes('info')) {
        if (!fs.statSync(path.join(${JSON.stringify(journal)}, 'socket')).isSocket()) process.exit(91);
        if (settings.journal === 'die-during-docker') {
          process.kill(Number(fs.readFileSync(${JSON.stringify(path.join(temporary, 'local-ci-docker/journald.pid'))}, 'utf8')), 'SIGTERM');
          Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100);
        }
      } else if (args.includes('buildx')) console.log('github.com/docker/buildx v0.37.2 revision');
      else if (args.includes('version')) console.log(JSON.stringify({ApiVersion: '1.48', Version: settings.wrongServer ? '29.7.2' : '29.8.2', Components: [{Name: 'containerd', Version: settings.wrongComponent ? 'v2.3.6' : 'v2.4.1'}, {Name: 'runc', Version: '1.5.2'}]}));
      else process.exit(92);
    } else if (tool === 'curl') {
      const output = args[args.indexOf('--output') + 1];
      if (settings.wrongChecksum) fs.writeFileSync(output, 'corrupt archive');
      else {
        const name = args.some(arg => arg.includes('github.com/containerd/')) ? 'containerd.tgz'
          : args.some(arg => arg.includes('github.com/docker/buildx/')) ? 'docker-buildx' : 'docker.tgz';
        fs.copyFileSync(path.join(${JSON.stringify(directory)}, name), output);
      }
    } else process.exit(93);
  `);
  for (const tool of ['uname', 'dockerd', 'docker', 'journald', 'curl']) {
    if (tool === 'journald' && options.missingJournal) continue;
    const file = path.join(bin, tool);
    await writeFile(file, `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(dispatcher)} ${quote(tool)} "$@"\n`);
    await chmod(file, 0o755);
  }
  for (const [tool, script] of [
    ['sudo', 'exec "$@"'],
    ['timeout', 'while [ "${1#--}" != "$1" ]; do shift; done\nshift\nexec "$@"'],
    ['sleep', 'exec /bin/sleep 0.01'],
    ['sha256sum', 'exec shasum -a 256 -c -s'],
  ]) {
    await writeFile(path.join(bin, tool), `#!/bin/sh\n${script}\n`);
    await chmod(path.join(bin, tool), 0o755);
  }
  const archiveRoot = path.join(directory, 'archive');
  await mkdir(path.join(archiveRoot, 'docker'), { recursive: true });
  for (const tool of ['dockerd', 'docker', 'containerd', 'containerd-shim-runc-v2', 'ctr', 'docker-init', 'docker-proxy', 'runc']) {
    const content = ['dockerd', 'docker'].includes(tool)
      ? await readFile(path.join(bin, tool)) : '#!/bin/sh\nexit 0\n';
    await writeFile(path.join(archiveRoot, 'docker', tool), content);
  }
  const pack = spawn('/usr/bin/tar', ['-czf', path.join(directory, 'docker.tgz'), '-C', archiveRoot, 'docker']);
  const [packStatus] = await once(pack, 'close');
  assert.equal(packStatus, 0);
  const digest = createHash('sha256').update(await readFile(path.join(directory, 'docker.tgz'))).digest('hex');
  source = source.replaceAll(/docker_sha256=[a-f0-9]{64}/g, `docker_sha256=${digest}`);
  await mkdir(path.join(archiveRoot, 'bin'));
  for (const tool of ['containerd', 'containerd-shim-runc-v2', 'ctr']) {
    await writeFile(path.join(archiveRoot, 'bin', tool), '#!/bin/sh\nexit 0\n');
  }
  const componentPack = spawn('/usr/bin/tar', ['-czf', path.join(directory, 'containerd.tgz'), '-C', archiveRoot, 'bin']);
  const [componentStatus] = await once(componentPack, 'close');
  assert.equal(componentStatus, 0);
  await writeFile(path.join(directory, 'docker-buildx'), '#!/bin/sh\nexit 0\n');
  for (const [key, filename] of [['containerd', 'containerd.tgz'], ['buildx', 'docker-buildx']]) {
    const componentDigest = createHash('sha256').update(await readFile(path.join(directory, filename))).digest('hex');
    source = source.replaceAll(new RegExp(`${key}_sha256=[a-f0-9]{64}`, 'g'), `${key}_sha256=${componentDigest}`);
  }
  await writeFile(helper, source);
  return {
    directory, temporary, journal, journalLogs, config, githubEnv,
    async calls() { return (await readFile(callsFile, 'utf8')).split('\n').filter(Boolean).map(JSON.parse); },
    async run(env = {}) {
      const child = spawn('/bin/bash', [helper], {
        cwd: root,
        env: { PATH: `${bin}:${process.env.PATH}`, RUNNER_TEMP: temporary, GITHUB_ENV: githubEnv,
          ACT: 'true', LOCAL_CI_DISPOSABLE: '1', ...env },
      });
      let output = '';
      child.stdout.on('data', data => { output += data; });
      child.stderr.on('data', data => { output += data; });
      const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
      const [status, signal] = await once(child, 'close');
      clearTimeout(timer);
      assert.equal(signal, null, output);
      return { status, output };
    },
  };
}

test('private Docker starts a bounded real-journal command before its daemon and clears inherited activation', async t => {
  const f = await fixture(t);
  const result = await f.run({ LISTEN_FDS: '99', LISTEN_PID: '1', NOTIFY_SOCKET: '/host/socket',
    RUNTIME_DIRECTORY: '/host/journal', LOGS_DIRECTORY: '/host/logs' });
  assert.equal(result.status, 0, result.output);
  const calls = await f.calls();
  const journal = calls.findIndex(call => call.tool === 'journald');
  const daemon = calls.findIndex(call => call.tool === 'dockerd' && !call.args.includes('--version'));
  assert.ok(journal >= 0 && daemon > journal);
  for (const name of ['LISTEN_FDS', 'LISTEN_PID', 'NOTIFY_SOCKET', 'RUNTIME_DIRECTORY', 'LOGS_DIRECTORY']) {
    assert.equal(calls[journal].env[name], undefined);
  }
  assert.equal(await readFile(f.config, 'utf8'), '[Journal]\nStorage=volatile\nRuntimeMaxUse=32M\nReadKMsg=no\n');
  assert.equal((await stat(path.join(f.temporary, 'local-ci-docker/journald.log'))).mode & 0o777, 0o600);
  assert.match(await readFile(f.githubEnv, 'utf8'), /DOCKER_HOST=unix:/);
});

test('existing journal state and dangling links are refused before daemon startup', async t => {
  for (const name of ['journal', 'journalLogs', 'config']) {
    const f = await fixture(t);
    await mkdir(path.dirname(f[name]), { recursive: true });
    await symlink(path.join(f.directory, 'absent'), f[name]);
    const result = await f.run();
    assert.equal(result.status, 2, result.output);
    assert.match(result.output, /Existing journal state was left untouched/);
    assert.equal((await f.calls()).some(call => ['journald', 'dockerd'].includes(call.tool)), false);
  }
});

test('container, image and binary prerequisites fail before journal startup', async t => {
  for (const settings of [{ noContainer: true }, { missingJournal: true }, { wrongDocker: true }]) {
    const f = await fixture(t, settings);
    const result = await f.run();
    assert.equal(result.status, 2, result.output);
    assert.equal((await f.calls()).some(call => call.tool === 'journald'), false);
    assert.equal(await readFile(f.githubEnv, 'utf8'), '');
  }
});

test('exited or unready journal cannot start Docker or publish readiness', async t => {
  for (const journal of ['exit', 'stall']) {
    const f = await fixture(t, { journal });
    const result = await f.run();
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /job-owned journal did not become ready/);
    assert.equal((await f.calls()).some(call => call.tool === 'dockerd' && !call.args.includes('--version')), false);
    assert.equal(await readFile(f.githubEnv, 'utf8'), '');
  }
});

test('journal loss during Docker startup cannot publish a usable daemon', async t => {
  const f = await fixture(t, { journal: 'die-during-docker' });
  const result = await f.run();
  assert.equal(result.status, 1, result.output);
  assert.match(result.output, /journal exited during Docker startup/);
  assert.equal(await readFile(f.githubEnv, 'utf8'), '');
});


test('a corrupt Docker download never starts the journal or daemon', async t => {
  const f = await fixture(t, { wrongChecksum: true });
  const result = await f.run();
  assert.notEqual(result.status, 0, result.output);
  assert.equal((await f.calls()).some(call => ['journald', 'dockerd'].includes(call.tool)), false);
  assert.equal(await readFile(f.githubEnv, 'utf8'), '');
});

test('an unexpected running daemon version cannot publish readiness', async t => {
  const f = await fixture(t, { wrongServer: true });
  const result = await f.run();
  assert.notEqual(result.status, 0, result.output);
  assert.match(result.output, /running Docker daemon differs/);
  assert.equal(await readFile(f.githubEnv, 'utf8'), '');
});


test('an outdated active containerd cannot publish runtime readiness', async t => {
  const f = await fixture(t, { wrongComponent: true });
  const result = await f.run();
  assert.notEqual(result.status, 0, result.output);
  assert.match(result.output, /running Docker component differs/);
  assert.equal(await readFile(f.githubEnv, 'utf8'), '');
});
