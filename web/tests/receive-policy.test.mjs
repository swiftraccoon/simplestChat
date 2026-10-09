import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
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

async function fixture(t, count = 12) {
  const state = {
    media: null,
    tracks: [],
    deferred: [],
    errors: [],
    consumed: [],
    retired: [],
    maximum: 0,
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
      './receive-policy': policy,
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
    setOnConnectionLost() {},
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
                  source: index === count - 1 ? 'screen' : 'camera',
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
  });
  t.after(() => room.leave());
  await room.join('room', 'Local');
  return { room, state, signaling, settle: () => room.receiveReconcile ?? Promise.resolve() };
}

test('room caps subscriptions, gives audio its own path, and includes screenshares in the same budget', async (t) => {
  const { room, state, settle } = await fixture(t);
  assert.equal(state.consumed.filter((id) => id.startsWith('v')).length, 9);
  assert.equal(state.consumed.filter((id) => id.startsWith('a')).length, 12);
  assert.equal(state.deferred.length, 3);
  room.setPinnedRemoteVideo('p11', 'screen');
  await settle();
  assert.equal(state.maximum, 9);
  assert.ok(state.media.tracks.has('v11'));
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
  assert.ok(state.media.tracks.has('v9'));
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
