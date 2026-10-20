// Generated from the Rust wire types by `cargo run -q -p xtask -- protocol-ts`.
// Do not edit: change the Rust types and limits xtask/src/protocol_ts.rs lists and regenerate.

import type {
  DtlsParameters,
  IceCandidate,
  IceParameters,
  RtpCapabilities,
  RtpCapabilities as RtpCapabilitiesFinalized,
  RtpParameters,
} from 'mediasoup-client/types';

export const MEDIA_KINDS = ['audio', 'video'] as const;
export type MediaKind = (typeof MEDIA_KINDS)[number];

/** The fewest NFC-normalized characters a new password may have. */
export const MIN_PASSWORD_CHARS = 15;
/**
 * The most NFC-normalized characters, counted as characters so scripts with multi-byte
 * letters are neither admitted short nor refused long.
 */
export const MAX_PASSWORD_CHARS = 128;
/** Bounds the hashing input; 128 characters of any script fit within it. */
export const MAX_PASSWORD_BYTES = 512;
/**
 * The longest account display name in bytes; the profile form caps its field at as many
 * characters (mirrored into the browser contract by `protocol-ts`).
 */
export const MAX_DISPLAY_NAME_LEN = 64;

export const ERROR_CODES = [
  'invalid_message',
  'invalid',
  'forbidden',
  'not_found',
  'rate_limited',
  'unavailable',
  'timed_out',
  'request_failed',
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

export const ROLES = ['guest', 'user', 'member', 'moderator', 'admin', 'owner'] as const;
export type Role = (typeof ROLES)[number];

export interface SocialResponses {
  setChatPreferences: {
    allowPrivateMessages: boolean;
    ignoredParticipantIds: string[];
  };
  changeNickname: {
    nickname: string;
  };
  setChatStyle: {
    chatStyle: ChatStyle;
  };
  removeChatMessage: {
    messageId: string;
    removedAt: string;
  };
  editChatMessage: {
    message: ChatEntry;
  };
  getAttachmentAccess: {
    token: string;
    expiresAt: string;
  };
  getPinnedMessages: {
    messages: ChatEntry[];
  };
  setPinnedMessage: {
    messages: ChatEntry[];
  };
  reactToMessage: {
    messageId: string;
    reactions: ChatReaction[];
  };
  getRoomSnapshot: RoomSnapshot;
  getChatHistory: ChatHistoryPage;
  markChatRead: {
    readMessageId: string | null;
  };
  setRoomHistory: {
    retentionDays: number;
  };
  listRoomBans: {
    bans: BanEntry[];
    hasMore: boolean;
  };
  removeRoomBan: {
    removed: boolean;
  };
  listRoomMembers: {
    members: MemberEntry[];
    hasMore: boolean;
  };
  setMemberRole: {
    updated: boolean;
  };
  reportParticipant: {
    reportId: string;
    status: ReportStatus;
  };
  listRoomReports: {
    reports: ReportEntry[];
    hasMore: boolean;
  };
  resolveRoomReport: {
    reportId: string;
    status: ReportResolution;
  };
  listModerationEvents: {
    events: ModerationEventEntry[];
    hasMore: boolean;
  };
}
export type SocialAction = keyof SocialResponses;
export type SocialResponse<A extends SocialAction = SocialAction> = {
  [K in A]: { type: 'socialResponse'; requestId: string; action: K; data: SocialResponses[K] };
}[A];

export type ClientMessage =
  | {
      type: 'renewAuthentication';
      requestId: string;
      token: string;
    }
  | {
      type: 'joinRoom';
      roomId: string;
      participantName: string;
      password?: string;
      chatStyle?: ChatStyle;
    }
  | { type: 'leaveRoom' }
  | { type: 'getRouterRtpCapabilities' }
  | { type: 'createSendTransport' }
  | { type: 'createRecvTransport' }
  | {
      type: 'replaceRecvTransport';
      transportId: string;
    }
  | {
      type: 'connectTransport';
      transportId: string;
      dtlsParameters: DtlsParameters;
    }
  | {
      type: 'produce';
      transportId: string;
      kind: MediaKind;
      rtpParameters: RtpParameters;
      source?: string;
    }
  | {
      type: 'consume';
      producerId: string;
      rtpCapabilities: RtpCapabilities;
    }
  | {
      type: 'resumeConsumer';
      consumerId: string;
    }
  | {
      type: 'pauseConsumer';
      consumerId: string;
    }
  | {
      type: 'closeConsumer';
      consumerId: string;
    }
  | {
      type: 'closeProducer';
      producerId: string;
    }
  | {
      type: 'pauseProducer';
      producerId: string;
    }
  | {
      type: 'resumeProducer';
      producerId: string;
    }
  | {
      type: 'reconnect';
      participantId: string;
      roomId: string;
      reconnectToken: string;
    }
  | {
      type: 'restartIce';
      transportId: string;
    }
  | {
      type: 'setConsumerPreferredLayers';
      consumerId: string;
      spatialLayer: number;
      temporalLayer?: number;
    }
  | {
      type: 'chatMessage';
      content: string;
      attachmentIds?: string[];
      clientMessageId?: string;
      sequence?: number;
      replyTo?: string;
    }
  | {
      type: 'privateMessage';
      targetParticipantId: string;
      content: string;
      attachmentIds?: string[];
      clientMessageId: string;
      sequence?: number;
      replyTo?: string;
    }
  | ({ type: 'retryChatMessage' } & RetryChatMessage)
  | {
      type: 'getAttachmentAccess';
      requestId: string;
      attachmentId: string;
    }
  | {
      type: 'getChatHistory';
      requestId: string;
      before?: string;
      after?: string;
      around?: string;
      resume?: boolean;
      q?: string;
      limit?: number;
    }
  | {
      type: 'markChatRead';
      requestId: string;
      messageId: string;
    }
  | {
      type: 'setRoomHistory';
      requestId: string;
      retentionDays: number;
    }
  | {
      type: 'setChatPreferences';
      requestId: string;
      allowPrivateMessages: boolean;
      ignoredParticipantIds: string[];
    }
  | {
      type: 'setChatStyle';
      requestId: string;
      chatStyle: ChatStyle;
    }
  | {
      type: 'editChatMessage';
      requestId: string;
      messageId: string;
      content: string;
      expectedRevision: number;
    }
  | {
      type: 'getPinnedMessages';
      requestId: string;
    }
  | {
      type: 'setPinnedMessage';
      requestId: string;
      messageId: string;
      pinned: boolean;
    }
  | {
      type: 'removeChatMessage';
      requestId: string;
      messageId: string;
    }
  | {
      type: 'reactToMessage';
      requestId: string;
      messageId: string;
      emoji: string;
    }
  | {
      type: 'changeNickname';
      requestId: string;
      nickname: string;
    }
  | {
      type: 'getRoomSnapshot';
      requestId: string;
    }
  | {
      type: 'listRoomBans';
      requestId: string;
      offset?: number;
    }
  | {
      type: 'removeRoomBan';
      requestId: string;
      banId: string;
    }
  | {
      type: 'listRoomMembers';
      requestId: string;
      offset?: number;
    }
  | {
      type: 'setMemberRole';
      requestId: string;
      targetUserId: string;
      role: Role;
    }
  | {
      type: 'reportParticipant';
      requestId: string;
      targetParticipantId: string;
      reason: string;
    }
  | {
      type: 'listRoomReports';
      requestId: string;
      offset?: number;
    }
  | {
      type: 'resolveRoomReport';
      requestId: string;
      reportId: string;
      status: ReportResolution;
    }
  | {
      type: 'listModerationEvents';
      requestId: string;
      offset?: number;
    }
  | {
      type: 'typing';
      targetParticipantId?: string;
    }
  | {
      type: 'closeCam';
      targetParticipantId: string;
    }
  | {
      type: 'camBan';
      targetParticipantId: string;
      reason?: string;
    }
  | {
      type: 'camUnban';
      targetParticipantId: string;
    }
  | {
      type: 'textMute';
      targetParticipantId: string;
    }
  | {
      type: 'textUnmute';
      targetParticipantId: string;
    }
  | {
      type: 'kick';
      targetParticipantId: string;
      reason?: string;
      reportId?: string;
    }
  | {
      type: 'ban';
      targetParticipantId: string;
      reason?: string;
      duration?: number;
      reportId?: string;
    }
  | {
      type: 'unban';
      targetUserId: string;
    }
  | {
      type: 'setRole';
      targetParticipantId: string;
      role: Role;
    }
  | { type: 'requestVoice' }
  | ({
      type: 'updateRoomSettings';
    } & SettingsPatch)
  | {
      type: 'setTopic';
      topic: string;
    }
  | {
      type: 'admitFromLobby';
      targetParticipantId: string;
    }
  | {
      type: 'denyFromLobby';
      targetParticipantId: string;
    };

export type ServerMessage =
  | {
      type: 'roomJoined';
      requestId?: string;
      participantId: string;
      participants: ParticipantInfo[];
      reconnectToken: string;
      yourRole: Role;
      yourName: string;
      yourChatStyle: ChatStyle;
      roomSettings?: RoomSettings;
    }
  | {
      type: 'authenticationRenewed';
      requestId: string;
      expiresAt: number;
    }
  | {
      type: 'authenticationRenewalFailed';
      requestId: string;
    }
  | {
      type: 'authenticationRenewalDeferred';
      requestId: string;
      retryAfterMs: number;
      expiresAt: number;
    }
  | {
      type: 'error';
      requestId?: string;
      code: ErrorCode;
      message: string;
    }
  | { type: 'roomPasswordRequired'; requestId?: string }
  | {
      type: 'roomClosed';
      requestId?: string;
      reason: string;
    }
  | {
      type: 'serverRestarting';
      requestId?: string;
      reason: string;
    }
  | {
      type: 'routerRtpCapabilities';
      requestId?: string;
      rtpCapabilities: RtpCapabilitiesFinalized;
    }
  | {
      type: 'transportCreated';
      requestId?: string;
      transportId: string;
      iceParameters: IceParameters;
      iceCandidates: IceCandidate[];
      dtlsParameters: DtlsParameters;
      iceServers?: IceServerEntry[];
    }
  | {
      type: 'transportConnected';
      requestId?: string;
      transportId: string;
    }
  | {
      type: 'producerCreated';
      requestId?: string;
      producerId: string;
    }
  | {
      type: 'consumerCreated';
      requestId?: string;
      consumerId: string;
      producerId: string;
      kind: MediaKind;
      rtpParameters: RtpParameters;
    }
  | {
      type: 'participantJoined';
      requestId?: string;
      participantId: string;
      participantName: string;
      role: Role;
      authenticated: boolean;
      chatStyle: ChatStyle;
    }
  | {
      type: 'participantLeft';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'newProducer';
      requestId?: string;
      participantId: string;
      producerId: string;
      kind: MediaKind;
      source?: string;
    }
  | {
      type: 'producerClosed';
      requestId?: string;
      producerId: string;
    }
  | {
      type: 'producerPaused';
      requestId?: string;
      producerId: string;
    }
  | {
      type: 'producerResumed';
      requestId?: string;
      producerId: string;
    }
  | {
      type: 'consumerResumed';
      requestId?: string;
      consumerId: string;
    }
  | {
      type: 'consumerPaused';
      requestId?: string;
      consumerId: string;
    }
  | { type: 'mediaControlApplied'; requestId?: string }
  | { type: 'roomControlApplied'; requestId?: string }
  | {
      type: 'reconnectResult';
      requestId?: string;
      success: boolean;
      participantId: string;
      reconnectToken?: string;
    }
  | {
      type: 'iceRestarted';
      requestId?: string;
      transportId: string;
      iceParameters: IceParameters;
      iceServers: IceServerEntry[];
    }
  | {
      type: 'connectionStats';
      requestId?: string;
      availableBitrate: number | null;
    }
  | {
      type: 'consumerLayersChanged';
      requestId?: string;
      consumerId: string;
      spatialLayer: number | null;
      temporalLayer: number | null;
    }
  | {
      type: 'chatReceived';
      requestId?: string;
      participantId: string;
      participantName: string;
      content: string;
      attachments?: ChatAttachment[];
      messageId: string;
      clientMessageId: string;
      sentAt: string;
      removedAt?: string;
      revision: number;
      editedAt?: string;
      chatStyle: ChatStyle;
      replyTo?: ChatReplyRef;
    }
  | {
      type: 'messageAck';
      requestId?: string;
      clientMessageId: string;
      message: ChatEntry;
    }
  | {
      type: 'messageRetryResult';
      requestId?: string;
      clientMessageId: string;
      outcome: ChatRetryOutcome;
      reason: ChatRetryReason;
    }
  | {
      type: 'privateMessageReceived';
      requestId?: string;
      message: ChatEntry;
    }
  | SocialResponse
  | {
      type: 'socialError';
      requestId?: string;
      clientMessageId?: string;
      code: ErrorCode;
      message: string;
    }
  | {
      type: 'nicknameChanged';
      requestId?: string;
      participantId: string;
      nickname: string;
    }
  | {
      type: 'chatStyleChanged';
      requestId?: string;
      participantId: string;
      chatStyle: ChatStyle;
    }
  | {
      type: 'participantTyping';
      requestId?: string;
      participantId: string;
      targetParticipantId?: string;
    }
  | {
      type: 'chatMessageEdited';
      requestId?: string;
      message: ChatEntry;
    }
  | {
      type: 'pinnedMessagesChanged';
      requestId?: string;
      messages: ChatEntry[];
    }
  | {
      type: 'chatMessageRemoved';
      requestId?: string;
      messageId: string;
      removedAt: string;
    }
  | {
      type: 'messageReactions';
      requestId?: string;
      messageId: string;
      reactions: ChatReaction[];
    }
  | {
      type: 'activeSpeaker';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'audioLevels';
      requestId?: string;
      levels: AudioLevelEntry[];
    }
  | {
      type: 'forceClosedProducer';
      requestId?: string;
      producerId: string;
      reason: string;
    }
  | {
      type: 'camBanned';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'camUnbanned';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'textMuted';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'textUnmuted';
      requestId?: string;
      participantId: string;
    }
  | {
      type: 'participantKicked';
      requestId?: string;
      participantId: string;
      reason?: string;
    }
  | {
      type: 'participantBanned';
      requestId?: string;
      participantId: string;
      reason?: string;
    }
  | {
      type: 'roleChanged';
      requestId?: string;
      participantId: string;
      newRole: Role;
      grantedBy: string;
    }
  | {
      type: 'voiceRequested';
      requestId?: string;
      participantId: string;
      displayName: string;
    }
  | {
      type: 'roomSettingsChanged';
      requestId?: string;
      settings: RoomSettings;
    }
  | {
      type: 'topicChanged';
      requestId?: string;
      topic: string;
      changedBy: string;
    }
  | {
      type: 'lobbyWaiting';
      requestId?: string;
      roomName: string;
      topic?: string;
      participantCount: number;
      moderatorCount: number;
    }
  | {
      type: 'lobbyStatus';
      requestId?: string;
      participantCount: number;
      moderatorCount: number;
    }
  | {
      type: 'lobbyJoin';
      requestId?: string;
      participantId: string;
      displayName: string;
      authenticated: boolean;
    }
  | {
      type: 'lobbyDenied';
      requestId?: string;
      reason?: string;
    }
  | { type: 'lobbyAdmitted'; requestId?: string };

export interface ChatPreferences {
  allowPrivateMessages: boolean;
  sounds: boolean;
  largeText: boolean;
  timestamps: TimestampFormat;
  ignored: IgnoredPerson[];
}

export interface PreferencePatch {
  allowPrivateMessages?: boolean;
  sounds?: boolean;
  largeText?: boolean;
  timestamps?: TimestampFormat;
  ignore?: IgnoredPerson;
  unignore?: string;
}

export interface BanEntry {
  banId: string;
  displayName: string;
  authenticated: boolean;
  reason?: string;
  expiresAt?: string;
}

export interface ChatEntry {
  messageId: string;
  clientMessageId: string;
  participantId: string;
  participantName: string;
  recipientId?: string;
  recipientName?: string;
  accountConversation?: boolean;
  content: string;
  attachments?: ChatAttachment[];
  sentAt: string;
  removedAt?: string;
  revision: number;
  editedAt?: string;
  chatStyle: ChatStyle;
  replyTo?: ChatReplyRef;
  reactions?: ChatReaction[];
}

export interface ChatReaction {
  emoji: string;
  participantIds: string[];
}

export interface ChatStyle {
  color: string | null;
  style: ChatStyleKind;
}

export interface ChatHistoryPage {
  messages: ChatEntry[];
  nextCursor: string | null;
  newerCursor: string | null;
  firstUnreadMessageId: string | null;
  readMessageId: string | null;
  retentionDays: number;
}

export interface MemberEntry {
  userId: string;
  displayName: string;
  role: Role;
  online: boolean;
  authenticated: boolean;
}

export type ModerationEventEntry = ModerationEvent & {
  targetIp?: string;
};

export interface ReportEntry {
  reportId: string;
  reporterId: string;
  reporterName: string;
  targetParticipantId: string;
  targetName: string;
  reason: string;
  status: ReportStatus;
  createdAt: string;
  resolvedAt?: string;
  outcome?: ReportOutcome;
}

export const REPORT_RESOLUTIONS = ['resolved', 'dismissed'] as const;
export type ReportResolution = (typeof REPORT_RESOLUTIONS)[number];

export const REPORT_STATUSES = ['open', 'resolved', 'dismissed'] as const;
export type ReportStatus = (typeof REPORT_STATUSES)[number];

export interface RoomSnapshot {
  participants: ParticipantInfo[];
  messages: ChatEntry[];
  lobby: SnapshotLobbyEntry[];
  yourRole: Role;
  chatSessionId: string;
  roomSettings: RoomSettings | null;
  nickname: string;
  allowPrivateMessages: boolean;
  ignoredParticipantIds: string[];
  pausedProducerIds: string[];
  textMuted: boolean;
  camBanned: boolean;
  canChat: boolean;
  localProducerIds: string[];
  canBroadcast: boolean;
}

export interface RetryChatMessage {
  clientMessageId: string;
  sequence: number;
  chatSessionId: string;
  content: string;
  attachmentIds?: string[];
  targetParticipantId?: string;
  replyTo?: string;
}

export interface SettingsPatch {
  moderated?: boolean;
  lobbyEnabled?: boolean;
  guestsAllowed?: boolean;
  guestsCanBroadcast?: boolean;
  maxBroadcasters?: number | null;
  maxParticipants?: number | null;
  allowScreenSharing?: boolean;
  allowChat?: boolean;
  allowVideo?: boolean;
  requireRegistration?: boolean;
  inviteOnly?: boolean;
  pushToTalk?: boolean;
  secret?: boolean;
  password?: string | null;
}

export interface AudioLevelEntry {
  participantId: string;
  volume: number;
}

export interface ChatAttachment {
  id: string;
  name: string;
  contentType: string;
  size: number;
}

export interface ChatReplyRef {
  messageId: string;
  participantId: string;
  participantName: string;
  excerpt: string;
}

export const CHAT_RETRY_OUTCOMES = ['unknown'] as const;
export type ChatRetryOutcome = (typeof CHAT_RETRY_OUTCOMES)[number];

export const CHAT_RETRY_REASONS = [
  'session_changed',
  'receipt_expired',
  'sequence_superseded',
  'capacity',
  'conflict',
  'recipient_unconfirmed',
  'storage_unconfirmed',
] as const;
export type ChatRetryReason = (typeof CHAT_RETRY_REASONS)[number];

export interface IceServerEntry {
  urls: string[];
  username?: string;
  credential?: string;
}

export interface ParticipantInfo {
  id: string;
  name: string;
  producers: ProducerMetadata[];
  role: Role;
  authenticated: boolean;
  chatStyle: ChatStyle;
}

export interface RoomSettings {
  id: string;
  displayName: string;
  passwordProtected: boolean;
  requireRegistration: boolean;
  maxParticipants?: number;
  maxBroadcasters?: number;
  allowScreenSharing: boolean;
  allowChat: boolean;
  allowVideo: boolean;
  moderated: boolean;
  inviteOnly: boolean;
  secret: boolean;
  lobbyEnabled: boolean;
  pushToTalk: boolean;
  guestsAllowed: boolean;
  guestsCanBroadcast: boolean;
  topic?: string;
  nameStyle: ChatStyle;
  topicStyle: ChatStyle;
  historyRetentionDays: number;
}

export interface IgnoredPerson {
  id: string;
  name: string;
}

export const TIMESTAMP_FORMATS = ['hover', 'time', 'time12', 'seconds', 'datetime'] as const;
export type TimestampFormat = (typeof TIMESTAMP_FORMATS)[number];

export const CHAT_STYLE_KINDS = ['accent', 'text', 'bubble'] as const;
export type ChatStyleKind = (typeof CHAT_STYLE_KINDS)[number];

export interface ModerationEvent {
  eventId: string;
  action: ModerationAction;
  actorId: string;
  actorName: string;
  targetId: string;
  targetName: string;
  targetAuthenticated: boolean;
  reason?: string;
  expiresAt?: string;
  reportId?: string;
  createdAt: string;
}

export interface ReportOutcome {
  action: string;
  createdAt: string;
}

export interface SnapshotLobbyEntry {
  participantId: string;
  displayName: string;
  authenticated: boolean;
}

export interface ProducerMetadata {
  id: string;
  kind: MediaKind;
  source?: string;
}

export const MODERATION_ACTIONS = [
  'kick',
  'ban',
  'unban',
  'cam_ban',
  'cam_unban',
  'text_mute',
  'text_unmute',
  'report_resolved',
  'report_dismissed',
  'message_removed',
] as const;
export type ModerationAction = (typeof MODERATION_ACTIONS)[number];

/** The action selects both the outgoing payload and the validated response. */
export interface SocialRequests {
  getAttachmentAccess: {
    attachmentId: string;
  };
  getChatHistory: {
    before?: string;
    after?: string;
    around?: string;
    resume?: boolean;
    q?: string;
    limit?: number;
  };
  markChatRead: {
    messageId: string;
  };
  setRoomHistory: {
    retentionDays: number;
  };
  setChatPreferences: {
    allowPrivateMessages: boolean;
    ignoredParticipantIds: string[];
  };
  setChatStyle: {
    chatStyle: ChatStyle;
  };
  editChatMessage: {
    messageId: string;
    content: string;
    expectedRevision: number;
  };
  getPinnedMessages: undefined;
  setPinnedMessage: {
    messageId: string;
    pinned: boolean;
  };
  removeChatMessage: {
    messageId: string;
  };
  reactToMessage: {
    messageId: string;
    emoji: string;
  };
  changeNickname: {
    nickname: string;
  };
  getRoomSnapshot: undefined;
  listRoomBans: {
    offset?: number;
  };
  removeRoomBan: {
    banId: string;
  };
  listRoomMembers: {
    offset?: number;
  };
  setMemberRole: {
    targetUserId: string;
    role: Role;
  };
  reportParticipant: {
    targetParticipantId: string;
    reason: string;
  };
  listRoomReports: {
    offset?: number;
  };
  resolveRoomReport: {
    reportId: string;
    status: ReportResolution;
  };
  listModerationEvents: {
    offset?: number;
  };
}
export type SocialRequest = {
  [A in SocialAction]: { type: A; requestId: string } & (SocialRequests[A] extends undefined
    ? object
    : SocialRequests[A]);
}[SocialAction];
/** No-payload/page actions may omit data; mutation payloads remain required. */
export type SocialRequestArguments<A extends SocialAction> = {
  [K in A]: SocialRequests[K] extends undefined
    ? [action: K]
    : object extends SocialRequests[K]
      ? [action: K, data?: SocialRequests[K]]
      : [action: K, data: SocialRequests[K]];
}[A];
