import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const preferences = {
  cameraDeviceId: '', microphoneDeviceId: '', resolution: '720p', frameRate: 30,
  echoCancellation: true, autoGainControl: true, noiseSuppression: true,
};

function stream() {
  const tracks = ['video', 'audio'].map(kind => ({ kind, readyState: 'live', stop() { this.readyState = 'ended'; } }));
  return { getTracks: () => tracks, getAudioTracks: () => tracks.filter(track => track.kind === 'audio') };
}

async function fixture(t) {
  const stored = new Map();
  const storage = { getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  const media = await loadTypeScript('src/media.ts', { modules: { 'mediasoup-client': {} }, globals: { localStorage: storage } });
  const state = { capture: () => Promise.resolve(stream()), streams: [], levels: [], contexts: [], frames: new Map(), captures: [] };
  class AudioContext {
    state = 'running';
    closed = false;
    disconnected = false;
    constructor() { state.contexts.push(this); }
    createAnalyser() { return { fftSize: 512, getByteTimeDomainData(samples) { samples.fill(144); } }; }
    createMediaStreamSource() {
      return { connect() {}, disconnect: () => { this.disconnected = true; } };
    }
    async resume() {}
    async close() { this.closed = true; }
  }
  const api = await loadTypeScript('src/media-controls.ts', {
    modules: { './media': media, './media-controls.css': {} },
    globals: {
      localStorage: storage,
      navigator: { mediaDevices: { getUserMedia: constraints => { state.captures.push(constraints); return state.capture(constraints); } } },
      window: { AudioContext }, AudioContext,
      requestAnimationFrame: callback => { const id = state.frames.size + 1; state.frames.set(id, callback); return id; },
      cancelAnimationFrame: id => state.frames.delete(id),
    },
  });
  const preview = new api.MediaPreview(value => state.streams.push(value), value => state.levels.push(value));
  t.after(() => preview.stop());
  return { ...api, ...media, state, preview, stored };
}

test('preview captures only on start and closes tracks, meter, and audio context on stop', async t => {
  const { preview, state } = await fixture(t);
  assert.equal(state.captures.length, 0);
  await preview.start(preferences);
  const captured = state.streams.at(-1);
  assert.ok(state.levels.at(-1) > 0);
  assert.equal(state.contexts.length, 1);
  assert.equal(state.frames.size, 1);
  preview.stop();
  assert.ok(captured.getTracks().every(track => track.readyState === 'ended'));
  assert.equal(state.contexts[0].closed, true);
  assert.equal(state.contexts[0].disconnected, true);
  assert.equal(state.frames.size, 0);
  assert.equal(state.streams.at(-1), null);
  assert.equal(state.levels.at(-1), 0);
});

test('closing preview while permission is pending stops late tracks without a meter or stream attachment', async t => {
  const { preview, state } = await fixture(t);
  const pending = deferred();
  state.capture = () => pending.promise;
  const opening = preview.start(preferences);
  preview.stop();
  const captured = stream();
  pending.resolve(captured);
  await opening;
  assert.ok(captured.getTracks().every(track => track.readyState === 'ended'));
  assert.ok(state.streams.every(value => value === null));
  assert.equal(state.contexts.length, 0);
});

test('an older preview permission result cannot replace a newer preview', async t => {
  const { preview, state } = await fixture(t);
  const oldPermission = deferred();
  state.capture = () => oldPermission.promise;
  const older = preview.start(preferences);
  const current = stream();
  state.capture = () => Promise.resolve(current);
  await preview.start(preferences);
  const stale = stream();
  oldPermission.resolve(stale);
  await older;
  assert.equal(state.streams.at(-1), current);
  assert.ok(current.getTracks().every(track => track.readyState === 'live'));
  assert.ok(stale.getTracks().every(track => track.readyState === 'ended'));
  assert.equal(state.contexts.length, 1);
});

test('preview capture errors clear resources and allow a microphone-only retry', async t => {
  const { preview, state } = await fixture(t);
  state.capture = async () => { throw new Error('Permission denied'); };
  await assert.rejects(preview.start(preferences), /Permission denied/);
  assert.equal(state.streams.at(-1), null);
  state.capture = async () => stream();
  await preview.start(preferences, false, true);
  assert.equal(state.captures.at(-1).video, false);
  assert.equal(state.captures.at(-1).audio.echoCancellation, true);
});

test('preferences survive reload and normalize damaged local storage without capture', async t => {
  const { state, stored, loadCapturePreferences, saveCapturePreferences } = await fixture(t);
  saveCapturePreferences({ ...preferences, microphoneDeviceId: 'selected-mic', resolution: '360p', frameRate: 15 });
  assert.equal(loadCapturePreferences().microphoneDeviceId, 'selected-mic');
  assert.equal(loadCapturePreferences().resolution, '360p');
  stored.set('simplestchat.capturePreferences', '{broken');
  assert.equal(loadCapturePreferences().resolution, '720p');
  stored.set('simplestchat.capturePreferences', '{"frameRate":900,"resolution":"8k","cameraDeviceId":{}}');
  const loaded = loadCapturePreferences();
  assert.equal(loaded.frameRate, 30);
  assert.equal(loaded.cameraDeviceId, '');
  assert.equal(state.captures.length, 0);
});

test('master and personal volume multiply while hide/restore preserves local mute', async t => {
  const { applyPersonalPlayback } = await fixture(t);
  const element = {
    volume: 1, muted: false, paused: false, srcObject: {},
    pause() { this.paused = true; },
    async play() { this.paused = false; },
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
