import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
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
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './media': { MediaManager: FakeMediaManager },
    },
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
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './media': { MediaManager: FakeMediaManager },
    },
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
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './media': { MediaManager: FakeMediaManager },
    },
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
    replacements = 0;
    receiverVersion = 0;
    replacing = false;
    closedConsumers = [];
    failedConsumes = new Set();
    qualities = [];
    closed = false;
    async setup() {}
    callHealthTransportState() {
      return {
        microphone: this.audioEnabled ? 'live' : 'off',
        camera: this.videoEnabled ? 'live' : 'off',
        sharing: false,
        sendState: 'connected',
        receiveState: this.replacing ? 'connecting' : 'connected',
      };
    }
    async consume(producerId) {
      const version = this.receiverVersion;
      this.consumed.push(producerId);
      if (this.replacing) throw new Error('Device/transport not ready');
      await this.beforeConsume?.(producerId);
      if (version !== this.receiverVersion || this.closed) throw new Error('Media session closed');
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
    async replaceReceiveTransport() {
      this.replacements++;
      this.receiverVersion++;
      this.replacing = true;
      this.retired.push(...this.consumers.keys());
      this.consumers.clear();
      try {
        await this.beforeReplace?.();
      } finally {
        this.replacing = false;
      }
    }
    closeConsumerByProducer(producerId) {
      this.closedConsumers.push(producerId);
      this.consumers.delete(producerId);
    }
    closeLocalProducer() {
      return false;
    }
    setConsumerQualityByProducer(producerId, quality) {
      this.qualities.push([producerId, quality]);
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
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './media': { MediaManager: FakeMediaManager },
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
                source: id === 'microphone' ? 'microphone' : id === 'screen' ? 'screen' : 'camera',
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
  assert.deepEqual([...media.retired].sort(), ['camera', 'microphone']);
  assert.deepEqual(media.consumed, ['microphone', 'camera', 'microphone', 'camera']);
  assert.equal(media.replacements, 1);
  assert.equal(tracks.length, 4);
  assert.equal(managers.length, 1, 'refresh must reuse the current media manager');
  assert.equal(media.captureTrack, capture);
  assert.equal(capture.stopped, false);
  assert.equal(room.audioEnabled, false);
  assert.equal(room.videoEnabled, true);
  await room.leave();
});

test('camera quality choices never lower a participant screen share', async () => {
  const { room, managers } = await incomingRefreshFixture(['camera', 'screen']);
  room.setRemoteVideoQuality('remote', 'low');
  assert.deepEqual(managers[0].qualities, [['camera', 'low']]);
  await room.refreshIncomingMedia();
  assert.ok(managers[0].qualities.every(([producer]) => producer === 'camera'));
  assert.ok(managers[0].consumers.has('screen'));
  await room.leave();
});

test('concurrent incoming refresh clicks share the same pending receiver replacement', async () => {
  const { room, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeReplace = () => pending.promise;
  const first = room.refreshIncomingMedia();
  const second = room.refreshIncomingMedia();
  assert.equal(first, second);
  await flush();
  assert.deepEqual(managers[0].retired, ['camera']);
  assert.equal(managers[0].replacements, 1);
  pending.resolve();
  await first;
  assert.deepEqual(managers[0].consumed, ['camera', 'camera']);
  await room.leave();
});

for (const transition of ['leave', 'replace membership', 'lose signaling']) {
  test(`incoming refresh cannot resubscribe after ${transition} during replacement`, async () => {
    const { room, managers, signaling } = await incomingRefreshFixture(['camera']);
    const oldMedia = managers[0];
    const pending = deferred();
    oldMedia.beforeReplace = () => pending.promise;
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

test('producer removal during replacement does not start another subscription', async () => {
  const { room, signaling, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeReplace = () => pending.promise;
  const refresh = room.refreshIncomingMedia();
  await flush();
  signaling.onMessage({ type: 'producerClosed', producerId: 'camera' });
  pending.resolve();
  await refresh;
  assert.deepEqual(managers[0].consumed, ['camera']);
  assert.equal(room.getParticipants().get('remote').producers.size, 0);
  await room.leave();
});

test('incoming refresh reconciles newly arriving video once on the replacement receiver', async () => {
  const { room, signaling, managers } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeReplace = () => pending.promise;
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
  assert.equal(managers[0].replacements, 1);
  assert.equal(managers[0].consumed.filter((id) => id === 'new-camera').length, 1);
  await room.leave();
});

test('new voice arriving during receiver replacement waits for the fresh transport', async () => {
  const { room, signaling, managers, unavailable } = await incomingRefreshFixture(['camera']);
  const pending = deferred();
  managers[0].beforeReplace = () => pending.promise;
  const refresh = room.refreshIncomingMedia();
  await flush();
  signaling.onMessage({
    type: 'newProducer',
    participantId: 'remote',
    producerId: 'new-voice',
    kind: 'audio',
    source: 'microphone',
  });
  await flush();
  assert.equal(
    managers[0].consumed.includes('new-voice'),
    false,
    'fresh voice must not attempt to use the deliberately retired receiver',
  );
  pending.resolve();
  await refresh;
  assert.equal(managers[0].consumed.filter((id) => id === 'new-voice').length, 1);
  assert.equal(unavailable.length, 0);
  await room.leave();
});

test('an old hung consume cannot block replacement or overwrite its fresh track later', async () => {
  const { room, signaling, managers, tracks } = await incomingRefreshFixture(['camera']);
  const old = deferred();
  let block = true;
  managers[0].beforeConsume = (id) => {
    if (id === 'late-camera' && block) {
      block = false;
      return old.promise;
    }
  };
  signaling.onMessage({
    type: 'newProducer',
    participantId: 'remote',
    producerId: 'late-camera',
    kind: 'video',
    source: 'camera',
  });
  await flush();
  await room.refreshIncomingMedia();
  const fresh = managers[0].consumers.get('late-camera');
  assert.ok(fresh, 'the fresh receiver does not wait for the superseded native promise');
  const published = tracks.length;
  old.resolve();
  await flush();
  assert.equal(managers[0].consumers.get('late-camera'), fresh);
  assert.equal(tracks.length, published, 'the late old consume cannot publish another track');
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
  assert.deepEqual([...managers[0].retired].sort(), ['camera', 'microphone']);
  assert.deepEqual(managers[0].consumed, ['microphone', 'camera', 'microphone', 'camera']);
  assert.equal(unavailable.length, 1);
  assert.equal(unavailable[0][4], 'Subscription unavailable');
  assert.ok(managers[0].consumers.has('microphone'));
  managers[0].failedConsumes.clear();
  await room.refreshIncomingMedia();
  assert.equal(managers[0].replacements, 2);
  assert.equal(room.callHealthState().receiveFailed, false);
  await room.leave();
});

test('incoming refresh reports a rejected receiver replacement without consuming and permits explicit retry', async () => {
  const { room, managers } = await incomingRefreshFixture();
  managers[0].beforeReplace = () => {
    throw new Error('Replacement not confirmed');
  };
  await assert.rejects(room.refreshIncomingMedia(), /Replacement not confirmed/);
  assert.deepEqual(managers[0].consumed, ['microphone', 'camera']);
  assert.equal(room.callHealthState().receiveFailed, true);
  managers[0].beforeReplace = undefined;
  await room.refreshIncomingMedia();
  assert.equal(managers[0].replacements, 2);
  assert.equal(room.callHealthState().receiveFailed, false);
  await room.leave();
});

test('room construction defers the heavy media module and a retired import cannot allocate media', async () => {
  let imports = 0;
  let instances = 0;
  const modules = { './receive-policy': await loadTypeScript('src/receive-policy.ts') };
  Object.defineProperty(modules, './media', {
    get() {
      imports++;
      return {
        MediaManager: class {
          constructor() {
            instances++;
          }
          async setup() {}
        },
      };
    },
  });
  const { RoomClient } = await loadTypeScript('src/room.ts', { modules });
  assert.equal(imports, 0);
  const signaling = {
    setOnMessage() {},
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
  };
  const room = new RoomClient(signaling, {});
  assert.equal(imports, 0, 'home/account use of the room module never loads mediasoup');
  const setup = room.setupMedia(room.membershipVersion);
  room.generation++;
  await setup;
  assert.equal(imports, 1);
  assert.equal(instances, 0, 'leaving while the chunk loads retires its completion');
});

test('a failed deferred import leaves chat connected and an explicit incoming refresh retries without capture', async () => {
  let imports = 0;
  let setups = 0;
  const errors = [];
  const modules = { './receive-policy': await loadTypeScript('src/receive-policy.ts') };
  Object.defineProperty(modules, './media', {
    get() {
      imports++;
      if (imports === 1) throw new Error('Chunk unavailable after release');
      return {
        MediaManager: class {
          async setup() {
            setups++;
          }
        },
      };
    },
  });
  const { RoomClient } = await loadTypeScript('src/room.ts', { modules });
  const signaling = {
    connected: true,
    setOnMessage() {},
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
  };
  const room = new RoomClient(signaling, { onBackgroundError: (message) => errors.push(message) });
  room.localId = 'viewer';
  await room.setupMedia(room.membershipVersion);
  assert.equal(room.connected, true);
  assert.equal(room.hasMedia, false);
  assert.match(errors[0], /Chat remains available/);
  assert.equal(imports, 1, 'there is no automatic reload or repeated import');
  await room.refreshIncomingMedia();
  assert.equal(room.hasMedia, true);
  assert.equal(imports, 2);
  assert.equal(setups, 1);
});

test('a browser-retained module failure keeps chat and gives explicit leave/reload guidance', async () => {
  const errors = [];
  const modules = { './receive-policy': await loadTypeScript('src/receive-policy.ts') };
  Object.defineProperty(modules, './media', {
    get() {
      throw new Error('Retained module failure');
    },
  });
  const { RoomClient } = await loadTypeScript('src/room.ts', { modules });
  const room = new RoomClient(
    {
      connected: true,
      setOnMessage() {},
      setOnReconnected() {},
      setOnReconnectFailed() {},
      setOnConnectionLost() {},
    },
    { onBackgroundError: (message) => errors.push(message) },
  );
  room.localId = 'viewer';
  await room.setupMedia(room.membershipVersion);
  await assert.rejects(room.refreshIncomingMedia(), /Leave the call and reload/);
  assert.equal(room.connected, true);
  assert.equal(room.hasMedia, false);
  assert.equal(errors.length, 2);
});
