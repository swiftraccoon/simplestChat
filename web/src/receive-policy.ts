/** Incoming video choices never change publishing, audio subscriptions, or tile size. */
export type ReceiveMode = 'balanced' | 'data-saver' | 'audio-only';
export type VideoDeferredReason = 'offscreen' | 'limit' | 'audio-only' | 'hidden';

export const VIDEO_RECEIVE_LIMITS: Readonly<Record<ReceiveMode, number>> = {
  balanced: 9,
  'data-saver': 4,
  'audio-only': 0,
};
const STORAGE_KEY = 'simplestchat.receiveMode';

export function normalizeReceiveMode(value: unknown): ReceiveMode {
  return value === 'data-saver' || value === 'audio-only' ? value : 'balanced';
}

export function loadReceiveMode(): ReceiveMode {
  try {
    return normalizeReceiveMode(localStorage.getItem(STORAGE_KEY));
  } catch {
    return 'balanced';
  }
}

export function saveReceiveMode(mode: ReceiveMode): void {
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    /* The choice remains effective in this tab. */
  }
}

export function videoTileKey(participantId: string, source?: string): string {
  return source === 'screen' ? `${participantId}:screen` : participantId;
}

export interface ReceiveCandidate {
  id: string;
  visible?: boolean | undefined;
  pinned: boolean;
  hidden: boolean;
  pictureInPicture?: boolean;
}

/** Stable ties retain subscriptions instead of rotating videos on every update. */
export function selectIncomingVideos(
  candidates: readonly ReceiveCandidate[],
  mode: ReceiveMode,
  pageActive: boolean,
  current: ReadonlySet<string>,
): Set<string> {
  if (mode === 'audio-only') return new Set();
  return new Set(
    candidates
      .filter(
        (candidate) =>
          !candidate.hidden &&
          (candidate.pictureInPicture ||
            (pageActive && (candidate.visible !== false || candidate.pinned))),
      )
      .sort(
        (left, right) =>
          Number(Boolean(right.pictureInPicture)) - Number(Boolean(left.pictureInPicture)) ||
          Number(right.pinned) - Number(left.pinned) ||
          Number(right.visible === true) - Number(left.visible === true) ||
          Number(current.has(right.id)) - Number(current.has(left.id)),
      )
      .slice(0, VIDEO_RECEIVE_LIMITS[mode])
      .map((candidate) => candidate.id),
  );
}
