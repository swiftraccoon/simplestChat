import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import pwa from '../e2e/pwa-checks.cjs';

function fixture() {
  const calls = [];
  const window = {
    fetch(input, init) {
      assert.equal(this, window);
      calls.push({ input, init });
      return Promise.resolve(Response.json({ native: true }));
    },
  };
  const scope = {
    window,
    navigator: { userActivation: { isActive: true } },
    Notification: class {},
    EventTarget,
    Request,
    Response,
    URL,
    location: new URL('https://owned.example.test/'),
  };
  vm.runInNewContext(`(${pwa.installBrowserPushFixture.toString()})()`, scope);
  return {
    scope,
    evidence: window.__pushFixture,
    calls,
    fetch: window.fetch,
    push: (method = 'GET', body, extra = {}) =>
      window.fetch('/api/auth/push', {
        method,
        headers: { Authorization: 'Bearer owned-fixture-only', 'Content-Type': 'application/json' },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        ...extra,
      }),
  };
}

test('the controlled push lifecycle requires explicit permission and exact endpoint intent', async () => {
  const f = fixture();
  assert.equal(f.evidence.permissionRequests, 0);
  assert.equal(f.evidence.subscriptions, 0);
  const initial = await (await f.push()).json();
  assert.deepEqual(initial, { enabled: false, publicKey: `B${'A'.repeat(86)}` });
  assert.equal(await f.scope.Notification.requestPermission(), 'granted');
  assert.deepEqual(Array.from(f.evidence.permissionGestures), [true]);
  const registration = await f.scope.navigator.serviceWorker.register('/sw.js', {
    scope: '/',
    updateViaCache: 'none',
  });
  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: initial.publicKey,
  });
  const stored = await f.push('PUT', { endpoint: subscription.endpoint });
  assert.equal(stored.status, 204);
  assert.equal(await stored.text(), '');
  assert.equal((await (await f.push()).json()).enabled, true);
  assert.equal((await f.push('DELETE')).status, 204);
  assert.equal(await subscription.unsubscribe(), true);
  assert.equal((await (await f.push()).json()).enabled, false);
  assert.equal(await registration.pushManager.getSubscription(), null);
  assert.equal(f.evidence.http.put, 1);
  assert.equal(f.evidence.http.delete, 1);
  assert.equal(f.evidence.http.rejected, 0);
  assert.equal(f.calls.length, 0, 'the fake endpoint cannot escape to the real backend');
});

test('other APIs, account SSE, alternate push URLs and requests retain native fetch semantics', async () => {
  const f = fixture();
  const input = new Request('https://owned.example.test/api/auth/updates', {
    headers: { Authorization: 'Bearer native-account-only' },
  });
  const controller = new AbortController();
  const init = { signal: controller.signal, credentials: 'same-origin' };
  assert.deepEqual(await (await f.fetch(input, init)).json(), { native: true });
  assert.equal(f.calls[0].input, input);
  assert.equal(f.calls[0].init, init);
  for (const url of [
    '/api/auth/notification-preferences',
    '/api/auth/push?unowned=1',
    '/api/auth/push/extra',
    'https://other.example.test/api/auth/push',
  ])
    assert.deepEqual(await (await f.fetch(url)).json(), { native: true });
  assert.equal(f.calls.length, 5);
  assert.equal(f.evidence.http.requests, 0);
});

test('push fixture rejects missing bearer, unexpected methods and malformed or foreign endpoint bodies', async () => {
  const f = fixture();
  assert.equal((await f.push('GET', undefined, { headers: {} })).status, 401);
  assert.equal((await f.push('POST')).status, 405);
  for (const body of [
    { endpoint: 'https://web.push.apple.com/unowned' },
    { endpoint: 'https://push.example.test/owned-browser-fixture', extra: true },
    [],
    null,
  ])
    assert.equal((await f.push('PUT', body)).status, 400);
  assert.equal((await f.push('PUT', undefined, { body: '{' })).status, 400);
  assert.equal((await f.push('DELETE', { unexpected: true })).status, 400);
  assert.equal(f.evidence.enabled, false);
  assert.equal(f.evidence.http.rejected, 8);
  assert.equal(f.calls.length, 0);
});

test('aborted fixture mutations cannot enable delivery and diagnostics remain bounded and content-free', async () => {
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    f.push(
      'PUT',
      { endpoint: 'https://push.example.test/owned-browser-fixture' },
      {
        signal: controller.signal,
      },
    ),
    (error) => error.name === 'AbortError',
  );
  assert.equal(f.evidence.enabled, false);
  f.evidence.http.requests = 1000;
  await f.push();
  assert.equal(f.evidence.http.requests, 1000);
  const evidence = vm.runInNewContext(`(${pwa.collectPushFixtureEvidence.toString()})()`, f.scope);
  const serialized = JSON.stringify(evidence);
  assert.ok(!serialized.includes('Bearer'));
  assert.ok(!serialized.includes('endpoint'));
  assert.ok(!serialized.includes('owned-fixture-only'));
  assert.equal(fixture().evidence.enabled, false, 'a fresh document never implicitly enables push');
});
