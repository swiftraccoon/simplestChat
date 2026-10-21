import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { deferred, flush } from './ui-fixture.mjs';

const policy = await loadTypeScript('src/receive-policy.ts');
const candidates = Array.from({ length: 20 }, (_, index) => ({
  id: String(index),
  visible: true,
  hidden: false,
  pinned: false,
}));

test('receive budgets bound video, prefer pins, and keep existing ties stable', () => {
  const current = new Set(['9', '10', '11', '12']);
  const next = candidates.map((candidate) => ({ ...candidate, pinned: candidate.id === '19' }));
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'balanced', true, current)],
    ['19', '9', '10', '11', '12', '0', '1', '2', '3'],
  );
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'data-saver', true, current)],
    ['19', '9', '10', '11'],
  );
  assert.equal(policy.selectIncomingVideos(next, 'audio-only', true, current).size, 0);
  assert.equal(policy.selectIncomingVideos(next, 'balanced', false, current).size, 0);
});

test('offscreen video receives only when pinned and explicit hiding always wins', () => {
  const next = candidates.map((candidate) => ({
    ...candidate,
    visible: false,
    pinned: candidate.id === '19',
  }));
  assert.deepEqual([...policy.selectIncomingVideos(next, 'balanced', true, new Set())], ['19']);
  next[19].hidden = true;
  assert.equal(policy.selectIncomingVideos(next, 'balanced', true, new Set()).size, 0);
});

test('an explicit picture-in-picture window stays visible while its page is hidden', () => {
  const next = candidates.map((candidate) => ({
    ...candidate,
    visible: false,
    pictureInPicture: candidate.id === '19',
  }));
  assert.deepEqual([...policy.selectIncomingVideos(next, 'balanced', false, new Set())], ['19']);
  assert.equal(policy.selectIncomingVideos(next, 'audio-only', false, new Set()).size, 0);
  next[19].hidden = true;
  assert.equal(policy.selectIncomingVideos(next, 'balanced', false, new Set()).size, 0);
});

test('observed visible tiles replace unobserved ones before retaining current ties', () => {
  const next = candidates.map((candidate) => ({
    ...candidate,
    visible: candidate.id === '19' ? true : undefined,
  }));
  const selected = policy.selectIncomingVideos(
    next,
    'data-saver',
    true,
    new Set(['0', '1', '2', '3']),
  );
  assert.deepEqual([...selected], ['19', '0', '1', '2']);
});

test('visible screen shares replace unpinned cameras within both video budgets', () => {
  const next = candidates.map((candidate) => ({
    ...candidate,
    source: candidate.id === '19' ? 'screen' : 'camera',
  }));
  const current = new Set(['0', '1', '2', '3', '4', '5', '6', '7', '8']);
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'balanced', true, current)],
    ['19', '0', '1', '2', '3', '4', '5', '6', '7'],
  );
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'data-saver', true, current)],
    ['19', '0', '1', '2'],
  );
});

test('picture-in-picture and explicit pins outrank screen shares, whose ties stay stable', () => {
  const next = [
    { id: 'first-share', source: 'screen', visible: true, pinned: false, hidden: false },
    { id: 'retained-share', source: 'screen', visible: true, pinned: false, hidden: false },
    { id: 'third-share', source: 'screen', visible: true, pinned: false, hidden: false },
    { id: 'pinned-camera', source: 'camera', visible: false, pinned: true, hidden: false },
    {
      id: 'pip-camera',
      source: 'camera',
      visible: false,
      pinned: false,
      hidden: false,
      pictureInPicture: true,
    },
  ];
  const current = new Set(['retained-share', 'third-share']);
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'data-saver', true, current)],
    ['pip-camera', 'pinned-camera', 'retained-share', 'third-share'],
  );
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'balanced', false, current)],
    ['pip-camera'],
  );
  assert.equal(policy.selectIncomingVideos(next, 'audio-only', true, current).size, 0);
});

test('screen-share priority keeps visibility and hiding rules', () => {
  const next = [
    { id: 'offscreen-share', source: 'screen', visible: false, pinned: false, hidden: false },
    { id: 'hidden-share', source: 'screen', visible: true, pinned: true, hidden: true },
    { id: 'new-share', source: 'screen', pinned: false, hidden: false },
    ...candidates.slice(0, 4),
  ];
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'data-saver', true, new Set(['new-share']))],
    ['0', '1', '2', '3'],
    'an unobserved share must not displace a confirmed-visible camera',
  );
  next[2].visible = true;
  assert.deepEqual(
    [...policy.selectIncomingVideos(next, 'data-saver', true, new Set())],
    ['new-share', '0', '1', '2'],
  );
});

test('stored receive mode is bounded and storage failure does not prevent joining', async () => {
  assert.equal(policy.loadReceiveMode(), 'balanced');
  assert.equal(policy.normalizeReceiveMode('unlimited'), 'balanced');
  let value = 'audio-only';
  const stored = await loadTypeScript('src/receive-policy.ts', {
    globals: {
      localStorage: {
        getItem: () => value,
        setItem: (_key, next) => {
          value = next;
        },
      },
    },
  });
  assert.equal(stored.loadReceiveMode(), 'audio-only');
  stored.saveReceiveMode('data-saver');
  assert.equal(stored.loadReceiveMode(), 'data-saver');
});

async function fixture(t, count = 12, includeScreen = true, options = {}) {
  const state = {
    media: null,
    tracks: [],
    deferred: [],
    errors: [],
    consumed: [],
    retired: [],
    maximum: 0,
    followed: [],
  };
  class Media {
    tracks = new Map();
    caps = new Map();
    hidden = new Map();
    qualities = new Map();
    constructor() {
      state.media = this;
    }
    async setup() {}
    async consume(id) {
      state.consumed.push(id);
      await state.beforeConsume?.(id);
      const track = { id };
      this.tracks.set(id, track);
      state.maximum = Math.max(
        state.maximum,
        [...this.tracks.keys()].filter((key) => key.startsWith('v')).length,
      );
      return track;
    }
    getConsumerTrackByProducer(id) {
      return this.tracks.get(id);
    }
    async retireConsumerByProducer(id) {
      if (!this.tracks.has(id)) return;
      state.retired.push(id);
      // Simulate the real resource remaining server-owned until its ACK.
      await state.beforeRetire?.(id);
      this.tracks.delete(id);
    }
    closeConsumerByProducer(id) {
      this.tracks.delete(id);
    }
    closeLocalProducer() {
      return false;
    }
    setConsumerSizeCapByProducer(id, cap) {
      this.caps.set(id, cap);
    }
    setConsumerQualityByProducer(id, quality) {
      this.qualities.set(id, quality);
    }
    setConsumerHiddenByProducer(id, hidden) {
      this.hidden.set(id, hidden);
    }
    setPageActive() {}
    suspendSignaling() {}
    close() {
      this.tracks.clear();
    }
    toggleAudio() {
      assert.fail('receive policy must never capture');
    }
    toggleVideo() {
      assert.fail('receive policy must never capture');
    }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: {
      './receive-policy': options.policy ?? policy,
      './media': { MediaManager: Media },
    },
  });
  const signaling = {
    connected: true,
    setOnMessage(handler) {
      this.onMessage = handler;
    },
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost(handler) {
      this.onConnectionLost = handler;
    },
    completeRestartRecovery() {},
    send(message) {
      if (message.type === 'joinRoom')
        queueMicrotask(() =>
          this.onMessage({
            type: 'roomJoined',
            participantId: 'local',
            reconnectToken: 'token',
            yourRole: 'user',
            participants: Array.from({ length: count }, (_, index) => ({
              id: `p${index}`,
              name: `Peer ${index}`,
              role: 'user',
              producers: [
                {
                  id: `v${index}`,
                  kind: 'video',
                  source: includeScreen && index === count - 1 ? 'screen' : 'camera',
                },
                { id: `a${index}`, kind: 'audio' },
              ],
            })),
          }),
        );
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onLocalMediaChanged() {},
    onRemoteTrackRemoved() {},
    onRemoteTrack: (...args) => state.tracks.push(args),
    onRemoteVideoDeferred: (...args) => state.deferred.push(args),
    onBackgroundError: (message) => state.errors.push(message),
    onActiveSpeaker() {},
    onAudioLevels() {},
    onFollowedSpeakerChanged: (id) => state.followed.push(id),
    onParticipantLeft() {},
  });
  t.after(() => room.leave());
  await room.join('room', 'Local');
  return { room, state, signaling, settle: () => room.receiveReconcile ?? Promise.resolve() };
}

function speakerClock() {
  let now = 0;
  let nextId = 0;
  const timers = new Map();
  return {
    timers,
    globals: {
      performance: { now: () => now },
      setTimeout(callback, delay) {
        const id = ++nextId;
        timers.set(id, { callback, at: now + delay });
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
    },
    advance(ms) {
      const until = now + ms;
      while (timers.size) {
        const [id, timer] = [...timers].sort((left, right) => left[1].at - right[1].at)[0];
        if (timer.at > until) break;
        now = timer.at;
        timers.delete(id);
        timer.callback();
      }
      now = until;
    },
  };
}

test('Follow speaker promotes an offscreen camera after settling within the existing video budget', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, true, { policy: speakerPolicy });
  await settle();
  room.setRemoteVideoVisibility('p10', 'camera', false);
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p10' });
  clock.advance(1_499);
  await settle();
  assert.equal(state.media.tracks.has('v10'), false);
  clock.advance(1);
  await settle();
  assert.deepEqual(state.followed, ['p10']);
  assert.equal(state.media.tracks.has('v10'), true);
  assert.equal(state.media.tracks.has('v11'), true, 'the visible screen share remains selected');
  assert.equal(state.maximum, 9, 'retire the old camera before consuming the speaker');
  assert.equal(state.consumed.filter((id) => id.startsWith('a')).length, 12);
  assert.deepEqual(state.errors, []);
});

test('Follow speaker stays opt-in and disabling it retires an offscreen focused camera', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  assert.equal(room.followsSpeaker, false);
  assert.equal(clock.timers.size, 0);
  room.setRemoteVideoVisibility('p11', 'camera', false);
  room.setFollowSpeaker(true);
  clock.advance(1_500);
  await settle();
  assert.equal(state.media.tracks.has('v11'), true);
  room.setFollowSpeaker(false);
  await settle();
  assert.deepEqual(state.followed, ['p11', null]);
  assert.equal(state.media.tracks.has('v11'), false);
  assert.equal(clock.timers.size, 0);
});

test('speaker focus respects hidden cameras, audio-only and a hidden page', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  clock.advance(1_500);
  await settle();
  room.setRemoteMediaHidden('p11', true);
  await settle();
  assert.equal(state.media.tracks.has('v11'), false);
  assert.equal(clock.timers.size, 0);
  room.setRemoteMediaHidden('p11', false);
  room.setReceiveMode('audio-only');
  clock.advance(5_000);
  await settle();
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('v')).length, 0);
  room.setReceiveMode('data-saver');
  room.setMediaPageActive(false);
  clock.advance(5_000);
  await settle();
  assert.equal(clock.timers.size, 0);
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('v')).length, 0);
  room.setMediaPageActive(true);
  clock.advance(1_500);
  await settle();
  assert.equal(state.media.tracks.has('v11'), true);
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('v')).length, 4);
});

test('departing speakers and teardown cancel focus timers without later subscriptions', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  signaling.onMessage({ type: 'participantLeft', participantId: 'p11' });
  clock.advance(5_000);
  await settle();
  assert.deepEqual(state.followed, []);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p10' });
  assert.equal(clock.timers.size, 1);
  await room.leave();
  clock.advance(5_000);
  assert.equal(clock.timers.size, 0);
  assert.deepEqual(state.followed, []);
});

test('camera pause and closure clear focus while a fresh camera can be followed again', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  clock.advance(1_500);
  await settle();
  signaling.onMessage({ type: 'producerPaused', producerId: 'v11' });
  assert.deepEqual(state.followed, ['p11', null]);
  assert.equal(clock.timers.size, 0);
  signaling.onMessage({ type: 'producerResumed', producerId: 'v11' });
  clock.advance(1_500);
  await settle();
  assert.deepEqual(state.followed, ['p11', null, 'p11']);
  signaling.onMessage({ type: 'producerClosed', producerId: 'v11' });
  assert.deepEqual(state.followed, ['p11', null, 'p11', null]);
  assert.equal(clock.timers.size, 0);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'local' });
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'unknown' });
  clock.advance(5_000);
  await settle();
  assert.equal(clock.timers.size, 0, 'local or unknown speakers cannot nominate a remote camera');
});

test('moderation closure immediately retires a focused camera without waiting for a second event', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  clock.advance(1_500);
  await settle();
  signaling.onMessage({ type: 'forceClosedProducer', producerId: 'v11' });
  await settle();
  assert.deepEqual(state.followed, ['p11', null]);
  assert.equal(clock.timers.size, 0);
  assert.equal(state.media.tracks.has('v11'), false);
});

test('signaling loss cancels pending focus and recovery waits for a fresh speaker event', async (t) => {
  const clock = speakerClock();
  const speakerPolicy = await loadTypeScript('src/receive-policy.ts', { globals: clock.globals });
  const { room, state, signaling, settle } = await fixture(t, 12, false, { policy: speakerPolicy });
  room.setFollowSpeaker(true);
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p11' });
  clock.advance(700);
  signaling.connected = false;
  signaling.onConnectionLost();
  assert.equal(clock.timers.size, 0);
  clock.advance(10_000);
  await settle();
  assert.deepEqual(state.followed, []);
  assert.equal(room.followsSpeaker, true);
  signaling.connected = true;
  room.setMediaPageActive(false);
  room.setMediaPageActive(true);
  assert.equal(clock.timers.size, 0, 'old dominant-speaker state was retired');
  signaling.onMessage({ type: 'activeSpeaker', participantId: 'p10' });
  clock.advance(1_500);
  await settle();
  assert.deepEqual(state.followed, ['p10']);
});

for (const [mode, limit] of [
  ['balanced', 9],
  ['data-saver', 4],
]) {
  test(`a new visible screen share replaces a camera after retirement in ${mode}`, async (t) => {
    const { room, state, signaling, settle } = await fixture(t, limit, false);
    room.setReceiveMode(mode);
    for (let index = 0; index < limit; index++)
      room.setRemoteVideoVisibility(`p${index}`, 'camera', true);
    await settle();
    const consumed = state.consumed.length;
    const retired = deferred();
    state.beforeRetire = () => retired.promise;
    signaling.onMessage({
      type: 'newProducer',
      participantId: 'p0',
      producerId: 'v-screen',
      kind: 'video',
      source: 'screen',
    });
    room.setRemoteVideoVisibility('p0', 'screen', true);
    await flush();
    assert.equal(state.consumed.length, consumed, 'the old consumer must be retired first');
    assert.deepEqual(state.retired, [`v${limit - 1}`]);
    retired.resolve();
    await settle();
    assert.ok(state.media.tracks.has('v-screen'), 'the new share receives without a pin');
    assert.ok(!state.media.tracks.has(`v${limit - 1}`));
    assert.equal(state.maximum, limit);
    assert.deepEqual(
      [...state.media.tracks.keys()].filter((id) => id.startsWith('a')),
      Array.from({ length: limit }, (_, index) => `a${index}`),
    );
    assert.ok(
      state.tracks.some(
        ([participantId, , track, kind, source]) =>
          participantId === 'p0' &&
          track.id === 'v-screen' &&
          kind === 'video' &&
          source === 'screen',
      ),
    );
    signaling.onMessage({ type: 'producerClosed', producerId: 'v-screen' });
    await settle();
    assert.ok(state.media.tracks.has(`v${limit - 1}`), 'closing the share restores the camera');
    assert.equal(state.maximum, limit);
    assert.deepEqual(state.errors, []);
  });
}

test('room caps subscriptions, gives audio its own path, and includes screenshares in the same budget', async (t) => {
  const { room, state, settle } = await fixture(t);
  assert.equal(state.consumed.filter((id) => id.startsWith('v')).length, 9);
  assert.equal(state.consumed.filter((id) => id.startsWith('a')).length, 12);
  assert.equal(state.deferred.length, 3);
  assert.ok(state.media.tracks.has('v11'), 'the existing share receives without a pin');
  room.setPinnedRemoteVideo('p10');
  await settle();
  assert.equal(state.maximum, 9);
  assert.ok(state.media.tracks.has('v11'));
  assert.ok(state.media.tracks.has('v10'));
  assert.equal(state.retired.length, 1);
  assert.equal(room.telemetryCallState().selected, 21);
});

test('visible replacement waits for retirement confirmation before allocating its slot', async (t) => {
  const { room, state, settle } = await fixture(t);
  const retired = deferred();
  state.beforeRetire = () => retired.promise;
  room.setRemoteVideoVisibility('p0', 'camera', false);
  await flush();
  assert.equal(state.consumed.length, 21, 'no replacement before server ACK');
  retired.resolve();
  await settle();
  assert.ok(state.media.tracks.has('v8'));
  assert.ok(!state.media.tracks.has('v0'));
  assert.equal(state.maximum, 9);
  assert.deepEqual(
    state.deferred.find(([id, , , reason]) => id === 'p0' && reason === 'offscreen'),
    ['p0', 'Peer 0', 'camera', 'offscreen'],
  );
});

test('data saver and audio only retain audio and explicit person choices across restoration', async (t) => {
  const { room, state, settle } = await fixture(t, 5);
  room.setRemoteVideoQuality('p0', 'high');
  room.setRemoteVideoSizeCap('p0', 1);
  room.setRemoteMediaHidden('p1', true);
  room.setReceiveMode('data-saver');
  await settle();
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('v')).length, 4);
  assert.equal(state.media.caps.get('v0'), 0);
  assert.equal(state.media.caps.get('v4'), 0, 'screenshares also receive the saver ceiling');
  room.setReceiveMode('audio-only');
  await settle();
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('v')).length, 0);
  assert.equal([...state.media.tracks.keys()].filter((id) => id.startsWith('a')).length, 5);
  assert.equal(state.media.hidden.get('a1'), true);
  assert.equal(room.telemetryCallState().selected, 4);
  room.setReceiveMode('balanced');
  await settle();
  assert.equal(state.media.caps.get('v0'), 1);
  assert.equal(state.media.qualities.get('v0'), 'high');
  assert.ok(!state.media.tracks.has('v1'), 'hidden person stays hidden');
});

test('hidden pages release video and resume subscriptions without local capture', async (t) => {
  const { room, state, settle } = await fixture(t, 2);
  room.setPinnedRemoteVideo('p0');
  room.setMediaPageActive(false);
  await settle();
  assert.deepEqual([...state.media.tracks.keys()], ['a0', 'a1']);
  room.setMediaPageActive(true);
  await settle();
  assert.ok(state.media.tracks.has('v0'));
  assert.ok(state.media.tracks.has('v1'));
});

test('a pending video cannot reach the UI after audio-only supersedes it', async (t) => {
  const { room, state, settle } = await fixture(t, 1);
  room.setReceiveMode('audio-only');
  await settle();
  const pending = deferred();
  state.beforeConsume = () => pending.promise;
  const delivered = state.tracks.length;
  room.setReceiveMode('balanced');
  await flush();
  room.setReceiveMode('audio-only');
  pending.resolve();
  await settle();
  assert.equal(state.tracks.length, delivered);
  assert.deepEqual([...state.media.tracks.keys()], ['a0']);
});

test('leaving during retirement prevents late replacement and a retirement error never exceeds the budget', async (t) => {
  const { room, state, settle } = await fixture(t);
  state.beforeRetire = () => {
    throw new Error('Connection lost');
  };
  room.setRemoteVideoVisibility('p0', 'camera', false);
  await assert.rejects(settle(), /Connection lost/);
  assert.equal(state.consumed.length, 21);
  assert.equal(state.maximum, 9);
  const pending = deferred();
  state.beforeRetire = () => pending.promise;
  room.setPinnedRemoteVideo('p11', 'screen');
  const settling = settle();
  await flush();
  await room.leave();
  pending.resolve();
  await settling;
  assert.equal(state.consumed.length, 21);
});
