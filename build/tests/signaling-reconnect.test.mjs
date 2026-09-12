import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import observation from '../../web/e2e/signaling-reconnect.cjs';

const { installSignalingReconnectObservation } = observation;
const secret = 'PRIVATE_SIGNALING_RECONNECT_SENTINEL';
const plain = value => JSON.parse(JSON.stringify(value));

// The exported browser function must serialize without module closures. This
// fixture never starts a browser, opens a socket, or requests a media device.
function fixture(t, { href = 'http://127.0.0.1:3109/room', withConstructor = true, install = true } = {}) {
  let now = 1000;
  const constructions = [], sends = [], closes = [], forbiddenCalls = [];
  const forbidden = name => () => { forbiddenCalls.push(name); throw new Error(`Unexpected ${name}`); };
  const sentResult = Object.freeze({ nativeSendResult: true });
  const closedResult = Object.freeze({ nativeCloseResult: true });
  class NativeSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;
    static fixtureMarker = 'native-static-property';
    readyState = NativeSocket.CONNECTING;
    listeners = new Map();
    sendError = null;
    constructor(...args) {
      constructions.push({ socket: this, args, newTarget: new.target });
      const parsed = new URL(args[0], href);
      if (parsed.protocol === 'http:') parsed.protocol = 'ws:';
      if (parsed.protocol === 'https:') parsed.protocol = 'wss:';
      this.url = parsed.href;
      this.protocol = secret;
    }
    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? [];
      listeners.push(listener);
      this.listeners.set(type, listeners);
    }
    emit(type, event = {}) {
      if (type === 'open') this.readyState = NativeSocket.OPEN;
      if (type === 'close') this.readyState = NativeSocket.CLOSED;
      for (const listener of this.listeners.get(type) ?? []) listener.call(this, event);
      this[`on${type}`]?.call(this, event);
    }
    send(...args) {
      sends.push({ socket: this, args });
      if (this.sendError) throw this.sendError;
      return sentResult;
    }
    close(...args) {
      closes.push({ socket: this, args });
      this.readyState = NativeSocket.CLOSING;
      return closedResult;
    }
  }
  const nativeSend = NativeSocket.prototype.send;
  const nativeClose = NativeSocket.prototype.close;
  const location = new URL(href);
  const window = { WebSocket: withConstructor ? NativeSocket : undefined, location };
  const browserInstall = runInNewContext(`(${installSignalingReconnectObservation.toString()})`, {
    window, location, URL, performance: { now: () => now },
    setTimeout: forbidden('setTimeout'), clearTimeout: forbidden('clearTimeout'),
    setInterval: forbidden('setInterval'), clearInterval: forbidden('clearInterval'),
    fetch: forbidden('fetch'), XMLHttpRequest: forbidden('XMLHttpRequest'),
    navigator: { mediaDevices: { getUserMedia: forbidden('getUserMedia'), getDisplayMedia: forbidden('getDisplayMedia') } },
  }, { timeout: 1000 });
  if (install) browserInstall();
  t.after(() => assert.deepEqual(forbiddenCalls, [], 'the observer must not add timers, network requests or media access'));
  return {
    window, NativeSocket, nativeSend, nativeClose, constructions, sends, closes, sentResult, closedResult,
    install: browserInstall,
    create: (...args) => new window.WebSocket(...args),
    snapshot: () => plain(window.__communitySignalingReconnect.snapshot()),
    closeCurrent: () => plain(window.__communitySignalingReconnect.closeCurrent()),
    advance(ms = 1) { now += ms; },
  };
}

function assertPrivate(snapshot) {
  assert.deepEqual(Object.keys(snapshot).sort(), ['schemaVersion', 'socketsObserved', 'socketsRetained',
    'openSocketOrdinals', 'counters', 'events', 'droppedEvents', 'droppedSockets'].sort());
  assert.deepEqual(Object.keys(snapshot.counters).sort(), ['sentReconnect', 'sentJoinRoom', 'sentCreateSendTransport',
    'sentCreateRecvTransport', 'sentProduce', 'sentGetRoomSnapshot', 'receivedReconnectResult',
    'reconnectSuccess', 'reconnectFailure', 'receivedRoomSnapshot'].sort());
  assert.equal(snapshot.schemaVersion, 1);
  assert.ok(snapshot.socketsRetained <= 16);
  assert.ok(snapshot.events.length <= 64);
  for (const value of Object.values(snapshot.counters)) assert.ok(Number.isInteger(value) && value >= 0 && value <= 10000);
  const serialized = JSON.stringify(snapshot);
  for (const forbidden of [secret, 'reconnectToken', 'participantId', 'roomId', 'requestId', 'protocols', 'ws://', 'wss://']) {
    assert.equal(serialized.includes(forbidden), false, `observable evidence exposed ${forbidden}`);
  }
}

function frame(type, details = {}) {
  return JSON.stringify({ type, ...details });
}

function received(socket, type, details = {}) {
  socket.emit('message', { data: frame(type, details) });
}

test('signaling observation serializes independently and preserves native construction and prototypes', t => {
  const f = fixture(t);
  const protocols = Object.freeze(['simplestchat', secret]);
  let coercions = 0;
  const argument = { toString() { coercions++; return '/ws'; } };
  const socket = f.create(argument, protocols);
  assert.equal(coercions, 1, 'observation must not repeat native URL argument coercion');
  assert.ok(socket instanceof f.NativeSocket);
  assert.ok(socket instanceof f.window.WebSocket);
  assert.equal(Object.getPrototypeOf(socket), f.NativeSocket.prototype);
  assert.equal(f.window.WebSocket.prototype, f.NativeSocket.prototype);
  assert.equal(f.NativeSocket.prototype.send, f.nativeSend);
  assert.equal(f.NativeSocket.prototype.close, f.nativeClose);
  assert.equal(f.window.WebSocket.fixtureMarker, 'native-static-property');
  assert.equal(f.window.WebSocket.OPEN, f.NativeSocket.OPEN);
  assert.equal(f.constructions[0].args[0], argument);
  assert.equal(f.constructions[0].args[1], protocols);
  class Derived extends f.window.WebSocket {}
  const derived = new Derived('/ws', protocols);
  assert.equal(Object.getPrototypeOf(derived), Derived.prototype);
  assert.equal(f.constructions[1].newTarget, Derived);
  assert.throws(() => f.window.WebSocket('/ws'), /class constructor|without 'new'/i);
  assert.equal(f.snapshot().socketsObserved, 2);
  assertPrivate(f.snapshot());
});

test('only the exact owned same-origin signaling endpoint is observed', t => {
  const f = fixture(t);
  const unrelated = [
    'ws://example.invalid:3109/ws', 'ws://localhost:3109/ws', 'ws://127.0.0.1:3110/ws',
    'wss://127.0.0.1:3109/ws', '/other', '/ws/', `/ws?token=${secret}`, `/ws#${secret}`,
    `ws://${secret}@127.0.0.1:3109/ws`,
  ].map(url => f.create(url));
  for (const socket of unrelated) {
    assert.equal(socket.send, f.nativeSend);
    assert.equal(socket.listeners.size, 0);
    socket.emit('open');
    socket.send(frame('reconnect', { reconnectToken: secret }));
    received(socket, 'reconnectResult', { success: true, reconnectToken: secret });
  }
  assert.equal(f.snapshot().socketsObserved, 0);
  assert.deepEqual(f.snapshot().openSocketOrdinals, []);
  assert.throws(() => f.closeCurrent());
  assert.deepEqual(f.closes, []);
  const owned = f.create('/ws');
  owned.emit('open');
  assert.equal(f.snapshot().socketsObserved, 1);
  assert.deepEqual(f.snapshot().openSocketOrdinals, [1]);
  assertPrivate(f.snapshot());
});

test('secure pages observe wss on their own host but never plaintext or another endpoint', t => {
  const f = fixture(t, { href: 'https://localhost:3143/room' });
  const owned = f.create('wss://localhost:3143/ws');
  owned.emit('open');
  for (const url of ['ws://localhost:3143/ws', 'wss://127.0.0.1:3143/ws', 'wss://localhost:3143/elsewhere']) {
    const other = f.create(url);
    other.emit('open');
    assert.equal(other.send, f.nativeSend);
  }
  assert.equal(f.snapshot().socketsObserved, 1);
  f.closeCurrent();
  assert.equal(f.closes.length, 1);
  assert.equal(f.closes[0].socket, owned);
});

test('ordinary native sends preserve argument identity, returns, exceptions and message delivery', t => {
  const f = fixture(t), socket = f.create('/ws');
  socket.emit('open');
  const payload = Object.freeze({ privateFixture: secret });
  const extra = Object.freeze({ nativeExtraArgument: true });
  assert.equal(socket.send(payload, extra), f.sentResult);
  assert.equal(f.sends[0].socket, socket);
  assert.equal(f.sends[0].args[0], payload);
  assert.equal(f.sends[0].args[1], extra);
  const failure = new Error(secret);
  socket.sendError = failure;
  assert.throws(() => socket.send(frame('reconnect', { reconnectToken: secret })), error => error === failure);
  assert.equal(f.snapshot().counters.sentReconnect, 0);
  socket.sendError = null;
  const delivered = [];
  const applicationHandler = event => delivered.push(event);
  socket.onmessage = applicationHandler;
  socket.addEventListener('message', event => delivered.push(event));
  const message = Object.freeze({ data: frame('reconnectResult', { success: true, reconnectToken: secret }) });
  socket.emit('message', message);
  assert.equal(socket.onmessage, applicationHandler);
  assert.deepEqual(delivered, [message, message]);
  assertPrivate(f.snapshot());
});

test('borrowed send methods preserve native receivers without attributing an unrelated socket to the owned connection', t => {
  const f = fixture(t), owned = f.create('/ws'), unrelated = f.create('ws://example.invalid/ws');
  owned.emit('open'); unrelated.emit('open');
  const data = frame('reconnect', { reconnectToken: secret });
  assert.equal(owned.send.call(unrelated, data), f.sentResult);
  assert.equal(f.sends[0].socket, unrelated);
  assert.equal(f.sends[0].args[0], data);
  assert.equal(f.snapshot().counters.sentReconnect, 0);
  owned.send(data);
  assert.equal(f.snapshot().counters.sentReconnect, 1);
});

test('the explicit fault closes exactly the one active owned socket through its native close', t => {
  const f = fixture(t), closed = f.create('/ws'), current = f.create('/ws'), connecting = f.create('/ws');
  const other = f.create('ws://example.invalid/ws');
  closed.emit('open'); closed.emit('close'); current.emit('open'); other.emit('open');
  f.advance(25);
  const request = f.closeCurrent();
  assert.deepEqual(Object.keys(request).sort(), ['requestedAtMs', 'socketOrdinal']);
  assert.equal(request.socketOrdinal, 2);
  assert.ok(Number.isFinite(request.requestedAtMs) && request.requestedAtMs >= 0);
  assert.equal(f.closes.length, 1);
  assert.equal(f.closes[0].socket, current);
  assert.equal(f.closes[0].args[0], 4000);
  assert.equal(typeof f.closes[0].args[1], 'string');
  assert.doesNotMatch(f.closes[0].args[1], /PRIVATE/);
  assert.equal(connecting.readyState, f.NativeSocket.CONNECTING);
  assert.equal(other.readyState, f.NativeSocket.OPEN);
  assert.throws(() => f.closeCurrent(), 'a second fault cannot close a connecting or already closing socket');
  assert.equal(f.closes.length, 1);
  assertPrivate(f.snapshot());
});

test('the fault uses the captured native close rather than an instance replacement', t => {
  const f = fixture(t), socket = f.create('/ws'); socket.emit('open');
  socket.close = () => assert.fail('the injected fault must use the original native close');
  f.closeCurrent();
  assert.equal(f.closes.length, 1);
  assert.equal(f.closes[0].socket, socket);
});

test('ambiguous active owned sockets fail loudly without closing either connection', t => {
  const f = fixture(t), first = f.create('/ws'), second = f.create('/ws');
  first.emit('open'); second.emit('open');
  assert.throws(() => f.closeCurrent());
  assert.deepEqual(f.closes, []);
});

test('reconnect success retains only bounded primitive evidence and distinguishes full-rejoin fallback', t => {
  const f = fixture(t), original = f.create('/ws');
  original.emit('open');
  original.send(frame('joinRoom', { roomId: secret, participantName: secret }));
  original.send(frame('createSendTransport'));
  original.send(frame('createRecvTransport'));
  original.send(frame('produce', { rtpParameters: { secret } }));
  const before = f.snapshot();
  f.closeCurrent(); original.emit('close', { code: 4000, reason: secret });
  f.advance(100);
  const resumed = f.create('/ws'); resumed.emit('open');
  resumed.send(frame('reconnect', { participantId: secret, roomId: secret, reconnectToken: secret }));
  received(resumed, 'reconnectResult', { success: true, participantId: secret, reconnectToken: secret });
  resumed.send(frame('getRoomSnapshot', { requestId: secret }));
  received(resumed, 'socialResponse', { action: 'getRoomSnapshot', requestId: secret, data: { room: secret, peers: [secret] } });
  const restored = f.snapshot();
  assert.equal(restored.counters.sentReconnect - before.counters.sentReconnect, 1);
  assert.equal(restored.counters.reconnectSuccess - before.counters.reconnectSuccess, 1);
  assert.equal(restored.counters.reconnectFailure, 0);
  assert.equal(restored.counters.receivedReconnectResult, 1);
  assert.equal(restored.counters.sentGetRoomSnapshot, 1);
  assert.equal(restored.counters.receivedRoomSnapshot, 1);
  for (const key of ['sentJoinRoom', 'sentCreateSendTransport', 'sentCreateRecvTransport', 'sentProduce']) {
    assert.equal(restored.counters[key], before.counters[key], `${key} must distinguish restoration from replacement`);
  }
  assert.equal(restored.socketsObserved, 2);
  assert.equal(restored.droppedEvents, 0); assert.equal(restored.droppedSockets, 0);
  assertPrivate(restored);
  const allowedEvents = new Set(['open', 'close', 'reconnect-sent', 'reconnect-result', 'room-snapshot']);
  for (const event of restored.events) {
    assert.ok(allowedEvents.has(event.event));
    assert.deepEqual(Object.keys(event).sort(), (event.event === 'reconnect-result'
      ? ['elapsedMs', 'event', 'socketOrdinal', 'success'] : ['elapsedMs', 'event', 'socketOrdinal']).sort());
    assert.ok(Number.isInteger(event.socketOrdinal) && event.socketOrdinal > 0);
    assert.ok(Number.isFinite(event.elapsedMs) && event.elapsedMs >= 0);
    if (Object.hasOwn(event, 'success')) assert.equal(typeof event.success, 'boolean');
  }
  received(resumed, 'reconnectResult', { success: false, participantId: secret });
  resumed.send(frame('joinRoom', { roomId: secret, password: secret }));
  resumed.send(frame('createSendTransport'));
  resumed.send(frame('createRecvTransport'));
  resumed.send(frame('produce', { transportId: secret, rtpParameters: { secret } }));
  const fallback = f.snapshot();
  assert.equal(fallback.counters.reconnectFailure, 1);
  for (const key of ['sentJoinRoom', 'sentCreateSendTransport', 'sentCreateRecvTransport', 'sentProduce']) {
    assert.equal(fallback.counters[key], restored.counters[key] + 1);
  }
  assertPrivate(fallback);
});

test('unrecognized, malformed and nonboolean protocol fields cannot invent successful reconnect evidence', t => {
  const f = fixture(t), socket = f.create('/ws');
  socket.emit('open');
  for (const data of [secret, '{', 'null', '[]', frame('chatMessage', { content: secret })]) {
    socket.send(data); socket.emit('message', { data });
  }
  for (const success of ['true', 1, {}, null]) received(socket, 'reconnectResult', { success, reconnectToken: secret });
  received(socket, 'socialResponse', { action: 'listRoomBans', requestId: secret, data: [secret] });
  received(socket, 'socialError', { action: 'getRoomSnapshot', requestId: secret, message: secret });
  const snapshot = f.snapshot();
  assert.equal(snapshot.counters.sentReconnect, 0);
  assert.equal(snapshot.counters.reconnectSuccess, 0);
  assert.equal(snapshot.counters.reconnectFailure, 0);
  assert.equal(snapshot.counters.receivedRoomSnapshot, 0);
  assertPrivate(snapshot);
});

test('snapshot results cannot mutate retained counters or event history', t => {
  const f = fixture(t), socket = f.create('/ws'); socket.emit('open');
  socket.send(frame('reconnect', { reconnectToken: secret }));
  const snapshot = f.window.__communitySignalingReconnect.snapshot();
  try { snapshot.counters.sentReconnect = 999; snapshot.events.length = 0; snapshot.openSocketOrdinals.push(999); } catch {}
  const after = f.snapshot();
  assert.equal(after.counters.sentReconnect, 1);
  assert.ok(after.events.length > 0);
  assert.deepEqual(after.openSocketOrdinals, [1]);
});

test('missing native instrumentation fails loudly instead of looking like a successful empty capture', t => {
  const f = fixture(t, { withConstructor: false, install: false });
  assert.doesNotThrow(() => f.install(), 'passive setup must not mask unrelated navigation failures');
  assert.throws(() => f.window.__communitySignalingReconnect.snapshot(), /Native WebSocket unavailable/);
  assert.throws(() => f.window.__communitySignalingReconnect.closeCurrent(), /Native WebSocket unavailable/);
  assert.equal(f.window.WebSocket, undefined);
  assert.deepEqual(f.closes, []);
});

for (const replacement of ['constructor', 'api', 'send']) {
  test(`replaced ${replacement} instrumentation fails loudly without injecting a fault`, t => {
    const f = fixture(t), socket = f.create('/ws'); socket.emit('open');
    const api = f.window.__communitySignalingReconnect;
    if (replacement === 'constructor') f.window.WebSocket = class Replacement {};
    if (replacement === 'api') f.window.__communitySignalingReconnect = {};
    if (replacement === 'send') socket.send = () => {};
    assert.throws(() => api.snapshot());
    assert.throws(() => api.closeCurrent());
    assert.deepEqual(f.closes, []);
  });
}

test('socket capture caps preserve native construction but reject incomplete evidence and faults', t => {
  const f = fixture(t);
  for (let index = 0; index < 16; index++) f.create('/ws');
  assert.equal(f.snapshot().socketsObserved, 16);
  assert.equal(f.snapshot().socketsRetained, 16);
  assert.equal(f.snapshot().droppedSockets, 0);
  f.create('/ws');
  assert.equal(f.constructions.length, 17);
  assert.throws(() => f.snapshot());
  assert.throws(() => f.closeCurrent());
  assert.deepEqual(f.closes, []);
});

test('event capture caps preserve native sends but reject incomplete evidence and faults', t => {
  const f = fixture(t), socket = f.create('/ws'); socket.emit('open');
  for (let index = 0; index < 63; index++) socket.send(frame('reconnect', { reconnectToken: secret }));
  assert.equal(f.snapshot().events.length, 64);
  assert.equal(f.snapshot().droppedEvents, 0);
  socket.send(frame('reconnect', { reconnectToken: secret }));
  assert.equal(f.sends.length, 64);
  assert.throws(() => f.snapshot());
  assert.throws(() => f.closeCurrent());
  assert.deepEqual(f.closes, []);
});

test('counter caps bound non-event signaling evidence without stopping native writes', t => {
  const f = fixture(t), socket = f.create('/ws'); socket.emit('open');
  const message = frame('joinRoom', { roomId: secret, password: secret });
  for (let index = 0; index < 10000; index++) socket.send(message);
  assert.equal(f.snapshot().counters.sentJoinRoom, 10000);
  assert.equal(f.snapshot().events.length, 1);
  assertPrivate(f.snapshot());
  socket.send(message);
  assert.equal(f.sends.length, 10001);
  assert.throws(() => f.snapshot());
  assert.throws(() => f.closeCurrent());
  assert.deepEqual(f.closes, []);
});
