// Signaling protocol: the wire types come from src/signaling/protocol.rs through
// `cargo run -q -p xtask -- protocol-ts` (web/src/protocol.generated.ts); this
// module adds the TypeScript-side helpers around them.

export * from './protocol.generated';
import { ROLES } from './protocol.generated';
import type {
  ChatStyle,
  ClientMessage,
  ErrorCode,
  Role,
  SocialResponses,
} from './protocol.generated';

// --- Requests and acknowledgements ---
/** Only acknowledged commands use SignalingClient.request; events use send. */
export interface RequestResponses {
  getRouterRtpCapabilities: 'routerRtpCapabilities';
  createSendTransport: 'transportCreated';
  createRecvTransport: 'transportCreated';
  replaceRecvTransport: 'transportCreated';
  connectTransport: 'transportConnected';
  produce: 'producerCreated';
  consume: 'consumerCreated';
  resumeConsumer: 'consumerResumed';
  pauseConsumer: 'consumerPaused';
  closeConsumer: 'mediaControlApplied';
  closeProducer: 'mediaControlApplied';
  setConsumerPreferredLayers: 'mediaControlApplied';
  pauseProducer: 'producerPaused';
  resumeProducer: 'producerResumed';
  reconnect: 'reconnectResult';
  restartIce: 'iceRestarted';
  closeCam: 'roomControlApplied';
  camBan: 'roomControlApplied';
  camUnban: 'roomControlApplied';
  textMute: 'roomControlApplied';
  textUnmute: 'roomControlApplied';
  kick: 'roomControlApplied';
  ban: 'roomControlApplied';
  unban: 'roomControlApplied';
  setRole: 'roomControlApplied';
  requestVoice: 'roomControlApplied';
  updateRoomSettings: 'roomControlApplied';
  setTopic: 'roomControlApplied';
  admitFromLobby: 'roomControlApplied';
  denyFromLobby: 'roomControlApplied';
}

/** Mutations acknowledge application separately from their room broadcasts. */
export type RoomControl = Extract<
  ClientMessage,
  {
    type: {
      [Type in keyof RequestResponses]: RequestResponses[Type] extends 'roomControlApplied'
        ? Type
        : never;
    }[keyof RequestResponses];
  }
>;

// --- Shared types ---

// --- Shared types ---

/**
 * A refusal the server sent: an `Error` whose `name` is `ServerError` and that
 * carries the code clients switch on. Built with `Object.assign` where the
 * frame is received, so this module stays type-only for its importers.
 */
export interface ServerError extends Error {
  readonly code: ErrorCode;
}

/** A participant's standing in a room, least to most trusted; the wire carries the name. */

/** Position of a role in the trust order. */
export const rank = (role: Role): number => ROLES.indexOf(role);
/** The role a select or query string names, or undefined. */
export const asRole = (value: string): Role | undefined =>
  (ROLES as readonly string[]).includes(value) ? (value as Role) : undefined;

/** Paged social listings, named for the code that walks them. */
export type RoomBansPage = SocialResponses['listRoomBans'];
export type RoomMembersPage = SocialResponses['listRoomMembers'];
export type RoomReportsPage = SocialResponses['listRoomReports'];
export type ModerationEventsPage = SocialResponses['listModerationEvents'];

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
  /** `null` when the room's state was busy at listing time; never render as zero. */
  participant_count: number | null;
  password_protected: boolean;
  moderated: boolean;
  /** `null` when the room's state was busy at listing time; never render as zero. */
  broadcaster_count: number | null;
  description: string;
  image_url: string | null;
  secret: boolean;
  name_style: ChatStyle;
  topic_style: ChatStyle;
}

export interface PublicProfile {
  id: string;
  display_name: string;
  avatar_url: string | null;
  bio: string;
  profile_style: ChatStyle;
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
