import assert from 'node:assert/strict';
import { once } from 'node:events';
import { existsSync } from 'node:fs';
import { mkdtemp, readdir, rm } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import tls from 'node:tls';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { localPort, startTestHttpsProxy } from '../test-https-proxy.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const pinned = path.join(process.env.OPENSSL_DIR || path.join(root, 'target/openssl-4.1.0'), 'bin/openssl');
// Unit tests can use the controller's crypto fixture tool. The browser wrapper
// independently requires its canonical checksum-pinned OpenSSL installation.
const opensslBinary = existsSync(pinned) ? pinned : '/usr/bin/openssl';

async function unusedPort() {
  const server = net.createServer().listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

async function privateTemporaryDirectory(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-tls-test.'));
  const old = process.env.TMPDIR;
  process.env.TMPDIR = directory;
  t.after(async () => {
    if (old === undefined) delete process.env.TMPDIR;
    else process.env.TMPDIR = old;
    await rm(directory, { recursive: true, force: true });
  });
  return directory;
}

test('proxy refuses nonlocal port expressions and ambiguous backend selection', async () => {
  for (const value of ['https://remote.invalid:443', 'remote.invalid:1234', '0', '999', '65536', '1234/path', '01234', '']) {
    assert.throws(() => localPort(value));
  }
  await assert.rejects(startTestHttpsProxy({ backendPort: 1234, port: 1234, opensslBinary }));
  await assert.rejects(startTestHttpsProxy({ backendPort: 1234, port: 1235, opensslBinary: 'openssl' }));
});

test('TLS proxy streams HTTP and raw WebSocket upgrades and removes its key before serving', async t => {
  const temporary = await privateTemporaryDirectory(t);
  const backend = http.createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    response.writeHead(201, { 'set-cookie': '__Host-refresh=fixture; Secure; HttpOnly; Path=/', 'content-type': 'application/json' });
    response.end(JSON.stringify({ method: request.method, path: request.url, origin: request.headers.origin, body }));
  });
  let upgrade;
  backend.on('upgrade', (request, socket, head) => {
    upgrade = { url: request.url, origin: request.headers.origin };
    socket.on('error', () => {});
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n');
    if (head.length) socket.write(head);
    socket.pipe(socket);
  });
  backend.listen(0, '127.0.0.1');
  await once(backend, 'listening');
  let proxy;
  t.after(async () => { await proxy?.close(); await new Promise(resolve => backend.close(resolve)); });
  const port = await unusedPort();
  proxy = await startTestHttpsProxy({ backendPort: backend.address().port, port, opensslBinary });
  assert.deepEqual(await readdir(temporary), [], 'certificate and private key must already be deleted');
  const origin = `https://127.0.0.1:${port}`;
  const response = await new Promise((resolve, reject) => {
    const request = https.request(`${origin}/api/echo?owned=1`, { method: 'POST', ca: proxy.certificate, headers: { origin } }, incoming => {
      let text = '';
      incoming.on('data', chunk => { text += chunk; });
      incoming.once('end', () => resolve({ status: incoming.statusCode, headers: incoming.headers, body: JSON.parse(text) }));
    });
    request.once('error', reject);
    request.end('owned request');
  });
  assert.equal(response.status, 201);
  assert.match(response.headers['set-cookie'][0], /Secure; HttpOnly/);
  assert.deepEqual(response.body, { method: 'POST', path: '/api/echo?owned=1', origin, body: 'owned request' });
  const warnings = [];
  const warning = value => { if (value.name === 'MaxListenersExceededWarning') warnings.push(value); };
  process.on('warning', warning);
  t.after(() => process.removeListener('warning', warning));
  for (let request = 0; request < 15; request++) {
    await new Promise((resolve, reject) => {
      https.get(`${origin}/api/reused-connection`, { ca: proxy.certificate }, incoming => {
        incoming.resume();
        incoming.once('end', resolve);
      }).once('error', reject);
    });
  }
  assert.deepEqual(warnings, [], 'reused HTTP sockets must not accumulate cleanup listeners');
  const socket = tls.connect({ host: '127.0.0.1', port, ca: proxy.certificate });
  socket.on('error', () => {});
  await once(socket, 'secureConnect');
  const certificate = socket.getPeerX509Certificate();
  assert.equal(certificate.checkHost('localhost'), 'localhost');
  assert.equal(certificate.checkIP('127.0.0.1'), '127.0.0.1');
  assert.equal(Date.parse(certificate.validTo) - Date.parse(certificate.validFrom), 24 * 60 * 60 * 1000);
  const echoed = new Promise((resolve, reject) => {
    let value = '';
    const timer = setTimeout(() => reject(new Error('WebSocket bytes were not forwarded')), 3000);
    socket.on('data', chunk => {
      value += chunk;
      if (value.includes('owned upgrade bytes')) { clearTimeout(timer); resolve(value); }
    });
  });
  socket.write(`GET /ws?owned=1 HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nOrigin: ${origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nowned upgrade bytes`);
  assert.match(await echoed, /^HTTP\/1.1 101 Switching Protocols/);
  assert.deepEqual(upgrade, { url: '/ws?owned=1', origin });
  const disconnected = once(socket, 'close');
  await proxy.close();
  await disconnected;
  assert.equal(socket.destroyed, true);
  const rebound = net.createServer().listen(port, '127.0.0.1');
  await once(rebound, 'listening');
  await new Promise(resolve => rebound.close(resolve));
});

test('failed certificate generation leaves no key, temporary directory or listener', async t => {
  const temporary = await privateTemporaryDirectory(t);
  const port = await unusedPort();
  await assert.rejects(startTestHttpsProxy({ backendPort: await unusedPort(), port, opensslBinary: '/missing-test-openssl/bin/openssl' }));
  assert.deepEqual(await readdir(temporary), []);
  const server = net.createServer().listen(port, '127.0.0.1');
  await once(server, 'listening');
  await new Promise(resolve => server.close(resolve));
});
