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

async function rules(page, header) {
  const loaded = responseFor(page, 'GET');
  const dialog = await header(page, 'Account');
  await loaded;
  const section = dialog.locator('.notification-preferences');
  await section.getByRole('button', { name: 'Save notification rules', exact: true }).waitFor();
  return { dialog, section };
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
async function checkNotificationPreferences(owner, secondDevice, member, { header, close }) {
  const initial = await rules(owner, header);
  await initial.section.getByLabel('Private-message alerts', { exact: true }).uncheck();
  await initial.section.getByLabel('Mention alerts', { exact: true }).uncheck();
  await initial.section.getByLabel('Quiet hours every day', { exact: true }).check();
  await initial.section.getByLabel('From', { exact: true }).fill('22:00');
  await initial.section.getByLabel('Until', { exact: true }).fill('07:00');
  await initial.section.getByLabel('Time zone', { exact: true }).fill('America/New_York');
  const stored = responseFor(owner, 'PUT');
  await initial.section
    .getByRole('button', { name: 'Save notification rules', exact: true })
    .click();
  const saved = await (await stored).json();
  assert.deepEqual(saved.quietHours, {
    startMinute: 1320,
    endMinute: 420,
    timeZone: 'America/New_York',
  });
  await close(initial.dialog);
  const shared = await rules(secondDevice, header);
  assert.equal(
    await shared.section.getByLabel('Private-message alerts', { exact: true }).isChecked(),
    false,
  );
  assert.equal(
    await shared.section.getByLabel('Mention alerts', { exact: true }).isChecked(),
    false,
  );
  assert.equal(
    await shared.section.getByLabel('Quiet hours every day', { exact: true }).isChecked(),
    true,
  );
  assert.equal(
    await shared.section.getByLabel('Time zone', { exact: true }).inputValue(),
    'America/New_York',
  );
  await close(shared.dialog);
  const separate = await rules(member, header);
  assert.equal(
    await separate.section.getByLabel('Private-message alerts', { exact: true }).isChecked(),
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
  const restored = await rules(owner, header);
  await restored.section.getByLabel('Private-message alerts', { exact: true }).check();
  await restored.section.getByLabel('Mention alerts', { exact: true }).check();
  await restored.section.getByLabel('Quiet hours every day', { exact: true }).uncheck();
  const reset = responseFor(owner, 'PUT');
  await restored.section
    .getByRole('button', { name: 'Save notification rules', exact: true })
    .click();
  assert.equal((await (await reset).json()).quietHours, null);
  await close(restored.dialog);
}

module.exports = { checkNotificationPreferences };
