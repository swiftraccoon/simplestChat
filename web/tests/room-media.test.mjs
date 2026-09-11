import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

test('producer closure updates local controls and still removes remote media', async () => {
  const localProducers = new Set(['local-camera']);
  const closedConsumers = [];
  const removedTracks = [];
  let localChanges = 0;
  class FakeMediaManager {
    async setup() {}
    async consume() { return {}; }
    closeLocalProducer(id) { return localProducers.delete(id); }
    closeConsumerByProducer(id) { closedConsumers.push(id); }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
  });
  const signaling = {
    setOnMessage(handler) { this.onMessage = handler; },
    setOnReconnected() {},
    send() {
      queueMicrotask(() => this.onMessage({
        type: 'roomJoined', participantId: 'local', reconnectToken: 'token',
        yourRole: 'user', participants: [{
          id: 'remote', name: 'Remote participant', role: 'user',
          producers: [{ id: 'remote-camera', kind: 'video', source: 'camera' }],
        }],
      }));
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {},
    onRemoteTrack() {},
    onRemoteTrackRemoved(...args) { removedTracks.push(args); },
    onLocalMediaChanged() { localChanges++; },
  });
  await room.join('room', 'Local participant');

  signaling.onMessage({ type: 'forceClosedProducer', producerId: 'local-camera', reason: 'Closed' });
  assert.equal(localChanges, 1);
  assert.equal(localProducers.size, 0);
  assert.deepEqual(closedConsumers, []);
  // The server can follow the moderation notification with normal closure.
  signaling.onMessage({ type: 'producerClosed', producerId: 'local-camera' });
  assert.equal(localChanges, 1, 'duplicate closure should not reset controls twice');

  signaling.onMessage({ type: 'forceClosedProducer', producerId: 'remote-camera', reason: 'Closed' });
  assert.equal(localChanges, 1);
  assert.deepEqual(closedConsumers, ['remote-camera']);
  assert.deepEqual(removedTracks, [['remote', 'remote-camera', 'video', 'camera']]);
  assert.equal(room.getParticipants().get('remote').producers.size, 0);

  localProducers.add('local-mic');
  signaling.onMessage({ type: 'producerClosed', producerId: 'local-mic' });
  assert.equal(localChanges, 2, 'ordinary closure must also release a local producer');
  assert.equal(localProducers.size, 0);
});

async function captureStoppedFixture(events = {}) {
  const managers = [];
  class FakeMediaManager {
    constructor() { managers.push(this); }
    audioEnabled = true;
    videoEnabled = true;
    closed = false;
    async setup() {}
    close() { this.closed = true; }
    stopCapture(kind) {
      this[`${kind}Enabled`] = false;
      this.onLocalCaptureStopped?.(kind);
    }
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: FakeMediaManager } },
  });
  const signaling = {
    setOnMessage(handler) { this.onMessage = handler; },
    setOnReconnected() {},
    send(message) {
      if (message.type !== 'joinRoom') return;
      queueMicrotask(() => this.onMessage({
        type: 'roomJoined', participantId: 'local', reconnectToken: 'token',
        yourRole: 'user', participants: [],
      }));
    },
  };
  const room = new RoomClient(signaling, {
    onParticipantsChanged() {}, onLocalMediaChanged() {}, ...events,
  });
  await room.join('room', 'Local participant');
  return { room, managers };
}

test('capture stop refreshes current local state before reporting only the stopped kind', async () => {
  const observed = [];
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() { observed.push(['changed', room.audioEnabled, room.videoEnabled]); },
    onLocalCaptureStopped(kind) { observed.push(['stopped', kind]); },
  });
  managers[0].stopCapture('audio');
  assert.deepEqual(observed, [['changed', false, true], ['stopped', 'audio']]);
  managers[0].stopCapture('video');
  assert.deepEqual(observed.slice(2), [['changed', false, false], ['stopped', 'video']]);
  await room.leave();
});

test('capture stop remains compatible with handlers that only refresh local media', async () => {
  let changes = 0;
  const { room, managers } = await captureStoppedFixture({ onLocalMediaChanged() { changes++; } });
  managers[0].stopCapture('audio');
  assert.equal(changes, 1);
  await room.leave();
});

test('capture callbacks from a left or replaced media manager cannot update the room', async () => {
  const observed = [];
  const { room, managers } = await captureStoppedFixture({
    onLocalMediaChanged() { observed.push('changed'); },
    onLocalCaptureStopped(kind) { observed.push(kind); },
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
    onLocalMediaChanged() { observed.push('changed'); void room.leave(); },
    onLocalCaptureStopped(kind) { observed.push(kind); },
  });
  managers[0].stopCapture('audio');
  assert.deepEqual(observed, ['changed']);
  assert.equal(managers[0].closed, true);
});
