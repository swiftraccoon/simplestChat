import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadContractModules, loadTypeScript } from './source-loader.mjs';

const signalingModule = await loadTypeScript('src/signaling.ts', {
  modules: await loadContractModules(),
});

const { Producer } = await import(
  new URL('./Producer.js', import.meta.resolve('mediasoup-client'))
);
const { Consumer } = await import(
  new URL('./Consumer.js', import.meta.resolve('mediasoup-client'))
);
const { Firefox120 } = await import(
  new URL('./handlers/Firefox120.js', import.meta.resolve('mediasoup-client'))
);

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

class Track extends EventTarget {
  enabled = true;
  readyState = 'live';
  constructor(kind) {
    super();
    this.kind = kind;
  }
  stop() {
    this.readyState = 'ended';
  }
  endExternally() {
    this.readyState = 'ended';
    this.dispatchEvent(new Event('ended'));
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
    this.tracks = this.tracks.filter((item) => item !== track);
  }
}

async function fixture(t) {
  const state = {
    tracks: [],
    producers: [],
    sent: [],
    captureCalls: [],
    logs: [],
    warnings: [],
    capture: null,
    displayCapture: null,
    beforeProduce: null,
    replace: null,
  };
  const newTrack = (kind) => {
    const track = new Track(kind);
    state.tracks.push(track);
    return track;
  };
  const { MediaManager } = await loadTypeScript('src/media.ts', {
    modules: { 'mediasoup-client': {}, './signaling': signalingModule },
    globals: {
      localStorage: {
        getItem() {
          return null;
        },
        setItem() {},
      },
      MediaStream: Stream,
      navigator: {
        mediaDevices: {
          getUserMedia: (constraints) => {
            state.captureCalls.push(constraints);
            return state.capture
              ? state.capture(constraints)
              : Promise.resolve(new Stream([newTrack(constraints.audio ? 'audio' : 'video')]));
          },
          getDisplayMedia: async () =>
            state.displayCapture
              ? state.displayCapture()
              : new Stream([newTrack('video'), newTrack('audio')]),
        },
      },
      console: {
        log(...values) {
          state.logs.push(values);
        },
        warn(...values) {
          state.warnings.push(values);
        },
      },
    },
  });
  const signaling = {
    connected: true,
    send(message) {
      if (this.connected) state.sent.push(message);
    },
    async request(message, responseType) {
      if (!this.connected) throw new Error('WebSocket closed');
      state.sent.push(message);
      return (await state.controlReply?.(message)) ?? { type: responseType };
    },
  };
  const media = new MediaManager(signaling);
  media.sendTransport = {
    async produce(options) {
      await state.beforeProduce?.(options);
      // Use the installed library's real pause/track semantics, especially
      // construction with a disabled track and asynchronous replaceTrack.
      const producer = new Producer({
        id: `producer-${state.producers.length}`,
        localId: 'local',
        track: options.track,
        rtpParameters: {},
        stopTracks: true,
        disableTrackOnPause: true,
        zeroRtpOnPause: false,
        appData: options.appData,
      });
      t.mock.method(producer, 'close');
      producer.on('@replacetrack', (track, resolve, reject) => {
        Promise.resolve()
          .then(() => state.replace?.(track))
          .then(resolve, reject);
      });
      state.producers.push(producer);
      return producer;
    },
    close() {},
  };
  t.after(() => media.close());
  return { ...state, state, media, signaling, newTrack };
}

const startLocal = (media, kind) => (kind === 'audio' ? media.unmuteAudio() : media.unmuteVideo());
const localEnabled = (media, kind) => (kind === 'audio' ? media.audioEnabled : media.videoEnabled);
const switchLocal = (media, kind) =>
  kind === 'audio' ? media.switchMic('next-mic') : media.switchCamera('next-camera');
const localTracks = (media, kind) =>
  media
    .getLocalStream()
    ?.getTracks()
    .filter((track) => track.kind === kind) ?? [];
const closeMessages = (state, producer) =>
  state.sent.filter(
    (message) => message.type === 'closeProducer' && message.producerId === producer.id,
  );

async function settleControls() {
  for (let index = 0; index < 12; index++) await Promise.resolve();
}

for (const outcome of ['resolve', 'reject']) {
  test(`an old control ${outcome} cannot retire a replacement request after socket loss`, async (t) => {
    const { media, signaling, state } = await fixture(t);
    await media.unmuteAudio();
    const oldReply = deferred();
    const currentReply = deferred();
    const replies = [oldReply, currentReply];
    state.controlReply = () => replies.shift().promise;
    media.muteAudio();
    assert.equal(state.sent.length, 1);
    assert.equal(media.pendingControls.size, 1, 'a native send is not acknowledgement');
    signaling.connected = false;
    media.suspendSignaling();
    signaling.connected = true;
    media.resumeSignaling();
    assert.equal(state.sent.length, 2);
    oldReply[outcome](new Error('Old socket closed'));
    await settleControls();
    assert.equal(media.pendingControls.size, 1, 'an old completion cannot erase the replay');
    currentReply.resolve();
    await settleControls();
    assert.equal(media.pendingControls.size, 0);
    media.resumeSignaling();
    assert.equal(state.sent.length, 2, 'acknowledged controls must not replay again');
  });
}

test('rapid controls coalesce while acknowledgement is pending and eventually apply the last state', async (t) => {
  const { media, state } = await fixture(t);
  await media.unmuteAudio();
  const reply = deferred();
  state.controlReply = () => reply.promise;
  media.muteAudio();
  await media.unmuteAudio();
  media.muteAudio();
  await media.unmuteAudio();
  assert.equal(state.sent.length, 1, 'only one producer control may be in flight');
  reply.resolve();
  await settleControls();
  assert.deepEqual(
    state.sent.map(({ type }) => type),
    ['pauseProducer', 'resumeProducer'],
  );
  assert.equal(media.pendingControls.size, 0);
});

test('a live control rejection reports failure once and permits the next user action', async (t) => {
  const { media, state } = await fixture(t);
  await media.unmuteAudio();
  let errors = 0;
  media.onControlError = () => {
    errors++;
  };
  state.controlReply = () => Promise.reject(new Error('Private native error details'));
  media.muteAudio();
  await settleControls();
  assert.equal(errors, 1);
  assert.equal(media.pendingControls.size, 0);
  assert.equal(state.sent.length, 1, 'a failed control must not spin on retries');
  assert.deepEqual(state.warnings, [['[media] pauseProducer could not be confirmed']]);
  state.controlReply = undefined;
  await media.unmuteAudio();
  await settleControls();
  assert.equal(state.sent.at(-1).type, 'resumeProducer');
  assert.equal(errors, 1);
});

test('a failed superseded control still delivers the newest desired state', async (t) => {
  const { media, state } = await fixture(t);
  await media.unmuteAudio();
  const reply = deferred();
  state.controlReply = () => reply.promise;
  media.muteAudio();
  await media.unmuteAudio();
  state.controlReply = undefined;
  reply.reject(new Error('Old pause rejected'));
  await settleControls();
  assert.deepEqual(
    state.sent.map(({ type }) => type),
    ['pauseProducer', 'resumeProducer'],
  );
  assert.equal(media.pendingControls.size, 0);
  assert.deepEqual(state.warnings, []);
});

test('a timeout before socket closure retains the desired control without retrying on unrelated acknowledgements', async (t) => {
  const { media, state, signaling } = await fixture(t);
  await media.unmuteAudio();
  await media.unmuteVideo();
  const audioId = media.audioProducer.id;
  state.controlReply = (message) => {
    if (message.producerId === audioId)
      return Promise.reject(new signalingModule.SignalingRequestTimeoutError('producerPaused'));
  };
  media.muteAudio();
  await settleControls();
  assert.equal(media.pendingControls.size, 1);
  media.pauseVideo();
  await settleControls();
  assert.equal(state.sent.filter((message) => message.producerId === audioId).length, 1);
  assert.equal(state.warnings.length, 1);
  signaling.connected = false;
  media.suspendSignaling();
  state.controlReply = undefined;
  signaling.connected = true;
  media.resumeSignaling();
  await settleControls();
  assert.deepEqual(state.sent.at(-1), { type: 'pauseProducer', producerId: audioId });
  assert.equal(media.pendingControls.size, 0);
  assert.equal(media.inFlightControls.size, 0);
});

test('a new user choice proceeds after a timeout without waiting for socket recovery', async (t) => {
  const { media, state } = await fixture(t);
  await media.unmuteAudio();
  state.controlReply = () =>
    Promise.reject(new signalingModule.SignalingRequestTimeoutError('producerPaused'));
  media.muteAudio();
  await settleControls();
  state.controlReply = undefined;
  await media.unmuteAudio();
  await settleControls();
  assert.deepEqual(
    state.sent.map(({ type }) => type),
    ['pauseProducer', 'resumeProducer'],
  );
  assert.equal(media.pendingControls.size, 0);
  assert.equal(media.inFlightControls.size, 0);
});

for (const retirement of ['session', 'revoked-producer']) {
  test(`late control failures after ${retirement} retirement cannot warn or replay`, async (t) => {
    const { media, state } = await fixture(t);
    await media.unmuteAudio();
    const reply = deferred();
    state.controlReply = () => reply.promise;
    media.muteAudio();
    if (retirement === 'session') media.close();
    else media.reconcileLocalProducers([]);
    reply.reject(new Error('Retired operation failed'));
    await settleControls();
    media.resumeSignaling();
    assert.equal(state.sent.length, 1);
    assert.equal(media.pendingControls.size, 0);
    assert.equal(media.inFlightControls.size, 0);
    assert.deepEqual(state.warnings, []);
  });
}

test('a snapshot confirms an unacknowledged producer closure without repeating it', async (t) => {
  const { media, state, signaling } = await fixture(t);
  await media.startScreenShare();
  const reply = deferred();
  state.controlReply = () => reply.promise;
  media.stopScreenShare();
  assert.equal(media.pendingControls.size, 2);
  signaling.connected = false;
  media.suspendSignaling();
  media.reconcileLocalProducers([]);
  signaling.connected = true;
  media.resumeSignaling();
  reply.resolve();
  await settleControls();
  assert.equal(state.sent.length, 2);
  assert.equal(media.pendingControls.size, 0);
});

test('correlated ICE acknowledgement applies the returned credentials to its native transport', async (t) => {
  const { media, state } = await fixture(t);
  const parameters = { usernameFragment: 'fresh', password: 'fresh-password', iceLite: true };
  const applied = [];
  const transport = {
    id: 'transport',
    closed: false,
    close() {},
    on(_event, changed) {
      this.changed = changed;
    },
    async restartIce(value) {
      applied.push(value);
    },
  };
  media.recvTransport = transport;
  media.setupIceRecovery(transport);
  state.controlReply = async () => ({
    type: 'iceRestarted',
    transportId: transport.id,
    iceParameters: parameters,
  });
  transport.changed('failed');
  await settleControls();
  assert.deepEqual(applied, [{ iceParameters: parameters }]);
  assert.equal(media.pendingControls.size, 0);
});

test('offline microphone changes coalesce until the room resumes on its new socket', async (t) => {
  const { media, signaling, state } = await fixture(t);
  await media.unmuteAudio();
  const producer = state.producers[0];
  signaling.connected = false;
  media.suspendSignaling();
  media.muteAudio();
  await media.unmuteAudio();
  media.muteAudio();
  assert.equal(media.audioEnabled, false);
  assert.ok(state.tracks.every((track) => track.readyState === 'ended'));
  assert.equal(media.pendingControls.size, 1);
  assert.deepEqual(state.sent, []);
  media.resumeSignaling();
  assert.deepEqual(state.sent, [], 'a disconnected resume must not discard pending controls');
  signaling.connected = true;
  media.muteAudio();
  assert.deepEqual(state.sent, [], 'socket open alone does not restore room ownership');
  media.resumeSignaling();
  assert.deepEqual(state.sent, [{ type: 'pauseProducer', producerId: producer.id }]);
  await settleControls();
  assert.equal(media.pendingControls.size, 0);
  media.resumeSignaling();
  assert.equal(state.sent.length, 1, 'a flushed state must not be replayed twice');
});

test('offline viewer changes retain only the final pause and quality state', async (t) => {
  const { media, signaling, state, newTrack } = await fixture(t);
  const consumer = new Consumer({
    id: 'consumer',
    localId: '0',
    producerId: 'remote',
    track: newTrack('video'),
    rtpParameters: { encodings: [{ scalabilityMode: 'L3T3' }] },
  });
  media.consumers.set(consumer.id, consumer);
  media.producerToConsumer.set('remote', consumer.id);
  media.setConsumerHiddenByProducer('remote', true);
  state.sent.length = 0;
  signaling.connected = false;
  media.suspendSignaling();
  for (let index = 0; index < 100; index++) {
    media.setConsumerHiddenByProducer('remote', false);
    media.setConsumerQualityByProducer('remote', 'high');
    media.setConsumerHiddenByProducer('remote', true);
    media.setConsumerQualityByProducer('remote', 'low');
  }
  media.setConsumerHiddenByProducer('remote', false);
  assert.equal(consumer.paused, false);
  assert.equal(media.pendingControls.size, 2);
  assert.deepEqual(state.sent, []);
  signaling.connected = true;
  media.resumeSignaling();
  assert.deepEqual(state.sent, [
    { type: 'resumeConsumer', consumerId: consumer.id },
    { type: 'setConsumerPreferredLayers', consumerId: consumer.id, spatialLayer: 0 },
  ]);
});

test('receive recovery awaits retirement without changing capture or mute intent', async (t) => {
  const { media, state, newTrack } = await fixture(t);
  media.recvTransport = { close() {}, closed: false };
  await media.unmuteVideo();
  await media.unmuteAudio();
  media.muteAudio();
  await settleControls();
  const localVideo = media.getLocalStream().getVideoTracks()[0];
  const captureCount = state.captureCalls.length;
  const consumer = new Consumer({
    id: 'consumer',
    localId: '0',
    producerId: 'remote',
    track: newTrack('video'),
    rtpParameters: {},
  });
  media.consumers.set(consumer.id, consumer);
  media.producerToConsumer.set('remote', consumer.id);
  const reply = deferred();
  state.controlReply = () => reply.promise;
  state.sent.length = 0;
  let completed = false;
  const retiring = media.retireConsumerByProducer('remote').then(() => {
    completed = true;
  });
  await Promise.resolve();
  assert.equal(consumer.closed, true);
  assert.equal(media.getConsumerTrackByProducer('remote'), null);
  assert.equal(completed, false, 'resubscription must wait for the server retirement');
  assert.deepEqual(state.sent, [{ type: 'closeConsumer', consumerId: 'consumer' }]);
  reply.resolve({ type: 'mediaControlApplied' });
  await retiring;
  assert.equal(state.captureCalls.length, captureCount);
  assert.equal(media.audioEnabled, false);
  assert.equal(media.videoEnabled, true);
  assert.equal(media.getLocalStream().getVideoTracks()[0], localVideo);
  assert.equal(localVideo.readyState, 'live');
});

for (const failure of ['rejected', 'replacement', 'closed']) {
  test(`receive recovery rejects a ${failure} retirement and never closes a replacement`, async (t) => {
    const { media, state, newTrack } = await fixture(t);
    media.recvTransport = { close() {}, closed: false };
    const consumer = new Consumer({
      id: 'old',
      localId: '0',
      producerId: 'remote',
      track: newTrack('video'),
      rtpParameters: {},
    });
    media.consumers.set(consumer.id, consumer);
    media.producerToConsumer.set('remote', consumer.id);
    const reply = deferred();
    state.controlReply = () => reply.promise;
    const retiring = media.retireConsumerByProducer('remote');
    const rejected = assert.rejects(retiring);
    let replacement;
    if (failure === 'closed') media.close();
    else {
      if (failure === 'replacement') media.recvTransport = { close() {}, closed: false };
      replacement = new Consumer({
        id: 'new',
        localId: '1',
        producerId: 'remote',
        track: newTrack('video'),
        rtpParameters: {},
      });
      media.consumers.set(replacement.id, replacement);
      media.producerToConsumer.set('remote', replacement.id);
    }
    if (failure === 'rejected') reply.reject(new Error('retirement refused'));
    else reply.resolve({ type: 'mediaControlApplied' });
    await rejected;
    if (replacement) {
      assert.equal(replacement.closed, false);
      assert.equal(media.getConsumerTrackByProducer('remote'), replacement.track);
    }
  });
}

test('receive recovery requires a live receive transport and restored signaling ownership', async (t) => {
  const { media, signaling, state } = await fixture(t);
  await assert.rejects(media.retireConsumerByProducer('missing'));
  media.recvTransport = { close() {}, closed: false };
  media.suspendSignaling();
  await assert.rejects(media.retireConsumerByProducer('missing'));
  signaling.connected = false;
  await assert.rejects(media.retireConsumerByProducer('missing'));
  assert.deepEqual(state.sent, []);
});

for (const failure of ['refused', 'timeout']) {
  test(`a ${failure} receive retirement must be acknowledged before another server consumer is created`, async (t) => {
    const { media, state, newTrack } = await fixture(t);
    media.device = { recvRtpCapabilities: {} };
    media.recvTransport = {
      close() {},
      closed: false,
      async consume() {
        throw new Error('A new native receiver is not part of this check');
      },
    };
    const consumer = new Consumer({
      id: 'old',
      localId: '0',
      producerId: 'remote',
      track: newTrack('video'),
      rtpParameters: {},
    });
    media.consumers.set(consumer.id, consumer);
    media.producerToConsumer.set('remote', consumer.id);
    const error = new Error('Retirement was not confirmed');
    error.name = failure === 'timeout' ? 'TimeoutError' : 'Error';
    state.controlReply = () => Promise.reject(error);
    await assert.rejects(media.retireConsumerByProducer('remote'));
    assert.equal(media.consumerRetirements.get('remote').id, 'old');
    await assert.rejects(media.consume('remote'));
    assert.deepEqual(
      state.sent.map((message) => message.type),
      ['closeConsumer', 'closeConsumer'],
    );
    const reply = deferred();
    state.controlReply = (message) =>
      message.type === 'closeConsumer'
        ? reply.promise
        : Promise.reject(new Error('New subscription reached only after retirement ACK'));
    const first = media.retireConsumerByProducer('remote');
    const second = media.consume('remote');
    const consumeFailed = assert.rejects(second);
    assert.equal(
      state.sent.filter((message) => message.type === 'closeConsumer').length,
      3,
      'concurrent cleanup shares one native request',
    );
    assert.equal(
      state.sent.some((message) => message.type === 'consume'),
      false,
    );
    reply.resolve({ type: 'mediaControlApplied' });
    await first;
    await consumeFailed;
    assert.equal(state.sent.at(-1).type, 'consume');
    assert.equal(media.consumerRetirements.size, 0);
  });
}

test('unconfirmed retirement identities are bounded without evicting or closing another consumer', async (t) => {
  const { media, state, newTrack } = await fixture(t);
  media.recvTransport = { close() {}, closed: false };
  for (let index = 0; index < 256; index++)
    media.consumerRetirements.set(`retired-${index}`, { id: `old-${index}` });
  const consumer = new Consumer({
    id: 'live',
    localId: '0',
    producerId: 'remote',
    track: newTrack('video'),
    rtpParameters: {},
  });
  media.consumers.set(consumer.id, consumer);
  media.producerToConsumer.set('remote', consumer.id);
  await assert.rejects(media.retireConsumerByProducer('remote'), /cleanup is still pending/);
  assert.equal(media.consumerRetirements.size, 256);
  assert.equal(consumer.closed, false);
  assert.deepEqual(state.sent, []);
  media.close();
  assert.equal(media.consumerRetirements.size, 0);
});

for (const closure of ['local', 'server', 'session']) {
  test(`deferred consumer controls respect ${closure} teardown`, async (t) => {
    const { media, signaling, state, newTrack } = await fixture(t);
    const consumer = new Consumer({
      id: 'consumer',
      localId: '0',
      producerId: 'remote',
      track: newTrack('video'),
      rtpParameters: { encodings: [{ scalabilityMode: 'L3T3' }] },
    });
    media.consumers.set(consumer.id, consumer);
    media.producerToConsumer.set('remote', consumer.id);
    signaling.connected = false;
    media.suspendSignaling();
    media.setConsumerHiddenByProducer('remote', true);
    media.setConsumerQualityByProducer('remote', 'low');
    if (closure === 'session') media.close();
    else media.closeConsumerByProducer('remote', closure === 'local');
    media.setPreferredLayers(consumer.id, 2);
    signaling.connected = true;
    media.resumeSignaling();
    assert.deepEqual(
      state.sent,
      closure === 'local' ? [{ type: 'closeConsumer', consumerId: consumer.id }] : [],
    );
    await settleControls();
    assert.equal(media.pendingControls.size, 0);
  });
}

test('server producer revocation retires pending resume without restarting capture', async (t) => {
  const { media, signaling, state } = await fixture(t);
  await media.unmuteAudio();
  const producer = state.producers[0];
  media.muteAudio();
  state.sent.length = 0;
  signaling.connected = false;
  media.suspendSignaling();
  await media.unmuteAudio();
  assert.equal(media.pendingControls.size, 1);
  assert.equal(media.reconcileLocalProducers([]), true);
  assert.equal(producer.closed, true);
  const captures = state.captureCalls.length;
  signaling.connected = true;
  media.resumeSignaling();
  assert.deepEqual(state.sent, []);
  assert.equal(media.audioEnabled, false);
  assert.equal(state.captureCalls.length, captures);
});

test('offline screen closure is delivered once for each retired producer', async (t) => {
  const { media, signaling, state } = await fixture(t);
  await media.startScreenShare();
  signaling.connected = false;
  media.suspendSignaling();
  media.stopScreenShare();
  media.stopScreenShare();
  assert.ok(state.tracks.every((track) => track.readyState === 'ended'));
  signaling.connected = true;
  media.resumeSignaling();
  assert.deepEqual(
    state.sent,
    state.producers.map((producer) => ({ type: 'closeProducer', producerId: producer.id })),
  );
});

for (const recovered of [false, true]) {
  test(`offline ICE restart ${recovered ? 'is cancelled after native recovery' : 'waits for signaling recovery'}`, async (t) => {
    const { media, signaling, state } = await fixture(t);
    let changed;
    const transport = {
      id: 'transport',
      on(_event, callback) {
        changed = callback;
      },
      close() {},
    };
    media.recvTransport = transport;
    media.setupIceRecovery(transport);
    signaling.connected = false;
    media.suspendSignaling();
    changed('failed');
    changed('failed');
    if (recovered) changed('connected');
    signaling.connected = true;
    media.resumeSignaling();
    assert.deepEqual(
      state.sent,
      recovered ? [] : [{ type: 'restartIce', transportId: transport.id }],
    );
  });
}

for (const direction of ['sendTransport', 'recvTransport']) {
  test(`ICE restart on ${direction} reports applied credentials only after completion`, async (t) => {
    const { state, media } = await fixture(t);
    const restart = deferred();
    const parameters = { usernameFragment: 'local-test', password: 'local-test' };
    let received;
    media[direction] = {
      id: 'current',
      closed: false,
      close() {},
      restartIce(options) {
        received = options;
        return restart.promise;
      },
    };
    const operation = media.handleIceRestarted('current', parameters);
    assert.deepEqual(received, { iceParameters: parameters });
    assert.deepEqual(state.logs, []);
    restart.resolve();
    await operation;
    assert.deepEqual(state.logs, [
      ['[media] ICE restart credentials applied for transport current'],
    ]);
    assert.deepEqual(state.warnings, []);
    assert.deepEqual(state.captureCalls, []);
  });
}

test('ICE restart installs fresh TURN credentials before regathering', async (t) => {
  const { state, media } = await fixture(t);
  const calls = [];
  const iceServers = [{ urls: ['turn:relay.example:3478'], username: '1:u', credential: 'c' }];
  const parameters = { usernameFragment: 'local-test', password: 'local-test' };
  media.sendTransport = {
    id: 'current',
    closed: false,
    close() {},
    async updateIceServers(options) {
      calls.push(['updateIceServers', options]);
    },
    async restartIce(options) {
      calls.push(['restartIce', options]);
    },
  };
  await media.handleIceRestarted('current', parameters, iceServers);
  assert.deepEqual(calls, [
    ['updateIceServers', { iceServers }],
    ['restartIce', { iceParameters: parameters }],
  ]);
  assert.deepEqual(state.warnings, []);
});

for (const relayConfigured of [false, true]) {
  test(`Firefox ICE recovery ${relayConfigured ? 'requests fresh transports for changed TURN credentials' : 'restarts without an unsupported empty server update'}`, async (t) => {
    const { state, media } = await fixture(t);
    const calls = [];
    media.onTransportRebuildRequired = () => calls.push('rebuild');
    media.sendTransport = {
      id: 'current',
      closed: false,
      close() {},
      async updateIceServers({ iceServers }) {
        calls.push('update');
        // Exercise the installed handler's actual UnsupportedError, not a
        // guessed Firefox capability or a replacement implementation.
        await Firefox120.prototype.updateIceServers.call({ assertNotClosed() {} }, iceServers);
      },
      async restartIce() {
        calls.push('restart');
      },
    };
    await media.handleIceRestarted(
      'current',
      {
        usernameFragment: 'local-test',
        password: 'local-test',
      },
      relayConfigured
        ? [{ urls: ['turn:relay.example:3478'], username: 'fresh', credential: 'secret' }]
        : [],
    );
    assert.deepEqual(calls, relayConfigured ? ['update', 'rebuild'] : ['restart']);
    assert.deepEqual(state.captureCalls, []);
    assert.deepEqual(state.warnings, []);
    assert.equal(state.logs.length, relayConfigured ? 0 : 1);
  });
}

test('a rejected TURN update requests fresh transports without exposing credentials', async (t) => {
  const { state, media } = await fixture(t);
  let rebuilds = 0;
  media.onTransportRebuildRequired = () => rebuilds++;
  media.sendTransport = {
    id: 'current',
    closed: false,
    close() {},
    async updateIceServers() {
      throw new Error('private relay credentials');
    },
    restartIce() {
      assert.fail('Failed credential updates must not be reported as applied');
    },
  };
  await media.handleIceRestarted('current', { usernameFragment: 'u', password: 'p' }, [
    { urls: ['turn:relay.example'] },
  ]);
  assert.deepEqual(state.warnings, [['[media] ICE restart failed for transport current']]);
  assert.deepEqual(state.logs, []);
  assert.equal(rebuilds, 1, 'failed native recovery must not strand the current transport');
});

for (const retirement of ['close', 'replace']) {
  test(`an unsupported TURN update after ${retirement} cannot rebuild another media session`, async (t) => {
    const { state, media } = await fixture(t);
    const update = deferred();
    let rebuilds = 0;
    media.onTransportRebuildRequired = () => rebuilds++;
    media.sendTransport = {
      id: 'current',
      closed: false,
      close() {},
      updateIceServers: () => update.promise,
      restartIce() {
        assert.fail('Retired transport must not restart');
      },
    };
    const operation = media.handleIceRestarted(
      'current',
      { usernameFragment: 'u', password: 'p' },
      [{ urls: ['turn:relay.example'] }],
    );
    if (retirement === 'close') media.close();
    else media.sendTransport = null;
    update.reject(Object.assign(new Error('private'), { name: 'UnsupportedError' }));
    await operation;
    assert.equal(rebuilds, 0);
    assert.deepEqual(state.warnings, []);
  });
}

for (const failure of ['throw', 'reject']) {
  test(`ICE restart handles a current transport ${failure} without exposing native error details`, async (t) => {
    const { state, media } = await fixture(t);
    const error = new Error('native-detail-must-not-be-logged');
    let rebuilds = 0;
    media.onTransportRebuildRequired = () => rebuilds++;
    media.sendTransport = {
      id: 'current',
      closed: false,
      close() {},
      restartIce() {
        if (failure === 'throw') throw error;
        return Promise.reject(error);
      },
    };
    await media.handleIceRestarted('current', {
      usernameFragment: 'local-test',
      password: 'local-test',
    });
    assert.deepEqual(state.logs, []);
    assert.deepEqual(state.warnings, [['[media] ICE restart failed for transport current']]);
    assert.equal(rebuilds, 1, 'the room must be able to replace failed native transports');
    assert.deepEqual(state.sent, [], 'the room owns admission and transport replacement');
    assert.deepEqual(state.captureCalls, []);
  });
}

for (const retirement of ['close', 'replace', 'transport-close']) {
  for (const result of ['resolve', 'reject']) {
    test(`ICE restart ignores a late ${result} after ${retirement}`, async (t) => {
      const { state, media } = await fixture(t);
      const restart = deferred();
      media.onTransportRebuildRequired = () =>
        assert.fail('Retired operations cannot rebuild the current session');
      const transport = {
        id: 'current',
        closed: false,
        close() {
          this.closed = true;
        },
        restartIce() {
          return restart.promise;
        },
      };
      media.sendTransport = transport;
      const operation = media.handleIceRestarted('current', {
        usernameFragment: 'local-test',
        password: 'local-test',
      });
      if (retirement === 'close') media.close();
      else if (retirement === 'replace') media.sendTransport = { ...transport };
      else transport.close();
      state.logs.length = 0;
      restart[result](new Error('retired operation'));
      await operation;
      assert.deepEqual(state.logs, []);
      assert.deepEqual(state.warnings, []);
      assert.deepEqual(state.sent, []);
    });
  }
}

test('ICE restart ignores unknown and already-closed transports', async (t) => {
  const { state, media } = await fixture(t);
  media.sendTransport = {
    id: 'closed',
    closed: true,
    close() {},
    restartIce() {
      assert.fail('closed transport must not be restarted');
    },
  };
  await media.handleIceRestarted('unknown', {
    usernameFragment: 'local-test',
    password: 'local-test',
  });
  await media.handleIceRestarted('closed', {
    usernameFragment: 'local-test',
    password: 'local-test',
  });
  assert.deepEqual(state.logs, []);
  assert.deepEqual(state.warnings, []);
});

test('track fixture distinguishes explicit stop from external capture termination', () => {
  const track = new Track('audio');
  let events = 0;
  track.addEventListener('ended', () => events++);
  track.stop();
  assert.equal(track.readyState, 'ended');
  assert.equal(events, 0);
  track.endExternally();
  assert.equal(events, 1);
});

for (const kind of ['audio', 'video']) {
  test(
    `external ${kind} end closes only its producer once and requires an explicit restart`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await startLocal(media, kind);
      const producer = state.producers[0];
      const track = producer.track;
      const otherKind = kind === 'audio' ? 'video' : 'audio';
      await startLocal(media, otherKind);
      const otherProducer = state.producers[1];
      const captures = state.captureCalls.length;

      track.endExternally();
      track.endExternally(); // Duplicate/late delivery must be harmless.
      await Promise.resolve();
      assert.equal(producer.closed, true);
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, producer).length, 1);
      assert.equal(localEnabled(media, kind), false);
      assert.deepEqual(localTracks(media, kind), []);
      assert.deepEqual(stopped, [kind]);
      assert.equal(otherProducer.closed, false);
      assert.equal(localEnabled(media, otherKind), true);
      assert.equal(
        state.captureCalls.length,
        captures,
        'external end must not request another capture',
      );

      await startLocal(media, kind);
      assert.equal(state.captureCalls.length, captures + 1);
      assert.equal(state.producers.length, 3);
      assert.equal(localEnabled(media, kind), true);
      assert.equal(localTracks(media, kind)[0].readyState, 'live');
      assert.deepEqual(stopped, [kind]);
    },
  );

  test(
    `external ${kind} end follows replaceTrack and ignores the retired track`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await startLocal(media, kind);
      const producer = state.producers[0];
      const retiredTrack = producer.track;
      await switchLocal(media, kind);
      const currentTrack = producer.track;
      const captures = state.captureCalls.length;
      assert.notEqual(currentTrack, retiredTrack);
      assert.equal(retiredTrack.readyState, 'ended');
      assert.deepEqual(stopped, [], 'deliberate replacement does not report an external end');

      retiredTrack.endExternally();
      assert.equal(producer.closed, false);
      assert.equal(localEnabled(media, kind), true);
      assert.deepEqual(localTracks(media, kind), [currentTrack]);
      assert.deepEqual(stopped, []);
      currentTrack.endExternally();
      assert.equal(producer.closed, true);
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, producer).length, 1);
      assert.equal(localEnabled(media, kind), false);
      assert.deepEqual(localTracks(media, kind), []);
      assert.deepEqual(stopped, [kind]);
      assert.equal(state.captureCalls.length, captures);
    },
  );

  test(
    `external ${kind} end during produce discards the already-ended returned producer`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      const started = deferred(),
        produce = deferred();
      t.after(() => produce.resolve());
      state.beforeProduce = (options) => {
        started.resolve(options.track);
        return produce.promise;
      };
      const activation = startLocal(media, kind);
      const track = await started.promise;
      track.endExternally();
      produce.resolve();
      await activation;
      const producer = state.producers[0];
      assert.equal(producer.closed, true);
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, producer).length, 1);
      assert.equal(localEnabled(media, kind), false);
      assert.deepEqual(localTracks(media, kind), []);
      assert.deepEqual(stopped, [kind]);
      assert.equal(state.captureCalls.length, 1);
      state.beforeProduce = null;
      await startLocal(media, kind);
      assert.equal(localEnabled(media, kind), true);
      assert.equal(state.producers.length, 2);
    },
  );

  test(
    `external end of pending ${kind} replacement cannot enable an ended track`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await startLocal(media, kind);
      const producer = state.producers[0];
      const started = deferred(),
        replacement = deferred();
      t.after(() => replacement.resolve());
      state.replace = (track) => {
        started.resolve(track);
        return replacement.promise;
      };
      const switching = switchLocal(media, kind);
      const track = await started.promise;
      track.endExternally();
      replacement.resolve();
      await switching;
      assert.equal(producer.closed, true);
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, producer).length, 1);
      assert.equal(localEnabled(media, kind), false);
      assert.deepEqual(localTracks(media, kind), []);
      assert.equal(track.readyState, 'ended');
      assert.deepEqual(stopped, [kind]);
      assert.equal(state.captureCalls.length, 2);
      state.replace = null;
      await startLocal(media, kind);
      assert.equal(localEnabled(media, kind), true);
      assert.equal(state.producers.length, 2);
    },
  );

  test(
    `temporary ${kind} mute/unmute events do not close or recapture`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await startLocal(media, kind);
      const producer = state.producers[0];
      const track = producer.track;
      track.dispatchEvent(new Event('mute'));
      track.dispatchEvent(new Event('unmute'));
      await Promise.resolve();
      assert.equal(track.readyState, 'live');
      assert.equal(producer.closed, false);
      assert.equal(producer.close.mock.callCount(), 0);
      assert.deepEqual(closeMessages(state, producer), []);
      assert.equal(localEnabled(media, kind), true);
      assert.deepEqual(localTracks(media, kind), [track]);
      assert.deepEqual(stopped, []);
      assert.equal(state.captureCalls.length, 1);
    },
  );

  test(
    `external ${kind} end after manager close cannot notify, signal, or recapture`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await startLocal(media, kind);
      const producer = state.producers[0];
      const track = producer.track;
      media.close();
      const sent = [...state.sent];
      track.endExternally();
      await Promise.resolve();
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(localEnabled(media, kind), false);
      assert.equal(media.getLocalStream(), null);
      assert.deepEqual(stopped, []);
      assert.deepEqual(state.sent, sent);
      assert.equal(state.captureCalls.length, 1);
    },
  );
}

for (const stage of ['capture', 'replace']) {
  test(
    `external active camera end during device ${stage} cancels the late replacement`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media, newTrack } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      await media.unmuteVideo();
      const producer = state.producers[0];
      const activeTrack = producer.track;
      const started = deferred(),
        pending = deferred();
      const replacementTrack = newTrack('video');
      const result = stage === 'capture' ? new Stream([replacementTrack]) : undefined;
      t.after(() => pending.resolve(result));
      state.capture = () =>
        stage === 'capture'
          ? (started.resolve(), pending.promise)
          : Promise.resolve(new Stream([replacementTrack]));
      if (stage === 'replace')
        state.replace = () => {
          started.resolve();
          return pending.promise;
        };
      const switching = media.switchCamera('next-camera');
      await started.promise;
      activeTrack.endExternally();
      pending.resolve(result);
      await switching;
      assert.equal(producer.closed, true);
      assert.equal(producer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, producer).length, 1);
      assert.equal(replacementTrack.readyState, 'ended');
      assert.equal(media.videoEnabled, false);
      assert.deepEqual(localTracks(media, 'video'), []);
      assert.deepEqual(stopped, ['video']);
      assert.equal(state.captureCalls.length, 2);
    },
  );
}

for (const kind of ['audio', 'video']) {
  test(
    `an already-ended ${kind} capture result never reaches produce`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media, newTrack } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      const track = newTrack(kind);
      track.endExternally(); // The browser's event predates the manager's listener.
      state.capture = async () => new Stream([track]);
      await startLocal(media, kind);
      assert.equal(state.producers.length, 0);
      assert.equal(localEnabled(media, kind), false);
      assert.deepEqual(localTracks(media, kind), []);
      assert.deepEqual(stopped, [kind]);
      assert.equal(state.captureCalls.length, 1);
      track.endExternally();
      assert.deepEqual(stopped, [kind]);
      state.capture = null;
      await startLocal(media, kind);
      assert.equal(localEnabled(media, kind), true);
      assert.equal(state.producers.length, 1);
    },
  );

  for (const stage of ['produce', 'replace']) {
    test(
      `${kind} ${stage} checks ended readiness before the queued event arrives`,
      { timeout: 2_000 },
      async (t) => {
        const { state, media } = await fixture(t);
        const stopped = [];
        media.onLocalCaptureStopped = (value) => stopped.push(value);
        if (stage === 'replace') await startLocal(media, kind);
        const started = deferred(),
          pending = deferred();
        t.after(() => pending.resolve());
        if (stage === 'produce') {
          state.beforeProduce = (options) => {
            started.resolve(options.track);
            return pending.promise;
          };
        } else {
          state.replace = (track) => {
            started.resolve(track);
            return pending.promise;
          };
        }
        const activation = stage === 'produce' ? startLocal(media, kind) : switchLocal(media, kind);
        const track = await started.promise;
        // Native readiness can change before the ended event's task is delivered.
        // Do not dispatch here: this specifically tests the post-await state check.
        track.readyState = 'ended';
        pending.resolve();
        await activation;
        const producer = state.producers[0];
        assert.equal(producer.closed, true);
        assert.equal(producer.close.mock.callCount(), 1);
        assert.equal(closeMessages(state, producer).length, 1);
        assert.equal(localEnabled(media, kind), false);
        assert.deepEqual(localTracks(media, kind), []);
        assert.deepEqual(stopped, [kind]);
        track.endExternally();
        assert.equal(producer.close.mock.callCount(), 1);
        assert.equal(closeMessages(state, producer).length, 1);
        assert.deepEqual(stopped, [kind]);
        assert.equal(state.captureCalls.length, stage === 'produce' ? 1 : 2);
      },
    );
  }
}

test(
  'late ended events from deliberately paused capture do not close producers or notify',
  { timeout: 2_000 },
  async (t) => {
    const { state, media } = await fixture(t);
    const stopped = [];
    media.onLocalCaptureStopped = (value) => stopped.push(value);
    await media.unmuteAudio();
    await media.unmuteVideo();
    const [audioProducer, videoProducer] = state.producers;
    const retiredTracks = state.producers.map((producer) => producer.track);
    media.muteAudio();
    media.pauseVideo();
    for (const track of retiredTracks) track.endExternally();
    assert.equal(audioProducer.closed, false);
    assert.equal(videoProducer.closed, false);
    assert.equal(audioProducer.close.mock.callCount(), 0);
    assert.equal(videoProducer.close.mock.callCount(), 0);
    assert.equal(media.audioEnabled, false);
    assert.equal(media.videoEnabled, false);
    assert.deepEqual(stopped, []);
    assert.equal(state.captureCalls.length, 2);
    assert.equal(
      state.sent.some((message) => message.type === 'closeProducer'),
      false,
    );
    await media.unmuteAudio();
    await media.unmuteVideo();
    assert.equal(state.producers.length, 2);
    assert.equal(media.audioEnabled, true);
    assert.equal(media.videoEnabled, true);
    for (const track of retiredTracks) track.endExternally();
    assert.equal(media.audioEnabled, true);
    assert.equal(media.videoEnabled, true);
    assert.deepEqual(stopped, []);
  },
);

for (const stage of ['produce', 'replace']) {
  test(
    `a fresh camera start survives stale ${stage} completion and retains pending-capture cancellation`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      if (stage === 'replace') await media.unmuteVideo();
      const oldStarted = deferred(),
        oldPending = deferred();
      const freshStarted = deferred(),
        freshPending = deferred();
      t.after(() => {
        oldPending.resolve();
        freshPending.resolve();
      });
      if (stage === 'produce') {
        state.beforeProduce = (options) => {
          oldStarted.resolve(options.track);
          return oldPending.promise;
        };
      } else {
        state.replace = (track) => {
          oldStarted.resolve(track);
          return oldPending.promise;
        };
      }
      const oldActivation =
        stage === 'produce' ? media.unmuteVideo() : media.switchCamera('next-camera');
      const oldTrack = await oldStarted.promise;
      oldTrack.endExternally();
      assert.deepEqual(stopped, ['video']);

      state.beforeProduce = (options) => {
        freshStarted.resolve(options.track);
        return freshPending.promise;
      };
      const freshActivation = media.unmuteVideo();
      const freshTrack = await freshStarted.promise;
      assert.notEqual(freshTrack, oldTrack);
      oldPending.resolve();
      await oldActivation;
      assert.equal(freshTrack.readyState, 'live', 'stale completion must not stop a fresh capture');
      assert.deepEqual(stopped, ['video']);
      assert.equal(state.producers[0].closed, true);
      assert.equal(state.producers[0].close.mock.callCount(), 1);
      assert.equal(closeMessages(state, state.producers[0]).length, 1);

      // This public operation also proves the old finally block did not erase
      // the newer pending-track reference and leave capture running after pause.
      media.pauseVideo();
      assert.equal(freshTrack.readyState, 'ended');
      freshPending.resolve();
      await freshActivation;
      assert.equal(state.producers[1].closed, true);
      assert.equal(state.producers[1].close.mock.callCount(), 1);
      assert.equal(closeMessages(state, state.producers[1]).length, 1);
      assert.equal(media.videoEnabled, false);
      assert.deepEqual(localTracks(media, 'video'), []);
      assert.deepEqual(stopped, ['video']);
      state.beforeProduce = state.replace = null;
      await media.unmuteVideo();
      assert.equal(media.videoEnabled, true);
      assert.equal(state.producers.length, 3);
      assert.deepEqual(stopped, ['video']);
    },
  );

  test(
    `an already-active fresh camera survives an older cancelled ${stage}`,
    { timeout: 2_000 },
    async (t) => {
      const { state, media } = await fixture(t);
      const stopped = [];
      media.onLocalCaptureStopped = (value) => stopped.push(value);
      if (stage === 'replace') await media.unmuteVideo();
      const started = deferred(),
        pending = deferred();
      t.after(() => pending.resolve());
      if (stage === 'produce') {
        state.beforeProduce = (options) => {
          started.resolve(options.track);
          return pending.promise;
        };
      } else {
        state.replace = (track) => {
          started.resolve(track);
          return pending.promise;
        };
      }
      const oldActivation =
        stage === 'produce' ? media.unmuteVideo() : media.switchCamera('next-camera');
      const oldTrack = await started.promise;
      oldTrack.endExternally();
      state.beforeProduce = state.replace = null;
      await media.unmuteVideo();
      const freshTrack = localTracks(media, 'video')[0];
      const freshProducer = state.producers.find((producer) => producer.track === freshTrack);
      assert.equal(media.videoEnabled, true);
      oldTrack.endExternally();
      pending.resolve();
      await oldActivation;
      assert.equal(media.videoEnabled, true);
      assert.equal(freshTrack.readyState, 'live');
      assert.deepEqual(localTracks(media, 'video'), [freshTrack]);
      assert.equal(freshProducer.closed, false);
      assert.equal(freshProducer.close.mock.callCount(), 0);
      assert.deepEqual(closeMessages(state, freshProducer), []);
      const oldProducer = state.producers.find((producer) => producer !== freshProducer);
      assert.equal(oldProducer.closed, true);
      assert.equal(oldProducer.close.mock.callCount(), 1);
      assert.equal(closeMessages(state, oldProducer).length, 1);
      assert.deepEqual(stopped, ['video']);
      assert.equal(state.captureCalls.length, stage === 'produce' ? 2 : 3);
    },
  );
}

async function pttFor(media) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const functions = ast.statements.filter(
    (node) =>
      ts.isFunctionDeclaration(node) && ['pttActivate', 'pttDeactivate'].includes(node.name?.text),
  );
  const buttons = [],
    errors = [];
  const api = evaluateTypeScript(
    `
    let pttHeld = false;
    let pttActivation = 0;
    ${functions.map((node) => node.getText(ast)).join('\n')}
    export { pttActivate, pttDeactivate };
    export function held() { return pttHeld; }
  `,
    {
      globals: {
        room: {
          hasMedia: true,
          get audioEnabled() {
            return media.audioEnabled;
          },
          unmuteAudio: () => media.unmuteAudio(),
          muteAudio: () => media.muteAudio(),
        },
        updateMicButton: (enabled) => buttons.push(enabled),
        updateLocalTile() {},
        showToast: (error) => errors.push(error),
      },
    },
  );
  return { ...api, buttons, errors };
}

test('PTT release cancels a pending permission/capture result', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(),
    capture = deferred();
  state.capture = () => {
    started.resolve();
    return capture.promise;
  };
  const activation = ptt.pttActivate();
  await started.promise;
  ptt.pttDeactivate();
  const track = newTrack('audio');
  capture.resolve(new Stream([track]));
  await activation;
  assert.equal(track.readyState, 'ended');
  assert.equal(media.audioEnabled, false);
  assert.equal(ptt.held(), false);
  assert.deepEqual(ptt.buttons, [false]);
  assert.equal(state.producers.length, 0);
});

test('PTT release during produce stops capture immediately and closes the late producer', async (t) => {
  const { state, media } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(),
    produce = deferred();
  state.beforeProduce = (options) => {
    started.resolve(options.track);
    return produce.promise;
  };
  const activation = ptt.pttActivate();
  const track = await started.promise;
  assert.equal(track.enabled, false);
  ptt.pttDeactivate();
  assert.equal(track.readyState, 'ended');
  produce.resolve();
  await activation;
  assert.equal(state.producers[0].closed, true);
  assert.equal(media.audioEnabled, false);
  assert.deepEqual(ptt.buttons, [false]);
  assert.deepEqual(state.sent, [{ type: 'closeProducer', producerId: state.producers[0].id }]);
});

test('PTT release during replaceTrack cannot resume the paused producer', async (t) => {
  const { state, media } = await fixture(t);
  await media.unmuteAudio();
  assert.equal(media.audioEnabled, true);
  media.muteAudio();
  const ptt = await pttFor(media);
  const started = deferred(),
    replacement = deferred();
  state.replace = (track) => {
    started.resolve(track);
    return replacement.promise;
  };
  const activation = ptt.pttActivate();
  const track = await started.promise;
  ptt.pttDeactivate();
  assert.equal(track.readyState, 'ended');
  replacement.resolve();
  await activation;
  assert.equal(media.audioEnabled, false);
  assert.equal(
    state.sent.some((message) => message.type === 'resumeProducer'),
    false,
  );
  state.replace = null;
  await ptt.pttActivate();
  assert.equal(media.audioEnabled, true);
  assert.equal(state.producers.length, 1);
});

test('a new PTT hold survives completion of an earlier cancelled activation', async (t) => {
  const { state, media } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(),
    produce = deferred();
  state.beforeProduce = (options) => {
    started.resolve(options.track);
    return produce.promise;
  };
  const first = ptt.pttActivate();
  await started.promise;
  ptt.pttDeactivate();
  const second = ptt.pttActivate();
  state.beforeProduce = null;
  produce.resolve();
  await Promise.all([first, second]);
  assert.equal(ptt.held(), true);
  assert.equal(media.audioEnabled, true);
  assert.equal(state.producers[0].closed, true);
  assert.equal(state.producers[1].paused, false);
  assert.deepEqual(ptt.buttons, [false, true]);
});

for (const stage of ['capture', 'produce', 'replace']) {
  test(`PTT handles ${stage} errors, stops captured tracks, and permits retry`, async (t) => {
    const { state, media } = await fixture(t);
    if (stage === 'replace') {
      await media.unmuteAudio();
      media.muteAudio();
    }
    const ptt = await pttFor(media);
    const failure = async () => {
      throw new Error(`${stage} failed`);
    };
    state[stage === 'produce' ? 'beforeProduce' : stage] = failure;
    await ptt.pttActivate();
    assert.equal(ptt.held(), false);
    assert.equal(media.audioEnabled, false);
    assert.equal(
      state.tracks.every((track) => track.readyState === 'ended'),
      true,
    );
    assert.deepEqual(ptt.errors, [`${stage} failed`]);
    state.capture = state.beforeProduce = state.replace = null;
    await ptt.pttActivate();
    assert.equal(ptt.held(), true);
    assert.equal(media.audioEnabled, true);
  });
}

for (const source of ['microphone', 'camera', 'screen', 'screen-audio']) {
  test(`forced ${source} closure stops local capture and allows a fresh producer`, async (t) => {
    const { state, media } = await fixture(t);
    const start = () =>
      source === 'microphone'
        ? media.unmuteAudio()
        : source === 'camera'
          ? media.toggleVideo()
          : media.startScreenShare();
    await start();
    const producer = state.producers.find((item) => item.appData.source === source);
    const oldTracks = [...state.tracks];
    assert.equal(media.closeLocalProducer(producer.id), true);
    assert.equal(media.closeLocalProducer(producer.id), false);
    assert.equal(producer.closed, true);
    assert.equal(
      oldTracks.every((track) => track.readyState === 'ended'),
      true,
    );
    assert.equal(media.audioEnabled, false);
    assert.equal(media.videoEnabled, false);
    assert.equal(media.isScreenSharing, false);
    await start();
    assert.equal(state.producers.filter((item) => item.appData.source === source).length, 2);
    assert.equal(
      source === 'microphone'
        ? media.audioEnabled
        : source === 'camera'
          ? media.videoEnabled
          : media.isScreenSharing,
      true,
    );
    assert.equal(
      state.sent.some(
        (message) => message.type === 'resumeProducer' && message.producerId === producer.id,
      ),
      false,
    );
  });
}

for (const stage of ['capture', 'replace']) {
  test(`forced camera closure during ${stage} discards the pending camera track`, async (t) => {
    const { state, media, newTrack } = await fixture(t);
    await media.toggleVideo();
    const producer = state.producers[0];
    await media.toggleVideo();
    const started = deferred(),
      pending = deferred();
    const track = newTrack('video');
    if (stage === 'capture') {
      state.capture = () => {
        started.resolve();
        return pending.promise;
      };
    } else {
      state.capture = async () => new Stream([track]);
      state.replace = () => {
        started.resolve();
        return pending.promise;
      };
    }
    const activation = media.toggleVideo();
    await started.promise;
    media.closeLocalProducer(producer.id);
    pending.resolve(new Stream([track]));
    assert.equal(await activation, false);
    assert.equal(track.readyState, 'ended');
    assert.equal(media.videoEnabled, false);
  });
}

for (const kind of ['audio', 'video']) {
  test(`forced closure during ${kind} device switching releases the new capture`, async (t) => {
    const { state, media, newTrack } = await fixture(t);
    if (kind === 'audio') await media.unmuteAudio();
    else await media.toggleVideo();
    const producer = state.producers[0];
    const started = deferred(),
      capture = deferred();
    state.capture = () => {
      started.resolve();
      return capture.promise;
    };
    const switching =
      kind === 'audio' ? media.switchMic('new-device') : media.switchCamera('new-device');
    await started.promise;
    media.closeLocalProducer(producer.id);
    const track = newTrack(kind);
    capture.resolve(new Stream([track]));
    await switching;
    assert.equal(track.readyState, 'ended');
    assert.equal(kind === 'audio' ? media.audioEnabled : media.videoEnabled, false);
  });
}

test('PTT release cancels microphone capture during a device switch', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  await media.unmuteAudio();
  const started = deferred(),
    capture = deferred();
  state.capture = () => {
    started.resolve();
    return capture.promise;
  };
  const switching = media.switchMic('new-mic');
  await started.promise;
  media.muteAudio();
  const track = newTrack('audio');
  capture.resolve(new Stream([track]));
  await switching;
  assert.equal(track.readyState, 'ended');
  assert.equal(media.audioEnabled, false);
});

test('forced screen-video closure cannot resurrect a pending screen-audio producer', async (t) => {
  const { state, media } = await fixture(t);
  const started = deferred(),
    audio = deferred();
  state.beforeProduce = (options) => {
    if (options.appData.source === 'screen-audio') {
      started.resolve();
      return audio.promise;
    }
  };
  const sharing = media.startScreenShare();
  await started.promise;
  const video = state.producers[0];
  media.closeLocalProducer(video.id);
  assert.equal(
    state.tracks.every((track) => track.readyState === 'ended'),
    true,
  );
  audio.resolve();
  assert.deepEqual(await sharing, { status: 'not_started', reason: 'superseded' });
  assert.equal(
    state.producers.every((producer) => producer.closed),
    true,
  );
  assert.equal(media.isScreenSharing, false);
  state.beforeProduce = null;
  assert.equal((await media.startScreenShare()).status, 'started');
  assert.equal(media.isScreenSharing, true);
});

test('ending screen capture during video publication cancels the whole pending share', async (t) => {
  const { state, media } = await fixture(t);
  const started = deferred(),
    published = deferred();
  let stopped = 0;
  media.onScreenShareStopped = () => stopped++;
  state.beforeProduce = (options) => {
    if (options.appData.source === 'screen') {
      started.resolve();
      return published.promise;
    }
  };
  const sharing = media.startScreenShare();
  await started.promise;
  state.tracks[0].endExternally();
  assert.ok(state.tracks.every((track) => track.readyState === 'ended'));
  published.resolve();
  assert.deepEqual(await sharing, { status: 'not_started', reason: 'superseded' });
  assert.equal(media.isScreenSharing, false);
  assert.equal(stopped, 1);
  assert.equal(state.producers.length, 1, 'cancelled sharing must not publish audio');
  assert.ok(state.producers.every((producer) => producer.closed));
  assert.equal(closeMessages(state, state.producers[0]).length, 1);
  state.beforeProduce = null;
  assert.equal(
    (await media.startScreenShare()).status,
    'started',
    'a new capture can start after cancellation',
  );
});

for (const [name, reason] of [
  ['NotAllowedError', 'cancelled_or_denied'],
  ['NotReadableError', 'not_readable'],
  ['NotFoundError', 'no_source'],
  ['InvalidStateError', 'invalid_state'],
  ['AbortError', 'failed'],
]) {
  test(`screen picker ${name} returns a safe explicit outcome and permits a new attempt`, async (t) => {
    const { state, media } = await fixture(t);
    state.displayCapture = () => {
      throw new DOMException('private device detail', name);
    };
    assert.deepEqual(await media.startScreenShare(), { status: 'not_started', reason });
    assert.equal(media.isScreenSharing, false);
    assert.equal(media.screenShareAudio, 'off');
    state.displayCapture = null;
    assert.deepEqual(await media.startScreenShare(), { status: 'started', audio: 'sharing' });
  });
}

test('optional screen audio failure keeps video sharing and clearly reports partial success', async (t) => {
  const { state, media } = await fixture(t);
  state.beforeProduce = (options) => {
    if (options.appData.source === 'screen-audio') throw new Error('private native failure');
  };
  assert.deepEqual(await media.startScreenShare(), { status: 'started', audio: 'failed' });
  assert.equal(media.isScreenSharing, true);
  assert.equal(state.tracks.find((track) => track.kind === 'video').readyState, 'live');
  assert.equal(state.tracks.find((track) => track.kind === 'audio').readyState, 'ended');
  assert.equal(state.producers.length, 1);
  media.stopScreenShare();
  assert.equal(media.screenShareAudio, 'off');
  assert.ok(state.tracks.every((track) => track.readyState === 'ended'));
});

test('screen audio ending independently updates state and retires only its producer', async (t) => {
  const { state, media } = await fixture(t);
  const changes = [];
  media.onScreenShareAudioChanged = (value) => changes.push(value);
  await media.startScreenShare();
  const audio = state.tracks.find((track) => track.kind === 'audio');
  audio.endExternally();
  assert.equal(media.isScreenSharing, true);
  assert.equal(media.screenShareAudio, 'ended');
  assert.equal(state.producers.find((producer) => producer.kind === 'audio').closed, true);
  assert.equal(state.producers.find((producer) => producer.kind === 'video').closed, false);
  assert.deepEqual(changes, ['not_provided', 'sharing', 'ended']);
  media.close();
  assert.equal(media.screenShareAudio, 'off');
});

test('screen sharing without an audio track does not claim that sound is included', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  state.displayCapture = () => new Stream([newTrack('video')]);
  assert.deepEqual(await media.startScreenShare(), { status: 'started', audio: 'not_provided' });
  assert.equal(media.isScreenSharing, true);
});

for (const stage of ['receiver', 'resume']) {
  test(`failed consumer ${stage} closes the server subscription and permits a clean retry`, async (t) => {
    const { state, media, newTrack } = await fixture(t);
    let fail = true;
    const consumers = [];
    const capabilities = { codecs: [] };
    media.device = { recvRtpCapabilities: capabilities };
    const controlRequest = media.signaling.request.bind(media.signaling);
    media.signaling.request = async (message, responseType) => {
      if (message.type === 'closeConsumer') return controlRequest(message, responseType);
      if (message.type === 'consume') {
        assert.equal(message.rtpCapabilities, capabilities);
        return {
          consumerId: fail ? 'failed-consumer' : 'retried-consumer',
          producerId: 'remote',
          kind: 'video',
          rtpParameters: {},
        };
      }
      if (fail && stage === 'resume') throw new Error('resume failed');
      return { type: 'consumerResumed', consumerId: message.consumerId };
    };
    media.recvTransport = {
      close() {},
      async consume(options) {
        if (fail && stage === 'receiver') throw new Error('receiver failed');
        const consumer = new Consumer({
          ...options,
          localId: options.id,
          track: newTrack('video'),
        });
        consumers.push(consumer);
        return consumer;
      },
    };
    await assert.rejects(media.consume('remote'), /failed/);
    assert.deepEqual(state.sent, [{ type: 'closeConsumer', consumerId: 'failed-consumer' }]);
    assert.ok(consumers.every((consumer) => consumer.closed));
    assert.equal(media.consumers.size, 0);
    assert.equal(media.producerToConsumer.size, 0);
    fail = false;
    assert.equal((await media.consume('remote')).readyState, 'live');
    assert.equal(media.consumers.size, 1);
    assert.equal(media.producerToConsumer.get('remote'), 'retried-consumer');
  });
}

for (const closure of ['session', 'producer', 'server']) {
  test(`consumer quality state is released on ${closure} closure`, async (t) => {
    const { state, media, newTrack } = await fixture(t);
    const consumer = new Consumer({
      id: 'consumer',
      localId: '0',
      producerId: 'remote',
      track: newTrack('video'),
      rtpParameters: { encodings: [{ scalabilityMode: 'L3T3' }] },
    });
    media.consumers.set(consumer.id, consumer);
    media.producerToConsumer.set('remote', consumer.id);
    media.setConsumerQualityByProducer('remote', 'low');
    media.setConsumerSizeCapByProducer('remote', 1);
    state.sent.length = 0;
    if (closure === 'session') media.close();
    else {
      media.closeConsumerByProducer('remote', closure !== 'server');
      media.closeConsumerByProducer('remote', closure !== 'server');
    }
    assert.equal(consumer.closed, true);
    assert.equal(media.consumerQualities.size, 0);
    assert.equal(media.consumerSizeCaps.size, 0);
    assert.deepEqual(
      state.sent,
      closure === 'producer' ? [{ type: 'closeConsumer', consumerId: consumer.id }] : [],
    );
  });
}

test('leaving during microphone capture discards its late track', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  const started = deferred(),
    capture = deferred();
  state.capture = () => {
    started.resolve();
    return capture.promise;
  };
  const activation = media.unmuteAudio();
  await started.promise;
  media.close();
  const track = newTrack('audio');
  capture.resolve(new Stream([track]));
  await activation;
  assert.equal(track.readyState, 'ended');
  assert.equal(media.audioEnabled, false);
  assert.equal(state.producers.length, 0);
});

test('saved setup configures future capture without starting devices', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  const captures = [];
  state.capture = async (constraints) => {
    captures.push(constraints);
    return new Stream([newTrack(constraints.audio ? 'audio' : 'video')]);
  };
  media.setCapturePreferences({
    cameraDeviceId: 'camera-two',
    microphoneDeviceId: 'mic-two',
    resolution: '1080p',
    frameRate: 15,
    echoCancellation: false,
    autoGainControl: false,
    noiseSuppression: true,
  });
  assert.equal(captures.length, 0);
  await media.toggleVideo();
  await media.unmuteAudio();
  assert.equal(captures[0].video.deviceId.exact, 'camera-two');
  assert.equal(captures[0].video.width.ideal, 1920);
  assert.equal(captures[0].video.frameRate.ideal, 15);
  assert.equal(captures[1].audio.deviceId.exact, 'mic-two');
  assert.equal(captures[1].audio.echoCancellation, false);
  assert.equal(captures[1].audio.autoGainControl, false);
  assert.equal(captures[1].audio.noiseSuppression, true);
});

test('switching an inactive camera saves its selection without capturing it', async (t) => {
  const { state, media } = await fixture(t);
  let captures = 0;
  state.capture = async () => {
    captures++;
    throw new Error('Unexpected capture');
  };
  await media.switchCamera('camera-two');
  assert.equal(captures, 0);
  assert.equal(media.capturePreferences.cameraDeviceId, 'camera-two');
});

for (const stage of ['capture', 'produce']) {
  test(`leaving during first camera ${stage} discards the late result`, async (t) => {
    const { state, media, newTrack } = await fixture(t);
    const started = deferred(),
      pending = deferred();
    let track;
    if (stage === 'capture')
      state.capture = () => {
        started.resolve();
        return pending.promise;
      };
    else
      state.beforeProduce = async (options) => {
        track = options.track;
        started.resolve();
        await pending.promise;
      };
    const activation = media.toggleVideo();
    await started.promise;
    media.close();
    if (stage === 'capture') {
      track = newTrack('video');
      pending.resolve(new Stream([track]));
    } else pending.resolve();
    assert.equal(await activation, false);
    assert.equal(track.readyState, 'ended');
    assert.equal(media.videoEnabled, false);
    assert.ok(state.producers.every((producer) => producer.closed));
  });
}

test('hide and restore pause only the selected viewer consumer', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  const track = newTrack('video');
  const consumer = new Consumer({
    id: 'consumer',
    localId: '0',
    producerId: 'remote',
    track,
    rtpParameters: {},
  });
  media.consumers.set(consumer.id, consumer);
  media.producerToConsumer.set('remote', consumer.id);
  media.setConsumerHiddenByProducer('remote', true);
  media.setConsumerHiddenByProducer('remote', true);
  assert.equal(consumer.paused, true);
  assert.equal(track.enabled, false);
  assert.equal(track.readyState, 'live');
  media.setConsumerHiddenByProducer('remote', false);
  assert.equal(consumer.paused, false);
  assert.equal(track.enabled, true);
  await settleControls();
  assert.deepEqual(
    state.sent.map((message) => message.type),
    ['pauseConsumer', 'resumeConsumer'],
  );
  assert.ok(
    state.sent.every((message) => message.consumerId === consumer.id && !message.producerId),
  );
});

test('the microphone producer negotiates Opus DTX and in-band FEC', async (t) => {
  const { state, media } = await fixture(t);
  const options = [];
  state.beforeProduce = async (produceOptions) => {
    options.push(produceOptions);
  };
  await startLocal(media, 'audio');
  assert.equal(options.length, 1);
  assert.deepEqual(options[0].appData, { source: 'microphone' });
  assert.deepEqual(options[0].codecOptions, { opusDtx: true, opusFec: true });
});

test('quality preferences respect available simulcast layers and skip single-layer video', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  for (const [id, mode] of [
    ['camera', 'S3T3'],
    ['screen', 'L1T1'],
  ]) {
    const consumer = new Consumer({
      id,
      localId: id,
      producerId: id,
      track: newTrack('video'),
      rtpParameters: { encodings: [{ scalabilityMode: mode }] },
    });
    media.consumers.set(id, consumer);
    media.producerToConsumer.set(id, id);
  }
  for (const quality of ['low', 'medium', 'high', 'auto']) {
    assert.equal(media.setConsumerQualityByProducer('camera', quality), true);
    await settleControls();
  }
  assert.equal(media.setConsumerQualityByProducer('screen', 'high'), false);
  assert.equal(media.setConsumerQualityByProducer('missing', 'high'), false);
  assert.deepEqual(
    state.sent.map((message) => message.spatialLayer),
    [0, 1, 2, 2],
  );
  assert.ok(
    state.sent.every(
      (message) => message.type === 'setConsumerPreferredLayers' && message.consumerId === 'camera',
    ),
  );
});

test('a tile-size cap bounds the requested layer and the manual choice stays the stricter one', async (t) => {
  const { state, media, newTrack } = await fixture(t);
  for (const [id, mode] of [
    ['camera', 'S3T3'],
    ['screen', 'L1T1'],
  ]) {
    const consumer = new Consumer({
      id,
      localId: id,
      producerId: id,
      track: newTrack('video'),
      rtpParameters: { encodings: [{ scalabilityMode: mode }] },
    });
    media.consumers.set(id, consumer);
    media.producerToConsumer.set(id, id);
  }
  assert.equal(
    media.setConsumerSizeCapByProducer('camera', 1),
    true,
    'a small tile caps at layer 1',
  );
  await settleControls();
  assert.equal(
    media.setConsumerQualityByProducer('camera', 'high'),
    true,
    'high cannot exceed the cap',
  );
  await settleControls();
  assert.equal(
    media.setConsumerSizeCapByProducer('camera', null),
    true,
    'a large tile lifts the cap',
  );
  await settleControls();
  assert.equal(media.setConsumerQualityByProducer('camera', 'low'), true);
  await settleControls();
  assert.equal(
    media.setConsumerSizeCapByProducer('camera', 2),
    true,
    'low stays below a loose cap',
  );
  await settleControls();
  assert.equal(
    media.setConsumerSizeCapByProducer('screen', 0),
    false,
    'single-layer video ignores caps',
  );
  assert.equal(media.setConsumerSizeCapByProducer('missing', 0), false);
  assert.deepEqual(
    state.sent.map((message) => message.spatialLayer),
    [1, 1, 2, 0, 0],
  );
});

for (const stage of [
  'capabilities',
  'device-factory',
  'device-load',
  'send-transport',
  'receive-transport',
]) {
  test(`closing media during ${stage} setup prevents later transport work`, async (t) => {
    const pending = deferred(),
      started = deferred();
    const requests = [],
      transports = [];
    const responseFor = (message) =>
      message.type === 'getRouterRtpCapabilities'
        ? { rtpCapabilities: {} }
        : { transportId: message.type === 'createSendTransport' ? 'send' : 'receive' };
    const expectedType =
      stage === 'capabilities'
        ? 'getRouterRtpCapabilities'
        : stage === 'send-transport'
          ? 'createSendTransport'
          : 'createRecvTransport';
    class Device {
      static async factory() {
        if (stage === 'device-factory') {
          started.resolve();
          await pending.promise;
        }
        return new Device();
      }
      async load() {
        if (stage === 'device-load') {
          started.resolve();
          await pending.promise;
        }
      }
      createSendTransport() {
        return this.transport('send');
      }
      createRecvTransport() {
        return this.transport('receive');
      }
      transport(id) {
        const result = {
          id,
          closed: false,
          on() {},
          close() {
            this.closed = true;
          },
        };
        transports.push(result);
        return result;
      }
    }
    const { MediaManager } = await loadTypeScript('src/media.ts', {
      modules: { 'mediasoup-client': { Device }, './signaling': signalingModule },
      globals: {
        localStorage: {
          getItem() {
            return null;
          },
        },
        console: { log() {} },
      },
    });
    const manager = new MediaManager({
      async request(message) {
        requests.push(message.type);
        if (!stage.startsWith('device-') && message.type === expectedType) {
          started.resolve();
          await pending.promise;
        }
        return responseFor(message);
      },
    });
    t.after(() => manager.close());
    const setup = manager.setup();
    await started.promise;
    manager.close();
    const requestCount = requests.length;
    pending.resolve();
    await assert.rejects(setup, /Media session closed/);
    assert.equal(requests.length, requestCount);
    assert.ok(transports.every((transport) => transport.closed));
    assert.equal(manager.sendTransport, null);
    assert.equal(manager.recvTransport, null);
  });
}

for (const stage of ['response', 'receiver', 'resume']) {
  test(`closing media during consumer ${stage} discards the result and stops its track`, async (t) => {
    const { media, newTrack } = await fixture(t);
    const pending = deferred(),
      started = deferred();
    let consumer,
      consumeCalls = 0;
    const requests = [];
    media.device = { recvRtpCapabilities: {} };
    media.signaling.request = async (message) => {
      requests.push(message.type);
      if (
        (stage === 'response' && message.type === 'consume') ||
        (stage === 'resume' && message.type === 'resumeConsumer')
      ) {
        started.resolve();
        await pending.promise;
      }
      return {
        consumerId: 'remote-consumer',
        producerId: 'remote-producer',
        kind: 'video',
        rtpParameters: {},
      };
    };
    media.recvTransport = {
      close() {},
      async consume() {
        consumeCalls++;
        consumer = new Consumer({
          id: 'remote-consumer',
          localId: '0',
          producerId: 'remote-producer',
          track: newTrack('video'),
          rtpParameters: {},
        });
        if (stage === 'receiver') {
          started.resolve();
          await pending.promise;
        }
        return consumer;
      },
    };
    const consuming = media.consume('remote-producer');
    await started.promise;
    media.close();
    const requestCount = requests.length;
    pending.resolve();
    await assert.rejects(consuming, /Media session closed/);
    assert.equal(requests.length, requestCount, 'no consumer resume should be sent after close');
    assert.equal(consumeCalls, stage === 'response' ? 0 : 1);
    if (consumer) {
      assert.equal(consumer.closed, true);
      assert.equal(consumer.track.readyState, 'ended');
    }
    assert.equal(media.consumers.size, 0);
    assert.equal(media.producerToConsumer.size, 0);
  });
}

test('snapshot reconciliation stops missing local producers and preserves acknowledged capture', async (t) => {
  const { state, media } = await fixture(t);
  await media.unmuteAudio();
  await media.toggleVideo();
  const [audio, camera] = state.producers;
  assert.equal(media.reconcileLocalProducers([audio.id, camera.id]), false);
  assert.equal(media.reconcileLocalProducers([audio.id]), true);
  assert.equal(camera.closed, true);
  assert.equal(camera.track.readyState, 'ended');
  assert.equal(audio.closed, false);
  assert.equal(media.audioEnabled, true);
  assert.equal(media.videoEnabled, false);
  assert.equal(media.reconcileLocalProducers([audio.id]), false);
});
