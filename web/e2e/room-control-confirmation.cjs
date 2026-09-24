/** Owned loopback DOM tests with fully synthetic membership and control replies. */
const assert = require('node:assert/strict');
const { browserOptions } = require('./browser-options.cjs');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:38179');
if (
  process.env.ROOM_CONTROL_E2E !== '1' ||
  origin.protocol !== 'http:' ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error('Set ROOM_CONTROL_E2E=1 with an owned HTTP loopback frontend origin.');

const options = browserOptions(process.env.E2E_BROWSER);

async function main() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  const context = await browser.newContext();
  const failures = [];
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    page.on('pageerror', (error) => failures.push(error.message));
    await page.addInitScript(() => {
      const fixture = { commands: [], captures: 0, socket: null };
      window.__roomControlFixture = fixture;
      const settings = (id) => ({
        id,
        displayName: id,
        passwordProtected: false,
        requireRegistration: false,
        allowScreenSharing: true,
        allowChat: true,
        allowVideo: true,
        moderated: false,
        inviteOnly: false,
        secret: false,
        lobbyEnabled: true,
        pushToTalk: false,
        guestsAllowed: true,
        guestsCanBroadcast: true,
        topic: '',
      });
      class FixtureSocket {
        static OPEN = 1;
        static CONNECTING = 0;
        readyState = 0;
        room = null;
        constructor() {
          fixture.socket = this;
          setTimeout(() => {
            this.readyState = 1;
            this.onopen?.();
          }, 0);
        }
        close() {
          this.readyState = 3;
        }
        receive(message) {
          this.onmessage?.({ data: JSON.stringify(message) });
        }
        send(raw) {
          const request = JSON.parse(raw);
          if (request.type === 'joinRoom') {
            this.room = { settings: settings(request.roomId), lobby: [] };
            setTimeout(
              () =>
                this.receive({
                  type: 'roomJoined',
                  participantId: 'owned-local',
                  participants: [],
                  reconnectToken: 'owned-reconnect',
                  yourRole: 'owner',
                  roomSettings: this.room.settings,
                }),
              0,
            );
          } else if (request.type === 'getRouterRtpCapabilities') {
            setTimeout(
              () =>
                this.receive({
                  type: 'error',
                  requestId: request.requestId,
                  message: 'Owned chat-only fixture',
                }),
              0,
            );
          } else if (request.type === 'getRoomSnapshot') {
            setTimeout(
              () =>
                this.receive({
                  type: 'socialResponse',
                  requestId: request.requestId,
                  action: request.type,
                  data: {
                    participants: [],
                    messages: [],
                    yourRole: 'owner',
                    roomSettings: this.room.settings,
                    allowPrivateMessages: true,
                    ignoredParticipantIds: [],
                    lobby: this.room.lobby,
                  },
                }),
              0,
            );
          } else if (
            ['updateRoomSettings', 'setTopic', 'admitFromLobby', 'denyFromLobby'].includes(
              request.type,
            )
          ) {
            if (fixture.commands.length >= 32) throw new Error('Fixture command bound exceeded');
            fixture.commands.push({ request, socket: this, room: this.room });
          }
        }
      }
      fixture.reply = (index, accepted) => {
        const entry = fixture.commands[index];
        if (!entry) throw new Error('Missing fixture command');
        const { socket, room, request } = entry;
        if (accepted) {
          if (request.type === 'setTopic') room.settings.topic = request.topic;
          if (request.type === 'updateRoomSettings' && 'password' in request)
            room.settings.passwordProtected = request.password !== null;
          if (request.type === 'admitFromLobby' || request.type === 'denyFromLobby')
            room.lobby = room.lobby.filter(
              (entry) => entry.participantId !== request.targetParticipantId,
            );
          if (socket.room === room && ['updateRoomSettings', 'setTopic'].includes(request.type))
            socket.receive({ type: 'roomSettingsChanged', settings: room.settings });
          socket.receive({ type: 'roomControlApplied', requestId: request.requestId });
        } else
          socket.receive({
            type: 'error',
            requestId: request.requestId,
            message: 'Fixture rejected change',
          });
      };
      fixture.waiter = (name) => {
        fixture.socket.room.lobby.push({
          participantId: 'owned-waiter',
          displayName: name,
          authenticated: false,
        });
        fixture.socket.receive({
          type: 'lobbyJoin',
          participantId: 'owned-waiter',
          displayName: name,
          authenticated: false,
        });
      };
      window.WebSocket = FixtureSocket;
      navigator.mediaDevices.getUserMedia = () => {
        fixture.captures++;
        return Promise.reject(new Error('Capture is outside this control test'));
      };
    });
    await context.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/api/auth/refresh')
        await route.fulfill({ status: 401, json: { error: 'No session' } });
      else if (path === '/api/telemetry') await route.fulfill({ status: 204 });
      else await route.fulfill({ json: [] });
    });
    const join = async (id) => {
      await page.locator('#name-input').fill('Owned owner');
      await page.locator('#room-input').fill(id);
      await page.locator('#join-btn').click();
      await page.locator('#room-settings-btn').waitFor({ state: 'visible' });
    };
    const count = (value) =>
      page.waitForFunction(
        (length) => window.__roomControlFixture.commands.length === length,
        value,
      );
    const reply = (index, accepted) =>
      page.evaluate(({ index, accepted }) => window.__roomControlFixture.reply(index, accepted), {
        index,
        accepted,
      });
    const home = async () => {
      await page.evaluate(() => document.getElementById('home-link').click());
      await page.locator('#join-screen').waitFor({ state: 'visible' });
    };
    const change = async (selector, value) => {
      await page.locator(selector).fill(value);
      await page.locator(selector).press('Tab');
    };
    await page.goto(origin.toString());
    await join('owned-room-a');
    await page.locator('#room-settings-btn').click();
    await change('#rs-password', 'Owned-test-password');
    await count(1);
    assert.equal(await page.locator('#rs-password').isDisabled(), true);
    assert.equal(await page.locator('#room-settings-result').textContent(), 'Saving change…');
    await reply(0, false);
    await page.getByRole('button', { name: 'Retry change', exact: true }).waitFor();
    assert.equal(await page.locator('#rs-password').inputValue(), 'Owned-test-password');
    assert.match(
      await page.locator('#room-settings-result').textContent(),
      /Fixture rejected change/,
    );
    await page.getByRole('button', { name: 'Retry change', exact: true }).click();
    await count(2);
    await reply(1, true);
    await page.waitForFunction(
      () => document.getElementById('room-settings-result').textContent === 'Change saved',
    );
    assert.equal(await page.locator('#rs-password').inputValue(), '');
    await change('#rs-topic', 'Retained topic draft');
    await count(3);
    await reply(2, false);
    await page.getByRole('button', { name: 'Retry change', exact: true }).waitFor();
    assert.equal(await page.locator('#rs-topic').inputValue(), 'Retained topic draft');
    await page.getByRole('button', { name: 'Retry change', exact: true }).click();
    await count(4);
    await reply(3, true);
    await page.waitForFunction(
      () => document.getElementById('room-settings-result').textContent === 'Change saved',
    );

    await change('#rs-topic', 'Old room draft');
    await count(5);
    await home();
    await join('owned-room-b');
    await page.locator('#room-settings-btn').click();
    await change('#rs-topic', 'New room draft');
    await count(6);
    await reply(4, false);
    await page.waitForTimeout(50);
    assert.equal(await page.locator('#rs-topic').inputValue(), 'New room draft');
    assert.equal(
      await page.locator('#rs-topic').isDisabled(),
      true,
      'old completion cannot unlock newer settings',
    );
    assert.equal(await page.locator('#room-settings-result').textContent(), 'Saving change…');
    await reply(5, true);
    await page.waitForFunction(
      () => document.getElementById('room-settings-result').textContent === 'Change saved',
    );
    await page.keyboard.press('Escape');

    await page.evaluate(() => window.__roomControlFixture.waiter('First waiter'));
    await page.locator('[data-tab="lobby"]').click();
    const waiter = page.locator('.lobby-entry');
    await waiter.getByRole('button', { name: 'Admit', exact: true }).click();
    await count(7);
    assert.equal(await waiter.isVisible(), true, 'lobby entry stays until ACK');
    assert.equal(await waiter.getAttribute('aria-busy'), 'true');
    assert.equal(
      await waiter.getByRole('button', { name: 'Deny', exact: true }).isDisabled(),
      true,
    );
    await reply(6, false);
    await waiter.getByRole('alert').waitFor();
    assert.match(await waiter.textContent(), /Fixture rejected change/);
    await waiter.getByRole('button', { name: 'Admit', exact: true }).click();
    await count(8);
    await reply(7, true);
    await waiter.waitFor({ state: 'detached' });

    await page.evaluate(() => window.__roomControlFixture.waiter('Old waiter'));
    await waiter.getByRole('button', { name: 'Deny', exact: true }).click();
    await count(9);
    await home();
    await join('owned-room-c');
    await page.evaluate(() => window.__roomControlFixture.waiter('Replacement waiter'));
    await page.locator('[data-tab="lobby"]').click();
    await reply(8, false);
    await page.waitForTimeout(50);
    assert.match(await waiter.textContent(), /Replacement waiter/);
    assert.equal(await waiter.getByRole('alert').count(), 0);
    await waiter.getByRole('button', { name: 'Deny', exact: true }).click();
    await count(10);
    await reply(9, true);
    await waiter.waitFor({ state: 'detached' });
    assert.equal(await page.evaluate(() => window.__roomControlFixture.captures), 0);
    assert.deepEqual(failures, []);
    console.log(
      JSON.stringify({
        browser: options.name,
        passed: true,
        checks: [
          'pending-room-controls-disabled',
          'failed-password-topic-drafts-retained',
          'explicit-control-retry',
          'stale-settings-response-fenced',
          'lobby-remains-until-ack',
          'lobby-error-retry',
          'stale-lobby-response-fenced',
        ],
      }),
    );
  } finally {
    await context.close();
    await browser.close();
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
