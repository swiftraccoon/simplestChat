#!/usr/bin/env node
// Disposable TLS termination for an owned HTTP-loopback browser test server.
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

const execute = promisify(execFile);

export function localPort(value) {
  if (!/^[1-9][0-9]{3,4}$/.test(String(value)) || Number(value) > 65535) {
    throw new Error('Test HTTPS ports must be integers from 1000 through 65535.');
  }
  return Number(value);
}

export async function startTestHttpsProxy({ backendPort, port, opensslBinary, signal }) {
  backendPort = localPort(backendPort);
  port = localPort(port);
  if (backendPort === port || !path.isAbsolute(opensslBinary)) {
    throw new Error('Test HTTPS proxy requires distinct local ports and an absolute OpenSSL binary.');
  }
  const directory = await mkdtemp(path.join(os.tmpdir(), 'simplestchat-test-tls.'));
  let key;
  let cert;
  try {
    const keyFile = path.join(directory, 'key.pem');
    const certificate = path.join(directory, 'certificate.pem');
    await execute(opensslBinary, [
      'req', '-x509', '-config', os.devNull, '-newkey', 'rsa:2048', '-noenc', '-keyout', keyFile,
      '-out', certificate, '-days', '1', '-subj', '/CN=localhost',
      '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
    ], { timeout: 15000, maxBuffer: 64 * 1024, signal, env: { PATH: process.env.PATH } });
    [key, cert] = await Promise.all([readFile(keyFile), readFile(certificate)]);
  } finally {
    // Only TLS memory retains the key; browser artifacts never contain credentials.
    await rm(directory, { recursive: true, force: true });
  }
  signal?.throwIfAborted();
  const sockets = new Set();
  const own = socket => {
    if (sockets.has(socket)) return socket;
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
    return socket;
  };
  const server = https.createServer({ key, cert, handshakeTimeout: 10000 }, (request, response) => {
    const upstream = http.request({
      hostname: '127.0.0.1', port: backendPort, method: request.method,
      path: request.url, headers: request.headers,
    }, incoming => {
      response.writeHead(incoming.statusCode, incoming.headers);
      incoming.pipe(response);
      incoming.once('error', () => response.destroy());
    });
    upstream.once('socket', own);
    upstream.once('error', () => {
      if (!response.headersSent) response.writeHead(502);
      response.end();
    });
    request.once('aborted', () => upstream.destroy());
    response.once('close', () => upstream.destroy());
    request.pipe(upstream);
  });
  server.on('connection', own);
  server.on('upgrade', (request, socket, head) => {
    own(socket);
    const upstream = own(net.connect({ host: '127.0.0.1', port: backendPort }));
    socket.once('error', () => upstream.destroy());
    upstream.once('error', () => socket.destroy());
    socket.once('close', () => upstream.destroy());
    upstream.once('close', () => socket.destroy());
    upstream.once('connect', () => {
      const headers = [];
      for (let index = 0; index < request.rawHeaders.length; index += 2) {
        headers.push(`${request.rawHeaders[index]}: ${request.rawHeaders[index + 1]}`);
      }
      upstream.write(`${request.method} ${request.url} HTTP/${request.httpVersion}\r\n${headers.join('\r\n')}\r\n\r\n`);
      if (head.length) upstream.write(head);
      upstream.pipe(socket);
      socket.pipe(upstream);
    });
  });
  let closing;
  const close = () => {
    if (!closing) {
      closing = new Promise(resolve => {
        for (const socket of sockets) socket.destroy();
        server.close(resolve);
      });
    }
    return closing;
  };
  const abort = () => { void close(); };
  signal?.addEventListener('abort', abort, { once: true });
  server.once('close', () => signal?.removeEventListener('abort', abort));
  try {
    await new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen({ host: '127.0.0.1', port }, resolve);
    });
    signal?.throwIfAborted();
  } catch (error) {
    await close();
    throw error;
  }
  return { server, close, certificate: cert };
}

async function main() {
  const controller = new AbortController();
  const stop = () => controller.abort();
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  try {
    if (process.argv.length !== 5) throw new Error('Invalid test HTTPS proxy arguments.');
    const [backendPort, port, opensslBinary] = process.argv.slice(2);
    const proxy = await startTestHttpsProxy({ backendPort, port, opensslBinary, signal: controller.signal });
    console.log(`Test HTTPS proxy listening on 127.0.0.1:${port}`);
    await once(proxy.server, 'close');
  } catch {
    if (!controller.signal.aborted) {
      console.error('Owned test HTTPS proxy failed.');
      process.exitCode = 1;
    }
  } finally {
    process.removeListener('SIGINT', stop);
    process.removeListener('SIGTERM', stop);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] || '').href) await main();
