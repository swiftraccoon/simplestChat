/** Keyboard, error and dialog review against production assets; no services, capture or sound. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { productionAssets } = require('./homepage-layout.cjs');
const { capabilities } = require('./capabilities-fixture.cjs');
const { silentUiGuard, silentUiState } = require('./silent-ui-guard.cjs');
const {
  closeOwnedBrowser,
  startFinalizationWatchdog,
  canCancelFinalizationWatchdog,
} = require('./lifecycle-cleanup.cjs');

const origin = 'http://127.0.0.1:39881';
const viewports = [
  { width: 1440, height: 900 },
  { width: 320, height: 568 },
  { width: 844, height: 390 },
];

async function run() {
  const browserName = process.env.E2E_BROWSER || 'chromium';
  assert.ok(['chromium', 'firefox'].includes(browserName), 'Unsupported silent UI browser');
  assert.ok(process.env.E2E_ARTIFACTS, 'Set E2E_ARTIFACTS to a new owned directory');
  const artifacts = path.resolve(process.env.E2E_ARTIFACTS);
  fs.mkdirSync(artifacts, { mode: 0o700 });
  const assets = productionAssets();
  const report = {
    browserName,
    scope: 'Production UI with isolated HTTP/WS fixtures and capture/playback APIs blocked.',
    limitations:
      'Desktop keyboard policy and resized viewports do not establish physical-device behavior.',
    assets: [...assets].map(([url, asset]) => ({ url, sha256: asset.sha256 })),
    checks: [],
    findings: [],
    pageErrors: 0,
    unexpectedRequests: 0,
    signalingMessages: 0,
    guardedDocuments: 0,
    blockedApiAttempts: 0,
    passed: false,
  };
  const save = () =>
    fs.writeFileSync(path.join(artifacts, 'results.json'), JSON.stringify(report, null, 2) + '\n');
  const playwright = require('playwright');
  let server;
  let activePage;
  let deadline;
  let failed;
  const record = (name) => report.checks.push(name);
  const focused = async (page, selector) =>
    assert.equal(
      await page.locator(selector).evaluate((node) => node === document.activeElement),
      true,
    );
  const activate = async (page, selector) => {
    await page.locator(selector).waitFor({ state: 'visible' });
    await page.locator(selector).focus();
    await focused(page, selector);
    await page.keyboard.press('Enter');
  };
  const visible = async (locator, name) => {
    await locator
      .page()
      .waitForFunction(() =>
        document.getAnimations().every((animation) => animation.playState !== 'running'),
      );
    const state = await locator.evaluate((node) => {
      const rect = node.getBoundingClientRect();
      let left = Math.max(0, rect.left);
      let top = Math.max(0, rect.top);
      let right = Math.min(innerWidth, rect.right);
      let bottom = Math.min(innerHeight, rect.bottom);
      for (let parent = node.parentElement; parent; parent = parent.parentElement) {
        if (!/(auto|scroll|hidden|clip)/.test(getComputedStyle(parent).overflow)) continue;
        const clip = parent.getBoundingClientRect();
        left = Math.max(left, clip.left);
        top = Math.max(top, clip.top);
        right = Math.min(right, clip.right);
        bottom = Math.min(bottom, clip.bottom);
      }
      const hit =
        right > left && bottom > top
          ? document.elementFromPoint((left + right) / 2, (top + bottom) / 2)
          : null;
      return {
        visible: hit !== null && node.contains(hit),
        horizontallyContained: rect.left >= 0 && rect.right <= document.documentElement.clientWidth,
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    });
    record({ name, ...state });
    if (!state.visible || !state.horizontallyContained) report.findings.push({ name, ...state });
  };
  const screenshot = async (page, name) =>
    page.screenshot({
      path: path.join(artifacts, `${name}.png`),
      fullPage: true,
      animations: 'disabled',
    });

  try {
    server = await playwright[browserName].launchServer({ headless: true, timeout: 15000 });
    const browser = await playwright[browserName].connect(server.wsEndpoint());
    report.browserVersion = browser.version();
    const scenarios = async () => {
      for (const signedIn of [false, true]) {
        for (const viewport of viewports) {
          const name = `${signedIn ? 'account' : 'guest'}-${viewport.width}x${viewport.height}`;
          report.activeStep = name;
          const context = await browser.newContext({
            viewport,
            permissions: [],
            serviceWorkers: 'block',
          });
          await context.addInitScript(silentUiGuard);
          let login;
          const loginSeen = Promise.withResolvers();
          const profile = {
            id: 'silent-owner',
            email: 'owner@example.test',
            display_name: 'Quiet UI review',
            avatar_url: null,
            bio: '',
            profile_style: { color: null, style: 'accent' },
            recovery_enabled: false,
          };
          await context.routeWebSocket(/.*/, (socket) => {
            assert.equal(socket.url().split('?')[0], origin.replace('http:', 'ws:') + '/ws');
            socket.onMessage(() => report.signalingMessages++);
          });
          await context.route('**/*', (route) => {
            const request = route.request();
            const url = new URL(request.url());
            const json = (body, status = 200) =>
              route.fulfill({
                status,
                contentType: 'application/json',
                body: JSON.stringify(body),
              });
            if (url.origin === origin) {
              const asset = assets.get(url.pathname);
              if (asset && request.method() === 'GET')
                return route.fulfill({
                  status: 200,
                  contentType: asset.contentType,
                  body: asset.body,
                  headers: {
                    'Permissions-Policy':
                      'camera=(), microphone=(), display-capture=(), autoplay=()',
                  },
                });
              if (url.pathname === '/favicon.ico') return route.fulfill({ status: 204 });
              if (url.pathname === '/api/capabilities') return json(capabilities);
              if (url.pathname === '/api/auth/refresh')
                return json(
                  signedIn
                    ? { token: 'owned-silent-fixture', user: profile }
                    : { error: 'No saved session' },
                  signedIn ? 200 : 401,
                );
              if (url.pathname === '/api/auth/ws-ticket')
                return json({ ticket: 'a'.repeat(43), expires_in: 30 });
              if (url.pathname === '/api/auth/login' && request.method() === 'POST') {
                login = route;
                loginSeen.resolve();
                return;
              }
              if (url.pathname === '/api/rooms') return json([]);
              if (url.pathname === '/api/auth/saved-rooms') return json({ rooms: [] });
              if (url.pathname === '/api/auth/inbox/unread') return json({ unreadCount: 0 });
              if (url.pathname === '/api/auth/preferences') {
                assert.equal(request.method(), 'GET');
                assert.match(request.headers().authorization, /^Bearer /);
                return json({
                  allowPrivateMessages: true,
                  sounds: false,
                  largeText: false,
                  timestamps: 'time',
                  ignored: [],
                });
              }
              if (url.pathname === '/api/auth/notification-preferences')
                return json({
                  privateMessages: false,
                  mentions: false,
                  quietHours: null,
                  conversations: [],
                });
              if (
                url.pathname === '/api/auth/profile' ||
                url.pathname.startsWith('/api/auth/profiles/')
              )
                return json(profile);
              if (url.pathname === '/api/rooms/mine')
                return json({ error: 'Rooms could not be loaded. Please try again later.' }, 503);
            }
            report.unexpectedRequests++;
            return route.abort('blockedbyclient');
          });
          const page = await context.newPage();
          activePage = page;
          page.setDefaultTimeout(5000);
          page.on('pageerror', () => report.pageErrors++);
          await page.goto(origin, { waitUntil: 'load' });
          await page.locator('#connection-status.connected').waitFor();
          await page.locator(signedIn ? '#auth-bar-user' : '#sign-in-btn').waitFor();
          if (!signedIn) {
            await activate(page, '#sign-in-btn');
            await focused(page, '#login-email');
            await page.locator('#login-email').fill('review@example.test');
            await page.keyboard.press('Enter');
            await focused(page, '#login-password');
            await page.locator('#login-password').fill('disposable-fixture-password');
            await page.keyboard.press('Enter');
            await loginSeen.promise;
            await page.keyboard.press('Escape');
            assert.equal(await page.locator('#login-modal').isVisible(), true);
            assert.equal(await page.locator('#login-close').isDisabled(), true);
            await login.fulfill({
              status: 401,
              contentType: 'application/json',
              body: JSON.stringify({ error: 'Email or password incorrect. Please try again.' }),
            });
            await page.locator('#login-error').waitFor();
            await visible(page.locator('#login-error'), `${name}-password-error-visible`);
            await focused(page, '#login-password');
            assert.equal(await page.locator('#login-error').getAttribute('role'), 'alert');
            assert.equal(await page.locator('#login-password').inputValue(), '');
            assert.equal(await page.locator('#login-submit').isEnabled(), true);
            await screenshot(page, `${name}-password-error`);
            await page.keyboard.press('Escape');
            await focused(page, '#sign-in-btn');
            record(
              `${name}: keyboard password flow, pending-dialog ownership and decisive failure recovery`,
            );
            await activate(page, '#sign-in-btn');
            await activate(page, '#login-to-register');
            await focused(page, '#register-email');
            await page.locator('#register-email').fill('review@example.test');
            await page.locator('#register-name').fill('Quiet UI review');
            await page.locator('#register-password').fill('short');
            await page.locator('#register-confirm').fill('short');
            await activate(page, '#register-submit');
            await page.locator('#register-error').waitFor();
            await visible(page.locator('#register-error'), `${name}-registration-error-visible`);
            await focused(page, '#register-submit');
            assert.equal(await page.locator('#register-error').getAttribute('role'), 'alert');
            await screenshot(page, `${name}-registration-error`);
            await page.locator('#register-close').scrollIntoViewIfNeeded();
            await visible(page.locator('#register-close'), `${name}-registration-close-reachable`);
            await page.keyboard.press('Escape');
            await focused(page, '#sign-in-btn');
            record(`${name}: registration validation and short-window close reachability`);
          } else {
            await activate(page, '#account-menu-btn');
            await focused(page, '#community-actions button:first-child');
            await page.keyboard.press('Enter');
            await page.getByRole('region', { name: 'Settings', exact: true }).waitFor();
            await page.getByRole('button', { name: 'Save profile', exact: true }).waitFor();
            await focused(page, '#settings-sheet-title');
            const close = page.getByRole('button', { name: 'Close settings', exact: true });
            await close.scrollIntoViewIfNeeded();
            await visible(close, `${name}-settings-close-reachable`);
            await screenshot(page, `${name}-profile`);
            await page.locator('#settings-sheet-title').focus();
            await page.keyboard.press('Escape');
            await page.keyboard.press('Escape');
            await focused(page, '#account-menu-btn');
            await activate(page, '#account-menu-btn');
            await activate(page, '#community-actions button:last-child');
            const dialog = page.getByRole('dialog', { name: 'My rooms', exact: true });
            await dialog.locator('[role="alert"]').waitFor();
            await visible(dialog.locator('[role="alert"]'), `${name}-rooms-error-visible`);
            await screenshot(page, `${name}-rooms-error`);
            await page.keyboard.press('Escape');
            await focused(page, '#account-menu-btn');
            record(
              `${name}: account menu keyboard activation, sheet navigation and modal error focus return`,
            );
          }
          const state = await silentUiState(page);
          report.guardedDocuments++;
          report.blockedApiAttempts += state.attempts;
          assert.equal(
            await page.locator('audio, video').count(),
            0,
            'No media elements in silent UI states',
          );
          assert.equal(
            await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
            true,
          );
          await context.close();
          activePage = null;
          save();
        }
      }
      assert.equal(report.pageErrors, 0);
      assert.equal(report.unexpectedRequests, 0);
      assert.equal(report.signalingMessages, 0, 'Silent UI review never joins or publishes');
      assert.deepEqual(
        report.findings,
        [],
        'Every validation failure must be visible without hunting through a scrolled dialog',
      );
    };
    await Promise.race([
      scenarios(),
      new Promise((_, reject) => {
        deadline = setTimeout(() => reject(new Error('Silent UI deadline exceeded')), 60000);
      }),
    ]);
    report.complete = true;
  } catch (error) {
    failed = error;
    report.failedStep = report.activeStep;
    report.failure = error.message;
    if (activePage) await screenshot(activePage, 'failure').catch(() => {});
  } finally {
    clearTimeout(deadline);
    const cancel = startFinalizationWatchdog(report, save, () => {
      server?.process().kill('SIGKILL');
      process.exit(1);
    });
    try {
      if (server) await closeOwnedBrowser(server, report, save);
    } catch (error) {
      failed = error;
    }
    report.passed = !failed && report.complete === true && report.browserCleanup?.passed === true;
    save();
    if (canCancelFinalizationWatchdog(server, report)) cancel();
  }
  assert.equal(report.passed, true, `Silent UI failed; inspect ${artifacts}`);
  console.log(`PASS silent UI: ${report.checks.length} checks; ${artifacts}`);
}

if (require.main === module)
  run().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
