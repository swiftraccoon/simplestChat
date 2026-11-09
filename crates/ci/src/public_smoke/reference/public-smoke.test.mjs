import assert from 'node:assert/strict';
import test from 'node:test';
import { assetPaths, matchingChat, parseOptions, sanitizedSummary, validSecurityHeaders } from '../public-smoke.mjs';

test('public smoke requires an explicit HTTPS origin and defaults only the room', () => {
  assert.deepEqual(parseOptions(['--origin', 'https://chat.example.com/']), { origin: 'https://chat.example.com', room: 'lobby' });
  assert.deepEqual(parseOptions(['--room', 'test_room-1', '--origin', 'https://chat.example.com:8443']),
    { origin: 'https://chat.example.com:8443', room: 'test_room-1' });
  assert.deepEqual(parseOptions(['--help']), { help: true });
});

test('invalid arguments and unsafe origins fail without including supplied values', () => {
  const bad = [[], ['--room', 'lobby'], ['--origin'], ['--unknown', 'secret'], ['--help', '--origin', 'https://chat.example.com'],
    ['--origin', 'https://chat.example.com', '--origin', 'https://other.example.com']];
  for (const origin of ['http://chat.example.com', 'https://secret@chat.example.com', 'https://user:secret@chat.example.com',
    'https://chat.example.com/path', 'https://chat.example.com/?secret', 'https://chat.example.com/#secret',
    'https://chat.example.com?', 'https://chat.example.com#', ' https://chat.example.com',
    'https://chat.example.com\n', 'https://chat.example.com\\secret', 'not-an-origin']) bad.push(['--origin', origin]);
  for (const args of bad) assert.throws(() => parseOptions(args), error => {
    assert.match(error.message, /^invalid_(arguments|origin)$/);
    assert.doesNotMatch(error.message, /secret/);
    return true;
  });
});

test('room names use the actual server room ID constraints', () => {
  for (const room of ['', 'a'.repeat(129), 'room/name', 'room name', 'room?token=secret', 'room\n']) {
    assert.throws(() => parseOptions(['--origin', 'https://chat.example.com', '--room', room]), /invalid_(room|arguments)/);
  }
  assert.equal(parseOptions(['--origin', 'https://chat.example.com', '--room', 'a'.repeat(128)]).room.length, 128);
});

const html = `<script type="module" crossorigin src="/assets/index-A1.js"></script>
<link rel="stylesheet" crossorigin href="/assets/index-B2.css">`;

test('asset discovery permits only bounded same-origin static JS and CSS paths', () => {
  assert.deepEqual(assetPaths(html), [{ path: '/assets/index-A1.js', kind: 'js' }, { path: '/assets/index-B2.css', kind: 'css' }]);
  for (const path of ['https://other.example.com/x.js', '//other.example.com/x.js', '/api/auth/register',
    '/assets/../secret.js', '/assets/x.js?secret', '/assets/a%2fb.js']) {
    assert.throws(() => assetPaths(html.replace('/assets/index-A1.js', path)), /unsafe_asset_path/);
  }
  assert.throws(() => assetPaths(''), /missing_or_excess_assets/);
  assert.throws(() => assetPaths(html + html), /duplicate_asset/);
  assert.throws(() => assetPaths(html + Array.from({ length: 5 }, (_, i) => `<script type="module" src="/assets/m${i}.js">`).join('')), /missing_or_excess_assets/);
});

const secureHeaders = () => new Headers({
  'strict-transport-security': 'max-age=31536000', 'x-content-type-options': 'nosniff', 'x-frame-options': 'DENY',
  'referrer-policy': 'no-referrer', 'cross-origin-opener-policy': 'same-origin', 'cross-origin-resource-policy': 'same-origin',
  'permissions-policy': 'camera=(self), microphone=(self), display-capture=(self), geolocation=()',
  'content-security-policy': "default-src 'self'; script-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'",
});

test('security predicate requires the supplied proxy protections without loosening script policy', () => {
  assert.equal(validSecurityHeaders(secureHeaders()), true);
  for (const name of [...secureHeaders().keys()]) {
    const headers = secureHeaders();
    headers.delete(name);
    assert.equal(validSecurityHeaders(headers), false, name);
  }
  for (const [name, value] of [['strict-transport-security', 'max-age=0'], ['x-frame-options', 'SAMEORIGIN'],
    ['content-security-policy', `${secureHeaders().get('content-security-policy')}; script-src *`],
    ['content-security-policy', secureHeaders().get('content-security-policy').replace("script-src 'self'", "script-src 'self' 'unsafe-inline'")]]) {
    const headers = secureHeaders();
    headers.set(name, value);
    assert.equal(validSecurityHeaders(headers), false);
  }
});

const expected = { clientMessageId: 'smoke-owned', participantId: 'owned-sender', content: 'owned marker' };
const entry = { ...expected, messageId: 'server-message-1' };

test('text delivery requires the exact owned sender, correlation ID, content, and server message ID', () => {
  assert.equal(matchingChat({ type: 'chatReceived', ...entry }, { ...expected, type: 'chatReceived' }), true);
  assert.equal(matchingChat({ type: 'messageAck', clientMessageId: expected.clientMessageId, message: entry },
    { ...expected, type: 'messageAck' }), true);
  for (const change of [{ type: 'chatMessage' }, { participantId: 'another-sender' }, { content: 'other text' },
    { clientMessageId: 'another-run' }, { messageId: '' }]) {
    assert.equal(matchingChat({ type: 'chatReceived', ...entry, ...change }, { ...expected, type: 'chatReceived' }), false);
  }
  for (const value of [null, {}, { type: 'messageAck', clientMessageId: expected.clientMessageId },
    { type: 'messageAck', clientMessageId: expected.clientMessageId, message: { ...entry, clientMessageId: 'wrong' } }]) {
    assert.equal(matchingChat(value, { ...expected, type: 'messageAck' }), false);
  }
});

test('summary never copies server messages, reconnect credentials, errors, or account data', () => {
  const checks = Object.fromEntries(['health', 'ready', 'homepage', 'securityHeaders', 'staticAssets', 'privateEndpoints',
    'guestJoins', 'textAcknowledgedAndDelivered', 'firstLeaveObserved', 'webSocketCleanup'].map(name => [name, true]));
  const state = { origin: 'https://chat.example.com', room: 'lobby', runId: 'owned', startedAt: '2026-01-01T00:00:00Z',
    started: performance.now(), completed: true, failure: null, closed: 2, joined: 2, sent: 1, leaves: 2,
    checks: { ...checks, extra: 'private-secret' }, reconnectToken: 'private-secret', message: { content: 'private-secret' },
    error: new Error('private-secret'), user: { email: 'private-secret' } };
  const report = sanitizedSummary(state);
  assert.equal(report.passed, true);
  assert.equal(report.publicMessagesSent, 1);
  assert.doesNotMatch(JSON.stringify(report), /private-secret|reconnectToken|email/);
  for (const patch of [{ completed: false }, { closed: 1 }, { joined: 1 }, { sent: 0 }, { sent: 2 }, { leaves: 1 },
    { checks: { ...checks, staticAssets: false } }, { checks: { ...checks, health: 'private-secret' } },
    { failure: { stage: 'guest_join', code: 'operation_failed', raw: 'private-secret' } }]) {
    const failed = sanitizedSummary({ ...state, ...patch });
    assert.equal(failed.passed, false);
    assert.doesNotMatch(JSON.stringify(failed), /private-secret/);
  }
});
