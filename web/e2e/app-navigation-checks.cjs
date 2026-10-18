const assert = require('node:assert/strict');
const { openRoomMenu } = require('./room-menu.cjs');

const mediaChunk = /\/assets\/media-[A-Za-z0-9_-]+\.js$/;
function captureCount(page) {
  return page.evaluate(() => window.__communityCaptureRequests);
}
async function noMediaStartup(page) {
  assert.equal(await captureCount(page), 0);
  assert.deepEqual(
    await page.evaluate(() =>
      performance
        .getEntriesByType('resource')
        .filter((entry) => /\/assets\/media-[A-Za-z0-9_-]+\.js$/.test(new URL(entry.name).pathname))
        .map((entry) => new URL(entry.name).pathname),
    ),
    [],
    'account navigation must not download the deferred call engine',
  );
}
function privacyChecked(value) {
  const control = [...document.querySelectorAll('input[role="switch"]')].find(
    (node) =>
      document.getElementById(node.getAttribute('aria-labelledby'))?.textContent ===
      'Private messages from others',
  );
  return control && !control.disabled && control.checked === value;
}

function notificationChecked({ privateMessages, mentions }) {
  const controls = [...document.querySelectorAll('#settings-sheet input[role="switch"]')];
  return [
    ['Private messages', privateMessages],
    ['Mentions', mentions],
  ].every(([label, checked]) => {
    const control = controls.find(
      (node) =>
        document.getElementById(node.getAttribute('aria-labelledby'))?.textContent === label,
    );
    return control && !control.disabled && control.checked === checked;
  });
}

/** Hosted-only proof: separate account pages stay live without a room socket. */
async function checkHomepageAccountUpdates(
  owner,
  member,
  otherMember,
  { header, close, settingsScreen, ownerId, memberId },
) {
  await Promise.all([owner, member, otherMember].map(noMediaStartup));
  const receivedInbox = await header(member, 'Messages');
  const otherInbox = await header(otherMember, 'Messages');
  const inbox = await header(owner, 'Messages');
  await inbox.locator('.discovery-contacts > summary').click();
  await inbox
    .locator('.discovery-row')
    .filter({ hasText: 'E2E Member' })
    .getByRole('button', { name: 'Message', exact: true })
    .click();
  const sent = owner.getByRole('dialog', { name: 'Messages with E2E Member', exact: true });
  const text = 'Live account message without a room';
  await sent.getByLabel('Private message', { exact: true }).fill(text);
  await sent.getByLabel('Private message', { exact: true }).press('Enter');
  const recipientRow = receivedInbox.locator('.inbox-conversation').filter({ hasText: text });
  await recipientRow.waitFor({ timeout: 5000 });
  await recipientRow.getByText(/^E2E Owner · [1-9][0-9,]* unread$/).waitFor({ timeout: 5000 });
  const otherRow = otherInbox.locator('.inbox-conversation').filter({ hasText: text });
  await otherRow.click();
  const received = otherMember.getByRole('dialog', {
    name: 'Messages with E2E Owner',
    exact: true,
  });
  await received.getByText(text, { exact: true }).waitFor({ timeout: 5000 });
  await recipientRow.getByText('E2E Owner', { exact: true }).waitFor({ timeout: 5000 });

  const ownRow = sent.locator('.history-message').filter({ hasText: text });
  await ownRow.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = owner.getByRole('dialog', { name: 'Edit message', exact: true });
  const edited = 'Live account message edited without a room';
  await edit.getByLabel('Message', { exact: true }).fill(edited);
  await edit.getByRole('button', { name: 'Save changes', exact: true }).click();
  const incoming = received.locator('.history-message').filter({ hasText: edited });
  await incoming.waitFor({ timeout: 5000 });
  await incoming.getByRole('button', { name: 'React', exact: true }).click();
  await incoming
    .locator('.reaction-picker')
    .getByRole('button', { name: '👍', exact: true })
    .click();
  await sent
    .locator('.history-message')
    .filter({ hasText: edited })
    .getByRole('button', { name: '👍 1', exact: true })
    .waitFor({ timeout: 5000 });

  // Sever an established stream, mutate while offline, then require ready-resync.
  await otherMember.context().setOffline(true);
  await sent
    .locator('.history-message')
    .filter({ hasText: edited })
    .getByRole('button', { name: 'Edit', exact: true })
    .click();
  const missed = 'Recovered after the account stream disconnected';
  const offlineEdit = owner.getByRole('dialog', { name: 'Edit message', exact: true });
  await offlineEdit.getByLabel('Message', { exact: true }).fill(missed);
  await offlineEdit.getByRole('button', { name: 'Save changes', exact: true }).click();
  await otherMember.context().setOffline(false);
  await received.getByText(missed, { exact: true }).waitFor({ timeout: 10000 });
  await close(received);
  await close(otherInbox);
  await close(receivedInbox);
  await close(sent);
  await close(inbox);

  const firstPrivacy = await settingsScreen(member, 'Privacy');
  const secondPrivacy = await settingsScreen(otherMember, 'Privacy');
  await member.waitForFunction(privacyChecked, true);
  await otherMember.waitForFunction(privacyChecked, true);
  await firstPrivacy.getByLabel('Private messages from others', { exact: true }).uncheck();
  await otherMember.waitForFunction(privacyChecked, false, { timeout: 5000 });
  await firstPrivacy.getByLabel('Private messages from others', { exact: true }).check();
  await otherMember.waitForFunction(privacyChecked, true, { timeout: 5000 });
  await close(firstPrivacy);
  await close(secondPrivacy);
  const firstNotifications = await settingsScreen(member, 'Notifications');
  const secondNotifications = await settingsScreen(otherMember, 'Notifications');
  await member.waitForFunction(notificationChecked, { privateMessages: true, mentions: true });
  await otherMember.waitForFunction(notificationChecked, { privateMessages: true, mentions: true });
  await firstNotifications.getByLabel('Private messages', { exact: true }).uncheck();
  await otherMember.waitForFunction(
    notificationChecked,
    { privateMessages: false, mentions: true },
    { timeout: 5000 },
  );
  await secondNotifications.getByLabel('Mentions', { exact: true }).uncheck();
  await member.waitForFunction(
    notificationChecked,
    { privateMessages: false, mentions: false },
    { timeout: 5000 },
  );
  await otherMember.waitForFunction(
    notificationChecked,
    { privateMessages: false, mentions: false },
    { timeout: 5000 },
  );
  await firstNotifications.getByLabel('Private messages', { exact: true }).check();
  await otherMember.waitForFunction(
    notificationChecked,
    { privateMessages: true, mentions: false },
    { timeout: 5000 },
  );
  await secondNotifications.getByLabel('Mentions', { exact: true }).check();
  await member.waitForFunction(
    notificationChecked,
    { privateMessages: true, mentions: true },
    { timeout: 5000 },
  );
  await otherMember.waitForFunction(
    notificationChecked,
    { privateMessages: true, mentions: true },
    { timeout: 5000 },
  );
  await close(firstNotifications);
  await close(secondNotifications);
  await Promise.all([owner, member, otherMember].map(noMediaStartup));
  assert.ok(ownerId && memberId);
  return {
    homepagePm: true,
    liveEdit: true,
    liveReaction: true,
    crossDeviceRead: true,
    streamReconnect: true,
    homepagePreferenceSync: true,
    homepageNotificationFieldSync: true,
    deferredCallEngine: true,
  };
}

/** Back/Forward and reload restore read surfaces and drafts, never a submission. */
async function checkAccountNavigation(owner, { base, header, close, memberId }) {
  const roomInput = owner.locator('#room-input');
  const previousRoomInput = await roomInput.inputValue();
  await roomInput.fill('unsubmitted room / ?');
  const draftInbox = await header(owner, 'Messages');
  assert.equal(
    new URL(owner.url()).hash,
    '#view=inbox',
    'an unsubmitted form value is not a room destination',
  );
  await owner.goBack();
  await draftInbox.waitFor({ state: 'hidden' });
  assert.equal(await owner.locator('#join-screen').isVisible(), true);
  assert.equal(await roomInput.inputValue(), 'unsubmitted room / ?');
  await roomInput.fill(previousRoomInput);
  const inbox = await header(owner, 'Messages');
  await inbox.locator('.inbox-conversation').filter({ hasText: 'E2E Member' }).click();
  let conversation = owner.getByRole('dialog', { name: 'Messages with E2E Member', exact: true });
  await conversation.waitFor();
  await owner.goBack();
  await conversation.waitFor({ state: 'hidden' });
  await inbox.waitFor({ state: 'visible' });
  await owner.goForward();
  conversation = owner.getByRole('dialog', { name: 'Messages with E2E Member', exact: true });
  await conversation.waitFor();
  assert.equal(new URL(owner.url()).hash, `#view=conversation&peer=${memberId}`);
  const draft = 'Draft restored by account URL, never submitted';
  await conversation.getByLabel('Private message', { exact: true }).fill(draft);
  let sends = 0;
  const observe = (request) => {
    if (
      request.method() === 'POST' &&
      /\/api\/auth\/inbox\/[^/]+\/messages$/.test(new URL(request.url()).pathname)
    )
      sends++;
  };
  owner.on('request', observe);
  try {
    await owner.reload();
    const restored = owner.getByRole('dialog', { name: 'Messages with Contact', exact: true });
    await restored.waitFor();
    assert.equal(await restored.getByLabel('Private message', { exact: true }).inputValue(), draft);
    assert.equal(sends, 0, 'reload cannot replay a draft or a send attempt');
    assert.equal(await owner.locator('#join-screen').isVisible(), true);
    await noMediaStartup(owner);
    await restored.getByLabel('Private message', { exact: true }).fill('');
    await close(restored);
    // Supported fragments arriving in an already-open app retain their explicit offer step.
    await owner.evaluate((id) => {
      location.hash = `contact=${id}`;
    }, memberId);
    const offer = owner.getByRole('dialog', { name: 'Add contact', exact: true });
    await offer.getByRole('button', { name: 'Send request', exact: true }).waitFor();
    assert.equal(sends, 0);
    assert.equal(await owner.locator('#join-screen').isVisible(), true);
    await close(offer);
    await owner.goto(`${base}/#contact=${memberId}`);
    const coldOffer = owner.getByRole('dialog', { name: 'Add contact', exact: true });
    await coldOffer.getByRole('button', { name: 'Send request', exact: true }).waitFor();
    await close(coldOffer);
    await noMediaStartup(owner);
  } finally {
    owner.off('request', observe);
  }
  return {
    backForward: true,
    coldConversation: true,
    draftRestoredWithoutSend: true,
    unsubmittedRoomPreserved: true,
    liveContactOffer: true,
    coldContactOffer: true,
    noImplicitJoinOrCapture: true,
  };
}

/** Reject only the first call-engine request; explicit retry must preserve room chat. */
async function rejectFirstMediaChunk(page, { join, send, visible, ready }) {
  let rejected = 0;
  await page.route(
    mediaChunk,
    (route) => {
      rejected++;
      return route.abort();
    },
    { times: 1 },
  );
  return async () => {
    assert.equal(rejected, 1, 'the explicit room action requested the deferred engine');
    await page.getByText(/Call media could not be loaded\. Chat remains available\./).waitFor();
    assert.equal(await captureCount(page), 0);
    const beforeRetry = page.url();
    await openRoomMenu(page);
    await page.locator('#refresh-incoming-media').click();
    await page.waitForFunction(
      () =>
        document.getElementById('refresh-incoming-media')?.getAttribute('aria-busy') === 'false',
    );
    const success = page.getByText(
      'Incoming subscriptions refreshed. Your microphone and camera settings are unchanged.',
      { exact: true },
    );
    const nativeRetrySucceeded = await success.isVisible();
    assert.equal(await page.locator('#room-screen').isVisible(), true);
    assert.equal(
      page.url(),
      beforeRetry,
      'a failed import and retry cannot reload or leave the call',
    );
    assert.equal(await captureCount(page), 0);
    await send(page, 'Room chat survives an unavailable call engine');
    await visible(page, 'Room chat survives an unavailable call engine');
    if (!nativeRetrySucceeded) {
      await page
        .getByText(
          'Call media could not be loaded. Leave the call and reload to get the current app version, then join again.',
          { exact: true },
        )
        .waitFor();
      await page.locator('#leave-btn').click();
      await page.locator('#join-screen').waitFor({ state: 'visible' });
      await page.reload({ waitUntil: 'domcontentloaded' });
      await page.waitForFunction(ready);
      assert.equal(await page.locator('#join-screen').isVisible(), true);
      await noMediaStartup(page);
      await join(page, 'E2E Owner');
    }
    await page.waitForFunction(() => window.__communityPeers.length >= 2);
    assert.equal(await captureCount(page), 0, 'recovery never starts local capture');
    return {
      failedChunkPreservedRoom: true,
      explicitRetryAttempts: 1,
      retryOutcome: nativeRetrySucceeded ? 'successful' : 'browserRetainedFailure',
      explicitLeaveReloadJoin: !nativeRetrySucceeded,
      noAutomaticReloadOrCapture: true,
    };
  };
}

async function checkInRoomBackNavigation(page, { header, close, settingsScreen }) {
  const before = await captureCount(page);
  const roomId = await page.locator('#room-input').inputValue();
  const inbox = await header(page, 'Messages');
  await page.goBack();
  await inbox.waitFor({ state: 'hidden' });
  assert.equal(await page.locator('#room-screen').isVisible(), true);
  await page.goForward();
  await page.getByRole('dialog', { name: 'Messages', exact: true }).waitFor();
  await close(page.getByRole('dialog', { name: 'Messages', exact: true }));
  const privacy = await settingsScreen(page, 'Privacy');
  await page.goBack();
  await page
    .locator('#settings-sheet')
    .getByRole('heading', { name: 'Settings', exact: true })
    .waitFor();
  assert.equal(await page.locator('#room-screen').isVisible(), true);
  await page.goForward();
  await page
    .locator('#settings-sheet')
    .getByRole('heading', { name: 'Privacy', exact: true })
    .waitFor();
  await close(privacy);
  assert.equal(await page.locator('#room-input').inputValue(), roomId);
  assert.equal(await captureCount(page), before);
  return { callPreserved: true, settingsBackForward: true, noImplicitCapture: true };
}
module.exports = {
  checkHomepageAccountUpdates,
  checkAccountNavigation,
  rejectFirstMediaChunk,
  checkInRoomBackNavigation,
};

/** One hosted cold document under explicit Chromium throttling; no baseline inference. */
async function checkColdPhoneStartup({ browser, browserName, contextOptions, base, ready }) {
  if (browserName !== 'chromium')
    return { skipped: true, reason: 'CDP network/CPU measurement is Chromium-only.' };
  const context = await browser.newContext({
    ...contextOptions,
    viewport: { width: 390, height: 844 },
  });
  try {
    const page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    await cdp.send('Network.enable');
    await cdp.send('Network.emulateNetworkConditions', {
      offline: false,
      latency: 40,
      downloadThroughput: 1_500_000 / 8,
      uploadThroughput: 750_000 / 8,
      connectionType: 'cellular3g',
    });
    await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
    await page.addInitScript(() => {
      window.__navigationCaptureCalls = 0;
      const devices = navigator.mediaDevices;
      if (devices) {
        for (const name of ['getUserMedia', 'getDisplayMedia']) {
          if (typeof devices[name] === 'function')
            devices[name] = () => {
              window.__navigationCaptureCalls++;
              return Promise.reject(
                new Error('Capture is forbidden in the cold account startup check'),
              );
            };
        }
      }
    });
    let heavyRequests = 0;
    const errors = [];
    page.on('request', (request) => {
      if (mediaChunk.test(new URL(request.url()).pathname)) heavyRequests++;
    });
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(base, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(ready, null, { timeout: 30000 });
    const measurement = await page.evaluate(() => {
      const navigation = performance.getEntriesByType('navigation')[0];
      return {
        documentToReadyMs: Math.round(performance.now()),
        domContentLoadedToReadyMs: Math.round(
          Math.max(0, performance.now() - navigation.domContentLoadedEventEnd),
        ),
        captureRequests: window.__navigationCaptureCalls,
        assetTransferBytes: performance
          .getEntriesByType('resource')
          .filter((entry) => new URL(entry.name).pathname.startsWith('/assets/'))
          .reduce((total, entry) => total + entry.transferSize, 0),
      };
    });
    assert.equal(heavyRequests, 0, 'phone homepage readiness must not request the call engine');
    assert.equal(measurement.captureRequests, 0);
    assert.deepEqual(errors, []);
    return {
      browser: 'chromium',
      viewport: { width: 390, height: 844 },
      network: { downloadBitsPerSecond: 1500000, uploadBitsPerSecond: 750000, latencyMs: 40 },
      cpuSlowdown: 4,
      ...measurement,
      heavyChunkRequests: heavyRequests,
      readiness: 'conversation toolbar, Sign in available and signaling connected',
      scope:
        'One isolated cold hosted document; simulated viewport/network/CPU, not a physical phone or baseline comparison.',
    };
  } finally {
    await context.close();
  }
}
module.exports.checkColdPhoneStartup = checkColdPhoneStartup;
