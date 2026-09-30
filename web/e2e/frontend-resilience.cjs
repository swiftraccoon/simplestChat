/** Current production UI with isolated HTTP/WS fixtures; no services or capture. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { productionAssets } = require('./homepage-layout.cjs');
const { closeOwnedBrowser } = require('./lifecycle-cleanup.cjs');
const { chromium } = require('playwright');

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
  const artifacts = process.env.E2E_ARTIFACTS
    ? path.resolve(process.env.E2E_ARTIFACTS)
    : fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-frontend-resilience-'));
  fs.mkdirSync(artifacts, { recursive: true });
  const report = {
    scope:
      'Isolated production UI with mocked HTTP/signaling; no backend, native device or mobile-browser claim.',
    checks: [],
    pageErrors: 0,
  };
  const save = () =>
    fs.writeFileSync(path.join(artifacts, 'results.json'), JSON.stringify(report, null, 2));
  const assets = productionAssets();
  const server = await chromium.launchServer({ headless: true });
  const browser = await chromium.connect(server.wsEndpoint());
  let deadline;
  try {
    await Promise.race([
      scenarios(),
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
  } = {}) {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      serviceWorkers: 'block',
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
      assert.equal(url.origin, origin, 'No external requests');
      const asset = assets.get(url.pathname);
      if (asset)
        return route.fulfill({ status: 200, contentType: asset.contentType, body: asset.body });
      const json = (body, status = 200) =>
        route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
      if (url.pathname === '/favicon.ico') return route.fulfill({ status: 204 });
      if (url.pathname === '/api/capabilities') return json(features);
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
      if (url.pathname === '/api/auth/refresh')
        return json(
          account ? { token: 'owned-fixture', user: account } : { error: 'No saved session' },
          account ? 200 : 401,
        );
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
    page.on('pageerror', () => {
      report.pageErrors++;
    });
    await page.goto(invite ? `${origin}/#invite=${'a'.repeat(32)}` : origin);
    if (holdTicket) await ticketSeen.promise;
    else if (holdSocket) await socketSeen.promise;
    else await page.getByText('Connected', { exact: true }).waitFor();
    return {
      context,
      page,
      sent,
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

  async function scenarios() {
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

    const call = await fixture({ room: true });
    await call.page.locator('#name-input').fill('Fixture guest');
    await call.page.locator('#room-input').fill('fixture-room');
    await call.page.locator('#join-btn').click();
    await call.page.locator('#room-screen').waitFor({ state: 'visible' });
    assert.equal(await call.page.locator('#participant-list li').count(), 0);
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
      for (const emoji of [false, true]) {
        if (emoji)
          await call.page.getByRole('button', { name: 'Choose emoji', exact: true }).click();
        const visible = await call.page.locator('#chat-send-btn').evaluate((node) => {
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
    assert.equal(report.pageErrors, 0, 'No unhandled browser errors');
  }
}
run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
