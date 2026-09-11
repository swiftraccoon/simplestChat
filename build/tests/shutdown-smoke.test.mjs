import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, symlink } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { runCli, runShutdownSmoke, serverEnvironment } from '../shutdown-smoke.mjs';

// No processes, sockets, native media, files or databases are created here.
// The production runner's owned-process transitions are exercised directly.
function fixture(options = {}) {
  const state = { commands: [], kills: [], requests: [], messages: [], clock: 0, socket: null, validated: [] };
  const child = new EventEmitter();
  child.pid = 43210;
  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  const finish = (code, signal) => {
    child.exitCode = code;
    child.signalCode = signal;
    child.emit('exit', code, signal);
  };
  child.kill = signal => {
    state.kills.push(signal);
    if (signal === 'SIGKILL') {
      if (!options.cleanupFails) queueMicrotask(() => finish(null, 'SIGKILL'));
      return true;
    }
    if (options.killRefused && state.kills.length === 1) return false;
    if (options.stalled || options.cleanupFails) return true;
    queueMicrotask(() => {
      if (state.socket && !options.noRoomClosed) state.socket.receive({ type: 'roomClosed', reason: options.wrongReason ? 'Room deleted' : 'Server shutting down' });
      if (state.socket && !options.noClose) state.socket.serverClose(options.closeCode ?? 1001, options.cleanClose ?? true);
      finish(options.exitCode ?? 0, options.exitSignal ?? null);
    });
    return true;
  };
  class Socket extends EventTarget {
    static CLOSED = 3;
    readyState = 0;
    constructor(url, protocols) {
      super();
      state.socket = this;
      state.socketUrl = url;
      state.protocols = protocols;
      queueMicrotask(() => {
        if (options.neverOpen) return;
        if (options.socketError) { this.dispatchEvent(new Event('error')); return; }
        this.readyState = 1;
        this.dispatchEvent(new Event('open'));
      });
    }
    send(value) {
      state.messages.push(JSON.parse(value));
      queueMicrotask(() => {
        if (options.neverJoin) return;
        this.receive(options.admissionError ? { type: 'error', message: 'fixture' } : { type: 'roomJoined', participantId: 'owned-participant' });
      });
    }
    receive(message) { this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(message) })); }
    serverClose(code, wasClean) {
      this.readyState = Socket.CLOSED;
      const event = new Event('close');
      Object.assign(event, { code, wasClean });
      this.dispatchEvent(event);
    }
    close() {
      state.clientClosed = true;
      if (options.closeThrows) throw new Error('Socket close failed');
      this.readyState = Socket.CLOSED;
    }
  }
  const dependencies = {
    validateBinary: async binary => { state.validated.push(binary); if (options.missingBinary) throw new Error('Binary missing'); },
    reservePorts: async () => options.ports ?? { http: 45101, media: 45102 },
    spawn: (...args) => {
      state.commands.push(args);
      queueMicrotask(() => {
        if (options.spawnError) child.emit('error', new Error('Cannot execute'));
        if (options.earlyExit) finish(9, null);
        if (options.output) child.stderr.emit('data', Buffer.from(options.output));
      });
      return child;
    },
    fetch: async (url, init) => {
      state.requests.push([url, init]);
      if (options.fetchError) throw new Error('Not listening');
      return { status: options.status ?? 200, json: async () => options.readyBody ?? { status: 'ready' } };
    },
    WebSocket: Socket,
    wait: async () => { await Promise.resolve(); state.clock += 5; },
    now: () => state.clock,
  };
  const run = overrides => runShutdownSmoke({ binary: '/owned checkout/target/debug/simplestChat', startupTimeoutMs: 50, shutdownTimeoutMs: 50, cleanupTimeoutMs: 20, ...overrides }, dependencies);
  return { state, dependencies, run };
}

test('shutdown smoke verifies readiness, unique guest admission, terminal room event and clean owned exit', async () => {
  const first = fixture();
  const result = await first.run();
  assert.equal(result.ready, true);
  assert.equal(result.joined, true);
  assert.equal(result.roomClosed, true);
  assert.equal(result.closeCode, 1001);
  assert.equal(result.exitCode, 0);
  assert.ok(result.shutdownMs < 50);
  assert.deepEqual(first.state.kills, ['SIGTERM']);
  assert.equal(first.state.commands.length, 1);
  assert.equal(first.state.commands[0][0], '/owned checkout/target/debug/simplestChat');
  assert.deepEqual(first.state.commands[0][1], []);
  assert.deepEqual(first.state.commands[0][2].stdio, ['ignore', 'pipe', 'pipe']);
  assert.equal(first.state.socketUrl, 'ws://127.0.0.1:45101/ws');
  assert.deepEqual(first.state.protocols, ['simplestchat']);
  assert.ok(first.state.requests.every(([url, init]) => url === 'http://127.0.0.1:45101/ready' && init.redirect === 'error'));
  assert.equal(first.state.messages.length, 1, 'no capture, transport, reconnect or leave requests');
  assert.equal(first.state.messages[0].type, 'joinRoom');
  assert.match(first.state.messages[0].roomId, /^shutdown-smoke-[a-f0-9-]{36}$/);
  const second = fixture();
  await second.run();
  assert.notEqual(first.state.messages[0].roomId, second.state.messages[0].roomId);
});

test('owned server configuration is explicit and contains no database, auth, TURN or inherited settings', () => {
  assert.deepEqual(serverEnvironment({ http: 41001, media: 41002 }, '/fixture/bin'), {
    PATH: '/fixture/bin', BIND_ADDR: '127.0.0.1', PORT: '41001', ANNOUNCE_IP: '127.0.0.1',
    MEDIA_WORKERS: '1', WEBRTC_SERVER_PORT_BASE: '41002', ALLOWED_ORIGINS: 'http://127.0.0.1:41001',
    ALLOW_AD_HOC_ROOMS: 'true', REGISTRATION_ENABLED: 'false', RUN_MIGRATIONS: 'false',
    RUST_LOG: 'simplestChat=info,mediasoup=warn',
  });
});

test('invalid inputs and missing prerequisites fail before spawning or contacting a service', async () => {
  for (const binary of [undefined, '', 'target/debug/simplestChat', 'http://localhost/server']) {
    const f = fixture();
    await assert.rejects(f.run({ binary }), /Usage:/);
    assert.equal(f.state.commands.length, 0);
    assert.equal(f.state.requests.length, 0);
  }
  for (const options of [{ missingBinary: true }, { ports: { http: 0, media: 45102 } }, { ports: { http: 45101, media: 65536 } }]) {
    const f = fixture(options);
    await assert.rejects(f.run());
    assert.equal(f.state.commands.length, 0);
  }
  const missingNode = fixture();
  missingNode.dependencies.WebSocket = null;
  await assert.rejects(missingNode.run(), /native fetch and WebSocket/);
  assert.equal(missingNode.state.commands.length, 0);
});

for (const [options, expected] of [
  [{ spawnError: true }, /could not be started/],
  [{ earlyExit: true }, /exited early/],
  [{ status: 503 }, /did not become ready/],
  [{ fetchError: true }, /did not become ready/],
  [{ readyBody: {} }, /unexpected result/],
  [{ neverOpen: true }, /did not open/],
  [{ socketError: true }, /WebSocket failed/],
  [{ neverJoin: true }, /was not joined/],
  [{ admissionError: true }, /admission failed/],
  [{ noRoomClosed: true }, /terminal roomClosed/],
  [{ wrongReason: true }, /terminal roomClosed/],
  [{ noClose: true }, /exceeded its deadline/],
  [{ closeCode: 1006 }, /clean WebSocket close/],
  [{ cleanClose: false }, /clean WebSocket close/],
  [{ exitCode: 2 }, /was not clean/],
  [{ exitSignal: 'SIGTERM' }, /was not clean/],
  [{ killRefused: true }, /Could not signal owned server/],
]) {
  test(`shutdown smoke rejects ${Object.keys(options)[0]} without borrowing another process`, async () => {
    const f = fixture(options);
    await assert.rejects(f.run(), expected);
    assert.equal(f.state.commands.length, 1);
    assert.ok(f.state.kills.every(signal => ['SIGTERM', 'SIGKILL'].includes(signal)));
    if (options.spawnError || options.earlyExit) assert.deepEqual(f.state.kills, [], 'an unowned or exited PID must never be signaled');
  });
}

test('stalled shutdown fails its deadline and escalates cleanup on the owned child only', async () => {
  const f = fixture({ stalled: true });
  await assert.rejects(f.run(), /exceeded its deadline/);
  assert.deepEqual(f.state.kills, ['SIGTERM', 'SIGTERM', 'SIGKILL']);
  assert.equal(f.state.clientClosed, true);
});

test('a cleanup failure preserves the original failure and bounded diagnostics', async () => {
  const f = fixture({ cleanupFails: true, output: 'x'.repeat(40_000) });
  await assert.rejects(f.run(), error => {
    assert.ok(error instanceof AggregateError);
    assert.match(error.errors[0].message, /exceeded its deadline/);
    assert.match(error.errors[1].message, /could not be cleaned up/);
    assert.equal(error.serverOutput.length, 32_768);
    return true;
  });
});

test('socket cleanup failure cannot bypass owned-process cleanup', async () => {
  const f = fixture({ neverOpen: true, closeThrows: true });
  await assert.rejects(f.run(), error => {
    assert.ok(error instanceof AggregateError);
    assert.match(error.errors[0].message, /did not open/);
    assert.match(error.errors[1].message, /Socket close failed/);
    return true;
  });
  assert.deepEqual(f.state.kills, ['SIGTERM']);
});

test('cancellation before startup opens nothing and cancellation while polling cleans its child', async () => {
  const cancellation = new AbortController();
  cancellation.abort();
  const before = fixture();
  await assert.rejects(before.run({ signal: cancellation.signal }), /cancelled/);
  assert.equal(before.state.commands.length, 0);
  const active = new AbortController();
  const during = fixture({ status: 503 });
  during.dependencies.wait = async () => active.abort();
  await assert.rejects(during.run({ signal: active.signal }), /cancelled/);
  assert.deepEqual(during.state.kills, ['SIGTERM']);
});

test('CLI requires a binary and exposes no external endpoint, database or process-ID controls', async () => {
  let calls = 0, output = '', errors = '';
  const options = { run: async () => { calls++; return { ready: true }; }, stdout: { write: value => { output += value; } }, stderr: { write: value => { errors += value; } } };
  assert.equal(await runCli({ ...options, args: ['--help'] }), 0);
  for (const args of [[], ['--binary', 'relative'], ['--url', 'http://localhost'], ['--binary', '/owned/server', '--pid', '1']]) assert.equal(await runCli({ ...options, args }), 2);
  assert.equal(calls, 0);
  assert.match(output, /Usage:/);
  assert.match(errors, /Usage:/);
  assert.equal(await runCli({ ...options, args: ['--binary', '/owned/server'] }), 0);
  assert.equal(calls, 1);
  assert.equal(await runCli({ ...options, args: ['--binary', '/owned/server'], run: async () => { throw new Error('fixture failure'); } }), 1);
  assert.match(errors, /fixture failure/);
});

test('direct, symlink and temporary-directory aliases execute the CLI while imports remain inert', async t => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-shutdown-cli.'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const runner = fileURLToPath(new URL('../shutdown-smoke.mjs', import.meta.url));
  const alias = path.join(directory, 'smoke alias.mjs');
  await symlink(runner, alias);
  for (const entry of [runner, alias]) {
    const help = spawnSync(process.execPath, [entry, '--help'], { encoding: 'utf8', timeout: 3000 });
    assert.equal(help.status, 0, help.stderr);
    assert.match(help.stdout, /^Usage: node build\/shutdown-smoke.mjs --binary/);
    const invalid = spawnSync(process.execPath, [entry, '--existing-pid', '1'], { encoding: 'utf8', timeout: 3000 });
    assert.equal(invalid.status, 2, invalid.stderr);
    assert.match(invalid.stderr, /^Usage:/);
  }
  const imported = spawnSync(process.execPath, ['--input-type=module', '-e', `await import(${JSON.stringify(new URL('../shutdown-smoke.mjs', import.meta.url).href)}); console.log('imported');`], { encoding: 'utf8', timeout: 3000 });
  assert.equal(imported.status, 0, imported.stderr);
  assert.equal(imported.stdout, 'imported\n');
  assert.equal(imported.stderr, '');
});
