/** Current production UI with isolated HTTP/WS fixtures; no services or capture. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { productionAssets } = require('./homepage-layout.cjs');
const { openRoomMenu } = require('./room-menu.cjs');
const { closeOwnedBrowser } = require('./lifecycle-cleanup.cjs');
const { mediaLayout } = require('./media-layout.cjs');
const playwright = require('playwright');

const origin = 'http://127.0.0.1:39879';
const capabilities = {
  version: 1,
  accounts: true,
  passwordLogin: true,
  passkeyLogin: false,
  passwordRegistration: 'invite',
  passkeyRegistration: 'disabled',
  roomDirectory: true,
  roomCreation: true,
  adHocRooms: true,
};
const settings = {
  id: 'fixture-room',
  displayName: 'Fixture room',
  passwordProtected: false,
  requireRegistration: false,
  allowScreenSharing: true,
  allowChat: true,
  allowVideo: true,
  moderated: false,
  inviteOnly: false,
  secret: false,
  lobbyEnabled: false,
  pushToTalk: false,
  guestsAllowed: true,
  guestsCanBroadcast: true,
};
const person = {
  id: 'remote-fixture',
  name: 'Other person',
  role: 'user',
  authenticated: false,
  producers: [],
};

async function run() {
  const browserName = process.env.E2E_BROWSER || 'chromium';
  assert.ok(['chromium', 'firefox'].includes(browserName), 'Unsupported E2E_BROWSER');
  const artifacts = process.env.E2E_ARTIFACTS
    ? path.resolve(process.env.E2E_ARTIFACTS)
    : fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-frontend-resilience-'));
  fs.mkdirSync(artifacts, { recursive: true });
  const report = {
    scope:
      'Isolated production UI with mocked HTTP/signaling; no backend, native device or mobile-browser claim.',
    checks: [],
    browserName,
    pageErrors: 0,
  };
  const save = () =>
    fs.writeFileSync(path.join(artifacts, 'results.json'), JSON.stringify(report, null, 2));
  const assets = productionAssets();
  report.assets = [...assets].map(([url, asset]) => ({ url, sha256: asset.sha256 }));
  const server = await playwright[browserName].launchServer({ headless: true });
  const browser = await playwright[browserName].connect(server.wsEndpoint());
  let deadline;
  try {
    await Promise.race([
      process.env.E2E_MEDIA_LAYOUT_ONLY === '1' ? mediaScenarios() : scenarios(),
      new Promise((_, reject) => {
        deadline = setTimeout(
          () => reject(new Error('Frontend resilience deadline exceeded')),
          60_000,
        );
      }),
    ]);
    report.complete = true;
  } finally {
    clearTimeout(deadline);
    await closeOwnedBrowser(server, report, save);
    report.passed =
      report.complete === true && report.browserCleanup.passed && report.pageErrors === 0;
    save();
  }
  assert.equal(report.passed, true);
  console.log(`PASS frontend resilience: ${report.checks.length} checks; ${artifacts}`);

  async function fixture({
    signedIn = false,
    storage,
    features = capabilities,
    room = false,
    invite = false,
    holdSocket = false,
    holdInvite = false,
    holdTicket = false,
    holdFeatures = false,
    holdRestore = false,
    clock = false,
    hasTouch = false,
  } = {}) {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      serviceWorkers: 'block',
      hasTouch,
    });
    await context.addInitScript((blocked) => {
      window.__captureRequests = 0;
      if (navigator.mediaDevices)
        for (const name of ['getUserMedia', 'getDisplayMedia'])
          Object.defineProperty(navigator.mediaDevices, name, {
            value: () => {
              window.__captureRequests++;
              return Promise.reject(new DOMException('Disabled by fixture', 'NotAllowedError'));
            },
          });
      if (blocked === 'get')
        Object.defineProperty(window, 'localStorage', {
          get() {
            throw new DOMException('Blocked storage', 'SecurityError');
          },
        });
      if (blocked === 'write')
        Storage.prototype.setItem = () => {
          throw new DOMException('Full storage', 'QuotaExceededError');
        };
    }, storage);
    const sent = [];
    const requests = [];
    const featureRequests = [];
    const featureSeen = Promise.withResolvers();
    const refreshRequests = [];
    const previews = [];
    const socketGate = Promise.withResolvers();
    const socketSeen = Promise.withResolvers();
    const ticketSeen = Promise.withResolvers();
    const tickets = [];
    let connections = 0;
    const inviteSeen = Promise.withResolvers();
    const redemptions = [];
    context.on('close', () => socketGate.resolve());
    let socket;
    const creations = [];
    let account = signedIn
      ? { id: 'fixture-account', email: 'fixture@example.test', display_name: 'Fixture owner' }
      : null;
    await context.routeWebSocket('**/ws', async (owned) => {
      connections++;
      socket = owned;
      const send = (message) => owned.send(JSON.stringify(message));
      owned.onMessage((wire) => {
        const message = JSON.parse(wire);
        sent.push(message);
        if (message.type === 'joinRoom' && room)
          send({
            type: 'roomJoined',
            participantId: 'local-fixture',
            reconnectToken: 'owned-fixture',
            yourRole: 'user',
            participants: [person],
            roomSettings: settings,
          });
        if (message.type === 'chatMessage')
          send({
            type: 'messageAck',
            clientMessageId: message.clientMessageId,
            message: {
              messageId: `fixture-${message.clientMessageId}`,
              clientMessageId: message.clientMessageId,
              participantId: 'local-fixture',
              participantName: 'Fixture guest',
              content: message.content,
              sentAt: new Date().toISOString(),
            },
          });
        if (message.type === 'getRouterRtpCapabilities')
          send({
            type: 'error',
            requestId: message.requestId,
            message: 'Media is disabled in this layout fixture',
          });
        if (message.type === 'setChatPreferences')
          send({
            type: 'socialResponse',
            requestId: message.requestId,
            action: message.type,
            data: { allowPrivateMessages: true, ignoredParticipantIds: [] },
          });
        if (message.type === 'getRoomSnapshot')
          send({
            type: 'socialResponse',
            requestId: message.requestId,
            action: message.type,
            data: {
              participants: [person],
              messages: [],
              yourRole: 'user',
              roomSettings: settings,
              allowPrivateMessages: true,
              ignoredParticipantIds: [],
            },
          });
      });
      socketSeen.resolve();
      // The mocked handshake stays CONNECTING until this handler returns.
      if (holdSocket) await socketGate.promise;
    });
    const confirmInvite = (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          room_id: 'fixture-room',
          display_name: 'Fixture room',
          role: 'user',
        }),
      });
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url());
      requests.push(url.pathname);
      assert.equal(url.origin, origin, 'No external requests');
      const asset = assets.get(url.pathname);
      if (asset)
        return route.fulfill({ status: 200, contentType: asset.contentType, body: asset.body });
      const json = (body, status = 200) =>
        route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
      if (url.pathname === '/favicon.ico') return route.fulfill({ status: 204 });
      if (url.pathname === '/api/capabilities') {
        featureRequests.push(route);
        featureSeen.resolve();
        if (holdFeatures) return;
        return json(features);
      }
      if (url.pathname === '/api/auth/ws-ticket') {
        assert.equal(route.request().method(), 'POST');
        assert.deepEqual(route.request().postDataJSON(), {});
        assert.match(route.request().headers().authorization, /^Bearer /);
        tickets.push(route);
        ticketSeen.resolve();
        if (holdTicket) return;
        return json({ ticket: 'a'.repeat(43), expires_in: 30 });
      }
      if (url.pathname === '/api/auth/profiles/local-fixture')
        return json({
          id: 'local-fixture',
          display_name: 'Fixture owner',
          avatar_url: null,
          bio: '',
        });
      if (url.pathname === '/api/rooms/invites/preview') {
        previews.push(route);
        assert.equal(route.request().method(), 'POST');
        assert.deepEqual(route.request().postDataJSON(), { code: 'a'.repeat(32) });
        return confirmInvite(route);
      }
      if (url.pathname === '/api/rooms/invites/redeem') {
        assert.equal(route.request().method(), 'POST');
        assert.deepEqual(route.request().postDataJSON(), { code: 'a'.repeat(32) });
        redemptions.push(route);
        inviteSeen.resolve();
        if (!holdInvite) return confirmInvite(route);
        return;
      }
      if (url.pathname === '/api/auth/logout') {
        account = null;
        return json({});
      }
      if (url.pathname === '/api/auth/login') {
        account = {
          id: 'replacement-account',
          email: 'replacement@example.test',
          display_name: 'Replacement owner',
        };
        return json({ token: 'replacement-fixture', user: account });
      }
      if (url.pathname === '/api/auth/refresh') {
        refreshRequests.push(route);
        if (holdRestore) return;
        return json(
          account ? { token: 'owned-fixture', user: account } : { error: 'No saved session' },
          account ? 200 : 401,
        );
      }
      if (url.pathname === '/api/rooms' && route.request().method() === 'POST') {
        creations.push(route);
        return;
      }
      if (url.pathname === '/api/rooms') return json([]);
      if (url.pathname === '/api/auth/preferences')
        return json({
          allowPrivateMessages: true,
          sounds: false,
          largeText: false,
          timestamps: 'hover',
          ignored: [],
        });
      throw new Error(`Unexpected fixture path ${url.pathname}`);
    });
    const page = await context.newPage();
    page.setDefaultTimeout(5000);
    if (clock) await page.clock.install();
    page.on('pageerror', () => {
      report.pageErrors++;
    });
    await page.goto(invite ? `${origin}/#invite=${'a'.repeat(32)}` : origin);
    if (holdRestore) await featureSeen.promise;
    else if (holdTicket) await ticketSeen.promise;
    else if (holdSocket) await socketSeen.promise;
    else await page.getByText('Connected', { exact: true }).waitFor();
    return {
      context,
      page,
      sent,
      requests,
      previews,
      featureRequests,
      finishFeatures: (index, body = features, status = 200) =>
        featureRequests[index].fulfill({ status, json: body }),
      finishRestore: () =>
        refreshRequests[0].fulfill({ json: { token: 'owned-fixture', user: account } }),
      redemptions,
      inviteSeen: inviteSeen.promise,
      connections: () => connections,
      finishTicket: (index) =>
        tickets[index].fulfill({ json: { ticket: 'b'.repeat(43), expires_in: 30 } }),
      replaceAccount: async () => {
        account = {
          id: 'replacement-account',
          email: 'replacement@example.test',
          display_name: 'Replacement owner',
        };
        await page.evaluate(() => {
          const channel = new BroadcastChannel('simplestchat-account-change-v1');
          channel.postMessage({ version: 1, revision: 'a'.repeat(32) });
          channel.close();
        });
        await page
          .locator('#auth-display-name')
          .filter({ hasText: 'Replacement owner' })
          .waitFor({ state: 'visible' });
      },
      releaseSocket: () => socketGate.resolve(),
      finishInvite: () => confirmInvite(redemptions[0]),
      send: (message) => socket.send(JSON.stringify(message)),
      failCreate: () => creations.shift().abort('failed'),
      finishCreate: () =>
        creations.shift().fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(settings),
        }),
    };
  }

  async function mediaScenarios() {
    const call = await fixture({ room: true });
    try {
      await call.page.locator('#name-input').fill('Layout guest');
      await call.page.locator('#room-input').fill('fixture-room');
      await call.page.locator('#join-btn').click();
      await call.page.locator('#room-screen').waitFor({ state: 'visible' });
      await mediaLayout(call.page, artifacts, report);
      assert.equal(await call.page.evaluate(() => window.__captureRequests), 0);
    } finally {
      await call.context.close();
    }
  }

  async function assertRoomLayout(page, viewport) {
    const layout = await page.evaluate(() => {
      const header = document.querySelector('header').getBoundingClientRect();
      const room = document.querySelector('#room-screen').getBoundingClientRect();
      const controls = document.querySelector('#controls');
      const controlsBounds = controls.getBoundingClientRect();
      const stage = document.querySelector('#main-area');
      const stageBounds = stage.getBoundingClientRect();
      const sidebar = document.querySelector('#sidebar').getBoundingClientRect();
      const roster = document.querySelector('#classic-users-panel');
      const rosterBounds = roster?.getBoundingClientRect();
      const desktop =
        innerWidth > 768 &&
        !matchMedia('(max-width: 900px) and (max-height: 500px) and (orientation: landscape)')
          .matches;
      const brand = document.querySelector('header h1').getBoundingClientRect();
      const label = document.querySelector('#room-label').getBoundingClientRect();
      const buttons = [
        ...controls.querySelectorAll('button'),
        document.querySelector('#room-more-btn'),
      ]
        .filter((node) => node.getClientRects().length)
        .map((node) => {
          const box = node.getBoundingClientRect();
          const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
          return {
            id: node.id,
            visible:
              box.left >= 0 &&
              box.right <= innerWidth + 1 &&
              box.top >= 0 &&
              box.bottom <= innerHeight + 1 &&
              hit !== null &&
              node.contains(hit),
          };
        });
      return {
        horizontalOverflow: document.documentElement.scrollWidth > innerWidth + 1,
        headerHeight: header.height,
        controlsHeight: controlsBounds.height,
        redundantChatLabelVisible:
          desktop &&
          document.querySelector('#room-screen').classList.contains('layout-classic') &&
          document.querySelector('#lobby-tab').hidden &&
          document.querySelector('#sidebar-tabs').getClientRects().length > 0,
        rosterLabelVisible:
          (roster?.querySelector('.panel-title')?.getClientRects().length ?? 0) > 0,
        roomTop: room.top,
        roomBottom: room.bottom,
        headerBottom: header.bottom,
        controlsInStage: stage.contains(controls),
        controlsConfinedToStage:
          controlsBounds.left >= stageBounds.left - 1 &&
          controlsBounds.right <= stageBounds.right + 1 &&
          controlsBounds.top >= stageBounds.top &&
          Math.abs(controlsBounds.bottom - stageBounds.bottom) <= 1,
        sidebarReachesBottom: Math.abs(sidebar.bottom - room.bottom) <= 1,
        desktopPanelsUseFullHeight:
          !desktop ||
          (Math.abs(sidebar.top - room.top) <= 1 &&
            (!rosterBounds?.width ||
              (Math.abs(rosterBounds.top - room.top) <= 1 &&
                Math.abs(rosterBounds.bottom - room.bottom) <= 1))),
        roomLabelCenteredBesideBrand:
          !brand.width || Math.abs(label.y + label.height / 2 - (brand.y + brand.height / 2)) <= 1,
        buttons,
      };
    });
    assert.equal(layout.horizontalOverflow, false, `Room overflow at ${viewport.width}`);
    assert.ok(
      layout.controlsHeight <= 56,
      `Compact call bar at ${viewport.width}: ${layout.controlsHeight}`,
    );
    assert.equal(
      layout.redundantChatLabelVisible,
      false,
      'The sole desktop Chat label does not take a row',
    );
    assert.equal(layout.rosterLabelVisible, false, 'The roster title does not take a row');
    assert.equal(layout.controlsInStage, true, 'Call controls belong below the video');
    assert.equal(
      layout.controlsConfinedToStage,
      true,
      'Call controls fit the bottom of the video column',
    );
    assert.equal(layout.sidebarReachesBottom, true, 'Chat reaches the bottom of the room');
    assert.equal(
      layout.desktopPanelsUseFullHeight,
      true,
      'Desktop chat and people use the full room height',
    );
    assert.equal(
      layout.roomLabelCenteredBesideBrand,
      true,
      'Room name is centered beside the brand',
    );
    assert.ok(
      layout.headerHeight <= 96,
      `Compact header at ${viewport.width}: ${layout.headerHeight}`,
    );
    assert.ok(
      Math.abs(layout.roomTop - layout.headerBottom) <= 1,
      'Room starts immediately below the header',
    );
    assert.ok(
      Math.abs(layout.roomBottom - viewport.height) <= 1,
      'The room uses the full remaining screen height',
    );
    for (const button of layout.buttons)
      assert.equal(button.visible, true, `${button.id} remains reachable at ${viewport.width}`);
    await openRoomMenu(page);
    const menu = page.locator('#room-more-menu');
    const menuBounds = await menu.boundingBox();
    assert.ok(
      menuBounds.x >= 0 &&
        menuBounds.y >= 0 &&
        menuBounds.x + menuBounds.width <= viewport.width + 1 &&
        menuBounds.y + menuBounds.height <= viewport.height + 1,
      `More menu fits ${viewport.width}x${viewport.height}`,
    );
    await menu.locator('button:visible').last().click({ trial: true });
    if (viewport.width === 320)
      await page.screenshot({ path: path.join(artifacts, 'room-menu-320x568.png') });
    await page.keyboard.press('Escape');
    report.checks.push(
      `Video-only bottom controls, full-height side panels, and scrolling More menu remain contained at ${viewport.width}x${viewport.height}`,
    );
  }

  async function scenarios() {
    await mediaScenarios();
    const discovering = await fixture({
      signedIn: true,
      holdFeatures: true,
      invite: true,
    });
    await discovering.page.locator('#auth-display-name').waitFor({ state: 'visible' });
    assert.equal(await discovering.page.locator('#join-btn').isDisabled(), true);
    assert.equal(await discovering.page.locator('#community-actions').isVisible(), false);
    assert.equal(discovering.requests.includes('/api/rooms'), false);
    assert.equal(discovering.previews.length, 0);
    await discovering.finishFeatures(0, { error: 'Temporary unavailability' }, 503);
    await discovering.page.locator('#server-features-retry').waitFor({ state: 'visible' });
    assert.equal(discovering.previews.length, 0);
    const retryRequested = discovering.page.waitForRequest('**/api/capabilities');
    await discovering.page.locator('#server-features-retry').click();
    await retryRequested;
    assert.equal(await discovering.page.locator('#server-features-retry').isDisabled(), true);
    await discovering.finishFeatures(1);
    await discovering.page.getByRole('dialog', { name: 'Review room invitation' }).waitFor();
    await discovering.page.getByRole('button', { name: 'Accept invitation' }).waitFor();
    assert.equal(discovering.previews.length, 1);
    assert.equal(discovering.redemptions.length, 0);
    assert.equal(
      discovering.sent.some((message) => message.type === 'joinRoom'),
      false,
    );
    assert.equal(discovering.featureRequests.length, 2);
    report.checks.push(
      'Failed discovery blocks directory and invitation calls; a bounded explicit retry resumes the restored-account invitation exactly once without accepting or joining',
    );
    await discovering.context.close();

    const restoring = await fixture({ signedIn: true, holdRestore: true, invite: true });
    await restoring.page.getByRole('dialog', { name: 'Room invitation', exact: true }).waitFor();
    assert.equal(restoring.previews.length, 0);
    await restoring.finishRestore();
    await restoring.page.getByRole('dialog', { name: 'Review room invitation' }).waitFor();
    await restoring.page.getByRole('button', { name: 'Accept invitation' }).waitFor();
    assert.equal(restoring.previews.length, 1);
    assert.equal(restoring.redemptions.length, 0);
    assert.equal(
      restoring.sent.some((message) => message.type === 'joinRoom'),
      false,
    );
    report.checks.push(
      'Discovery before session restoration previews the invitation once for the restored identity and never accepts or joins',
    );
    await restoring.context.close();

    for (const failure of ['invalid', 'deadline']) {
      const unavailable = await fixture({ holdFeatures: true, clock: true, room: true });
      await unavailable.page.locator('#name-input').fill('Fixture guest');
      await unavailable.page.locator('#room-input').fill('fixture-room');
      assert.equal(await unavailable.page.locator('#join-btn').isDisabled(), true);
      assert.equal(await unavailable.page.locator('#sign-in-btn').isVisible(), false);
      assert.equal(await unavailable.page.locator('#room-browser').isVisible(), false);
      if (failure === 'invalid')
        await unavailable.finishFeatures(0, { ...capabilities, version: 2 });
      else await unavailable.page.clock.fastForward(15_001);
      await unavailable.page.locator('#server-features-retry').waitFor({ state: 'visible' });
      assert.equal(await unavailable.page.locator('#join-btn').isDisabled(), true);
      assert.equal(unavailable.requests.includes('/api/rooms'), false);
      const retried = unavailable.page.waitForRequest('**/api/capabilities');
      await unavailable.page.locator('#server-features-retry').click();
      await retried;
      await unavailable.finishFeatures(1, {
        ...capabilities,
        accounts: false,
        passwordLogin: false,
        passwordRegistration: 'disabled',
        roomDirectory: false,
        roomCreation: false,
      });
      await unavailable.page.locator('#server-features-retry').waitFor({ state: 'hidden' });
      assert.equal(await unavailable.page.locator('#join-btn').isEnabled(), true);
      assert.equal(await unavailable.page.locator('#sign-in-btn').isVisible(), false);
      assert.equal(await unavailable.page.locator('#room-browser').isVisible(), false);
      await unavailable.page.locator('#join-btn').click();
      await unavailable.page.locator('#room-screen').waitFor({ state: 'visible' });
      assert.equal(unavailable.sent.filter((message) => message.type === 'joinRoom').length, 1);
      report.checks.push(
        `A discovery ${failure} keeps features unavailable until explicit retry advertises guest rooms; one guest join then succeeds`,
      );
      await unavailable.context.close();
    }

    const preparing = await fixture({ signedIn: true, holdTicket: true });
    assert.equal(preparing.connections(), 0);
    const aborted = preparing.page.waitForEvent('requestfailed', {
      predicate: (request) => new URL(request.url()).pathname === '/api/auth/ws-ticket',
    });
    const nextTicket = preparing.page.waitForRequest(
      (request) => new URL(request.url()).pathname === '/api/auth/ws-ticket',
    );
    await preparing.replaceAccount();
    await aborted;
    await nextTicket;
    assert.equal(preparing.connections(), 0, 'changing account cannot reuse a pending ticket');
    await preparing.finishTicket(1);
    await preparing.page.getByText('Connected', { exact: true }).waitFor();
    assert.equal(preparing.connections(), 1);
    report.checks.push(
      'Account replacement cancels an in-flight ticket mint and waits for its own ticket before opening a socket',
    );
    await preparing.context.close();

    const invited = await fixture({ signedIn: true, room: true, invite: true, holdSocket: true });
    const review = invited.page.getByRole('dialog', {
      name: 'Review room invitation',
      exact: true,
    });
    await review
      .getByText('Room: Fixture room (fixture-room). Offered role: user.', { exact: true })
      .waitFor();
    assert.equal(invited.redemptions.length, 0, 'a link may only preview');
    assert.equal(
      new URL(invited.page.url()).hash,
      '',
      'the secret is removed from browser history',
    );
    await review.getByRole('button', { name: 'Accept invitation', exact: true }).click();
    await invited.page
      .getByText(
        'Invitation confirmed for Fixture room. Current permissions apply. Choose Join when ready.',
        { exact: true },
      )
      .waitFor();
    assert.equal(await invited.page.locator('#room-input').inputValue(), 'fixture-room');
    assert.equal(invited.sent.filter((message) => message.type === 'joinRoom').length, 0);
    invited.releaseSocket();
    await invited.page.getByText('Connected', { exact: true }).waitFor();
    assert.equal(
      invited.sent.filter((message) => message.type === 'joinRoom').length,
      0,
      'readiness cannot turn acceptance into an automatic join',
    );
    assert.equal(await invited.page.locator('#join-screen').isVisible(), true);
    await invited.page.locator('#join-btn').click();
    await invited.page.locator('#room-screen').waitFor({ state: 'visible' });
    assert.deepEqual(
      invited.sent
        .filter((message) => message.type === 'joinRoom')
        .map((message) => message.roomId),
      ['fixture-room'],
    );
    assert.equal(invited.redemptions.length, 1);
    assert.equal(await invited.page.evaluate(() => window.__captureRequests), 0);
    await invited.page
      .locator('#room-screen')
      .evaluate((node) => node.style.setProperty('--roster-width', '160px'));
    const marker = invited.page.locator('#classic-users-panel .identity-badge').first();
    await marker.waitFor({ state: 'visible' });
    assert.equal(
      await marker.evaluate((node) => {
        const markerBounds = node.getBoundingClientRect();
        const rowBounds = node.closest('li').getBoundingClientRect();
        return markerBounds.left >= rowBounds.left && markerBounds.right <= rowBounds.right + 1;
      }),
      true,
      'identity marker remains visible in the minimum-width roster',
    );
    await invited.page.screenshot({ path: path.join(artifacts, 'identity-roster.png') });
    report.checks.push(
      'An invitation requires explicit acceptance and a separate join even when signaling becomes ready later',
    );
    await invited.context.close();

    const inPlace = await fixture({ signedIn: true });
    await inPlace.page.locator('#room-input').fill('existing-choice');
    await inPlace.page.goto(`${origin}/#invite=${'a'.repeat(32)}`);
    await inPlace.page
      .getByRole('dialog', { name: 'Review room invitation', exact: true })
      .getByRole('button', { name: 'Accept invitation', exact: true })
      .waitFor();
    assert.equal(inPlace.redemptions.length, 0);
    assert.equal(inPlace.sent.filter((message) => message.type === 'joinRoom').length, 0);
    await inPlace.page.keyboard.press('Escape');
    report.checks.push(
      'A fragment link opened in the existing document previews without becoming a room navigation',
    );
    await inPlace.context.close();

    const cancelled = await fixture({ signedIn: true, invite: true, holdSocket: true });
    await cancelled.page
      .getByRole('dialog', { name: 'Review room invitation', exact: true })
      .getByRole('button', { name: 'Accept invitation', exact: true })
      .waitFor();
    await cancelled.page.keyboard.press('Escape');
    cancelled.releaseSocket();
    await cancelled.page.getByText('Connected', { exact: true }).waitFor();
    assert.equal(cancelled.redemptions.length, 0);
    assert.equal(cancelled.sent.filter((message) => message.type === 'joinRoom').length, 0);
    report.checks.push('Dismissing an invitation neither redeems nor joins it');
    await cancelled.context.close();

    const superseded = await fixture({ signedIn: true, invite: true, holdInvite: true });
    await superseded.page
      .getByRole('dialog', { name: 'Review room invitation', exact: true })
      .getByRole('button', { name: 'Accept invitation', exact: true })
      .click();
    await superseded.inviteSeen;
    await superseded.page.evaluate(() => {
      window.location.hash = 'newer-choice';
    });
    await superseded.page.waitForFunction(
      () => document.querySelector('#room-input').value === 'newer-choice',
    );
    // Await application-side JSON consumption before checking that it ignored the
    // obsolete response. A completed route.fulfill alone is not such a barrier.
    await superseded.page.evaluate(() => {
      const parse = Response.prototype.json;
      window.__inviteResponseRead = false;
      Response.prototype.json = async function () {
        const value = await parse.call(this);
        if (this.url.includes('/api/rooms/invites/'))
          setTimeout(() => (window.__inviteResponseRead = true), 0);
        return value;
      };
    });
    await superseded.finishInvite();
    await superseded.page.waitForFunction(() => window.__inviteResponseRead);
    assert.equal(await superseded.page.locator('#room-input').inputValue(), 'newer-choice');
    assert.equal(superseded.sent.filter((message) => message.type === 'joinRoom').length, 0);
    assert.equal(superseded.redemptions.length, 1);
    report.checks.push('A late invitation response preserves newer navigation');
    await superseded.context.close();

    const signedOut = await fixture({ signedIn: true });
    await signedOut.page.evaluate(() => {
      for (const [key, value] of [
        ['displayName', 'Private label'],
        ['simplestchat.capturePreferences', '{"microphoneId":"private-device"}'],
        ['simplestchat.chat.v1.fixture-account', '{"ignored":["private-contact"]}'],
        ['unrelated-app', 'keep'],
      ])
        localStorage.setItem(key, value);
    });
    await signedOut.page.locator('#logout-btn').click();
    await signedOut.page.locator('#sign-in-btn').waitFor({ state: 'visible' });
    await signedOut.page.waitForFunction(
      () =>
        document.querySelector('#name-input').value === '' &&
        localStorage.getItem('displayName') === null,
    );
    assert.deepEqual(await signedOut.page.evaluate(() => Object.keys(localStorage).sort()), [
      'simplestchat-account-change-v1',
      'unrelated-app',
    ]);
    assert.match(
      await signedOut.page.evaluate(() => localStorage.getItem('simplestchat-account-change-v1')),
      /^[a-f0-9]{32}$/,
      'A credential-free revision lets suspended tabs observe sign-out',
    );
    report.checks.push(
      'Ordinary sign-out clears browser identity, device and chat data while preserving unrelated app storage',
    );
    await signedOut.context.close();

    const f = await fixture();
    await f.page.locator('#sign-in-btn').click();
    const dialog = f.page.getByRole('dialog', { name: 'Sign In', exact: true });
    await dialog.waitFor();
    assert.equal(
      await f.page.locator('#login-email').evaluate((node) => node === document.activeElement),
      true,
    );
    for (let count = 0; count < 12; count++) {
      await f.page.keyboard.press(count < 6 ? 'Tab' : 'Shift+Tab');
      assert.equal(
        await f.page.evaluate(
          () =>
            document.activeElement === document.body ||
            !!document.activeElement.closest('#login-modal'),
        ),
        true,
      );
    }
    await f.page.locator('#name-input').evaluate((node) => node.focus());
    assert.equal(
      await f.page.locator('#name-input').evaluate((node) => node === document.activeElement),
      false,
    );
    await f.page.keyboard.press('Escape');
    assert.equal(
      await f.page.locator('#sign-in-btn').evaluate((node) => node === document.activeElement),
      true,
    );
    await f.page.locator('#sign-in-btn').click();
    await f.page.locator('#login-to-register').click();
    await f.page.getByRole('dialog', { name: 'Create Account', exact: true }).waitFor();
    assert.equal(
      await f.page.locator('#register-email').evaluate((node) => node === document.activeElement),
      true,
    );
    assert.equal(await f.page.locator('#register-passkey-btn').isVisible(), false);
    assert.match(await f.page.locator('#registration-help').textContent(), /password account/);
    await f.page.keyboard.press('Escape');
    report.checks.push(
      'Auth dialogs own keyboard focus and reflect invite-only passkey availability',
    );
    await f.context.close();

    for (const storage of ['get', 'write']) {
      const f = await fixture({ storage });
      await f.page.locator('#name-input').fill('Fixture guest');
      await f.page.locator('#room-input').fill('storage-fixture');
      await f.page.locator('#join-btn').click();
      await f.page.waitForFunction(
        () => document.querySelector('#join-btn').textContent === 'Joining...',
      );
      assert.equal(
        f.sent.some((message) => message.type === 'joinRoom'),
        true,
      );
      report.checks.push(`Storage ${storage} failure preserves startup and joining`);
      await f.context.close();
    }

    const creating = await fixture({ signedIn: true });
    await creating.page.locator('#create-room-btn').click();
    await creating.page.locator('#cr-name').fill('Pending room');
    await creating.page.locator('#create-room-submit').click();
    await creating.page.getByRole('button', { name: 'Creating...', exact: true }).waitFor();
    await creating.page.locator('#create-room-close').click();
    await creating.page.locator('#room-input').fill('newer-choice');
    await creating.finishCreate();
    await creating.page
      .getByText('Room created. Open it from My rooms when ready.', { exact: true })
      .waitFor();
    assert.equal(await creating.page.locator('#room-input').inputValue(), 'newer-choice');
    assert.equal(
      creating.sent.some((message) => message.type === 'joinRoom'),
      false,
    );
    report.checks.push('Closed room creation cannot override a newer destination');
    await creating.context.close();

    const switching = await fixture({ signedIn: true });
    await switching.page.locator('#create-room-btn').click();
    await switching.page.locator('#cr-name').fill('First account room');
    await switching.page.locator('#create-room-submit').click();
    await switching.page.getByRole('button', { name: 'Creating...', exact: true }).waitFor();
    await switching.page.locator('#create-room-close').click();
    await switching.replaceAccount();
    await switching.page.locator('#create-room-btn').click();
    assert.equal(await switching.page.locator('#create-room-submit').isEnabled(), true);
    await switching.page.locator('#cr-name').fill('Second account room');
    await switching.page.locator('#create-room-submit').click();
    await switching.page.getByRole('button', { name: 'Creating...', exact: true }).waitFor();
    const failed = switching.page.waitForEvent('requestfailed', {
      predicate: (request) =>
        request.url() === `${origin}/api/rooms` && request.method() === 'POST',
    });
    await switching.failCreate();
    await failed;
    await switching.page.waitForTimeout(100);
    assert.equal(await switching.page.locator('#create-room-submit').textContent(), 'Creating...');
    assert.equal(await switching.page.locator('#create-room-submit').isDisabled(), true);
    await switching.page.locator('#create-room-close').click();
    await switching.finishCreate();
    await switching.page
      .getByText('Room created. Open it from My rooms when ready.', { exact: true })
      .waitFor();
    report.checks.push(
      'An old account creation failure cannot retire a replacement account mutation',
    );
    await switching.context.close();

    const guest = await fixture({
      features: {
        ...capabilities,
        accounts: false,
        passwordLogin: false,
        passwordRegistration: 'disabled',
        roomDirectory: false,
        roomCreation: false,
      },
    });
    await guest.page.locator('#server-mode').waitFor();
    assert.equal(await guest.page.locator('#sign-in-btn').isVisible(), false);
    assert.equal(await guest.page.locator('#room-browser').isVisible(), false);
    report.checks.push('Guest-only server exposes available onboarding');
    await guest.context.close();

    const call = await fixture({ room: true, signedIn: true });
    await call.page.locator('#name-input').fill('Fixture guest');
    await call.page.locator('#room-input').fill('fixture-room');
    await call.page.locator('#join-btn').click();
    await call.page.locator('#room-screen').waitFor({ state: 'visible' });
    assert.equal(await call.page.locator('#participant-list li').count(), 0);
    const input = call.page.locator('#chat-input');
    assert.equal(await input.evaluate((node) => node.tagName), 'TEXTAREA');
    assert.equal(await call.page.locator('#chat-send-btn').isVisible(), false);
    await input.fill('First line');
    const oneLineHeight = (await input.boundingBox()).height;
    await input.press('Shift+Enter');
    await input.pressSequentially('Second line');
    assert.equal(await input.inputValue(), 'First line\nSecond line');
    assert.equal(call.sent.filter((message) => message.type === 'chatMessage').length, 0);
    assert.ok((await input.boundingBox()).height > oneLineHeight);
    await input.press('Enter');
    await call.page.waitForFunction(() => document.querySelector('#chat-input').value === '');
    assert.equal(
      call.sent.find((message) => message.type === 'chatMessage').content,
      'First line\nSecond line',
    );
    assert.ok((await input.boundingBox()).height <= oneLineHeight + 1);
    report.checks.push(
      'Desktop multiline composer grows, Shift+Enter adds a line, Enter sends and resets its height',
    );
    await call.page.locator('.toast').evaluateAll((nodes) => nodes.forEach((node) => node.click()));
    await assertRoomLayout(call.page, { width: 1440, height: 900 });
    await call.page.screenshot({ path: path.join(artifacts, 'room-1440x900.png') });
    await call.page.locator('#settings-btn').click();
    await call.page.getByRole('dialog', { name: 'Your settings', exact: true }).waitFor();
    await call.page.keyboard.press('Escape');
    assert.equal(
      await call.page.locator('#settings-btn').evaluate((node) => node === document.activeElement),
      true,
    );
    report.checks.push(
      'Settings remain below the video and Escape returns focus to the settings button',
    );
    call.send({
      type: 'roomSettingsChanged',
      settings: {
        ...settings,
        displayName: 'A very long room name that must fit the compact room header',
        topic:
          'A long room topic that should stay accessible without taking height away from chat and people.',
      },
    });
    const action = call.page.locator(
      '#classic-users-panel [data-participant-id="remote-fixture"] button',
    );
    await action.focus();
    call.send({
      type: 'participantJoined',
      participantId: 'another',
      participantName: 'Another person',
      role: 'user',
      authenticated: false,
    });
    await call.page.waitForFunction(
      () => document.querySelectorAll('#classic-users-panel li').length === 3,
    );
    assert.equal(await action.evaluate((node) => node === document.activeElement), true);
    report.checks.push('One keyed visible roster preserves action focus during membership churn');
    await call.page.locator('#toggle-roster').click();
    assert.equal(await call.page.locator('#classic-users-panel li').count(), 0);
    await call.page.locator('#toggle-roster').click();
    assert.equal(await call.page.locator('#classic-users-panel li').count(), 3);
    report.checks.push('Collapsing the roster releases hidden rows and reopening restores members');
    const publicConversation = call.page.locator(
      '.conversation-tab[data-conversation-id="public"]',
    );
    const privateConversation = call.page.locator(
      '.conversation-tab[data-conversation-id="remote-fixture"]',
    );
    const longName = 'Alexandra with a very long display name for the conversation';
    const privateDraft = 'Unsent private draft\nwith a second line';
    call.send({ type: 'nicknameChanged', participantId: 'remote-fixture', nickname: longName });
    const incomingPrivate = (index, participantId = 'remote-fixture', participantName = longName) =>
      call.send({
        type: 'privateMessageReceived',
        message: {
          messageId: `private-fixture-${participantId}-${index}`,
          clientMessageId: `private-fixture-${participantId}-${index}`,
          participantId,
          participantName,
          recipientId: 'local-fixture',
          recipientName: 'Fixture guest',
          content: `Private fixture message ${index}`,
          sentAt: new Date().toISOString(),
        },
      });
    await input.fill('Unsent public draft');
    incomingPrivate(1);
    await privateConversation.locator('.conversation-unread').waitFor({ state: 'visible' });
    assert.match(await privateConversation.getAttribute('aria-label'), /1 unread/);
    await privateConversation.click();
    assert.equal(await privateConversation.getAttribute('aria-pressed'), 'true');
    assert.equal(await input.inputValue(), '');
    assert.equal(await privateConversation.locator('.conversation-unread').isVisible(), false);
    await call.page.getByText('Private fixture message 1', { exact: true }).waitFor();
    await input.fill(privateDraft);
    await publicConversation.click();
    assert.equal(await input.inputValue(), 'Unsent public draft');
    assert.equal(
      await call.page.getByText('Private fixture message 1', { exact: true }).count(),
      0,
    );
    incomingPrivate(2);
    await privateConversation.locator('.conversation-unread').waitFor({ state: 'visible' });
    assert.match(await privateConversation.getAttribute('aria-label'), /1 unread/);
    await input.fill('');
    incomingPrivate(1, 'another', 'Another person');
    await call.page
      .locator('.conversation-tab[data-conversation-id="another"] .conversation-unread')
      .waitFor({ state: 'visible' });
    await call.page.screenshot({ path: path.join(artifacts, 'room-conversations-1440x900.png') });
    report.checks.push(
      'Conversation pills preserve independent multiline drafts, unread badges, and private-message isolation',
    );
    for (const viewport of [
      { width: 320, height: 568 },
      { width: 390, height: 844 },
      { width: 844, height: 390 },
    ]) {
      await call.page.setViewportSize(viewport);
      await call.page.waitForTimeout(250);
      assert.equal(await call.page.locator('#participant-list li').count(), 0);
      if (viewport.width === 320) {
        await call.page.locator('#sidebar-tabs [data-tab="users"]').click();
        assert.equal(await call.page.locator('#participant-list li').count(), 3);
        await call.page.locator('#sidebar-tabs [data-tab="chat"]').click();
        assert.equal(await call.page.locator('#participant-list li').count(), 0);
        report.checks.push('Hidden mobile People tab has no roster rows');
      }
      await assertRoomLayout(call.page, viewport);
      await privateConversation.click();
      assert.equal(await input.inputValue(), privateDraft);
      await publicConversation.click();
      assert.equal(await input.inputValue(), '');
      assert.ok(
        (await call.page.locator('.conversation-toolbar').boundingBox()).height <= 60,
        'Long conversation names keep a compact single row',
      );
      await call.page
        .getByRole('button', { name: 'Chat options', exact: true })
        .click({ trial: true });
      await input.fill('A multiline draft with enough text to wrap.\n'.repeat(12));
      for (const emoji of [false, true]) {
        if (emoji)
          await call.page.getByRole('button', { name: 'Choose emoji', exact: true }).click();
        const visible = await call.page.locator('#chat-input').evaluate((node) => {
          const box = node.getBoundingClientRect();
          const panel = document.querySelector('#chat-panel').getBoundingClientRect();
          const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
          return (
            box.bottom <= panel.bottom + 1 &&
            box.top >= panel.top &&
            hit !== null &&
            node.contains(hit)
          );
        });
        assert.equal(
          visible,
          true,
          `Composer ${viewport.width}x${viewport.height}, emoji=${emoji}`,
        );
        if (emoji)
          await call.page.getByRole('button', { name: 'Choose emoji', exact: true }).click();
      }
      await call.page.screenshot({
        path: path.join(artifacts, `room-${viewport.width}x${viewport.height}.png`),
      });
      await input.fill('');
      report.checks.push(
        `Composer remains visible with optional emoji at ${viewport.width}x${viewport.height}`,
      );
    }
    await call.page.setViewportSize({ width: 390, height: 844 });
    await call.page.locator('#sidebar-tabs [data-tab="users"]').click();
    await call.page.locator('#sidebar-collapse').click();
    assert.equal(await call.page.locator('#participant-list li').count(), 0);
    await call.page.setViewportSize({ width: 844, height: 390 });
    await call.page.waitForFunction(
      () => document.querySelectorAll('#participant-list li').length === 3,
    );
    assert.equal(await call.page.locator('#users-panel').isVisible(), true);
    report.checks.push(
      'Landscape rotation restores the visible People panel after portrait collapse',
    );
    assert.equal(await call.page.evaluate(() => window.__captureRequests), 0);
    await call.context.close();

    const touch = await fixture({ room: true, hasTouch: true });
    await touch.page.setViewportSize({ width: 390, height: 844 });
    await touch.page.locator('#name-input').fill('Touch guest');
    await touch.page.locator('#room-input').fill('fixture-room');
    await touch.page.locator('#join-btn').click();
    await touch.page.locator('#room-screen').waitFor({ state: 'visible' });
    const touchInput = touch.page.locator('#chat-input');
    await touch.page.locator('#chat-send-btn').waitFor({ state: 'visible' });
    await touchInput.fill('Touch first line');
    await touchInput.press('Enter');
    await touchInput.pressSequentially('Touch second line');
    assert.equal(await touchInput.inputValue(), 'Touch first line\nTouch second line');
    assert.equal(touch.sent.filter((message) => message.type === 'chatMessage').length, 0);
    await touch.page.locator('#chat-send-btn').click();
    await touch.page.waitForFunction(() => document.querySelector('#chat-input').value === '');
    assert.equal(
      touch.sent.find((message) => message.type === 'chatMessage').content,
      'Touch first line\nTouch second line',
    );
    await touchInput.fill('Keyboard shortcut on a touch device');
    await touchInput.press('Control+Enter');
    await touch.page.waitForFunction(() => document.querySelector('#chat-input').value === '');
    assert.equal(touch.sent.filter((message) => message.type === 'chatMessage').length, 2);
    await touch.page
      .locator('.toast')
      .evaluateAll((nodes) => nodes.forEach((node) => node.click()));
    await touchInput.fill('Long touch draft\n'.repeat(12));
    await touch.page.getByRole('button', { name: 'Choose emoji', exact: true }).click();
    await touch.page.locator('#chat-send-btn').click({ trial: true });
    await touch.page.screenshot({ path: path.join(artifacts, 'room-touch-390x844.png') });
    assert.equal(await touch.page.evaluate(() => window.__captureRequests), 0);
    await touch.context.close();
    report.checks.push(
      'Touch composer keeps Send reachable with a long draft and emoji; Enter adds lines and Ctrl+Enter sends',
    );
    assert.equal(report.pageErrors, 0, 'No unhandled browser errors');
  }
}
run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
