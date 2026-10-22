/** Fixed, bounded dialogue with the Rust lease owner on anonymous stdin duplex. */
const { Socket } = require('node:net');

const LIMIT = 1024;
const BUDGET = 600000;
const STEPS = [
  ['baseline', 'clear', null],
  ['lossy', 'apply', 'lossy'],
  ['constrained', 'apply', 'constrained'],
  ['severe', 'apply', 'severe'],
  ['recovery', 'clear', null],
  ['lossyJoin', 'apply', 'lossyJoin'],
  ['finish', 'clear', null],
];
const TCP_STEPS = [
  ['block', 'block-udp', null],
  ['baseline', 'clear', null],
  ['finish', 'clear', null],
  ['unblock', 'unblock-udp', null],
];
const PROFILES = {
  lossy: { IMPAIR_LOSS: '5', IMPAIR_DELAY_MS: '50', IMPAIR_JITTER_MS: '10' },
  constrained: { IMPAIR_RATE_KBIT: '400', IMPAIR_DELAY_MS: '30' },
  severe: { IMPAIR_RATE_KBIT: '150', IMPAIR_DELAY_MS: '30' },
  lossyJoin: {
    IMPAIR_LOSS: '5',
    IMPAIR_DELAY_MS: '50',
    IMPAIR_JITTER_MS: '10',
    IMPAIR_DIRECTION: 'both',
  },
};
class ControlFailure extends Error {
  constructor(status) {
    super('Owned browser impairment control failed.');
    this.status = status;
  }
}
const statusOf = (error) => (error instanceof ControlFailure ? error.status : 1);

/**
 * Own FD0 exclusively as a Unix duplex stream, never as process.stdin too.
 * The injected stream/clock/timers are inert test seams. One overall deadline
 * includes every awaited request; Rust's Node watchdog is the outer bound.
 * No pathname, endpoint, arbitrary argv or numeric profile is accepted.
 */
function createChannel({
  runId,
  mode = 'adaptive',
  stream,
  now = () => performance.now(),
  setTimer = setTimeout,
  clearTimer = clearTimeout,
} = {}) {
  if (
    !['adaptive', 'tcp'].includes(mode) ||
    typeof runId !== 'string' ||
    !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(runId)
  )
    throw new ControlFailure(1);
  const tcp = mode === 'tcp';
  const steps = tcp ? TCP_STEPS : STEPS;
  stream ??= new Socket({ fd: 0, readable: true, writable: true });
  const deadline = now() + BUDGET;
  let buffer = Buffer.alloc(0);
  let pending;
  let sequence = 0;
  let finished = false;
  let finalizing = false;
  let ordered = true;
  let closing = false;
  let closed = false;
  let terminal;
  let retirement;
  let closeResolve;
  const closeWait = new Promise((resolve) => (closeResolve = resolve));
  const fail = (status = 1) => {
    terminal ??= new ControlFailure(status);
    if (pending) {
      const request = pending;
      pending = undefined;
      clearTimer(request.timer);
      request.reject(terminal);
    }
    if (!closed) stream.destroy();
    return terminal;
  };
  const data = (chunk) => {
    if (now() >= deadline) {
      fail(124);
      return;
    }
    if (!Buffer.isBuffer(chunk)) chunk = Buffer.from(chunk);
    if (!pending || closing || buffer.length + chunk.length > LIMIT) {
      fail();
      return;
    }
    buffer = Buffer.concat([buffer, chunk]);
    const newline = buffer.indexOf(10);
    if (newline === -1) {
      if (buffer.length >= LIMIT) fail();
      return;
    }
    if (newline !== buffer.length - 1) {
      fail();
      return;
    }
    let reply;
    try {
      const text = new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, -1));
      reply = JSON.parse(text);
      if (
        JSON.stringify(reply) !== text ||
        Object.keys(reply).join(',') !== 'schemaVersion,runId,sequence,passed,status' ||
        reply.schemaVersion !== 1 ||
        reply.runId !== runId ||
        reply.sequence !== pending.sequence ||
        !Number.isInteger(reply.status) ||
        reply.status < 0 ||
        reply.status > 255 ||
        typeof reply.passed !== 'boolean' ||
        reply.passed !== (reply.status === 0)
      )
        throw new Error('Invalid control reply');
    } catch {
      fail();
      return;
    }
    if (!reply.passed) {
      fail(reply.status);
      return;
    }
    const request = pending;
    pending = undefined;
    clearTimer(request.timer);
    sequence = request.sequence;
    ordered &&= request.ordered;
    finalizing ||= request.phase === 'finish';
    finished = request.phase === (tcp ? 'unblock' : 'finish');
    buffer = Buffer.alloc(0);
    request.resolve();
  };
  const end = () => {
    if (!closing) fail();
  };
  const error = () => fail();
  const close = () => {
    closed = true;
    if (!closing) fail();
    stream.removeListener('data', data);
    stream.removeListener('end', end);
    stream.removeListener('error', error);
    stream.removeListener('close', close);
    closeResolve();
  };
  stream.on('data', data);
  stream.on('end', end);
  stream.on('error', error);
  stream.on('close', close);
  return {
    impair(action, values = {}, phaseName) {
      if (terminal) return Promise.reject(terminal);
      const implicit =
        tcp && action === 'block-udp'
          ? 'block'
          : tcp && action === 'unblock-udp'
            ? 'unblock'
            : 'finish';
      const phase = phaseName === undefined ? implicit : phaseName;
      const expected = steps[sequence];
      const inOrder = phase === expected?.[0] && action === expected?.[1];
      const finalClear = phase === 'finish' && phaseName === undefined && action === 'clear';
      const finalUnblock =
        tcp &&
        finalizing &&
        phase === 'unblock' &&
        phaseName === undefined &&
        action === 'unblock-udp';
      const profile = tcp ? {} : (PROFILES[phase] ?? {});
      if (
        pending ||
        closing ||
        finished ||
        !expected ||
        typeof values !== 'object' ||
        values === null ||
        Array.isArray(values) ||
        (finalizing ? !finalUnblock : !finalClear && !inOrder) ||
        (phase === 'finish' && !finalClear) ||
        (tcp && phase !== 'baseline' && phaseName !== undefined) ||
        Object.keys(values).sort().join(',') !== Object.keys(profile).sort().join(',') ||
        !Object.entries(profile).every(([key, value]) => values[key] === value)
      )
        return Promise.reject(fail());
      const remaining = deadline - now();
      if (remaining <= 0) return Promise.reject(fail(124));
      const number = sequence + 1;
      const frame = `${JSON.stringify({
        schemaVersion: 1,
        runId,
        sequence: number,
        phase,
        action,
        profile: finalClear ? null : expected[2],
      })}\n`;
      if (Buffer.byteLength(frame) > LIMIT) return Promise.reject(fail());
      return new Promise((resolve, reject) => {
        pending = {
          resolve,
          reject,
          sequence: number,
          phase,
          ordered: inOrder,
          timer: setTimer(() => fail(124), remaining),
        };
        try {
          stream.write(frame, (writeError) => {
            if (writeError) fail();
          });
        } catch {
          fail();
        }
      });
    },
    cancel(code) {
      fail(code === 130 || code === 143 ? code : 1);
    },
    finish() {
      retirement ??= (async () => {
        closing = true;
        if (pending || !finished || !ordered || sequence !== steps.length) fail();
        if (!closed) stream.destroy();
        await closeWait;
        if (terminal) throw terminal;
      })();
      return retirement;
    },
    statusOf,
  };
}
module.exports = { createChannel, statusOf };
