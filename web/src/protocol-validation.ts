import type {
  ServerMessage,
  SocialAction,
  SocialResponses,
  SocialResponse,
  ParticipantInfo,
  ProducerMetadata,
  ChatEntry,
  RoomSettings,
  RoomSnapshot,
  BanEntry,
  MemberEntry,
  ReportEntry,
  IceParameters,
  IceCandidate,
  DtlsParameters,
  IceServerEntry,
  RtpCapabilities,
  RtpParameters,
} from './protocol';
import type {
  RtpCodecCapability,
  RtpCodecParameters,
  RtpHeaderExtension,
  RtpHeaderExtensionParameters,
  RtpEncodingParameters,
  RtcpParameters,
  RtcpFeedback,
  DtlsFingerprint,
} from 'mediasoup-client/types';

import {
  type Decoder,
  type Fields,
  invalid,
  record,
  text,
  boolean,
  number,
  integer,
  choice,
  optional,
  nullable,
  list,
  object,
} from './validation';
type Variant<K extends ServerMessage['type']> = Extract<ServerMessage, { type: K }>;
function message<K extends ServerMessage['type']>(
  type: K,
  fields: Fields<Omit<Variant<K>, 'type'>>,
): Decoder<Variant<K>> {
  const decode = object(fields);
  return (value) => {
    const result = decode(value);
    if (record(value)['type'] !== type) return invalid();
    // The discriminant and every remaining field have been validated above.
    return { ...result, type } as Variant<K>;
  };
}

const mediaKind = choice('audio', 'video');
const direction = choice('sendrecv', 'sendonly', 'recvonly', 'inactive');
const priority = choice('very-low', 'low', 'medium', 'high');
const uint32 = integer(0xffffffff);
const byte = integer(255);
const producer = object<ProducerMetadata>({ id: text, kind: mediaKind, source: optional(text) });
const participant = object<ParticipantInfo>({
  id: text,
  name: text,
  producers: list(producer),
  role: text,
  authenticated: optional(boolean),
});
const chat = object<ChatEntry>({
  messageId: text,
  clientMessageId: text,
  participantId: text,
  participantName: text,
  recipientId: optional(text),
  recipientName: optional(text),
  content: text,
  sentAt: text,
});
export const decodeRoomSettings = object<RoomSettings>({
  id: text,
  displayName: text,
  passwordProtected: boolean,
  requireRegistration: boolean,
  maxParticipants: optional(integer()),
  maxBroadcasters: optional(integer()),
  allowScreenSharing: boolean,
  allowChat: boolean,
  allowVideo: boolean,
  moderated: boolean,
  inviteOnly: boolean,
  secret: boolean,
  lobbyEnabled: boolean,
  pushToTalk: boolean,
  guestsAllowed: boolean,
  guestsCanBroadcast: boolean,
  topic: optional(text),
});
const settings = decodeRoomSettings;
const lobbyEntry = object<{ participantId: string; displayName: string; authenticated: boolean }>({
  participantId: text,
  displayName: text,
  authenticated: boolean,
});
const snapshot = object<RoomSnapshot>({
  participants: list(participant),
  messages: list(chat),
  yourRole: text,
  roomSettings: nullable(optional(settings)),
  nickname: optional(text),
  textMuted: optional(boolean),
  camBanned: optional(boolean),
  canChat: optional(boolean),
  canBroadcast: optional(boolean),
  pausedProducerIds: optional(list(text)),
  localProducerIds: optional(list(text)),
  allowPrivateMessages: boolean,
  ignoredParticipantIds: list(text),
  lobby: optional(list(lobbyEntry)),
});
const ban = object<BanEntry>({
  banId: text,
  displayName: text,
  reason: optional(text),
  expiresAt: optional(text),
  authenticated: boolean,
});
const member = object<MemberEntry>({
  userId: text,
  displayName: text,
  role: text,
  online: boolean,
  authenticated: boolean,
});
const reportStatus = choice('open', 'resolved', 'dismissed');
const report = object<ReportEntry>({
  reportId: text,
  reporterId: text,
  reporterName: text,
  targetParticipantId: text,
  targetName: text,
  reason: text,
  status: reportStatus,
  createdAt: text,
  resolvedAt: optional(text),
});

const ice = object<IceParameters>({
  usernameFragment: text,
  password: text,
  iceLite: optional(boolean),
});
const candidateFields = object<
  Omit<IceCandidate, 'address' | 'ip'> & { address?: string; ip?: string }
>({
  foundation: text,
  priority: uint32,
  address: optional(text),
  ip: optional(text),
  protocol: choice('udp', 'tcp'),
  port: integer(65535),
  type: choice('host', 'srflx', 'prflx', 'relay'),
  tcpType: optional(choice('active', 'passive', 'so')),
});
const candidate: Decoder<IceCandidate> = (value) => {
  const parsed = candidateFields(value);
  // Current Rust emits address; older wire data used ip. The client API types
  // retain both names. Never invent an address when neither is present.
  const address = parsed.address ?? parsed.ip ?? invalid();
  return { ...parsed, address, ip: parsed.ip ?? address };
};
const fingerprint = object<DtlsFingerprint>({
  algorithm: choice('sha-1', 'sha-224', 'sha-256', 'sha-384', 'sha-512'),
  value: text,
});
const dtls = object<DtlsParameters>({
  role: optional(choice('auto', 'client', 'server')),
  fingerprints: list(fingerprint),
});
const iceServer = object<IceServerEntry>({
  urls: list(text),
  username: optional(text),
  credential: optional(text),
});

const feedback = object<RtcpFeedback>({ type: text, parameter: optional(text) });
const parameterMap: Decoder<Record<string, unknown>> = (value) => {
  const result: [string, string | number][] = [];
  for (const [key, entry] of Object.entries(record(value))) {
    result.push([key, typeof entry === 'string' ? entry : number(entry)]);
  }
  return Object.fromEntries(result);
};
const codecFields = {
  mimeType: text,
  clockRate: uint32,
  channels: optional(integer(65535)),
  parameters: optional(parameterMap),
  rtcpFeedback: optional(list(feedback)),
};
const capabilityCodec = object<RtpCodecCapability>({
  ...codecFields,
  kind: mediaKind,
  preferredPayloadType: byte,
});
const codec = object<RtpCodecParameters>({ ...codecFields, payloadType: byte });
const extensionUri = choice(
  'urn:ietf:params:rtp-hdrext:sdes:mid',
  'urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id',
  'urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id',
  'http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time',
  'http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01',
  'urn:ietf:params:rtp-hdrext:ssrc-audio-level',
  'https://aomediacodec.github.io/av1-rtp-spec/#dependency-descriptor-rtp-header-extension',
  'urn:3gpp:video-orientation',
  'http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time',
  'urn:ietf:params:rtp-hdrext:toffset',
  'http://www.webrtc.org/experiments/rtp-hdrext/playout-delay',
  'urn:mediasoup:params:rtp-hdrext:packet-id',
);
const capabilityExtension = object<RtpHeaderExtension>({
  kind: mediaKind,
  uri: extensionUri,
  preferredId: integer(65535),
  preferredEncrypt: optional(boolean),
  direction: optional(direction),
});
const extension = object<RtpHeaderExtensionParameters>({
  uri: extensionUri,
  id: integer(65535),
  encrypt: optional(boolean),
  parameters: optional(parameterMap),
});
const encoding = object<RtpEncodingParameters>({
  active: optional(boolean),
  ssrc: optional(uint32),
  rid: optional(text),
  codecPayloadType: optional(byte),
  rtx: optional(object<{ ssrc: number }>({ ssrc: uint32 })),
  dtx: optional(boolean),
  scalabilityMode: optional(text),
  scaleResolutionDownBy: optional(number),
  maxBitrate: optional(uint32),
  maxFramerate: optional(number),
  adaptivePtime: optional(boolean),
  priority: optional(priority),
  networkPriority: optional(priority),
});
const rtcp = object<RtcpParameters>({
  cname: optional(text),
  reducedSize: optional(boolean),
  mux: optional(boolean),
});
const capabilities = object<RtpCapabilities>({
  codecs: optional(list(capabilityCodec)),
  headerExtensions: optional(list(capabilityExtension)),
});
const rtp = object<RtpParameters>({
  codecs: list(codec),
  mid: optional(text),
  headerExtensions: optional(list(extension)),
  encodings: optional(list(encoding)),
  rtcp: optional(rtcp),
  msid: optional(text),
});

const socialDecoders: { [A in SocialAction]: Decoder<SocialResponses[A]> } = {
  setChatPreferences: object<{ allowPrivateMessages: boolean; ignoredParticipantIds: string[] }>({
    allowPrivateMessages: boolean,
    ignoredParticipantIds: list(text),
  }),
  changeNickname: object<{ nickname: string }>({ nickname: text }),
  getRoomSnapshot: snapshot,
  listRoomBans: object<{ bans: BanEntry[]; hasMore: boolean }>({
    bans: list(ban),
    hasMore: boolean,
  }),
  removeRoomBan: object<{ removed: boolean }>({ removed: boolean }),
  listRoomMembers: object<{ members: MemberEntry[]; hasMore: boolean }>({
    members: list(member),
    hasMore: boolean,
  }),
  setMemberRole: object<{ updated: boolean }>({ updated: boolean }),
  reportParticipant: object<SocialResponses['reportParticipant']>({
    reportId: text,
    status: choice('open'),
  }),
  listRoomReports: object<{ reports: ReportEntry[]; hasMore: boolean }>({
    reports: list(report),
    hasMore: boolean,
  }),
  resolveRoomReport: object<SocialResponses['resolveRoomReport']>({
    reportId: text,
    status: choice('resolved', 'dismissed'),
  }),
};
export function decodeSocialData<A extends SocialAction>(
  action: A,
  value: unknown,
): SocialResponses[A] {
  return socialDecoders[action](value);
}
const socialResponse: Decoder<Variant<'socialResponse'>> = (value) => {
  const source = record(value);
  const action = socialAction(source['action']);
  // The chosen decoder validates exactly the data associated with this action.
  // TypeScript cannot retain that correlation through a computed map lookup.
  return {
    type: 'socialResponse',
    requestId: text(source['requestId']),
    action,
    data: decodeSocialData(action, source['data']),
  } as SocialResponse;
};
const socialAction = choice(
  'setChatPreferences',
  'changeNickname',
  'getRoomSnapshot',
  'listRoomBans',
  'removeRoomBan',
  'listRoomMembers',
  'setMemberRole',
  'reportParticipant',
  'listRoomReports',
  'resolveRoomReport',
);

// Adding a ServerMessage variant fails typechecking until its decoder exists.
const messages = {
  roomJoined: message('roomJoined', {
    participantId: text,
    participants: list(participant),
    reconnectToken: text,
    yourRole: text,
    roomSettings: optional(settings),
  }),
  error: message('error', { message: text }),
  roomPasswordRequired: message('roomPasswordRequired', {}),
  roomClosed: message('roomClosed', { reason: text }),
  routerRtpCapabilities: message('routerRtpCapabilities', { rtpCapabilities: capabilities }),
  transportCreated: message('transportCreated', {
    transportId: text,
    iceParameters: ice,
    iceCandidates: list(candidate),
    dtlsParameters: dtls,
    iceServers: optional(list(iceServer)),
  }),
  transportConnected: message('transportConnected', { transportId: text }),
  producerCreated: message('producerCreated', { producerId: text }),
  consumerCreated: message('consumerCreated', {
    consumerId: text,
    producerId: text,
    kind: mediaKind,
    rtpParameters: rtp,
  }),
  participantJoined: message('participantJoined', {
    participantId: text,
    participantName: text,
    role: text,
    authenticated: boolean,
  }),
  participantLeft: message('participantLeft', { participantId: text }),
  newProducer: message('newProducer', {
    participantId: text,
    producerId: text,
    kind: mediaKind,
    source: optional(text),
  }),
  producerClosed: message('producerClosed', { producerId: text }),
  producerPaused: message('producerPaused', { producerId: text }),
  producerResumed: message('producerResumed', { producerId: text }),
  consumerResumed: message('consumerResumed', { consumerId: text }),
  consumerPaused: message('consumerPaused', { consumerId: text }),
  reconnectResult: message('reconnectResult', {
    success: boolean,
    participantId: text,
    reconnectToken: optional(text),
  }),
  iceRestarted: message('iceRestarted', { transportId: text, iceParameters: ice }),
  connectionStats: message('connectionStats', {
    availableBitrate: nullable(uint32),
    rtt: nullable(number),
  }),
  consumerLayersChanged: message('consumerLayersChanged', {
    consumerId: text,
    spatialLayer: nullable(byte),
    temporalLayer: nullable(byte),
  }),
  chatReceived: message('chatReceived', {
    messageId: text,
    clientMessageId: text,
    participantId: text,
    participantName: text,
    recipientId: optional(text),
    recipientName: optional(text),
    content: text,
    sentAt: text,
  }),
  privateMessageReceived: message('privateMessageReceived', { message: chat }),
  messageAck: message('messageAck', { clientMessageId: text, message: chat }),
  socialResponse,
  socialError: message('socialError', {
    requestId: optional(text),
    clientMessageId: optional(text),
    message: text,
  }),
  nicknameChanged: message('nicknameChanged', { participantId: text, nickname: text }),
  activeSpeaker: message('activeSpeaker', { participantId: text }),
  audioLevels: message('audioLevels', {
    levels: list(
      object<{ participantId: string; volume: number }>({
        participantId: text,
        volume: integer(127, -128),
      }),
    ),
  }),
  forceClosedProducer: message('forceClosedProducer', { producerId: text, reason: text }),
  camBanned: message('camBanned', { participantId: text }),
  camUnbanned: message('camUnbanned', { participantId: text }),
  textMuted: message('textMuted', { participantId: text }),
  textUnmuted: message('textUnmuted', { participantId: text }),
  participantKicked: message('participantKicked', { participantId: text, reason: optional(text) }),
  participantBanned: message('participantBanned', { participantId: text, reason: optional(text) }),
  roleChanged: message('roleChanged', { participantId: text, newRole: text, grantedBy: text }),
  voiceRequested: message('voiceRequested', { participantId: text, displayName: text }),
  roomSettingsChanged: message('roomSettingsChanged', { settings }),
  topicChanged: message('topicChanged', { topic: text, changedBy: text }),
  lobbyWaiting: message('lobbyWaiting', {
    roomName: text,
    topic: optional(text),
    participantCount: uint32,
  }),
  lobbyJoin: message('lobbyJoin', {
    participantId: text,
    displayName: text,
    authenticated: boolean,
  }),
  lobbyDenied: message('lobbyDenied', { reason: optional(text) }),
  lobbyAdmitted: message('lobbyAdmitted', {}),
} satisfies { [K in ServerMessage['type']]: Decoder<Variant<K>> };
const byType = new Map<string, Decoder<ServerMessage>>(Object.entries(messages));

/** Decode parsed JSON, stripping unknown fields and normalizing Rust optionals. */
export function decodeServerMessage(value: unknown): ServerMessage {
  try {
    const source = record(value);
    const decode = byType.get(text(source['type'])) ?? invalid();
    return decode(source);
  } catch {
    throw new Error('Invalid server message');
  }
}
