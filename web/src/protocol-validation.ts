import type {
  ServerMessage,
  SocialAction,
  SocialResponses,
  SocialResponse,
  ParticipantInfo,
  ChatStyle,
  ChatReaction,
  ChatReplyRef,
  ProducerMetadata,
  ChatEntry,
  ChatAttachment,
  ChatHistoryPage,
  RoomSettings,
  RoomSnapshot,
  BanEntry,
  MemberEntry,
  ModerationEventEntry,
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
import { decodeAppearance } from './api-validation';

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

// The wire envelope permits uncorrelated replies and unsolicited events. The
// signaling dispatcher separately enforces browser request correlation. Present
// IDs must be bounded ASCII tokens; null cannot downgrade a reply into an event.
function requestId(value: unknown): string {
  const id = text(value);
  return /^[A-Za-z0-9_-]{1,64}$/.test(id) ? id : invalid();
}
const optionalRequestId: Decoder<string | undefined> = (value) =>
  value === undefined ? undefined : requestId(value);
const chatSessionId: Decoder<string> = (value) => {
  const id = text(value);
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id) ? id : invalid();
};

const mediaKind = choice('audio', 'video');
const direction = choice('sendrecv', 'sendonly', 'recvonly', 'inactive');
const priority = choice('very-low', 'low', 'medium', 'high');
const uint32 = integer(0xffffffff);
const byte = integer(255);
const producer = object<ProducerMetadata>({ id: text, kind: mediaKind, source: optional(text) });
// Cosmetic, so it degrades rather than dropping the message: a treatment a
// newer peer adds shows as the accent, and a color that is not a palette-shaped
// token shows as the automatic one. The renderer maps unknown tokens the same way.
const chatStyle: Decoder<ChatStyle> = (value) => {
  const source = record(value);
  const color = source['color'];
  const style = source['style'];
  return {
    color: typeof color === 'string' && /^[a-z]{3,12}$/.test(color) ? color : null,
    style: style === 'text' || style === 'bubble' ? style : 'accent',
  };
};
const replyRef = object<ChatReplyRef>({
  messageId: text,
  participantId: text,
  participantName: text,
  excerpt: text,
});
const reaction = object<ChatReaction>({ emoji: text, participantIds: list(text) });
const participant = object<ParticipantInfo>({
  id: text,
  name: text,
  producers: list(producer),
  role: text,
  authenticated: optional(boolean),
  chatStyle: optional(chatStyle),
});
export const decodeAttachment = object<ChatAttachment>({
  id: chatSessionId,
  name: (value) => {
    const name = text(value);
    return name.length > 0 &&
      Array.from(name).length <= 120 &&
      !Array.from(name).some(
        (character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127,
      )
      ? name
      : invalid();
  },
  contentType: choice('image/png', 'image/jpeg', 'image/webp', 'application/octet-stream'),
  size: (value) => {
    const size = integer(5 * 1024 * 1024)(value);
    return size > 0 ? size : invalid();
  },
});
const attachments: Decoder<ChatAttachment[]> = (value) => {
  const entries = list(decodeAttachment)(value);
  return entries.length <= 4 && new Set(entries.map((entry) => entry.id)).size === entries.length
    ? entries
    : invalid();
};
const chat = object<ChatEntry>({
  messageId: text,
  clientMessageId: text,
  participantId: text,
  participantName: text,
  recipientId: optional(text),
  recipientName: optional(text),
  content: text,
  sentAt: text,
  chatStyle: optional(chatStyle),
  replyTo: optional(replyRef),
  reactions: optional(list(reaction)),
  attachments: optional(attachments),
  removedAt: optional(text),
  editedAt: optional(text),
  revision: integer(4_294_967_295),
});
const pins = (value: unknown): ChatEntry[] => {
  const entries = list(chat)(value);
  return entries.length <= 3 && entries.every((entry) => !entry.recipientId && !entry.removedAt)
    ? entries
    : invalid();
};
export const decodeChatEntry = chat;
export const decodeHistoryRetention = (value: unknown): number => {
  const days = integer(90)(value);
  return [0, 1, 7, 30, 90].includes(days) ? days : invalid();
};
export const decodeChatHistory = object<ChatHistoryPage>({
  messages: (value) => {
    const items = list(chat)(value);
    return items.length <= 100 ? items : invalid();
  },
  nextCursor: nullable(text),
  readMessageId: nullable(text),
  newerCursor: nullable(text),
  firstUnreadMessageId: nullable(text),
  retentionDays: decodeHistoryRetention,
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
  historyRetentionDays: decodeHistoryRetention,
  allowVideo: boolean,
  moderated: boolean,
  inviteOnly: boolean,
  secret: boolean,
  lobbyEnabled: boolean,
  pushToTalk: boolean,
  guestsAllowed: boolean,
  guestsCanBroadcast: boolean,
  topic: optional(text),
  nameStyle: decodeAppearance,
  topicStyle: decodeAppearance,
});
const settings = decodeRoomSettings;
const lobbyEntry = object<{ participantId: string; displayName: string; authenticated: boolean }>({
  participantId: text,
  displayName: text,
  authenticated: boolean,
});
const snapshot = object<RoomSnapshot>({
  chatSessionId: optional(chatSessionId),
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
  outcome: optional(
    object<{ action: string; createdAt: string }>({ action: text, createdAt: text }),
  ),
});
const moderationEvent = object<ModerationEventEntry>({
  eventId: text,
  action: text,
  actorId: text,
  actorName: text,
  targetId: text,
  targetName: text,
  targetAuthenticated: boolean,
  reason: optional(text),
  expiresAt: optional(text),
  reportId: optional(text),
  createdAt: text,
  targetIp: optional(text),
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
  // The pinned client still requires this deprecated alias in its IceCandidate type.
  // oxlint-disable-next-line typescript/no-deprecated
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
  setChatStyle: object<{ chatStyle: ChatStyle }>({ chatStyle }),
  removeChatMessage: object<{ messageId: string; removedAt: string }>({
    messageId: text,
    removedAt: text,
  }),
  editChatMessage: object<{ message: ChatEntry }>({ message: chat }),
  getAttachmentAccess: object<SocialResponses['getAttachmentAccess']>({
    token: (value) => {
      const token = text(value);
      return token.length > 0 && token.length <= 4096 && /^[A-Za-z0-9_.-]+$/.test(token)
        ? token
        : invalid();
    },
    expiresAt: (value) => {
      const expires = text(value);
      return expires.length <= 40 &&
        /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?(?:Z|[+-]\d\d:\d\d)$/.test(expires) &&
        Number.isFinite(Date.parse(expires))
        ? expires
        : invalid();
    },
  }),
  getPinnedMessages: object<{ messages: ChatEntry[] }>({ messages: pins }),
  setPinnedMessage: object<{ messages: ChatEntry[] }>({ messages: pins }),
  reactToMessage: object<{ messageId: string; reactions: ChatReaction[] }>({
    messageId: text,
    reactions: list(reaction),
  }),
  getRoomSnapshot: snapshot,
  getChatHistory: decodeChatHistory,
  markChatRead: object<{ readMessageId: string | null }>({ readMessageId: nullable(text) }),
  setRoomHistory: object<{ retentionDays: number }>({ retentionDays: decodeHistoryRetention }),
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
  listModerationEvents: object<{ events: ModerationEventEntry[]; hasMore: boolean }>({
    events: list(moderationEvent),
    hasMore: boolean,
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
    requestId: requestId(source['requestId']),
    action,
    data: decodeSocialData(action, source['data']),
  } as SocialResponse;
};
const socialAction = choice(
  'setChatPreferences',
  'changeNickname',
  'setChatStyle',
  'removeChatMessage',
  'editChatMessage',
  'getAttachmentAccess',
  'getPinnedMessages',
  'setPinnedMessage',
  'reactToMessage',
  'getRoomSnapshot',
  'getChatHistory',
  'markChatRead',
  'setRoomHistory',
  'listRoomBans',
  'removeRoomBan',
  'listRoomMembers',
  'setMemberRole',
  'reportParticipant',
  'listRoomReports',
  'resolveRoomReport',
  'listModerationEvents',
);

// Adding a ServerMessage variant fails typechecking until its decoder exists.
const messages = {
  authenticationRenewed: message('authenticationRenewed', {
    requestId,
    expiresAt: integer(),
  }),
  authenticationRenewalFailed: message('authenticationRenewalFailed', { requestId }),
  authenticationRenewalDeferred: message('authenticationRenewalDeferred', {
    requestId,
    retryAfterMs: integer(5000, 1),
    expiresAt: integer(),
  }),
  roomJoined: message('roomJoined', {
    participantId: text,
    participants: list(participant),
    reconnectToken: text,
    yourRole: text,
    yourChatStyle: optional(chatStyle),
    yourName: optional(text),
    roomSettings: optional(settings),
  }),
  error: message('error', { requestId: optionalRequestId, message: text }),
  roomPasswordRequired: message('roomPasswordRequired', {}),
  roomClosed: message('roomClosed', { reason: text }),
  serverRestarting: message('serverRestarting', { reason: text }),
  routerRtpCapabilities: message('routerRtpCapabilities', {
    requestId: optionalRequestId,
    rtpCapabilities: capabilities,
  }),
  transportCreated: message('transportCreated', {
    requestId: optionalRequestId,
    transportId: text,
    iceParameters: ice,
    iceCandidates: list(candidate),
    dtlsParameters: dtls,
    iceServers: optional(list(iceServer)),
  }),
  transportConnected: message('transportConnected', {
    requestId: optionalRequestId,
    transportId: text,
  }),
  producerCreated: message('producerCreated', { requestId: optionalRequestId, producerId: text }),
  consumerCreated: message('consumerCreated', {
    requestId: optionalRequestId,
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
    chatStyle: optional(chatStyle),
  }),
  participantLeft: message('participantLeft', { participantId: text }),
  newProducer: message('newProducer', {
    participantId: text,
    producerId: text,
    kind: mediaKind,
    source: optional(text),
  }),
  producerClosed: message('producerClosed', { producerId: text }),
  producerPaused: message('producerPaused', { requestId: optionalRequestId, producerId: text }),
  producerResumed: message('producerResumed', { requestId: optionalRequestId, producerId: text }),
  consumerResumed: message('consumerResumed', { requestId: optionalRequestId, consumerId: text }),
  consumerPaused: message('consumerPaused', { requestId: optionalRequestId, consumerId: text }),
  mediaControlApplied: message('mediaControlApplied', { requestId }),
  roomControlApplied: message('roomControlApplied', { requestId }),
  reconnectResult: message('reconnectResult', {
    requestId: optionalRequestId,
    success: boolean,
    participantId: text,
    reconnectToken: optional(text),
  }),
  iceRestarted: message('iceRestarted', {
    requestId: optionalRequestId,
    transportId: text,
    iceParameters: ice,
    iceServers: list(iceServer),
  }),
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
    revision: integer(4_294_967_295),
    editedAt: optional(text),
    chatStyle: optional(chatStyle),
    replyTo: optional(replyRef),
    reactions: optional(list(reaction)),
    attachments: optional(attachments),
    removedAt: optional(text),
  }),
  privateMessageReceived: message('privateMessageReceived', { message: chat }),
  chatMessageRemoved: message('chatMessageRemoved', { messageId: text, removedAt: text }),
  chatMessageEdited: message('chatMessageEdited', { message: chat }),
  pinnedMessagesChanged: message('pinnedMessagesChanged', { messages: pins }),
  messageReactions: message('messageReactions', { messageId: text, reactions: list(reaction) }),
  messageAck: message('messageAck', { clientMessageId: text, message: chat }),
  messageRetryResult: message('messageRetryResult', {
    clientMessageId: requestId,
    outcome: choice('unknown'),
    reason: choice(
      'session_changed',
      'receipt_expired',
      'sequence_superseded',
      'capacity',
      'conflict',
      'recipient_unconfirmed',
      'storage_unconfirmed',
    ),
  }),
  socialResponse,
  socialError: message('socialError', {
    requestId: optionalRequestId,
    clientMessageId: optional(text),
    message: text,
  }),
  nicknameChanged: message('nicknameChanged', { participantId: text, nickname: text }),
  chatStyleChanged: message('chatStyleChanged', { participantId: text, chatStyle }),
  participantTyping: message('participantTyping', {
    participantId: text,
    targetParticipantId: optional(text),
  }),
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
    moderatorCount: uint32,
  }),
  lobbyStatus: message('lobbyStatus', { participantCount: uint32, moderatorCount: uint32 }),
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
