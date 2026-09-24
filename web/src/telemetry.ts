import type {
  TelemetryEvent,
  TelemetryHandler,
  TelemetryMediaHandler,
  TelemetryName,
  TelemetryOutcome,
} from './telemetry-types';

const MAX_PENDING = 64;
const MAX_HISTORY = 80;
const MAX_MEDIA_HISTORY = 32;
const MAX_STREAMS = 64;
const MAX_BATCH = 16;
const FLUSH_MS = 10_000;
const FETCH_TIMEOUT_MS = 5_000;
const SESSION_MS = 30 * 60_000;

interface LocalMediaSample {
  atMs: number;
  stream: number | null;
  kind: 'audio' | 'video';
  windowMs: number | null;
  decodedFrames?: number | null;
  decodedFps?: number | null;
  videoFreezeMs?: number | null;
  audioConcealmentPercent?: number | null;
  packetLossPercent: number | null;
  rttMs: number | null;
}

function bounded(value: number | undefined, maximum: number): number | null {
  return value !== undefined && Number.isFinite(value) && value >= 0 && value <= maximum
    ? Math.round(value * 1000) / 1000
    : null;
}

function routineQuality(event: TelemetryEvent): boolean {
  if (event.name === 'media_sample') return event.outcome === 'ok';
  return (
    (event.outcome === 'ok' || event.outcome === 'unknown') &&
    (event.name === 'media_video_progress' ||
      event.name === 'media_video_freeze' ||
      event.name === 'media_audio_concealment' ||
      event.name === 'media_packet_loss' ||
      event.name === 'media_rtt')
  );
}

function retention(history: { atMs: number }[], capacity: number, evicted: number) {
  return {
    capacity,
    evicted,
    oldestAtMs: history[0]?.atMs ?? null,
    newestAtMs: history[history.length - 1]?.atMs ?? null,
  };
}

function createLocalReportReference(): string {
  try {
    // randomUUID is unavailable on non-secure origins. This local report ID
    // must never make optional diagnostics a prerequisite for app startup.
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
  } catch {
    return 'unavailable';
  }
}

export function browserFamily(agent: string): 'firefox' | 'chromium' | 'safari' | 'other' {
  if (/Firefox\//i.test(agent)) return 'firefox';
  if (/(Chrome|Chromium|Edg)\//i.test(agent)) return 'chromium';
  if (/Safari\//i.test(agent)) return 'safari';
  return 'other';
}

export function failureOutcome(error: unknown): TelemetryOutcome {
  // Error messages, stacks and browser event payloads can contain secrets.
  if (!(error instanceof Error)) return 'error';
  if (error.name === 'SessionOutcomeUnknownError') return 'unknown';
  if (error.name === 'NotAllowedError') return 'cancelled_or_timeout';
  if (error.name === 'AbortError') return 'superseded';
  if (error.name === 'TimeoutError' || error.name === 'SignalingRequestTimeoutError')
    return 'timeout';
  return 'error';
}

/** Best effort, same-origin, bounded reliability measurements, with no identity. */
export class ClientTelemetry {
  private pending: TelemetryEvent[] = [];
  private history: (TelemetryEvent & { atMs: number })[] = [];
  private mediaHistory: LocalMediaSample[] = [];
  private evictedEvents = 0;
  private evictedMedia = 0;
  private streams = new WeakMap<object, number>();
  private streamSequence = 0;
  private unidentifiedStreams = 0;
  private dropped = 0;
  private deliveryFailures = 0;
  private sending = false;
  private uploadController: AbortController | null = null;
  private disposed = false;
  private enabled = false;

  get sharingEnabled(): boolean {
    return this.enabled;
  }

  setSharing(enabled: boolean): void {
    this.enabled = enabled;
    if (!enabled) {
      this.pending = [];
      this.uploadController?.abort();
    }
    try {
      localStorage.setItem('reliabilityTelemetry', String(enabled));
    } catch {
      /* Private browsing can reject storage. */
    }
  }
  private epoch = performance.now();
  private reportStartedAt = new Date().toISOString();
  private localReportReference = createLocalReportReference();
  private attemptSequence = 0;
  private recentCounts = new Map<string, number>();
  private timer: ReturnType<typeof setInterval>;
  readonly browser = browserFamily(navigator.userAgent);

  constructor(private readonly release: string) {
    try {
      this.enabled = localStorage.getItem('reliabilityTelemetry') === 'true';
    } catch {
      /* Keep uploads disabled. */
    }
    this.timer = setInterval(() => {
      this.recentCounts.clear();
      this.flush().catch(() => {
        /* Telemetry must never create an unhandled rejection. */
      });
    }, FLUSH_MS);
  }

  readonly record: TelemetryHandler = (event) => {
    try {
      this.recordSafe(event);
    } catch {
      this.dropped += 1;
    }
  };

  private reportTime(): number {
    const now = performance.now();
    if (now - this.epoch >= SESSION_MS) {
      this.epoch = now;
      this.reportStartedAt = new Date().toISOString();
      this.localReportReference = createLocalReportReference();
      this.history = [];
      this.mediaHistory = [];
      this.evictedEvents = this.evictedMedia = this.unidentifiedStreams = this.streamSequence = 0;
      this.streams = new WeakMap();
    }
    return Math.round(Math.max(0, now - this.epoch));
  }

  readonly recordMediaSample: TelemetryMediaHandler = (sample) => {
    if (this.disposed) return;
    try {
      const atMs = this.reportTime();
      let stream = this.streams.get(sample.key) ?? null;
      if (stream === null && this.streamSequence < MAX_STREAMS) {
        stream = ++this.streamSequence;
        this.streams.set(sample.key, stream);
      }
      if (stream === null) this.unidentifiedStreams += 1;
      const windowMs = bounded(sample.windowMs, SESSION_MS);
      // A new report must not claim an interval from the preceding report.
      const interval = windowMs !== null && windowMs > 0 && windowMs <= atMs;
      const frames = interval ? bounded(sample.frames, 1_000_000) : null;
      // Project only named numbers. Neither the key nor arbitrary stats are exported.
      this.mediaHistory.push({
        atMs,
        stream,
        kind: sample.kind,
        windowMs: interval ? windowMs : null,
        ...(sample.kind === 'video'
          ? {
              decodedFrames: frames,
              decodedFps:
                frames === null || windowMs === null
                  ? null
                  : bounded((frames * 1000) / windowMs, 1_000_000),
              videoFreezeMs: interval ? bounded(sample.freezeMs, windowMs) : null,
            }
          : {
              audioConcealmentPercent: interval
                ? bounded(
                    sample.concealment === undefined ? undefined : sample.concealment / 100,
                    100,
                  )
                : null,
            }),
        packetLossPercent: interval
          ? bounded(sample.packetLoss === undefined ? undefined : sample.packetLoss / 100, 100)
          : null,
        rttMs: bounded(sample.rttMs, 120_000),
      });
      if (this.mediaHistory.length > MAX_MEDIA_HISTORY) {
        this.mediaHistory.shift();
        this.evictedMedia += 1;
      }
    } catch {
      this.dropped += 1;
    }
  };

  private recordSafe(event: TelemetryEvent): void {
    if (this.disposed) return;
    const atMs = this.reportTime();
    const key = `${event.name}:${event.outcome}`;
    const count = this.recentCounts.get(key) ?? 0;
    if (count >= 8) {
      this.dropped += 1;
      return;
    }
    this.recentCounts.set(key, count + 1);
    // Projection prevents accidental future properties leaking through spread.
    const safe: TelemetryEvent = { name: event.name, outcome: event.outcome };
    if (event.durationMs !== undefined && Number.isFinite(event.durationMs))
      safe.durationMs = Math.round(Math.max(0, Math.min(120_000, event.durationMs)));
    if (event.value !== undefined && Number.isFinite(event.value))
      safe.value = Math.round(Math.max(0, Math.min(1_000_000, event.value)));
    if (!routineQuality(safe)) {
      this.history.push({
        ...safe,
        ...(event.attempt !== undefined && Number.isSafeInteger(event.attempt) && event.attempt > 0
          ? { attempt: event.attempt }
          : {}),
        atMs,
      });
      if (this.history.length > MAX_HISTORY) {
        this.history.shift();
        this.evictedEvents += 1;
      }
    }
    if (!this.enabled) return;
    if (this.pending.length === MAX_PENDING) {
      this.dropped += 1;
      return;
    }
    this.pending.push(safe);
  }

  nextAttemptId(): number {
    return ++this.attemptSequence;
  }

  async measure<T>(name: TelemetryName, work: () => Promise<T>): Promise<T> {
    const start = performance.now();
    const attempt = this.nextAttemptId();
    this.record({ name, outcome: 'started', attempt });
    try {
      const result = await work();
      this.record({ name, outcome: 'ok', durationMs: performance.now() - start, attempt });
      return result;
    } catch (error) {
      this.record({
        name,
        outcome: failureOutcome(error),
        durationMs: performance.now() - start,
        attempt,
      });
      throw error;
    }
  }

  async flush(): Promise<void> {
    if (this.disposed || !this.enabled || this.sending || this.pending.length === 0) return;
    this.sending = true;
    const events = this.pending.splice(0, MAX_BATCH);
    const controller = new AbortController();
    this.uploadController = controller;
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    try {
      const response = await fetch('/api/telemetry', {
        method: 'POST',
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ version: 1, browser: this.browser, events }),
        signal: controller.signal,
      });
      if (!response.ok) this.deliveryFailures += events.length;
    } catch {
      // No retries or recursive reporting during outages; never block a user action.
      this.deliveryFailures += events.length;
    } finally {
      clearTimeout(timer);
      this.uploadController = null;
      this.sending = false;
    }
  }

  summary(): string {
    const snapshotAtMs = this.reportTime();
    return JSON.stringify(
      {
        version: 2,
        localReportReference: this.localReportReference,
        reportStartedAt: this.reportStartedAt,
        generatedAt: new Date().toISOString(),
        snapshotAtMs,
        release: this.release,
        browser: this.browser,
        droppedEvents: this.dropped,
        undeliveredEvents: this.deliveryFailures,
        pendingEvents: this.pending.length,
        counterScope: 'page_lifetime',
        retention: {
          reportWindowMs: SESSION_MS,
          events: retention(this.history, MAX_HISTORY, this.evictedEvents),
          mediaSamples: retention(this.mediaHistory, MAX_MEDIA_HISTORY, this.evictedMedia),
          streamLimit: MAX_STREAMS,
          unidentifiedStreamSamples: this.unidentifiedStreams,
        },
        events: this.history,
        mediaSamples: this.mediaHistory,
      },
      null,
      2,
    );
  }

  dispose(): void {
    this.disposed = true;
    this.uploadController?.abort();
    clearInterval(this.timer);
    this.pending = [];
    this.history = [];
    this.mediaHistory = [];
    this.streams = new WeakMap();
  }
}
