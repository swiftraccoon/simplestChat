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
  });
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
          endpoint: 'https://push.example.test/owned-browser-fixture',
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
  let enabled = false;
  await page.route('**/api/auth/push', async (route) => {
    const request = route.request();
    assert.match(request.headers().authorization ?? '', /^Bearer /);
    if (request.method() === 'GET') {
      await route.fulfill({
        json: { enabled, publicKey: `B${'A'.repeat(86)}` },
      });
    } else if (request.method() === 'PUT') {
      assert.deepEqual(request.postDataJSON(), {
        endpoint: 'https://push.example.test/owned-browser-fixture',
      });
      enabled = true;
      await route.fulfill({ status: 204 });
    } else if (request.method() === 'DELETE') {
      enabled = false;
      await route.fulfill({ status: 204 });
    } else throw new Error('Unexpected push fixture method');
  });
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
    })),
    {
      permissionRequests: 1,
      gestures: [true],
      subscriptions: 1,
      registration: { url: '/sw.js', scope: '/', updateViaCache: 'none' },
    },
  );
  await account.getByRole('button', { name: 'Disable notifications', exact: true }).click();
  await account.getByRole('button', { name: 'Enable notifications', exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.__pushFixture.unsubscribed), 1);
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
}

module.exports = { installPwaFixture, checkPwa };
