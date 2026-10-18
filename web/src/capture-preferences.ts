export interface CapturePreferences {
  cameraDeviceId: string;
  microphoneDeviceId: string;
  resolution: '360p' | '720p' | '1080p';
  frameRate: 15 | 30 | 60;
  echoCancellation: boolean;
  autoGainControl: boolean;
  noiseSuppression: boolean;
}

export type RemoteVideoQuality = 'auto' | 'low' | 'medium' | 'high';

export type ScreenShareAudio = 'off' | 'sharing' | 'not_provided' | 'failed' | 'ended';
export type ScreenShareResult =
  | { status: 'started'; audio: Exclude<ScreenShareAudio, 'off'> }
  | {
      status: 'not_started';
      reason:
        | 'cancelled_or_denied'
        | 'unavailable'
        | 'not_ready'
        | 'busy'
        | 'superseded'
        | 'not_readable'
        | 'no_source'
        | 'invalid_state'
        | 'failed';
    };

export const DEFAULT_CAPTURE_PREFERENCES: CapturePreferences = {
  cameraDeviceId: '',
  microphoneDeviceId: '',
  resolution: '720p',
  frameRate: 30,
  echoCancellation: true,
  autoGainControl: true,
  noiseSuppression: true,
};

const CAPTURE_STORAGE_KEY = 'simplestchat.capturePreferences';

export function normalizeCapturePreferences(value: unknown): CapturePreferences {
  const preferences = value && typeof value === 'object' ? (value as Record<string, unknown>) : {};
  return {
    cameraDeviceId:
      typeof preferences['cameraDeviceId'] === 'string' ? preferences['cameraDeviceId'] : '',
    microphoneDeviceId:
      typeof preferences['microphoneDeviceId'] === 'string'
        ? preferences['microphoneDeviceId']
        : '',
    resolution:
      preferences['resolution'] === '360p' || preferences['resolution'] === '1080p'
        ? preferences['resolution']
        : '720p',
    frameRate:
      preferences['frameRate'] === 15 || preferences['frameRate'] === 60
        ? preferences['frameRate']
        : 30,
    echoCancellation: preferences['echoCancellation'] !== false,
    autoGainControl: preferences['autoGainControl'] !== false,
    noiseSuppression: preferences['noiseSuppression'] !== false,
  };
}

export function loadCapturePreferences(): CapturePreferences {
  try {
    return normalizeCapturePreferences(
      JSON.parse(localStorage.getItem(CAPTURE_STORAGE_KEY) ?? 'null'),
    );
  } catch {
    return { ...DEFAULT_CAPTURE_PREFERENCES };
  }
}

export function saveCapturePreferences(preferences: CapturePreferences): void {
  try {
    localStorage.setItem(
      CAPTURE_STORAGE_KEY,
      JSON.stringify(normalizeCapturePreferences(preferences)),
    );
  } catch {
    /* Storage may be unavailable. */
  }
}

/** The preview and publisher use the same requested capture settings. */
export function captureConstraints(
  preferences: CapturePreferences,
  kind: 'audio' | 'video',
): MediaTrackConstraints {
  if (kind === 'audio') {
    return {
      ...(preferences.microphoneDeviceId && {
        deviceId: { exact: preferences.microphoneDeviceId },
      }),
      echoCancellation: preferences.echoCancellation,
      autoGainControl: preferences.autoGainControl,
      noiseSuppression: preferences.noiseSuppression,
    };
  }
  const [width, height] =
    preferences.resolution === '360p'
      ? [640, 360]
      : preferences.resolution === '1080p'
        ? [1920, 1080]
        : [1280, 720];
  return {
    ...(preferences.cameraDeviceId && { deviceId: { exact: preferences.cameraDeviceId } }),
    width: { ideal: width },
    height: { ideal: height },
    frameRate: { ideal: preferences.frameRate },
  };
}

/** Some cameras cannot start in the browser's preferred format, even with ideal constraints. */
export async function captureMedia(
  constraints: MediaStreamConstraints,
  isCurrent: () => boolean,
): Promise<MediaStream> {
  try {
    return await navigator.mediaDevices.getUserMedia(constraints);
  } catch (error) {
    const video = constraints.video;
    if (
      !isCurrent() ||
      !error ||
      typeof error !== 'object' ||
      !('name' in error) ||
      error.name !== 'NotReadableError' ||
      !video ||
      typeof video !== 'object' ||
      !('width' in video || 'height' in video || 'frameRate' in video)
    ) {
      console.warn('[media] capture failed', error);
      throw error;
    }
    // Retain the selected device and audio request; only relax camera quality.
    const fallback = { ...video };
    delete fallback.width;
    delete fallback.height;
    delete fallback.frameRate;
    console.warn(
      '[media] camera could not start at preferred quality; trying browser defaults',
      error,
    );
    try {
      return await navigator.mediaDevices.getUserMedia({
        ...constraints,
        video: Object.keys(fallback).length ? fallback : true,
      });
    } catch (fallbackError) {
      console.warn('[media] camera capture failed with browser defaults', fallbackError);
      throw fallbackError;
    }
  }
}
