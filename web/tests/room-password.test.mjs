import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function roomWithReplies(replies, telemetry = () => {}) {
  const sent = [];
  const signaling = {
    setOnMessage(handler) {
      this.onMessage = handler;
    },
    setOnReconnected() {},
    setOnReconnectFailed() {},
    setOnConnectionLost() {},
    completeRestartRecovery() {},
    send(message) {
      sent.push(message);
      const reply = replies.shift();
      assert.ok(reply, 'each join must have a server reply');
      queueMicrotask(() => this.onMessage(reply));
    },
  };
  const { RoomClient, RoomPasswordRequiredError } = await loadTypeScript('src/room.ts', {
    modules: {
      './media': {
        MediaManager: class {
          async setup() {}
        },
      },
    },
  });
  const events = {
    onTelemetry: telemetry,
    onParticipantsChanged() {},
    onLobbyWaiting() {},
    onLobbyDenied() {},
  };
  return { room: new RoomClient(signaling, events), sent, signaling, RoomPasswordRequiredError };
}

test('password challenge permits a join retry carrying the entered password', async () => {
  const { room, sent, RoomPasswordRequiredError } = await roomWithReplies([
    { type: 'roomPasswordRequired' },
    {
      type: 'roomJoined',
      participantId: 'guest',
      participants: [],
      reconnectToken: 'session-token',
      yourRole: 'guest',
    },
  ]);
  await assert.rejects(room.join('private-room', 'Guest'), (error) => {
    assert.ok(error instanceof RoomPasswordRequiredError);
    return true;
  });
  assert.equal(await room.join('private-room', 'Guest', 'correct-password'), 'joined');
  assert.equal(sent.length, 2);
  assert.equal(sent[0].password, undefined);
  assert.equal(sent[1].password, 'correct-password');
  assert.equal(room.localParticipantId, 'guest');
});

test('ordinary join errors do not prompt for a password based on their text', async () => {
  const { room, RoomPasswordRequiredError } = await roomWithReplies([
    { type: 'error', message: 'This room requires a password' },
  ]);
  await assert.rejects(room.join('private-room', 'Guest'), (error) => {
    assert.ok(error instanceof Error);
    assert.ok(!(error instanceof RoomPasswordRequiredError));
    return true;
  });
  assert.equal(room.localParticipantId, null);
});

test('a waiting-room entry does not count as a successful joined room and denial completes its separate duration', async () => {
  const events = [];
  const f = await roomWithReplies(
    [{ type: 'lobbyWaiting', roomName: 'Private room', participantCount: 1 }],
    (event) => events.push(event),
  );
  assert.equal(await f.room.join('room', 'Guest'), 'lobby');
  assert.ok(events.some((event) => event.name === 'room_join' && event.outcome === 'waiting'));
  assert.equal(
    events.some((event) => event.name === 'room_join' && event.outcome === 'ok'),
    false,
  );
  assert.ok(events.some((event) => event.name === 'room_admission' && event.outcome === 'started'));
  f.signaling.onMessage({ type: 'lobbyDenied' });
  assert.equal(events.at(-1).name, 'room_admission');
  assert.equal(events.at(-1).outcome, 'denied');
  assert.ok(events.at(-1).durationMs >= 0);
  assert.equal(JSON.stringify(events).includes('Private room'), false);
});

test('a failed telemetry observer cannot interrupt a normal room join', async () => {
  const f = await roomWithReplies(
    [
      {
        type: 'roomJoined',
        participantId: 'guest',
        participants: [],
        reconnectToken: 'session-token',
        yourRole: 'guest',
      },
    ],
    () => {
      throw new Error('collector failed');
    },
  );
  assert.equal(await f.room.join('room', 'Guest'), 'joined');
});
