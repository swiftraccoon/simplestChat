import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function flush() { for (let i = 0; i < 12; i++) await Promise.resolve(); }

async function harness(options = {}) {
  const sent = [], instances = [], social = [], tracks = [], removed = [], recovery = [];
  const records = { admissions: 0, localChanges: 0, publicChats: [], departures: [] };
  const timers = new Map();
  let timerId = 0;
  class Media {
    tracks = new Map();
    closedConsumers = [];
    closes = 0;
    audioEnabled = false;
    videoEnabled = false;
    isScreenSharing = false;
    reconciled = [];
    constructor() { instances.push(this); }
    async setup() { await options.setup?.(this); }
    async consume(id) {
      const track = await (options.consume?.(id, this) ?? { id });
      this.tracks.set(id, track);
      return track;
    }
    close() { this.closes++; this.tracks.clear(); }
    closeConsumerByProducer(id) { this.closedConsumers.push(id); this.tracks.delete(id); }
    getConsumerTrackByProducer(id) { return this.tracks.get(id) ?? null; }
    closeLocalProducer() { return false; }
    reconcileLocalProducers(ids) { this.reconciled.push(ids); return options.reconcileChanged ?? false; }
    setConsumerHiddenByProducer() {}
    setConsumerQualityByProducer() { return true; }
    setCapturePreferences() {}
  }
  const signaling = {
    connected: true,
    setOnMessage(handler) { this.onMessage = handler; },
    setOnReconnected(handler) { this.onReconnected = handler; },
    send(message) {
      sent.push(message);
      if (options.sendError?.(message)) throw new Error('Send failed');
      if (message.type === 'joinRoom' && options.autoJoin !== false) {
        const reply = options.joinReply?.(message) ?? {
          type: 'roomJoined', participantId: 'local', participants: [],
          yourRole: options.role ?? 'user', reconnectToken: 'reconnect-token', roomSettings: options.settings,
        };
        queueMicrotask(() => this.onMessage(reply));
      }
    },
    async request(message) {
      sent.push(message);
      return await (options.reconnect?.(message) ?? {
        type: 'reconnectResult', success: true, participantId: 'local', reconnectToken: 'rotated-token',
      });
    },
  };
  const events = {
    onParticipantsChanged() {}, onLocalStream() {},
    onLocalMediaChanged() { records.localChanges++; },
    onRemoteTrack(...args) { tracks.push(args); },
    onRemoteTrackRemoved(...args) { removed.push(args); },
    onParticipantLeft(id) { records.departures.push(id); },
    onParticipantJoined() {},
    onChatMessage(...args) { records.publicChats.push(args); },
    onConnectionQuality() {}, onActiveSpeaker() {}, onAudioLevels() {},
    onModeration() {}, onRoleChanged() {}, onRoomSettingsChanged() {},
    onTopicChanged() {}, onVoiceRequested() {}, onLobbyWaiting() {},
    onLobbyJoin() {}, onLobbyAdmitted() {}, onLobbyDenied() {},
    onAdmissionComplete() { records.admissions++; },
    onSocialEvent(message) { social.push(message); },
    onRecoveryState(...args) { recovery.push(args); },
    ...(options.events ?? {}),
  };
  const globals = { crypto: { randomUUID } };
  if (options.fakeTimers) {
    globals.setTimeout = (fn) => { const id = ++timerId; timers.set(id, fn); return id; };
    globals.clearTimeout = id => { timers.delete(id); };
  }
  const { RoomClient } = await loadTypeScript('src/room.ts', {
    modules: { './media': { MediaManager: Media } }, globals,
  });
  const room = new RoomClient(signaling, events);
  const reply = message => signaling.onMessage(message);
  const respond = (request, data) => reply({ type: 'socialResponse', requestId: request.requestId, action: request.type, data });
  return { room, signaling, sent, instances, social, tracks, removed, recovery, records, timers, reply, respond };
}

function snapshot(overrides = {}) {
  return { participants: [], messages: [], yourRole: 'user', roomSettings: null,
    nickname: 'Local', allowPrivateMessages: true, ignoredParticipantIds: [],
    textMuted: false, camBanned: false, canChat: true, canBroadcast: true,
    pausedProducerIds: [], localProducerIds: [], ...overrides };
}

test('room closure releases media and pending actions and cannot reconnect', async () => {
  const closed = [];
  const h = await harness({ events: { onRoomClosed: reason => closed.push(reason) } });
  await h.room.join('deleted-room', 'Local');
  const pending = h.room.requestSocial('listRoomMembers');
  const rejected = assert.rejects(pending, /Room left/);
  h.reply({ type: 'roomClosed', reason: 'Room deleted' });
  await rejected;
  assert.equal(h.room.localParticipantId, null);
  assert.equal(h.room.currentRoomId, null);
  assert.equal(h.instances[0].closes, 1);
  assert.deepEqual(closed, ['Room deleted']);
  const before = h.sent.length;
  await h.signaling.onReconnected();
  assert.equal(h.sent.length, before, 'closed membership must not resume');
});

test('room closure also clears a waiting lobby membership', async () => {
  const closed = [];
  const h = await harness({ joinReply: () => ({ type: 'lobbyWaiting', roomName: 'Waiting', participantCount: 1 }),
    events: { onRoomClosed: reason => closed.push(reason) } });
  assert.equal(await h.room.join('deleted-lobby', 'Local'), 'lobby');
  h.reply({ type: 'roomClosed', reason: 'Room deleted' });
  assert.equal(h.room.currentRoomId, null);
  assert.deepEqual(closed, ['Room deleted']);
});

test('social responses correlate by request ID and action even when replies arrive out of order', async () => {
  const h = await harness(); await h.room.join('room', 'Local');
  const first = h.room.requestSocial('listRoomBans');
  const firstRequest = h.sent.at(-1);
  const second = h.room.requestSocial('listRoomMembers');
  const secondRequest = h.sent.at(-1);
  h.reply({ type: 'socialResponse', requestId: firstRequest.requestId, action: 'listRoomMembers', data: { wrong: true } });
  assert.equal(h.room.socialRequests.size, 2);
  h.respond(secondRequest, { members: [], hasMore: false });
  assert.deepEqual(await second, { members: [], hasMore: false });
  h.respond(firstRequest, { bans: [], hasMore: false });
  assert.deepEqual(await first, { bans: [], hasMore: false });
  assert.equal(h.room.socialRequests.size, 0);
  h.respond(firstRequest, { bans: [{ stale: true }] });
  assert.equal(h.social.length, 2, 'unsolicited or duplicate replies do not update UI state');
});

test('social timeouts, send errors, server errors and leave remove pending requests', async () => {
  const h = await harness({ fakeTimers: true, sendError: message => message.type === 'changeNickname' });
  await h.room.join('room', 'Local');
  const pending = h.room.requestSocial('listRoomBans');
  const timeout = h.timers.values().next().value; h.timers.clear(); timeout();
  await assert.rejects(pending, /did not respond/); assert.equal(h.room.socialRequests.size, 0);
  await assert.rejects(h.room.requestSocial('changeNickname', { nickname: 'new' }), /Send failed/);
  assert.equal(h.room.socialRequests.size, 0); assert.equal(h.timers.size, 0);
  const rejected = h.room.requestSocial('listRoomReports'); const request = h.sent.at(-1);
  h.reply({ type: 'socialError', requestId: request.requestId, message: 'Not a moderator' });
  await assert.rejects(rejected, /Not a moderator/);
  const abandoned = h.room.requestSocial('listRoomMembers');
  const rejection = assert.rejects(abandoned, /Room left/);
  await h.room.leave(); await rejection;
  assert.equal(h.room.socialRequests.size, 0); assert.equal(h.timers.size, 0);
});

test('private messages and acknowledgments dispatch only through the social event hook', async () => {
  const h = await harness(); await h.room.join('room', 'Local');
  const message = { messageId: 'message', clientMessageId: 'draft', participantId: 'remote', participantName: 'Remote',
    recipientId: 'local', recipientName: 'Local', content: 'private', sentAt: new Date().toISOString() };
  h.reply({ type: 'privateMessageReceived', message });
  h.reply({ type: 'messageAck', clientMessageId: 'draft', message });
  assert.equal(h.social.length, 2); assert.deepEqual(h.records.publicChats, []);
  h.room.sendPrivate('remote', 'outgoing', 'client-id');
  assert.deepEqual(h.sent.at(-1), { type: 'privateMessage', targetParticipantId: 'remote', content: 'outgoing', clientMessageId: 'client-id' });
});

test('snapshot restores nickname, role, sanctions and server permissions before social delivery', async () => {
  const h = await harness({ reconcileChanged: true }); await h.room.join('room', 'Old nickname');
  const request = h.room.requestSocial('getRoomSnapshot');
  h.respond(h.sent.at(-1), snapshot({ nickname: 'New nickname', yourRole: 'member', textMuted: true,
    camBanned: true, canChat: false, canBroadcast: false, localProducerIds: ['approved'] }));
  await request;
  assert.equal(h.room.nickname, 'New nickname'); assert.equal(h.room.role, 'member');
  assert.equal(h.room.textMuted, true); assert.equal(h.room.camBanned, true);
  assert.equal(h.room.canChat, false); assert.equal(h.room.canBroadcast, false);
  assert.deepEqual(h.instances[0].reconciled, [['approved']]); assert.equal(h.records.localChanges, 1);
  assert.throws(() => h.room.sendChat('blocked'), /not allowed/);
  h.reply({ type: 'textUnmuted', participantId: 'local' });
  assert.equal(h.room.canChat, true, 'a later moderation event invalidates stale permission snapshots');
  h.reply({ type: 'roleChanged', participantId: 'local', newRole: 'user' });
  h.reply({ type: 'roomSettingsChanged', settings: { moderated: true, allowChat: true, guestsCanBroadcast: true } });
  assert.equal(h.room.canChat, false);
  h.reply({ type: 'roleChanged', participantId: 'local', newRole: 'member' });
  assert.equal(h.room.canChat, true);
});

test('moderated-room chat is gated before any snapshot, and nickname broadcasts update local state', async () => {
  const h = await harness({ role: 'guest', settings: { moderated: true, allowChat: true } });
  await h.room.join('room', 'Guest');
  assert.equal(h.room.canChat, false);
  h.reply({ type: 'nicknameChanged', participantId: 'local', nickname: 'Alias' });
  assert.equal(h.room.nickname, 'Alias');
  h.reply({ type: 'roleChanged', participantId: 'local', newRole: 'member' });
  h.room.sendChat('allowed', 'draft'); assert.equal(h.sent.at(-1).type, 'chatMessage');
});

test('snapshot reconciles paused tracks and removes departed participants without duplicate consumers', async () => {
  const h = await harness(); await h.room.join('room', 'Local');
  h.reply({ type: 'participantJoined', participantId: 'remote', participantName: 'Remote', role: 'user', authenticated: true });
  h.reply({ type: 'newProducer', participantId: 'remote', producerId: 'camera', kind: 'video', source: 'camera' });
  await flush(); assert.equal(h.tracks.length, 1);
  const remote = { id: 'remote', name: 'Renamed', role: 'member', authenticated: true, producers: [{ id: 'camera', kind: 'video', source: 'camera' }] };
  let request = h.room.requestSocial('getRoomSnapshot');
  h.respond(h.sent.at(-1), snapshot({ participants: [remote], pausedProducerIds: ['camera'] })); await request;
  assert.equal(h.removed.length, 1); assert.equal(h.room.getParticipants().get('remote').name, 'Renamed');
  request = h.room.requestSocial('getRoomSnapshot');
  h.respond(h.sent.at(-1), snapshot({ participants: [remote] })); await request;
  assert.equal(h.tracks.length, 2, 'resume renders the already-created consumer');
  request = h.room.requestSocial('getRoomSnapshot'); h.respond(h.sent.at(-1), snapshot()); await request;
  assert.equal(h.room.getParticipants().size, 0);
  assert.deepEqual(h.instances[0].closedConsumers, ['camera']);
});

test('consumer jobs serialize and late completion cannot restore a departed participant', async () => {
  const gate = deferred(), consumed = [];
  const h = await harness({ consume: id => { consumed.push(id); return id === 'first' ? gate.promise : { id }; } });
  await h.room.join('room', 'Local');
  h.reply({ type: 'participantJoined', participantId: 'remote', participantName: 'Remote', role: 'user', authenticated: true });
  h.reply({ type: 'newProducer', participantId: 'remote', producerId: 'first', kind: 'audio' });
  h.reply({ type: 'newProducer', participantId: 'remote', producerId: 'first', kind: 'audio' });
  h.reply({ type: 'newProducer', participantId: 'remote', producerId: 'second', kind: 'video' });
  await flush(); assert.deepEqual(consumed, ['first']);
  const request = h.room.requestSocial('getRoomSnapshot');
  h.respond(h.sent.at(-1), snapshot({ participants: [{ id: 'remote', name: 'Remote', role: 'user', authenticated: true,
    producers: [{ id: 'first', kind: 'audio' }, { id: 'second', kind: 'video' }] }] }));
  await request;
  h.reply({ type: 'participantLeft', participantId: 'remote' });
  gate.resolve({ id: 'late-track' }); await flush();
  assert.deepEqual(consumed, ['first']); assert.deepEqual(h.tracks, []);
  assert.ok(h.instances[0].closedConsumers.includes('first'));
});

test('a snapshot response cannot resurrect media after leaving while consumption is pending', async () => {
  const gate = deferred(); const h = await harness({ consume: () => gate.promise });
  await h.room.join('room', 'Local');
  const request = h.room.requestSocial('getRoomSnapshot');
  const wireRequest = h.sent.at(-1);
  h.respond(wireRequest, snapshot({ participants: [{ id: 'remote', name: 'Remote', role: 'user', authenticated: true,
    producers: [{ id: 'camera', kind: 'video' }] }] })); await request; await flush();
  await h.room.leave(); gate.resolve({ id: 'late' }); await flush();
  h.respond(wireRequest, snapshot({ nickname: 'stale' }));
  assert.equal(h.room.localParticipantId, null); assert.equal(h.room.nickname, '');
  assert.equal(h.room.hasMedia, false); assert.deepEqual(h.tracks, []);
});

test('snapshot errors do not force a successfully resumed room to rejoin', async () => {
  const h = await harness(); await h.room.join('room', 'Local', 'private-password');
  const reconnect = h.room.attemptReconnect(); await flush();
  const request = h.sent.find(message => message.type === 'getRoomSnapshot'); assert.ok(request);
  h.reply({ type: 'socialError', requestId: request.requestId, message: 'Room requests are rate limited' });
  await reconnect;
  assert.equal(h.sent.filter(message => message.type === 'joinRoom').length, 1);
  assert.equal(h.instances[0].closes, 0); assert.equal(h.room.connected, true);
  assert.equal(h.room.reconnectToken, 'rotated-token'); assert.equal(h.recovery.at(-1)[0], 'connected');
});

test('malformed snapshot rejects its request without partially replacing room state', async () => {
  const h = await harness(); await h.room.join('room', 'Local');
  const request = h.room.requestSocial('getRoomSnapshot');
  h.respond(h.sent.at(-1), { participants: [{ id: 'broken' }], yourRole: 'owner', nickname: 'Invalid' });
  await assert.rejects(request, /incomplete/);
  assert.equal(h.room.role, 'user'); assert.equal(h.room.nickname, 'Local'); assert.equal(h.social.length, 0);
});

test('full reconnect retains the room password in memory and prompts for a replacement challenge', async () => {
  let joins = 0, prompts = 0;
  const h = await harness({
    reconnect: () => ({ type: 'reconnectResult', success: false, participantId: 'local' }),
    joinReply: () => ++joins === 2 ? { type: 'roomPasswordRequired' } : undefined,
    events: { onPasswordRequired: async () => { prompts++; return 'replacement-password'; } },
  });
  await h.room.join('room', 'Local', 'remembered-password'); await h.room.attemptReconnect();
  const requests = h.sent.filter(message => message.type === 'joinRoom');
  assert.equal(requests[1].password, 'remembered-password'); assert.equal(requests[2].password, 'replacement-password');
  assert.equal(prompts, 1); assert.equal(h.records.admissions, 1); assert.equal(h.room.connected, true);
  await h.room.leave(); assert.equal(h.room.joinPassword, undefined);
});

test('leaving during the reconnect password prompt prevents a late answer from joining', async () => {
  const answer = deferred(); let joins = 0;
  const h = await harness({
    reconnect: () => ({ type: 'reconnectResult', success: false, participantId: 'local' }),
    joinReply: () => ++joins === 2 ? { type: 'roomPasswordRequired' } : undefined,
    events: { onPasswordRequired: () => answer.promise },
  });
  await h.room.join('room', 'Local'); const reconnect = h.room.attemptReconnect(); await flush();
  await h.room.leave(); answer.resolve('too-late'); await reconnect;
  assert.equal(h.sent.filter(message => message.type === 'joinRoom').length, 2);
  assert.equal(h.room.localParticipantId, null); assert.equal(h.records.admissions, 0);
  assert.ok(h.recovery.every(([state]) => state !== 'failed'));
});

test('leave during media setup cancels joining and closes the captured manager', async () => {
  const gate = deferred(); const h = await harness({ setup: () => gate.promise });
  const joining = h.room.join('room', 'Local'); const rejected = assert.rejects(joining, /cancelled/); await flush();
  await h.room.leave(); gate.resolve(); await rejected;
  assert.equal(h.room.hasMedia, false); assert.equal(h.room.localParticipantId, null);
  assert.ok(h.instances[0].closes >= 1); assert.equal(h.records.admissions, 0);
});

test('an older setup result cannot close or replace a newer room membership', async () => {
  const first = deferred(); let setups = 0;
  const h = await harness({ setup: () => ++setups === 1 ? first.promise : undefined });
  const oldJoin = h.room.join('old-room', 'Old'); const rejection = assert.rejects(oldJoin, /cancelled/); await flush();
  await h.room.join('new-room', 'New'); first.resolve(); await rejection;
  assert.equal(h.room.currentRoomId, 'new-room'); assert.equal(h.room.nickname, 'New');
  assert.equal(h.room.hasMedia, true); assert.equal(h.instances[1].closes, 0);
});

test('lobby admission cannot complete after leaving during media setup', async () => {
  const gate = deferred();
  const h = await harness({ setup: () => gate.promise, joinReply: () => ({ type: 'lobbyWaiting', roomName: 'Room', participantCount: 1 }) });
  assert.equal(await h.room.join('room', 'Local'), 'lobby');
  h.reply({ type: 'lobbyAdmitted' });
  h.reply({ type: 'roomJoined', participantId: 'local', participants: [], yourRole: 'user', reconnectToken: 'admitted' });
  await flush(); await h.room.leave(); gate.resolve(); await flush();
  assert.equal(h.records.admissions, 0); assert.equal(h.room.hasMedia, false);
});

test('join timeout restores the normal handler and allows a later independent join', async () => {
  const h = await harness({ fakeTimers: true, autoJoin: false });
  const handler = h.signaling.onMessage;
  const joining = h.room.join('room', 'Local');
  const timeout = h.timers.values().next().value; h.timers.clear(); timeout();
  await assert.rejects(joining, /Timeout/); assert.equal(h.signaling.onMessage, handler);
  const retry = h.room.join('room', 'Local');
  h.reply({ type: 'roomJoined', participantId: 'local', participants: [], yourRole: 'user', reconnectToken: 'fresh' });
  await retry; assert.equal(h.signaling.onMessage, handler); assert.equal(h.timers.size, 0);
});
