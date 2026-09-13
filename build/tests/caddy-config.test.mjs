import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

// Offline source invariants, not a replacement for `caddy validate` against the
// deployment's exact image and environment. Never start a proxy or contact a CA.
const source = readFileSync(new URL('../../Caddyfile', import.meta.url), 'utf8');
const configuration = source.split('\n').filter(line => !/^\s*#/.test(line)).join('\n');

test('Caddy supports deployment environment overrides while retaining host-local defaults', () => {
  assert.match(configuration, /^\{\$CADDY_DOMAIN:simplestchat\.example\.com\} \{$/m);
  assert.match(configuration, /^\s*reverse_proxy \{\$CADDY_UPSTREAM:127\.0\.0\.1:3000\} \{$/m);
  assert.equal((configuration.match(/^\s*reverse_proxy\b/gm) ?? []).length, 1);
});

test('Caddy denies the root and descendant paths of private operational endpoints', () => {
  const matcher = configuration.match(/^\s*@private path (.+)$/m);
  assert.ok(matcher, 'The shared private-endpoint matcher must be explicit');
  assert.deepEqual(matcher[1].split(/\s+/).sort(),
    ['/metrics', '/metrics/*', '/diagnostics', '/diagnostics/*'].sort());
  assert.match(configuration, /^\s*respond @private "Not Found" 404$/m);
  assert.doesNotMatch(configuration, /^\s*(?:order|rewrite|uri|route|handle|handle_path)\b/m,
    'Routing changes require revalidating that the private denial runs before the upstream');
});

test('Caddy replaces untrusted proxy-authentication headers with the configured secret', () => {
  const headers = configuration.match(/^\s*header_up\b.*$/gm) ?? [];
  assert.deepEqual(headers.map(line => line.trim()),
    ['header_up X-SimplestChat-Proxy {$TRUSTED_PROXY_SECRET}']);
});

test('Caddy leaves admin access at its loopback-only default without remote proxy trust', () => {
  assert.doesNotMatch(configuration, /^\s*admin\b/m);
  assert.doesNotMatch(configuration, /^\s*(?:trusted_proxies|client_ip_headers)\b/m);
});

test('Caddy does not enable access, credential, or verbose request logging', () => {
  assert.doesNotMatch(configuration, /^\s*(?:log|log_append|log_credentials|debug|trace)\b/m);
});

test('Caddy retains browser isolation, transport security, and media permissions', () => {
  for (const expected of [
    'Strict-Transport-Security "max-age=31536000"',
    'X-Content-Type-Options "nosniff"',
    'X-Frame-Options "DENY"',
    'Referrer-Policy "no-referrer"',
    'Permissions-Policy "camera=(self), microphone=(self), display-capture=(self), geolocation=()"',
    'Cross-Origin-Opener-Policy "same-origin"',
    'Cross-Origin-Resource-Policy "same-origin"',
    '-Server',
  ]) assert.ok(configuration.split('\n').some(line => line.trim() === expected), expected);
  assert.match(configuration, /^\s*Content-Security-Policy "default-src 'self'; .*frame-ancestors 'none'; .*connect-src 'self'; .*upgrade-insecure-requests"$/m);
});

test('Caddy bounds request setup without imposing an upgraded WebSocket write deadline', () => {
  assert.match(configuration, /^\s*read_header 10s$/m);
  assert.match(configuration, /^\s*read_body 30s$/m);
  assert.match(configuration, /^\s*idle 2m$/m);
  assert.match(configuration, /^\s*max_header_size 16KB$/m);
  assert.doesNotMatch(configuration, /^\s*write\b/m);
});
