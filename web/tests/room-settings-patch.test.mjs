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
      connected: true,
      async request(message, response, timeout) {
        assert.equal(response, 'roomControlApplied');
        assert.equal(timeout, 25_000);
        messages.push(JSON.parse(JSON.stringify(message)));
      },
    },
    {},
  );
  room.localId = 'local';
  return { room, messages };
}

test('settings patches omit unchanged nullable fields from the wire', async () => {
  const { room, messages } = await settingsSender();
  await room.updateRoomSettings({});
  await room.updateRoomSettings({ allowChat: false });
  assert.deepEqual(messages, [
    { type: 'updateRoomSettings' },
    { type: 'updateRoomSettings', allowChat: false },
  ]);
});

test('settings patches send explicit null to clear password and capacity limits', async () => {
  const { room, messages } = await settingsSender();
  const patch = Object.freeze({ password: null, maxBroadcasters: null, maxParticipants: null });
  await room.updateRoomSettings(patch);
  assert.deepEqual(messages, [{ type: 'updateRoomSettings', ...patch }]);
  assert.equal(Object.hasOwn(patch, 'type'), false, 'sending does not mutate the caller patch');
});

test('settings patches retain values and mixed omitted, cleared, and set fields', async () => {
  const { room, messages } = await settingsSender();
  await room.updateRoomSettings({
    password: 'room-passphrase',
    maxBroadcasters: 4,
    maxParticipants: 12,
  });
  await room.updateRoomSettings({ password: null, maxParticipants: 20, guestsAllowed: false });
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

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

test('room commands remain pending until acknowledgement and block duplicate actions', async () => {
  const { room } = await settingsSender();
  const ack = deferred();
  const commands = [];
  room.signaling.request = (command) => {
    commands.push(command);
    return ack.promise;
  };
  let complete = false;
  const first = room.admitFromLobby('waiting').then(() => {
    complete = true;
  });
  await Promise.resolve();
  assert.equal(complete, false);
  await assert.rejects(room.admitFromLobby('waiting'), /awaiting confirmation/);
  assert.deepEqual(commands, [{ type: 'admitFromLobby', targetParticipantId: 'waiting' }]);
  ack.resolve({ type: 'roomControlApplied', requestId: 'accepted' });
  await first;
  assert.equal(complete, true);
});

test('lost acknowledgements refresh state without repeating a mutation and remain uncertain', async () => {
  const { room } = await settingsSender();
  const sent = [];
  room.signaling.request = async (command) => {
    sent.push(command);
    throw new Error('Request timed out');
  };
  room.requestSocial = async (action) => {
    assert.equal(action, 'getRoomSnapshot');
    sent.push(action);
  };
  await assert.rejects(room.setTopic('new topic'), /Current room state has been refreshed/);
  assert.deepEqual(sent, [{ type: 'setTopic', topic: 'new topic' }, 'getRoomSnapshot']);
  assert.equal(room.pendingControls.size, 0);
});

test('room changes fence both late success and failure without refreshing a replacement session', async () => {
  for (const rejected of [false, true]) {
    const { room } = await settingsSender();
    const ack = deferred();
    room.signaling.request = () => ack.promise;
    room.requestSocial = () => assert.fail('must not reconcile another membership');
    const result = room.kick('participant');
    room.generation++;
    if (rejected) ack.reject(new Error('old error'));
    else ack.resolve({ type: 'roomControlApplied', requestId: 'old' });
    await assert.rejects(result, /room session changed/);
    assert.equal(room.pendingControls.size, 0);
  }
});

test('disconnected room commands fail visibly without being queued for a later session', async () => {
  const { room, messages } = await settingsSender();
  room.signaling.connected = false;
  await assert.rejects(room.updateRoomSettings({ allowChat: false }), /Reconnect/);
  assert.deepEqual(messages, []);
});
