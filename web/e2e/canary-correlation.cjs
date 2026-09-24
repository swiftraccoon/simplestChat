/** Private, opt-in evidence for the two owned canary clients. Never upload this file. */
const fs = require('node:fs');
const path = require('node:path');

const MAX_SAMPLES = 40;
const MAX_STREAMS = 16;
const MAX_BYTES = 512 * 1024;
const uint = (value) => Number.isSafeInteger(value) && value >= 0;
const finite = (value) => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const numberOrNull = (value) => (finite(value) ? value : null);
const integerOrNull = (value) => (uint(value) ? value : null);
const ssrcOrNull = (value) => (uint(value) && value <= 0xffffffff ? value : null);

/** Self-contained for page.evaluate(). Only named numeric counters leave the page. */
async function collectCanaryMediaSample() {
  const uint = (value) => Number.isSafeInteger(value) && value >= 0;
  const numeric = (value) =>
    typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
  const integer = (value) => (uint(value) ? value : null);
  const ssrc = (value) => (uint(value) && value <= 0xffffffff ? value : null);
  const state = (window.__canaryMediaCorrelation ??= {
    peers: new WeakMap(),
    peerCount: 0,
    streamCount: 0,
  });
  const result = {
    startedAtMs: performance.now(),
    epochAtStartMs: Date.now(),
    finishedAtMs: null,
    epochAtFinishMs: null,
    status: 'complete',
    issues: [],
    streams: [],
  };
  const issue = (name) => {
    result.status = 'incomplete';
    if (!result.issues.includes(name)) result.issues.push(name);
  };
  const peers = window.__communityPeers || [];
  if (peers.length > 4) issue('peer_limit');
  const requests = [];
  for (const peer of peers.slice(0, 4)) {
    if (peer.connectionState === 'closed') continue;
    let identity = state.peers.get(peer);
    if (!identity) {
      if (state.peerCount === 4) {
        issue('peer_limit');
        continue;
      }
      identity = { ordinal: ++state.peerCount, streams: new Map(), pending: false };
      state.peers.set(peer, identity);
    }
    if (identity.pending) {
      issue('stats_pending');
      continue;
    }
    identity.pending = true;
    requests.push(
      Promise.resolve()
        .then(() => peer.getStats())
        .then((stats) => {
          if (result.finishedAtMs !== null) return;
          let visited = 0;
          for (const stat of stats.values()) {
            if (++visited > 256) {
              issue('stats_limit');
              break;
            }
            if (
              !['inbound-rtp', 'outbound-rtp'].includes(stat.type) ||
              !['audio', 'video'].includes(stat.kind)
            )
              continue;
            if (result.streams.length === 16) {
              issue('stream_limit');
              break;
            }
            if (ssrc(stat.ssrc) === null || typeof stat.id !== 'string') {
              issue('invalid_stream');
              continue;
            }
            let ordinal = identity.streams.get(stat.id);
            if (!ordinal) {
              if (state.streamCount === 64) {
                issue('stream_limit');
                break;
              }
              ordinal = ++state.streamCount;
              identity.streams.set(stat.id, ordinal);
            }
            const outbound = stat.type === 'outbound-rtp';
            const codec = stats.get(stat.codecId)?.mimeType;
            result.streams.push({
              peerOrdinal: identity.ordinal,
              streamOrdinal: ordinal,
              direction: outbound ? 'send' : 'receive',
              kind: stat.kind,
              ssrc: stat.ssrc,
              // Some browsers omit RTX identity. Absence is unknown, never SSRC zero.
              rtxSsrc: ssrc(stat.rtxSsrc),
              codec: [
                'audio/opus',
                'video/VP8',
                'video/VP9',
                'video/H264',
                'video/AV1',
                'video/rtx',
              ].includes(codec)
                ? codec
                : null,
              timestampMs: numeric(stat.timestamp),
              packets: integer(outbound ? stat.packetsSent : stat.packetsReceived),
              bytes: integer(outbound ? stat.bytesSent : stat.bytesReceived),
              frames: integer(outbound ? stat.framesEncoded : stat.framesDecoded),
              retransmittedPackets: integer(stat.retransmittedPacketsSent),
              frameWidth: integer(stat.frameWidth),
              frameHeight: integer(stat.frameHeight),
              active: typeof stat.active === 'boolean' ? stat.active : null,
            });
          }
        })
        .catch(() => issue('stats_failed'))
        .finally(() => {
          identity.pending = false;
        }),
    );
  }
  let timer;
  try {
    await Promise.race([
      Promise.all(requests),
      new Promise((resolve) => {
        timer = setTimeout(() => {
          issue('stats_timeout');
          resolve();
        }, 1000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
    result.finishedAtMs = performance.now();
    result.epochAtFinishMs = Date.now();
  }
  if (!result.streams.length) issue('no_streams');
  return result;
}

const ISSUES = [
  'peer_limit',
  'stats_pending',
  'stats_limit',
  'stream_limit',
  'invalid_stream',
  'stats_failed',
  'stats_timeout',
  'no_streams',
  'collection_failed',
  'invalid_sample',
  'collection_timeout',
  'collection_pending',
];

function projectSample(source) {
  if (!source || !Array.isArray(source.streams) || !Array.isArray(source.issues))
    return { status: 'incomplete', issues: ['invalid_sample'], streams: [] };
  const streams = [];
  const issues = source.issues.slice(0, ISSUES.length).filter((issue) => ISSUES.includes(issue));
  if (
    source.issues.length > ISSUES.length ||
    source.issues.slice(0, ISSUES.length).some((issue) => !ISSUES.includes(issue)) ||
    !['complete', 'incomplete'].includes(source.status)
  )
    issues.push('invalid_sample');
  if (source.streams.length > MAX_STREAMS) issues.push('stream_limit');
  const identities = new Set();
  for (const stream of source.streams.slice(0, MAX_STREAMS)) {
    if (
      !stream ||
      !uint(stream.peerOrdinal) ||
      stream.peerOrdinal < 1 ||
      stream.peerOrdinal > 4 ||
      !uint(stream.streamOrdinal) ||
      stream.streamOrdinal < 1 ||
      stream.streamOrdinal > 64 ||
      !['send', 'receive'].includes(stream.direction) ||
      !['audio', 'video'].includes(stream.kind) ||
      ssrcOrNull(stream.ssrc) === null ||
      identities.has(stream.streamOrdinal)
    ) {
      issues.push('invalid_stream');
      continue;
    }
    identities.add(stream.streamOrdinal);
    streams.push({
      peerOrdinal: stream.peerOrdinal,
      streamOrdinal: stream.streamOrdinal,
      direction: stream.direction,
      kind: stream.kind,
      ssrc: stream.ssrc,
      rtxSsrc: ssrcOrNull(stream.rtxSsrc),
      codec: [
        'audio/opus',
        'video/VP8',
        'video/VP9',
        'video/H264',
        'video/AV1',
        'video/rtx',
      ].includes(stream.codec)
        ? stream.codec
        : null,
      timestampMs: numberOrNull(stream.timestampMs),
      ...Object.fromEntries(
        ['packets', 'bytes', 'frames', 'retransmittedPackets', 'frameWidth', 'frameHeight'].map(
          (name) => [name, integerOrNull(stream[name])],
        ),
      ),
      active: typeof stream.active === 'boolean' ? stream.active : null,
    });
  }
  const clocks = Object.fromEntries(
    ['startedAtMs', 'finishedAtMs', 'epochAtStartMs', 'epochAtFinishMs'].map((name) => [
      name,
      numberOrNull(source[name]),
    ]),
  );
  if (
    Object.values(clocks).some((value) => value === null) ||
    clocks.finishedAtMs < clocks.startedAtMs ||
    clocks.epochAtFinishMs < clocks.epochAtStartMs
  )
    issues.push('invalid_sample');
  if (!streams.length) issues.push('no_streams');
  return {
    ...clocks,
    status: source.status === 'complete' && !issues.length ? 'complete' : 'incomplete',
    issues: [...new Set(issues)],
    streams,
  };
}

function movement(previous, stream) {
  if (!previous) return 'first_observation';
  if (
    previous.ssrc !== stream.ssrc ||
    previous.direction !== stream.direction ||
    previous.kind !== stream.kind ||
    ['timestampMs', 'packets', 'bytes', 'frames'].some(
      (key) => previous[key] !== null && stream[key] !== null && stream[key] < previous[key],
    )
  )
    return 'reset_or_replaced';
  if (previous.packets === null || stream.packets === null) return 'unknown';
  return stream.packets > previous.packets ? 'increased' : 'flat';
}

function openCanaryCorrelation({
  file,
  artifacts,
  enabled,
  profiles,
  impairmentEnabled,
  githubActions = false,
}) {
  if (!file) return null;
  if (githubActions)
    throw new Error('Private canary correlation is unavailable in public GitHub Actions workflows');
  if (
    !enabled ||
    impairmentEnabled ||
    profiles.length !== 1 ||
    profiles[0] !== 'baseline' ||
    !path.isAbsolute(file)
  )
    throw new Error(
      'Private canary correlation requires CANARY_MODE=1, baseline only, IMPAIR_SCRIPT=none and an absolute file path',
    );
  const parent = fs.realpathSync(path.dirname(file));
  const publicRoot = fs.realpathSync(artifacts);
  const destination = path.join(parent, path.basename(file));
  const relative = path.relative(publicRoot, destination);
  if (
    !relative ||
    (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative))
  )
    throw new Error('Private canary correlation must be outside E2E_ARTIFACTS');
  const descriptor = fs.openSync(
    destination,
    fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW,
    0o600,
  );
  const report = {
    schemaVersion: 1,
    coverage: 'incomplete',
    issues: [],
    droppedSamples: 0,
    samples: [],
    limitations: [
      'Owned clients only. SSRCs may be rewritten by the SFU; publisher and receiver identities are not interchangeable.',
      'Browser and server clocks differ. Use observation intervals; no cross-clock packet arithmetic.',
      'Missing counters and RTX identities are unknown. Sparse samples cannot exclude interruptions between observations.',
    ],
  };
  const previous = new Map();
  let closed = false;
  let persistenceFailed = false;
  const mark = (issue) => {
    if (!report.issues.includes(issue)) report.issues.push(issue);
  };
  function persist() {
    if (closed || persistenceFailed) return;
    try {
      const body = `${JSON.stringify(report, null, 2)}\n`;
      if (Buffer.byteLength(body) > MAX_BYTES) {
        mark('byte_limit');
        persistenceFailed = true;
        return;
      }
      const bytes = Buffer.from(body);
      let written = 0;
      while (written < bytes.length) {
        const count = fs.writeSync(descriptor, bytes, written, bytes.length - written, written);
        if (!count) throw new Error('Private evidence write made no progress');
        written += count;
      }
      fs.ftruncateSync(descriptor, bytes.length);
    } catch {
      mark('write_failed');
      persistenceFailed = true;
    }
  }
  persist();
  return {
    add(role, source, clock) {
      if (closed) return;
      if (
        !['publisher', 'viewer'].includes(role) ||
        !clock ||
        !['startedAtMs', 'finishedAtMs', 'epochAtStartMs', 'epochAtFinishMs'].every((key) =>
          finite(clock[key]),
        ) ||
        clock.finishedAtMs < clock.startedAtMs ||
        clock.epochAtFinishMs < clock.epochAtStartMs
      ) {
        mark('invalid_clock');
        persist();
        return;
      }
      if (report.samples.length === MAX_SAMPLES) {
        report.droppedSamples++;
        mark('sample_limit');
        persist();
        return;
      }
      const sample = projectSample(source);
      for (const stream of sample.streams) {
        const key = `${role}/${stream.peerOrdinal}/${stream.streamOrdinal}`;
        stream.packetMovement = movement(previous.get(key), stream);
        previous.set(key, stream);
      }
      report.samples.push({
        ordinal: report.samples.length + 1,
        role,
        hostClock: Object.fromEntries(
          ['startedAtMs', 'finishedAtMs', 'epochAtStartMs', 'epochAtFinishMs'].map((name) => [
            name,
            clock[name],
          ]),
        ),
        ...sample,
      });
      if (sample.status !== 'complete') mark('collection_incomplete');
      persist();
    },
    finish() {
      if (closed) return;
      if (
        !report.samples.some((sample) => sample.role === 'publisher') ||
        !report.samples.some((sample) => sample.role === 'viewer')
      )
        mark('missing_client');
      report.coverage = report.issues.length ? 'incomplete' : 'complete';
      persist();
      closed = true;
      try {
        fs.closeSync(descriptor);
      } catch {
        mark('close_failed');
      }
    },
    summary() {
      return {
        enabled: true,
        coverage: report.issues.length ? 'incomplete' : report.coverage,
        sampleCount: report.samples.length,
        droppedSamples: report.droppedSamples,
        issues: [...report.issues],
      };
    },
  };
}

module.exports = {
  collectCanaryMediaSample,
  openCanaryCorrelation,
  projectSample,
  movement,
  MAX_SAMPLES,
  MAX_BYTES,
};
