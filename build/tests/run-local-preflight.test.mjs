import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import dgram from 'node:dgram';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import net from 'node:net';
import { promisify } from 'node:util';
import test from 'node:test';

// Exercise the launcher's actual inline preflight without running npm, rustup,
// the native build, or an application server. Sockets belong only to each test.
const script = await readFile(new URL('../run-local.sh', import.meta.url), 'utf8');
const source = script.match(/node --input-type=module <<'JS'\n([\s\S]+?)\nJS/)?.[1];
assert.ok(source, 'local launcher must contain its preflight');
const exec = promisify(execFile);

async function preflight(environment = {}) {
  try {
    const { stderr } = await exec(process.execPath, ['--input-type=module', '-e', source], {
      env: { ANNOUNCE_IP: '192.0.2.10', PORT: '3139', MEDIA_WORKERS: '1', WEBRTC_SERVER_PORT_BASE: '41300', ...environment },
      timeout: 5000,
    });
    return { status: 0, stderr };
  } catch (error) {
    assert.equal(error.killed, false, 'preflight must not hang');
    return { status: error.code, stderr: error.stderr };
  }
}

async function tcpSocket(t) {
  const server = net.createServer().listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => server.listening ? new Promise(resolve => server.close(resolve)) : undefined);
  return server;
}

test('local preflight rejects invalid addresses and port ranges before binding', async () => {
  for (const environment of [
    { ANNOUNCE_IP: '::1' }, { ANNOUNCE_IP: 'not-an-ip' },
    { PORT: '0' }, { PORT: '65536' }, { PORT: '03' },
    { MEDIA_WORKERS: '0' }, { MEDIA_WORKERS: '65' },
    { WEBRTC_SERVER_PORT_BASE: '65535', MEDIA_WORKERS: '2' },
  ]) {
    const result = await preflight(environment);
    assert.equal(result.status, 2, result.stderr);
    assert.match(result.stderr, /must be|range exceeds/);
  }
});

test('local preflight rejects remote or empty database settings without leaking credentials', async () => {
  for (const database of [
    '', 'not-a-url',
    'postgres://user:private-password@example.test/chat_dev',
    'postgres://user:private-password@127.0.0.1/chat_dev?hostaddr=192.0.2.20',
    'postgres://127.0.0.1/chat_dev?options=unexpected',
  ]) {
    const result = await preflight({ DATABASE_URL: database });
    assert.equal(result.status, 2, result.stderr);
    assert.match(result.stderr, /DATABASE_URL/);
    assert.doesNotMatch(result.stderr, /private-password/);
  }
});

test('local preflight preserves occupied HTTP and media sockets', async t => {
  const server = await tcpSocket(t);
  const tcp = await preflight({ PORT: String(server.address().port) });
  assert.equal(tcp.status, 2, tcp.stderr);
  assert.match(tcp.stderr, /EADDRINUSE/);
  assert.equal(server.listening, true);

  const udp = dgram.createSocket('udp4').bind(0, '0.0.0.0');
  await once(udp, 'listening');
  t.after(() => new Promise(resolve => udp.close(resolve)));
  const freeTcp = server.address().port;
  await new Promise(resolve => server.close(resolve));
  const media = await preflight({ PORT: String(freeTcp), WEBRTC_SERVER_PORT_BASE: String(udp.address().port) });
  assert.equal(media.status, 2, media.stderr);
  assert.match(media.stderr, /EADDRINUSE/);
  assert.ok(udp.address().port > 0, 'the original UDP owner is still bound');
});

test('successful preflight releases both probes and does not connect to a local database', async t => {
  const server = await tcpSocket(t);
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  const udp = dgram.createSocket('udp4').bind(0, '0.0.0.0');
  await once(udp, 'listening');
  const mediaPort = udp.address().port;
  await new Promise(resolve => udp.close(resolve));
  const env = { PORT: String(port), WEBRTC_SERVER_PORT_BASE: String(mediaPort),
    DATABASE_URL: 'postgres://user:private-password@127.0.0.1:1/chat_dev?sslmode=disable' };
  const first = await preflight(env);
  assert.equal(first.status, 0, first.stderr);
  const again = await preflight(env);
  assert.equal(again.status, 0, 'the first preflight must have released its ports');
});
