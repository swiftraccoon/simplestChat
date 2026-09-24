/** Owned local frontend only. HTTP/credential fixtures exercise real browser DOM
 * dismissal/cookie behavior; this does not certify native passkey-manager UX. */
const assert = require('node:assert/strict');
const { browserOptions } = require('./browser-options.cjs');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:38179');
if (
  process.env.AUTH_LIFECYCLE_E2E !== '1' ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error(
    'Set AUTH_LIFECYCLE_E2E=1 with a credential-free owned loopback frontend origin.',
  );

const options = browserOptions(process.env.E2E_BROWSER);

async function run() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  const context = await browser.newContext();
  const failures = [];
  let pendingLogin;
  let loginObserved;
  let logins = 0;
  let passkeyFinishes = 0;
  let passkeyStarts = 0;
  let uploads = 0;
  try {
    const page = await context.newPage();
    page.on('pageerror', (error) => failures.push(error.message));
    page.setDefaultTimeout(5000);
    await page.addInitScript(() => {
      window.joinRequestsFixture = [];
      class FixtureSocket {
        static OPEN = 1;
        static CONNECTING = 0;
        readyState = 0;
        constructor() {
          setTimeout(() => {
            this.readyState = 1;
            this.onopen?.();
          }, 0);
        }
        close() {
          this.readyState = 3;
        }
        send(raw) {
          const request = JSON.parse(raw);
          if (request.type === 'joinRoom') {
            window.joinRequestsFixture.push(request);
            setTimeout(
              () => this.onmessage?.({ data: JSON.stringify({ type: 'roomPasswordRequired' }) }),
              0,
            );
          }
        }
      }
      window.WebSocket = FixtureSocket;
      Object.defineProperty(navigator.credentials, 'get', {
        value: (request) =>
          new Promise((resolve) => {
            window.ceremonyOptionsFixture = request;
            window.finishCeremonyFixture = resolve;
          }),
      });
    });
    await context.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/api/auth/login') {
        logins++;
        pendingLogin = route;
        loginObserved?.();
        return;
      }
      if (path === '/api/auth/passkey/login/start') {
        passkeyStarts++;
        assert.deepEqual(route.request().postDataJSON(), {}, 'passkey start has no email');
        await route.fulfill({
          json: {
            ceremony_id: 'fixture',
            mediation: 'required',
            publicKey: {
              challenge: 'AQI',
              rpId: 'localhost',
              allowCredentials: [],
              userVerification: 'required',
            },
          },
        });
        return;
      }
      if (path.includes('/passkey/login/finish')) passkeyFinishes++;
      if (path === '/api/telemetry') uploads++;
      if (path === '/api/auth/refresh')
        await route.fulfill({ status: 401, json: { error: 'No session' } });
      else if (path === '/api/auth/logout' || path === '/api/telemetry')
        await route.fulfill({ status: 204 });
      else await route.fulfill({ json: [] });
    });
    const submitLogin = async () => {
      const expected = logins + 1;
      let timer;
      const submitted = new Promise((resolve, reject) => {
        loginObserved = resolve;
        timer = setTimeout(() => reject(new Error('Login did not reach the HTTP fixture')), 5000);
      });
      try {
        // Disabled controls precede Web Lock acquisition. Observe submission
        // before testing cookie effects or advancing the request deadline.
        await Promise.all([page.locator('#login-submit').click(), submitted]);
        assert.equal(logins, expected);
      } finally {
        clearTimeout(timer);
        loginObserved = undefined;
      }
    };
    await page.goto(origin.toString());
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-email').fill('fixture@example.test');
    await page.locator('#login-password').fill('Disposable-test-password');
    await submitLogin();
    await page.waitForFunction(() => document.getElementById('login-close').disabled);
    assert.equal(logins, 1);
    for (const action of ['close', 'switch', 'backdrop', 'escape']) {
      if (action === 'escape') await page.keyboard.press('Escape');
      else
        await page.evaluate((route) => {
          if (route === 'backdrop') document.getElementById('login-modal').click();
          else
            document
              .getElementById(route === 'close' ? 'login-close' : 'login-to-register')
              .click();
        }, action);
      assert.equal(await page.locator('#login-modal').isVisible(), true, action);
      assert.equal(await page.locator('#register-modal').isVisible(), false, action);
      assert.equal(
        (await context.cookies()).some((cookie) => cookie.name === 'fixture_refresh'),
        false,
      );
    }
    const payload = Buffer.from(
      JSON.stringify({ exp: Math.floor(Date.now() / 1000) + 900 }),
    ).toString('base64url');
    await pendingLogin.fulfill({
      headers: { 'Set-Cookie': 'fixture_refresh=accepted; HttpOnly; SameSite=Strict; Path=/' },
      json: {
        token: `fixture.${payload}.signature`,
        user: { id: 'fixture-user', email: 'fixture@example.test', display_name: 'Fixture' },
      },
    });
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    assert.equal(await page.locator('#login-password').inputValue(), '');
    assert.equal(
      (await context.cookies()).find((cookie) => cookie.name === 'fixture_refresh').httpOnly,
      true,
    );
    await page.locator('#logout-btn').click();
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-email').fill('');
    await page.locator('#login-passkey-btn').click();
    await page.waitForFunction(() => typeof window.finishCeremonyFixture === 'function');
    assert.equal(passkeyStarts, 1, 'passkey sign-in requires no email');
    assert.equal(await page.evaluate(() => window.ceremonyOptionsFixture.mediation), 'required');
    assert.deepEqual(
      await page.evaluate(() => window.ceremonyOptionsFixture.publicKey.allowCredentials),
      [],
    );
    await page.keyboard.press('Escape');
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-password').fill('New-test-password');
    await submitLogin();
    await page.waitForFunction(() => document.getElementById('login-close').disabled);
    await page.evaluate(() => window.finishCeremonyFixture({ id: 'retired-fixture' }));
    await page.waitForTimeout(50);
    assert.equal(passkeyFinishes, 0, 'retired ceremony cannot create a session');
    assert.equal(
      await page.locator('#login-close').isDisabled(),
      true,
      'old completion cannot unlock a newer request',
    );
    await pendingLogin.fulfill({ status: 401, json: { error: 'Invalid credentials' } });
    await page.locator('#login-close').click();
    assert.equal(await page.locator('#login-password').inputValue(), '');
    await page.locator('#name-input').fill('Fixture');
    await page.locator('#room-input').fill('fixture-room');
    await page.locator('#join-btn').click();
    const password = page.locator('#join-room-password');
    await password.waitFor();
    assert.equal(await password.getAttribute('type'), 'password');
    assert.equal(await password.getAttribute('autocomplete'), 'current-password');
    await password.fill('Masked-test-password');
    await page.evaluate(() => {
      window.retiredPasswordFormFixture = document
        .getElementById('join-room-password')
        .closest('form');
      document.getElementById('home-link').click();
    });
    await password.waitFor({ state: 'detached' });
    await page.locator('#room-input').fill('replacement-room');
    await page.locator('#join-btn').click();
    await password.waitFor();
    await password.fill('Current-test-password');
    await page.evaluate(() =>
      window.retiredPasswordFormFixture.dispatchEvent(
        new Event('submit', { bubbles: true, cancelable: true }),
      ),
    );
    assert.equal(
      await password.inputValue(),
      'Current-test-password',
      'retired prompt cannot touch replacement',
    );
    assert.equal(
      await page.evaluate(() => window.joinRequestsFixture.length),
      2,
      'retired password cannot join another room',
    );
    await page.keyboard.press('Escape');
    await password.waitFor({ state: 'detached' });
    await page.setViewportSize({ width: 320, height: 720 });
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      true,
      'diagnostics navigation fits narrow screens',
    );
    await page.locator('#diagnostics-btn').click();
    const preview = page.getByLabel('Diagnostic summary preview');
    const summary = await preview.inputValue();
    const originalSummary = JSON.parse(summary);
    assert.equal(originalSummary.version, 2);
    assert.ok(Array.isArray(originalSummary.events));
    assert.ok(Array.isArray(originalSummary.mediaSamples));
    assert.equal(summary.includes('fixture@example.test'), false);
    assert.equal(summary.includes('Masked-test-password'), false);
    assert.equal(summary.includes('signature'), false);
    // A diagnostic arriving while the dialog is open must not silently change
    // the reviewed copy. Refresh is an explicit gesture and requires no consent.
    await page.evaluate(() => window.dispatchEvent(new Event('error')));
    assert.equal(await preview.inputValue(), summary);
    await page.getByRole('button', { name: 'Refresh preview', exact: true }).click();
    const refreshedSummary = JSON.parse(await preview.inputValue());
    assert.equal(refreshedSummary.localReportReference, originalSummary.localReportReference);
    assert.equal(
      refreshedSummary.events.filter((event) => event.name === 'js_error').length,
      originalSummary.events.filter((event) => event.name === 'js_error').length + 1,
    );
    assert.equal(uploads, 0, 'uploads remain disabled without consent');
    await page.getByRole('button', { name: 'Close', exact: true }).click();
    await page.clock.install();
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-email').fill('fixture@example.test');
    await page.locator('#login-password').fill('Unconfirmed-test-password');
    await submitLogin();
    await page.waitForFunction(() => document.getElementById('login-close').disabled);
    const uncertainCount = logins;
    await page.clock.runFor(20000);
    await page.getByRole('button', { name: 'Reload and check session', exact: true }).waitFor();
    assert.equal(await page.locator('#login-password').inputValue(), '');
    assert.equal(await page.locator('#login-modal').isVisible(), true);
    assert.equal(await page.locator('#login-close').isDisabled(), true);
    assert.equal(await page.locator('#login-submit').isDisabled(), true);
    await page.keyboard.press('Escape');
    await page.evaluate(() => {
      document.getElementById('login-modal').click();
      document.getElementById('login-to-register').click();
      document.getElementById('login-submit').click();
    });
    assert.equal(await page.locator('#login-modal').isVisible(), true);
    assert.equal(logins, uncertainCount, 'uncertain outcome cannot start another request');
    assert.match(await page.locator('#login-error').textContent(), /may have signed you in/);

    assert.deepEqual(failures, []);
    process.stdout.write(
      JSON.stringify({
        browser: options.name,
        checks: [
          'session-dismissal-cookie-boundary',
          'usernameless-passkey-options',
          'late-ceremony-ownership',
          'masked-password-teardown',
          'password-prompt-membership-ownership',
          'diagnostic-privacy',
          'bounded-uncertain-session-reload',
        ],
        passed: true,
      }) + '\n',
    );
  } finally {
    await context.close();
    await browser.close();
  }
}
run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
