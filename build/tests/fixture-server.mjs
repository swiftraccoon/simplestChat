#!/usr/bin/env node
// Minimal child used only to test test-runner ownership and environment isolation.
import assert from 'node:assert/strict';
import http from 'node:http';

assert.equal(process.env.RUN_MIGRATIONS, 'true');
assert.equal(process.env.MEDIA_WORKERS, '1');
assert.equal(process.env.BIND_ADDR, '127.0.0.1');
for (const name of ['WEBAUTHN_ORIGIN', 'TURN_URLS', 'AUTH_MAX_CONCURRENCY', 'MAX_USERS']) {
  assert.equal(process.env[name], undefined, `Inherited ${name} must not reach the test server`);
}
const server = http.createServer((_, response) => {
  response.setHeader('Content-Type', 'application/json');
  response.end('[]');
});
server.listen(Number(process.env.PORT), process.env.BIND_ADDR, () => {
  console.log(`FIXTURE_PID=${process.pid}`);
});
process.on('SIGTERM', () => server.close());
