/** Incoming video choices never change publishing, audio subscriptions, or tile size. */
export type ReceiveMode = 'balanced' | 'data-saver' | 'audio-only';
export type VideoDeferredReason = 'offscreen' | 'limit' | 'audio-only' | 'hidden';

export const VIDEO_RECEIVE_LIMITS: Readonly<Record<ReceiveMode, number>> = {
  balanced: 9,
  'data-saver': 4,
  'audio-only': 0,
};
const STORAGE_KEY = 'simplestchat.receiveMode';
const FOLLOW_SPEAKER_KEY = 'simplestchat.followSpeaker';

export function loadFollowSpeaker(): boolean {
  try {
    return localStorage.getItem(FOLLOW_SPEAKER_KEY) === 'true';
  } catch {
    return false;
  }
}

export function saveFollowSpeaker(enabled: boolean): void {
  try {
    localStorage.setItem(FOLLOW_SPEAKER_KEY, String(enabled));
  } catch {
    /* The choice remains effective in this tab. */
  }
}

/** A new speaker must settle; retained focus changes at most once every five seconds. */
export class SpeakerFollow {
  private readonly changed: (participantId: string | null) => void;
  private readonly now: () => number;

  private focused: string | null = null;
  private candidate: string | null = null;
  private changedAt = -Infinity;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    changed: (participantId: string | null) => void,
    now: () => number = () => performance.now(),
  ) {
    this.changed = changed;
    this.now = now;
  }

  get participantId(): string | null {
    return this.focused;
  }

  note(participantId: string | null): void {
    if (participantId !== null && participantId === this.candidate) return;
    this.cancelPending();
    if (participantId === null || participantId === this.focused) return;
    this.candidate = participantId;
    this.timer = setTimeout(
      () => {
        this.timer = null;
        this.candidate = null;
        this.focused = participantId;
        this.changedAt = this.now();
        this.changed(participantId);
      },
      Math.max(1_500, this.changedAt + 5_000 - this.now()),
    );
  }

  forget(participantId: string): void {
    if (this.focused === participantId) this.reset();
    else if (this.candidate === participantId) this.cancelPending();
  }

  reset(): void {
    this.cancelPending();
    this.changedAt = -Infinity;
    if (this.focused === null) return;
    this.focused = null;
    this.changed(null);
  }

  private cancelPending(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.candidate = null;
  }
}

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
  source?: string | undefined;
  visible?: boolean | undefined;
  pinned: boolean;
  hidden: boolean;
  pictureInPicture?: boolean;
  followedSpeaker?: boolean;
}

/** Explicit choices lead, then visible shares and the followed camera, then stable ties. */
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
            (pageActive &&
              (candidate.visible !== false || candidate.pinned || candidate.followedSpeaker))),
      )
      .sort(
        (left, right) =>
          Number(Boolean(right.pictureInPicture)) - Number(Boolean(left.pictureInPicture)) ||
          Number(right.pinned) - Number(left.pinned) ||
          Number(right.visible === true || Boolean(right.followedSpeaker)) -
            Number(left.visible === true || Boolean(left.followedSpeaker)) ||
          Number(right.source === 'screen') - Number(left.source === 'screen') ||
          Number(Boolean(right.followedSpeaker)) - Number(Boolean(left.followedSpeaker)) ||
          Number(current.has(right.id)) - Number(current.has(left.id)),
      )
      .slice(0, VIDEO_RECEIVE_LIMITS[mode])
      .map((candidate) => candidate.id),
  );
}
