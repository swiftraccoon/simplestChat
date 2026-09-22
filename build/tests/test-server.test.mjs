import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { chmod, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
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

async function turnFixture(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-turn-fixture.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const turn = path.join(directory, 'turnserver');
  await writeFile(turn, `#!/usr/bin/env node
    import assert from 'node:assert/strict';
    import fs from 'node:fs';
    import net from 'node:net';
    import dgram from 'node:dgram';
    if (process.argv[2] === '--version') { console.log('4.18.0'); process.exit(0); }
    assert.equal(process.env.TURN_SECRET, undefined);
    assert.equal(process.env.NODE_OPTIONS, undefined);
    assert.equal(process.argv[2], '-c');
    const file = process.argv[3];
    assert.equal(fs.statSync(file).mode & 0o777, 0o600);
    const config = Object.fromEntries(fs.readFileSync(file, 'utf8').trim().split('\\n').map(line => {
      const separator = line.indexOf('=');
      return separator < 0 ? [line, true] : [line.slice(0, separator), line.slice(separator + 1)];
    }));
    assert.equal(config['listening-ip'], '127.0.0.1');
    assert.equal(config['relay-ip'], '127.0.0.1');
    assert.equal(config['allowed-peer-ip'], '127.0.0.1');
    assert.equal(config['denied-peer-ip'], '::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff');
    assert.equal(config['use-auth-secret'], true);
    assert.match(config['static-auth-secret'], /^[a-f0-9]{64}$/);
    const tcp = net.createServer(socket => socket.end()).listen(Number(config['listening-port']), '127.0.0.1');
    const udp = dgram.createSocket('udp4').bind(Number(config['listening-port']), '127.0.0.1');
    console.log('FIXTURE_TURN_PID=' + process.pid);
    process.on('SIGTERM', () => { tcp.close(); udp.close(); });
  `);
  await chmod(turn, 0o755);
  const server = path.join(directory, 'server.mjs');
  const port = await unusedPort();
  await writeFile(server, `#!/usr/bin/env node
    import assert from 'node:assert/strict';
    assert.equal(process.env.TURN_URLS, 'turn:127.0.0.1:${port}?transport=udp');
    assert.equal(process.env.TURN_TTL, '60');
    assert.match(process.env.TURN_SECRET, /^[a-f0-9]{64}$/);
    delete process.env.TURN_URLS;
    await import(${JSON.stringify(pathToFileURL(fixture).href)});
  `);
  await chmod(server, 0o755);
  return { PATH: `${directory}:${process.env.PATH}`, TURN_E2E: '1', TEST_TURN_PORT: String(port), TEST_SERVER_BINARY: server };
}

for (const status of [0, 23]) {
  test(`owned relay uses disposable credentials and releases both ports after command status ${status}`, async t => {
    const environment = await turnFixture(t);
    const result = await run(t, { ...environment, TURN_URLS: 'turn:production.invalid', TURN_SECRET: 'must-not-inherit' },
      [process.execPath, '-e', `process.exit(${status})`]);
    assert.equal(result.status, status, result.output);
    const log = await readFile(path.join(result.artifacts, 'turn.log'), 'utf8');
    const pid = Number(log.match(/FIXTURE_TURN_PID=(\d+)/)?.[1]);
    assert.ok(pid > 0);
    assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' });
    assert.doesNotMatch(result.output + log, /must-not-inherit/);
    assert.equal((await readdir(result.artifacts)).some(name => name.startsWith('turn-config.')), false);
    const shutdown = JSON.parse(await readFile(path.join(result.artifacts, 'turn-shutdown.json'), 'utf8'));
    assert.equal(shutdown.pid, pid);
    assert.equal(shutdown.passed, true);
    assert.equal(shutdown.forced, false);
    assert.equal(shutdown.waitStatus, 0);
    const tcp = net.createServer().listen(Number(environment.TEST_TURN_PORT), '127.0.0.1');
    await once(tcp, 'listening');
    await new Promise(resolve => tcp.close(resolve));
    const udp = dgram.createSocket('udp4').bind(Number(environment.TEST_TURN_PORT), '127.0.0.1');
    await once(udp, 'listening');
    await new Promise(resolve => udp.close(resolve));
  });
}

for (const protocol of ['tcp', 'udp']) {
  test(`relay preflight refuses an occupied ${protocol} socket without touching its owner`, async t => {
    const environment = await turnFixture(t);
    const listener = protocol === 'tcp' ? net.createServer().listen(0, '127.0.0.1') : dgram.createSocket('udp4').bind(0, '127.0.0.1');
    await once(listener, 'listening');
    t.after(() => new Promise(resolve => listener.close(resolve)));
    const port = listener.address().port;
    const result = await run(t, { ...environment, TEST_TURN_PORT: String(port) });
    assert.equal(result.status, 2, result.output);
    assert.equal(listener.address().port, port);
    await assert.rejects(readFile(path.join(result.artifacts, 'turn.log')), { code: 'ENOENT' });
  });
}

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

test('lifecycle, UI stress and session soak counts use a fresh scoped credential and the owned server PID', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-metrics-fixture.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const server = path.join(directory, 'server.mjs');
  await writeFile(server, `#!/usr/bin/env node
    import http from 'node:http';
    const server = http.createServer((request, response) => {
      if (request.url === '/metrics') {
        response.statusCode = request.headers.authorization === 'Bearer ' + process.env.METRICS_TOKEN ? 200 : 401;
      }
      response.end(JSON.stringify({ enabled: !!process.env.METRICS_TOKEN, pid: process.pid }));
    });
    server.listen(Number(process.env.PORT), '127.0.0.1');
    process.on('SIGTERM', () => server.close());
  `);
  await chmod(server, 0o755);
  for (const profile of ['disabled', 'lifecycle', 'ui-stress', 'session-soak']) {
    const enabled = profile !== 'disabled';
    const result = await run(t, {
      TEST_SERVER_BINARY: server, LIFECYCLE_E2E: profile === 'lifecycle' ? '1' : '0',
      UI_STRESS_E2E: profile === 'ui-stress' ? '1' : '0',
      SESSION_SOAK_E2E: profile === 'session-soak' ? '1' : '0',
      METRICS_TOKEN: 'inherited-private-token', TEST_METRICS_TOKEN: 'inherited-test-token', TEST_SERVER_PID: '1',
    }, [process.execPath, '--input-type=module', '-e', `
      import assert from 'node:assert/strict';
      const response = await fetch(process.env.BASE_URL + '/metrics', {
        headers: { Authorization: 'Bearer ' + process.env.TEST_METRICS_TOKEN },
      });
      const body = await response.json();
      assert.equal(body.enabled, ${enabled});
      assert.equal(body.pid, Number(process.env.TEST_SERVER_PID));
      assert.ok(body.pid > 1);
      assert.equal(process.env.TEST_SERVER_BINARY, ${JSON.stringify(server)});
      assert.equal(process.env.TEST_SERVER_WORKDIR, ${JSON.stringify(path.resolve(repoRoot))});
      if (${enabled}) {
        assert.match(process.env.TEST_METRICS_TOKEN, /^[a-f0-9]{64}$/);
        assert.equal(response.status, 200);
      } else assert.equal(process.env.TEST_METRICS_TOKEN, undefined);
    `]);
    assert.equal(result.status, 0, result.output);
    assert.doesNotMatch(result.output, /inherited-private-token|inherited-test-token|[a-f0-9]{64}/);
  }
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
