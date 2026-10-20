// The wire and HTTP types come from the Rust sources `xtask/src/protocol_ts.rs`
// lists, through `cargo run -q -p xtask -- protocol-ts` (web/src/protocol.generated.ts);
// this module adds the TypeScript-side helpers around them.

export * from './protocol.generated';
import { ROLES } from './protocol.generated';
import type {
  ClientMessage,
  ErrorCode,
  Role,
  SettingsPatch,
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
export type RoomSettingsPatch = SettingsPatch;

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
