import { readMediaSample, sampleChanges, type Sample } from './media-telemetry';

/** Local evidence only: native objects and reports never enter a diagnostic export. */
export interface CallHealthSource {
  key: object;
  direction: 'sending' | 'receiving';
  kind: 'audio' | 'video';
  track?: MediaStreamTrack;
  active: () => boolean;
  getStats: () => Promise<RTCStatsReport>;
}

export interface CallHealthState {
  connected: boolean;
  receiveRecovering: boolean;
  receiveFailed: boolean;
  microphone: 'off' | 'live' | 'interrupted';
  camera: 'off' | 'live' | 'interrupted';
  sharing: boolean;
  sendState: RTCPeerConnectionState | 'unknown';
  receiveState: RTCPeerConnectionState | 'unknown';
  expected: number;
  selected: number;
  unavailable: boolean;
}

export type CallHealthLevel = 'good' | 'warning' | 'problem' | 'unknown' | 'idle';
export interface CallHealthPart {
  level: CallHealthLevel;
  label: string;
  detail: string;
}
export interface CallHealthSnapshot {
  sending: CallHealthPart;
  receiving: CallHealthPart;
  playback: CallHealthPart;
  level: CallHealthLevel;
}
export interface CallHealthContext {
  scope: object;
  membership: number;
  state: CallHealthState;
}
interface Options {
  context: () => CallHealthContext | null;
  sources: () => CallHealthSource[];
  playback: () => HTMLMediaElement[];
  changed: (snapshot: CallHealthSnapshot) => void;
}
interface Evidence {
  previous: CallHealthSample | undefined;
  at: number;
  startedAt: number;
  part: CallHealthPart;
}
const FRESH_MS = 15_000;
const TIMEOUT_MS = 2_000;
const MAX_PENDING = 4;
const MAX_EVIDENCE = 64;
const part = (level: CallHealthLevel, label: string, detail: string): CallHealthPart => ({
  level,
  label,
  detail,
});
const unknown = (): CallHealthPart =>
  part('unknown', 'Checking', 'Waiting for current local media evidence.');

function count(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined;
}

interface OutboundSample {
  identity: string;
  timestamp: number | undefined;
  received: number | undefined;
  frames: number | undefined;
}
interface CallHealthSample extends Sample {
  /** Bounded native continuity metadata, retained locally for reset detection only. */
  outbound?: OutboundSample[];
}

function total(rows: OutboundSample[], key: 'received' | 'frames'): number | undefined {
  let result = 0;
  for (const row of rows) {
    const value = row[key];
    if (value === undefined || value > Number.MAX_SAFE_INTEGER - result) return undefined;
    result += value;
  }
  return result;
}

/** Fixed counters only. Candidate addresses, SDP and native reports are never retained. */
export function readCallHealthSample(
  report: RTCStatsReport,
  source: CallHealthSource,
): CallHealthSample {
  if (source.direction === 'receiving') {
    const sample = readMediaSample(report, source.kind);
    // Audio progress can use received packets even when totalSamplesReceived is unsupported.
    return sample;
  }
  const rows: OutboundSample[] = [];
  let matches = 0;
  let missingIdentity = false;
  report.forEach((raw: unknown) => {
    if (typeof raw !== 'object' || raw === null) return;
    const row = raw as Record<string, unknown>;
    if (row['type'] !== 'outbound-rtp' || (row['kind'] ?? row['mediaType']) !== source.kind) return;
    matches++;
    if (matches > 8) return;
    const identity = row['id'];
    if (typeof identity !== 'string' || identity.length === 0 || identity.length > 256)
      missingIdentity = true;
    rows.push({
      identity: typeof identity === 'string' && identity.length <= 256 ? identity : '',
      timestamp: count(row['timestamp']),
      received: count(row['packetsSent']),
      frames: count(row['framesEncoded']),
    });
  });
  if (!rows.length || matches > 8) return {};
  // A producer can expose several simulcast encodings. Keep their bounded identities
  // so reordering is harmless, but layer replacement or an individual reset is not progress.
  if (missingIdentity) {
    if (matches !== 1) return {};
    const row = rows[0]!;
    return { timestamp: row.timestamp, received: row.received, frames: row.frames };
  }
  rows.sort((left, right) => left.identity.localeCompare(right.identity));
  if (new Set(rows.map((row) => row.identity)).size !== rows.length) return {};
  const timestamps = rows.map((row) => row.timestamp);
  return {
    identity: JSON.stringify(rows.map((row) => row.identity)),
    timestamp: timestamps.every((value) => value !== undefined)
      ? Math.min(...timestamps)
      : undefined,
    received: total(rows, 'received'),
    frames: total(rows, 'frames'),
    outbound: rows,
  };
}

/** Low absolute bitrate is never evidence of a problem, especially with audio silence/DTX. */
export function callHealthChanges(
  current: CallHealthSample,
  previous: CallHealthSample | undefined,
  kind: 'audio' | 'video',
): CallHealthPart {
  if (current.outbound && previous?.outbound) {
    if (
      current.outbound.length !== previous.outbound.length ||
      current.outbound.some((row, index) => {
        const old = previous.outbound![index]!;
        return (
          row.identity !== old.identity ||
          (row.timestamp !== undefined &&
            old.timestamp !== undefined &&
            (row.timestamp <= old.timestamp || row.timestamp - old.timestamp > FRESH_MS)) ||
          (row.received !== undefined &&
            old.received !== undefined &&
            row.received < old.received) ||
          (row.frames !== undefined && old.frames !== undefined && row.frames < old.frames)
        );
      })
    )
      return unknown();
  }
  const change = sampleChanges(current, previous);
  if (change.windowMs === undefined || change.windowMs < 500 || change.windowMs > FRESH_MS)
    return unknown();
  if (change.packetLoss !== undefined && change.packetLoss >= 500)
    return part(
      'warning',
      'Packet loss observed',
      'Some media packets were lost in the latest sample.',
    );
  if (change.concealment !== undefined && change.concealment >= 1000)
    return part('warning', 'Audio gaps observed', 'The browser repaired gaps in received audio.');
  if (change.freezeMs !== undefined && change.freezeMs >= 500)
    return part(
      'warning',
      'Video freezes observed',
      'Received video paused during the latest sample.',
    );
  const packets =
    current.received !== undefined &&
    previous?.received !== undefined &&
    current.received >= previous.received
      ? current.received - previous.received
      : undefined;
  if (
    (kind === 'video' && change.frames !== undefined && change.frames > 0) ||
    (kind === 'audio' && packets !== undefined && packets > 0)
  )
    return part('good', 'Media progressing', 'Current local counters show media moving.');
  return part(
    'unknown',
    kind === 'audio' ? 'No recent audio packets' : 'No recent video frames',
    kind === 'audio'
      ? 'Silence, paused content or unavailable counters can produce this result.'
      : 'A still picture, paused content or unavailable counters can produce this result.',
  );
}

function summarize(
  sources: CallHealthSource[],
  evidence: Map<object, Evidence>,
  now: number,
): CallHealthPart {
  const current = sources.map((source) => {
    const value = evidence.get(source.key);
    return value && now - value.at <= FRESH_MS ? value.part : unknown();
  });
  const problem = current.find((value) => value.level === 'problem' || value.level === 'warning');
  if (problem) return problem;
  if (current.length && current.every((value) => value.level === 'good'))
    return part(
      'good',
      'Media progressing',
      'All active streams have recent local progress evidence.',
    );
  return part(
    'unknown',
    'Current evidence incomplete',
    'Some active streams have stale or unavailable statistics.',
  );
}

/** Bounded, consent-independent local observer. It never records or uploads observations. */
export class CallHealth {
  private readonly evidence = new Map<object, Evidence>();
  private readonly inFlight = new WeakSet<object>();
  private readonly blocked = new WeakSet<HTMLMediaElement>();
  private pending = 0;
  private offset = 0;
  private scope: object | null = null;
  private membership = -1;
  private disposed = false;
  private readonly timers = new Set<ReturnType<typeof setTimeout>>();
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly visibility = (): void => {
    this.evidence.clear();
    this.sample();
  };
  private last = '';

  constructor(private readonly options: Options) {
    document.addEventListener('visibilitychange', this.visibility);
    this.timer = setInterval(() => this.sample(), 2_000);
  }

  playbackResult(element: HTMLMediaElement, blocked: boolean): void {
    if (blocked) this.blocked.add(element);
    else this.blocked.delete(element);
    this.publish();
  }

  sample(): void {
    if (this.disposed) return;
    const context = this.options.context();
    if (context?.scope !== this.scope || context?.membership !== this.membership) {
      this.evidence.clear();
      this.scope = context?.scope ?? null;
      this.membership = context?.membership ?? -1;
    }
    if (!context || document.visibilityState !== 'visible') {
      this.evidence.clear();
      this.publish();
      return;
    }
    const sources = this.options.sources().filter((source) => source.active());
    const active = new Set(sources.map((source) => source.key));
    for (const key of this.evidence.keys()) if (!active.has(key)) this.evidence.delete(key);
    for (let index = 0; index < Math.min(MAX_PENDING, sources.length); index++) {
      const source = sources[(this.offset + index) % sources.length]!;
      if (this.pending >= MAX_PENDING || this.inFlight.has(source.key)) continue;
      let value = this.evidence.get(source.key);
      if (!value) {
        if (this.evidence.size >= MAX_EVIDENCE)
          this.evidence.delete(this.evidence.keys().next().value!);
        value = { previous: undefined, at: 0, startedAt: -Infinity, part: unknown() };
        this.evidence.set(source.key, value);
      }
      if (performance.now() - value.startedAt < 2_000) continue;
      value.startedAt = performance.now();
      this.collect(source, value, context);
    }
    if (sources.length) this.offset = (this.offset + MAX_PENDING) % sources.length;
    this.publish();
  }

  private collect(source: CallHealthSource, value: Evidence, context: CallHealthContext): void {
    this.inFlight.add(source.key);
    this.pending++;
    let expired = false;
    const current = (): boolean => {
      const next = this.options.context();
      return (
        !this.disposed &&
        !expired &&
        document.visibilityState === 'visible' &&
        next?.scope === context.scope &&
        next.membership === context.membership &&
        this.evidence.get(source.key) === value &&
        source.active()
      );
    };
    const timeout = setTimeout(() => {
      this.timers.delete(timeout);
      if (current()) {
        value.previous = undefined;
        value.at = 0;
        value.part = unknown();
      }
      expired = true;
      this.publish();
    }, TIMEOUT_MS);
    this.timers.add(timeout);
    // Timeout retires evidence, never the native operation's busy slot.
    Promise.resolve()
      .then(() => source.getStats())
      .then((report) => {
        if (!current()) return;
        const sample = readCallHealthSample(report, source);
        value.part = callHealthChanges(sample, value.previous, source.kind);
        value.previous = sample;
        value.at = performance.now();
      })
      .catch(() => {
        if (current()) {
          value.previous = undefined;
          value.at = 0;
          value.part = unknown();
        }
      })
      .finally(() => {
        clearTimeout(timeout);
        this.timers.delete(timeout);
        this.inFlight.delete(source.key);
        this.pending--;
        this.publish();
      });
  }

  snapshot(): CallHealthSnapshot {
    const context = this.options.context();
    const state = context?.state;
    let sending = unknown();
    let receiving = unknown();
    let playback = unknown();
    if (context && state && document.visibilityState === 'visible') {
      const sources = this.options.sources().filter((source) => source.active());
      const outgoing = sources.filter((source) => source.direction === 'sending');
      const incoming = sources.filter((source) => source.direction === 'receiving');
      if (!state.connected) {
        sending = part(
          'unknown',
          'Call reconnecting',
          'Sending evidence is unavailable while the call reconnects.',
        );
        receiving = part(
          'unknown',
          'Call reconnecting',
          'Receiving evidence is unavailable while the call reconnects.',
        );
      } else {
        const evidence =
          context.scope === this.scope && context.membership === this.membership
            ? this.evidence
            : new Map<object, Evidence>();
        sending =
          !outgoing.length && state.microphone === 'off' && state.camera === 'off' && !state.sharing
            ? part('idle', 'Microphone and camera off', 'Nothing is being sent from your devices.')
            : summarize(outgoing, evidence, performance.now());
        if (state.microphone === 'interrupted' || state.camera === 'interrupted')
          sending = part(
            'problem',
            state.microphone === 'interrupted' ? 'Microphone interrupted' : 'Camera interrupted',
            'A device or browser muted or stopped capture. Check Camera & mic before explicitly restarting it.',
          );
        else if (outgoing.length && (state.sendState === 'failed' || state.sendState === 'closed'))
          sending = part(
            'problem',
            'Sending connection interrupted',
            'Your outgoing media transport is unavailable.',
          );
        else if (
          outgoing.length &&
          (state.sendState === 'disconnected' || state.sendState === 'connecting')
        )
          sending = part(
            'warning',
            'Sending reconnecting',
            'Waiting for the outgoing media connection.',
          );
        receiving =
          state.selected === 0
            ? part(
                'idle',
                state.expected ? 'Incoming media paused by your choices' : 'No incoming media',
                'No active incoming stream is selected.',
              )
            : incoming.length < state.selected
              ? part(
                  'unknown',
                  'Waiting for selected media',
                  'Some selected streams do not yet have an active local receiver.',
                )
              : summarize(incoming, evidence, performance.now());
        if (state.receiveRecovering)
          receiving = part(
            'unknown',
            'Refreshing incoming media',
            'Your outgoing microphone, camera and screen share keep their current settings.',
          );
        else if (
          state.receiveFailed ||
          state.unavailable ||
          (state.selected > 0 && ['failed', 'closed'].includes(state.receiveState))
        )
          receiving = part(
            'problem',
            'Incoming media unavailable',
            'Refresh incoming media to request fresh subscriptions.',
          );
        else if (state.selected > 0 && ['connecting', 'disconnected'].includes(state.receiveState))
          receiving = part(
            'warning',
            'Receiving reconnecting',
            'Waiting for the incoming media connection.',
          );
      }
      const elements = this.options
        .playback()
        .filter((element) => element.isConnected && element.srcObject);
      const audible = elements.filter(
        (element) => element.tagName === 'AUDIO' && !element.muted && element.volume > 0,
      );
      if (
        elements.some(
          (element) => this.blocked.has(element) && (element.tagName === 'VIDEO' || !element.muted),
        )
      )
        playback = part(
          'problem',
          'Playback blocked',
          'The browser could not start playback. Retry playback with the button below.',
        );
      else if (elements.some((element) => element.error !== null))
        playback = part(
          'problem',
          'Playback interrupted',
          'A local media element reported a playback error.',
        );
      else if (!elements.length)
        playback =
          state.selected > 0
            ? part(
                'unknown',
                'Waiting for playback',
                'Selected incoming media has no active local playback element yet.',
              )
            : part('idle', 'Nothing to play', 'No remote media element is active.');
      else if (audible.some((element) => element.paused))
        playback = part(
          'warning',
          'Audio playback paused',
          'Retry playback, then check the selected speaker and system volume.',
        );
      else if (audible.some((element) => element.readyState < 2))
        playback = part(
          'unknown',
          'Audio playback waiting',
          'The browser is waiting for playable audio data.',
        );
      else if (audible.length)
        playback = part(
          'good',
          'Audio playback running',
          'The browser is playing audio. This cannot confirm your physical speaker or system volume.',
        );
      else if (elements.some((element) => element.tagName === 'VIDEO' && element.paused))
        playback = part(
          'warning',
          'Video playback paused',
          'Retry playback to resume the selected video.',
        );
      else if (elements.some((element) => element.tagName === 'VIDEO' && element.readyState >= 2))
        playback = part('good', 'Video playback running', 'The browser has playable video data.');
      else
        playback = part(
          'idle',
          'Audio muted or not present',
          'Your local playback choices do not currently include audible audio.',
        );
    }
    const values = [sending, receiving, playback];
    const level =
      (['problem', 'warning', 'unknown', 'good', 'idle'] as const).find((candidate) =>
        values.some((value) => value.level === candidate),
      ) ?? 'unknown';
    return { sending, receiving, playback, level };
  }

  private publish(): void {
    if (this.disposed) return;
    const snapshot = this.snapshot();
    const value = JSON.stringify(snapshot);
    if (value !== this.last) {
      this.last = value;
      this.options.changed(snapshot);
    }
  }

  dispose(): void {
    this.disposed = true;
    clearInterval(this.timer);
    for (const timer of this.timers) clearTimeout(timer);
    this.timers.clear();
    this.evidence.clear();
    document.removeEventListener('visibilitychange', this.visibility);
  }
}
