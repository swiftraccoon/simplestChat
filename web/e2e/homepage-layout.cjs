/** Production homepage layout, with intercepted fixtures and no live services. */
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  closeOwnedBrowser,
  startFinalizationWatchdog,
  canCancelFinalizationWatchdog,
} = require('./lifecycle-cleanup.cjs');

const origin = 'http://homepage.simplestchat.test';
const viewports = [
  { width: 320, height: 568 },
  { width: 375, height: 667 },
  { width: 768, height: 1024 },
  { width: 1440, height: 900 },
  { width: 667, height: 320 },
];
const thumbnail =
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=';
const rooms = Array.from({ length: 21 }, (_, index) => ({
  id: `layout-room-${index}`,
  display_name: index % 2 ? 'A welcoming room with a lengthy descriptive name' : 'Room'.repeat(24),
  topic: 'A topic that must remain inside its room card '.repeat(4),
  participant_count: 9999,
  password_protected: index % 2 === 0,
  moderated: true,
  broadcaster_count: 999,
  description: 'Description'.repeat(24),
  image_url: index % 3 ? null : thumbnail,
  secret: false,
}));

function productionAssets() {
  const directory = path.resolve(__dirname, '../dist');
  const assets = new Map();
  const add = (url, filename, contentType) => {
    const body = fs.readFileSync(filename);
    assets.set(url, {
      body,
      contentType,
      sha256: crypto.createHash('sha256').update(body).digest('hex'),
    });
  };
  add('/', path.join(directory, 'index.html'), 'text/html');
  for (const entry of fs.readdirSync(path.join(directory, 'assets'), { withFileTypes: true })) {
    if (!entry.isFile() || !/^[A-Za-z0-9_-]+\.(js|css)$/.test(entry.name)) continue;
    add(
      `/assets/${entry.name}`,
      path.join(directory, 'assets', entry.name),
      entry.name.endsWith('.css') ? 'text/css' : 'text/javascript',
    );
  }
  assert.ok(assets.size >= 3, 'Build the production UI before running layout checks');
  return assets;
}

/** Compare real rendered boxes; clipping the document alone cannot pass this gate. */
async function geometry(page) {
  return page.evaluate(() => {
    const problems = [];
    const problem = (message) => {
      if (problems.length < 40) problems.push(message);
    };
    const box = (node) => node.getBoundingClientRect();
    const inside = (child, parent, label) => {
      if (child.left < parent.left - 1 || child.right > parent.right + 1)
        problem(`${label}: horizontal containment`);
    };
    const card = document.querySelector('.join-card');
    const bounds = box(card);
    const style = getComputedStyle(card);
    const content = {
      left: bounds.left + parseFloat(style.borderLeftWidth) + parseFloat(style.paddingLeft),
      right: bounds.right - parseFloat(style.borderRightWidth) - parseFloat(style.paddingRight),
    };
    if (document.documentElement.scrollWidth > innerWidth + 1)
      problem('document: horizontal overflow');
    inside(bounds, { left: 0, right: innerWidth }, 'join card');
    for (const selector of [
      '#room-browser',
      '#room-search-input',
      '#room-list',
      '.room-card',
      '.room-list-empty',
      '.auth-bar',
      '#auth-display-name',
      '#create-room-btn',
      '.join-form',
      '#name-input',
      '#room-input',
      '#join-btn',
    ]) {
      for (const node of document.querySelectorAll(selector)) {
        if (!node.getClientRects().length) continue;
        inside(box(node), content, selector);
        // Single-line fields intentionally scroll long values inside their
        // contained border box; that is not layout overflow.
        if (
          !(node instanceof HTMLInputElement) &&
          node.clientWidth &&
          node.scrollWidth > node.clientWidth + 1
        )
          problem(`${selector}: internal horizontal overflow`);
      }
    }
    const intersects = (first, second) =>
      Math.min(first.right, second.right) - Math.max(first.left, second.left) > 1 &&
      Math.min(first.bottom, second.bottom) - Math.max(first.top, second.top) > 1;
    for (const room of document.querySelectorAll('.room-card')) {
      const children = [...room.children];
      for (let index = 0; index < children.length; index++) {
        inside(box(children[index]), box(room), 'room content');
        for (const sibling of children.slice(index + 1))
          if (intersects(box(children[index]), box(sibling))) problem('room content: overlap');
      }
      const name = room.querySelector('.room-card-name');
      for (const child of name.children) inside(box(child), box(name), 'room name');
      const meta = room.querySelector('.room-card-meta');
      for (const child of meta.children) inside(box(child), box(meta), 'room metadata');
    }
    for (const selector of ['#room-search-input', '#name-input', '#room-input']) {
      if (parseFloat(getComputedStyle(document.querySelector(selector)).fontSize) < 16)
        problem(`${selector}: text below 16px`);
    }
    return {
      problems,
      viewport: { width: innerWidth, height: innerHeight },
      card: { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height },
      directoryWidth: box(document.querySelector('#room-browser')).width,
      mediaCalls: window.__homepageMediaCalls,
    };
  });
}

async function reachable(locator) {
  await locator.scrollIntoViewIfNeeded();
  assert.equal(
    await locator.evaluate((node) => {
      const rect = node.getBoundingClientRect();
      const x = rect.x + rect.width / 2;
      const y = rect.y + rect.height / 2;
      const hit = document.elementFromPoint(x, y);
      return (
        x >= 0 && x < innerWidth && y >= 0 && y < innerHeight && hit !== null && node.contains(hit)
      );
    }),
    true,
    'Control must be scroll-reachable and unobscured',
  );
}

async function run(env = process.env) {
  const browserName = env.E2E_BROWSER || 'chromium';
  if (!['chromium', 'firefox', 'webkit'].includes(browserName))
    throw new Error('Unsupported E2E_BROWSER; expected chromium, firefox, or webkit.');
  const assets = productionAssets();
  const artifacts = env.E2E_ARTIFACTS
    ? path.resolve(env.E2E_ARTIFACTS)
    : fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-homepage-layout-'));
  if (env.E2E_ARTIFACTS) fs.mkdirSync(artifacts, { mode: 0o700 });
  fs.chmodSync(artifacts, 0o700);
  const report = {
    startedAt: new Date().toISOString(),
    passed: false,
    browserName,
    scope: `Production UI in isolated ${browserName}; mocked APIs/signaling, no backend or media.`,
    limitations: 'Resized desktop viewports do not establish native mobile-browser behavior.',
    assets: [...assets].map(([url, asset]) => ({ url, sha256: asset.sha256 })),
    checks: [],
    pageErrors: 0,
    unexpectedRequests: 0,
    signalingMessages: 0,
  };
  const save = () =>
    fs.writeFileSync(
      path.join(artifacts, 'homepage-layout-results.json'),
      JSON.stringify(report, null, 2) + '\n',
      {
        mode: 0o600,
      },
    );
  let browserServer;
  let page;
  let deadline;
  let failed = false;
  console.log(`Homepage layout artifacts: ${artifacts}`);
  save();
  try {
    const playwright = require(env.PLAYWRIGHT_MODULE || 'playwright');
    const engine = playwright[browserName];
    browserServer = await engine.launchServer({ headless: true, timeout: 15000 });
    const browser = await engine.connect(browserServer.wsEndpoint(), {
      timeout: 10000,
    });
    report.browserVersion = browser.version();
    const scenarios = async () => {
      for (const viewport of viewports) {
        for (const signedIn of [false, true]) {
          const name = `${signedIn ? 'account' : 'guest'}-${viewport.width}x${viewport.height}`;
          report.activeStep = name;
          const context = await browser.newContext({
            viewport,
            permissions: [],
            serviceWorkers: 'block',
          });
          await context.addInitScript(() => {
            window.__homepageMediaCalls = 0;
            const deny = () => {
              window.__homepageMediaCalls++;
              return Promise.reject(
                new DOMException('Media disabled by layout test', 'NotAllowedError'),
              );
            };
            Object.defineProperty(navigator, 'mediaDevices', {
              value: Object.freeze({
                getUserMedia: deny,
                getDisplayMedia: deny,
                enumerateDevices: deny,
              }),
              configurable: false,
            });
            for (const method of ['getUserMedia', 'webkitGetUserMedia', 'mozGetUserMedia'])
              Object.defineProperty(navigator, method, { value: deny, configurable: false });
          });
          await context.routeWebSocket(/.*/, (socket) => {
            if (socket.url() !== `${origin.replace('http:', 'ws:')}/ws`) {
              report.unexpectedRequests++;
              socket.close();
              return;
            }
            // Deliberately never connectToServer(): this is an in-process mock.
            socket.onMessage(() => {
              report.signalingMessages++;
            });
          });
          let searches = 0;
          await context.route('**/*', async (route) => {
            const request = route.request();
            const url = new URL(request.url());
            const json = (status, body) =>
              route.fulfill({
                status,
                contentType: 'application/json',
                body: JSON.stringify(body),
              });
            if (url.origin === origin) {
              const asset = assets.get(url.pathname);
              if (asset && request.method() === 'GET' && !url.search)
                return route.fulfill({
                  status: 200,
                  contentType: asset.contentType,
                  body: asset.body,
                });
              if (url.pathname === '/favicon.ico' && request.method() === 'GET')
                return route.fulfill({ status: 204 });
              if (url.pathname === '/api/auth/refresh' && request.method() === 'POST')
                return json(
                  signedIn ? 200 : 401,
                  signedIn
                    ? {
                        token: 'owned-layout-fixture',
                        user: {
                          id: 'layout-owner',
                          email: 'layout@example.test',
                          display_name: 'Account'.repeat(12),
                        },
                      }
                    : { error: 'No refresh cookie' },
                );
              if (url.pathname === '/api/rooms' && request.method() === 'GET') {
                searches++;
                const query = url.searchParams.get('q');
                if (query === 'error') return json(503, { error: 'Owned fixture unavailable' });
                if (query === 'empty') return json(200, []);
                if (query === 'featured') return json(200, [rooms[0]]);
                return json(
                  200,
                  url.searchParams.get('page') === '2' ? rooms.slice(20) : rooms.slice(0, 20),
                );
              }
            }
            report.unexpectedRequests++;
            return route.abort('blockedbyclient');
          });
          page = await context.newPage();
          page.setDefaultTimeout(5000);
          page.setDefaultNavigationTimeout(5000);
          page.on('pageerror', () => {
            report.pageErrors++;
          });
          report.activeStep = `${name}-navigation`;
          await page.goto(origin, { waitUntil: 'load' });
          report.activeStep = `${name}-directory-ready`;
          await page.waitForFunction(() => document.querySelectorAll('.room-card').length === 20);
          report.activeStep = `${name}-thumbnails-ready`;
          await page.waitForFunction(() =>
            [...document.images].every((image) => image.complete && image.naturalWidth > 0),
          );
          report.activeStep = `${name}-auth-ready`;
          await page
            .locator(signedIn ? '#auth-bar-user' : '#auth-bar-guest')
            .waitFor({ state: 'visible' });
          assert.equal(await page.locator('#create-room-btn').isVisible(), signedIn);
          const inspect = async (state) => {
            report.activeStep = `${name}-${state}`;
            const result = await geometry(page);
            report.checks.push({ name: report.activeStep, ...result });
            save();
            assert.deepEqual(result.problems, [], `${report.activeStep}: layout containment`);
            assert.equal(result.mediaCalls, 0, 'Homepage must not access media devices');
          };
          await page.screenshot({ path: path.join(artifacts, `${name}.png`), fullPage: true });
          await inspect('populated');
          await reachable(page.locator('.room-card').last());
          await page.locator('#room-load-more').click();
          await page.waitForFunction(() => document.querySelectorAll('.room-card').length === 21);
          await reachable(page.locator('.room-card').last());
          await page.locator('.room-card').last().focus();
          await page.keyboard.press('Enter');
          assert.equal(await page.locator('#room-input').inputValue(), rooms[20].id);
          await page.locator('#name-input').fill('Layout Guest');
          for (const selector of ['#name-input', '#room-input', '#join-btn']) {
            await reachable(page.locator(selector));
            await page.locator(selector).focus();
          }
          const search = page.locator('#room-search-input');
          const beforeMinimum = searches;
          await search.fill('xy');
          await page
            .getByText('Enter at least 3 consecutive letters or numbers', { exact: true })
            .waitFor();
          assert.equal(searches, beforeMinimum, 'Short search must not request the API');
          await inspect('minimum-search');
          await search.fill('empty');
          await page.getByText('No rooms match your search', { exact: true }).waitFor();
          await inspect('empty');
          await search.fill('error');
          await page.locator('.directory-status').waitFor();
          await inspect('unavailable');
          await reachable(page.getByRole('button', { name: 'Retry directory' }));
          await search.fill('featured');
          await page.waitForFunction(() => document.querySelectorAll('.room-card').length === 1);
          await inspect('filtered');
          assert.equal(report.pageErrors, 0, 'No unhandled production UI errors');
          assert.equal(report.unexpectedRequests, 0, 'Every request must use an explicit fixture');
          assert.equal(report.signalingMessages, 0, 'Layout checks must not join or publish');
          await context.close();
          page = null;
          console.log(`PASS homepage ${name}`);
        }
      }
    };
    await Promise.race([
      scenarios(),
      new Promise((_, reject) => {
        deadline = setTimeout(
          () => reject(new Error('Homepage layout work deadline exceeded')),
          120000,
        );
      }),
    ]);
    report.complete = true;
  } catch {
    failed = true;
    report.failedStep = report.activeStep || 'browser startup';
    if (page)
      await page
        .screenshot({ path: path.join(artifacts, 'failure.png'), fullPage: true, timeout: 3000 })
        .catch(() => {});
  } finally {
    clearTimeout(deadline);
    const cancelWatchdog = startFinalizationWatchdog(report, save, () => {
      browserServer?.process().kill('SIGKILL');
      process.exit(1);
    });
    try {
      if (browserServer) await closeOwnedBrowser(browserServer, report, save);
    } catch {
      failed = true;
    }
    report.passed = !failed && report.complete === true && report.browserCleanup?.passed === true;
    report.finishedAt = new Date().toISOString();
    save();
    if (canCancelFinalizationWatchdog(browserServer, report)) cancelWatchdog();
  }
  if (!report.passed)
    throw new Error(
      `Homepage layout failed at ${report.failedStep || 'cleanup'}; inspect ${artifacts}`,
    );
  console.log(`PASS homepage layout: ${report.checks.length} rendered checks; ${artifacts}`);
  return report;
}

module.exports = { run, geometry, productionAssets };
if (require.main === module)
  run().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
