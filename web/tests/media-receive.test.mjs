import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';
import { deferred, flush } from './ui-fixture.mjs';

// Shipped resource objects exercise real close/pause/track ownership; every
// transport, device and native promise below is a source-level fake.
const { Producer } = await import(
  new URL('./Producer.js', import.meta.resolve('mediasoup-client'))
);
const { Consumer } = await import(
  new URL('./Consumer.js', import.meta.resolve('mediasoup-client'))
);
const signalingModule = await loadTypeScript('src/signaling.ts', {
  modules: await loadContractModules(),
});

class Track extends EventTarget {
  enabled = true;
  muted = false;
  readyState = 'live';
  stops = 0;

  constructor(kind) {
    super();
    this.kind = kind;
  }

  stop() {
    this.stops++;
    this.readyState = 'ended';
  }
}

class Stream {
  constructor(tracks = []) {
    this.tracks = [...tracks];
  }
  getTracks() {
    return [...this.tracks];
  }
  getAudioTracks() {
    return this.tracks.filter((track) => track.kind === 'audio');
  }
  getVideoTracks() {
    return this.tracks.filter((track) => track.kind === 'video');
  }
  addTrack(track) {
    this.tracks.push(track);
  }
  removeTrack(track) {
    this.tracks = this.tracks.filter((current) => current !== track);
  }
}

function transportReply(id) {
  return {
    type: 'transportCreated',
    transportId: id,
    iceParameters: { usernameFragment: 'owned', password: 'owned-test', iceLite: true },
    iceCandidates: [],
    dtlsParameters: { role: 'auto', fingerprints: [] },
  };
}

async function fixture(t, { published = true } = {}) {
  const state = {
    requests: [],
    receivers: [],
    consumers: [],
    captures: 0,
    consumerSequence: 0,
    replacementSequence: 0,
    request: null,
    consume: null,
    adoptFailure: null,
  };
  const makeConsumer = (options, track = new Track(options.kind)) => {
    const consumer = new Consumer({
      ...options,
      localId: options.id,
      rtpReceiver: {},
      track,
    });
    t.mock.method(consumer, 'close');
    state.consumers.push(consumer);
    return consumer;
  };
  class Transport {
    closed = false;
    connectionState = 'connected';
    listeners = new Map();
    closes = 0;
    restart = null;
    update = null;

    constructor(id) {
      this.id = id;
    }
    on(name, listener) {
      this.listeners.set(name, listener);
    }
    emit(name, ...args) {
      this.listeners.get(name)?.(...args);
    }
    consume(options) {
      return state.consume ? state.consume(options, this) : Promise.resolve(makeConsumer(options));
    }
    restartIce(options) {
      return this.restart?.(options) ?? Promise.resolve();
    }
    updateIceServers(options) {
      return this.update?.(options) ?? Promise.resolve();
    }
    close() {
      this.closed = true;
      this.closes++;
    }
  }
  const sender = new Transport('sender');
  class Device {
    recvRtpCapabilities = { codecs: [], headerExtensions: [] };
    static factory() {
      return Promise.resolve(new Device());
    }
    load() {
      return Promise.resolve();
    }
    createSendTransport() {
      return sender;
    }
    createRecvTransport(options) {
      if (state.adoptFailure) throw state.adoptFailure;
      const transport = new Transport(options.id);
      state.receivers.push(transport);
      return transport;
    }
  }
  const globals = {
    MediaStream: Stream,
    localStorage: { getItem: () => null, setItem() {} },
    document: { visibilityState: 'visible' },
    navigator: {
      mediaDevices: {
        getUserMedia() {
          state.captures++;
          throw new Error('Unexpected capture in receive-only fixture');
        },
        getDisplayMedia() {
          state.captures++;
          throw new Error('Unexpected display capture in receive-only fixture');
        },
      },
    },
    console: { log() {}, warn() {} },
  };
  const { MediaManager } = await loadTypeScript('src/media.ts', {
    globals,
    modules: {
      '../node_modules/mediasoup-client/lib/Device.js': { Device },
      './signaling': signalingModule,
      './capture-preferences': await loadTypeScript('src/capture-preferences.ts', { globals }),
      './screen-presentation': await loadTypeScript('src/screen-presentation.ts', { globals }),
    },
  });
  const signaling = {
    connected: true,
    send(message) {
      state.requests.push(message);
    },
    async request(message, type) {
      state.requests.push(message);
      if (state.request) {
        const response = await state.request(message, type);
        if (response !== undefined) return response;
      }
      if (message.type === 'getRouterRtpCapabilities')
        return { type, rtpCapabilities: { codecs: [], headerExtensions: [] } };
      if (message.type === 'createSendTransport') return transportReply('sender');
      if (message.type === 'createRecvTransport') return transportReply('receiver');
      if (message.type === 'replaceRecvTransport')
        return transportReply(`replacement-${++state.replacementSequence}`);
      if (message.type === 'consume')
        return {
          type,
          consumerId: `consumer-${++state.consumerSequence}`,
          producerId: message.producerId,
          kind: message.producerId === 'voice' ? 'audio' : 'video',
          rtpParameters: { encodings: [{ scalabilityMode: 'L3T3' }] },
        };
      return { ...message, type, transportId: message.transportId, iceParameters: {} };
    },
  };
  const media = new MediaManager(signaling);
  t.after(() => media.close());
  await media.setup();
  const producers = [];
  if (published) {
    for (const [field, kind] of [
      ['audioProducer', 'audio'],
      ['videoProducer', 'video'],
      ['screenProducer', 'video'],
      ['screenAudioProducer', 'audio'],
    ]) {
      const producer = new Producer({
        id: field,
        localId: field,
        track: new Track(kind),
        rtpParameters: { encodings: [{ scalabilityMode: 'L1T3' }] },
        stopTracks: true,
        disableTrackOnPause: true,
        zeroRtpOnPause: false,
      });
      media[field] = producer;
      producers.push(producer);
    }
    media.localStream = new Stream(producers.slice(0, 2).map((producer) => producer.track));
  }
  return { media, state, signaling, sender, producers, makeConsumer };
}

function assertPublishingPreserved(f) {
  assert.equal(f.media.sendTransport, f.sender);
  assert.equal(f.sender.closes, 0);
  assert.equal(f.state.captures, 0);
  for (const producer of f.producers) {
    assert.equal(producer.closed, false);
    assert.equal(producer.track.readyState, 'live');
    assert.equal(producer.track.stops, 0);
  }
  assert.equal(f.media.audioEnabled, true);
  assert.equal(f.media.videoEnabled, true);
  assert.equal(f.media.isScreenSharing, true);
}

test('replacement closes only incoming resources and retires their controls and ICE timer', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = await fixture(t);
  const old = f.media.recvTransport;
  await f.media.consume('camera');
  const consumer = f.state.consumers[0];
  f.media.setConsumerQualityByProducer('camera', 'low');
  f.media.setConsumerSizeCapByProducer('camera', 1);
  await flush();
  old.emit('connectionstatechange', 'disconnected');
  f.sender.emit('connectionstatechange', 'disconnected');
  await f.media.replaceReceiveTransport();
  assert.equal(old.closes, 1);
  assert.equal(consumer.closed, true);
  assert.equal(f.media.consumers.size, 0);
  assert.equal(f.media.producerToConsumer.size, 0);
  assert.equal(f.media.consumerQualities.size, 0);
  assert.equal(f.media.consumerSizeCaps.size, 0);
  assert.equal(f.media.consumerRetirements.size, 0);
  assert.equal(f.media.iceRestartTimers.has(old.id), false);
  assert.equal(f.media.pendingControls.has(`ice:${old.id}`), false);
  assert.equal(f.media.inFlightControls.has(`ice:${old.id}`), false);
  t.mock.timers.tick(3_000);
  await flush();
  assert.deepEqual(
    f.state.requests
      .filter((message) => message.type === 'restartIce')
      .map((message) => message.transportId),
    ['sender'],
    'the sender timer survives while the retired receiver cannot restart',
  );
  assertPublishingPreserved(f);
});

test('response loss retains the expected old ID, deduplicates callers, and retries the same server replacement', async (t) => {
  const f = await fixture(t);
  const old = f.media.recvTransport;
  const gate = deferred();
  let attempts = 0;
  f.state.request = (message) => {
    if (message.type !== 'replaceRecvTransport') return;
    attempts++;
    return attempts === 1 ? gate.promise : transportReply('same-server-replacement');
  };
  const first = f.media.replaceReceiveTransport();
  const rejected = assert.rejects(first, /lost reply/);
  assert.equal(f.media.replaceReceiveTransport(), first);
  assert.equal(attempts, 1);
  gate.reject(new Error('lost reply'));
  await rejected;
  assert.equal(f.media.recvTransport, null);
  assert.equal(old.closes, 1);
  await f.media.replaceReceiveTransport();
  assert.equal(f.media.recvTransport.id, 'same-server-replacement');
  assert.deepEqual(
    f.state.requests
      .filter((message) => message.type === 'replaceRecvTransport')
      .map((message) => message.transportId),
    [old.id, old.id],
  );
  assertPublishingPreserved(f);
});

test('replacement fences an in-flight receiver ICE reply and preserves the sender control', async (t) => {
  const f = await fixture(t);
  const old = f.media.recvTransport;
  const receiver = deferred(),
    sender = deferred();
  f.state.request = (message) => {
    if (message.type === 'restartIce')
      return message.transportId === old.id ? receiver.promise : sender.promise;
  };
  old.emit('connectionstatechange', 'failed');
  f.sender.emit('connectionstatechange', 'failed');
  assert.equal(f.media.inFlightControls.has(`ice:${old.id}`), true);
  assert.equal(f.media.inFlightControls.has(`ice:${f.sender.id}`), true);
  await f.media.replaceReceiveTransport();
  const nativeReceiver = t.mock.method(f.media.recvTransport, 'restartIce');
  const nativeSender = t.mock.method(f.sender, 'restartIce');
  assert.equal(f.media.pendingControls.has(`ice:${old.id}`), false);
  assert.equal(f.media.inFlightControls.has(`ice:${old.id}`), false);
  assert.equal(f.media.pendingControls.has(`ice:${f.sender.id}`), true);
  assert.equal(f.media.inFlightControls.has(`ice:${f.sender.id}`), true);
  receiver.resolve({ type: 'iceRestarted', transportId: old.id, iceParameters: {} });
  sender.resolve({ type: 'iceRestarted', transportId: f.sender.id, iceParameters: {} });
  await flush();
  assert.equal(
    nativeReceiver.mock.callCount(),
    0,
    'retired ICE work cannot target the new receiver',
  );
  assert.equal(nativeSender.mock.callCount(), 1, 'outgoing recovery remains owned by the sender');
  assertPublishingPreserved(f);
});

test('failed native adoption keeps the same expected old ID for a later explicit retry', async (t) => {
  const f = await fixture(t);
  f.state.request = (message) =>
    message.type === 'replaceRecvTransport' ? transportReply('allocated-on-server') : undefined;
  f.state.adoptFailure = new Error('handler adoption failed');
  await assert.rejects(f.media.replaceReceiveTransport(), /handler adoption failed/);
  f.state.adoptFailure = null;
  await f.media.replaceReceiveTransport();
  assert.equal(f.media.recvTransport.id, 'allocated-on-server');
  assert.deepEqual(
    f.state.requests
      .filter((message) => message.type === 'replaceRecvTransport')
      .map((message) => message.transportId),
    ['receiver', 'receiver'],
  );
  assertPublishingPreserved(f);
});

test('a hung native consume times out at ten seconds and poisons only the receive transport', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = await fixture(t);
  const old = f.media.recvTransport;
  const gate = deferred();
  let options;
  f.state.consume = (value) => {
    options = value;
    return gate.promise;
  };
  const directions = [];
  f.media.onTransportRebuildRequired = (direction) => directions.push(direction);
  const pending = f.media.consume('camera');
  const timedOut = assert.rejects(pending, /Incoming media setup timed out/);
  await flush();
  t.mock.timers.tick(9_999);
  await flush();
  assert.equal(old.closed, false);
  assert.deepEqual(directions, []);
  t.mock.timers.tick(1);
  await timedOut;
  assert.equal(old.closed, true);
  assert.equal(f.media.recvTransport, null);
  assert.deepEqual(directions, ['recv']);
  const late = f.makeConsumer(options);
  gate.resolve(late);
  await flush();
  assert.equal(late.closed, true);
  assert.equal(f.media.consumers.size, 0);
  assertPublishingPreserved(f);
});

test('explicit receiver replacement unblocks hung negotiation and a late consumer cannot erase the new mapping', async (t) => {
  const f = await fixture(t);
  const gate = deferred();
  let oldOptions;
  f.state.consume = (options) => {
    oldOptions = options;
    return gate.promise;
  };
  const pending = f.media.consume('camera');
  const retired = assert.rejects(pending, /Incoming media changed/);
  await flush();
  await f.media.replaceReceiveTransport();
  await retired;
  f.state.consume = null;
  const fresh = await f.media.consume('camera');
  const current = f.media.recvTransport;
  const late = f.makeConsumer(oldOptions);
  gate.resolve(late);
  await flush();
  assert.equal(late.closed, true);
  assert.equal(fresh.readyState, 'live');
  assert.equal(f.media.getConsumerTrackByProducer('camera'), fresh);
  assert.equal(f.media.recvTransport, current);
  assert.equal(current.closed, false);
  assertPublishingPreserved(f);
});

test('a native consumer settling at the timeout boundary is still closed', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const f = await fixture(t);
  const gate = deferred();
  let options;
  f.state.consume = (value) => {
    options = value;
    return gate.promise;
  };
  const pending = assert.rejects(f.media.consume('camera'), /Incoming media setup timed out/);
  await flush();
  const late = f.makeConsumer(options);
  t.mock.timers.tick(10_000);
  // Timeout and native settlement can queue reactions before the awaiting
  // continuation has marked its result obsolete. The resource still needs an owner.
  gate.resolve(late);
  await pending;
  await flush();
  assert.equal(late.closed, true, 'a timeout winner must retire the native losing result');
  assertPublishingPreserved(f);
});

test('replacement retires an unacknowledged consumer close without blocking fresh subscriptions', async (t) => {
  const f = await fixture(t);
  await f.media.consume('camera');
  const gate = deferred();
  f.state.request = (message) => (message.type === 'closeConsumer' ? gate.promise : undefined);
  const retirement = assert.rejects(
    f.media.retireConsumerByProducer('camera'),
    /Media session changed/,
  );
  await flush();
  await f.media.replaceReceiveTransport();
  assert.equal(f.media.consumerRetirements.size, 0);
  const current = await f.media.consume('camera');
  gate.resolve({ type: 'mediaControlApplied' });
  await retirement;
  assert.equal(f.media.getConsumerTrackByProducer('camera'), current);
  assert.equal(current.readyState, 'live');
  assertPublishingPreserved(f);
});

for (const change of ['replace', 'leave']) {
  test(`a pending old receive connect cannot acknowledge after ${change}`, async (t) => {
    const f = await fixture(t);
    const old = f.media.recvTransport;
    const gate = deferred();
    f.state.request = (message) => (message.type === 'connectTransport' ? gate.promise : undefined);
    const accepted = [],
      rejected = [];
    old.emit(
      'connect',
      { dtlsParameters: {} },
      () => accepted.push(true),
      (error) => rejected.push(error),
    );
    const requests = f.state.requests.length;
    if (change === 'replace') await f.media.replaceReceiveTransport();
    else f.media.close();
    gate.resolve({ type: 'transportConnected', transportId: old.id });
    await flush();
    assert.deepEqual(accepted, []);
    assert.equal(rejected.length, 1);
    assert.match(rejected[0].message, /Incoming media changed/);
    old.emit(
      'connect',
      { dtlsParameters: {} },
      () => accepted.push(true),
      (error) => rejected.push(error),
    );
    await flush();
    assert.equal(rejected.length, 2);
    assert.equal(f.state.requests.length, requests + (change === 'replace' ? 1 : 0));
    if (change === 'replace') assertPublishingPreserved(f);
  });
}

test('health and optional telemetry share one pending native stats call, including a hung call', async (t) => {
  const f = await fixture(t);
  await f.media.consume('voice');
  const consumer = f.state.consumers[0];
  const gate = deferred();
  const stats = t.mock.method(consumer, 'getStats', () => gate.promise);
  const health = f.media.callHealthSources(new Set()).find((source) => source.key === consumer);
  const telemetry = f.media.telemetrySources(new Set())[0];
  const first = health.getStats();
  assert.equal(telemetry.getStats(), first);
  await flush();
  for (let index = 0; index < 20; index++) assert.equal(health.getStats(), first);
  assert.equal(stats.mock.callCount(), 1);
  const report = new Map();
  gate.resolve(report);
  assert.equal(await first, report);
  await flush();
  await telemetry.getStats();
  assert.equal(stats.mock.callCount(), 2, 'settlement permits the next native sample');
  const outbound = f.producers[0];
  const outboundGate = deferred();
  const outgoingStats = t.mock.method(outbound, 'getStats', () => outboundGate.promise);
  const source = f.media.callHealthSources(new Set()).find((row) => row.key === outbound);
  const native = source.getStats();
  for (let index = 0; index < 20; index++) assert.equal(source.getStats(), native);
  await flush();
  assert.equal(
    outgoingStats.mock.callCount(),
    1,
    'an unresolved sender sample remains single-flight',
  );
});

test('microphone OS interruption remains distinct from user pause and recovery never recaptures', async (t) => {
  const f = await fixture(t);
  const microphone = f.producers[0];
  const source = () => f.media.callHealthSources(new Set()).find((row) => row.key === microphone);
  assert.equal(f.media.callHealthTransportState().microphone, 'live');
  assert.equal(source().active(), true);
  microphone.track.muted = true;
  microphone.track.dispatchEvent(new Event('mute'));
  assert.equal(f.media.callHealthTransportState().microphone, 'interrupted');
  assert.equal(source().active(), false);
  microphone.track.muted = false;
  microphone.track.dispatchEvent(new Event('unmute'));
  assert.equal(f.media.callHealthTransportState().microphone, 'live');
  microphone.pause();
  assert.equal(f.media.callHealthTransportState().microphone, 'off');
  assert.equal(source().active(), false);
  assert.equal(f.state.captures, 0);
  assert.equal(microphone.closed, false);
});

test('a rejected health stats call is shared and then released for a fresh sample', async (t) => {
  const f = await fixture(t);
  const producer = f.producers[0];
  const gate = deferred();
  const stats = t.mock.method(producer, 'getStats', () => gate.promise);
  const source = f.media.callHealthSources(new Set()).find((row) => row.key === producer);
  const first = source.getStats();
  const rejected = assert.rejects(first, /stats unavailable/);
  assert.equal(source.getStats(), first);
  gate.reject(new Error('stats unavailable'));
  await rejected;
  const report = new Map();
  stats.mock.mockImplementation(() => Promise.resolve(report));
  assert.equal(await source.getStats(), report);
  assert.equal(stats.mock.callCount(), 2);
});

for (const direction of ['send', 'recv']) {
  for (const failure of ['native', 'server']) {
    test(`${direction} ICE ${failure} failure reports the correct recovery direction`, async (t) => {
      const f = await fixture(t);
      const transport = direction === 'send' ? f.sender : f.media.recvTransport;
      const directions = [];
      f.media.onTransportRebuildRequired = (value) => directions.push(value);
      if (failure === 'native') {
        transport.restart = () => Promise.reject(new Error('native restart rejected'));
        await f.media.handleIceRestarted(transport.id, {});
      } else {
        f.state.request = (message) => {
          if (message.type === 'restartIce') throw new Error('server retired transport');
        };
        transport.emit('connectionstatechange', 'failed');
        await flush();
      }
      assert.deepEqual(directions, [direction]);
      assertPublishingPreserved(f);
    });
  }
}
