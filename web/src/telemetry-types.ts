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
  kind: 'audio' | 'video';
  active: () => boolean;
  getStats: () => Promise<RTCStatsReport>;
}
