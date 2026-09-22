import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';

const validation = (await loadContractModules())['./protocol-validation'];
const CLOCK_EPOCH_MS = 1_800_000_000_000;

function transportReply(transportId, requestId) {
  return {
    type: 'transportCreated',
    ...(requestId === undefined ? {} : { requestId }),
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
    get now() {
      return now;
    },
    get callbacks() {
      return [...timers.values()].map((timer) => timer.callback);
    },
    advanceWithoutTimers(milliseconds) {
      now += milliseconds;
    },
    tick(milliseconds) {
      const end = now + milliseconds;
      while (true) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.at <= end)
          .sort((left, right) => left[1].at - right[1].at)[0];
        if (!next) break;
        const [id, timer] = next;
        now = Math.max(now, timer.at);
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

async function connectedClient(t, random = 0.5, token) {
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
      Date: { now: () => CLOCK_EPOCH_MS + timers.now },
      performance: { now: () => timers.now },
      console: {
        log() {},
        error(...args) {
          errors.push(args);
        },
      },
    },
  });
  const client = new SignalingClient('ws://localhost/ws');
  client.connect(token);
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  t.after(() => client.disconnect());
  return { client, socket, timers, FakeWebSocket, errors };
}

function deferRenewal(socket, expiresAt = CLOCK_EPOCH_MS / 1000 + 60, retryAfterMs = 3000) {
  socket.receive({
    type: 'authenticationRenewalDeferred',
    requestId: socket.sent.at(-1).requestId,
    retryAfterMs,
    expiresAt,
  });
}

test('refresh renews the same authenticated socket without disturbing room requests', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  const statuses = [],
    messages = [];
  client.setOnStatusChange((value) => statuses.push(value));
  client.setOnMessage((value) => messages.push(value));
  const pending = client.request({ type: 'createSendTransport' }, 'transportCreated');
  client.setToken('refreshed');
  const renewal = socket.sent.at(-1);
  assert.equal(renewal.type, 'renewAuthentication');
  assert.equal(renewal.token, 'refreshed');
  socket.receive({ type: 'authenticationRenewed', requestId: 'unrelated', expiresAt: 1800000000 });
  assert.equal(timers.pendingCount, 2);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: renewal.requestId,
    expiresAt: 1800000000,
  });
  assert.equal(timers.pendingCount, 1);
  socket.receive(transportReply('unchanged', socket.sent[0].requestId));
  assert.equal((await pending).transportId, 'unchanged');
  client.setToken('refreshed');
  assert.equal(socket.sent.length, 2, 'adopted token is not redundantly renewed');
  assert.equal(FakeWebSocket.instances.length, 1);
  assert.deepEqual(statuses, []);
  assert.deepEqual(messages, []);
});

test('ordinary request errors cannot reject a correlated authentication renewal', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  const renewal = socket.sent.at(-1);
  const rejected = assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated'),
    /Room operation failed/,
  );
  socket.receive({
    type: 'error',
    requestId: socket.sent.at(-1).requestId,
    message: 'Room operation failed',
  });
  await rejected;
  assert.equal(timers.pendingCount, 1);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: renewal.requestId,
    expiresAt: 1800000000,
  });
  assert.equal(timers.pendingCount, 0);
  assert.equal(client.connected, true);
});

test('deferred renewal retries on the same socket without disturbing membership or room requests', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  const events = [];
  client.setOnMessage((message) => events.push(message));
  client.setOnStatusChange((status) => events.push(status));
  client.setOnConnectionLost(() => events.push('lost'));
  const pending = client.request({ type: 'createSendTransport' }, 'transportCreated', 20000);
  client.setToken('refreshed');
  const original = socket.sent.at(-1);
  const staleTimeout = timers.callbacks.at(-1);
  socket.receive({
    type: 'authenticationRenewalDeferred',
    requestId: 'unrelated',
    retryAfterMs: 3000,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 60,
  });
  assert.deepEqual(timers.delays, [20000, 5000]);
  deferRenewal(socket);
  assert.deepEqual(timers.delays, [20000, 3125]);
  timers.tick(1000);
  deferRenewal(socket);
  assert.deepEqual(timers.delays, [19000, 2125], 'duplicate response cannot postpone the retry');
  staleTimeout();
  timers.tick(2124);
  assert.equal(socket.sent.length, 2);
  timers.tick(1);
  const retry = socket.sent.at(-1);
  assert.equal(retry.token, 'refreshed');
  assert.notEqual(retry.requestId, original.requestId);
  staleTimeout();
  socket.receive({ type: 'authenticationRenewalFailed', requestId: original.requestId });
  assert.equal(client.connected, true);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: retry.requestId,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 120,
  });
  socket.receive(transportReply('same-membership', socket.sent[0].requestId));
  assert.equal((await pending).transportId, 'same-membership');
  assert.equal(timers.pendingCount, 0);
  assert.equal(FakeWebSocket.instances.length, 1);
  assert.deepEqual(events, []);
});

test('token updates retain the delay and use only the latest pending credential on each retry', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('second');
  deferRenewal(socket);
  timers.tick(1000);
  client.setToken('third');
  client.setToken('fourth');
  assert.equal(socket.sent.length, 1);
  assert.deepEqual(timers.delays, [2125]);
  timers.tick(2125);
  assert.equal(socket.sent[1].token, 'fourth');
  client.setToken('fifth');
  deferRenewal(socket);
  client.setToken('latest');
  assert.equal(socket.sent.length, 2);
  assert.deepEqual(timers.delays, [3125]);
  timers.tick(3125);
  assert.deepEqual(
    socket.sent.map((message) => message.token),
    ['second', 'fourth', 'latest'],
  );
  socket.receive({
    type: 'authenticationRenewed',
    requestId: socket.sent[2].requestId,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 120,
  });
  assert.equal(timers.pendingCount, 0);
  assert.equal(client.connected, true);
});

for (const [random, delay] of [
  [0, 3000],
  [0.999, 3249],
]) {
  test(`renewal retry jitter respects the server delay with random=${random}`, async (t) => {
    const { client, socket, timers } = await connectedClient(t, random, 'initial');
    client.setToken('refreshed');
    deferRenewal(socket);
    assert.deepEqual(timers.delays, [delay]);
    timers.tick(delay - 1);
    assert.equal(socket.sent.length, 1);
    timers.tick(1);
    assert.equal(socket.sent.length, 2);
    assert.deepEqual(timers.delays, [5000]);
  });
}

test('the third deferral exhausts the attempt limit and reconnects using the latest token', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  for (let attempt = 0; attempt < 2; attempt++) {
    deferRenewal(socket);
    timers.tick(3125);
  }
  assert.equal(socket.sent.length, 3);
  client.setToken('latest');
  deferRenewal(socket);
  assert.equal(client.connected, false);
  assert.equal(socket.sent.length, 3);
  assert.deepEqual(timers.delays, [120000, 1500]);
  timers.tick(1500);
  const replacement = FakeWebSocket.instances.at(-1);
  assert.deepEqual(replacement.protocols, ['simplestchat', 'auth.latest']);
  replacement.open();
  assert.equal(timers.pendingCount, 0);
});

test('late deferrals and token updates cannot extend the original fifteen-second budget', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('second');
  timers.tick(4900);
  deferRenewal(socket, CLOCK_EPOCH_MS / 1000 + 60, 5000);
  timers.tick(5125);
  assert.equal(socket.sent.length, 2);
  timers.tick(4900);
  deferRenewal(socket, CLOCK_EPOCH_MS / 1000 + 120, 5000);
  client.setToken('latest');
  assert.deepEqual(timers.delays, [75]);
  timers.tick(74);
  assert.equal(client.connected, true);
  timers.tick(1);
  assert.equal(timers.now, 15000);
  assert.equal(client.connected, false);
  assert.equal(socket.sent.length, 2, 'deadline cannot start another request');
});

test('a retry request timeout is shortened to the remaining overall budget', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  deferRenewal(socket);
  timers.tick(3125);
  timers.tick(4000);
  deferRenewal(socket);
  timers.tick(3125);
  assert.equal(socket.sent.length, 3);
  assert.deepEqual(timers.delays, [4750]);
  timers.tick(4749);
  assert.equal(client.connected, true);
  timers.tick(1);
  assert.equal(timers.now, 15000);
  assert.equal(client.connected, false);
});

for (const expirySeconds of [0, 2, 5]) {
  test(`accepted-token expiry bounds a deferred renewal with ${expirySeconds}s remaining`, async (t) => {
    const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
    client.setToken('refreshed');
    deferRenewal(socket, CLOCK_EPOCH_MS / 1000 + expirySeconds);
    if (expirySeconds > 0) {
      timers.tick(expirySeconds * 1000 - 1);
      assert.equal(client.connected, true);
      timers.tick(1);
    }
    assert.equal(client.connected, false);
    assert.equal(socket.sent.length, expirySeconds > 3 ? 2 : 1);
    assert.deepEqual(timers.delays, [120000, 1500]);
  });
}

test('later deferrals cannot extend the previously accepted expiry cap', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  deferRenewal(socket, CLOCK_EPOCH_MS / 1000 + 10);
  timers.tick(3125);
  deferRenewal(socket, CLOCK_EPOCH_MS / 1000 + 60);
  timers.tick(3125);
  assert.deepEqual(timers.delays, [3750]);
  timers.tick(3749);
  assert.equal(client.connected, true);
  timers.tick(1);
  assert.equal(timers.now, 10000);
  assert.equal(client.connected, false);
});

test('successful renewal retires its deadline and starts a fresh budget for the next refresh', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('second');
  deferRenewal(socket);
  timers.tick(3125);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: socket.sent.at(-1).requestId,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 60,
  });
  assert.equal(timers.pendingCount, 0);
  timers.tick(13000);
  client.setToken('third');
  assert.deepEqual(timers.delays, [5000]);
  timers.tick(4000);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: socket.sent.at(-1).requestId,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 120,
  });
  assert.equal(client.connected, true);
  assert.equal(timers.pendingCount, 0);
});

test('disconnect and replacement retire deferred callbacks and stale replies', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  client.setToken('old-refresh');
  const original = socket.sent[0];
  deferRenewal(socket);
  const staleRetry = timers.callbacks[0];
  client.disconnect();
  assert.equal(timers.pendingCount, 0);
  client.connect('other-account');
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  client.setToken('other-refresh');
  staleRetry();
  for (const type of [
    'authenticationRenewalDeferred',
    'authenticationRenewed',
    'authenticationRenewalFailed',
  ]) {
    socket.receive({
      type,
      requestId: original.requestId,
      retryAfterMs: 3000,
      expiresAt: CLOCK_EPOCH_MS / 1000 + 60,
    });
  }
  assert.equal(client.connected, true);
  assert.equal(replacement.sent.length, 1);
  assert.equal(replacement.sent[0].token, 'other-refresh');
  assert.deepEqual(timers.delays, [5000]);
});

test('a network close during deferred renewal cancels its retry before ordinary reconnect', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  deferRenewal(socket);
  const staleRetry = timers.callbacks[0];
  socket.close();
  assert.deepEqual(timers.delays, [120000, 1500]);
  timers.tick(1500);
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  staleRetry();
  timers.tick(16000);
  assert.equal(client.connected, true);
  assert.equal(socket.sent.length, 1);
  assert.equal(replacement.sent.length, 0);
  assert.deepEqual(replacement.protocols, ['simplestchat', 'auth.refreshed']);
  assert.equal(timers.pendingCount, 0);
});

test('clearing credentials cancels deferred retries and guest deferrals are ignored', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  deferRenewal(socket);
  const staleRetry = timers.callbacks[0];
  client.setToken(undefined);
  staleRetry();
  timers.tick(16000);
  assert.equal(socket.sent.length, 1);
  assert.equal(timers.pendingCount, 0);
  assert.equal(client.connected, true);
  client.disconnect();
  const guest = await connectedClient(t);
  guest.client.setToken('authenticated');
  guest.socket.receive({
    type: 'authenticationRenewalDeferred',
    requestId: 'auth-1',
    retryAfterMs: 3000,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 60,
  });
  assert.equal(guest.socket.sent.length, 0);
  assert.equal(guest.timers.pendingCount, 0);
});

test('delayed event delivery cannot accept an expired request or retry past the cycle deadline', async (t) => {
  const fixture = await connectedClient(t, 0.5, 'initial');
  fixture.client.setToken('refreshed');
  fixture.timers.advanceWithoutTimers(5000);
  fixture.socket.receive({
    type: 'authenticationRenewed',
    requestId: fixture.socket.sent[0].requestId,
    expiresAt: CLOCK_EPOCH_MS / 1000 + 60,
  });
  assert.equal(fixture.client.connected, false);
  fixture.client.disconnect();
  const delayed = await connectedClient(t, 0.5, 'initial');
  delayed.client.setToken('refreshed');
  deferRenewal(delayed.socket);
  delayed.timers.advanceWithoutTimers(15000);
  delayed.timers.tick(0);
  assert.equal(delayed.client.connected, false);
  assert.equal(delayed.socket.sent.length, 1);
});

for (const failure of ['timeout', 'rejected', 'send']) {
  test(`retry ${failure} retains the existing hard-failure recovery path`, async (t) => {
    const { client, socket, timers, errors } = await connectedClient(t, 0.5, 'initial');
    client.setToken('secret-token');
    deferRenewal(socket);
    if (failure === 'send')
      socket.send = () => {
        throw new Error('secret-token');
      };
    timers.tick(3125);
    if (failure === 'timeout') timers.tick(5000);
    if (failure === 'rejected')
      socket.receive({
        type: 'authenticationRenewalFailed',
        requestId: socket.sent.at(-1).requestId,
      });
    assert.equal(client.connected, false);
    assert.deepEqual(timers.delays, [120000, 1500]);
    assert.equal(JSON.stringify(errors).includes('secret-token'), false);
  });
}

test('refresh during a pending handshake renews on open and disconnected refresh uses the newest handshake', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  socket.close();
  client.setToken('second');
  timers.tick(1500);
  const replacement = FakeWebSocket.instances.at(-1);
  assert.deepEqual(replacement.protocols, ['simplestchat', 'auth.second']);
  client.setToken('third');
  assert.equal(replacement.sent.length, 0);
  replacement.open();
  assert.equal(replacement.sent[0].type, 'renewAuthentication');
  assert.equal(replacement.sent[0].token, 'third');
  replacement.receive({
    type: 'authenticationRenewed',
    requestId: replacement.sent[0].requestId,
    expiresAt: 1800000000,
  });
  assert.equal(timers.pendingCount, 0);
});

test('a renewal send failure during open cannot announce a usable replacement socket', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  const statuses = [],
    reconnects = [];
  client.setOnStatusChange((status) => statuses.push(status));
  client.setOnReconnected(() => reconnects.push('connected'));
  socket.close();
  timers.tick(1500);
  const replacement = FakeWebSocket.instances.at(-1);
  client.setToken('refreshed');
  replacement.send = () => {
    throw new Error('fixture send failure');
  };
  replacement.open();
  assert.equal(client.connected, false);
  assert.equal(statuses.at(-1), 'disconnected');
  assert.deepEqual(reconnects, []);
  assert.deepEqual(timers.delays, [120000, 1500]);
});

test('overlapping refreshes serialize and stale acknowledgements cannot overwrite the latest token', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  client.setToken('second');
  client.setToken('third');
  assert.equal(socket.sent.length, 1);
  const first = socket.sent[0];
  socket.receive({
    type: 'authenticationRenewed',
    requestId: first.requestId,
    expiresAt: 1800000000,
  });
  const second = socket.sent[1];
  assert.equal(second.token, 'third');
  socket.receive({ type: 'authenticationRenewalFailed', requestId: first.requestId });
  assert.equal(client.connected, true);
  socket.receive({
    type: 'authenticationRenewed',
    requestId: second.requestId,
    expiresAt: 1800000001,
  });
  assert.equal(timers.pendingCount, 0);
  socket.close();
  timers.tick(1500);
  assert.deepEqual(FakeWebSocket.instances.at(-1).protocols, ['simplestchat', 'auth.third']);
});

for (const failure of ['timeout', 'rejected', 'send']) {
  test(`renewal ${failure} enters bounded recovery without logging credentials`, async (t) => {
    const { client, socket, timers, FakeWebSocket, errors } = await connectedClient(
      t,
      0.5,
      'initial',
    );
    if (failure === 'send')
      socket.send = () => {
        throw new Error('secret-token');
      };
    client.setToken('secret-token');
    if (failure === 'timeout') timers.tick(5000);
    if (failure === 'rejected')
      socket.receive({ type: 'authenticationRenewalFailed', requestId: socket.sent[0].requestId });
    assert.equal(client.connected, false);
    assert.deepEqual(timers.delays, [120000, 1500]);
    assert.equal(JSON.stringify(errors).includes('secret-token'), false);
    timers.tick(1500);
    const replacement = FakeWebSocket.instances.at(-1);
    assert.deepEqual(replacement.protocols, ['simplestchat', 'auth.secret-token']);
    replacement.open();
    assert.equal(replacement.sent.length, 0, 'new handshake already uses the refreshed credential');
    assert.equal(timers.pendingCount, 0);
  });
}

test('logout and replacement sockets retire renewal timers and late results', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  const requestId = socket.sent[0].requestId;
  client.disconnect();
  assert.equal(timers.pendingCount, 0);
  client.connect('other-account');
  const replacement = FakeWebSocket.instances.at(-1);
  replacement.open();
  socket.receive({ type: 'authenticationRenewed', requestId, expiresAt: 1800000000 });
  timers.tick(6000);
  assert.equal(client.connected, true);
  assert.deepEqual(replacement.protocols, ['simplestchat', 'auth.other-account']);
  assert.equal(replacement.sent.length, 0);
});

test('clearing the token retires renewal and a guest socket is never upgraded in place', async (t) => {
  const { client, socket, timers } = await connectedClient(t, 0.5, 'initial');
  client.setToken('refreshed');
  const requestId = socket.sent[0].requestId;
  client.setToken(undefined);
  socket.receive({ type: 'authenticationRenewed', requestId, expiresAt: 1800000000 });
  assert.equal(timers.pendingCount, 0);
  client.disconnect();
  const guest = await connectedClient(t);
  guest.client.setToken('authenticated');
  assert.equal(guest.socket.sent.length, 0);
});

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
  const reply = transportReply('valid', socket.sent.at(-1).requestId);
  socket.receive(reply);
  assert.deepEqual(await pending, reply);
  assert.equal(timers.pendingCount, 0);
});

test('late success and error replies cannot settle a retry or become application events', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const events = [];
  client.setOnMessage((message) => events.push(message));
  const expired = assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
    /Timeout waiting for transportCreated/,
  );
  const originalId = socket.sent[0].requestId;
  const originalTimeout = timers.callbacks[0];
  timers.tick(10);
  await expired;

  const retry = client.request({ type: 'createSendTransport' }, 'transportCreated', 10);
  const retryId = socket.sent[1].requestId;
  assert.notEqual(retryId, originalId);
  socket.receive(transportReply('expired', originalId));
  socket.receive({ type: 'error', requestId: originalId, message: 'Old operation failed' });
  originalTimeout();
  assert.equal(timers.pendingCount, 1);
  assert.deepEqual(events, []);
  const reply = transportReply('retry', retryId);
  socket.receive(reply);
  assert.deepEqual(await retry, reply);
  socket.receive(reply);
  socket.receive({ type: 'error', requestId: retryId, message: 'Duplicate reply' });
  assert.deepEqual(events, []);
  assert.equal(timers.pendingCount, 0);
});

test('server errors reject the request and leave the next response available', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const rejected = assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
    /Transport limit reached/,
  );
  socket.receive({
    type: 'error',
    requestId: socket.sent.at(-1).requestId,
    message: 'Transport limit reached',
  });
  await rejected;
  assert.equal(timers.pendingCount, 0);

  const retry = client.request({ type: 'createSendTransport' }, 'transportCreated', 10);
  const reply = transportReply('replacement', socket.sent.at(-1).requestId);
  socket.receive(reply);
  timers.tick(10);
  assert.deepEqual(await retry, reply);
});

test('simultaneous transport requests resolve by ID when replies arrive in reverse order', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const send = client.request({ type: 'createSendTransport' }, 'transportCreated');
  const recv = client.request({ type: 'createRecvTransport' }, 'transportCreated');
  const sendId = socket.sent[0].requestId;
  const recvId = socket.sent[1].requestId;
  assert.match(sendId, /^[A-Za-z0-9_-]{1,64}$/);
  assert.notEqual(sendId, recvId);
  socket.receive(transportReply('receiver', recvId));
  assert.equal((await recv).transportId, 'receiver');
  assert.equal(timers.pendingCount, 1);
  socket.receive(transportReply('sender', sendId));
  assert.equal((await send).transportId, 'sender');
  assert.equal(timers.pendingCount, 0);
});

test('a correlated error rejects only its operation regardless of request order', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const send = client.request({ type: 'createSendTransport' }, 'transportCreated');
  const recv = assert.rejects(
    client.request({ type: 'createRecvTransport' }, 'transportCreated'),
    /Receiver rejected/,
  );
  socket.receive({
    type: 'error',
    requestId: socket.sent[1].requestId,
    message: 'Receiver rejected',
  });
  await recv;
  assert.equal(timers.pendingCount, 1);
  socket.receive(transportReply('sender', socket.sent[0].requestId));
  assert.equal((await send).transportId, 'sender');
  assert.equal(timers.pendingCount, 0);
});

test('missing or unknown IDs and mismatched response types cannot consume a request', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const events = [];
  client.setOnMessage((message) => events.push(message));
  const pending = client.request({ type: 'createRecvTransport' }, 'transportCreated');
  const requestId = socket.sent[0].requestId;
  socket.receive(transportReply('legacy'));
  socket.receive(transportReply('unknown', 'unknown'));
  socket.receive({ type: 'producerCreated', requestId, producerId: 'wrong-type' });
  socket.receive({ type: 'error', requestId: 'unknown', message: 'Unrelated error' });
  assert.equal(timers.pendingCount, 1);
  assert.deepEqual(events, []);

  const notice = { type: 'error', message: 'Uncorrelated connection notice' };
  const pause = { type: 'producerPaused', producerId: 'producer' };
  const social = { type: 'socialError', requestId, message: 'Social operation failed' };
  const socialReply = {
    type: 'socialResponse',
    requestId,
    action: 'changeNickname',
    data: { nickname: 'Guest' },
  };
  for (const event of [notice, pause, social, socialReply]) socket.receive(event);
  assert.deepEqual(events, [notice, pause, social, socialReply]);
  assert.equal(timers.pendingCount, 1);
  socket.receive(transportReply('current', requestId));
  assert.equal((await pending).transportId, 'current');
});

test('a reply at the deadline rejects even when browser timeout callbacks have been delayed', async (t) => {
  for (const responseType of ['success', 'error']) {
    const { client, socket, timers } = await connectedClient(t);
    const expired = assert.rejects(
      client.request({ type: 'createRecvTransport' }, 'transportCreated', 10),
      /Timeout waiting for transportCreated/,
    );
    const requestId = socket.sent[0].requestId;
    timers.advanceWithoutTimers(10);
    socket.receive(
      responseType === 'success'
        ? transportReply('too-late', requestId)
        : { type: 'error', requestId, message: 'Too late' },
    );
    await expired;
    assert.equal(timers.pendingCount, 0);
  }
});

test('invalid request deadlines fail before sending or retaining timers', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  for (const timeout of [0, -1, NaN, Infinity, 2 ** 31]) {
    await assert.rejects(
      client.request({ type: 'createRecvTransport' }, 'transportCreated', timeout),
      /Invalid request timeout/,
    );
  }
  assert.equal(socket.sent.length, 0);
  assert.equal(timers.pendingCount, 0);
});

test('a synchronous send failure removes the pending media request before a retry', async (t) => {
  const { client, socket, timers } = await connectedClient(t);
  const send = socket.send;
  socket.send = () => {
    throw new Error('send failed');
  };
  await assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
    /send failed/,
  );
  assert.equal(timers.pendingCount, 0);
  socket.send = send;
  const retry = client.request({ type: 'createSendTransport' }, 'transportCreated', 10);
  const reply = transportReply('replacement', socket.sent.at(-1).requestId);
  socket.receive(reply);
  timers.tick(10);
  assert.deepEqual(await retry, reply);
});

test('a disconnected media request fails immediately without retaining a timeout', async (t) => {
  const { client, timers } = await connectedClient(t);
  client.disconnect();
  await assert.rejects(
    client.request({ type: 'createSendTransport' }, 'transportCreated', 10),
    /not connected/,
  );
  assert.equal(timers.pendingCount, 0);
});

test('disconnect rejects all requests, cancels their timers, and permits a fresh connection', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
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
  const oldId = socket.sent[1].requestId;
  assert.notEqual(replacement.sent[0].requestId, oldId);
  replacement.receive(transportReply('old-connection', oldId));
  assert.equal(timers.pendingCount, 1);
  const reply = transportReply('new-connection', replacement.sent.at(-1).requestId);
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
  const reply = transportReply('current', replacement.sent.at(-1).requestId);
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
  const reply = transportReply('current', replacement.sent.at(-1).requestId);
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

test('restart deadline rejects unacknowledged work after socket open and preserves identity for retry', async (t) => {
  const { client, socket, timers, FakeWebSocket } = await connectedClient(t);
  const failures = [];
  client.setToken('fixture-current-token');
  client.setOnReconnectFailed(() => failures.push('expired'));
  socket.receive({ type: 'serverRestarting', reason: 'Server shutting down' });
  socket.close();
  timers.tick(1500);
  const stalled = FakeWebSocket.instances.at(-1);
  stalled.open();
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
