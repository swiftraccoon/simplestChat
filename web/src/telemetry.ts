import type {
  TelemetryEvent,
  TelemetryHandler,
  TelemetryName,
  TelemetryOutcome,
} from './telemetry-types';

const MAX_PENDING = 64;
const MAX_HISTORY = 80;
const MAX_BATCH = 16;
const FLUSH_MS = 10_000;
const FETCH_TIMEOUT_MS = 5_000;
const SESSION_MS = 30 * 60_000;

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

  private recordSafe(event: TelemetryEvent): void {
    if (this.disposed) return;
    const now = performance.now();
    if (now - this.epoch >= SESSION_MS) {
      this.epoch = now;
      this.localReportReference = createLocalReportReference();
      this.history = [];
    }
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
    this.history.push({
      ...safe,
      ...(event.attempt !== undefined && Number.isSafeInteger(event.attempt) && event.attempt > 0
        ? { attempt: event.attempt }
        : {}),
      atMs: Math.round((now - this.epoch) / 100) * 100,
    });
    if (this.history.length > MAX_HISTORY) this.history.shift();
    if (!this.enabled) return;
    if (this.pending.length === MAX_PENDING) {
      this.dropped += 1;
      return;
    }
    this.pending.push(safe);
  }

  async measure<T>(name: TelemetryName, work: () => Promise<T>): Promise<T> {
    const start = performance.now();
    const attempt = ++this.attemptSequence;
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
    return JSON.stringify(
      {
        version: 1,
        localReportReference: this.localReportReference,
        generatedAt: new Date().toISOString(),
        release: this.release,
        browser: this.browser,
        droppedEvents: this.dropped,
        undeliveredEvents: this.deliveryFailures,
        pendingEvents: this.pending.length,
        events: this.history,
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
  }
}
