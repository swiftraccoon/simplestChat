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
