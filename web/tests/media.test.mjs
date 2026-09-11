import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadTypeScript } from './source-loader.mjs';

const { Producer } = await import(
  new URL('./Producer.js', import.meta.resolve('mediasoup-client'))
);
const { Consumer } = await import(
  new URL('./Consumer.js', import.meta.resolve('mediasoup-client'))
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
    beforeProduce: null,
    replace: null,
  };
  const newTrack = (kind) => {
    const track = new Track(kind);
    state.tracks.push(track);
    return track;
  };
  const { MediaManager } = await loadTypeScript('src/media.ts', {
    modules: { 'mediasoup-client': {} },
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
          getDisplayMedia: async () => new Stream([newTrack('video'), newTrack('audio')]),
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
  const media = new MediaManager({ send: (message) => state.sent.push(message) });
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
  return { ...state, state, media, newTrack };
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

for (const failure of ['throw', 'reject']) {
  test(`ICE restart handles a current transport ${failure} without exposing native error details`, async (t) => {
    const { state, media } = await fixture(t);
    const error = new Error('native-detail-must-not-be-logged');
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
    assert.deepEqual(state.sent, [], 'failure does not silently retry or recreate a transport');
    assert.deepEqual(state.captureCalls, []);
  });
}

for (const retirement of ['close', 'replace', 'transport-close']) {
  for (const result of ['resolve', 'reject']) {
    test(`ICE restart ignores a late ${result} after ${retirement}`, async (t) => {
      const { state, media } = await fixture(t);
      const restart = deferred();
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
  assert.equal(await sharing, null);
  assert.equal(
    state.producers.every((producer) => producer.closed),
    true,
  );
  assert.equal(media.isScreenSharing, false);
  state.beforeProduce = null;
  assert.ok(await media.startScreenShare());
  assert.equal(media.isScreenSharing, true);
});

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
  assert.deepEqual(
    state.sent.map((message) => message.type),
    ['pauseConsumer', 'resumeConsumer'],
  );
  assert.ok(
    state.sent.every((message) => message.consumerId === consumer.id && !message.producerId),
  );
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
  for (const quality of ['low', 'medium', 'high', 'auto'])
    assert.equal(media.setConsumerQualityByProducer('camera', quality), true);
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

for (const stage of ['capabilities', 'device-load', 'send-transport', 'receive-transport']) {
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
      modules: { 'mediasoup-client': { Device } },
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
        if (stage !== 'device-load' && message.type === expectedType) {
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
    media.device = { rtpCapabilities: {} };
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
