import type {
  TelemetryCallSignal,
  TelemetryCallState,
  TelemetryHandler,
  TelemetryMediaSource,
  TelemetryName,
  TelemetryOutcome,
  TelemetryPlaybackSource,
} from './telemetry-types';

type NumericStats = Record<string, unknown>;
export interface Sample {
  frames?: number | undefined;
  freezeSeconds?: number | undefined;
  concealed?: number | undefined;
  samples?: number | undefined;
  lost?: number | undefined;
  received?: number | undefined;
}
interface SourceState {
  previous: Sample | undefined;
}

function number(stats: NumericStats, key: string): number | undefined {
  const value = stats[key];
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined;
}

function delta(current: number | undefined, previous: number | undefined): number | undefined {
  return current !== undefined && previous !== undefined && current >= previous
    ? current - previous
    : undefined;
}

/** Only inspect named numeric fields: never serialize an RTCStatsReport. */
export function readMediaSample(report: RTCStatsReport, kind: 'audio' | 'video'): Sample {
  let result: Sample = {};
  report.forEach((raw: unknown) => {
    if (typeof raw !== 'object' || raw === null) return;
    const stats = raw as NumericStats;
    if (stats['type'] !== 'inbound-rtp' || (stats['kind'] ?? stats['mediaType']) !== kind) return;
    result = {
      frames: number(stats, 'framesDecoded'),
      freezeSeconds: number(stats, 'totalFreezesDuration'),
      concealed: number(stats, 'concealedSamples'),
      samples: number(stats, 'totalSamplesReceived'),
      lost: number(stats, 'packetsLost'),
      received: number(stats, 'packetsReceived'),
    };
  });
  return result;
}

export function sampleChanges(
  current: Sample,
  previous: Sample | undefined,
): {
  frames: number | undefined;
  freezeMs: number | undefined;
  concealment: number | undefined;
  packetLoss: number | undefined;
} {
  const concealed = delta(current.concealed, previous?.concealed);
  const samples = delta(current.samples, previous?.samples);
  const lost = delta(current.lost, previous?.lost);
  const received = delta(current.received, previous?.received);
  const freeze = delta(current.freezeSeconds, previous?.freezeSeconds);
  return {
    frames: delta(current.frames, previous?.frames),
    freezeMs: freeze === undefined ? undefined : freeze * 1000,
    // Silence/DTX or unsupported counters are unknown, never a fabricated zero.
    concealment:
      concealed !== undefined && samples !== undefined && samples > 0 && concealed <= samples
        ? (concealed / samples) * 10_000
        : undefined,
    packetLoss:
      lost !== undefined && received !== undefined && lost + received > 0
        ? (lost / (lost + received)) * 10_000
        : undefined,
  };
}

/** Best effort client observations never drive server admission or media decisions. */
export class MediaTelemetry {
  private readonly states = new Map<object, SourceState>();
  private readonly inFlight = new WeakSet<object>();
  private offset = 0;
  private pendingCount = 0;
  private disposed = false;
  private ticks = 0;
  private readonly visibilityChanged = (): void => {
    this.states.clear();
  };
  private readonly timer: ReturnType<typeof setInterval>;

  constructor(
    private readonly sources: () => TelemetryMediaSource[],
    private readonly record: TelemetryHandler,
    private readonly calls?: CallOutcomeTelemetry,
  ) {
    this.record = (event) => {
      try {
        record(event);
      } catch {
        /* Keep the media path independent of reporting. */
      }
    };
    document.addEventListener('visibilitychange', this.visibilityChanged);
    this.timer = setInterval(() => {
      this.ticks += 1;
      const quality = this.ticks % 15 === 0;
      if (this.calls?.pending || quality) this.sample(quality);
    }, 1_000);
  }

  sample(quality = true): void {
    try {
      this.sampleSources(quality);
    } catch {
      this.record({ name: 'media_sample', outcome: 'unknown' });
    }
  }

  private sampleSources(quality: boolean): void {
    if (this.disposed) return;
    if (document.visibilityState !== 'visible') {
      this.states.clear();
      return;
    }
    const sources = this.sources();
    const active = sources.filter((source) => source.active());
    const keys = new Set(active.map((source) => source.key));
    for (const key of this.states.keys()) if (!keys.has(key)) this.states.delete(key);
    if (active.length === 0) return;
    if (this.pendingCount >= 4) {
      if (quality) this.record({ name: 'media_sample', outcome: 'unavailable' });
      return;
    }
    for (let index = 0; index < Math.min(4, active.length); index += 1) {
      const source = active[(this.offset + index) % active.length]!;
      if (this.pendingCount >= 4 || this.inFlight.has(source.key)) continue;
      let state = this.states.get(source.key);
      if (!state) {
        // Maximum retained observations is independent of room size.
        if (this.states.size >= 64) this.states.delete(this.states.keys().next().value!);
        state = { previous: undefined };
        this.states.set(source.key, state);
      }
      this.collect(source, state, quality);
    }
    this.offset = (this.offset + 4) % active.length;
  }

  private collect(source: TelemetryMediaSource, state: SourceState, quality: boolean): void {
    this.inFlight.add(source.key);
    this.pendingCount += 1;
    let expired = false;
    const current = (): boolean => {
      try {
        return (
          !this.disposed &&
          !expired &&
          document.visibilityState === 'visible' &&
          source.active() &&
          this.states.get(source.key) === state
        );
      } catch {
        return false;
      }
    };
    const timeout = setTimeout(() => {
      if (quality && current()) this.record({ name: 'media_sample', outcome: 'timeout' });
      expired = true;
      state.previous = undefined;
    }, 2000);
    // A hung native getStats keeps this source busy; do not stack abandoned calls.
    Promise.resolve()
      .then(() => source.getStats())
      .then((report) => {
        if (!current()) return;
        const sample = readMediaSample(report, source.kind);
        this.calls?.sample(source, sample);
        if (!quality) return;
        this.record({ name: 'media_sample', outcome: 'ok' });
        const changes = sampleChanges(sample, state.previous);
        const emit = (name: TelemetryName, value: number | undefined): void => {
          this.record({
            name,
            outcome: value === undefined ? 'unknown' : 'ok',
            ...(value === undefined ? {} : { value }),
          });
        };
        if (source.kind === 'video') {
          emit('media_video_progress', changes.frames);
          emit('media_video_freeze', changes.freezeMs);
        } else emit('media_audio_concealment', changes.concealment);
        emit('media_packet_loss', changes.packetLoss);
        let rtt: number | undefined;
        report.forEach((raw: unknown) => {
          if (typeof raw !== 'object' || raw === null) return;
          const stats = raw as NumericStats;
          const selected = stats['selectedCandidatePairId'];
          if (stats['type'] !== 'transport' || typeof selected !== 'string') return;
          const pair: unknown = report.get(selected);
          if (typeof pair !== 'object' || pair === null) return;
          const selectedPair = pair as NumericStats;
          if (selectedPair['type'] === 'candidate-pair')
            rtt = number(selectedPair, 'currentRoundTripTime');
        });
        emit('media_rtt', rtt === undefined ? undefined : rtt * 1000);
        state.previous = sample;
      })
      .catch(() => {
        if (quality && current()) this.record({ name: 'media_sample', outcome: 'error' });
        state.previous = undefined;
      })
      .finally(() => {
        clearTimeout(timeout);
        this.inFlight.delete(source.key);
        this.pendingCount -= 1;
      })
      .catch(() => {
        /* A telemetry callback cannot escape into application code. */
      });
  }

  dispose(): void {
    this.disposed = true;
    clearInterval(this.timer);
    document.removeEventListener('visibilitychange', this.visibilityChanged);
    this.states.clear();
  }
}

interface CallAttempt {
  name: 'call_join' | 'call_reconnect' | 'call_admission';
  id: number;
  started: number;
  wallDeadline: number;
  frames: Map<HTMLVideoElement, { track: MediaStreamTrack; handle: number }>;
  decoded: Set<object>;
  samples: Map<object, number>;
  blocked: Set<HTMLMediaElement>;
  incomplete: boolean;
}

const CALL_DEADLINE_MS = 30_000;
const MAX_CALL_SOURCES = 64;

/** First observed usable received media, not proof that the user heard sound.
 * Audio sampling shares MediaTelemetry's opt-in, globally bounded native calls.
 */
export class CallOutcomeTelemetry {
  private attempt: CallAttempt | null = null;
  private disposed = false;
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly visibilityChanged = (): void => {
    if (document.visibilityState !== 'visible') this.finish('unknown');
  };

  constructor(
    private readonly state: () => TelemetryCallState,
    private readonly playback: () => TelemetryPlaybackSource[],
    private readonly record: TelemetryHandler,
    private readonly nextId: () => number,
  ) {
    this.timer = setInterval(() => this.check(), 1_000);
    document.addEventListener('visibilitychange', this.visibilityChanged);
  }

  get pending(): boolean {
    return this.attempt !== null;
  }

  signal(signal: TelemetryCallSignal): void {
    if (this.disposed) return;
    try {
      if (signal.type === 'start') {
        this.finish('superseded');
        const attempt: CallAttempt = {
          name: `call_${signal.kind}`,
          id: this.nextId(),
          started: performance.now(),
          wallDeadline: Date.now() + CALL_DEADLINE_MS,
          frames: new Map(),
          decoded: new Set(),
          samples: new Map(),
          blocked: new Set(),
          incomplete: false,
        };
        this.attempt = attempt;
        this.emit({ name: attempt.name, outcome: 'started', attempt: attempt.id });
        this.check();
      } else if (signal.type === 'waiting') this.finish('waiting');
      else if (signal.type === 'failed') this.finish('error');
      else if (signal.type === 'superseded') this.finish('superseded');
      else this.check();
    } catch {
      this.finish('unknown');
    }
  }

  playbackResult(element: HTMLMediaElement, blocked: boolean): void {
    const attempt = this.attempt;
    if (!attempt) return;
    if (blocked && attempt.blocked.size < MAX_CALL_SOURCES) attempt.blocked.add(element);
    else attempt.blocked.delete(element);
    this.check();
  }

  sample(source: TelemetryMediaSource, sample: Sample): void {
    const attempt = this.attempt;
    if (!attempt || source.kind !== 'audio') return;
    try {
      if (!source.active() || !this.state().settled) return;
      const decoded =
        sample.samples !== undefined &&
        sample.concealed !== undefined &&
        sample.samples >= sample.concealed
          ? sample.samples - sample.concealed
          : undefined;
      if (decoded !== undefined) {
        const previous = attempt.samples.get(source.key);
        if (previous !== undefined && decoded > previous) attempt.decoded.add(source.key);
        if (attempt.samples.size < MAX_CALL_SOURCES || attempt.samples.has(source.key))
          attempt.samples.set(source.key, decoded);
        else attempt.incomplete = true;
      }
      this.check();
    } catch {
      this.finish('unknown');
    }
  }

  check(): void {
    const attempt = this.attempt;
    if (!attempt) return;
    try {
      if (document.visibilityState !== 'visible') {
        this.finish('unknown');
        return;
      }
      const state = this.state();
      const expired = this.expired(attempt);
      const all = this.playback();
      const sources = all.slice(0, MAX_CALL_SOURCES);
      if (all.length > MAX_CALL_SOURCES) attempt.incomplete = true;
      const activeElements = new Set(sources.map(({ element }) => element));
      const activeKeys = new Set(
        sources
          .filter(
            ({ source, element }) =>
              this.currentSource(source, element) &&
              source.track?.enabled &&
              !(source.kind === 'audio' && (element.muted || element.volume === 0)),
          )
          .map(({ source }) => source.key),
      );
      for (const key of attempt.samples.keys())
        if (!activeKeys.has(key)) {
          attempt.samples.delete(key);
          attempt.decoded.delete(key);
        }
      for (const element of attempt.blocked) {
        if (!activeElements.has(element) || !element.paused) attempt.blocked.delete(element);
      }
      for (const [element, frame] of attempt.frames) {
        if (
          !sources.some(
            ({ element: current, source }) => current === element && source.track === frame.track,
          )
        ) {
          element.cancelVideoFrameCallback(frame.handle);
          attempt.frames.delete(element);
        }
      }
      if (!expired && state.settled && state.rosterKnown && state.expected === 0) {
        this.finish('no_media_expected');
        return;
      }
      if (!expired && state.settled && state.rosterKnown && state.selected === 0) {
        this.finish('media_disabled');
        return;
      }
      let enabled = 0;
      let disabled = 0;
      let unsupported = false;
      let audio = false;
      for (const { source, element } of sources) {
        const track = source.track;
        if (!track || !this.currentSource(source, element)) continue;
        if (
          !track.enabled ||
          (source.kind === 'audio' && (element.muted || element.volume === 0))
        ) {
          disabled++;
          continue;
        }
        enabled++;
        if (source.kind === 'audio') {
          audio = true;
          if (
            !expired &&
            state.settled &&
            attempt.decoded.has(source.key) &&
            !element.paused &&
            element.readyState >= 2
          ) {
            this.finish('audio_playback_ready');
            return;
          }
          continue;
        }
        const video = element as HTMLVideoElement;
        if (typeof video.requestVideoFrameCallback !== 'function') {
          unsupported = true;
          continue;
        }
        if (!expired && !attempt.frames.has(video)) {
          const handle = video.requestVideoFrameCallback(() => {
            if (this.attempt !== attempt) return;
            attempt.frames.delete(video);
            try {
              if (this.expired(attempt)) this.check();
              else if (
                this.state().settled &&
                document.visibilityState === 'visible' &&
                this.currentSource(source, video) &&
                track.enabled &&
                !video.paused
              )
                this.finish('video_ready');
            } catch {
              this.finish('unknown');
            }
          });
          attempt.frames.set(video, { track, handle });
        }
      }
      if (expired) {
        const outcome: TelemetryOutcome =
          !state.rosterKnown || attempt.incomplete
            ? 'unknown'
            : attempt.blocked.size > 0
              ? 'playback_blocked'
              : state.unavailable
                ? 'unavailable'
                : disabled > 0 && disabled >= state.selected && enabled === 0
                  ? 'media_disabled'
                  : unsupported || audio
                    ? 'unknown'
                    : 'timeout';
        this.finish(outcome);
      }
    } catch {
      this.finish('unknown');
    }
  }

  private currentSource(source: TelemetryMediaSource, element: HTMLMediaElement): boolean {
    return (
      !!source.track &&
      source.active() &&
      source.track.readyState === 'live' &&
      element.isConnected &&
      element.srcObject instanceof MediaStream &&
      element.srcObject.getTracks().includes(source.track)
    );
  }

  private expired(attempt: CallAttempt): boolean {
    return (
      performance.now() - attempt.started >= CALL_DEADLINE_MS || Date.now() >= attempt.wallDeadline
    );
  }

  private emit(event: Parameters<TelemetryHandler>[0]): void {
    try {
      this.record(event);
    } catch {
      /* Reporting cannot affect calls. */
    }
  }

  private finish(outcome: TelemetryOutcome): void {
    const attempt = this.attempt;
    if (!attempt) return;
    this.attempt = null;
    for (const [video, frame] of attempt.frames) {
      try {
        video.cancelVideoFrameCallback(frame.handle);
      } catch {
        /* Detached native element. */
      }
    }
    attempt.frames.clear();
    this.emit({
      name: attempt.name,
      outcome,
      durationMs: performance.now() - attempt.started,
      attempt: attempt.id,
    });
  }

  dispose(): void {
    this.disposed = true;
    this.finish('superseded');
    clearInterval(this.timer);
    document.removeEventListener('visibilitychange', this.visibilityChanged);
  }
}

const frameObservers = new WeakMap<HTMLVideoElement, () => void>();

/** Time from attaching a remote track to its first presented video frame.
 * Browsers without frame callbacks and hidden/retired elements are unknown.
 */
export function observeFirstVideoFrame(video: HTMLVideoElement, record: TelemetryHandler): void {
  frameObservers.get(video)?.();
  if (
    document.visibilityState !== 'visible' ||
    typeof video.requestVideoFrameCallback !== 'function'
  ) {
    record({ name: 'media_first_video_frame', outcome: 'unknown' });
    return;
  }
  const start = performance.now();
  let finished = false;
  let frame = 0;
  const finish = (outcome: 'ok' | 'timeout' | 'unknown'): void => {
    if (finished) return;
    finished = true;
    frameObservers.delete(video);
    video.cancelVideoFrameCallback(frame);
    clearTimeout(timer);
    document.removeEventListener('visibilitychange', hidden);
    video.removeEventListener('emptied', retired);
    record({
      name: 'media_first_video_frame',
      outcome,
      ...(outcome === 'unknown' ? {} : { durationMs: performance.now() - start }),
    });
  };
  frameObservers.set(video, () => finish('unknown'));
  const hidden = (): void => {
    if (document.visibilityState !== 'visible') finish('unknown');
  };
  const retired = (): void => {
    if (!video.srcObject || !video.isConnected) finish('unknown');
  };
  const timer = setTimeout(
    () => finish(video.isConnected && video.srcObject ? 'timeout' : 'unknown'),
    10_000,
  );
  document.addEventListener('visibilitychange', hidden);
  video.addEventListener('emptied', retired);
  frame = video.requestVideoFrameCallback(() =>
    finish(video.isConnected && document.visibilityState === 'visible' ? 'ok' : 'unknown'),
  );
}
