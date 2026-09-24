/** Closed vocabulary shared with the server; never put user data in events. */
export type TelemetryName =
  | 'auth_restore'
  | 'password_login'
  | 'password_register'
  | 'passkey_login_start'
  | 'passkey_login_ceremony'
  | 'passkey_login_finish'
  | 'passkey_register_start'
  | 'passkey_register_ceremony'
  | 'passkey_register_finish'
  | 'room_join'
  | 'room_admission'
  | 'call_join'
  | 'call_reconnect'
  | 'call_admission'
  | 'chat_send'
  | 'connection'
  | 'reconnect'
  | 'js_error'
  | 'unhandled_rejection'
  | 'media_sample'
  | 'media_first_video_frame'
  | 'media_video_progress'
  | 'media_video_freeze'
  | 'media_audio_concealment'
  | 'media_packet_loss'
  | 'media_rtt';

export type TelemetryOutcome =
  | 'started'
  | 'ok'
  | 'error'
  | 'timeout'
  | 'cancelled_or_timeout'
  | 'unavailable'
  | 'superseded'
  | 'denied'
  | 'unauthenticated'
  | 'normal_close'
  | 'going_away'
  | 'abnormal_close'
  | 'policy_close'
  | 'server_close'
  | 'other_close'
  | 'clean_close'
  | 'unclean_close'
  | 'unknown'
  | 'video_ready'
  | 'audio_playback_ready'
  | 'no_media_expected'
  | 'playback_blocked'
  | 'media_disabled'
  | 'waiting';

export interface TelemetryEvent {
  name: TelemetryName;
  outcome: TelemetryOutcome;
  durationMs?: number;
  /** Local diagnostic correlation only; omitted from network envelopes. */
  attempt?: number;
  value?: number;
}

export type TelemetryHandler = (event: TelemetryEvent) => void;

/** Keys remain local and are never included in telemetry or diagnostic exports. */
export interface TelemetryMediaSource {
  key: object;
  track?: MediaStreamTrack;
  kind: 'audio' | 'video';
  active: () => boolean;
  getStats: () => Promise<RTCStatsReport>;
}

/** Room state and native objects stay local; only finite outcomes are exported. */
export interface TelemetryCallState {
  settled: boolean;
  rosterKnown: boolean;
  expected: number;
  selected: number;
  unavailable: boolean;
}

export type TelemetryCallSignal =
  | { type: 'start'; kind: 'join' | 'reconnect' | 'admission' }
  | { type: 'ready' | 'waiting' | 'failed' | 'superseded' };

export interface TelemetryPlaybackSource {
  source: TelemetryMediaSource;
  element: HTMLMediaElement;
}
