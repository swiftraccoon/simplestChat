// Signaling protocol types — mirrors src/signaling/protocol.rs exactly.
// Rust uses #[serde(tag = "type", rename_all = "camelCase")]

// --- Client → Server ---

export type ClientMessage =
  | { type: 'joinRoom'; roomId: string; participantName: string }
  | { type: 'leaveRoom' }
  | { type: 'getRouterRtpCapabilities' }
  | { type: 'createSendTransport' }
  | { type: 'createRecvTransport' }
  | { type: 'connectTransport'; transportId: string; dtlsParameters: DtlsParameters }
  | { type: 'produce'; transportId: string; kind: MediaKind; rtpParameters: RtpParameters; source?: string }
  | { type: 'consume'; producerId: string; rtpCapabilities: RtpCapabilities }
  | { type: 'resumeConsumer'; consumerId: string }
  | { type: 'pauseConsumer'; consumerId: string }
  | { type: 'closeProducer'; producerId: string }
  | { type: 'pauseProducer'; producerId: string }
  | { type: 'resumeProducer'; producerId: string }
  | { type: 'reconnect'; participantId: string; roomId: string; reconnectToken: string }
  | { type: 'restartIce'; transportId: string }
  | { type: 'setConsumerPreferredLayers'; consumerId: string; spatialLayer: number; temporalLayer?: number }
  | { type: 'chatMessage'; content: string }
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
  | { type: 'updateRoomSettings'; [key: string]: unknown }
  | { type: 'setTopic'; topic: string }
  // Lobby
  | { type: 'admitFromLobby'; targetParticipantId: string }
  | { type: 'denyFromLobby'; targetParticipantId: string };

// --- Server → Client ---

export type ServerMessage =
  | { type: 'roomJoined'; participantId: string; participants: ParticipantInfo[]; reconnectToken: string; yourRole: string; roomSettings?: RoomSettings }
  | { type: 'error'; message: string }
  | { type: 'routerRtpCapabilities'; rtpCapabilities: RtpCapabilitiesFinalized }
  | { type: 'transportCreated'; transportId: string; iceParameters: IceParameters; iceCandidates: IceCandidate[]; dtlsParameters: DtlsParameters; iceServers?: IceServerEntry[] }
  | { type: 'transportConnected'; transportId: string }
  | { type: 'producerCreated'; producerId: string }
  | { type: 'consumerCreated'; consumerId: string; producerId: string; kind: MediaKind; rtpParameters: RtpParameters }
  | { type: 'participantJoined'; participantId: string; participantName: string; role: string; authenticated: boolean }
  | { type: 'participantLeft'; participantId: string }
  | { type: 'newProducer'; participantId: string; producerId: string; kind: MediaKind; source?: string }
  | { type: 'producerClosed'; producerId: string }
  | { type: 'producerPaused'; producerId: string }
  | { type: 'producerResumed'; producerId: string }
  | { type: 'consumerResumed'; consumerId: string }
  | { type: 'consumerPaused'; consumerId: string }
  | { type: 'reconnectResult'; success: boolean; participantId: string }
  | { type: 'iceRestarted'; transportId: string; iceParameters: IceParameters }
  | { type: 'connectionStats'; availableBitrate: number | null; rtt: number | null }
  | { type: 'consumerLayersChanged'; consumerId: string; spatialLayer: number | null; temporalLayer: number | null }
  | { type: 'chatReceived'; participantId: string; participantName: string; content: string }
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

// mediasoup types — these are opaque JSON objects passed through
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type DtlsParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpCapabilities = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpCapabilitiesFinalized = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type IceParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type IceCandidate = any;

export interface IceServerEntry {
  urls: string[];
  username?: string;
  credential?: string;
}
