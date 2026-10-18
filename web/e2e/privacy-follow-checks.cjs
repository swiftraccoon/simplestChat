/** Real owned account routes and UI; called only by the disposable community suite. */
const assert = require('node:assert/strict');

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
module.exports = { checkAccountPrivacyAcrossDevices };
