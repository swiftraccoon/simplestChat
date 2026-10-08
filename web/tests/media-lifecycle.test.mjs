import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function fixture() {
  const document = new EventTarget();
  document.visibilityState = 'visible';
  const window = new EventTarget();
  const mediaDevices = new EventTarget();
  const timers = new Map();
  let sequence = 0;
  const calls = [];
  const room = {
    membershipVersion: 1,
    connected: true,
    setMediaPageActive: (active) => calls.push(['active', active]),
    resumeMediaConnection: () => calls.push(['connection']),
  };
  const state = { room };
  const { MediaLifecycle } = await loadTypeScript('src/media-lifecycle.ts', {
    globals: {
      document,
      window,
      navigator: { mediaDevices },
      setTimeout: (callback, delay) => {
        timers.set(++sequence, { callback, delay });
        return sequence;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  const lifecycle = new MediaLifecycle({
    getRoom: () => state.room,
    setPageActive: (active) => calls.push(['previewActive', active]),
    resumePlayback: () => calls.push(['playback']),
    refreshDevices: () => calls.push(['devices']),
    resumeSignaling: () => calls.push(['signaling']),
  });
  const fire = (target, event) => target.dispatchEvent(new Event(event));
  const tick = () => {
    const pending = [...timers.values()];
    timers.clear();
    for (const { callback } of pending) callback();
  };
  return { document, window, mediaDevices, timers, calls, state, room, lifecycle, fire, tick };
}

test('hidden/pagehide suspends automatic work; visible/pageshow/online/device bursts coalesce', async () => {
  const f = await fixture();
  f.document.visibilityState = 'hidden';
  f.fire(f.document, 'visibilitychange');
  f.fire(f.window, 'pagehide');
  for (let count = 0; count < 30; count++) {
    f.fire(f.window, 'online');
    f.fire(f.mediaDevices, 'devicechange');
  }
  assert.equal(f.timers.size, 0);
  assert.ok(f.calls.every(([kind, active]) => /[Aa]ctive/.test(kind) && !active));
  f.calls.length = 0;
  f.document.visibilityState = 'visible';
  f.fire(f.window, 'pageshow');
  f.fire(f.document, 'visibilitychange');
  for (let count = 0; count < 30; count++) f.fire(f.window, 'online');
  assert.equal(f.timers.size, 1);
  f.tick();
  for (const kind of ['devices', 'connection', 'playback', 'signaling'])
    assert.equal(f.calls.filter(([entry]) => entry === kind).length, 1, kind);
  f.fire(f.window, 'online');
  assert.ok([...f.timers.values()][0].delay > 250, 'repeated hints are rate limited');
  f.lifecycle.dispose();
});

test('queued hints cannot repair a left, replaced, or fully rejoined room', async () => {
  for (const change of ['leave', 'replace', 'membership']) {
    const f = await fixture();
    f.fire(f.window, 'online');
    if (change === 'leave') f.state.room = null;
    else if (change === 'replace') f.state.room = { ...f.room };
    else f.room.membershipVersion++;
    f.tick();
    assert.deepEqual(f.calls, [], change);
    f.lifecycle.dispose();
  }
});

test('a recovering socket retains playback but cannot restart media until room ownership returns', async () => {
  const f = await fixture();
  f.room.connected = false;
  f.fire(f.window, 'online');
  f.tick();
  assert.ok(f.calls.some(([kind]) => kind === 'playback'));
  assert.ok(!f.calls.some(([kind]) => kind === 'connection'));
  f.lifecycle.dispose();
});

test('hiding cancels delayed work; disposal removes every listener and pending timer', async () => {
  const f = await fixture();
  f.fire(f.window, 'online');
  f.fire(f.window, 'pagehide');
  assert.equal(f.timers.size, 0);
  f.lifecycle.dispose();
  f.calls.length = 0;
  for (const event of ['online', 'pagehide', 'pageshow']) f.fire(f.window, event);
  f.fire(f.document, 'visibilitychange');
  f.fire(f.mediaDevices, 'devicechange');
  f.tick();
  assert.deepEqual(f.calls, []);
  assert.equal(f.timers.size, 0);
});

test('lobby device refresh is allowed without any room or capture recovery', async () => {
  const f = await fixture();
  f.state.room = null;
  f.fire(f.mediaDevices, 'devicechange');
  f.tick();
  assert.deepEqual(f.calls, [['signaling'], ['devices']]);
  f.lifecycle.dispose();
});
