import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';

const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
const helper = path.join(repoRoot, 'build/with-test-server.sh');
const fixture = fileURLToPath(new URL('./fixture-server.mjs', import.meta.url));

async function unusedPort(protocol = 'tcp') {
  if (protocol === 'udp') {
    const socket = dgram.createSocket('udp4').bind(0, '0.0.0.0');
    await once(socket, 'listening');
    const port = socket.address().port;
    await new Promise(resolve => socket.close(resolve));
    return port;
  }
  const server = net.createServer().listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

async function run(t, environment = {}, command = [process.execPath, '-e', 'process.exit(0)']) {
  const artifacts = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-helper-test.'));
  t.after(() => rm(artifacts, { recursive: true, force: true }));
  const child = spawn('bash', [helper, ...command], {
    cwd: repoRoot,
    env: {
      PATH: process.env.PATH,
      DISPOSABLE_TEST_DATABASE: '1',
      DATABASE_URL: 'postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable',
      TEST_SERVER_BINARY: fixture,
      TEST_SERVER_PORT: String(await unusedPort()),
      TEST_MEDIA_PORT: String(await unusedPort('udp')),
      E2E_ARTIFACTS: artifacts,
      ...environment,
    },
  });
  let output = '';
  child.stdout.on('data', chunk => { output += chunk; });
  child.stderr.on('data', chunk => { output += chunk; });
  const timer = setTimeout(() => child.kill('SIGTERM'), 10000);
  const [status, signal] = await once(child, 'close');
  clearTimeout(timer);
  assert.equal(signal, null, output);
  return { status, output, artifacts };
}

async function announcementFixture(t, expectedAddress) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-announcement-test.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const networkMock = path.join(directory, 'network-interfaces.cjs');
  // The mock applies to the Node preflight, not the env-isolated fake server.
  // No production bypass or test-only environment hook is added to the helper.
  await writeFile(networkMock, `
    require('node:os').networkInterfaces = () => ({
      loopback: [{ address: '127.0.0.1', family: 'IPv4', internal: true }],
      fixture: [{ address: '192.0.2.73', family: 'IPv4', internal: false }],
      unavailable: undefined,
    });
  `);
  const server = path.join(directory, 'announcement-server.mjs');
  await writeFile(server, `#!/usr/bin/env node
    import assert from 'node:assert/strict';
    assert.equal(process.env.ANNOUNCE_IP, ${JSON.stringify(expectedAddress)});
    assert.equal(process.env.BIND_ADDR, '127.0.0.1');
    assert.equal(process.env.ALLOWED_ORIGINS, 'http://127.0.0.1:' + process.env.PORT);
    assert.equal(new URL(process.env.DATABASE_URL).hostname, '127.0.0.1');
    assert.equal(process.env.NODE_OPTIONS, undefined);
    await import(${JSON.stringify(pathToFileURL(fixture).href)});
  `);
  await chmod(server, 0o755);
  return { NODE_OPTIONS: `--require ${JSON.stringify(networkMock)}`, TEST_SERVER_BINARY: server };
}

function provenanceCommand(expectedAddress) {
  return [process.execPath, '-e', `
    const assert = require('node:assert/strict');
    assert.equal(process.env.TEST_ANNOUNCE_IP, ${JSON.stringify(expectedAddress)});
    assert.equal(new URL(process.env.BASE_URL).hostname, '127.0.0.1');
    assert.equal(new URL(process.env.TEST_DATABASE_URL).hostname, '127.0.0.1');
  `];
}

test('helper requires disposable opt-in and rejects nonlocal/effective-host overrides', async t => {
  assert.equal((await run(t, { DISPOSABLE_TEST_DATABASE: '0' })).status, 2);
  for (const database of [
    'postgres://test_owner@example.test/simplestchat_test',
    'postgres://test_owner@127.0.0.1/production',
    'postgres://test_owner@127.0.0.1/simplestchat_test?hostaddr=203.0.113.10&sslmode=disable',
  ]) {
    assert.equal((await run(t, { DATABASE_URL: database })).status, 2);
  }
});

test('helper rejects empty, nonliteral, and unassigned media announcements before spawning', async t => {
  const environment = await announcementFixture(t, 'must-not-launch');
  for (const address of ['', 'localhost', '127.0.0.1.example.test', '::1', '127.1', ' 127.0.0.1',
    '0.0.0.0', '127.0.0.2', '203.0.113.77']) {
    const result = await run(t, { ...environment, TEST_ANNOUNCE_IP: address });
    assert.equal(result.status, 2, `${JSON.stringify(address)}: ${result.output}`);
    assert.match(result.output, /Refusing TEST_ANNOUNCE_IP/);
    await assert.rejects(readFile(path.join(result.artifacts, 'server.log')), { code: 'ENOENT' });
  }
});

test('helper retains the loopback default and accepts explicit loopback announcement', async t => {
  const environment = await announcementFixture(t, '127.0.0.1');
  for (const override of [{}, { TEST_ANNOUNCE_IP: '127.0.0.1' }]) {
    const result = await run(t, { ...environment, ...override }, provenanceCommand('127.0.0.1'));
    assert.equal(result.status, 0, result.output);
  }
});

test('helper accepts an interface-owned IPv4 announcement without widening HTTP or database access', async t => {
  const environment = await announcementFixture(t, '192.0.2.73');
  const result = await run(t, { ...environment, TEST_ANNOUNCE_IP: '192.0.2.73' }, provenanceCommand('192.0.2.73'));
  assert.equal(result.status, 0, result.output);
  const log = await readFile(path.join(result.artifacts, 'server.log'), 'utf8');
  const pid = Number(log.match(/FIXTURE_PID=(\d+)/)?.[1]);
  assert.ok(pid > 0, log);
  assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
});

test('helper refuses an existing non-HTTP listener without stopping it', async t => {
  const server = net.createServer().listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const result = await run(t, { TEST_SERVER_PORT: String(server.address().port) });
  assert.equal(result.status, 2, result.output);
  assert.match(result.output, /Refusing unavailable test ports/);
  assert.equal(server.listening, true);
});

test('helper refuses an occupied media port without closing its owner', async t => {
  const socket = dgram.createSocket('udp4').bind(0, '0.0.0.0');
  await once(socket, 'listening');
  t.after(() => new Promise(resolve => socket.close(resolve)));
  const port = socket.address().port;
  const result = await run(t, { TEST_MEDIA_PORT: String(port) });
  assert.equal(result.status, 2, result.output);
  assert.match(result.output, /Refusing unavailable test ports/);
  assert.equal(socket.address().port, port);
});

test('helper isolates server configuration and preserves command failures while cleaning its child', async t => {
  const result = await run(t, {
    WEBAUTHN_ORIGIN: 'https://deployment.example.test',
    TURN_URLS: 'turn:deployment.example.test',
    AUTH_MAX_CONCURRENCY: '0',
    MAX_USERS: '0',
  }, [process.execPath, '-e', 'process.exit(23)']);
  assert.equal(result.status, 23, result.output);
  const log = await readFile(path.join(result.artifacts, 'server.log'), 'utf8');
  const pid = Number(log.match(/FIXTURE_PID=(\d+)/)?.[1]);
  assert.ok(pid > 0, log);
  assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
});

test('helper reports an early server exit as failure', async t => {
  const result = await run(t, { TEST_SERVER_BINARY: '/usr/bin/false' });
  assert.equal(result.status, 1, result.output);
  assert.match(result.output, /exited before readiness/);
});

test('a successful command cannot hide a server exit during the test', async t => {
  const command = `
    const fs = require('node:fs');
    const path = require('node:path');
    const log = fs.readFileSync(path.join(process.env.E2E_ARTIFACTS, 'server.log'), 'utf8');
    const pid = Number(log.match(/FIXTURE_PID=(\\d+)/)[1]);
    process.kill(pid, 'SIGTERM');
    setTimeout(() => process.exit(0), 200);
  `;
  const result = await run(t, {}, [process.execPath, '-e', command]);
  assert.equal(result.status, 1, result.output);
  assert.match(result.output, /exited while the test command was running/);
});
