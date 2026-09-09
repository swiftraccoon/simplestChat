import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

function fakeTimers() {
  let now = 0;
  let nextId = 1;
  const timers = new Map();
  return {
    setTimeout(callback, delay) {
      const id = nextId++;
      timers.set(id, { callback, at: now + delay });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    tick(milliseconds) {
      const end = now + milliseconds;
      while (true) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.at <= end)
          .sort((left, right) => left[1].at - right[1].at)[0];
        if (!next) break;
        const [id, timer] = next;
        now = timer.at;
        timers.delete(id);
        timer.callback();
      }
      now = end;
    },
    get pendingCount() {
      return timers.size;
    },
  };
}

async function connectedClient(t) {
  class FakeWebSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static instances = [];
    readyState = FakeWebSocket.CONNECTING;
    sent = [];

    constructor() {
      FakeWebSocket.instances.push(this);
    }

    open() {
      this.readyState = FakeWebSocket.OPEN;
      this.onopen?.();
    }

    send(message) {
      this.sent.push(JSON.parse(message));
    }

    receive(message) {
      this.onmessage?.({ data: JSON.stringify(message) });
    }

    close() {
      this.readyState = 3;
      this.onclose?.();
    }
  }

  const timers = fakeTimers();
  const { SignalingClient } = await loadTypeScript('src/signaling.ts', {
    globals: {
      WebSocket: FakeWebSocket,
      setTimeout: timers.setTimeout,
      clearTimeout: timers.clearTimeout,
      console: { log() {}, error() {} },
    },
  });
  const client = new SignalingClient('ws://localhost/ws');
  client.connect();
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  t.after(() => client.disconnect());
  return { client, socket, timers, FakeWebSocket };
}

test('a timed-out request cannot consume the response to a later request', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const expired = assert.rejects(
    client.request({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities', 10),
    /Timeout waiting for routerRtpCapabilities/,
  );
  timers.tick(10);
  await expired;

  const retry = client.request(
    { type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities', 10,
  );
  const reply = { type: 'routerRtpCapabilities', rtpCapabilities: { codecs: [] } };
  socket.receive(reply);
  timers.tick(10);
  assert.deepEqual(await retry, reply);
  assert.equal(socket.sent.length, 2);
  assert.equal(timers.pendingCount, 0);
});

test('server errors reject the request and leave the next response available', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const rejected = assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
    /Transport limit reached/,
  );
  socket.receive({ type: 'error', message: 'Transport limit reached' });
  await rejected;
  assert.equal(timers.pendingCount, 0);

  const retry = client.request({ type: 'createSendTransport' }, 'transportCreated', 10);
  const reply = { type: 'transportCreated', transportId: 'replacement' };
  socket.receive(reply);
  timers.tick(10);
  assert.deepEqual(await retry, reply);
});

test('disconnect rejects all requests, cancels their timers, and permits a fresh connection', async (t) => {
  const { client, timers, FakeWebSocket } = await connectedClient(t);
  const pending = [
    assert.rejects(
      client.request({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities', 10),
      /WebSocket closed|Disconnected/,
    ),
    assert.rejects(
      client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
      /WebSocket closed|Disconnected/,
    ),
  ];
  client.disconnect();
  await Promise.all(pending);
  assert.equal(client.connected, false);
  assert.equal(timers.pendingCount, 0);

  client.connect();
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  const request = client.request({ type: 'createSendTransport' }, 'transportCreated', 10);
  const reply = { type: 'transportCreated', transportId: 'new-connection' };
  replacement.receive(reply);
  timers.tick(10);
  assert.deepEqual(await request, reply);
});
