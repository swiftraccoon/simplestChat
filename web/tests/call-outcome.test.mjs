import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function fixture(t, options = {}) {
  let now = 0;
  let wall = 1000;
  let next = 0;
  const events = [];
  const intervals = new Map();
  const listeners = new Map();
  const playback = [];
  const state = {
    settled: false,
    rosterKnown: false,
    expected: 0,
    selected: 0,
    unavailable: false,
  };
  const document = {
    visibilityState: 'visible',
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  class MediaStream {
    constructor(track) {
      this.track = track;
    }
    getTracks() {
      return [this.track];
    }
  }
  const { CallOutcomeTelemetry } = await loadTypeScript('src/media-telemetry.ts', {
    globals: {
      document,
      MediaStream,
      performance: { now: () => now },
      Date: { now: () => wall },
      setInterval: (fn) => {
        intervals.set(++next, fn);
        return next;
      },
      clearInterval: (id) => intervals.delete(id),
    },
  });
  const calls = new CallOutcomeTelemetry(
    options.state ?? (() => state),
    () => playback,
    (event) => {
      events.push(event);
      options.record?.(event);
    },
    () => ++next,
  );
  t.after(() => calls.dispose());
  const add = (kind = 'video') => {
    const frames = new Map();
    const track = { enabled: true, readyState: 'live' };
    const source = { key: {}, track, kind, active: () => true, getStats: async () => new Map() };
    const element = {
      isConnected: true,
      srcObject: new MediaStream(track),
      muted: false,
      paused: false,
      volume: 1,
      readyState: 2,
      requestVideoFrameCallback: (fn) => {
        frames.set(++next, fn);
        return next;
      },
      cancelVideoFrameCallback: (id) => frames.delete(id),
    };
    playback.push({ source, element });
    return {
      source,
      element,
      track,
      frames,
      present: () => {
        const callback = frames.values().next().value;
        frames.clear();
        callback?.();
      },
    };
  };
  return {
    calls,
    state,
    events,
    document,
    listeners,
    intervals,
    playback,
    add,
    advance: (ms) => {
      now += ms;
      wall += ms;
      calls.check();
    },
    advanceWall: (ms) => {
      wall += ms;
    },
    settle: (expected = 1) =>
      Object.assign(state, { settled: true, rosterKnown: true, expected, selected: expected }),
    terminal: () => events.filter((event) => event.outcome !== 'started'),
  };
}

test('empty/transient rosters cannot succeed before admission is settled', async (t) => {
  const f = await fixture(t);
  f.calls.signal({ type: 'start', kind: 'join' });
  f.advance(1000);
  f.state.rosterKnown = true;
  f.calls.check();
  assert.equal(f.terminal().length, 0);
  f.settle(0);
  f.calls.signal({ type: 'ready' });
  assert.deepEqual(
    f.terminal().map((event) => event.outcome),
    ['no_media_expected'],
  );
  assert.equal(f.events[0].attempt, f.events[1].attempt);
  f.calls.signal({ type: 'failed' });
  assert.equal(f.terminal().length, 1);
});

test('video readiness needs a current presented frame after room recovery, not live track presence', async (t) => {
  const f = await fixture(t);
  const video = f.add();
  f.calls.signal({ type: 'start', kind: 'reconnect' });
  video.present();
  assert.equal(f.terminal().length, 0);
  f.settle();
  f.advance(1200);
  video.present();
  assert.deepEqual(
    f.terminal().map(({ name, outcome, durationMs }) => ({ name, outcome, durationMs })),
    [{ name: 'call_reconnect', outcome: 'video_ready', durationMs: 1200 }],
  );
  f.advance(30000);
  assert.equal(f.terminal().length, 1);
});

test('audio-only readiness requires non-concealed sample progression and an unblocked playing element', async (t) => {
  const f = await fixture(t);
  const audio = f.add('audio');
  f.settle();
  audio.element.paused = true;
  f.calls.signal({ type: 'start', kind: 'join' });
  f.calls.playbackResult(audio.element, true);
  f.calls.sample(audio.source, { samples: 100, concealed: 20 });
  f.calls.sample(audio.source, { samples: 200, concealed: 120 });
  assert.equal(f.terminal().length, 0, 'concealed-only progress is not readiness');
  f.calls.sample(audio.source, { samples: 300, concealed: 120 });
  assert.equal(f.terminal().length, 0, 'decoding alone is not playback readiness');
  audio.element.paused = false;
  f.calls.playbackResult(audio.element, false);
  assert.equal(f.terminal()[0].outcome, 'audio_playback_ready');
  assert.equal(audio.frames.size, 0, 'audio needs no synthetic video callback');
});

for (const condition of ['missing', 'silent', 'hidden', 'unsupported', 'unsettled']) {
  test(`${condition} media stays unknown instead of claiming success or loss`, async (t) => {
    const f = await fixture(t);
    const media = f.add(condition === 'unsupported' ? 'video' : 'audio');
    if (condition !== 'unsettled') f.settle();
    if (condition === 'unsupported') delete media.element.requestVideoFrameCallback;
    f.calls.signal({ type: 'start', kind: 'join' });
    f.calls.sample(
      media.source,
      condition === 'missing' ? { samples: 100 } : { samples: 100, concealed: 10 },
    );
    f.calls.sample(
      media.source,
      condition === 'missing' ? { samples: 200 } : { samples: 100, concealed: 10 },
    );
    if (condition === 'hidden') {
      f.document.visibilityState = 'hidden';
      f.listeners.get('visibilitychange')();
    } else f.advance(30000);
    assert.equal(f.terminal()[0].outcome, 'unknown');
    assert.equal(f.calls.pending, false);
  });
}

for (const [condition, outcome] of [
  ['blocked', 'playback_blocked'],
  ['muted', 'media_disabled'],
  ['disabled', 'media_disabled'],
  ['hidden_track', 'media_disabled'],
  ['ended', 'timeout'],
  ['unavailable', 'unavailable'],
  ['no_frames', 'timeout'],
]) {
  test(`call deadline distinguishes ${condition}`, async (t) => {
    const f = await fixture(t);
    const media = f.add(condition === 'muted' ? 'audio' : 'video');
    f.settle();
    if (condition === 'muted') media.element.muted = true;
    if (condition === 'disabled') media.track.enabled = false;
    if (condition === 'ended') media.track.readyState = 'ended';
    if (condition === 'hidden_track') f.state.selected = 0;
    if (condition === 'unavailable') f.state.unavailable = true;
    f.calls.signal({ type: 'start', kind: 'join' });
    if (condition === 'blocked') {
      media.element.paused = true;
      f.calls.playbackResult(media.element, true);
    }
    f.advance(30000);
    assert.equal(f.terminal()[0].outcome, outcome);
    assert.equal(media.frames.size, 0);
  });
}

test('stale frame/sample callbacks cannot complete a replacement attempt or bypass the wall deadline', async (t) => {
  const f = await fixture(t);
  const old = f.add();
  f.settle();
  f.calls.signal({ type: 'start', kind: 'join' });
  const stale = old.frames.values().next().value;
  f.playback.length = 0;
  const current = f.add('audio');
  f.calls.signal({ type: 'start', kind: 'reconnect' });
  stale();
  f.calls.sample(old.source, { samples: 100, concealed: 0 });
  assert.deepEqual(
    f.terminal().map((event) => event.outcome),
    ['superseded'],
  );
  f.calls.sample(current.source, { samples: 100, concealed: 0 });
  f.advanceWall(30000);
  f.calls.sample(current.source, { samples: 200, concealed: 0 });
  assert.deepEqual(
    f.terminal().map((event) => event.outcome),
    ['superseded', 'unknown'],
  );
  assert.notEqual(f.terminal()[0].attempt, f.terminal()[1].attempt);
});

test('lost/disabled tracks erase audio progression; re-enabling needs new decoding evidence', async (t) => {
  const f = await fixture(t);
  const audio = f.add('audio');
  f.settle();
  audio.element.paused = true;
  f.calls.signal({ type: 'start', kind: 'join' });
  f.calls.sample(audio.source, { samples: 100, concealed: 0 });
  f.calls.sample(audio.source, { samples: 200, concealed: 0 });
  audio.track.enabled = false;
  f.calls.check();
  audio.track.enabled = true;
  audio.element.paused = false;
  f.calls.check();
  f.calls.sample(audio.source, { samples: 200, concealed: 0 });
  assert.equal(f.terminal().length, 0);
  f.calls.sample(audio.source, { samples: 300, concealed: 0 });
  assert.equal(f.terminal()[0].outcome, 'audio_playback_ready');
});

test('observation is bounded and reporting exceptions never duplicate or escape terminal outcomes', async (t) => {
  const f = await fixture(t, {
    record: () => {
      throw new Error('diagnostics unavailable');
    },
  });
  const videos = Array.from({ length: 70 }, () => f.add());
  f.settle(70);
  assert.doesNotThrow(() => f.calls.signal({ type: 'start', kind: 'join' }));
  assert.equal(
    videos.reduce((count, video) => count + video.frames.size, 0),
    64,
  );
  f.advance(30000);
  assert.equal(f.terminal()[0].outcome, 'unknown');
  assert.doesNotThrow(() => f.calls.dispose());
  assert.equal(f.terminal().length, 1);
  assert.equal(f.intervals.size, 0);
  assert.equal(f.listeners.size, 0);
  const broken = await fixture(t, {
    state: () => {
      throw new Error('stale room');
    },
  });
  assert.doesNotThrow(() => broken.calls.signal({ type: 'start', kind: 'join' }));
  assert.equal(broken.terminal()[0].outcome, 'unknown');
});

test('waiting admission ends join observation and creates an independently bounded admitted attempt', async (t) => {
  const f = await fixture(t);
  f.calls.signal({ type: 'start', kind: 'join' });
  f.calls.signal({ type: 'waiting' });
  f.advance(120000);
  f.calls.signal({ type: 'start', kind: 'admission' });
  f.settle(0);
  f.calls.signal({ type: 'ready' });
  assert.deepEqual(
    f.terminal().map(({ name, outcome }) => ({ name, outcome })),
    [
      { name: 'call_join', outcome: 'waiting' },
      { name: 'call_admission', outcome: 'no_media_expected' },
    ],
  );
});
