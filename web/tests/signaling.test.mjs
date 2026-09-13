import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';

const validation = (await loadContractModules())['./protocol-validation'];

function transportReply(transportId) {
  return {
    type: 'transportCreated',
    transportId,
    iceParameters: { usernameFragment: 'test-fragment', password: 'test-password', iceLite: true },
    iceCandidates: [],
    dtlsParameters: { role: 'auto', fingerprints: [] },
  };
}

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
    get delays() {
      return [...timers.values()].map((timer) => timer.at - now);
    },
  };
}

async function connectedClient(t, random = 0.5) {
  class FakeWebSocket {
    static OPEN = 1;
    static CONNECTING = 0;
    static instances = [];
    readyState = FakeWebSocket.CONNECTING;
    sent = [];

    constructor(url, protocols) {
      this.protocols = protocols;
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
  const errors = [];
  const { SignalingClient } = await loadTypeScript('src/signaling.ts', {
    modules: { './protocol-validation': validation },
    globals: {
      WebSocket: FakeWebSocket,
      setTimeout: timers.setTimeout,
      clearTimeout: timers.clearTimeout,
      Math: Object.assign(Object.create(Math), { random: () => random }),
      console: {
        log() {},
        error(...args) {
          errors.push(args);
        },
      },
    },
  });
  const client = new SignalingClient('ws://localhost/ws');
  client.connect();
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  t.after(() => client.disconnect());
  return { client, socket, timers, FakeWebSocket, errors };
}

test('malformed replies neither resolve pending requests nor reach application handlers', async (t) => {
  const { client, socket, timers, errors } = await connectedClient(t);
  const messages = [];
  client.setOnMessage((value) => messages.push(value));
  const pending = client.request({ type: 'createRecvTransport' }, 'transportCreated', 100);
  socket.receive({ type: 'transportCreated', transportId: 'missing-media-fields' });
  socket.receive({ type: 'error', message: { secret: 'payload-sentinel' } });
  socket.receive({ type: 'participantJoined', participantId: 'bad', participantName: 42 });
  socket.onmessage({ data: '{ malformed payload-sentinel' });
  socket.onmessage({ data: new ArrayBuffer(1) });
  assert.equal(timers.pendingCount, 1);
  assert.deepEqual(messages, []);
  assert.equal(errors.length, 5);
  assert.equal(JSON.stringify(errors).includes('payload-sentinel'), false);
  const reply = transportReply('valid');
  socket.receive(reply);
  assert.deepEqual(await pending, reply);
  assert.equal(timers.pendingCount, 0);
});

test('a timed-out request cannot consume the response to a later request', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const expired = assert.rejects(
    client.request({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities', 10),
    /Timeout waiting for routerRtpCapabilities/,
  );
  timers.tick(10);
  await expired;

  const retry = client.request({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities', 10);
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
  const reply = transportReply('replacement');
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
  const reply = transportReply('new-connection');
  replacement.receive(reply);
  timers.tick(10);
  assert.deepEqual(await request, reply);
});

test('a replaced socket closing late cannot disconnect the current connection or reject its requests', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  const statuses = [];
  client.setOnStatusChange((status) => statuses.push(status));
  // Browsers dispatch close asynchronously, sometimes after the replacement opens.
  socket.close = () => {
    socket.readyState = 2;
  };
  client.disconnect();
  client.connect('replacement-token');
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  const request = client.request({ type: 'createSendTransport' }, 'transportCreated', 100);
  request.catch(() => {});
  socket.readyState = 3;
  socket.onclose();
  assert.equal(statuses.at(-1), 'connected');
  assert.equal(client.connected, true);
  assert.equal(timers.pendingCount, 1, 'only the active request timer may remain');
  const reply = transportReply('current');
  replacement.receive(reply);
  assert.deepEqual(await request, reply);
  assert.equal(timers.pendingCount, 0);
});

test('events from a retired socket cannot reach current message or reconnect handlers', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  const messages = [];
  const statuses = [];
  let reconnects = 0;
  client.setOnMessage((message) => messages.push(message));
  client.setOnStatusChange((status) => statuses.push(status));
  client.setOnReconnected(() => reconnects++);
  client.disconnect();
  client.connect();
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  const statusCount = statuses.length;
  const request = client.request({ type: 'createRecvTransport' }, 'transportCreated', 100);
  socket.receive({ type: 'transportCreated', transportId: 'retired' });
  socket.receive({ type: 'chatReceived', content: 'retired event' });
  socket.receive({ type: 'serverRestarting', reason: 'Retired notice' });
  socket.onopen();
  socket.onerror({ type: 'retired-error' });
  assert.equal(timers.pendingCount, 1);
  assert.deepEqual(messages, []);
  assert.equal(statuses.length, statusCount);
  assert.equal(reconnects, 0);
  const reply = transportReply('current');
  replacement.receive(reply);
  assert.deepEqual(await request, reply);
});

test('the current socket closing still rejects requests and reconnects once', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  let reconnects = 0;
  client.setOnReconnected(() => reconnects++);
  const pending = assert.rejects(
    client.request({ type: 'createRecvTransport' }, 'transportCreated', 100),
    /WebSocket closed/,
  );
  socket.close();
  await pending;
  assert.equal(client.connected, false);
  assert.equal(timers.pendingCount, 2, 'one retry and one overall recovery deadline');
  timers.tick(2000);
  const replacement = FakeWebSocket.instances.at(-1);
  assert.notEqual(replacement, socket);
  replacement.open();
  assert.equal(reconnects, 1);
  assert.equal(client.connected, true);
  assert.equal(timers.pendingCount, 0);
});

test('restart recovery keeps its deadline across socket open until room recovery completes', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  const messages = [];
  client.setOnMessage((message) => messages.push(message));
  socket.receive({ type: 'serverRestarting', reason: 'Server shutting down' });
  assert.equal(messages[0].type, 'serverRestarting');
  assert.deepEqual(timers.delays, [120000]);
  socket.close();
  timers.tick(1500);
  FakeWebSocket.instances.at(-1).open();
  assert.equal(timers.pendingCount, 1, 'room rejoin remains under the original deadline');
  client.completeRestartRecovery();
  assert.equal(timers.pendingCount, 0);
});

test('restart deadline rejects pending work, closes a stalled attempt, and preserves identity for explicit retry', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  const failures = [];
  client.setToken('fixture-current-token');
  client.setOnReconnectFailed(() => failures.push('expired'));
  socket.receive({ type: 'serverRestarting', reason: 'Server shutting down' });
  socket.close();
  timers.tick(1500);
  const stalled = FakeWebSocket.instances.at(-1);
  const pending = assert.rejects(
    client.request({ type: 'createRecvTransport' }, 'transportCreated', 180000),
    /Reconnection timed out/,
  );
  timers.tick(118500);
  await pending;
  assert.deepEqual(failures, ['expired']);
  assert.equal(stalled.readyState, 3);
  assert.equal(timers.pendingCount, 0);
  const attempts = FakeWebSocket.instances.length;
  timers.tick(600000);
  assert.equal(FakeWebSocket.instances.length, attempts, 'no unlimited background retries');
  client.retryConnection();
  const retry = FakeWebSocket.instances.at(-1);
  assert.notEqual(retry, stalled);
  assert.deepEqual(retry.protocols, ['simplestchat', 'auth.fixture-current-token']);
  retry.open();
  client.completeRestartRecovery();
  assert.equal(client.connected, true);
  assert.equal(timers.pendingCount, 0);
});

test('repeated restart notices do not extend the deadline and disconnect cancels it', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  let failures = 0;
  client.setOnReconnectFailed(() => failures++);
  socket.receive({ type: 'serverRestarting', reason: 'Server shutting down' });
  timers.tick(60000);
  socket.receive({ type: 'serverRestarting', reason: 'Server shutting down' });
  assert.deepEqual(timers.delays, [60000]);
  client.disconnect();
  timers.tick(120000);
  assert.equal(failures, 0);
  assert.equal(timers.pendingCount, 0);
});

test('an initial connection outage also exposes an explicit retry after its deadline', async (t) => {
  const { client, timers, FakeWebSocket } = await connectedClient(t);
  client.disconnect();
  client.connect();
  FakeWebSocket.instances.at(-1).close();
  timers.tick(120000);
  assert.equal(client.reconnectExhausted, true);
  assert.equal(timers.pendingCount, 0);
  client.retryConnection();
  assert.equal(client.reconnectExhausted, false);
  FakeWebSocket.instances.at(-1).open();
  client.completeRestartRecovery();
  assert.equal(timers.pendingCount, 0);
});

for (const random of [0, 0.5, 0.999999]) {
  test(`reconnect delay is bounded equal jitter for random=${random}`, async (t) => {
    const { socket, timers, FakeWebSocket } = await connectedClient(t, random);
    socket.close();
    for (const ceiling of [2000, 4000, 8000, 16000, 30000, 30000]) {
      const delay = Math.min(...timers.delays);
      assert.equal(delay, Math.floor(ceiling / 2 + (random * ceiling) / 2));
      assert.ok(delay >= ceiling / 2 && delay < ceiling);
      timers.tick(delay);
      FakeWebSocket.instances.at(-1).close();
    }
  });
}
