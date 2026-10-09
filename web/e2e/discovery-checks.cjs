const assert = require('node:assert/strict');

/** Real account storage and message delivery, before either account joins a room. */
async function checkContactsBeforeRoom(owner, member, { base, header, close }) {
  const memberResponse = member.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === '/api/auth/contacts' &&
      response.request().method() === 'GET',
  );
  const memberInbox = await header(member, 'Messages');
  const { accountId } = await (await memberResponse).json();
  const ownerInbox = await header(owner, 'Messages');
  await ownerInbox.locator('.discovery-contacts > summary').click();
  await ownerInbox
    .getByLabel('Contact link', { exact: true })
    .fill(`${base}/#contact=${accountId}`);
  await ownerInbox.getByRole('button', { name: 'Send request', exact: true }).click();
  const waiting = ownerInbox.locator('.discovery-row').filter({ hasText: 'E2E Member' });
  await waiting.getByText('Waiting for acceptance', { exact: true }).waitFor();
  assert.equal(await waiting.getByRole('button', { name: 'Message', exact: true }).count(), 0);
  await memberInbox.locator('.discovery-contacts > summary').click();
  await memberInbox.getByRole('button', { name: 'Refresh contacts', exact: true }).click();
  const incoming = memberInbox.locator('.discovery-row').filter({ hasText: 'E2E Owner' });
  await incoming.getByRole('button', { name: 'Accept', exact: true }).click();
  await incoming.getByRole('button', { name: 'Message', exact: true }).waitFor();
  await ownerInbox.getByRole('button', { name: 'Refresh contacts', exact: true }).click();
  await waiting.getByRole('button', { name: 'Message', exact: true }).click();
  const sent = owner.getByRole('dialog', { name: 'Messages with E2E Member', exact: true });
  await sent.getByLabel('Private message', { exact: true }).fill('Contact hello before any room');
  await sent.getByLabel('Private message', { exact: true }).press('Enter');
  await sent.getByText('Contact hello before any room', { exact: true }).waitFor();
  assert.equal(
    await owner.locator('#join-screen').isVisible(),
    true,
    'Sending a contact PM does not join a room',
  );
  await incoming.getByRole('button', { name: 'Message', exact: true }).click();
  const received = member.getByRole('dialog', { name: 'Messages with E2E Owner', exact: true });
  await received.getByText('Contact hello before any room', { exact: true }).waitFor();
  assert.equal(await member.locator('#join-screen').isVisible(), true);
  await close(received);
  await close(memberInbox);
  await close(sent);
  await close(ownerInbox);
}

/** The account's second browser session sees the same favorites and accepted contacts. */
async function checkSavedRoomsAcrossDevices(owner, secondDevice, { header, close }) {
  const own = await header(owner, 'My rooms');
  const recent = own
    .locator('.discovery-saved-rooms .discovery-row')
    .filter({ hasText: 'E2E Community Room' });
  await recent.waitFor();
  await recent.getByRole('button', { name: 'Add to favorite rooms', exact: true }).click();
  await recent.getByRole('button', { name: 'Remove from favorite rooms', exact: true }).waitFor();
  const other = await header(secondDevice, 'My rooms');
  const shared = other
    .locator('.discovery-saved-rooms .discovery-row')
    .filter({ hasText: 'E2E Community Room' });
  await shared.getByRole('button', { name: 'Remove from favorite rooms', exact: true }).waitFor();
  assert.equal(
    await secondDevice.locator('#join-screen').isVisible(),
    true,
    'Reading synced recents does not auto-join',
  );
  await shared.getByRole('button', { name: 'Remove from favorite rooms', exact: true }).click();
  await shared.getByRole('button', { name: 'Add to favorite rooms', exact: true }).waitFor();
  await close(other);
  await close(own);
  const refreshed = await header(owner, 'My rooms');
  await refreshed
    .locator('.discovery-saved-rooms .discovery-row')
    .filter({ hasText: 'E2E Community Room' })
    .getByRole('button', { name: 'Add to favorite rooms', exact: true })
    .waitFor();
  await close(refreshed);
  const inbox = await header(secondDevice, 'Messages');
  await inbox.locator('.discovery-contacts > summary').click();
  await inbox
    .locator('.discovery-row')
    .filter({ hasText: 'E2E Member' })
    .getByRole('button', { name: 'Message', exact: true })
    .waitFor();
  await close(inbox);
}

module.exports = { checkContactsBeforeRoom, checkSavedRoomsAcrossDevices };
