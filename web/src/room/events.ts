import type { TelemetryCallSignal, TelemetryHandler } from '../telemetry-types';
import type { ChatStyle, Role, RoomSettingsView, ServerMessage } from '../protocol';
import type { VideoDeferredReason } from '../receive-policy';

export interface Participant {
  id: string;
  name: string;
  role: Role;
  authenticated?: boolean;
  chatStyle?: ChatStyle;
  producers: Map<string, { kind: 'audio' | 'video'; source?: string }>;
}

export type ConnectionQuality = 'good' | 'fair' | 'poor' | 'unknown';

export type RoomEventHandler = {
  onTelemetry?: TelemetryHandler;
  onCallSignal?: (signal: TelemetryCallSignal) => void;
  onParticipantsChanged: (participants: Map<string, Participant>) => void;
  onLocalStream: (stream: MediaStream) => void;
  onLocalMediaChanged: () => void;
  onLocalCaptureStopped?: (kind: 'audio' | 'video') => void;
  /** The camera is live but sending nothing (true), or sending again (false). */
  onLocalVideoStalled?: (stalled: boolean) => void;
  onRemoteTrack: (
    participantId: string,
    participantName: string,
    track: MediaStreamTrack,
    kind: 'audio' | 'video',
    source?: string,
  ) => void;
  onRemoteTrackRemoved: (
    participantId: string,
    producerId: string,
    kind: 'audio' | 'video',
    source?: string,
  ) => void;
  /** Preserve the same tile while this viewer deliberately leaves its video unsubscribed. */
  onRemoteVideoDeferred?: (
    participantId: string,
    participantName: string,
    source: string | undefined,
    reason: VideoDeferredReason,
  ) => void;
  /** The server or media stack refused a subscription; the tile would otherwise stay blank silently. */
  onRemoteMediaUnavailable?: (
    participantId: string,
    participantName: string,
    kind: 'audio' | 'video',
    source: string | undefined,
    reason: string,
  ) => void;
  onParticipantLeft: (participantId: string, participantName?: string) => void;
  onParticipantJoined: (participantId: string, participantName: string) => void;
  onChatMessage: (participantId: string, participantName: string, content: string) => void;
  onConnectionQuality: (quality: ConnectionQuality) => void;
  onActiveSpeaker: (participantId: string) => void;
  /** Stable, opt-in camera focus; this changes selection and ordering, never tile dimensions. */
  onFollowedSpeakerChanged?: (participantId: string | null) => void;
  onAudioLevels: (levels: { participantId: string; volume: number }[]) => void;
  onModeration: (action: string, participantId: string, reason?: string) => void;
  onRoleChanged: (participantId: string, newRole: Role) => void;
  onRoomSettingsChanged: (settings: RoomSettingsView) => void;
  onTopicChanged: (topic: string, changedBy: string) => void;
  onVoiceRequested: (participantId: string, displayName: string) => void;
  onLobbyWaiting: (
    roomName: string,
    topic: string | undefined,
    count: number,
    moderators: number,
  ) => void;
  onLobbyStatus?: (count: number, moderators: number) => void;
  onLobbyJoin: (participantId: string, displayName: string) => void;
  onLobbyAdmitted: () => void;
  onLobbyDenied: (reason?: string) => void;
  /** Fired after post-lobby-admission media setup finishes (room state + media are ready) */
  onAdmissionComplete: () => void;
  onSocialEvent?: (message: ServerMessage) => void;
  onRecoveryState?: (state: 'reconnecting' | 'connected' | 'failed', message?: string) => void;
  onPasswordRequired?: () => Promise<string | null>;
  onRoomClosed?: (reason: string) => void;
  /** Unexpected asynchronous failures belonging to the current membership only. */
  onBackgroundError?: (message: string) => void;
};
