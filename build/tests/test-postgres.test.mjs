import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const helper = path.join(root, 'build/with-test-postgres.sh');
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

async function unusedPort(udp = false) {
  const socket = udp ? dgram.createSocket('udp4').bind(0, '0.0.0.0') : net.createServer().listen(0, '127.0.0.1');
  await once(socket, 'listening');
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  return port;
}

// PostgreSQL tools are always intercepted. They record command contracts and
// simulate an owned cluster with a JSON state file, never a real database.
async function fixture(t, options = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-postgres-fixture.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = path.join(directory, 'fake bin');
  const temporary = path.join(directory, 'runner temp');
  const log = path.join(directory, 'commands.jsonl');
  const stateFile = path.join(directory, 'state.json');
  const dispatcher = path.join(directory, 'postgres-tool.cjs');
  await Promise.all([mkdir(bin), mkdir(temporary), writeFile(log, ''), writeFile(stateFile, '{}')]);
  await writeFile(dispatcher, `
    const fs = require('node:fs');
    const path = require('node:path');
    const [command, ...args] = process.argv.slice(2);
    const settings = ${JSON.stringify(options)};
    const stateFile = ${JSON.stringify(stateFile)};
    const state = JSON.parse(fs.readFileSync(stateFile, 'utf8'));
    const save = () => fs.writeFileSync(stateFile, JSON.stringify(state));
    fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ command, args, env: process.env }) + '\\n');
    const value = name => args[args.indexOf(name) + 1];
    const fail = phase => { if (settings.fail === phase) process.exit(37); };
    if (command === 'initdb') {
      fail('initdb');
      state.data = value('-D');
      fs.mkdirSync(state.data);
      fs.writeFileSync(path.join(state.data, 'PG_VERSION'), 'fixture only');
      save();
    } else if (command === 'pg_ctl') {
      if (value('-D') !== state.data) process.exit(91);
      if (args.includes('start')) {
        fs.writeFileSync(value('-l'), 'fixture postgres log\\n');
        fail('start');
        state.running = true; save();
        fail('start-partial');
      } else if (args.includes('status')) {
        process.exit(state.running ? 0 : 3);
      } else if (args.includes('stop')) {
        if (settings.fail === 'stop-all' || (settings.fail === 'stop-fast' && value('-m') === 'fast')) process.exit(37);
        state.running = false; save();
      } else process.exit(92);
    } else if (command === 'createdb') {
      fail('createdb');
      if (!state.running) process.exit(93);
    } else process.exit(94);
  `);
  for (const tool of ['initdb', 'pg_ctl', 'createdb']) {
    const file = path.join(bin, tool);
    await writeFile(file, `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(dispatcher)} ${quote(tool)} "$@"\n`);
    await chmod(file, 0o755);
  }
  const port = await unusedPort();
  return {
    directory, temporary, log, stateFile, port,
    async records() { return (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line)); },
    async state() { return JSON.parse(await readFile(stateFile, 'utf8')); },
    async run({ env = {}, command = [process.execPath, '-e', 'process.exit(0)'], signal } = {}) {
      // /bin/bash is Bash 3.2 on macOS, exercising the oldest supported shell.
      const child = spawn('/bin/bash', [helper, ...command], {
        cwd: root,
        env: { PATH: `${bin}:${process.env.PATH}`, RUNNER_TEMP: temporary, TEST_POSTGRES_PORT: String(port), ...env },
      });
      let output = '', signaled = false, timedOut = false;
      const collect = chunk => {
        output += chunk;
        if (signal && !signaled && output.includes('FIXTURE_CHILD_READY')) {
          signaled = true;
          child.kill(signal);
        }
      };
      child.stdout.on('data', collect); child.stderr.on('data', collect);
      const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); }, 25000);
      const [status, exitSignal] = await once(child, 'close');
      clearTimeout(timer);
      assert.equal(timedOut, false, `Helper timed out: ${output}`);
      assert.equal(exitSignal, null, output);
      if (signal) assert.equal(signaled, true, output);
      return { status, output };
    },
  };
}

test('PostgreSQL wrapper starts a private cluster and exports only its local database', async t => {
  const f = await fixture(t);
  const result = await f.run({
    env: {
      DATABASE_URL: 'postgres://remote.example/production', TEST_DATABASE_URL: 'postgres://remote.example/production',
      DISPOSABLE_TEST_DATABASE: '0', PGHOST: 'remote.example', PGHOSTADDR: '192.0.2.1', PGPORT: '5432',
      PGSERVICE: 'production', PGSERVICEFILE: '/does/not/exist', PGPASSFILE: '/does/not/exist',
      PGDATABASE: 'production', PGUSER: 'production', PGOPTIONS: '-c search_path=wrong', PGDATA: '/do/not/touch',
      PGSSLMODE: 'require', PGCLIENTENCODING: 'LATIN1', KEEP_TEST_SETTING: 'preserved',
    },
    command: [process.execPath, '-e', `
      const assert = require('node:assert/strict');
      const expected = 'postgres://test_owner@127.0.0.1:${f.port}/simplestchat_test?sslmode=disable';
      assert.equal(process.env.DATABASE_URL, expected);
      assert.equal(process.env.TEST_DATABASE_URL, expected);
      assert.equal(process.env.DISPOSABLE_TEST_DATABASE, '1');
      assert.equal(process.env.KEEP_TEST_SETTING, 'preserved');
      assert.deepEqual(Object.keys(process.env).filter(key => key.startsWith('PG')), []);
    `],
  });
  assert.equal(result.status, 0, result.output);
  const records = await f.records();
  for (const record of records) {
    assert.deepEqual(Object.keys(record.env).filter(key => key.startsWith('PG')), []);
    assert.equal(record.env.DATABASE_URL, undefined);
    assert.equal(record.env.LC_ALL, 'C');
  }
  const state = await f.state();
  assert.equal(state.running, false);
  assert.equal(path.dirname(path.dirname(state.data)), f.temporary);
  const cluster = path.dirname(state.data);
  assert.match(path.basename(cluster), /^simplestchat-postgres\.[A-Za-z0-9]{8}$/);
  assert.equal((await stat(cluster)).mode & 0o777, 0o700);
  for (const name of ['initdb.log', 'postgres.log']) {
    assert.equal((await stat(path.join(cluster, name))).mode & 0o777, 0o600);
  }
  assert.deepEqual(records.find(record => record.command === 'initdb').args,
    ['-D', state.data, '-U', 'test_owner', '--auth-local=reject', '--auth-host=trust', '--encoding=UTF8', '--no-locale']);
  const start = records.find(record => record.args.includes('start'));
  assert.equal(start.args[start.args.indexOf('-o') + 1], `-h 127.0.0.1 -p ${f.port} -c unix_socket_directories=''`);
  assert.deepEqual(records.find(record => record.command === 'createdb').args,
    ['-h', '127.0.0.1', '-p', String(f.port), '-U', 'test_owner', '--maintenance-db=postgres', 'simplestchat_test']);
  assert.match(result.output, /Disposable PostgreSQL logs:/);
});

test('PostgreSQL wrapper rejects missing commands and invalid ports before touching PostgreSQL', async t => {
  const f = await fixture(t);
  assert.equal((await f.run({ command: [] })).status, 2);
  for (const value of ['', '0', '999', '65536', '-1', '015434', '15434;echo bad', 'remote:5432']) {
    assert.equal((await f.run({ env: { TEST_POSTGRES_PORT: value } })).status, 2, value);
  }
  assert.deepEqual(await f.records(), []);
  assert.deepEqual(await readdir(f.temporary), []);
});

test('PostgreSQL wrapper refuses an occupied port without stopping its listener', async t => {
  const server = net.createServer().listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const f = await fixture(t);
  const result = await f.run({ env: { TEST_POSTGRES_PORT: String(server.address().port) } });
  assert.equal(result.status, 2, result.output);
  assert.match(result.output, /Refusing unavailable PostgreSQL test port/);
  assert.equal(server.listening, true);
  assert.deepEqual(await f.records(), []);
});

for (const phase of ['initdb', 'start', 'start-partial', 'createdb']) {
  test(`PostgreSQL wrapper preserves ${phase} failure and stops only a partially started cluster`, async t => {
    const f = await fixture(t, { fail: phase });
    const result = await f.run();
    assert.equal(result.status, 37, result.output);
    const stops = (await f.records()).filter(record => record.args.includes('stop'));
    assert.equal(stops.length, ['start-partial', 'createdb'].includes(phase) ? 1 : 0);
    assert.notEqual((await f.state()).running, true);
  });
}

for (const failure of [undefined, 'stop-fast']) {
  test(`PostgreSQL wrapper preserves child failure${failure ? ' even if fast shutdown fails' : ''}`, async t => {
    const f = await fixture(t, { fail: failure });
    const result = await f.run({ command: [process.execPath, '-e', 'process.exit(23)'] });
    assert.equal(result.status, 23, result.output);
    assert.equal((await f.state()).running, false);
    const stops = (await f.records()).filter(record => record.args.includes('stop'));
    assert.equal(stops.length, failure ? 2 : 1);
  });
}

test('PostgreSQL cleanup failure cannot turn a successful child into a successful run', async t => {
  const f = await fixture(t, { fail: 'stop-fast' });
  const result = await f.run();
  assert.equal(result.status, 1, result.output);
  assert.match(result.output, /Fast shutdown failed/);
  assert.equal((await f.state()).running, false);
});

test('PostgreSQL disappearing during a successful child still fails the run', async t => {
  const f = await fixture(t);
  const result = await f.run({ command: [process.execPath, '-e', `
    const fs = require('node:fs');
    const file = ${JSON.stringify(f.stateFile)};
    const state = JSON.parse(fs.readFileSync(file));
    state.running = false; fs.writeFileSync(file, JSON.stringify(state));
  `] });
  assert.equal(result.status, 1, result.output);
  assert.match(result.output, /Owned PostgreSQL exited/);
});

for (const [signal, expected] of [['SIGTERM', 143], ['SIGINT', 130]]) {
  test(`PostgreSQL wrapper forwards ${signal} cleanup to its test child and preserves signal status`, async t => {
    const f = await fixture(t);
    const childPidFile = path.join(f.directory, 'child.pid');
    const result = await f.run({ signal, command: [process.execPath, '-e', `
      require('node:fs').writeFileSync(${JSON.stringify(childPidFile)}, String(process.pid));
      process.on('SIGTERM', () => process.exit(0));
      console.log('FIXTURE_CHILD_READY'); setInterval(() => {}, 1000);
    `] });
    assert.equal(result.status, expected, result.output);
    const pid = Number(await readFile(childPidFile, 'utf8'));
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
    assert.equal((await f.state()).running, false);
  });
}

test('PostgreSQL wrapper terminates an ordinary nested test-server helper and only its fixture server', async t => {
  const f = await fixture(t);
  const artifacts = path.join(f.directory, 'browser artifacts');
  const result = await f.run({
    signal: 'SIGTERM',
    env: {
      TEST_SERVER_BINARY: path.join(root, 'build/tests/fixture-server.mjs'),
      TEST_SERVER_PORT: String(await unusedPort()), TEST_MEDIA_PORT: String(await unusedPort(true)),
      E2E_ARTIFACTS: artifacts,
    },
    command: [path.join(root, 'build/with-test-server.sh'), process.execPath, '-e',
      "console.log('FIXTURE_CHILD_READY'); setInterval(() => {}, 1000);"],
  });
  assert.equal(result.status, 143, result.output);
  const log = await readFile(path.join(artifacts, 'server.log'), 'utf8');
  const pid = Number(log.match(/FIXTURE_PID=(\d+)/)?.[1]);
  assert.ok(pid > 0, log);
  assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  assert.equal((await f.state()).running, false);
});
