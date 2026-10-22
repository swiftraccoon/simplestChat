/** Three fixed server transitions over Rust's anonymous standard-input duplex. */
const { Socket } = require('node:net');

const PHASES = ['stop-first', 'start-second', 'stop-second'];
const LIMIT = 1024;
class ControlFailure extends Error {
  constructor(status) {
    super('Owned restart browser control failed.');
    this.status = status;
  }
}
const statusOf = (error) => (error instanceof ControlFailure ? error.status : 1);

function createChannel({
  runId,
  stream,
  now = () => performance.now(),
  setTimer = setTimeout,
  clearTimer = clearTimeout,
} = {}) {
  if (typeof runId !== 'string' || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(runId))
    throw new ControlFailure(1);
  stream ??= new Socket({ fd: 0, readable: true, writable: true });
  const deadline = now() + 75000;
  let sequence = 0;
  let buffer = Buffer.alloc(0);
  let pending;
  let terminal;
  let closing = false;
  let closed = false;
  let retirement;
  let resolveClosed;
  const closeWait = new Promise((resolve) => (resolveClosed = resolve));
  const writes = new Set();
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
  const accept = (request) => {
    if (pending !== request || !request.acknowledged || !request.written) return;
    pending = undefined;
    clearTimer(request.timer);
    sequence = request.sequence;
    buffer = Buffer.alloc(0);
    request.resolve();
  };
  const data = (chunk) => {
    if (now() >= deadline) return void fail(124);
    if (!Buffer.isBuffer(chunk)) chunk = Buffer.from(chunk);
    if (!pending || pending.acknowledged || closing || buffer.length + chunk.length > LIMIT)
      return void fail();
    buffer = Buffer.concat([buffer, chunk]);
    const newline = buffer.indexOf(10);
    if (newline === -1) {
      if (buffer.length >= LIMIT) fail();
      return;
    }
    if (newline !== buffer.length - 1) return void fail();
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
        throw new Error('Invalid reply');
    } catch {
      return void fail();
    }
    if (!reply.passed) return void fail(reply.status);
    pending.acknowledged = true;
    accept(pending);
  };
  const error = () => fail();
  const end = () => {
    if (pending || sequence !== PHASES.length || buffer.length) fail();
  };
  const close = () => {
    closed = true;
    if (!closing || pending) fail();
    stream.removeListener('data', data);
    stream.removeListener('end', end);
    stream.removeListener('error', error);
    stream.removeListener('close', close);
    resolveClosed();
  };
  stream.on('data', data);
  stream.on('end', end);
  stream.on('error', error);
  stream.on('close', close);
  return {
    request(phase) {
      if (terminal) return Promise.reject(terminal);
      if (
        pending ||
        closing ||
        closed ||
        sequence >= PHASES.length ||
        typeof phase !== 'string' ||
        phase !== PHASES[sequence]
      )
        return Promise.reject(fail());
      const remaining = deadline - now();
      if (!Number.isFinite(remaining) || remaining <= 0) return Promise.reject(fail(124));
      const frame = `${JSON.stringify({ schemaVersion: 1, runId, sequence: sequence + 1, phase })}\n`;
      if (Buffer.byteLength(frame) > LIMIT) return Promise.reject(fail());
      return new Promise((resolve, reject) => {
        const request = {
          resolve,
          reject,
          sequence: sequence + 1,
          acknowledged: false,
          written: false,
          timer: setTimer(() => fail(124), remaining),
        };
        pending = request;
        let settle;
        const written = new Promise((done) => (settle = done));
        writes.add(written);
        written.then(() => writes.delete(written));
        const completed = (writeError) => {
          settle();
          if (writeError) fail();
          else {
            request.written = true;
            accept(request);
          }
        };
        try {
          stream.write(frame, completed);
        } catch {
          completed(new Error('Owned control write failed'));
        }
      });
    },
    cancel(code) {
      fail([124, 130, 143].includes(code) ? code : 1);
    },
    finish() {
      retirement ??= (async () => {
        closing = true;
        if (pending || sequence !== PHASES.length || buffer.length) fail();
        if (!closed) stream.destroy();
        // A positive ACK alone cannot retire a write whose callback is pending.
        // Socket destruction settles native writes; a violated runtime promise
        // stays unconfirmed under the parent's watchdog, never a positive receipt.
        await Promise.allSettled([...writes]);
        await closeWait;
        if (terminal) throw terminal;
      })();
      return retirement;
    },
  };
}

module.exports = { createChannel, statusOf };
