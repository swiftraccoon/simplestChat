import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { evaluateTypeScript, loadTypeScript } from './source-loader.mjs';

const { Producer } = await import(new URL('./Producer.js', import.meta.resolve('mediasoup-client')));
const { Consumer } = await import(new URL('./Consumer.js', import.meta.resolve('mediasoup-client')));

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

class Track extends EventTarget {
  enabled = true;
  readyState = 'live';
  constructor(kind) { super(); this.kind = kind; }
  stop() { this.readyState = 'ended'; }
}

class Stream {
  constructor(tracks = []) { this.tracks = [...tracks]; }
  getTracks() { return [...this.tracks]; }
  getAudioTracks() { return this.tracks.filter(track => track.kind === 'audio'); }
  getVideoTracks() { return this.tracks.filter(track => track.kind === 'video'); }
  addTrack(track) { this.tracks.push(track); }
  removeTrack(track) { this.tracks = this.tracks.filter(item => item !== track); }
}

async function fixture(t) {
  const state = {
    tracks: [], producers: [], sent: [],
    capture: null, beforeProduce: null, replace: null,
  };
  const newTrack = kind => {
    const track = new Track(kind);
    state.tracks.push(track);
    return track;
  };
  const { MediaManager } = await loadTypeScript('src/media.ts', {
    modules: { 'mediasoup-client': {} },
    globals: {
      localStorage: { getItem() { return null; }, setItem() {} },
      MediaStream: Stream,
      navigator: { mediaDevices: {
        getUserMedia: constraints => state.capture
          ? state.capture(constraints)
          : Promise.resolve(new Stream([newTrack(constraints.audio ? 'audio' : 'video')])),
        getDisplayMedia: async () => new Stream([newTrack('video'), newTrack('audio')]),
      } },
      console: { log() {} },
    },
  });
  const media = new MediaManager({ send: message => state.sent.push(message) });
  media.sendTransport = {
    async produce(options) {
      await state.beforeProduce?.(options);
      // Use the installed library's real pause/track semantics, especially
      // construction with a disabled track and asynchronous replaceTrack.
      const producer = new Producer({
        id: `producer-${state.producers.length}`, localId: 'local',
        track: options.track, rtpParameters: {}, stopTracks: true,
        disableTrackOnPause: true, zeroRtpOnPause: false, appData: options.appData,
      });
      producer.on('@replacetrack', (track, resolve, reject) => {
        Promise.resolve().then(() => state.replace?.(track)).then(resolve, reject);
      });
      state.producers.push(producer);
      return producer;
    },
    close() {},
  };
  t.after(() => media.close());
  return { ...state, state, media, newTrack };
}

async function pttFor(media) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const functions = ast.statements.filter(node => ts.isFunctionDeclaration(node)
    && ['pttActivate', 'pttDeactivate'].includes(node.name?.text));
  const buttons = [], errors = [];
  const api = evaluateTypeScript(`
    let pttHeld = false;
    let pttActivation = 0;
    ${functions.map(node => node.getText(ast)).join('\n')}
    export { pttActivate, pttDeactivate };
    export function held() { return pttHeld; }
  `, { globals: {
    room: {
      hasMedia: true,
      get audioEnabled() { return media.audioEnabled; },
      unmuteAudio: () => media.unmuteAudio(),
      muteAudio: () => media.muteAudio(),
    },
    updateMicButton: enabled => buttons.push(enabled),
    updateLocalTile() {},
    showToast: error => errors.push(error),
  } });
  return { ...api, buttons, errors };
}

test('PTT release cancels a pending permission/capture result', async t => {
  const { state, media, newTrack } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(), capture = deferred();
  state.capture = () => { started.resolve(); return capture.promise; };
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

test('PTT release during produce stops capture immediately and closes the late producer', async t => {
  const { state, media } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(), produce = deferred();
  state.beforeProduce = options => { started.resolve(options.track); return produce.promise; };
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

test('PTT release during replaceTrack cannot resume the paused producer', async t => {
  const { state, media } = await fixture(t);
  await media.unmuteAudio();
  assert.equal(media.audioEnabled, true);
  media.muteAudio();
  const ptt = await pttFor(media);
  const started = deferred(), replacement = deferred();
  state.replace = track => { started.resolve(track); return replacement.promise; };
  const activation = ptt.pttActivate();
  const track = await started.promise;
  ptt.pttDeactivate();
  assert.equal(track.readyState, 'ended');
  replacement.resolve();
  await activation;
  assert.equal(media.audioEnabled, false);
  assert.equal(state.sent.some(message => message.type === 'resumeProducer'), false);
  state.replace = null;
  await ptt.pttActivate();
  assert.equal(media.audioEnabled, true);
  assert.equal(state.producers.length, 1);
});

test('a new PTT hold survives completion of an earlier cancelled activation', async t => {
  const { state, media } = await fixture(t);
  const ptt = await pttFor(media);
  const started = deferred(), produce = deferred();
  state.beforeProduce = options => { started.resolve(options.track); return produce.promise; };
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
  test(`PTT handles ${stage} errors, stops captured tracks, and permits retry`, async t => {
    const { state, media } = await fixture(t);
    if (stage === 'replace') {
      await media.unmuteAudio();
      media.muteAudio();
    }
    const ptt = await pttFor(media);
    const failure = async () => { throw new Error(`${stage} failed`); };
    state[stage === 'produce' ? 'beforeProduce' : stage] = failure;
    await ptt.pttActivate();
    assert.equal(ptt.held(), false);
    assert.equal(media.audioEnabled, false);
    assert.equal(state.tracks.every(track => track.readyState === 'ended'), true);
    assert.deepEqual(ptt.errors, [`${stage} failed`]);
    state.capture = state.beforeProduce = state.replace = null;
    await ptt.pttActivate();
    assert.equal(ptt.held(), true);
    assert.equal(media.audioEnabled, true);
  });
}

for (const source of ['microphone', 'camera', 'screen', 'screen-audio']) {
  test(`forced ${source} closure stops local capture and allows a fresh producer`, async t => {
    const { state, media } = await fixture(t);
    const start = () => source === 'microphone' ? media.unmuteAudio()
      : source === 'camera' ? media.toggleVideo() : media.startScreenShare();
    await start();
    const producer = state.producers.find(item => item.appData.source === source);
    const oldTracks = [...state.tracks];
    assert.equal(media.closeLocalProducer(producer.id), true);
    assert.equal(media.closeLocalProducer(producer.id), false);
    assert.equal(producer.closed, true);
    assert.equal(oldTracks.every(track => track.readyState === 'ended'), true);
    assert.equal(media.audioEnabled, false);
    assert.equal(media.videoEnabled, false);
    assert.equal(media.isScreenSharing, false);
    await start();
    assert.equal(state.producers.filter(item => item.appData.source === source).length, 2);
    assert.equal(source === 'microphone' ? media.audioEnabled
      : source === 'camera' ? media.videoEnabled : media.isScreenSharing, true);
    assert.equal(state.sent.some(message => message.type === 'resumeProducer'
      && message.producerId === producer.id), false);
  });
}

for (const stage of ['capture', 'replace']) {
  test(`forced camera closure during ${stage} discards the pending camera track`, async t => {
    const { state, media, newTrack } = await fixture(t);
    await media.toggleVideo();
    const producer = state.producers[0];
    await media.toggleVideo();
    const started = deferred(), pending = deferred();
    const track = newTrack('video');
    if (stage === 'capture') {
      state.capture = () => { started.resolve(); return pending.promise; };
    } else {
      state.capture = async () => new Stream([track]);
      state.replace = () => { started.resolve(); return pending.promise; };
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
  test(`forced closure during ${kind} device switching releases the new capture`, async t => {
    const { state, media, newTrack } = await fixture(t);
    if (kind === 'audio') await media.unmuteAudio();
    else await media.toggleVideo();
    const producer = state.producers[0];
    const started = deferred(), capture = deferred();
    state.capture = () => { started.resolve(); return capture.promise; };
    const switching = kind === 'audio' ? media.switchMic('new-device') : media.switchCamera('new-device');
    await started.promise;
    media.closeLocalProducer(producer.id);
    const track = newTrack(kind);
    capture.resolve(new Stream([track]));
    await switching;
    assert.equal(track.readyState, 'ended');
    assert.equal(kind === 'audio' ? media.audioEnabled : media.videoEnabled, false);
  });
}

test('PTT release cancels microphone capture during a device switch', async t => {
  const { state, media, newTrack } = await fixture(t);
  await media.unmuteAudio();
  const started = deferred(), capture = deferred();
  state.capture = () => { started.resolve(); return capture.promise; };
  const switching = media.switchMic('new-mic');
  await started.promise;
  media.muteAudio();
  const track = newTrack('audio');
  capture.resolve(new Stream([track]));
  await switching;
  assert.equal(track.readyState, 'ended');
  assert.equal(media.audioEnabled, false);
});

test('forced screen-video closure cannot resurrect a pending screen-audio producer', async t => {
  const { state, media } = await fixture(t);
  const started = deferred(), audio = deferred();
  state.beforeProduce = options => {
    if (options.appData.source === 'screen-audio') {
      started.resolve();
      return audio.promise;
    }
  };
  const sharing = media.startScreenShare();
  await started.promise;
  const video = state.producers[0];
  media.closeLocalProducer(video.id);
  assert.equal(state.tracks.every(track => track.readyState === 'ended'), true);
  audio.resolve();
  assert.equal(await sharing, null);
  assert.equal(state.producers.every(producer => producer.closed), true);
  assert.equal(media.isScreenSharing, false);
  state.beforeProduce = null;
  assert.ok(await media.startScreenShare());
  assert.equal(media.isScreenSharing, true);
});

test('leaving during microphone capture discards its late track', async t => {
  const { state, media, newTrack } = await fixture(t);
  const started = deferred(), capture = deferred();
  state.capture = () => { started.resolve(); return capture.promise; };
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

test('saved setup configures future capture without starting devices', async t => {
  const { state, media, newTrack } = await fixture(t);
  const captures = [];
  state.capture = async constraints => {
    captures.push(constraints);
    return new Stream([newTrack(constraints.audio ? 'audio' : 'video')]);
  };
  media.setCapturePreferences({
    cameraDeviceId: 'camera-two', microphoneDeviceId: 'mic-two', resolution: '1080p', frameRate: 15,
    echoCancellation: false, autoGainControl: false, noiseSuppression: true,
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

test('switching an inactive camera saves its selection without capturing it', async t => {
  const { state, media } = await fixture(t);
  let captures = 0;
  state.capture = async () => { captures++; throw new Error('Unexpected capture'); };
  await media.switchCamera('camera-two');
  assert.equal(captures, 0);
  assert.equal(media.capturePreferences.cameraDeviceId, 'camera-two');
});

for (const stage of ['capture', 'produce']) {
  test(`leaving during first camera ${stage} discards the late result`, async t => {
    const { state, media, newTrack } = await fixture(t);
    const started = deferred(), pending = deferred();
    let track;
    if (stage === 'capture') state.capture = () => { started.resolve(); return pending.promise; };
    else state.beforeProduce = async options => { track = options.track; started.resolve(); await pending.promise; };
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
    assert.ok(state.producers.every(producer => producer.closed));
  });
}

test('hide and restore pause only the selected viewer consumer', async t => {
  const { state, media, newTrack } = await fixture(t);
  const track = newTrack('video');
  const consumer = new Consumer({ id: 'consumer', localId: '0', producerId: 'remote', track, rtpParameters: {} });
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
  assert.deepEqual(state.sent.map(message => message.type), ['pauseConsumer', 'resumeConsumer']);
  assert.ok(state.sent.every(message => message.consumerId === consumer.id && !message.producerId));
});

test('quality preferences respect available simulcast layers and skip single-layer video', async t => {
  const { state, media, newTrack } = await fixture(t);
  for (const [id, mode] of [['camera', 'S3T3'], ['screen', 'L1T1']]) {
    const consumer = new Consumer({
      id, localId: id, producerId: id, track: newTrack('video'),
      rtpParameters: { encodings: [{ scalabilityMode: mode }] },
    });
    media.consumers.set(id, consumer);
    media.producerToConsumer.set(id, id);
  }
  for (const quality of ['low', 'medium', 'high', 'auto']) assert.equal(media.setConsumerQualityByProducer('camera', quality), true);
  assert.equal(media.setConsumerQualityByProducer('screen', 'high'), false);
  assert.equal(media.setConsumerQualityByProducer('missing', 'high'), false);
  assert.deepEqual(state.sent.map(message => message.spatialLayer), [0, 1, 2, 2]);
  assert.ok(state.sent.every(message => message.type === 'setConsumerPreferredLayers' && message.consumerId === 'camera'));
});

for (const stage of ['capabilities', 'device-load', 'send-transport', 'receive-transport']) {
  test(`closing media during ${stage} setup prevents later transport work`, async t => {
    const pending = deferred(), started = deferred();
    const requests = [], transports = [];
    const responseFor = message => message.type === 'getRouterRtpCapabilities'
      ? { rtpCapabilities: {} } : { transportId: message.type === 'createSendTransport' ? 'send' : 'receive' };
    const expectedType = stage === 'capabilities' ? 'getRouterRtpCapabilities'
      : stage === 'send-transport' ? 'createSendTransport' : 'createRecvTransport';
    class Device {
      async load() { if (stage === 'device-load') { started.resolve(); await pending.promise; } }
      createSendTransport() { return this.transport('send'); }
      createRecvTransport() { return this.transport('receive'); }
      transport(id) {
        const result = { id, closed: false, on() {}, close() { this.closed = true; } };
        transports.push(result);
        return result;
      }
    }
    const { MediaManager } = await loadTypeScript('src/media.ts', {
      modules: { 'mediasoup-client': { Device } },
      globals: { localStorage: { getItem() { return null; } }, console: { log() {} } },
    });
    const manager = new MediaManager({
      async request(message) {
        requests.push(message.type);
        if (stage !== 'device-load' && message.type === expectedType) { started.resolve(); await pending.promise; }
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
    assert.ok(transports.every(transport => transport.closed));
    assert.equal(manager.sendTransport, null);
    assert.equal(manager.recvTransport, null);
  });
}

for (const stage of ['response', 'receiver', 'resume']) {
  test(`closing media during consumer ${stage} discards the result and stops its track`, async t => {
    const { media, newTrack } = await fixture(t);
    const pending = deferred(), started = deferred();
    let consumer, consumeCalls = 0;
    const requests = [];
    media.device = { rtpCapabilities: {} };
    media.signaling.request = async message => {
      requests.push(message.type);
      if ((stage === 'response' && message.type === 'consume') || (stage === 'resume' && message.type === 'resumeConsumer')) {
        started.resolve(); await pending.promise;
      }
      return { consumerId: 'remote-consumer', producerId: 'remote-producer', kind: 'video', rtpParameters: {} };
    };
    media.recvTransport = {
      close() {},
      async consume() {
        consumeCalls++;
        consumer = new Consumer({ id: 'remote-consumer', localId: '0', producerId: 'remote-producer', track: newTrack('video'), rtpParameters: {} });
        if (stage === 'receiver') { started.resolve(); await pending.promise; }
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

test('snapshot reconciliation stops missing local producers and preserves acknowledged capture', async t => {
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
