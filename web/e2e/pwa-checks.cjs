const assert = require('node:assert/strict');

function installBrowserPushFixture() {
  const evidence = (window.__pushFixture = {
    permissionRequests: 0,
    permissionGestures: [],
    subscriptions: 0,
    unsubscribed: 0,
    registration: null,
    permission: 'default',
    result: 'granted',
    enabled: false,
    http: { requests: 0, get: 0, put: 0, delete: 0, rejected: 0, native: 0 },
  });
  const endpoint = 'https://push.example.test/owned-browser-fixture';
  const nativeFetch = window.fetch.bind(window);
  const count = (key) => {
    evidence.http[key] = Math.min(evidence.http[key] + 1, 1000);
  };
  // Push delivery is a controlled fixture. WebKit returned a real backend 400
  // for its fake endpoint despite the former page.route interception. Keep
  // that endpoint inside this exact fetch boundary; the bypass cause is unknown.
  window.fetch = async (input, init) => {
    let url;
    try {
      url = new URL(input instanceof Request ? input.url : input, location.href);
    } catch {
      count('native');
      return nativeFetch(input, init);
    }
    if (url.origin !== location.origin || url.pathname !== '/api/auth/push' || url.search !== '') {
      count('native');
      return nativeFetch(input, init);
    }
    const request = new Request(input instanceof Request ? input : url, init);
    request.signal.throwIfAborted();
    count('requests');
    const reject = (status) => {
      count('rejected');
      return new Response(null, { status });
    };
    if (!/^Bearer \S+$/.test(request.headers.get('authorization') ?? '')) return reject(401);
    if (request.method === 'GET') {
      count('get');
      return Response.json({ enabled: evidence.enabled, publicKey: `B${'A'.repeat(86)}` });
    }
    if (request.method === 'PUT') {
      count('put');
      let body;
      try {
        body = await request.json();
      } catch {
        return reject(400);
      }
      request.signal.throwIfAborted();
      if (
        !body ||
        typeof body !== 'object' ||
        Array.isArray(body) ||
        Object.keys(body).length !== 1 ||
        body.endpoint !== endpoint
      )
        return reject(400);
      evidence.enabled = true;
      return new Response(null, { status: 204 });
    }
    if (request.method === 'DELETE') {
      count('delete');
      if ((await request.text()) !== '') return reject(400);
      request.signal.throwIfAborted();
      evidence.enabled = false;
      return new Response(null, { status: 204 });
    }
    return reject(405);
  };
  const container = new EventTarget();
  let subscription = null;
  const registration = {
    active: { postMessage() {} },
    pushManager: {
      getSubscription: async () => subscription,
      subscribe: async (options) => {
        if (!options.userVisibleOnly || !options.applicationServerKey)
          throw new Error('Expected visible notifications and application key');
        evidence.subscriptions++;
        subscription = {
          endpoint,
          unsubscribe: async () => {
            evidence.unsubscribed++;
            subscription = null;
            return true;
          },
        };
        return subscription;
      },
    },
  };
  container.register = async (url, options) => {
    evidence.registration = { url, scope: options.scope, updateViaCache: options.updateViaCache };
    return registration;
  };
  container.ready = Promise.resolve(registration);
  Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: container });
  window.PushManager ??= class {};
  Object.defineProperty(Notification, 'permission', {
    configurable: true,
    get: () => evidence.permission,
  });
  Notification.requestPermission = async () => {
    evidence.permissionRequests++;
    evidence.permissionGestures.push(navigator.userActivation.isActive);
    evidence.permission = evidence.result;
    return evidence.permission;
  };
}

/** Install before navigation. Native Push/permission and HTTP delivery are controlled fixtures. */
async function installPwaFixture(page) {
  await page.addInitScript(installBrowserPushFixture);
}

function collectPushFixtureEvidence() {
  const fixture = window.__pushFixture;
  if (!fixture) return null;
  return {
    permissionRequests: fixture.permissionRequests,
    subscriptions: fixture.subscriptions,
    unsubscribed: fixture.unsubscribed,
    http: { ...fixture.http },
  };
}

async function checkPwa(page, { openAccount, closeDialog }) {
  assert.equal(await page.evaluate(() => window.__pushFixture.permissionRequests), 0);
  const account = await openAccount();
  const enable = account.getByRole('button', { name: 'Enable notifications', exact: true });
  await enable.waitFor({ state: 'visible' });
  await page.waitForFunction(() =>
    [...document.querySelectorAll('button')].some(
      (button) => button.textContent === 'Enable notifications' && !button.disabled,
    ),
  );
  assert.equal(await page.evaluate(() => window.__pushFixture.permissionRequests), 0);
  await enable.click();
  await account.getByRole('button', { name: 'Disable notifications', exact: true }).waitFor();
  assert.deepEqual(
    await page.evaluate(() => ({
      permissionRequests: window.__pushFixture.permissionRequests,
      gestures: window.__pushFixture.permissionGestures,
      subscriptions: window.__pushFixture.subscriptions,
      registration: window.__pushFixture.registration,
      putRequests: window.__pushFixture.http.put,
      rejected: window.__pushFixture.http.rejected,
    })),
    {
      permissionRequests: 1,
      gestures: [true],
      subscriptions: 1,
      registration: { url: '/sw.js', scope: '/', updateViaCache: 'none' },
      putRequests: 1,
      rejected: 0,
    },
  );
  await account.getByRole('button', { name: 'Disable notifications', exact: true }).click();
  await account.getByRole('button', { name: 'Enable notifications', exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.__pushFixture.unsubscribed), 1);
  assert.deepEqual(
    await page.evaluate(() => ({
      put: window.__pushFixture.http.put,
      delete: window.__pushFixture.http.delete,
      rejected: window.__pushFixture.http.rejected,
    })),
    { put: 1, delete: 1, rejected: 0 },
  );
  await closeDialog(account);
  await page.evaluate(() => {
    navigator.serviceWorker.dispatchEvent(
      new MessageEvent('message', { data: { type: 'openMessages' } }),
    );
  });
  const inbox = page.getByRole('dialog', { name: 'Messages', exact: true });
  await inbox.waitFor({ state: 'visible' });
  await closeDialog(inbox);
  const manifest = await page.request.get(new URL('/manifest.webmanifest', page.url()).toString());
  assert.equal(manifest.status(), 200);
  const value = await manifest.json();
  assert.equal(value.display, 'standalone');
  assert.deepEqual(value.icons.map((icon) => icon.sizes).sort(), ['192x192', '512x512']);
  return {
    transport: 'controlled same-origin push fetch',
    ...(await page.evaluate(() => window.__pushFixture.http)),
  };
}

module.exports = {
  installBrowserPushFixture,
  installPwaFixture,
  checkPwa,
  collectPushFixtureEvidence,
};
