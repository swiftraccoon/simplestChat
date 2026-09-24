import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush } from './ui-fixture.mjs';

test('producer closure updates local controls and still removes remote media', async () => {
  const localProducers = new Set(['local-camera']);
  const closedConsumers = [];
  const removedTracks = [];
  let localChanges = 0;
  class FakeMediaManager {
    async setup() {}
    async consume() {
      return {};
    }
    closeLocalProducer(id) {
      return localProducers.delete(id);
    }
    closeConsumerByProducer(id) {
      closedConsumers.push(id);
    }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
  });
  const signaling = {
    setOnMessage(handler) {
      this.onMessage = handler;
    },
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
    completeRestartRecovery() {},
    send() {
      queueMicrotask(() =>
        this.onMessage({
          type: 'roomJoined',
          participantId: 'local',
          reconnectToken: 'token',
          yourRole: 'user',
          participants: [
            {
              id: 'remote',
              name: 'Remote participant',
              role: 'user',
              producers: [{ id: 'remote-camera', kind: 'video', source: 'camera' }],
            },
          ],
        }),
      );
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onRemoteTrack() {},
    onRemoteTrackRemoved(...args) {
      removedTracks.push(args);
    },
    onLocalMediaChanged() {
      localChanges++;
    },
  });
  await room.join('room', 'Local participant');

  signaling.onMessage({
    type: 'forceClosedProducer',
    producerId: 'local-camera',
    reason: 'Closed',
  });
  assert.equal(localChanges, 1);
  assert.equal(localProducers.size, 0);
  assert.deepEqual(closedConsumers, []);
  // The server can follow the moderation notification with normal closure.
  signaling.onMessage({ type: 'producerClosed', producerId: 'local-camera' });
  assert.equal(localChanges, 1, 'duplicate closure should not reset controls twice');

  signaling.onMessage({
    type: 'forceClosedProducer',
    producerId: 'remote-camera',
    reason: 'Closed',
  });
  assert.equal(localChanges, 1);
  assert.deepEqual(closedConsumers, ['remote-camera']);
  assert.deepEqual(removedTracks, [['remote', 'remote-camera', 'video', 'camera']]);
  assert.equal(room.getParticipants().get('remote').producers.size, 0);

  localProducers.add('local-mic');
  signaling.onMessage({ type: 'producerClosed', producerId: 'local-mic' });
  assert.equal(localChanges, 2, 'ordinary closure must also release a local producer');
  assert.equal(localProducers.size, 0);
});

test('a rejected remote subscription is reported instead of silently leaving a blank tile', async () => {
  const reported = [];
  class FakeMediaManager {
    async setup() {}
    async consume() {
      throw new Error('Consumer limit reached (64)');
    }
    closeConsumerByProducer() {}
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
  });
  const signaling = {
    setOnMessage(handler) {
      this.onMessage = handler;
    },
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
    completeRestartRecovery() {},
    send() {
      queueMicrotask(() =>
        this.onMessage({
          type: 'roomJoined',
          participantId: 'local',
          reconnectToken: 'token',
          yourRole: 'user',
          participants: [
            {
              id: 'remote',
              name: 'Remote participant',
              role: 'user',
              producers: [{ id: 'remote-camera', kind: 'video', source: 'camera' }],
            },
          ],
        }),
      );
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onRemoteTrack() {
      assert.fail('no track can be delivered for a rejected subscription');
    },
    onRemoteTrackRemoved() {},
    onLocalMediaChanged() {},
    onRemoteMediaUnavailable(...args) {
      reported.push(args);
    },
  });
  await room.join('room', 'Local participant');
  assert.deepEqual(reported, [
    ['remote', 'Remote participant', 'video', 'camera', 'Consumer limit reached (64)'],
  ]);
});

async function captureStoppedFixture(events = {}) {
  const managers = [];
  class FakeMediaManager {
    constructor() {
      managers.push(this);
    }
    audioEnabled = true;
    videoEnabled = true;
    closed = false;
    async setup() {}
    close() {
      this.closed = true;
    }
    stopCapture(kind) {
      this[`${kind}Enabled`] = false;
      this.onLocalCaptureStopped?.(kind);
    }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
  });
  const signaling = {
    setOnMessage(handler) {
      this.onMessage = handler;
    },
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
    completeRestartRecovery() {},
    send(message) {
      if (message.type !== 'joinRoom') return;
      queueMicrotask(() =>
        this.onMessage({
          type: 'roomJoined',
          participantId: 'local',
          reconnectToken: 'token',
          yourRole: 'user',
          participants: [],
        }),
      );
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onLocalMediaChanged() {},
    ...events,
  });
  await room.join('room', 'Local participant');
  return { room, managers };
}

test('capture stop refreshes current local state before reporting only the stopped kind', async () => {
  const observed = [];
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() {
      observed.push(['changed', room.audioEnabled, room.videoEnabled]);
    },
    onLocalCaptureStopped(kind) {
      observed.push(['stopped', kind]);
    },
  });
  managers[0].stopCapture('audio');
  assert.deepEqual(observed, [
    ['changed', false, true],
    ['stopped', 'audio'],
  ]);
  managers[0].stopCapture('video');
  assert.deepEqual(observed.slice(2), [
    ['changed', false, false],
    ['stopped', 'video'],
  ]);
  await room.leave();
});

test('capture stop remains compatible with handlers that only refresh local media', async () => {
  let changes = 0;
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() {
      changes++;
    },
  });
  managers[0].stopCapture('audio');
  assert.equal(changes, 1);
  await room.leave();
});

test('capture callbacks from a left or replaced media manager cannot update the room', async () => {
  const observed = [];
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() {
      observed.push('changed');
    },
    onLocalCaptureStopped(kind) {
      observed.push(kind);
    },
  });
  const oldCallback = managers[0].onLocalCaptureStopped;
  await room.leave();
  assert.equal(managers[0].closed, true);
  oldCallback('audio');
  assert.deepEqual(observed, []);
  await room.join('another-room', 'Local participant');
  oldCallback('video');
  assert.deepEqual(observed, []);
  managers[1].stopCapture('audio');
  assert.deepEqual(observed, ['changed', 'audio']);
  await room.leave();
});

test('capture notification is retired if the local-media callback leaves the room', async () => {
  const observed = [];
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() {
      observed.push('changed');
      void room.leave();
    },
    onLocalCaptureStopped(kind) {
      observed.push(kind);
    },
  });
  managers[0].stopCapture('audio');
  assert.deepEqual(observed, ['changed']);
  assert.equal(managers[0].closed, true);
});

async function incomingRefreshFixture(producerIds = ['camera', 'microphone']) {
  const managers = [];
  const tracks = [];
  const unavailable = [];
  class FakeMediaManager {
    constructor() {
      managers.push(this);
    }
    audioEnabled = false;
    videoEnabled = true;
    captureTrack = { kind: 'video', stopped: false };
    consumers = new Map();
    consumed = [];
    retired = [];
    closedConsumers = [];
    failedConsumes = new Set();
    closed = false;
    async setup() {}
    async consume(producerId) {
      this.consumed.push(producerId);
      await this.beforeConsume?.(producerId);
      if (this.failedConsumes.has(producerId)) throw new Error('Subscription unavailable');
      const track = { producerId };
      this.consumers.set(producerId, track);
      return track;
    }
    getConsumerTrackByProducer(producerId) {
      return this.consumers.get(producerId);
    }
    async retireConsumerByProducer(producerId) {
      this.retired.push(producerId);
      this.consumers.delete(producerId);
      await this.beforeRetire?.(producerId);
    }
    closeConsumerByProducer(producerId) {
      this.closedConsumers.push(producerId);
      this.consumers.delete(producerId);
    }
    closeLocalProducer() {
      return false;
    }
    close() {
      this.closed = true;
      this.consumers.clear();
    }
    suspendSignaling() {}
    toggleAudio() {
      assert.fail('receive recovery must not request local audio capture');
    }
    toggleVideo() {
      assert.fail('receive recovery must not request local video capture');
    }
    muteAudio() {
      assert.fail('receive recovery must preserve local mute intent');
    }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
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
      if (message.type !== 'joinRoom') return;
      queueMicrotask(() =>
        this.onMessage({
          type: 'roomJoined',
          participantId: 'local',
          reconnectToken: 'token',
          yourRole: 'user',
          participants: [
            {
              id: 'remote',
              name: 'Remote participant',
              role: 'user',
              producers: producerIds.map((id) => ({
                id,
                kind: id === 'microphone' ? 'audio' : 'video',
                source: id === 'microphone' ? 'microphone' : 'camera',
              })),
            },
          ],
        }),
      );
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onLocalMediaChanged() {},
    onRemoteTrack(...args) {
      tracks.push(args);
    },
    onRemoteTrackRemoved() {},
    onRemoteMediaUnavailable(...args) {
      unavailable.push(args);
    },
  });
  await room.join('room', 'Local participant');
  return { room, signaling, managers, tracks, unavailable };
}

test('incoming refresh replaces subscriptions while preserving local capture and mute intent', async () => {
  const { room, managers, tracks } = await incomingRefreshFixture();
  const media = managers[0];
  const capture = media.captureTrack;
  await room.refreshIncomingMedia();
  assert.deepEqual(media.retired, ['camera', 'microphone']);
  assert.deepEqual(media.consumed, ['camera', 'microphone', 'camera', 'microphone']);
  assert.equal(tracks.length, 4);
  assert.equal(managers.length, 1, 'refresh must reuse the current media manager');
  assert.equal(media.captureTrack, capture);
  assert.equal(capture.stopped, false);
  assert.equal(room.audioEnabled, false);
  assert.equal(room.videoEnabled, true);
  await room.leave();
});

test('concurrent incoming refresh clicks share the same pending retirement', async () => {
  const { room, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeRetire = () => pending.promise;
  const first = room.refreshIncomingMedia();
  const second = room.refreshIncomingMedia();
  assert.equal(first, second);
  await flush();
  assert.deepEqual(managers[0].retired, ['camera']);
  pending.resolve();
  await first;
  assert.deepEqual(managers[0].consumed, ['camera', 'camera']);
  await room.leave();
});

for (const transition of ['leave', 'replace membership', 'lose signaling']) {
  test(`incoming refresh cannot resubscribe after ${transition} during retirement`, async () => {
    const { room, managers, signaling } = await incomingRefreshFixture(['camera']);
    const oldMedia = managers[0];
    const pending = deferred();
    oldMedia.beforeRetire = () => pending.promise;
    const refresh = room.refreshIncomingMedia();
    const rejected = assert.rejects(refresh, /room connection changed/);
    await flush();
    assert.deepEqual(oldMedia.retired, ['camera']);
    if (transition === 'lose signaling') {
      signaling.connected = false;
      signaling.onConnectionLost();
    } else {
      await room.leave();
      if (transition === 'replace membership') await room.join('another-room', 'Local');
    }
    pending.resolve();
    await rejected;
    assert.deepEqual(oldMedia.consumed, ['camera']);
    await room.leave();
  });
}

test('producer removal during retirement does not start another subscription', async () => {
  const { room, signaling, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeRetire = () => pending.promise;
  const refresh = room.refreshIncomingMedia();
  await flush();
  signaling.onMessage({ type: 'producerClosed', producerId: 'camera' });
  pending.resolve();
  await refresh;
  assert.deepEqual(managers[0].consumed, ['camera']);
  assert.equal(room.getParticipants().get('remote').producers.size, 0);
  await room.leave();
});

test('incoming refresh owns a finite roster and leaves newly arriving sources to normal consumption', async () => {
  const { room, signaling, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeRetire = () => pending.promise;
  const refresh = room.refreshIncomingMedia();
  await flush();
  signaling.onMessage({
    type: 'newProducer',
    participantId: 'remote',
    producerId: 'new-camera',
    kind: 'video',
    source: 'camera',
  });
  await flush();
  pending.resolve();
  await refresh;
  assert.deepEqual(managers[0].retired, ['camera']);
  assert.equal(managers[0].consumed.filter((id) => id === 'new-camera').length, 1);
  await room.leave();
});

test('incoming refresh cannot publish a late replacement track after signaling ownership changes', async () => {
  const { room, signaling, managers, tracks } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeConsume = () => pending.promise;
  const refresh = room.refreshIncomingMedia();
  const rejected = assert.rejects(refresh, /room connection changed/);
  await flush();
  assert.deepEqual(managers[0].consumed, ['camera', 'camera']);
  signaling.connected = false;
  signaling.onConnectionLost();
  pending.resolve();
  await rejected;
  assert.equal(
    tracks.length,
    1,
    'the replacement socket must reconcile before adopting a late track',
  );
  assert.deepEqual(managers[0].closedConsumers, ['camera']);
  await room.leave();
});

test('incoming refresh reports partial subscription failure while trying the remaining sources', async () => {
  const { room, managers, unavailable } = await incomingRefreshFixture();
  managers[0].failedConsumes.add('camera');
  await assert.rejects(room.refreshIncomingMedia(), /Some incoming media could not be refreshed/);
  assert.deepEqual(managers[0].retired, ['camera', 'microphone']);
  assert.deepEqual(managers[0].consumed, ['camera', 'microphone', 'camera', 'microphone']);
  assert.equal(unavailable.length, 1);
  assert.equal(unavailable[0][4], 'Subscription unavailable');
  assert.ok(managers[0].consumers.has('microphone'));
  managers[0].failedConsumes.clear();
  await room.refreshIncomingMedia();
  await room.leave();
});

test('incoming refresh reports a rejected retirement without starting its replacement', async () => {
  const { room, managers } = await incomingRefreshFixture();
  managers[0].beforeRetire = (producerId) => {
    if (producerId === 'camera') throw new Error('Retirement not confirmed');
  };
  await assert.rejects(room.refreshIncomingMedia(), /Some incoming media could not be refreshed/);
  assert.deepEqual(managers[0].consumed, ['camera', 'microphone', 'microphone']);
  await room.leave();
});
