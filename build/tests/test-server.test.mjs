import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { chmod, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
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
  let timedOut = false;
  const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); }, 10000);
  const [status, signal] = await once(child, 'close');
  clearTimeout(timer);
  assert.equal(timedOut, false, `Helper timed out: ${output}`);
  assert.equal(signal, null, output);
  return { status, output, artifacts };
}

async function shutdownReport(result) {
  const file = path.join(result.artifacts, 'server-shutdown.json');
  assert.equal((await stat(file)).mode & 0o777, 0o600);
  const report = JSON.parse(await readFile(file, 'utf8'));
  assert.equal(report.schemaVersion, 1);
  assert.ok(Date.parse(report.finishedAt));
  return report;
}

async function shutdownFixture(t, behavior) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-shutdown-test.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const server = path.join(directory, 'shutdown-server.mjs');
  await writeFile(server, `#!/usr/bin/env node
    await import(${JSON.stringify(pathToFileURL(fixture).href)});
    process.removeAllListeners('SIGTERM');
    ${behavior}
  `);
  await chmod(server, 0o755);
  return { TEST_SERVER_BINARY: server };
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
  const report = await shutdownReport(result);
  assert.equal(report.passed, true, 'Shutdown succeeded even though the command failed');
  assert.equal(report.statusBeforeCleanup, 23);
  assert.equal(report.waitStatus, 0);
});

test('helper reports actual graceful exit separately from its requested TERM', async t => {
  const result = await run(t);
  assert.equal(result.status, 0, result.output);
  const report = await shutdownReport(result);
  assert.equal(report.statusBeforeCleanup, 0);
  assert.equal(report.termAttempted, true);
  assert.equal(report.termSent, true);
  assert.equal(report.killAttempted, false);
  assert.equal(report.killSent, false);
  assert.equal(report.waitStatus, 0);
  assert.equal(report.waitStatusInterpretation, 'exited_zero');
  assert.equal(report.passed, true);
  assert.throws(() => process.kill(report.serverPid, 0), { code: 'ESRCH' });
});

for (const [name, behavior, waitStatus, interpretation] of [
  ['nonzero exit', "process.on('SIGTERM', () => process.exit(17));", 17, 'nonzero_exit'],
  ['signal-only exit', '', 143, 'signal_or_high_exit_code'],
  ['explicit high exit', "process.on('SIGTERM', () => process.exit(143));", 143, 'signal_or_high_exit_code'],
  ['forced termination', "process.on('SIGTERM', () => {});", 137, 'signal_or_high_exit_code'],
]) {
  test(`helper fails ${name} and retains the raw wait status`, async t => {
    const result = await run(t, await shutdownFixture(t, behavior));
    assert.equal(result.status, 1, result.output);
    const report = await shutdownReport(result);
    assert.equal(report.statusBeforeCleanup, 0);
    assert.equal(report.termAttempted, true);
    assert.equal(report.termSent, true);
    assert.equal(report.killAttempted, name === 'forced termination');
    assert.equal(report.killSent, name === 'forced termination');
    assert.equal(report.waitStatus, waitStatus);
    assert.equal(report.waitStatusInterpretation, interpretation);
    assert.equal(report.passed, false);
    assert.equal(Object.hasOwn(report, 'signal'), false, 'A requested TERM does not identify the exit signal');
    assert.throws(() => process.kill(report.serverPid, 0), { code: 'ESRCH' });
  });
}

for (const [name, behavior] of [
  ['nonzero exit', "process.on('SIGTERM', () => process.exit(17));"],
  ['forced termination', "process.on('SIGTERM', () => {});"],
]) {
  test(`helper preserves an existing command failure after ${name}`, async t => {
    const result = await run(t, await shutdownFixture(t, behavior), [process.execPath, '-e', 'process.exit(23)']);
    assert.equal(result.status, 23, result.output);
    const report = await shutdownReport(result);
    assert.equal(report.statusBeforeCleanup, 23);
    assert.equal(report.passed, false);
    assert.throws(() => process.kill(report.serverPid, 0), { code: 'ESRCH' });
  });
}

for (const statusBeforeCleanup of [0, 23]) {
  test(`helper refuses to overwrite shutdown evidence and preserves status ${statusBeforeCleanup}`, async t => {
    const result = await run(t, {}, [process.execPath, '-e', `
      const fs = require('node:fs');
      const path = require('node:path');
      fs.writeFileSync(path.join(process.env.E2E_ARTIFACTS, 'server-shutdown.json'), 'prior owned fixture evidence');
      process.exit(${statusBeforeCleanup});
    `]);
    assert.equal(result.status, statusBeforeCleanup || 1, result.output);
    assert.match(result.output, /Could not retain the owned test server shutdown report/);
    assert.equal(await readFile(path.join(result.artifacts, 'server-shutdown.json'), 'utf8'), 'prior owned fixture evidence');
    const log = await readFile(path.join(result.artifacts, 'server.log'), 'utf8');
    const pid = Number(log.match(/FIXTURE_PID=(\d+)/)?.[1]);
    assert.ok(pid > 0, log);
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
  });
}

for (const [pid, waitStatus, interpretation] of [
  ['', '', 'not_waited'],
  ['12345', '127', 'unavailable_or_exit_127'],
]) {
  test(`cleanup rejects ${interpretation} without inventing successful evidence`, async t => {
    const artifacts = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-cleanup-status-test.'));
    t.after(() => rm(artifacts, { recursive: true, force: true }));
    const source = await readFile(helper, 'utf8');
    const start = source.indexOf('cleanup() {');
    const end = source.indexOf('\ntrap cleanup EXIT', start);
    assert.ok(start >= 0 && end > start);
    // Exercise the real cleanup function with unavailable shell bookkeeping.
    // All kill/wait operations are intercepted; the fixture PID is never signaled.
    const child = spawn('/bin/bash', ['-c', `${source.slice(start, end)}
      server_pid="$1"
      test_artifacts="$2"
      fixture_wait_status="$3"
      kill() { return 1; }
      wait() { return "$fixture_wait_status"; }
      true
      cleanup
    `, 'cleanup-fixture', pid, artifacts, waitStatus], { env: { PATH: process.env.PATH } });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    const [status, signal] = await once(child, 'close');
    assert.equal(signal, null, output);
    assert.equal(status, 1, output);
    const report = await shutdownReport({ artifacts });
    assert.equal(report.waitStatus, waitStatus === '' ? null : Number(waitStatus));
    assert.equal(report.waitStatusInterpretation, interpretation);
    assert.equal(report.passed, false);
  });
}

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
