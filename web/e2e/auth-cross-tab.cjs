/** Owned loopback frontend only: real tabs, cookies, Web Locks and account UI;
 * HTTP/signaling fixtures avoid creating accounts or joining real rooms. */
const assert = require('node:assert/strict');
const { browserOptions } = require('./browser-options.cjs');
const { capabilities } = require('./capabilities-fixture.cjs');
const { signOut } = require('./account-menu.cjs');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:38179');
if (
  process.env.AUTH_CROSS_TAB_E2E !== '1' ||
  origin.protocol !== 'http:' ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error('Set AUTH_CROSS_TAB_E2E=1 with an owned HTTP loopback frontend origin.');

const options = browserOptions(process.env.E2E_BROWSER);
const markerKey = 'simplestchat-account-change-v1';

function accountSession(name, revision) {
  const payload = Buffer.from(
    JSON.stringify({ sub: name, exp: Math.floor(Date.now() / 1000) + 900 }),
  ).toString('base64url');
  return {
    token: `fixture.${payload}.signature-${revision}`,
    user: { id: name, email: `${name}@example.test`, display_name: name },
  };
}

async function startupRecovery(browser, result) {
  const context = await browser.newContext();
  const failures = [];
  let refreshes = 0;
  let logins = 0;
  let revision = 0;
  let observeRetry;
  const retryRequest = new Promise((resolve) => {
    observeRetry = resolve;
  });
  try {
    await context.addCookies([
      {
        name: 'fixture_refresh',
        value: 'First',
        url: origin.origin,
        httpOnly: true,
        sameSite: 'Strict',
      },
    ]);
    await context.addInitScript(
      ({ deferBody }) => {
        window.__restoreEvidence = {
          documentMarker: Array.from(crypto.getRandomValues(new Uint32Array(4))).join(':'),
          refreshes: 0,
          authenticatedOpens: 0,
          captures: 0,
          joins: 0,
          heldBodies: 0,
          releasedBodies: 0,
        };
        const nativeFetch = window.fetch.bind(window);
        window.fetch = async (input, init) => {
          const pathname = new URL(input instanceof Request ? input.url : input, location.href)
            .pathname;
          if (pathname === '/api/auth/refresh') window.__restoreEvidence.refreshes++;
          const response = await nativeFetch(input, init);
          if (deferBody && pathname === '/api/auth/refresh' && response.ok) {
            const read = response.json.bind(response);
            response.json = async () => {
              const data = await read();
              window.__restoreEvidence.heldBodies++;
              // The platform promise deliberately ignores abort. A newer login
              // must remain authoritative even when this old body later arrives.
              return new Promise((resolve) => {
                window.__releaseOldRestoreBody = () => {
                  window.__restoreEvidence.releasedBodies++;
                  resolve(data);
                };
              });
            };
          }
          return response;
        };
        class FixtureSocket {
          static OPEN = 1;
          static CONNECTING = 0;
          readyState = 0;
          constructor(_url, protocols) {
            setTimeout(() => {
              if (this.readyState !== FixtureSocket.CONNECTING) return;
              this.readyState = FixtureSocket.OPEN;
              this.onopen?.();
              if (
                this.readyState === FixtureSocket.OPEN &&
                protocols.some((protocol) => protocol.startsWith('ticket.'))
              )
                window.__restoreEvidence.authenticatedOpens++;
            }, 0);
          }
          close() {
            this.readyState = 3;
          }
          send(raw) {
            const request = JSON.parse(raw);
            if (request.type === 'joinRoom') window.__restoreEvidence.joins++;
            if (request.type === 'renewAuthentication')
              setTimeout(
                () =>
                  this.onmessage?.({
                    data: JSON.stringify({
                      type: 'authenticationRenewed',
                      requestId: request.requestId,
                      expiresAt: Math.floor(Date.now() / 1000) + 900,
                    }),
                  }),
                0,
              );
          }
        }
        window.WebSocket = FixtureSocket;
        const refuseCapture = () => {
          window.__restoreEvidence.captures++;
          return Promise.reject(new Error('Saved sign-in recovery must not request capture'));
        };
        navigator.mediaDevices.getUserMedia = refuseCapture;
        navigator.mediaDevices.getDisplayMedia = refuseCapture;
      },
      { deferBody: result === 'superseded' },
    );
    await context.route('**/api/**', async (route) => {
      const pathname = new URL(route.request().url()).pathname;
      if (pathname === '/api/capabilities') await route.fulfill({ json: capabilities });
      else if (pathname === '/api/auth/refresh') {
        refreshes++;
        assert.match(route.request().headers().cookie || '', /fixture_refresh=First/);
        if (refreshes === 1)
          await route.fulfill(
            result === 'superseded'
              ? { json: accountSession('First', ++revision) }
              : { status: 503, json: { error: 'Temporarily unavailable' } },
          );
        else if (refreshes === 2) observeRetry(route);
        else await route.fulfill({ status: 503, json: { error: 'Unexpected refresh' } });
      } else if (pathname === '/api/auth/login') {
        logins++;
        assert.equal(result, 'superseded');
        assert.equal(route.request().postDataJSON().email, 'Second@example.test');
        await route.fulfill({
          headers: { 'Set-Cookie': 'fixture_refresh=Second; HttpOnly; SameSite=Strict; Path=/' },
          json: accountSession('Second', ++revision),
        });
      } else if (pathname === '/api/auth/preferences') {
        assert.equal(route.request().method(), 'GET');
        assert.match(route.request().headers().authorization, /^Bearer /);
        await route.fulfill({
          json: {
            allowPrivateMessages: true,
            sounds: false,
            largeText: false,
            timestamps: 'time',
            ignored: [],
          },
        });
      } else if (pathname === '/api/auth/ws-ticket')
        await route.fulfill({ json: { ticket: 'a'.repeat(43), expires_in: 30 } });
      else if (pathname === '/api/telemetry') await route.fulfill({ status: 204 });
      else await route.fulfill({ json: [] });
    });
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    page.on('pageerror', (error) => failures.push(error.message));
    await page.goto(origin.toString());
    const documentMarker = await page.evaluate(() => window.__restoreEvidence.documentMarker);
    if (result === 'superseded') {
      await page.waitForFunction(() => typeof window.__releaseOldRestoreBody === 'function');
      await page.locator('#sign-in-btn').click();
      await page.locator('#login-email').fill('Second@example.test');
      await page.locator('#login-password').fill('Disposable-test-password');
      await page.locator('#login-submit').click();
      await page.waitForFunction(
        () =>
          document.getElementById('auth-display-name').textContent === 'Second' &&
          window.__restoreEvidence.authenticatedOpens === 1,
      );
      await page.evaluate(async () => {
        window.__releaseOldRestoreBody();
        await new Promise(requestAnimationFrame);
        await new Promise(requestAnimationFrame);
      });
      assert.equal(await page.locator('#auth-display-name').textContent(), 'Second');
      assert.equal(await page.locator('#auth-restore-notice').isVisible(), false);
      assert.equal(logins, 1);
    } else {
      const status = page.locator('#auth-restore-status');
      const retry = page.locator('#auth-restore-retry');
      await page.waitForFunction(
        () =>
          !document.getElementById('auth-restore-notice').hidden &&
          !document.getElementById('auth-restore-retry').disabled,
      );
      assert.equal(await status.getAttribute('role'), 'status');
      assert.ok((await status.textContent()).trim().length > 0);
      assert.equal(await retry.isVisible(), true);
      assert.equal(await retry.isEnabled(), true);
      assert.equal(await page.locator('#auth-bar-user').isVisible(), false);
      assert.equal(refreshes, 1, 'an unavailable startup does not retry itself');
      await retry.click();
      let timer;
      let pending;
      try {
        pending = await Promise.race([
          retryRequest,
          new Promise((_, reject) => {
            timer = setTimeout(
              () => reject(new Error('Saved sign-in Retry did not reach the fixture')),
              10000,
            );
          }),
        ]);
      } finally {
        clearTimeout(timer);
      }
      assert.equal(await retry.isDisabled(), true);
      await page.evaluate(async () => {
        const retry = document.getElementById('auth-restore-retry');
        retry.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        retry.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        await new Promise(requestAnimationFrame);
        await new Promise(requestAnimationFrame);
      });
      assert.equal(
        await page.evaluate(() => window.__restoreEvidence.refreshes),
        2,
        'pending Retry cannot submit another refresh',
      );
      assert.equal(refreshes, 2);
      await pending.fulfill(
        result === 'restored'
          ? { json: accountSession('First', ++revision) }
          : { status: 401, json: { error: 'No session' } },
      );
      await page.locator('#auth-restore-notice').waitFor({ state: 'hidden' });
      if (result === 'restored') {
        await page.waitForFunction(
          () =>
            document.getElementById('auth-display-name').textContent === 'First' &&
            window.__restoreEvidence.authenticatedOpens === 1,
        );
        assert.equal(await page.locator('#sign-in-btn').isVisible(), false);
      } else {
        assert.equal(await page.locator('#sign-in-btn').isVisible(), true);
        assert.equal(await page.locator('#auth-bar-user').isVisible(), false);
      }
      assert.equal(logins, 0, 'saved sign-in recovery requires no entered credentials');
    }
    const evidence = await page.evaluate(() => window.__restoreEvidence);
    assert.equal(evidence.captures, 0);
    assert.equal(evidence.joins, 0);
    assert.equal(evidence.refreshes, result === 'superseded' ? 1 : 2);
    assert.equal(evidence.releasedBodies, result === 'superseded' ? 1 : 0);
    assert.equal(
      evidence.documentMarker,
      documentMarker,
      'saved sign-in recovery never replaces the document',
    );
    assert.deepEqual(failures, []);
  } finally {
    await context.close();
  }
}

async function scenario(browser, storageOnly) {
  const context = await browser.newContext();
  const failures = [];
  let revision = 0;
  const session = (name) => accountSession(name, ++revision);
  try {
    await context.addInitScript(
      ({ storageOnly, markerKey }) => {
        window.__crossTabEvidence = {
          connections: 0,
          authenticatedOpens: 0,
          closes: 0,
          renewals: 0,
          captures: 0,
          joins: 0,
          hints: [],
        };
        if (storageOnly) window.BroadcastChannel = undefined;
        else {
          const native = window.BroadcastChannel;
          window.BroadcastChannel = new Proxy(native, {
            construct(target, args) {
              const channel = Reflect.construct(target, args);
              if (args[0] === markerKey) {
                const send = channel.postMessage.bind(channel);
                channel.postMessage = (value) => {
                  window.__crossTabEvidence.hints.push(value);
                  send(value);
                };
              }
              return channel;
            },
          });
        }
        class FixtureSocket {
          static OPEN = 1;
          static CONNECTING = 0;
          readyState = 0;
          constructor(_url, protocols) {
            window.__crossTabEvidence.connections++;
            setTimeout(() => {
              if (this.readyState !== FixtureSocket.CONNECTING) return;
              this.readyState = 1;
              this.onopen?.();
              if (
                this.readyState === FixtureSocket.OPEN &&
                protocols.some((protocol) => protocol.startsWith('ticket.'))
              )
                window.__crossTabEvidence.authenticatedOpens++;
            }, 0);
          }
          close() {
            this.readyState = 3;
            window.__crossTabEvidence.closes++;
          }
          send(raw) {
            const request = JSON.parse(raw);
            if (request.type === 'joinRoom') window.__crossTabEvidence.joins++;
            if (request.type === 'renewAuthentication') {
              window.__crossTabEvidence.renewals++;
              setTimeout(
                () =>
                  this.onmessage?.({
                    data: JSON.stringify({
                      type: 'authenticationRenewed',
                      requestId: request.requestId,
                      expiresAt: Math.floor(Date.now() / 1000) + 900,
                    }),
                  }),
                0,
              );
            }
          }
        }
        window.WebSocket = FixtureSocket;
        navigator.mediaDevices.getUserMedia = () => {
          window.__crossTabEvidence.captures++;
          return Promise.reject(
            new Error('No capture is permitted in account synchronization checks'),
          );
        };
      },
      { storageOnly, markerKey },
    );
    await context.route('**/api/**', async (route) => {
      const request = route.request();
      const pathname = new URL(request.url()).pathname;
      if (pathname === '/api/capabilities') {
        await route.fulfill({ json: capabilities });
      } else if (pathname === '/api/auth/ws-ticket') {
        assert.equal(request.method(), 'POST');
        assert.deepEqual(request.postDataJSON(), {});
        await route.fulfill({ json: { ticket: 'a'.repeat(43), expires_in: 30 } });
      } else if (pathname === '/api/auth/login') {
        const name = request.postDataJSON().email.split('@')[0];
        assert.equal(name, 'First');
        await route.fulfill({
          headers: { 'Set-Cookie': `fixture_refresh=${name}; HttpOnly; SameSite=Strict; Path=/` },
          json: session(name),
        });
      } else if (pathname === '/api/auth/refresh') {
        const name = /(?:^|;\s*)fixture_refresh=(First|Second)(?:;|$)/.exec(
          request.headers().cookie || '',
        )?.[1];
        await route.fulfill(
          name ? { json: session(name) } : { status: 401, json: { error: 'No session' } },
        );
      } else if (pathname === '/api/auth/logout') {
        await route.fulfill({
          status: 204,
          headers: {
            'Set-Cookie': 'fixture_refresh=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0',
          },
        });
      } else if (pathname === '/api/auth/preferences') {
        assert.equal(request.method(), 'GET');
        assert.match(request.headers().authorization, /^Bearer /);
        await route.fulfill({
          json: {
            allowPrivateMessages: true,
            sounds: false,
            largeText: false,
            timestamps: 'time',
            ignored: [],
          },
        });
      } else if (pathname === '/api/telemetry') await route.fulfill({ status: 204 });
      else await route.fulfill({ json: [] });
    });
    const first = await context.newPage();
    const second = await context.newPage();
    for (const page of [first, second]) {
      page.setDefaultTimeout(10000);
      page.on('pageerror', (error) => failures.push(error.message));
      await page.goto(`${origin.toString()}${page === first ? '#room_%41-2' : ''}`);
      await page.locator('#sign-in-btn').waitFor({ state: 'visible' });
    }
    const selected = async (value) =>
      first.waitForFunction((room) => document.getElementById('room-input').value === room, value);
    await selected('room_A-2');
    await first.evaluate(() => {
      location.hash = 'room-b';
    });
    await selected('room-b');
    await first.locator('#home-link').click();
    await selected('');
    assert.equal(new URL(first.url()).hash, '');
    await first.goBack();
    await selected('room-b');
    await first.goBack();
    await selected('room_A-2');
    await first.goForward();
    await selected('room-b');
    await first.evaluate(() => {
      location.hash = 'room-c';
      location.hash = 'room-d';
    });
    await selected('room-d');
    await first.evaluate(() => {
      location.hash = '%';
    });
    await first.waitForFunction(() => location.hash === '#room-d');
    await selected('room-d');
    await first.locator('#sign-in-btn').click();
    await first.locator('#login-email').fill('First@example.test');
    await first.locator('#login-password').fill('Disposable-test-password');
    await first.locator('#login-submit').click();
    for (const page of [first, second])
      await page.waitForFunction(
        () => document.getElementById('auth-display-name').textContent === 'First',
      );
    // Account UI updates before the asynchronous ticket handshake completes.
    // Measure renewal only after the initial authenticated connection is open.
    for (const page of [first, second])
      await page.waitForFunction(() => window.__crossTabEvidence.authenticatedOpens === 1);
    const before = await second.evaluate(() => ({ ...window.__crossTabEvidence }));
    const announce = async () =>
      first.evaluate(
        ({ markerKey, storageOnly }) => {
          const marker = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
            byte.toString(16).padStart(2, '0'),
          ).join('');
          localStorage.setItem(markerKey, marker);
          if (!storageOnly) {
            const channel = new BroadcastChannel(markerKey);
            channel.postMessage({ version: 1, revision: marker });
            channel.close();
          }
        },
        { markerKey, storageOnly },
      );
    await announce();
    await second.waitForFunction(
      (count) => window.__crossTabEvidence.renewals > count,
      before.renewals,
    );
    const same = await second.evaluate(() => ({ ...window.__crossTabEvidence }));
    assert.equal(
      same.connections,
      before.connections,
      'same account keeps the signaling connection',
    );
    assert.equal(same.closes, before.closes);
    await context.addCookies([
      {
        name: 'fixture_refresh',
        value: 'Second',
        url: origin.origin,
        httpOnly: true,
        sameSite: 'Strict',
      },
    ]);
    await announce();
    await second.waitForFunction(
      () => document.getElementById('auth-display-name').textContent === 'Second',
    );
    await second.waitForFunction(
      (count) => window.__crossTabEvidence.connections > count,
      same.connections,
    );
    const switched = await second.evaluate(() => ({ ...window.__crossTabEvidence }));
    assert.ok(switched.closes > same.closes, 'a changed account retires the previous socket');
    await first.bringToFront();
    await first.evaluate(() => window.dispatchEvent(new Event('focus')));
    await first.waitForFunction(
      () => document.getElementById('auth-display-name').textContent === 'Second',
    );
    await signOut(first);
    await second.locator('#sign-in-btn').waitFor({ state: 'visible' });
    assert.equal(
      (await context.cookies()).some((cookie) => cookie.name === 'fixture_refresh'),
      false,
    );
    for (const page of [first, second]) {
      const evidence = await page.evaluate(() => window.__crossTabEvidence);
      assert.equal(evidence.captures, 0, 'account changes never start media capture');
      assert.equal(evidence.joins, 0, 'URL/history selection never joins a room automatically');
      for (const hint of evidence.hints) {
        assert.deepEqual(Object.keys(hint).sort(), ['revision', 'version']);
        assert.match(hint.revision, /^[a-f0-9]{32}$/);
        assert.equal(hint.version, 1);
      }
      assert.match(
        await page.evaluate((key) => localStorage.getItem(key), markerKey),
        /^[a-f0-9]{32}$/,
      );
    }
    assert.deepEqual(failures, []);
  } finally {
    await context.close();
  }
}

async function main() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  try {
    await startupRecovery(browser, 'restored');
    await startupRecovery(browser, 'unauthenticated');
    await startupRecovery(browser, 'superseded');
    await scenario(browser, false);
    await scenario(browser, true);
    console.log(
      JSON.stringify({
        browser: options.name,
        passed: true,
        checks: [
          'saved-sign-in-service-error-explicit-retry',
          'saved-sign-in-pending-retry-deduplicated',
          'saved-sign-in-verified-rejection-is-guest',
          'saved-sign-in-stale-body-cannot-replace-login',
          'saved-sign-in-no-reload-or-capture',
          'cross-tab-sign-in-sign-out',
          'same-account-socket-preserved',
          'different-account-socket-retired',
          'storage-fallback',
          'focus-catches-missed-hint',
          'no-identity-in-hints',
          'no-automatic-capture',
          'safe-room-link-selection',
          'back-forward-home-navigation',
          'rapid-latest-room-selection',
        ],
      }),
    );
  } finally {
    await browser.close();
  }
}
main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
