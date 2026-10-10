const assert = require('node:assert/strict');
const path = '/api/auth/notification-preferences';
const responseFor = (page, method, conversation = false) =>
  page.waitForResponse((response) => {
    const pathname = new URL(response.url()).pathname;
    return (
      (conversation ? pathname.startsWith(`${path}/conversations/`) : pathname === path) &&
      response.request().method() === method &&
      response.ok()
    );
  });

/** The account rules live on the sheet's Notifications screen and save as each row changes. */
async function rules(page, settingsScreen) {
  const loaded = responseFor(page, 'GET');
  const dialog = await settingsScreen(page, 'Notifications');
  await loaded;
  const section = dialog.locator('.notification-preferences');
  await section.getByLabel('Quiet hours every day', { exact: true }).waitFor();
  return { dialog, section };
}
/** Performs one change and returns the policy the server stored for it. */
async function stored(page, action) {
  const response = responseFor(page, 'PUT');
  await action();
  return (await response).json();
}
async function conversation(page, peer, header) {
  const inbox = await header(page, 'Messages');
  await inbox.locator('.inbox-conversation').filter({ hasText: peer }).click();
  const dialog = page.getByRole('dialog', { name: `Messages with ${peer}`, exact: true });
  const controls = dialog.locator('.notification-conversation');
  await controls.locator('summary').click();
  await controls.getByRole('button', { name: /^(Mute|Unmute)$/ }).waitFor();
  return { inbox, dialog, controls };
}

/** Actual account policy and PM storage, shared by two independent browser sessions. */
async function checkNotificationPreferences(
  owner,
  secondDevice,
  member,
  { header, close, settingsScreen },
) {
  const initial = await rules(owner, settingsScreen);
  const field = async (label, value) => {
    const input = initial.section.getByLabel(label, { exact: true });
    await input.fill(value);
    await input.press('Tab');
  };
  await stored(owner, () =>
    initial.section.getByLabel('Private messages', { exact: true }).uncheck(),
  );
  await stored(owner, () => initial.section.getByLabel('Mentions', { exact: true }).uncheck());
  const enabled = await stored(owner, () =>
    initial.section.getByLabel('Quiet hours every day', { exact: true }).check(),
  );
  assert.deepEqual(
    [enabled.quietHours.startMinute, enabled.quietHours.endMinute],
    [1320, 420],
    'quiet hours start with a 22:00 to 07:00 window',
  );
  // Each value differs from the defaults (and from any machine's local zone), so every
  // field change is a real change that the screen persists.
  await stored(owner, () => field('From', '23:00'));
  await stored(owner, () => field('Until', '06:00'));
  const saved = await stored(owner, () => field('Time zone', 'Pacific/Auckland'));
  assert.deepEqual(saved.quietHours, {
    startMinute: 1380,
    endMinute: 360,
    timeZone: 'Pacific/Auckland',
  });
  await close(initial.dialog);
  const shared = await rules(secondDevice, settingsScreen);
  assert.equal(
    await shared.section.getByLabel('Private messages', { exact: true }).isChecked(),
    false,
  );
  assert.equal(await shared.section.getByLabel('Mentions', { exact: true }).isChecked(), false);
  assert.equal(
    await shared.section.getByLabel('Quiet hours every day', { exact: true }).isChecked(),
    true,
  );
  assert.equal(
    await shared.section.getByLabel('Time zone', { exact: true }).inputValue(),
    'Pacific/Auckland',
  );
  await close(shared.dialog);
  const separate = await rules(member, settingsScreen);
  assert.equal(
    await separate.section.getByLabel('Private messages', { exact: true }).isChecked(),
    true,
  );
  assert.equal(
    await separate.section.getByLabel('Quiet hours every day', { exact: true }).isChecked(),
    false,
  );
  await close(separate.dialog);
  const own = await conversation(owner, 'E2E Member', header);
  const muted = responseFor(owner, 'PUT', true);
  await own.controls.getByRole('button', { name: 'Mute', exact: true }).click();
  assert.equal((await (await muted).json()).conversations[0].muted, true);
  await close(own.dialog);
  await close(own.inbox);
  const sender = await conversation(member, 'E2E Owner', header);
  const text = 'Muted alerts still deliver this private message';
  await sender.dialog.getByLabel('Private message', { exact: true }).fill(text);
  await sender.dialog.getByLabel('Private message', { exact: true }).press('Enter');
  await sender.dialog.getByText(text, { exact: true }).waitFor();
  await close(sender.dialog);
  await close(sender.inbox);
  const unread = owner.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === '/api/auth/inbox' &&
      response.request().method() === 'GET' &&
      response.ok(),
  );
  const inbox = await header(owner, 'Messages');
  const unreadState = await (await unread).json();
  const row = unreadState.conversations.find((entry) => entry.lastMessage.content === text);
  assert.ok(
    row && row.unreadCount >= 1,
    'Muting notifications preserves message delivery and unread state',
  );
  await close(inbox);
  const other = await conversation(secondDevice, 'E2E Member', header);
  await other.controls.getByRole('button', { name: 'Unmute', exact: true }).waitFor();
  await other.dialog.getByText(text, { exact: true }).waitFor();
  const snoozed = responseFor(secondDevice, 'PUT', true);
  await other.controls.getByRole('button', { name: 'Snooze 1 hour', exact: true }).click();
  const policy = (await (await snoozed).json()).conversations[0];
  assert.equal(policy.muted, false);
  assert.ok(Date.parse(policy.snoozedUntil) > Date.now() + 3500000);
  await close(other.dialog);
  await close(other.inbox);
  const reopened = await conversation(owner, 'E2E Member', header);
  await reopened.controls.getByText(/^Snoozed until /).waitFor();
  const resumed = responseFor(owner, 'PUT', true);
  await reopened.controls.getByRole('button', { name: 'Resume alerts', exact: true }).click();
  assert.equal((await (await resumed).json()).conversations.length, 0);
  await close(reopened.dialog);
  await close(reopened.inbox);
  const restored = await rules(owner, settingsScreen);
  await stored(owner, () =>
    restored.section.getByLabel('Private messages', { exact: true }).check(),
  );
  await stored(owner, () => restored.section.getByLabel('Mentions', { exact: true }).check());
  const reset = await stored(owner, () =>
    restored.section.getByLabel('Quiet hours every day', { exact: true }).uncheck(),
  );
  assert.equal(reset.quietHours, null);
  await close(restored.dialog);
}

module.exports = { checkNotificationPreferences };
