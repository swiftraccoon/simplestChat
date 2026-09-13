import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import observation from '../e2e/lifecycle-observer.cjs';

const { installLifecycleObservation } = observation;
const secret = 'PRIVATE_LIFECYCLE_SENTINEL';
const plain = (value) => JSON.parse(JSON.stringify(value));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function fixture({ unsupported = false } = {}) {
  const constructions = [];
  const references = [];
  const elements = [];
  const calls = [];
  let capture = () => Promise.resolve(new NativeStream([]));
  class NativeTarget {
    listeners = new Map();
    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? new Set();
      listeners.add(listener);
      this.listeners.set(type, listeners);
    }
    removeEventListener(type, listener) {
      this.listeners.get(type)?.delete(listener);
    }
    emit(type, data) {
      for (const listener of this.listeners.get(type) ?? []) listener.call(this, { type, data });
    }
    listenerCount() {
      return [...this.listeners.values()].reduce((count, listeners) => count + listeners.size, 0);
    }
  }
  class NativePeer extends NativeTarget {
    static fixtureMarker = 'native-peer';
    connectionState = 'new';
    signalingState = 'stable';
    stats = new Map();
    constructor(...args) {
      super();
      if (args[0]?.throw) throw args[0].throw;
      constructions.push({ kind: 'peer', object: this, args, newTarget: new.target });
    }
    close(...args) {
      if (!(this instanceof NativePeer)) throw new TypeError('Illegal invocation');
      if (this.closeError) throw this.closeError;
      calls.push({ kind: 'peerClose', object: this, args });
      this.connectionState = 'closed';
      this.signalingState = 'closed';
      return 17;
    }
    getStats() {
      if (!(this instanceof NativePeer)) throw new TypeError('Illegal invocation');
      calls.push({ kind: 'stats', object: this });
      if (this.statsError) return Promise.reject(this.statsError);
      return Promise.resolve(this.stats);
    }
  }
  class NativeTrack extends NativeTarget {
    readyState = 'live';
    label = secret;
    id = secret;
    constructor(kind = 'video') {
      super();
      this.kind = kind;
    }
    stop(...args) {
      if (!(this instanceof NativeTrack)) throw new TypeError('Illegal invocation');
      if (this.stopError) throw this.stopError;
      calls.push({ kind: 'stop', object: this, args });
      this.readyState = 'ended';
      return 23;
    }
    clone() {
      if (!(this instanceof NativeTrack)) throw new TypeError('Illegal invocation');
      if (this.cloneError) throw this.cloneError;
      const track = new NativeTrack(this.kind);
      track.readyState = this.readyState;
      return track;
    }
  }
  class NativeStream {
    constructor(tracks) {
      this.tracks = tracks;
    }
    getTracks() {
      if (!(this instanceof NativeStream)) throw new TypeError('Illegal invocation');
      return [...this.tracks];
    }
    clone() {
      if (!(this instanceof NativeStream)) throw new TypeError('Illegal invocation');
      // Native stream cloning does not invoke the JS track.clone wrapper.
      return new NativeStream(this.tracks.map((track) => new NativeTrack(track.kind)));
    }
  }
  class NativeSocket extends NativeTarget {
    static OPEN = 1;
    readyState = 1;
    constructor(url, ...args) {
      super();
      if (url instanceof Error) throw url;
      this.url = url;
      constructions.push({
        kind: 'socket',
        object: this,
        args: [url, ...args],
        newTarget: new.target,
      });
    }
    close(...args) {
      if (!(this instanceof NativeSocket)) throw new TypeError('Illegal invocation');
      if (this.closeError) throw this.closeError;
      calls.push({ kind: 'socketClose', object: this, args });
      this.readyState = 2;
    }
    send(...args) {
      if (!(this instanceof NativeSocket)) throw new TypeError('Illegal invocation');
      if (this.sendError) throw this.sendError;
      calls.push({ kind: 'socketSend', object: this, args });
      return 31;
    }
  }
  class ControlledWeakRef {
    constructor(object) {
      this.object = object;
      references.push(this);
    }
    deref() {
      return this.object;
    }
  }
  const window = {
    RTCPeerConnection: unsupported ? undefined : NativePeer,
    WebSocket: NativeSocket,
    MediaStreamTrack: NativeTrack,
    MediaStream: NativeStream,
    EventTarget: NativeTarget,
    location: { origin: 'https://localhost:3119' },
  };
  const navigator = {
    mediaDevices: {
      getUserMedia(...args) {
        if (this !== navigator.mediaDevices) throw new TypeError('Illegal invocation');
        calls.push({ kind: 'capture', args });
        return capture(...args);
      },
    },
  };
  const document = {
    querySelectorAll(selector) {
      assert.equal(selector, 'audio, video');
      return elements;
    },
  };
  const forbidden = () =>
    assert.fail('observer must not use timers, network or capture on install');
  const install = runInNewContext(`(${installLifecycleObservation.toString()})`, {
    window,
    navigator,
    document,
    URL,
    WeakRef: ControlledWeakRef,
    setTimeout: forbidden,
    setInterval: forbidden,
    fetch: forbidden,
    XMLHttpRequest: forbidden,
  });
  install();
  return {
    window,
    navigator,
    document,
    constructions,
    references,
    elements,
    calls,
    NativePeer,
    NativeTrack,
    NativeStream,
    NativeSocket,
    install,
    observer: window.__lifecycle,
    snapshot: () => plain(window.__lifecycle.snapshot()),
    mediaSample: async () => plain(await window.__lifecycle.mediaSample()),
    setCapture(value) {
      capture = value;
    },
    createPeer: (...args) => new window.RTCPeerConnection(...args),
    createSocket: (url = `wss://localhost:3119/ws?token=${secret}`) => new window.WebSocket(url),
    capture: (...args) => navigator.mediaDevices.getUserMedia(...args),
  };
}

function assertPrivate(value) {
  const json = JSON.stringify(value);
  for (const forbidden of [secret, 'wss:', 'https:', 'localhost', 'candidate:', 'sdp', 'label'])
    assert.equal(json.includes(forbidden), false, `observer exposed ${forbidden}`);
}

test('observer serializes independently, stays passive, and exposes only frozen methods', () => {
  const f = fixture();
  assert.deepEqual(f.calls, []);
  assert.equal(Object.isFrozen(f.observer), true);
  assert.deepEqual(Object.keys(f.observer), ['snapshot', 'mediaSample', 'closeCurrentSocket']);
  assert.deepEqual(f.snapshot(), {
    peersCreated: 0,
    peersClosed: 0,
    tracksObserved: 0,
    tracksEnded: 0,
    capturesRequested: 0,
    capturesResolved: 0,
    capturesRejected: 0,
    socketsCreated: 0,
    socketsClosed: 0,
    sentReconnect: 0,
    sentJoinRoom: 0,
    sentCreateSendTransport: 0,
    sentCreateRecvTransport: 0,
    sentProduce: 0,
    reconnectSuccess: 0,
    reconnectFailure: 0,
    openPeers: 0,
    liveLocalTracks: 0,
    pendingCaptures: 0,
    attachedMediaElements: 0,
    openSockets: 0,
  });
});

test('native peer construction, subclasses, static members and exceptions remain intact', () => {
  const f = fixture();
  const configuration = { iceServers: [{ urls: secret, credential: secret }] };
  const peer = f.createPeer(configuration, secret);
  assert.ok(peer instanceof f.NativePeer);
  assert.equal(Object.getPrototypeOf(peer), f.NativePeer.prototype);
  assert.equal(f.window.RTCPeerConnection.fixtureMarker, 'native-peer');
  assert.equal(f.constructions[0].args[0], configuration);
  assert.equal(f.constructions[0].args[1], secret);
  class Derived extends f.window.RTCPeerConnection {}
  const derived = new Derived();
  assert.equal(Object.getPrototypeOf(derived), Derived.prototype);
  assert.equal(f.constructions[1].newTarget, Derived);
  const error = new Error(secret);
  assert.throws(
    () => f.createPeer({ throw: error }),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().peersCreated, 2);
  assertPrivate(f.snapshot());
});

test('closed peers are promptly removed, listeners released, and terminal counts are idempotent', () => {
  const f = fixture();
  const peer = f.createPeer();
  assert.equal(peer.listenerCount(), 2);
  assert.equal(peer.close(secret), 17);
  assert.equal(peer.listenerCount(), 0);
  peer.emit('connectionstatechange');
  peer.close();
  assert.equal(f.snapshot().openPeers, 0);
  assert.equal(f.snapshot().peersClosed, 1);
  const external = f.createPeer();
  external.connectionState = 'closed';
  external.emit('connectionstatechange');
  assert.equal(external.listenerCount(), 0);
  assert.equal(f.snapshot().peersClosed, 2);
  for (let i = 0; i < 96; i++) f.createPeer().close();
  assert.equal(f.snapshot().peersCreated, 98);
  assert.equal(f.snapshot().openPeers, 0);
});

test('failed native close and illegal invocation are preserved without false release', () => {
  const f = fixture();
  const peer = f.createPeer();
  const error = new Error(secret);
  peer.closeError = error;
  assert.throws(
    () => peer.close(),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().openPeers, 1);
  assert.equal(f.snapshot().peersClosed, 0);
  assert.throws(() => f.NativePeer.prototype.close.call({}), /Illegal invocation/);
  assert.equal(f.snapshot().peersClosed, 0);
});

test('capture preserves promise identity, arguments, pending work and native rejection', async () => {
  const f = fixture();
  const pending = deferred();
  f.setCapture(() => pending.promise);
  const constraints = { audio: { deviceId: secret }, video: true };
  assert.equal(f.capture(constraints), pending.promise);
  assert.equal(f.calls[0].args[0], constraints);
  assert.equal(f.snapshot().pendingCaptures, 1);
  const track = new f.NativeTrack();
  pending.resolve(new f.NativeStream([track]));
  await pending.promise;
  assert.equal(f.snapshot().pendingCaptures, 0);
  assert.equal(f.snapshot().capturesResolved, 1);
  assert.equal(f.snapshot().liveLocalTracks, 1);
  const error = new Error(secret);
  const rejection = Promise.reject(error);
  f.setCapture(() => rejection);
  assert.equal(f.capture(), rejection);
  await assert.rejects(rejection, (caught) => caught === error);
  f.setCapture(() => {
    throw error;
  });
  assert.throws(
    () => f.capture(),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().capturesRejected, 2);
  assert.equal(f.snapshot().pendingCaptures, 0);
  assertPrivate(f.snapshot());
});

test('local stop, external end and local cloning release resources without counting remote tracks', async () => {
  const f = fixture();
  const audio = new f.NativeTrack('audio');
  const video = new f.NativeTrack('video');
  const stream = new f.NativeStream([audio, video]);
  f.setCapture(() => Promise.resolve(stream));
  await f.capture();
  const clonedTrack = audio.clone();
  const clonedStream = stream.clone();
  assert.equal(f.snapshot().liveLocalTracks, 5);
  const remote = new f.NativeTrack();
  remote.clone().stop();
  new f.NativeStream([remote]).clone();
  assert.equal(f.snapshot().tracksObserved, 5);
  assert.equal(audio.stop(secret), 23);
  assert.equal(audio.listenerCount(), 0);
  audio.stop();
  video.readyState = 'ended';
  video.emit('ended');
  clonedTrack.stop();
  for (const track of clonedStream.getTracks()) track.stop();
  assert.equal(f.snapshot().liveLocalTracks, 0);
  assert.equal(f.snapshot().tracksEnded, 5);
  assertPrivate(f.snapshot());
});

test('track stop and clone failures preserve native errors and live counts', async () => {
  const f = fixture();
  const track = new f.NativeTrack();
  f.setCapture(() => Promise.resolve(new f.NativeStream([track])));
  await f.capture();
  const error = new Error(secret);
  track.stopError = error;
  track.cloneError = error;
  assert.throws(
    () => track.stop(),
    (caught) => caught === error,
  );
  assert.throws(
    () => track.clone(),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().liveLocalTracks, 1);
  assert.equal(f.snapshot().tracksObserved, 1);
});

test('only native same-origin /ws sockets are observed and deliberate close is narrowly owned', () => {
  const f = fixture();
  const socket = f.createSocket();
  assert.ok(socket instanceof f.NativeSocket);
  assert.equal(f.window.WebSocket.OPEN, 1);
  for (const url of [
    `wss://${secret}.example/ws`,
    'ws://localhost:3119/ws',
    'wss://localhost:3119/ws/other',
    'wss://localhost:3119/other',
  ])
    f.createSocket(url);
  assert.equal(f.snapshot().openSockets, 1);
  assert.equal(f.observer.closeCurrentSocket(), 1);
  assert.equal(f.calls.at(-1).object, socket);
  assert.deepEqual(f.calls.at(-1).args, [4000, 'Synthetic reliability check']);
  assert.equal(f.snapshot().openSockets, 1, 'closing is not closed');
  socket.readyState = 3;
  socket.emit('close');
  assert.equal(socket.listenerCount(), 0);
  assert.equal(f.snapshot().openSockets, 0);
  assert.equal(f.snapshot().socketsClosed, 1);
  assert.throws(() => f.observer.closeCurrentSocket(), /exactly one/);
  f.createSocket();
  f.createSocket();
  assert.throws(() => f.observer.closeCurrentSocket(), /exactly one/);
  assertPrivate(f.snapshot());
});

test('socket construction and close exceptions are not replaced by observer errors', () => {
  const f = fixture();
  const error = new Error(secret);
  assert.throws(
    () => f.createSocket(error),
    (caught) => caught === error,
  );
  const socket = f.createSocket();
  socket.closeError = error;
  assert.throws(
    () => f.observer.closeCurrentSocket(),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().socketsCreated, 1);
  assert.equal(f.snapshot().socketsClosed, 0);
});

test('owned signaling counters retain only supported numeric kinds and preserve native send behavior', () => {
  const f = fixture();
  const socket = f.createSocket();
  const unrelated = f.createSocket(`wss://${secret}.example/ws`);
  for (const [type, counter] of [
    ['reconnect', 'sentReconnect'],
    ['joinRoom', 'sentJoinRoom'],
    ['createSendTransport', 'sentCreateSendTransport'],
    ['createRecvTransport', 'sentCreateRecvTransport'],
    ['produce', 'sentProduce'],
  ]) {
    const payload = JSON.stringify({ type, token: secret, sdp: secret, text: secret });
    assert.equal(socket.send(payload), 31);
    assert.equal(f.calls.at(-1).args[0], payload);
    unrelated.send(payload);
    assert.equal(f.snapshot()[counter], 1);
  }
  for (const data of [
    '{',
    'null',
    '[]',
    '1',
    '"reconnect"',
    new Uint8Array(3),
    '{}',
    '{"type":"unknown"}',
  ]) {
    socket.send(data);
    socket.emit('message', data);
  }
  socket.emit('message', JSON.stringify({ type: 'reconnectResult', success: true, token: secret }));
  socket.emit(
    'message',
    JSON.stringify({ type: 'reconnectResult', success: false, message: secret }),
  );
  socket.emit('message', JSON.stringify({ type: 'reconnectResult', success: 'false' }));
  unrelated.emit('message', JSON.stringify({ type: 'reconnectResult', success: false }));
  assert.equal(f.snapshot().reconnectSuccess, 1);
  assert.equal(f.snapshot().reconnectFailure, 1);
  const error = new Error(secret);
  socket.sendError = error;
  assert.throws(
    () => socket.send('{"type":"produce"}'),
    (caught) => caught === error,
  );
  assert.equal(f.snapshot().sentProduce, 1);
  assertPrivate(f.snapshot());
});

test('signaling message and counter bounds fail closed and replaced per-socket send is detected', () => {
  const messages = fixture();
  const socket = messages.createSocket();
  socket.send(' '.repeat(65537));
  assert.throws(messages.snapshot, /signaling message limit exceeded/);
  const counters = fixture();
  const counted = counters.createSocket();
  for (let i = 0; i < 10001; i++)
    counted.emit('message', '{"type":"reconnectResult","success":false}');
  assert.throws(counters.snapshot, /counter limit exceeded/);
  const replaced = fixture();
  replaced.createSocket().send = () => {};
  assert.throws(replaced.snapshot, /native resource method was replaced/);
});

test('snapshots count currently attached document media without retaining elements or streams', () => {
  const f = fixture();
  f.elements.push({ srcObject: null }, { srcObject: { content: secret } });
  assert.equal(f.snapshot().attachedMediaElements, 1);
  f.elements[1].srcObject = null;
  assert.equal(f.snapshot().attachedMediaElements, 0);
  assertPrivate(f.snapshot());
});

test('media sampling preserves numeric progress using opaque per-peer and current-stream ordinals', async () => {
  const f = fixture();
  const peer = f.createPeer();
  const video = {
    type: 'inbound-rtp',
    id: `${secret}-video`,
    kind: 'video',
    framesDecoded: 2,
    bytesReceived: 64,
    packetsLost: -1,
    totalDecodeTime: 0.125,
    trackIdentifier: secret,
    codecId: secret,
  };
  const audio = {
    type: 'inbound-rtp',
    id: `${secret}-audio`,
    kind: 'audio',
    totalSamplesReceived: 480,
    totalSamplesDuration: 0.01,
    totalAudioEnergy: 0.025,
    jitterBufferDelay: 0.002,
  };
  peer.stats = new Map([
    [0, video],
    [1, audio],
    [2, { type: 'candidate-pair', id: secret, address: secret }],
    [3, { type: 'outbound-rtp', id: secret, kind: 'video', framesEncoded: 3 }],
  ]);
  const first = await f.mediaSample();
  assert.equal(first.peers[0].peer, 1);
  assert.equal(first.peers[0].inbound.length, 2);
  assert.equal(first.peers[0].inbound[0].framesDecoded, 2);
  assert.equal(first.peers[0].inbound[0].packetsLost, -1);
  assert.equal(first.peers[0].inbound[0].totalSamplesReceived, null);
  assert.equal(first.peers[0].inbound[1].totalSamplesReceived, 480);
  video.framesDecoded += 10;
  audio.totalSamplesReceived += 480;
  const next = await f.mediaSample();
  assert.equal(next.peers[0].inbound[0].stream, first.peers[0].inbound[0].stream);
  assert.equal(next.peers[0].inbound[0].framesDecoded, 12);
  assert.equal(next.peers[0].inbound[1].totalSamplesReceived, 960);
  assertPrivate(first);
  assertPrivate(next);
  peer.stats.clear();
  assert.deepEqual((await f.mediaSample()).peers[0].inbound, []);
  peer.stats.set(0, video);
  assert.notEqual(
    (await f.mediaSample()).peers[0].inbound[0].stream,
    first.peers[0].inbound[0].stream,
  );
  peer.close();
  assert.deepEqual(await f.mediaSample(), { peers: [] });
});

test('sampling does not retain historical stream identities across a long-lived peer', async () => {
  const f = fixture();
  const peer = f.createPeer();
  let previous = 0;
  for (let i = 0; i < 100; i++) {
    peer.stats = new Map([
      [0, { type: 'inbound-rtp', id: `${secret}-${i}`, kind: 'video', framesDecoded: i }],
    ]);
    const sample = await f.mediaSample();
    const stream = sample.peers[0].inbound[0].stream;
    assert.ok(stream > previous);
    previous = stream;
  }
  assert.equal(f.snapshot().openPeers, 1);
});

test('pending stats reject concurrent reads and closing during a read does not resurrect a peer', async () => {
  const f = fixture();
  const peer = f.createPeer();
  const pending = deferred();
  peer.stats = pending.promise;
  const first = f.mediaSample();
  await assert.rejects(f.mediaSample(), /already pending/);
  peer.close();
  assert.equal(f.snapshot().openPeers, 0);
  pending.resolve(
    new Map([[0, { type: 'inbound-rtp', id: secret, kind: 'video', framesDecoded: 1 }]]),
  );
  assert.deepEqual(await first, { peers: [] });
  assert.deepEqual(await f.mediaSample(), { peers: [] });
});

test('native stats failures and malformed values fail closed without native error content', async () => {
  const failing = fixture();
  failing.createPeer().statsError = new Error(secret);
  await assert.rejects(failing.mediaSample(), (error) => {
    assert.match(error.message, /Lifecycle observation unavailable/);
    assertPrivate(error.message);
    return true;
  });
  assert.throws(failing.snapshot, /Lifecycle observation unavailable/);
  for (const framesDecoded of [-1, 0.1, '1', null, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
    const f = fixture();
    f.createPeer().stats = new Map([
      [0, { type: 'inbound-rtp', id: secret, kind: 'video', framesDecoded }],
    ]);
    await assert.rejects(f.mediaSample(), /bounded media sample unavailable/);
    assert.throws(f.snapshot, /bounded media sample unavailable/);
  }
});

test('active resource and pending-capture caps fail closed rather than silently truncating', async () => {
  const peers = fixture();
  for (let i = 0; i < 33; i++) peers.createPeer();
  assert.throws(peers.snapshot, /active resource limit exceeded/);
  const sockets = fixture();
  for (let i = 0; i < 9; i++) sockets.createSocket();
  assert.throws(sockets.snapshot, /active resource limit exceeded/);
  const tracks = fixture();
  tracks.setCapture(() =>
    Promise.resolve(
      new tracks.NativeStream(Array.from({ length: 65 }, () => new tracks.NativeTrack())),
    ),
  );
  await tracks.capture();
  assert.throws(tracks.snapshot, /capture track limit exceeded/);
  const captures = fixture();
  captures.setCapture(() => new Promise(() => {}));
  for (let i = 0; i < 17; i++) captures.capture();
  assert.throws(captures.snapshot, /pending capture limit exceeded/);
  const elements = fixture();
  elements.elements.push(...Array.from({ length: 257 }, () => ({ srcObject: null })));
  assert.throws(elements.snapshot, /media element limit exceeded/);
});

test('stats caps reject too many rows, inbound streams, long identifiers and duplicate identifiers', async () => {
  for (const rows of [
    Array.from({ length: 513 }, (_, i) => ({ type: 'candidate-pair', id: String(i) })),
    Array.from({ length: 65 }, (_, i) => ({ type: 'inbound-rtp', id: String(i), kind: 'audio' })),
    [{ type: 'inbound-rtp', id: 'a'.repeat(257), kind: 'audio' }],
    [
      { type: 'inbound-rtp', id: secret, kind: 'audio' },
      { type: 'inbound-rtp', id: secret, kind: 'video' },
    ],
  ]) {
    const f = fixture();
    f.createPeer().stats = new Map(rows.map((row, i) => [i, row]));
    await assert.rejects(f.mediaSample(), /bounded media sample unavailable/);
  }
});

test('unsupported, replaced and silently collected observers cannot claim clean shutdown', () => {
  const unsupported = fixture({ unsupported: true });
  assert.throws(unsupported.snapshot, /unsupported browser APIs/);
  const replaced = fixture();
  assert.throws(() => replaced.install(), /already installed/);
  assert.throws(() => {
    replaced.window.__lifecycle = { snapshot: () => ({ openPeers: 0 }) };
  }, TypeError);
  assert.throws(() => {
    replaced.observer.snapshot = () => ({});
  }, TypeError);
  replaced.window.RTCPeerConnection = class {};
  assert.throws(() => replaced.observer.snapshot(), /observer was replaced/);
  const collected = fixture();
  const peer = collected.createPeer();
  collected.references.find((reference) => reference.object === peer).object = undefined;
  assert.throws(collected.snapshot, /resource disappeared before terminal observation/);
});

test('capture, stats, media queries and track prototypes are integrity checked', () => {
  for (const tamper of [
    (f) => {
      f.navigator.mediaDevices.getUserMedia = () => Promise.resolve();
    },
    (f) => {
      f.NativePeer.prototype.getStats = () => Promise.resolve(new Map());
    },
    (f) => {
      f.NativeTrack.prototype.stop = () => {};
    },
    (f) => {
      f.document.querySelectorAll = () => [];
    },
    (f) => {
      f.window.MediaStreamTrack = class {};
    },
  ]) {
    const f = fixture();
    tamper(f);
    assert.throws(f.snapshot, /observer was replaced/);
  }
});

test('mixed local and remote stream cloning remains unsupported rather than miscounting ownership', async () => {
  const f = fixture();
  const local = new f.NativeTrack();
  f.setCapture(() => Promise.resolve(new f.NativeStream([local])));
  await f.capture();
  new f.NativeStream([local, new f.NativeTrack()]).clone();
  assert.throws(f.snapshot, /mixed local and remote stream clone unavailable/);
});
