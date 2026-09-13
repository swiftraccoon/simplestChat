import { spawn as spawnCommand } from 'node:child_process';
import dgram from 'node:dgram';
import { access, realpath } from 'node:fs/promises';
import { constants } from 'node:fs';
import net from 'node:net';
import { isAbsolute } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const usage = 'Usage: node build/shutdown-smoke.mjs --binary /absolute/path/to/simplestChat';

/** Reserve ports on this host only. Final binding still belongs to the child. */
export async function reserveLocalPorts() {
  const http = net.createServer();
  const media = dgram.createSocket('udp4');
  try {
    await new Promise((resolve, reject) => {
      http.once('error', reject);
      http.listen(0, '127.0.0.1', resolve);
    });
    await new Promise((resolve, reject) => {
      media.once('error', reject);
      media.bind(0, '0.0.0.0', resolve);
    });
    return { http: http.address().port, media: media.address().port };
  } finally {
    if (http.listening) await new Promise(resolve => http.close(resolve));
    await new Promise((resolve, reject) => {
      try { media.close(resolve); }
      catch (error) {
        if (error.code === 'ERR_SOCKET_DGRAM_NOT_RUNNING') resolve();
        else reject(error);
      }
    });
  }
}

/** No inherited database, credentials, proxy, TURN or production configuration. */
export function serverEnvironment(ports, executablePath = process.env.PATH) {
  return {
    PATH: executablePath ?? '/usr/bin:/bin',
    BIND_ADDR: '127.0.0.1', PORT: String(ports.http), ANNOUNCE_IP: '127.0.0.1',
    MEDIA_WORKERS: '1', WEBRTC_SERVER_PORT_BASE: String(ports.media),
    ALLOWED_ORIGINS: `http://127.0.0.1:${ports.http}`,
    ALLOW_AD_HOC_ROOMS: 'true', REGISTRATION_ENABLED: 'false', RUN_MIGRATIONS: 'false',
    RUST_LOG: 'simplestChat=info,mediasoup=warn',
  };
}

function validPort(port) {
  return Number.isSafeInteger(port) && port >= 1000 && port <= 65535;
}

/** Start and stop one owned guest server. Never connect to a supplied service URL. */
export async function runShutdownSmoke({
  binary, signal, startupTimeoutMs = 20_000, shutdownTimeoutMs = 20_000, cleanupTimeoutMs = 1000,
} = {}, dependencies = {}) {
  if (typeof binary !== 'string' || !isAbsolute(binary)) throw new Error(usage);
  const {
    spawn = spawnCommand, reservePorts = reserveLocalPorts,
    fetch = globalThis.fetch, WebSocket = globalThis.WebSocket,
    validateBinary = file => access(file, constants.X_OK),
    wait = delay, now = () => performance.now(),
  } = dependencies;
  if (typeof WebSocket !== 'function' || typeof fetch !== 'function') {
    throw new Error('Node with native fetch and WebSocket support is required');
  }
  await validateBinary(binary);
  if (signal?.aborted) throw new Error('Shutdown smoke cancelled');
  const ports = await reservePorts();
  if (!ports || !validPort(ports.http) || !validPort(ports.media)) throw new Error('Invalid allocated test ports');
  const baseUrl = `http://127.0.0.1:${ports.http}`;
  const child = spawn(binary, [], {
    cwd: repoRoot, env: serverEnvironment(ports), stdio: ['ignore', 'pipe', 'pipe'],
  });
  let exit, spawnFailure, socket, socketFailure, opened = false, joined = false, closed, serverRestarting = false;
  let output = '';
  const recordOutput = chunk => { output = (output + chunk.toString()).slice(-32_768); };
  child.stdout?.on('data', recordOutput);
  child.stderr?.on('data', recordOutput);
  child.once('error', error => { spawnFailure = error; });
  child.once('exit', (code, signal) => { exit = { code, signal }; });

  function assertActive() {
    if (signal?.aborted) throw new Error('Shutdown smoke cancelled');
    if (spawnFailure) throw new Error('Owned server could not be started', { cause: spawnFailure });
    if (exit) throw new Error(`Owned server exited early (code ${exit.code}, signal ${exit.signal})`);
    if (socketFailure) throw socketFailure;
  }
  async function waitUntil(check, timeout, message, cancellable = true) {
    const started = now();
    for (;;) {
      if (cancellable && signal?.aborted) throw new Error('Shutdown smoke cancelled');
      if (await check()) return;
      if (now() - started >= timeout) throw new Error(message);
      await wait(Math.min(25, timeout));
    }
  }

  let result, failure;
  try {
    await waitUntil(async () => {
      assertActive();
      let response;
      try {
        response = await fetch(`${baseUrl}/ready`, { signal: AbortSignal.timeout(750), redirect: 'error' });
      } catch { assertActive(); return false; }
      assertActive();
      if (response.status !== 200) return false;
      let body;
      try { body = await response.json(); }
      catch { throw new Error('Readiness returned invalid JSON'); }
      if (!body || body.status !== 'ready') throw new Error('Readiness returned an unexpected result');
      return true;
    }, startupTimeoutMs, 'Owned server did not become ready');

    const openSocket = () => {
      const owned = new WebSocket(`ws://127.0.0.1:${ports.http}/ws`, ['simplestchat']);
      owned.addEventListener('open', () => { if (socket === owned) opened = true; });
      owned.addEventListener('error', () => { if (socket === owned) socketFailure = new Error('Owned WebSocket failed'); });
      owned.addEventListener('close', event => { if (socket === owned) closed = { code: event.code, wasClean: event.wasClean }; });
      owned.addEventListener('message', event => {
        if (socket !== owned) return;
        let message;
        try { message = JSON.parse(event.data); }
        catch { socketFailure = new Error('Owned WebSocket returned invalid JSON'); return; }
        if (message?.type === 'roomJoined' && typeof message.participantId === 'string' && message.participantId) joined = true;
        else if (message?.type === 'serverRestarting' && message.reason === 'Server shutting down') serverRestarting = true;
        else if (message?.type === 'roomClosed') socketFailure = new Error('Shutdown incorrectly terminated the room');
        else if (['error', 'roomPasswordRequired', 'lobbyWaiting'].includes(message?.type)) socketFailure = new Error('Owned guest room admission failed');
      });
      return owned;
    };
    // Exercise the real connection handler before the independent server-drain
    // path. This guest socket never joins, captures, or sends application data.
    socket = openSocket();
    await waitUntil(() => { assertActive(); return opened; }, startupTimeoutMs, 'Owned WebSocket did not open');
    socket.close(1000, 'Peer close smoke');
    await waitUntil(() => { assertActive(); return Boolean(closed); }, 5000, 'Peer WebSocket close exceeded its deadline');
    if (closed.code !== 1000 || !closed.wasClean) throw new Error('Peer WebSocket close was not clean with code 1000');
    const peerCloseCode = closed.code;
    opened = false;
    closed = undefined;
    socket = openSocket();
    await waitUntil(() => { assertActive(); return opened; }, startupTimeoutMs, 'Owned WebSocket did not open');
    socket.send(JSON.stringify({ type: 'joinRoom', roomId: `shutdown-smoke-${randomUUID()}`, participantName: 'Shutdown smoke' }));
    await waitUntil(() => { assertActive(); return joined; }, startupTimeoutMs, 'Owned guest room was not joined');
    assertActive();
    const shutdownStarted = now();
    if (!child.kill('SIGTERM')) throw new Error('Could not signal owned server');
    await waitUntil(() => {
      if (spawnFailure) throw new Error('Owned server failed', { cause: spawnFailure });
      if (socketFailure) throw socketFailure;
      if (exit && (exit.code !== 0 || exit.signal !== null)) throw new Error(`Shutdown was not clean (code ${exit.code}, signal ${exit.signal})`);
      return Boolean(exit && closed);
    }, shutdownTimeoutMs, 'Shutdown exceeded its deadline');
    const shutdownMs = now() - shutdownStarted;
    if (!serverRestarting) throw new Error('Shutdown did not deliver the temporary serverRestarting event');
    if (closed.code !== 1001 || !closed.wasClean) throw new Error('Shutdown did not complete a clean WebSocket close with code 1001');
    if (shutdownMs >= shutdownTimeoutMs) throw new Error('Shutdown exceeded its deadline');
    result = { ready: true, peerCloseCode, joined: true, serverRestarting: true, closeCode: closed.code, exitCode: exit.code, shutdownMs, ports };
  } catch (error) { failure = error instanceof Error ? error : new Error('Shutdown smoke failed'); }

  // Only the ChildProcess object created above may be signaled. Cleanup never
  // searches process names, guesses PIDs, closes shared ports or opens a DB.
  try {
    if (socket && socket.readyState !== WebSocket.CLOSED) socket.close();
  } catch (cleanupError) {
    failure = failure ? new AggregateError([failure, cleanupError], 'Smoke failed and socket cleanup failed') : cleanupError;
  }
  try {
    if (!exit && child.pid && !spawnFailure) {
      child.kill('SIGTERM');
      try { await waitUntil(() => Boolean(exit), cleanupTimeoutMs, 'Owned server ignored cleanup', false); }
      catch {
        child.kill('SIGKILL');
        await waitUntil(() => Boolean(exit), cleanupTimeoutMs, 'Owned server could not be cleaned up', false);
      }
    }
  } catch (cleanupError) {
    failure = failure ? new AggregateError([failure, cleanupError], 'Smoke failed and owned cleanup failed') : cleanupError;
  }
  if (failure) {
    // The environment is guest-only, but keep bounded server diagnostics local.
    failure.serverOutput = output;
    throw failure;
  }
  return result;
}

export async function runCli({ args = [], stdout = process.stdout, stderr = process.stderr, run = runShutdownSmoke } = {}) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) { stdout.write(`${usage}\n`); return 0; }
  if (args.length !== 2 || args[0] !== '--binary' || !isAbsolute(args[1])) { stderr.write(`${usage}\n`); return 2; }
  const cancellation = new AbortController();
  const cancel = () => cancellation.abort();
  process.once('SIGINT', cancel);
  process.once('SIGTERM', cancel);
  try {
    stdout.write(`${JSON.stringify(await run({ binary: args[1], signal: cancellation.signal }))}\n`);
    return 0;
  } catch (error) {
    stderr.write(`Shutdown smoke failed: ${error.message}\n`);
    if (error.serverOutput) stderr.write(`Owned server output (bounded):\n${error.serverOutput}\n`);
    return 1;
  } finally {
    process.removeListener('SIGINT', cancel);
    process.removeListener('SIGTERM', cancel);
  }
}

// Node resolves an ESM entry module through symlinks, while argv may retain the
// invocation spelling (including macOS /var aliases). Imports must stay inert.
const entryFile = process.argv[1] ? await realpath(process.argv[1]).catch(() => undefined) : undefined;
if (entryFile && import.meta.url === pathToFileURL(entryFile).href) {
  process.exitCode = await runCli({ args: process.argv.slice(2) });
}
