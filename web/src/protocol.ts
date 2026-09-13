// Signaling protocol types — mirrors src/signaling/protocol.rs exactly.
// Rust uses #[serde(tag = "type", rename_all = "camelCase")]

// --- Client → Server ---

export type ClientMessage =
  | { type: 'joinRoom'; roomId: string; participantName: string; password?: string }
  | { type: 'leaveRoom' }
  | { type: 'getRouterRtpCapabilities' }
  | { type: 'createSendTransport' }
  | { type: 'createRecvTransport' }
  | { type: 'connectTransport'; transportId: string; dtlsParameters: DtlsParameters }
  | {
      type: 'produce';
      transportId: string;
      kind: MediaKind;
      rtpParameters: RtpParameters;
      source?: string;
    }
  | { type: 'consume'; producerId: string; rtpCapabilities: RtpCapabilities }
  | { type: 'resumeConsumer'; consumerId: string }
  | { type: 'pauseConsumer'; consumerId: string }
  | { type: 'closeProducer'; producerId: string }
  | { type: 'pauseProducer'; producerId: string }
  | { type: 'resumeProducer'; producerId: string }
  | { type: 'reconnect'; participantId: string; roomId: string; reconnectToken: string }
  | { type: 'restartIce'; transportId: string }
  | {
      type: 'setConsumerPreferredLayers';
      consumerId: string;
      spatialLayer: number;
      temporalLayer?: number;
    }
  | { type: 'chatMessage'; content: string; clientMessageId?: string }
  | {
      type: 'privateMessage';
      targetParticipantId: string;
      content: string;
      clientMessageId: string;
    }
  | SocialRequest
  // Moderation
  | { type: 'closeCam'; targetParticipantId: string }
  | { type: 'camBan'; targetParticipantId: string; reason?: string }
  | { type: 'camUnban'; targetParticipantId: string }
  | { type: 'textMute'; targetParticipantId: string }
  | { type: 'textUnmute'; targetParticipantId: string }
  | { type: 'kick'; targetParticipantId: string; reason?: string }
  | { type: 'ban'; targetParticipantId: string; reason?: string; duration?: number }
  | { type: 'unban'; targetUserId: string }
  | { type: 'setRole'; targetParticipantId: string; role: number }
  | { type: 'requestVoice' }
  // Room management
  | ({ type: 'updateRoomSettings' } & RoomSettingsPatch)
  | { type: 'setTopic'; topic: string }
  // Lobby
  | { type: 'admitFromLobby'; targetParticipantId: string }
  | { type: 'denyFromLobby'; targetParticipantId: string };

// --- Server → Client ---

export type ServerMessage =
  | {
      type: 'roomJoined';
      participantId: string;
      participants: ParticipantInfo[];
      reconnectToken: string;
      yourRole: string;
      roomSettings?: RoomSettings;
    }
  | { type: 'error'; message: string }
  | { type: 'roomPasswordRequired' }
  | { type: 'roomClosed'; reason: string }
  | { type: 'serverRestarting'; reason: string }
  | { type: 'routerRtpCapabilities'; rtpCapabilities: RtpCapabilitiesFinalized }
  | {
      type: 'transportCreated';
      transportId: string;
      iceParameters: IceParameters;
      iceCandidates: IceCandidate[];
      dtlsParameters: DtlsParameters;
      iceServers?: IceServerEntry[];
    }
  | { type: 'transportConnected'; transportId: string }
  | { type: 'producerCreated'; producerId: string }
  | {
      type: 'consumerCreated';
      consumerId: string;
      producerId: string;
      kind: MediaKind;
      rtpParameters: RtpParameters;
    }
  | {
      type: 'participantJoined';
      participantId: string;
      participantName: string;
      role: string;
      authenticated: boolean;
    }
  | { type: 'participantLeft'; participantId: string }
  | {
      type: 'newProducer';
      participantId: string;
      producerId: string;
      kind: MediaKind;
      source?: string;
    }
  | { type: 'producerClosed'; producerId: string }
  | { type: 'producerPaused'; producerId: string }
  | { type: 'producerResumed'; producerId: string }
  | { type: 'consumerResumed'; consumerId: string }
  | { type: 'consumerPaused'; consumerId: string }
  | { type: 'reconnectResult'; success: boolean; participantId: string; reconnectToken?: string }
  | { type: 'iceRestarted'; transportId: string; iceParameters: IceParameters }
  | { type: 'connectionStats'; availableBitrate: number | null; rtt: number | null }
  | {
      type: 'consumerLayersChanged';
      consumerId: string;
      spatialLayer: number | null;
      temporalLayer: number | null;
    }
  | ({ type: 'chatReceived' } & ChatEntry)
  | { type: 'privateMessageReceived'; message: ChatEntry }
  | { type: 'messageAck'; clientMessageId: string; message: ChatEntry }
  | SocialResponse
  | { type: 'socialError'; requestId?: string; clientMessageId?: string; message: string }
  | { type: 'nicknameChanged'; participantId: string; nickname: string }
  | { type: 'activeSpeaker'; participantId: string }
  | { type: 'audioLevels'; levels: { participantId: string; volume: number }[] }
  // Moderation broadcasts
  | { type: 'forceClosedProducer'; producerId: string; reason: string }
  | { type: 'camBanned'; participantId: string }
  | { type: 'camUnbanned'; participantId: string }
  | { type: 'textMuted'; participantId: string }
  | { type: 'textUnmuted'; participantId: string }
  | { type: 'participantKicked'; participantId: string; reason?: string }
  | { type: 'participantBanned'; participantId: string; reason?: string }
  | { type: 'roleChanged'; participantId: string; newRole: string; grantedBy: string }
  | { type: 'voiceRequested'; participantId: string; displayName: string }
  // Room state
  | { type: 'roomSettingsChanged'; settings: RoomSettings }
  | { type: 'topicChanged'; topic: string; changedBy: string }
  // Lobby
  | { type: 'lobbyWaiting'; roomName: string; topic?: string; participantCount: number }
  | { type: 'lobbyJoin'; participantId: string; displayName: string; authenticated: boolean }
  | { type: 'lobbyDenied'; reason?: string }
  | { type: 'lobbyAdmitted' };

// --- Shared types ---

export interface ParticipantInfo {
  id: string;
  name: string;
  producers: ProducerMetadata[];
  role: string;
  authenticated?: boolean;
}

export type SocialAction =
  | 'setChatPreferences'
  | 'changeNickname'
  | 'getRoomSnapshot'
  | 'listRoomBans'
  | 'removeRoomBan'
  | 'listRoomMembers'
  | 'setMemberRole'
  | 'reportParticipant'
  | 'listRoomReports'
  | 'resolveRoomReport';

/** The action selects both the outgoing payload and the validated response. */
export interface SocialRequests {
  setChatPreferences: { allowPrivateMessages: boolean; ignoredParticipantIds: string[] };
  changeNickname: { nickname: string };
  getRoomSnapshot: undefined;
  listRoomBans: { offset?: number };
  removeRoomBan: { banId: string };
  listRoomMembers: { offset?: number };
  setMemberRole: { targetUserId: string; role: number };
  reportParticipant: { targetParticipantId: string; reason: string };
  listRoomReports: { offset?: number };
  resolveRoomReport: { reportId: string; status: 'resolved' | 'dismissed' };
}

export interface SocialResponses {
  setChatPreferences: { allowPrivateMessages: boolean; ignoredParticipantIds: string[] };
  changeNickname: { nickname: string };
  getRoomSnapshot: RoomSnapshot;
  listRoomBans: RoomBansPage;
  removeRoomBan: { removed: boolean };
  listRoomMembers: RoomMembersPage;
  setMemberRole: { updated: boolean };
  reportParticipant: { reportId: string; status: 'open' };
  listRoomReports: RoomReportsPage;
  resolveRoomReport: { reportId: string; status: 'resolved' | 'dismissed' };
}

export type SocialRequest = {
  [A in SocialAction]: { type: A; requestId: string } & (SocialRequests[A] extends undefined
    ? object
    : SocialRequests[A]);
}[SocialAction];

export type SocialResponse<A extends SocialAction = SocialAction> = {
  [K in A]: { type: 'socialResponse'; requestId: string; action: K; data: SocialResponses[K] };
}[A];

/** No-payload/page actions may omit data; mutation payloads remain required. */
export type SocialRequestArguments<A extends SocialAction> = {
  [K in A]: SocialRequests[K] extends undefined
    ? [action: K]
    : object extends SocialRequests[K]
      ? [action: K, data?: SocialRequests[K]]
      : [action: K, data: SocialRequests[K]];
}[A];

export interface ChatEntry {
  messageId: string;
  clientMessageId: string;
  participantId: string;
  participantName: string;
  recipientId?: string;
  recipientName?: string;
  content: string;
  sentAt: string;
}

export interface RoomSnapshot {
  participants: ParticipantInfo[];
  messages: ChatEntry[];
  yourRole: string;
  roomSettings?: RoomSettings | null;
  nickname?: string;
  textMuted?: boolean;
  camBanned?: boolean;
  canChat?: boolean;
  canBroadcast?: boolean;
  pausedProducerIds?: string[];
  localProducerIds?: string[];
  allowPrivateMessages: boolean;
  ignoredParticipantIds: string[];
  lobby?: { participantId: string; displayName: string; authenticated: boolean }[];
}

export interface RoomBansPage {
  bans: BanEntry[];
  hasMore: boolean;
}
export interface RoomMembersPage {
  members: MemberEntry[];
  hasMore: boolean;
}
export interface RoomReportsPage {
  reports: ReportEntry[];
  hasMore: boolean;
}

export interface BanEntry {
  banId: string;
  displayName: string;
  reason?: string;
  expiresAt?: string;
  authenticated: boolean;
}

export interface MemberEntry {
  userId: string;
  displayName: string;
  role: string;
  online: boolean;
  authenticated: boolean;
}

export interface ReportEntry {
  reportId: string;
  reporterId: string;
  reporterName: string;
  targetParticipantId: string;
  targetName: string;
  reason: string;
  status: 'open' | 'resolved' | 'dismissed';
  createdAt: string;
  resolvedAt?: string;
}

/** Omitted fields stay unchanged; null clears a password or capacity limit. */
export interface RoomSettingsPatch {
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
}

export interface ProducerMetadata {
  id: string;
  kind: MediaKind;
  source?: string;
}

export type MediaKind = 'audio' | 'video';

// Use the client's actual transport/RTP contracts. Rust's finalized router
// capabilities share the client capability shape on the wire.
export type {
  DtlsParameters,
  RtpParameters,
  RtpCapabilities,
  RtpCapabilities as RtpCapabilitiesFinalized,
  IceParameters,
  IceCandidate,
} from 'mediasoup-client/types';
import type {
  DtlsParameters,
  RtpParameters,
  RtpCapabilities,
  RtpCapabilities as RtpCapabilitiesFinalized,
  IceParameters,
  IceCandidate,
} from 'mediasoup-client/types';

export interface IceServerEntry {
  urls: string[];
  username?: string;
  credential?: string;
}

// --- Auth API types ---

export interface AuthResponse {
  token: string;
  user: UserInfo;
}

export interface UserInfo {
  id: string;
  email: string;
  display_name: string;
}

export interface AuthErrorResponse {
  error: string;
}

export interface RoomListItem {
  id: string;
  display_name: string;
  topic: string | null;
  participant_count: number;
  password_protected: boolean;
  moderated: boolean;
  broadcaster_count: number;
  description: string;
  image_url: string | null;
  secret: boolean;
}

export interface PublicProfile {
  id: string;
  display_name: string;
  avatar_url: string | null;
  bio: string;
}

export interface AccountProfile extends PublicProfile {
  email: string;
  recovery_enabled: boolean;
}

export interface CreateRoomRequest {
  id: string;
  display_name: string;
  password?: string;
  require_registration?: boolean;
  max_participants?: number;
  max_broadcasters?: number;
  moderated?: boolean;
  secret?: boolean;
  lobby_enabled?: boolean;
  guests_allowed?: boolean;
  guests_can_broadcast?: boolean;
  topic?: string;
}
