import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function settingsSender() {
  const messages = [];
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: class {} } },
  });
  const room = new RoomClient(
    {
      setOnMessage() {},
      setOnReconnected() {},
      setOnReconnectFailed() {},
      setOnConnectionLost() {},
      completeRestartRecovery() {},
      send(message) {
        messages.push(JSON.parse(JSON.stringify(message)));
      },
    },
    {},
  );
  return { room, messages };
}

test('settings patches omit unchanged nullable fields from the wire', async () => {
  const { room, messages } = await settingsSender();
  room.updateRoomSettings({});
  room.updateRoomSettings({ allowChat: false });
  assert.deepEqual(messages, [
    { type: 'updateRoomSettings' },
    { type: 'updateRoomSettings', allowChat: false },
  ]);
});

test('settings patches send explicit null to clear password and capacity limits', async () => {
  const { room, messages } = await settingsSender();
  const patch = Object.freeze({ password: null, maxBroadcasters: null, maxParticipants: null });
  room.updateRoomSettings(patch);
  assert.deepEqual(messages, [{ type: 'updateRoomSettings', ...patch }]);
  assert.equal(Object.hasOwn(patch, 'type'), false, 'sending does not mutate the caller patch');
});

test('settings patches retain values and mixed omitted, cleared, and set fields', async () => {
  const { room, messages } = await settingsSender();
  room.updateRoomSettings({ password: 'room-passphrase', maxBroadcasters: 4, maxParticipants: 12 });
  room.updateRoomSettings({ password: null, maxParticipants: 20, guestsAllowed: false });
  assert.deepEqual(messages, [
    {
      type: 'updateRoomSettings',
      password: 'room-passphrase',
      maxBroadcasters: 4,
      maxParticipants: 12,
    },
    { type: 'updateRoomSettings', password: null, maxParticipants: 20, guestsAllowed: false },
  ]);
});
