import type { TelemetryHandler, TelemetryMediaSource, TelemetryName } from './telemetry-types';

type NumericStats = Record<string, unknown>;
interface Sample {
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
  private readonly visibilityChanged = (): void => {
    this.states.clear();
  };
  private readonly timer: ReturnType<typeof setInterval>;

  constructor(
    private readonly sources: () => TelemetryMediaSource[],
    private readonly record: TelemetryHandler,
  ) {
    this.record = (event) => {
      try {
        record(event);
      } catch {
        /* Keep the media path independent of reporting. */
      }
    };
    document.addEventListener('visibilitychange', this.visibilityChanged);
    this.timer = setInterval(() => this.sample(), 15_000);
  }

  sample(): void {
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
      this.record({ name: 'media_sample', outcome: 'unavailable' });
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
      this.collect(source, state);
    }
    this.offset = (this.offset + 4) % active.length;
  }

  private collect(source: TelemetryMediaSource, state: SourceState): void {
    this.inFlight.add(source.key);
    this.pendingCount += 1;
    let expired = false;
    const current = (): boolean =>
      !this.disposed &&
      !expired &&
      document.visibilityState === 'visible' &&
      source.active() &&
      this.states.get(source.key) === state;
    const timeout = setTimeout(() => {
      if (current()) this.record({ name: 'media_sample', outcome: 'timeout' });
      expired = true;
      state.previous = undefined;
    }, 2000);
    // A hung native getStats keeps this source busy; do not stack abandoned calls.
    Promise.resolve()
      .then(() => source.getStats())
      .then((report) => {
        if (!current()) return;
        this.record({ name: 'media_sample', outcome: 'ok' });
        const sample = readMediaSample(report, source.kind);
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
        if (current()) this.record({ name: 'media_sample', outcome: 'error' });
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
