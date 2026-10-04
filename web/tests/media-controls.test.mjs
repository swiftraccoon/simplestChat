import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';

const signalingModule = await loadTypeScript('src/signaling.ts', {
  modules: await loadContractModules(),
});

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const preferences = {
  cameraDeviceId: '',
  microphoneDeviceId: '',
  resolution: '720p',
  frameRate: 30,
  echoCancellation: true,
  autoGainControl: true,
  noiseSuppression: true,
};

function stream() {
  const tracks = ['video', 'audio'].map((kind) => ({
    kind,
    readyState: 'live',
    stop() {
      this.readyState = 'ended';
    },
  }));
  return {
    getTracks: () => tracks,
    getAudioTracks: () => tracks.filter((track) => track.kind === 'audio'),
  };
}

async function fixture(t) {
  const stored = new Map();
  const storage = {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, value),
  };
  const state = {
    capture: () => Promise.resolve(stream()),
    streams: [],
    levels: [],
    contexts: [],
    frames: new Map(),
    captures: [],
    warnings: [],
  };
  const navigator = {
    mediaDevices: {
      getUserMedia: (constraints) => {
        state.captures.push(constraints);
        return state.capture(constraints);
      },
    },
  };
  const captureConsole = { warn: (...values) => state.warnings.push(values) };
  const media = await loadTypeScript('src/media.ts', {
    modules: {
      '../node_modules/mediasoup-client/lib/Device.js': {},
      './signaling': signalingModule,
    },
    globals: { localStorage: storage, navigator, console: captureConsole },
  });
  class AudioContext {
    state = 'running';
    closed = false;
    disconnected = false;
    constructor() {
      state.contexts.push(this);
    }
    createAnalyser() {
      return {
        fftSize: 512,
        getByteTimeDomainData(samples) {
          samples.fill(144);
        },
      };
    }
    createMediaStreamSource() {
      return {
        connect() {},
        disconnect: () => {
          this.disconnected = true;
        },
      };
    }
    async resume() {}
    async close() {
      this.closed = true;
    }
  }
  const api = await loadTypeScript('src/media-controls.ts', {
    modules: {
      './media': media,
      './media-controls.css': {},
      './settings-dialog': {},
      './settings-dialog.css': {},
      './audio-output': await loadTypeScript('src/audio-output.ts'),
    },
    globals: {
      localStorage: storage,
      navigator,
      console: captureConsole,
      window: { AudioContext },
      AudioContext,
      requestAnimationFrame: (callback) => {
        const id = state.frames.size + 1;
        state.frames.set(id, callback);
        return id;
      },
      cancelAnimationFrame: (id) => state.frames.delete(id),
    },
  });
  const preview = new api.MediaPreview(
    (value) => state.streams.push(value),
    (value) => state.levels.push(value),
  );
  t.after(() => preview.stop());
  return { ...api, ...media, state, preview, stored };
}

test('preview captures only on start and closes tracks, meter, and audio context on stop', async (t) => {
  const { preview, state } = await fixture(t);
  assert.equal(state.captures.length, 0);
  await preview.start(preferences);
  const captured = state.streams.at(-1);
  assert.ok(state.levels.at(-1) > 0);
  assert.equal(state.contexts.length, 1);
  assert.equal(state.frames.size, 1);
  preview.stop();
  assert.ok(captured.getTracks().every((track) => track.readyState === 'ended'));
  assert.equal(state.contexts[0].closed, true);
  assert.equal(state.contexts[0].disconnected, true);
  assert.equal(state.frames.size, 0);
  assert.equal(state.streams.at(-1), null);
  assert.equal(state.levels.at(-1), 0);
});

test('closing preview while permission is pending stops late tracks without a meter or stream attachment', async (t) => {
  const { preview, state } = await fixture(t);
  const pending = deferred();
  state.capture = () => pending.promise;
  const opening = preview.start(preferences);
  preview.stop();
  const captured = stream();
  pending.resolve(captured);
  await opening;
  assert.ok(captured.getTracks().every((track) => track.readyState === 'ended'));
  assert.ok(state.streams.every((value) => value === null));
  assert.equal(state.contexts.length, 0);
});

test('a retired permission prompt blocks overlapping capture until its late tracks are stopped', async (t) => {
  const { preview, state } = await fixture(t);
  const oldPermission = deferred();
  state.capture = () => oldPermission.promise;
  const older = preview.start(preferences, true, false);
  assert.equal(preview.pending, true);
  preview.stop();
  assert.equal(preview.pending, true, 'Stop retires the result, not the native permission prompt');
  assert.equal(await preview.start(preferences, false, true), false);
  assert.equal(state.captures.length, 1, 'another kind cannot open a second permission prompt');
  const stale = stream();
  oldPermission.resolve(stale);
  assert.equal(await older, false);
  assert.equal(preview.pending, false);
  assert.ok(stale.getTracks().every((track) => track.readyState === 'ended'));
  assert.ok(state.streams.every((value) => value === null));
  assert.equal(state.contexts.length, 0);
  const current = stream();
  state.capture = () => Promise.resolve(current);
  assert.equal(await preview.start(preferences, false, true), true);
  assert.equal(state.captures.length, 2);
  assert.equal(state.streams.at(-1), current);
  assert.ok(current.getTracks().every((track) => track.readyState === 'live'));
  assert.equal(state.contexts.length, 1);
});

test('repeated preview start while permission is pending does not retire or duplicate the active request', async (t) => {
  const { preview, state } = await fixture(t);
  const permission = deferred();
  state.capture = () => permission.promise;
  const first = preview.start(preferences, true, false);
  assert.equal(await preview.start(preferences, true, false), false);
  assert.equal(state.captures.length, 1);
  const captured = stream();
  permission.resolve(captured);
  assert.equal(await first, true);
  assert.equal(preview.pending, false);
  assert.equal(state.streams.at(-1), captured);
  assert.ok(captured.getTracks().every((track) => track.readyState === 'live'));
});

for (const kind of ['camera', 'microphone']) {
  test(`testing the selected ${kind} requests only that kind and retains its exact selection`, async (t) => {
    const { preview, state, stored } = await fixture(t);
    const selected = {
      ...preferences,
      cameraDeviceId: 'selected-camera',
      microphoneDeviceId: 'selected-mic',
    };
    await preview.start(selected, kind === 'camera', kind === 'microphone');
    const constraints = state.captures[0];
    if (kind === 'camera') {
      assert.equal(constraints.audio, false);
      assert.equal(constraints.video.deviceId.exact, selected.cameraDeviceId);
    } else {
      assert.equal(constraints.video, false);
      assert.equal(constraints.audio.deviceId.exact, selected.microphoneDeviceId);
    }
    assert.equal(stored.size, 0, 'testing never saves settings');
  });
}

test('preview camera fallback preserves camera selection and all microphone constraints and cleans up', async (t) => {
  const { preview, state, stored } = await fixture(t);
  const selected = {
    ...preferences,
    cameraDeviceId: 'selected-camera',
    microphoneDeviceId: 'selected-mic',
    echoCancellation: false,
    autoGainControl: false,
  };
  const captured = stream();
  state.capture = async () => {
    if (state.captures.length === 1) {
      throw new DOMException('Starting videoinput failed', 'NotReadableError');
    }
    return captured;
  };
  assert.equal(await preview.start(selected), true);
  assert.equal(state.captures.length, 2);
  assert.deepEqual(state.captures[1].video, { deviceId: { exact: 'selected-camera' } });
  assert.deepEqual(state.captures[1].audio, state.captures[0].audio);
  assert.equal(state.captures[1].audio.deviceId.exact, 'selected-mic');
  assert.equal(state.captures[1].audio.echoCancellation, false);
  assert.equal(state.captures[1].audio.autoGainControl, false);
  assert.equal(state.captures[1].audio.noiseSuppression, true);
  assert.equal(state.streams.at(-1), captured);
  assert.equal(state.warnings.length, 1);
  assert.equal(stored.size, 0);
  preview.stop();
  assert.ok(captured.getTracks().every((track) => track.readyState === 'ended'));
  assert.equal(state.contexts[0].closed, true);
  assert.equal(state.frames.size, 0);
});

test('microphone-only NotReadableError is not retried by camera fallback', async (t) => {
  const { preview, state } = await fixture(t);
  const failure = new DOMException('Starting audioinput failed', 'NotReadableError');
  state.capture = async () => {
    throw failure;
  };
  await assert.rejects(preview.start(preferences, false, true), (error) => error === failure);
  assert.equal(state.captures.length, 1);
  assert.equal(state.captures[0].video, false);
  assert.equal(preview.pending, false);
  assert.equal(state.contexts.length, 0);
});

test('preview cancellation suppresses retry or disposes a late fallback stream without attachment', async (t) => {
  for (const stage of ['before retry', 'during retry']) {
    await t.test(stage, async (t) => {
      const { preview, state } = await fixture(t);
      const pending = deferred();
      const fallbackStarted = deferred();
      state.capture = async () => {
        if (stage === 'during retry' && state.captures.length === 1) {
          throw new DOMException('Starting videoinput failed', 'NotReadableError');
        }
        fallbackStarted.resolve();
        return pending.promise;
      };
      const opening = preview.start(preferences, true, false);
      await fallbackStarted.promise;
      preview.stop();
      assert.equal(await preview.start(preferences, false, true), false);
      const captured = stream();
      if (stage === 'before retry') {
        pending.reject(new DOMException('Starting videoinput failed', 'NotReadableError'));
      } else {
        assert.deepEqual(state.captures[1], { video: true, audio: false });
        pending.resolve(captured);
      }
      assert.equal(await opening, false);
      assert.equal(state.captures.length, stage === 'before retry' ? 1 : 2);
      assert.equal(preview.pending, false);
      assert.ok(state.streams.every((value) => value === null));
      assert.equal(state.contexts.length, 0);
      if (stage === 'during retry') {
        assert.ok(captured.getTracks().every((track) => track.readyState === 'ended'));
      }
    });
  }
});

test('preview capture errors clear resources and allow a microphone-only retry', async (t) => {
  const { preview, state } = await fixture(t);
  state.capture = async () => {
    throw new Error('Permission denied');
  };
  await assert.rejects(preview.start(preferences), /Permission denied/);
  assert.equal(preview.pending, false);
  assert.equal(state.streams.at(-1), null);
  state.capture = async () => stream();
  await preview.start(preferences, false, true);
  assert.equal(state.captures.at(-1).video, false);
  assert.equal(state.captures.at(-1).audio.echoCancellation, true);
});

test('late permission denial after cancellation clears the busy state without changing a retired preview', async (t) => {
  const { preview, state } = await fixture(t);
  const permission = deferred();
  state.capture = () => permission.promise;
  const starting = preview.start(preferences, false, true);
  preview.stop();
  permission.reject(new DOMException('Owned denial', 'NotAllowedError'));
  assert.equal(await starting, false);
  assert.equal(preview.pending, false);
  assert.ok(state.streams.every((value) => value === null));
});

test('preferences survive reload and normalize damaged local storage without capture', async (t) => {
  const { state, stored, loadCapturePreferences, saveCapturePreferences } = await fixture(t);
  saveCapturePreferences({
    ...preferences,
    microphoneDeviceId: 'selected-mic',
    resolution: '360p',
    frameRate: 15,
  });
  assert.equal(loadCapturePreferences().microphoneDeviceId, 'selected-mic');
  assert.equal(loadCapturePreferences().resolution, '360p');
  stored.set('simplestchat.capturePreferences', '{broken');
  assert.equal(loadCapturePreferences().resolution, '720p');
  stored.set(
    'simplestchat.capturePreferences',
    '{"frameRate":900,"resolution":"8k","cameraDeviceId":{}}',
  );
  const loaded = loadCapturePreferences();
  assert.equal(loaded.frameRate, 30);
  assert.equal(loaded.cameraDeviceId, '');
  assert.equal(state.captures.length, 0);
});

test('master and personal volume multiply while hide/restore preserves local mute', async (t) => {
  const { applyPersonalPlayback } = await fixture(t);
  const element = {
    volume: 1,
    muted: false,
    paused: false,
    srcObject: {},
    pause() {
      this.paused = true;
    },
    async play() {
      this.paused = false;
    },
  };
  applyPersonalPlayback([element], { volume: 0.6, muted: false, hidden: false }, 0.5);
  assert.equal(element.volume, 0.3);
  applyPersonalPlayback([element], { volume: 0.6, muted: true, hidden: true }, 0.5);
  assert.equal(element.muted, true);
  assert.equal(element.paused, true);
  applyPersonalPlayback([element], { volume: 0.6, muted: true, hidden: false }, 0.5);
  assert.equal(element.muted, true);
  assert.equal(element.paused, false);
  applyPersonalPlayback([element], { volume: 0.6, muted: false, hidden: false }, 0.5);
  assert.equal(element.muted, false);
  assert.equal(element.volume, 0.3);
});

function settingsRoom(audioEnabled = false, videoEnabled = false) {
  const actions = [];
  return {
    actions,
    audioEnabled,
    videoEnabled,
    setCapturePreferences(value) {
      actions.push(['save', value]);
    },
    async switchCamera(id) {
      actions.push(['camera', id]);
    },
    async switchMic(id) {
      actions.push(['microphone', id]);
    },
  };
}

test('saving personal settings never enables inactive capture', async (t) => {
  const { applyCaptureSettings, loadCapturePreferences } = await fixture(t);
  const room = settingsRoom();
  const next = { ...preferences, cameraDeviceId: 'camera-2', microphoneDeviceId: 'mic-2' };
  assert.equal(await applyCaptureSettings(room, next, preferences, () => true), true);
  assert.deepEqual(room.actions, [['save', next]]);
  assert.deepEqual(loadCapturePreferences(), next);
});

test('saving only replaces active capture kinds whose preferences changed', async (t) => {
  const { applyCaptureSettings } = await fixture(t);
  const room = settingsRoom(true, true);
  await applyCaptureSettings(room, preferences, preferences, () => true);
  assert.deepEqual(room.actions, [['save', preferences]]);
  room.actions.length = 0;
  const next = { ...preferences, frameRate: 15 };
  await applyCaptureSettings(room, next, preferences, () => true);
  assert.deepEqual(room.actions, [
    ['save', next],
    ['camera', ''],
  ]);
  room.actions.length = 0;
  const audio = { ...preferences, noiseSuppression: false };
  await applyCaptureSettings(room, audio, preferences, () => true);
  assert.deepEqual(room.actions, [
    ['save', audio],
    ['microphone', ''],
  ]);
});

test('saving default devices switches active capture back to the defaults', async (t) => {
  const { applyCaptureSettings } = await fixture(t);
  const room = settingsRoom(true, true);
  await applyCaptureSettings(
    room,
    preferences,
    { ...preferences, cameraDeviceId: 'old', microphoneDeviceId: 'old' },
    () => true,
  );
  assert.deepEqual(room.actions, [
    ['save', preferences],
    ['camera', ''],
    ['microphone', ''],
  ]);
});

test('leaving or closing settings during a camera switch prevents a subsequent microphone switch', async (t) => {
  const { applyCaptureSettings } = await fixture(t);
  const room = settingsRoom(true, true);
  const pending = deferred();
  room.switchCamera = () => pending.promise;
  let current = true;
  const next = { ...preferences, cameraDeviceId: 'new', microphoneDeviceId: 'new' };
  const saving = applyCaptureSettings(room, next, preferences, () => current);
  current = false;
  pending.resolve();
  assert.equal(await saving, false);
  assert.deepEqual(room.actions, [['save', next]]);
  room.actions.length = 0;
  assert.equal(await applyCaptureSettings(room, next, preferences, () => false), false);
  assert.deepEqual(room.actions, []);
});

test('a device-switch failure is reported without switching the other capture kind', async (t) => {
  const { applyCaptureSettings, loadCapturePreferences } = await fixture(t);
  const room = settingsRoom(true, true);
  room.switchCamera = async () => {
    throw new Error('Device unavailable');
  };
  const next = { ...preferences, cameraDeviceId: 'new', microphoneDeviceId: 'new' };
  await assert.rejects(
    applyCaptureSettings(room, next, preferences, () => true),
    /Device unavailable/,
  );
  assert.deepEqual(room.actions, [
    ['save', next],
    ['save', preferences],
  ]);
  assert.deepEqual(
    loadCapturePreferences(),
    preferences,
    'failed changes are not the saved baseline on reopen',
  );
  room.switchCamera = async (id) => {
    room.actions.push(['camera', id]);
  };
  room.actions.length = 0;
  await applyCaptureSettings(room, next, loadCapturePreferences(), () => true);
  assert.deepEqual(room.actions, [
    ['save', next],
    ['camera', 'new'],
    ['microphone', 'new'],
  ]);
});

test('a failed microphone switch retains a successful camera change when settings reopen', async (t) => {
  const { applyCaptureSettings, loadCapturePreferences } = await fixture(t);
  const room = settingsRoom(true, true);
  room.switchMic = async () => {
    throw new Error('Microphone unavailable');
  };
  const next = { ...preferences, cameraDeviceId: 'new-camera', microphoneDeviceId: 'new-mic' };
  await assert.rejects(
    applyCaptureSettings(room, next, preferences, () => true),
    /Microphone unavailable/,
  );
  const saved = loadCapturePreferences();
  assert.equal(saved.cameraDeviceId, 'new-camera');
  assert.equal(saved.microphoneDeviceId, '');
  room.switchMic = async (id) => {
    room.actions.push(['microphone', id]);
  };
  room.actions.length = 0;
  await applyCaptureSettings(room, next, saved, () => true);
  assert.deepEqual(room.actions, [
    ['save', next],
    ['microphone', 'new-mic'],
  ]);
});

test('after partial failure the confirmed baseline allows reverting the camera while retrying the microphone', async (t) => {
  const { applyCaptureSettings, loadCapturePreferences } = await fixture(t);
  const room = settingsRoom(true, true);
  room.switchMic = async () => {
    throw new Error('Microphone unavailable');
  };
  const next = { ...preferences, cameraDeviceId: 'new-camera', microphoneDeviceId: 'new-mic' };
  await assert.rejects(
    applyCaptureSettings(room, next, preferences, () => true),
    /Microphone unavailable/,
  );
  room.switchMic = async (id) => {
    room.actions.push(['microphone', id]);
  };
  room.actions.length = 0;
  const revertedCamera = { ...next, cameraDeviceId: '' };
  await applyCaptureSettings(room, revertedCamera, loadCapturePreferences(), () => true);
  assert.deepEqual(room.actions, [
    ['save', revertedCamera],
    ['camera', ''],
    ['microphone', 'new-mic'],
  ]);
});
