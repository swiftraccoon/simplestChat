/** Real owned account routes and UI; called only by the disposable community suite. */
const assert = require('node:assert/strict');
const { openRoomMenu } = require('./room-menu.cjs');

const preferencePath = '/api/auth/preferences';
const responseFor = (page, method) =>
  page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === preferencePath &&
      response.request().method() === method &&
      response.ok(),
  );

async function stored(page, action) {
  const response = responseFor(page, 'PATCH');
  await action();
  return (await response).json();
}

function privacyReady(checked) {
  const input = Array.from(document.querySelectorAll('input[role="switch"]')).find(
    (node) =>
      document.getElementById(node.getAttribute('aria-labelledby'))?.textContent ===
      'Private messages from others',
  );
  return input && !input.disabled && (checked === null || input.checked === checked);
}

async function privacy(page, settingsScreen) {
  const response = responseFor(page, 'GET');
  const region = await settingsScreen(page, 'Privacy');
  const state = await (await response).json();
  const allow = region.getByLabel('Private messages from others', { exact: true });
  await allow.waitFor({ state: 'visible' });
  await page.waitForFunction(privacyReady, null);
  return { region, allow, state };
}

/** Two independently signed-in sessions retain different field intents, then
 * the homepage recovers a genuinely committed PATCH whose response was lost. */
async function checkAccountPrivacyAcrossDevices(
  owner,
  secondDevice,
  { settingsScreen, header, close, peerName },
) {
  const home = await privacy(secondDevice, settingsScreen);
  assert.equal(home.state.allowPrivateMessages, true);
  const look = await settingsScreen(owner, 'Chat look');
  const large = look.getByLabel('Larger chat text', { exact: true });
  const originalLarge = await large.isChecked();
  assert.equal(originalLarge, false, 'the owned account starts with ordinary chat text');
  await Promise.all([
    stored(owner, () => large.check()),
    stored(secondDevice, () => home.allow.uncheck()),
  ]);
  await close(look);
  const roomPolicy = await privacy(owner, settingsScreen);
  assert.equal(roomPolicy.state.allowPrivateMessages, false);
  assert.equal(roomPolicy.state.largeText, true, 'a concurrent appearance PATCH preserves opt-out');
  await close(roomPolicy.region);
  await close(home.region);

  const inbox = await header(secondDevice, 'Messages');
  await inbox.locator('.inbox-conversation').filter({ hasText: peerName }).click();
  const conversation = secondDevice.getByRole('dialog', {
    name: `Messages with ${peerName}`,
    exact: true,
  });
  const ignored = await stored(secondDevice, () =>
    conversation.getByRole('button', { name: 'Ignore this person', exact: true }).click(),
  );
  assert.equal(
    ignored.ignored.some((person) => person.name === peerName),
    true,
  );
  assert.equal(ignored.allowPrivateMessages, false);
  assert.equal(ignored.largeText, true);
  await close(conversation);
  await close(inbox);

  let refreshed = await privacy(secondDevice, settingsScreen);
  await refreshed.region.getByText(peerName, { exact: true }).waitFor();
  await stored(secondDevice, () => refreshed.allow.check());
  let committed = 0;
  let patches = 0;
  const intercept = async (route) => {
    if (route.request().method() !== 'PATCH') {
      await route.continue();
      return;
    }
    patches++;
    const response = await route.fetch({ timeout: 10000, maxRetries: 0 });
    assert.equal(response.ok(), true, 'the real server commits before response loss');
    committed++;
    await route.abort('failed');
  };
  await secondDevice.route('**/api/auth/preferences', intercept);
  try {
    await refreshed.allow.uncheck();
    await refreshed.region.getByText(/This change was not confirmed/).waitFor();
    assert.equal(await refreshed.allow.isChecked(), false, 'restrictive intent remains visible');
    assert.equal(
      await refreshed.allow.isDisabled(),
      true,
      'uncertain policy cannot be overwritten',
    );
    assert.equal(committed, 1);
    await close(refreshed.region);
    refreshed = { ...refreshed, region: await settingsScreen(secondDevice, 'Privacy') };
    await refreshed.region.getByRole('button', { name: 'Refresh privacy', exact: true }).waitFor();
    assert.equal(patches, 1, 'reopening privacy never resubmits an uncertain mutation');
    const reloaded = responseFor(secondDevice, 'GET');
    await refreshed.region.getByRole('button', { name: 'Refresh privacy', exact: true }).click();
    const state = await (await reloaded).json();
    assert.equal(state.allowPrivateMessages, false);
    assert.equal(state.largeText, true);
    assert.equal(
      state.ignored.some((person) => person.name === peerName),
      true,
    );
    await secondDevice.waitForFunction(privacyReady, false);
    assert.equal(patches, 1, 'explicit recovery reads and synchronizes without replaying PATCH');
  } finally {
    await secondDevice.unroute('**/api/auth/preferences', intercept);
  }
  await stored(secondDevice, () =>
    refreshed.region.getByRole('button', { name: 'Unignore', exact: true }).click(),
  );
  await stored(secondDevice, () =>
    refreshed.region.getByLabel('Private messages from others', { exact: true }).check(),
  );
  await close(refreshed.region);
  const restored = await privacy(owner, settingsScreen);
  assert.equal(restored.state.allowPrivateMessages, true);
  assert.equal(restored.state.ignored.length, 0);
  await close(restored.region);
  const originalLook = await settingsScreen(owner, 'Chat look');
  await stored(owner, () =>
    originalLook.getByLabel('Larger chat text', { exact: true }).setChecked(originalLarge),
  );
  await close(originalLook);
  return { concurrentFieldIntents: true, homepageIgnore: true, committedResponseLoss: true };
}

/** The control works at phone size and survives a new same-origin document;
 * these actions cannot capture either device or create a room membership. */
async function checkFollowSpeakerPreference(owner, { base }) {
  const captures = await owner.evaluate(() => window.__communityCaptureRequests);
  const viewport = owner.viewportSize();
  let probe;
  try {
    await openRoomMenu(owner);
    const follow = owner.getByRole('checkbox', { name: 'Follow speaker', exact: true });
    assert.equal(await follow.isChecked(), false, 'following is optional and starts off');
    await follow.check();
    assert.equal(
      await owner.evaluate(() => localStorage.getItem('simplestchat.followSpeaker')),
      'true',
    );
    await owner.keyboard.press('Escape');
    await owner.setViewportSize({ width: 390, height: 844 });
    await openRoomMenu(owner);
    await follow.focus();
    await follow.scrollIntoViewIfNeeded();
    const box = await follow.boundingBox();
    assert.ok(
      box && box.x >= 0 && box.y >= 0 && box.x + box.width <= 391 && box.y + box.height <= 845,
    );
    await follow.press('Space');
    assert.equal(
      await follow.isChecked(),
      false,
      'the phone-sized control works from the keyboard',
    );
    await follow.check();
    await owner.keyboard.press('Escape');
    probe = await owner.context().newPage();
    let errors = 0;
    probe.on('pageerror', () => errors++);
    await probe.addInitScript(() => {
      let captures = 0;
      for (const name of ['getUserMedia', 'getDisplayMedia']) {
        const capture = navigator.mediaDevices[name]?.bind(navigator.mediaDevices);
        if (capture)
          navigator.mediaDevices[name] = (...args) => {
            captures++;
            return capture(...args);
          };
      }
      window.__followPreferenceCaptures = () => captures;
    });
    await probe.goto(new URL('/', base).href);
    const persisted = probe.locator('#room-volume-slot input[aria-label="Follow speaker"]');
    await persisted.waitFor({ state: 'attached' });
    assert.equal(await persisted.isChecked(), true);
    await probe.reload();
    await persisted.waitFor({ state: 'attached' });
    assert.equal(await persisted.isChecked(), true, 'a new document restores the opt-in');
    assert.equal(await probe.evaluate(() => window.__followPreferenceCaptures()), 0);
    assert.equal(
      await probe.locator('#join-screen').isVisible(),
      true,
      'the new tab remains outside a room',
    );
    assert.equal(errors, 0);
    assert.equal(await owner.evaluate(() => window.__communityCaptureRequests), captures);
  } finally {
    await probe?.close();
    if (viewport) await owner.setViewportSize(viewport);
    await openRoomMenu(owner);
    await owner.getByRole('checkbox', { name: 'Follow speaker', exact: true }).uncheck();
    await owner.keyboard.press('Escape');
  }
  return { defaultOff: true, phoneKeyboardControl: true, persisted: true, noCapture: true };
}

module.exports = { checkAccountPrivacyAcrossDevices, checkFollowSpeakerPreference };
