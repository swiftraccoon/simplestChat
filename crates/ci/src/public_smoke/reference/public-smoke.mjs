import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const WORK_TIMEOUT_MS = 35_000;
const CLEANUP_TIMEOUT_MS = 5_000;
const TOTAL_TIMEOUT_MS = 45_000;
const CHECK_NAMES = ['health', 'ready', 'homepage', 'securityHeaders', 'staticAssets', 'privateEndpoints',
  'guestJoins', 'textAcknowledgedAndDelivered', 'firstLeaveObserved', 'webSocketCleanup'];
const HELP = `Usage: node build/public-smoke.mjs --origin https://chat.example.com [--room lobby]

Use only a deployment you own or are authorized to test. The origin must be
explicit; redirects and off-origin assets are not followed. Requires Node 22.12+.
This joins two guests and WRITES ONE PUBLIC TEST MESSAGE in the selected room.
It checks HTTPS, static assets and WSS text delivery, then leaves and closes.
It does not access a camera or microphone, create accounts, or test media.
One sanitized JSON summary is printed. There are no retries. Total deadline: 45s.
`;

class SmokeFailure extends Error {
  constructor(code) { super(code); this.code = code; }
}

function requireCheck(condition, code) {
  if (!condition) throw new SmokeFailure(code);
}

/** Reject ambiguous inputs before any connection or mutation. */
export function parseOptions(args) {
  if (args.length === 1 && args[0] === '--help') return { help: true };
  const options = { room: 'lobby' };
  const seen = new Set();
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    requireCheck(['--origin', '--room'].includes(flag) && !seen.has(flag), 'invalid_arguments');
    const value = args[index + 1];
    requireCheck(typeof value === 'string' && value.length > 0 && !value.startsWith('--'), 'invalid_arguments');
    seen.add(flag);
    options[flag.slice(2)] = value;
  }
  requireCheck(typeof options.origin === 'string' && !/[\s\\]/.test(options.origin), 'invalid_origin');
  let origin;
  try { origin = new URL(options.origin); } catch { throw new SmokeFailure('invalid_origin'); }
  requireCheck(origin.protocol === 'https:' && origin.hostname && !origin.username && !origin.password
    && origin.pathname === '/' && !origin.search && !origin.hash
    && /^https:\/\/[^/?#]+\/?$/.test(options.origin), 'invalid_origin');
  requireCheck(/^[A-Za-z0-9_-]{1,128}$/.test(options.room), 'invalid_room');
  return { origin: origin.origin, room: options.room };
}

/** Only exact, same-origin Vite asset paths are eligible for this bounded smoke. */
export function assetPaths(html) {
  requireCheck(typeof html === 'string', 'invalid_homepage');
  const result = [];
  for (const tag of html.match(/<(?:script|link)\b[^>]*>/gi) ?? []) {
    const attribute = name => tag.match(new RegExp(`\\b${name}\\s*=\\s*(["'])(.*?)\\1`, 'i'))?.[2];
    const script = /^<script\b/i.test(tag) && attribute('type') === 'module';
    const style = /^<link\b/i.test(tag) && attribute('rel') === 'stylesheet';
    if (!script && !style) continue;
    const path = attribute(script ? 'src' : 'href');
    const kind = script ? 'js' : 'css';
    requireCheck(typeof path === 'string' && new RegExp(`^/assets/[A-Za-z0-9_.-]+\\.${kind}$`).test(path), 'unsafe_asset_path');
    requireCheck(!result.some(item => item.path === path), 'duplicate_asset');
    result.push({ path, kind });
  }
  requireCheck(result.length <= 6 && result.some(item => item.kind === 'js')
    && result.some(item => item.kind === 'css'), 'missing_or_excess_assets');
  return result;
}

export function validSecurityHeaders(headers) {
  const directives = new Map();
  for (const directive of (headers.get('content-security-policy') ?? '').split(';')) {
    const [name, ...values] = directive.trim().split(/\s+/);
    if (directives.has(name)) return false;
    directives.set(name, values.join(' '));
  }
  const hsts = headers.get('strict-transport-security')?.match(/(?:^|;)\s*max-age=(\d+)(?:\s*;|\s*$)/i);
  return !!hsts && Number(hsts[1]) >= 86400
    && headers.get('x-content-type-options') === 'nosniff'
    && headers.get('x-frame-options')?.toUpperCase() === 'DENY'
    && headers.get('referrer-policy') === 'no-referrer'
    && headers.get('cross-origin-opener-policy') === 'same-origin'
    && headers.get('cross-origin-resource-policy') === 'same-origin'
    && headers.get('permissions-policy') === 'camera=(self), microphone=(self), display-capture=(self), geolocation=()'
    && ['default-src', 'script-src', 'connect-src', 'form-action'].every(name => directives.get(name) === "'self'")
    && ['base-uri', 'object-src', 'frame-ancestors'].every(name => directives.get(name) === "'none'");
}

export function matchingChat(message, { type, clientMessageId, participantId, content }) {
  if (!['messageAck', 'chatReceived'].includes(type) || !message || message.type !== type
    || message.clientMessageId !== clientMessageId) return false;
  const entry = type === 'messageAck' ? message.message : message;
  return !!entry && entry.clientMessageId === clientMessageId && entry.participantId === participantId
    && entry.content === content && typeof entry.messageId === 'string' && entry.messageId.length > 0;
}

/** Deliberately project public inputs and counters; never serialize messages/errors. */
export function sanitizedSummary(state) {
  const checks = Object.fromEntries(CHECK_NAMES.map(name => [name, state.checks[name] === true]));
  return {
    schemaVersion: 1,
    origin: state.origin,
    room: state.room,
    runId: state.runId,
    startedAt: state.startedAt,
    completed: state.completed === true,
    passed: state.completed === true && state.failure === null && state.closed === 2 && state.joined === 2
      && state.sent === 1 && state.leaves === 2 && Object.values(checks).every(Boolean),
    durationMs: Math.max(0, Math.round(performance.now() - state.started)),
    checks,
    guestsJoined: state.joined,
    publicMessagesSent: state.sent,
    leaveRequestsSent: state.leaves,
    cleanWebSocketClosures: state.closed,
    failure: state.failure === null ? null : { stage: state.failure.stage, code: state.failure.code },
    limitations: [
      'No camera, microphone, media, browser rendering, passkeys, TURN, or capacity checks.',
      'Native WebSockets do not establish browser Origin-policy enforcement.',
      'The protocol has no leave acknowledgment; only the first leave is independently observed by the second guest.',
    ],
  };
}

async function bodyText(response, limit) {
  requireCheck(response.body !== null, 'missing_response_body');
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      requireCheck(size <= limit, 'response_too_large');
      chunks.push(value);
    }
  } finally { await reader.cancel().catch(() => {}); }
  return Buffer.concat(chunks, size).toString('utf8');
}

/** No raw frame is retained: predicates extract only the identity needed in memory. */
class Guest {
  constructor(url, signal) {
    requireCheck(!signal.aborted, 'work_deadline');
    this.socket = new WebSocket(url);
    this.waiters = new Set();
    this.failure = null;
    this.joined = false;
    this.left = false;
    this.closing = false;
    this.cleanClose = false;
    this.closed = new Promise(resolveClosed => {
      this.socket.addEventListener('close', event => {
        this.cleanClose = this.closing && event.wasClean && event.code === 1000;
        resolveClosed();
        this.fail('websocket_closed');
      });
    });
    this.opened = new Promise((resolveOpen, rejectOpen) => {
      this.socket.addEventListener('open', resolveOpen, { once: true });
      this.rejectOpen = rejectOpen;
    });
    this.opened.catch(() => {});
    this.socket.addEventListener('error', () => this.fail('websocket_failed'));
    this.socket.addEventListener('message', event => {
      if (typeof event.data !== 'string' || Buffer.byteLength(event.data) > 256 * 1024) {
        this.fail('invalid_websocket_message');
        return;
      }
      let message;
      try { message = JSON.parse(event.data); } catch { this.fail('invalid_websocket_message'); return; }
      if (!message || typeof message.type !== 'string') { this.fail('invalid_websocket_message'); return; }
      if (['error', 'socialError', 'roomPasswordRequired', 'roomClosed', 'lobbyWaiting'].includes(message.type)) {
        this.fail('room_request_rejected');
        return;
      }
      for (const waiter of this.waiters) {
        if (waiter.predicate(message)) {
          this.waiters.delete(waiter);
          waiter.resolve(message);
        }
      }
    });
    this.abort = () => this.fail('work_deadline');
    signal.addEventListener('abort', this.abort, { once: true });
    if (signal.aborted) this.abort();
  }

  fail(code) {
    this.failure ??= new SmokeFailure(code);
    this.rejectOpen?.(this.failure);
    for (const waiter of this.waiters) waiter.reject(this.failure);
    this.waiters.clear();
  }

  wait(predicate) {
    const promise = this.failure ? Promise.reject(this.failure)
      : new Promise((resolveWait, reject) => this.waiters.add({ predicate, resolve: resolveWait, reject }));
    promise.catch(() => {});
    return promise;
  }

  send(message) {
    requireCheck(this.socket.readyState === WebSocket.OPEN, 'websocket_not_open');
    this.socket.send(JSON.stringify(message));
  }

  leave(state) {
    if (this.joined && !this.left && this.socket.readyState === WebSocket.OPEN) {
      this.send({ type: 'leaveRoom' });
      this.left = true;
      state.leaves++;
    }
  }

  close() {
    this.closing = true;
    try { this.socket.close(1000, 'Public smoke complete'); } catch { this.fail('websocket_close_failed'); }
  }
}

export async function runSmoke(options, onDeadline = () => {}) {
  const state = { ...options, runId: randomUUID(), startedAt: new Date().toISOString(), started: performance.now(),
    completed: false, failure: null, checks: {}, joined: 0, sent: 0, leaves: 0, closed: 0 };
  const guests = [];
  const controller = new AbortController();
  let stage = 'configuration';
  const hardDeadline = setTimeout(() => {
    state.failure = { stage, code: 'total_deadline' };
    controller.abort();
    onDeadline(sanitizedSummary(state));
  }, TOTAL_TIMEOUT_MS);
  const workDeadline = setTimeout(() => controller.abort(), WORK_TIMEOUT_MS);
  const request = async (path, expectedStatus, limit, expectedType) => {
    const response = await fetch(`${options.origin}${path}`, { redirect: 'error', credentials: 'omit',
      signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8000)]), headers: { Accept: expectedType } });
    requireCheck(response.status === expectedStatus, 'unexpected_http_status');
    if (expectedType !== '*/*') requireCheck(response.headers.get('content-type')?.toLowerCase().startsWith(expectedType), 'unexpected_content_type');
    return { response, text: await bodyText(response, limit) };
  };
  try {
    requireCheck(process.env.NODE_TLS_REJECT_UNAUTHORIZED !== '0', 'tls_verification_disabled');
    requireCheck(typeof globalThis.WebSocket === 'function' && typeof globalThis.fetch === 'function', 'unsupported_node');
    for (const [path, status] of [['/health', 'ok'], ['/ready', 'ready']]) {
      stage = path.slice(1);
      const { text } = await request(path, 200, 4096, 'application/json');
      let body;
      try { body = JSON.parse(text); } catch { throw new SmokeFailure('invalid_status_json'); }
      requireCheck(body?.status === status, 'unexpected_service_status');
      state.checks[stage] = true;
    }
    stage = 'homepage';
    const home = await request('/', 200, 128 * 1024, 'text/html');
    requireCheck(validSecurityHeaders(home.response.headers), 'missing_security_headers');
    state.checks.homepage = true;
    state.checks.securityHeaders = true;
    stage = 'assets';
    for (const asset of assetPaths(home.text)) {
      const { response, text } = await request(asset.path, 200, 2 * 1024 * 1024, '*/*');
      const type = response.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
      requireCheck(asset.kind === 'css' ? type === 'text/css' : ['text/javascript', 'application/javascript'].includes(type), 'unexpected_asset_type');
      requireCheck(text.trim().length > 0, 'empty_asset');
    }
    state.checks.staticAssets = true;
    stage = 'private_endpoints';
    for (const path of ['/metrics', '/diagnostics', '/diagnostics/media']) await request(path, 404, 4096, '*/*');
    state.checks.privateEndpoints = true;
    stage = 'guest_join';
    for (let index = 0; index < 2; index++) {
      const guest = new Guest(`${options.origin.replace(/^https:/, 'wss:')}/ws`, controller.signal);
      guests.push(guest);
      await guest.opened;
      const joined = guest.wait(message => message.type === 'roomJoined');
      guest.send({ type: 'joinRoom', roomId: options.room, participantName: `Smoke ${state.runId.slice(0, 8)} ${index + 1}` });
      const message = await joined;
      requireCheck(typeof message.participantId === 'string' && message.participantId.length > 0, 'invalid_join_response');
      guest.participantId = message.participantId;
      guest.joined = true;
      state.joined++;
    }
    requireCheck(guests[0].participantId !== guests[1].participantId, 'duplicate_guest_identity');
    state.checks.guestJoins = true;
    stage = 'text_delivery';
    const expected = { clientMessageId: `smoke-${state.runId}`, participantId: guests[0].participantId,
      content: `[SimplestChat public smoke ${state.runId}] HTTPS and text delivery check.` };
    const acknowledgment = guests[0].wait(message => matchingChat(message, { ...expected, type: 'messageAck' }));
    const delivery = guests[1].wait(message => matchingChat(message, { ...expected, type: 'chatReceived' }));
    guests[0].send({ type: 'chatMessage', content: expected.content, clientMessageId: expected.clientMessageId });
    state.sent++;
    const [ack, received] = await Promise.all([acknowledgment, delivery]);
    requireCheck(ack.message.messageId === received.messageId, 'chat_identity_mismatch');
    state.checks.textAcknowledgedAndDelivered = true;
    stage = 'explicit_leave';
    const left = guests[1].wait(message => message.type === 'participantLeft' && message.participantId === guests[0].participantId);
    guests[0].leave(state);
    await left;
    state.checks.firstLeaveObserved = true;
    guests[1].leave(state);
  } catch (error) {
    state.failure = { stage, code: controller.signal.aborted ? 'work_deadline'
      : error instanceof SmokeFailure ? error.code : 'operation_failed' };
  } finally {
    clearTimeout(workDeadline);
    stage = 'cleanup';
    let cleanupTimer;
    try {
      for (const guest of guests) {
        try { guest.leave(state); } catch { state.failure ??= { stage, code: 'leave_failed' }; }
        guest.close();
      }
      await Promise.race([Promise.all(guests.map(guest => guest.closed)), new Promise((_, reject) => {
        cleanupTimer = setTimeout(() => reject(new SmokeFailure('cleanup_deadline')), CLEANUP_TIMEOUT_MS);
      })]);
      state.closed = guests.filter(guest => guest.cleanClose).length;
      requireCheck(guests.length === 2 && state.closed === 2, 'unclean_websocket_shutdown');
      state.checks.webSocketCleanup = true;
    } catch (error) {
      state.failure ??= { stage, code: error instanceof SmokeFailure ? error.code : 'cleanup_failed' };
    } finally {
      state.closed = guests.filter(guest => guest.cleanClose).length;
      clearTimeout(cleanupTimer);
      clearTimeout(hardDeadline);
      controller.abort();
      state.completed = true;
    }
  }
  return sanitizedSummary(state);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  let options;
  try { options = parseOptions(process.argv.slice(2)); }
  catch (error) {
    process.stderr.write(`${error instanceof SmokeFailure ? error.code : 'invalid_arguments'}; use --help\n`);
    process.exitCode = 1;
  }
  if (options?.help) process.stdout.write(HELP);
  else if (options) {
    let finished = false;
    const finish = summary => {
      if (finished) return;
      finished = true;
      // Force termination only of this CLI if native sockets did not close.
      process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`, () => process.exit(summary.passed ? 0 : 1));
      setTimeout(() => process.exit(1), 250);
    };
    finish(await runSmoke(options, finish));
  }
}
