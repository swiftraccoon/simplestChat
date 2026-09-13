import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from '@typescript/typescript6';
import {
  validateAndNormalizeRtpCapabilities,
  validateAndNormalizeRtpParameters,
} from 'mediasoup-client/ortc';
import { loadContractModules } from './source-loader.mjs';

const { decodeServerMessage: decode } = (await loadContractModules())['./protocol-validation'];
const invalid = /Invalid server message/;
const ice = { usernameFragment: 'fixture', password: 'fixture-password', iceLite: true };
const candidate = {
  foundation: 'udp',
  priority: 1076302079,
  address: '127.0.0.1',
  protocol: 'udp',
  port: 40000,
  type: 'host',
};
const dtls = { role: 'auto', fingerprints: [{ algorithm: 'sha-256', value: 'AA:BB' }] };
const codec = {
  mimeType: 'audio/opus',
  payloadType: 111,
  clockRate: 48000,
  channels: 2,
  parameters: { useinbandfec: 1, 'sprop-stereo': '1' },
  rtcpFeedback: [{ type: 'transport-cc', parameter: '' }],
};
const extension = { uri: 'urn:ietf:params:rtp-hdrext:ssrc-audio-level', id: 10, encrypt: false };
const rtp = {
  mid: '0',
  codecs: [codec],
  headerExtensions: [extension],
  encodings: [
    {
      ssrc: 1234,
      rtx: { ssrc: 1235 },
      codecPayloadType: 111,
      dtx: false,
      scalabilityMode: 'L1T1',
      maxBitrate: 100000,
    },
  ],
  rtcp: { cname: 'fixture', reducedSize: true },
  msid: 'fixture-stream',
};
const { payloadType, ...codecCapability } = codec;
const capabilities = {
  codecs: [{ ...codecCapability, kind: 'audio', preferredPayloadType: payloadType }],
  headerExtensions: [
    {
      kind: 'audio',
      uri: extension.uri,
      preferredId: 10,
      preferredEncrypt: false,
      direction: 'sendrecv',
    },
  ],
};
const participant = {
  id: 'participant',
  name: 'Person',
  role: 'user',
  authenticated: true,
  producers: [{ id: 'producer', kind: 'audio', source: null }],
};
const chat = {
  messageId: 'message',
  clientMessageId: 'draft',
  participantId: 'participant',
  participantName: 'Person',
  content: 'Hello',
  sentAt: '2026-09-11T00:00:00Z',
};
const settings = {
  id: 'room',
  ownerId: 'owner',
  displayName: 'Room',
  passwordProtected: false,
  requireRegistration: false,
  maxParticipants: null,
  maxBroadcasters: null,
  allowScreenSharing: true,
  allowChat: true,
  allowVideo: true,
  moderated: false,
  inviteOnly: false,
  secret: false,
  lobbyEnabled: false,
  pushToTalk: false,
  guestsAllowed: true,
  guestsCanBroadcast: true,
  topic: null,
};
const snapshot = {
  participants: [participant],
  messages: [chat],
  roomSettings: settings,
  yourRole: 'user',
  nickname: 'Person',
  textMuted: false,
  camBanned: false,
  canChat: true,
  canBroadcast: true,
  pausedProducerIds: [],
  localProducerIds: [],
  allowPrivateMessages: true,
  ignoredParticipantIds: [],
  lobby: [{ participantId: 'waiting', displayName: 'Guest', authenticated: false }],
};
const ban = {
  banId: 'ban',
  displayName: 'Person',
  reason: null,
  expiresAt: null,
  authenticated: true,
};
const member = {
  userId: 'user',
  displayName: 'Person',
  role: 'user',
  online: false,
  authenticated: true,
};
const report = {
  reportId: 'report',
  reporterId: 'reporter',
  reporterName: 'Reporter',
  targetParticipantId: 'target',
  targetName: 'Target',
  reason: 'Test report',
  status: 'open',
  createdAt: '2026-09-11T00:00:00Z',
  resolvedAt: null,
};
const socialData = {
  setChatPreferences: { allowPrivateMessages: true, ignoredParticipantIds: [] },
  changeNickname: { nickname: 'Person' },
  getRoomSnapshot: snapshot,
  listRoomBans: { bans: [ban], hasMore: false },
  removeRoomBan: { removed: true },
  listRoomMembers: { members: [member], hasMore: false },
  setMemberRole: { updated: true },
  reportParticipant: { reportId: 'report', status: 'open' },
  listRoomReports: { reports: [report], hasMore: false },
  resolveRoomReport: { reportId: 'report', status: 'resolved' },
};
const fixtures = [
  {
    type: 'roomJoined',
    participantId: 'self',
    participants: [participant],
    reconnectToken: 'fixture-token',
    yourRole: 'guest',
    roomSettings: settings,
  },
  { type: 'error', message: 'Test error' },
  { type: 'roomPasswordRequired' },
  { type: 'roomClosed', reason: 'Deleted' },
  { type: 'serverRestarting', reason: 'Server shutting down' },
  { type: 'routerRtpCapabilities', rtpCapabilities: capabilities },
  {
    type: 'transportCreated',
    transportId: 'transport',
    iceParameters: ice,
    iceCandidates: [candidate],
    dtlsParameters: dtls,
    iceServers: [{ urls: ['stun:example.test'], username: 'fixture', credential: 'fixture' }],
  },
  { type: 'transportConnected', transportId: 'transport' },
  { type: 'producerCreated', producerId: 'producer' },
  {
    type: 'consumerCreated',
    consumerId: 'consumer',
    producerId: 'producer',
    kind: 'audio',
    rtpParameters: rtp,
  },
  {
    type: 'participantJoined',
    participantId: 'participant',
    participantName: 'Person',
    role: 'guest',
    authenticated: false,
  },
  { type: 'participantLeft', participantId: 'participant' },
  {
    type: 'newProducer',
    participantId: 'participant',
    producerId: 'producer',
    kind: 'video',
    source: null,
  },
  ...['producerClosed', 'producerPaused', 'producerResumed'].map((type) => ({
    type,
    producerId: 'producer',
  })),
  ...['consumerResumed', 'consumerPaused'].map((type) => ({ type, consumerId: 'consumer' })),
  { type: 'reconnectResult', success: false, participantId: '' },
  { type: 'iceRestarted', transportId: 'transport', iceParameters: ice },
  { type: 'connectionStats', availableBitrate: null, rtt: null },
  {
    type: 'consumerLayersChanged',
    consumerId: 'consumer',
    spatialLayer: null,
    temporalLayer: null,
  },
  { type: 'chatReceived', ...chat },
  {
    type: 'privateMessageReceived',
    message: { ...chat, recipientId: 'self', recipientName: 'Recipient' },
  },
  { type: 'messageAck', clientMessageId: 'draft', message: chat },
  { type: 'socialResponse', requestId: 'request', action: 'getRoomSnapshot', data: snapshot },
  { type: 'socialError', requestId: 'request', clientMessageId: 'draft', message: 'Test error' },
  { type: 'nicknameChanged', participantId: 'participant', nickname: 'New name' },
  { type: 'activeSpeaker', participantId: 'participant' },
  { type: 'audioLevels', levels: [{ participantId: 'participant', volume: -127 }] },
  { type: 'forceClosedProducer', producerId: 'producer', reason: 'Closed' },
  ...['camBanned', 'camUnbanned', 'textMuted', 'textUnmuted'].map((type) => ({
    type,
    participantId: 'participant',
  })),
  ...['participantKicked', 'participantBanned'].map((type) => ({
    type,
    participantId: 'participant',
    reason: null,
  })),
  { type: 'roleChanged', participantId: 'participant', newRole: 'user', grantedBy: 'moderator' },
  { type: 'voiceRequested', participantId: 'participant', displayName: 'Person' },
  { type: 'roomSettingsChanged', settings },
  { type: 'topicChanged', topic: '', changedBy: 'moderator' },
  { type: 'lobbyWaiting', roomName: 'Room', topic: null, participantCount: 1 },
  { type: 'lobbyJoin', participantId: 'participant', displayName: 'Guest', authenticated: false },
  { type: 'lobbyDenied', reason: null },
  { type: 'lobbyAdmitted' },
];

test('fixtures cover every ServerMessage discriminant and every SocialAction', async () => {
  const source = ts.createSourceFile(
    'protocol.ts',
    await readFile(new URL('../src/protocol.ts', import.meta.url), 'utf8'),
    ts.ScriptTarget.Latest,
    true,
  );
  const aliases = new Map(
    source.statements.filter(ts.isTypeAliasDeclaration).map((node) => [node.name.text, node.type]),
  );
  const types = aliases.get('ServerMessage').types.map((node) => {
    if (ts.isTypeReferenceNode(node) && node.typeName.getText(source) === 'SocialResponse')
      return 'socialResponse';
    let literal = node;
    while (ts.isParenthesizedTypeNode(literal) || ts.isIntersectionTypeNode(literal)) {
      literal = ts.isParenthesizedTypeNode(literal) ? literal.type : literal.types[0];
    }
    return literal.members.find((member) => member.name?.getText(source) === 'type').type.literal
      .text;
  });
  assert.deepEqual(fixtures.map((value) => value.type).sort(), types.sort());
  assert.deepEqual(
    Object.keys(socialData).sort(),
    aliases
      .get('SocialAction')
      .types.map((node) => node.literal.text)
      .sort(),
  );
});

for (const fixture of fixtures) {
  test(`decodes valid Rust-shaped ${fixture.type} without mutating wire data`, () => {
    const before = structuredClone(fixture);
    const result = decode(fixture);
    assert.equal(result.type, fixture.type);
    assert.deepEqual(fixture, before);
    assert.notEqual(result, fixture);
    assert.deepEqual(decode(result), result, 'normalization is idempotent');
  });
}

test('social actions validate their own result contract, including report lifecycle status', () => {
  const response = (action, data) => ({
    type: 'socialResponse',
    requestId: 'request',
    action,
    data,
  });
  for (const [action, data] of [
    ['getRoomSnapshot', socialData.listRoomMembers],
    ['listRoomMembers', socialData.listRoomBans],
    ['listRoomReports', socialData.listRoomMembers],
    ['changeNickname', socialData.removeRoomBan],
    ['removeRoomBan', socialData.changeNickname],
    ['reportParticipant', { reportId: 'report', status: 'resolved' }],
    ['resolveRoomReport', { reportId: 'report', status: 'open' }],
  ])
    assert.throws(() => decode(response(action, data)), invalid);
  assert.equal(
    decode(response('reportParticipant', { reportId: 'report', status: 'open' })).data.status,
    'open',
  );
  for (const status of ['resolved', 'dismissed']) {
    assert.equal(
      decode(response('resolveRoomReport', { reportId: 'report', status })).data.status,
      status,
    );
  }
});

for (const [action, data] of Object.entries(socialData)) {
  test(`validates nested social data for ${action}`, () => {
    const value = { type: 'socialResponse', requestId: 'request', action, data };
    assert.equal(decode(value).action, action);
    assert.throws(() => decode({ ...value, data: [] }), invalid);
    assert.throws(() => decode({ ...value, data: {} }), invalid);
  });
}

test('normalizes Rust null optionals without losing meaningful false, zero, or empty strings', () => {
  const room = decode(fixtures.find((value) => value.type === 'roomJoined'));
  assert.equal(Object.hasOwn(room.roomSettings, 'maxParticipants'), false);
  assert.equal(Object.hasOwn(room.roomSettings, 'maxBroadcasters'), false);
  assert.equal(Object.hasOwn(room.roomSettings, 'topic'), false);
  assert.equal(Object.hasOwn(room.participants[0].producers[0], 'source'), false);
  assert.equal(Object.hasOwn(room.roomSettings, 'ownerId'), false, 'unknown fields are stripped');
  assert.deepEqual(decode({ type: 'connectionStats', availableBitrate: 0, rtt: 0 }), {
    type: 'connectionStats',
    availableBitrate: 0,
    rtt: 0,
  });
  assert.deepEqual(decode({ type: 'topicChanged', topic: '', changedBy: '' }), {
    type: 'topicChanged',
    topic: '',
    changedBy: '',
  });
  assert.deepEqual(decode({ type: 'reconnectResult', success: false, participantId: '' }), {
    type: 'reconnectResult',
    success: false,
    participantId: '',
  });
});

test('accepts current address-only and legacy ip-only ICE candidates, but requires an address', () => {
  const transport = fixtures.find((value) => value.type === 'transportCreated');
  assert.deepEqual(decode(transport).iceCandidates[0], { ...candidate, ip: candidate.address });
  const { address, ...legacy } = candidate;
  assert.deepEqual(
    decode({ ...transport, iceCandidates: [{ ...legacy, ip: address }] }).iceCandidates[0],
    { ...candidate, ip: address },
  );
  assert.throws(() => decode({ ...transport, iceCandidates: [legacy] }), invalid);
});

test('decoded media remains accepted by the installed client validators', () => {
  const caps = decode(
    fixtures.find((value) => value.type === 'routerRtpCapabilities'),
  ).rtpCapabilities;
  const parameters = decode(
    fixtures.find((value) => value.type === 'consumerCreated'),
  ).rtpParameters;
  assert.doesNotThrow(() => validateAndNormalizeRtpCapabilities(caps));
  assert.doesNotThrow(() => validateAndNormalizeRtpParameters(parameters));
  assert.equal(parameters.encodings[0].rtx.ssrc, 1235);
  assert.equal(caps.codecs[0].parameters.useinbandfec, 1);
});

test('video VP8/RTX parameters and an ad-hoc snapshot with null settings remain valid', () => {
  const codecs = [
    {
      mimeType: 'video/VP8',
      payloadType: 100,
      clockRate: 90000,
      parameters: {},
      rtcpFeedback: [{ type: 'nack', parameter: 'pli' }],
    },
    {
      mimeType: 'video/rtx',
      payloadType: 101,
      clockRate: 90000,
      parameters: { apt: 100 },
      rtcpFeedback: [],
    },
  ];
  const value = {
    type: 'consumerCreated',
    consumerId: 'consumer',
    producerId: 'producer',
    kind: 'video',
    rtpParameters: {
      codecs,
      encodings: [{ ssrc: 1234, rtx: { ssrc: 1235 }, scalabilityMode: 'L3T3' }],
      rtcp: { reducedSize: true },
    },
  };
  const result = decode(value);
  assert.deepEqual(result.rtpParameters.codecs, codecs);
  assert.doesNotThrow(() => validateAndNormalizeRtpParameters(result.rtpParameters));
  const restored = decode({
    type: 'socialResponse',
    requestId: 'request',
    action: 'getRoomSnapshot',
    data: { ...snapshot, roomSettings: null },
  });
  assert.equal(restored.data.roomSettings, null);
});

test('rejects missing or mistyped required top-level fields for every variant', () => {
  const optionals = {
    roomJoined: ['roomSettings'],
    transportCreated: ['iceServers'],
    newProducer: ['source'],
    reconnectResult: ['reconnectToken'],
    chatReceived: ['recipientId', 'recipientName'],
    socialError: ['requestId', 'clientMessageId'],
    participantKicked: ['reason'],
    participantBanned: ['reason'],
    lobbyWaiting: ['topic'],
    lobbyDenied: ['reason'],
  };
  for (const fixture of fixtures) {
    for (const key of Object.keys(fixture)) {
      if (optionals[fixture.type]?.includes(key)) continue;
      const missing = { ...fixture };
      delete missing[key];
      assert.throws(() => decode(missing), invalid, `${fixture.type}.${key} is required`);
      assert.throws(
        () => decode({ ...fixture, [key]: Array.isArray(fixture[key]) ? {} : [] }),
        invalid,
        `${fixture.type}.${key} rejects the wrong container`,
      );
    }
  }
});

test('rejects malformed nested participants, settings, messages, media and social pages', () => {
  const cases = [
    ['roomJoined', ['participants', 0, 'name'], 42],
    ['roomJoined', ['participants', 0, 'producers', 0, 'kind'], 'data'],
    ['roomJoined', ['participants', 0, 'authenticated'], 'true'],
    ['roomJoined', ['roomSettings', 'allowChat'], 'false'],
    ['roomJoined', ['roomSettings', 'maxParticipants'], 1.5],
    ['transportCreated', ['iceParameters', 'password'], false],
    ['transportCreated', ['iceCandidates', 0, 'port'], 65536],
    ['transportCreated', ['iceCandidates', 0, 'protocol'], 'quic'],
    ['transportCreated', ['dtlsParameters', 'fingerprints', 0, 'algorithm'], 'unknown'],
    ['transportCreated', ['dtlsParameters', 'fingerprints', 0, 'value'], 123],
    ['transportCreated', ['iceServers', 0, 'urls'], 'stun:example.test'],
    ['consumerCreated', ['rtpParameters', 'codecs', 0, 'clockRate'], '48000'],
    ['consumerCreated', ['rtpParameters', 'codecs', 0, 'parameters', 'useinbandfec'], {}],
    ['consumerCreated', ['rtpParameters', 'codecs', 0, 'rtcpFeedback', 0, 'type'], null],
    ['consumerCreated', ['rtpParameters', 'encodings', 0, 'rtx', 'ssrc'], '1235'],
    ['consumerCreated', ['rtpParameters', 'headerExtensions', 0, 'uri'], 'unknown'],
    ['consumerCreated', ['rtpParameters', 'rtcp', 'reducedSize'], 'true'],
    ['routerRtpCapabilities', ['rtpCapabilities', 'codecs', 0, 'kind'], false],
    ['routerRtpCapabilities', ['rtpCapabilities', 'headerExtensions', 0, 'direction'], 'sideways'],
    ['privateMessageReceived', ['message', 'content'], {}],
    ['messageAck', ['message', 'sentAt'], 123],
    ['audioLevels', ['levels', 0, 'volume'], Number.NaN],
    ['socialResponse', ['data', 'messages', 0, 'content'], 123],
    ['socialResponse', ['data', 'participants', 0, 'producers'], false],
    ['socialResponse', ['data', 'lobby', 0, 'authenticated'], 'false'],
  ];
  for (const [type, path, value] of cases) {
    const fixture = structuredClone(fixtures.find((entry) => entry.type === type));
    let target = fixture;
    for (const key of path.slice(0, -1)) target = target[key];
    target[path.at(-1)] = value;
    assert.throws(() => decode(fixture), invalid, `${type}.${path.join('.')}`);
  }
  for (const [action, data] of [
    ['listRoomBans', { bans: [{ ...ban, authenticated: 1 }], hasMore: false }],
    ['listRoomMembers', { members: [{ ...member, online: 'false' }], hasMore: false }],
    ['listRoomReports', { reports: [{ ...report, status: 'pending' }], hasMore: false }],
    ['setChatPreferences', { allowPrivateMessages: true, ignoredParticipantIds: [1] }],
  ])
    assert.throws(
      () => decode({ type: 'socialResponse', requestId: 'request', action, data }),
      invalid,
    );
});

test('unknown messages and non-JSON primitive shapes produce fixed payload-free errors', () => {
  for (const value of [
    null,
    [],
    true,
    1,
    'payload-sentinel',
    {},
    { type: 'payload-sentinel' },
    { type: 'error', message: { secret: 'payload-sentinel' } },
    { type: 'socialResponse', action: 'unknown', requestId: 'request', data: {} },
  ]) {
    assert.throws(
      () => decode(value),
      (error) => error.message === 'Invalid server message',
    );
  }
});
