import type { types as MediaTypes } from 'mediasoup-client';

export type ScreenPresentationMode = 'detail' | 'motion';

const STORAGE_KEY = 'simplestchat.screenPresentationMode';
let volatileMode: ScreenPresentationMode = 'detail';
let unsavedChoice = false;

/** Reading this preference never acquires a display or starts a producer. */
export function loadScreenPresentationMode(): ScreenPresentationMode {
  if (unsavedChoice) return volatileMode;
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    volatileMode = saved === 'motion' ? 'motion' : 'detail';
  } catch {
    // Keep this tab's explicit choice when storage is unavailable.
  }
  return volatileMode;
}

export function saveScreenPresentationMode(mode: ScreenPresentationMode): void {
  volatileMode = mode === 'motion' ? 'motion' : 'detail';
  unsavedChoice = true;
  try {
    localStorage.setItem(STORAGE_KEY, volatileMode);
    unsavedChoice = false;
  } catch {
    /* The choice still applies in this tab. */
  }
}

// The send transport already has a 3 Mbps server cap. Leave room for the
// camera's two lower streams (400 kbps), microphone and optional display audio
// (64 kbps each), plus retransmission/transport overhead.
export const SCREEN_VIDEO_BITRATE = 2_200_000;
export const PRESENTATION_AUDIO_BITRATE = 64_000;
export const PRESENTING_CAMERA_SPATIAL_LAYER = 1;
export const NORMAL_CAMERA_SPATIAL_LAYER = 2;

export function screenCaptureConstraints(mode: ScreenPresentationMode): DisplayMediaStreamOptions {
  return {
    video: {
      width: { max: 1920 },
      height: { max: 1080 },
      frameRate: { ideal: mode === 'motion' ? 30 : 15, max: mode === 'motion' ? 30 : 15 },
    },
    audio: true,
  };
}

export function screenContentHint(mode: ScreenPresentationMode): string {
  return mode === 'motion' ? 'motion' : 'detail';
}

/** Only query SVC for an already negotiated default VP9 codec. Codec presence
 * alone does not advertise scalability support in SDP. Older browsers reject
 * WebRTC MediaCapabilities queries and use a bounded single-stream fallback. */
export async function supportsVp9ScreenSvc(
  codecs: MediaTypes.RtpCodecCapability[],
  mode: ScreenPresentationMode,
): Promise<boolean> {
  const first = codecs.find((codec) => /^video\/(?!rtx$)/i.test(codec.mimeType));
  if (first?.mimeType.toLowerCase() !== 'video/vp9') return false;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    const result = await Promise.race([
      navigator.mediaCapabilities.encodingInfo({
        type: 'webrtc',
        video: {
          contentType: 'video/VP9',
          width: 1920,
          height: 1080,
          bitrate: SCREEN_VIDEO_BITRATE,
          framerate: mode === 'motion' ? 30 : 15,
          scalabilityMode: 'L3T3_KEY',
        },
      }),
      new Promise<null>((resolve) => {
        timeout = setTimeout(() => resolve(null), 1000);
      }),
    ]);
    return result !== null && result.supported && result.smooth;
  } catch {
    return false;
  } finally {
    clearTimeout(timeout);
  }
}

/** Lowest-to-highest encodings; mediasoup assigns RID and active itself.
 * Retain the negotiated codec order instead of forcing a new codec for sharing.
 * H264 hardware encoders may still deliver one stream; the receiver treats
 * their actual RTP parameters as authoritative. */
export function screenEncodingOptions(
  mode: ScreenPresentationMode,
  codecs: MediaTypes.RtpCodecCapability[],
  vp9Svc: boolean,
): { encodings: MediaTypes.RtpEncodingParameters[] } {
  const first = codecs.find((codec) => /^video\/(?!rtx$)/i.test(codec.mimeType));
  const mime = first?.mimeType.toLowerCase();
  const maxFramerate = mode === 'motion' ? 30 : 15;
  if (mime === 'video/vp8' || mime === 'video/h264') {
    const bitrates =
      mode === 'motion' ? [160_000, 540_000, 1_500_000] : [120_000, 380_000, 1_700_000];
    return {
      encodings: bitrates.map((maxBitrate, index) => ({
        maxBitrate,
        scaleResolutionDownBy: 2 ** (2 - index),
        maxFramerate,
        dtx: mode === 'detail',
      })),
    };
  }
  return {
    encodings: [
      {
        maxBitrate: SCREEN_VIDEO_BITRATE,
        maxFramerate,
        ...(mime === 'video/vp9' && vp9Svc ? { scalabilityMode: 'L3T3_KEY' } : {}),
      },
    ],
  };
}
