const assert = require('node:assert/strict');
const uploadPath = '/api/auth/attachments';
const downloadPath = (id) => `${uploadPath}/${id}`;
const responseFor = (page, path, method = 'GET') =>
  page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === path &&
      response.request().method() === method &&
      response.ok(),
  );
const ready = (scope) =>
  scope
    .locator('.attachment-status')
    .filter({ hasText: /^Ready$/ })
    .waitFor();

async function imageBytes(page) {
  return page.evaluate(() => {
    const canvas = document.createElement('canvas');
    canvas.width = 2;
    canvas.height = 2;
    const context = canvas.getContext('2d');
    context.fillStyle = '#3399cc';
    context.fillRect(0, 0, 2, 2);
    return Array.from(atob(canvas.toDataURL('image/png').split(',')[1]), (character) =>
      character.charCodeAt(0),
    );
  });
}
async function checkDownload(response, size) {
  assert.equal(response.status(), 200);
  const headers = response.headers();
  assert.equal(headers['content-type'], 'image/png');
  assert.equal(headers['x-content-type-options'], 'nosniff');
  assert.match(headers['cache-control'], /no-store/);
  assert.equal(headers['content-security-policy'], "default-src 'none'; sandbox");
  assert.match(headers['content-disposition'], /^inline;/);
  assert.equal((await response.body()).length, size);
}
async function contactConversation(page, peerName, header) {
  const inbox = await header(page, 'Messages');
  await inbox.locator('.discovery-contacts > summary').click();
  await inbox
    .locator('.discovery-row')
    .filter({ hasText: peerName })
    .getByRole('button', { name: 'Message', exact: true })
    .click();
  return {
    inbox,
    conversation: page.getByRole('dialog', { name: `Messages with ${peerName}`, exact: true }),
  };
}

async function privateAttachmentChecks(owner, member, outsider, { base, header, close, deadline }) {
  // This owned fixture credential stays in process memory and never enters artifacts.
  const outsiderRequest = outsider.waitForRequest(
    (request) => new URL(request.url()).pathname === '/api/auth/contacts',
  );
  const outsiderInbox = await header(outsider, 'Messages');
  const outsiderAuthorization = (await outsiderRequest).headers().authorization;
  assert.ok(outsiderAuthorization?.startsWith('Bearer '));
  await close(outsiderInbox);
  const sender = await contactConversation(owner, 'E2E Member', header);
  const receiver = await contactConversation(member, 'E2E Owner', header);
  const bytes = await imageBytes(owner);
  const upload = responseFor(owner, uploadPath, 'POST');
  let firstRoute;
  let observe;
  const intercepted = new Promise((resolve) => {
    observe = resolve;
  });
  await owner.route(
    `**${uploadPath}`,
    (route) => {
      firstRoute = route;
      observe();
    },
    { times: 1 },
  );
  // Clipboard data is controlled; the retry sends actual bytes to the local server.
  await sender.conversation
    .getByLabel('Private message', { exact: true })
    .evaluate((input, value) => {
      const clipboardData = new DataTransfer();
      clipboardData.items.add(
        new File([new Uint8Array(value)], 'private-pasted.png', { type: 'image/png' }),
      );
      input.dispatchEvent(
        new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }),
      );
    }, bytes);
  await deadline(intercepted, 10000);
  await sender.conversation
    .locator('.attachment-pending .attachment-status')
    .filter({ hasText: /\d+%/ })
    .waitFor();
  await firstRoute.fulfill({ status: 503, json: { error: 'Owned transient upload fixture' } });
  await sender.conversation.getByRole('button', { name: 'Retry', exact: true }).click();
  const uploaded = await (await upload).json();
  assert.equal(uploaded.contentType, 'image/png');
  await ready(sender.conversation);
  await sender.conversation
    .getByLabel('Private message', { exact: true })
    .fill('Private attachment from an accepted contact');
  await sender.conversation.getByLabel('Private message', { exact: true }).press('Enter');
  await sender.conversation
    .locator('.message-attachment')
    .filter({ hasText: 'private-pasted.png' })
    .waitFor();
  await close(receiver.conversation);
  await receiver.inbox
    .locator('.discovery-row')
    .filter({ hasText: 'E2E Owner' })
    .getByRole('button', { name: 'Message', exact: true })
    .click();
  const visible = receiver.conversation
    .locator('.message-attachment')
    .filter({ hasText: 'private-pasted.png' });
  const downloaded = responseFor(member, downloadPath(uploaded.id));
  await visible.getByRole('button', { name: 'Preview', exact: true }).click();
  await checkDownload(await downloaded, bytes.length);
  await visible.locator('img').evaluate((image) => image.decode());
  assert.equal(await visible.locator('img').evaluate((image) => image.naturalWidth), 2);
  const denied = await outsider.request.get(`${base}${downloadPath(uploaded.id)}`, {
    headers: { Authorization: outsiderAuthorization },
  });
  assert.equal(denied.status(), 400, 'A third account cannot fetch a private attachment');
  assert.equal(await owner.locator('#join-screen').isVisible(), true);
  assert.equal(await member.locator('#join-screen').isVisible(), true);
  const viewport = member.viewportSize();
  await member.setViewportSize({ width: 390, height: 844 });
  const mobileUpload = responseFor(member, uploadPath, 'POST');
  await receiver.conversation.getByLabel('Choose attachments', { exact: true }).setInputFiles({
    name: 'mobile-note.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('Mobile file chooser fixture'),
  });
  const mobileFile = await (await mobileUpload).json();
  await ready(receiver.conversation);
  await receiver.conversation.getByLabel('Private message', { exact: true }).press('Enter');
  await receiver.conversation
    .locator('.message-attachment')
    .filter({ hasText: 'mobile-note.txt' })
    .waitFor();
  assert.equal(mobileFile.contentType, 'application/octet-stream');
  if (viewport) await member.setViewportSize(viewport);
  await close(receiver.conversation);
  await close(receiver.inbox);
  await close(sender.conversation);
  await close(sender.inbox);
}

async function publicAttachmentChecks(owner, guest, { base, leave, join, send }) {
  const bytes = await imageBytes(owner);
  const upload = responseFor(owner, uploadPath, 'POST');
  await owner.locator('#chat-input-row').evaluate((host, value) => {
    const dataTransfer = new DataTransfer();
    dataTransfer.items.add(
      new File([new Uint8Array(value)], 'public-drop.png', { type: 'image/png' }),
    );
    host.dispatchEvent(new DragEvent('drop', { dataTransfer, bubbles: true, cancelable: true }));
  }, bytes);
  const file = await (await upload).json();
  await ready(owner.locator('#chat-input-row'));
  await send(owner, 'Public attachment with retained history');
  const row = guest.locator('.message-attachment').filter({ hasText: 'public-drop.png' });
  const firstDownload = responseFor(guest, downloadPath(file.id));
  await row.getByRole('button', { name: 'Preview', exact: true }).click();
  const response = await firstDownload;
  await checkDownload(response, bytes.length);
  const roomAuthorization = response.request().headers().authorization;
  assert.ok(roomAuthorization?.startsWith('Attachment '));
  await leave(guest);
  const departed = await guest.request.get(`${base}${downloadPath(file.id)}`, {
    headers: { Authorization: roomAuthorization },
  });
  assert.equal(departed.status(), 400, 'Leaving retires the guest room grant');
  await join(guest, 'E2E Guest');
  const original = owner
    .locator('.chat-msg:not([data-message-id^="pending:"])')
    .filter({ hasText: 'Public attachment with retained history' });
  await original.waitFor();
  const messageId = await original.getAttribute('data-message-id');
  assert.ok(messageId);
  const ownRow = owner.locator(`.chat-msg[data-message-id="${messageId}"]`);
  const ownerDownload = responseFor(owner, downloadPath(file.id));
  await ownRow.getByRole('button', { name: 'Preview', exact: true }).click();
  const ownerAuthorization = (await ownerDownload).request().headers().authorization;
  await ownRow.hover();
  await ownRow.getByRole('button', { name: 'Remove message', exact: true }).click();
  const confirmation = owner.getByRole('dialog', { name: 'Remove message?', exact: true });
  await confirmation.getByRole('button', { name: 'Remove message', exact: true }).click();
  await confirmation.waitFor({ state: 'hidden' });
  const removed = await owner.request.get(`${base}${downloadPath(file.id)}`, {
    headers: { Authorization: ownerAuthorization },
  });
  assert.equal(removed.status(), 400, 'Removal invalidates an already issued attachment grant');
  await ownRow.locator('.msg-text').filter({ hasText: 'Message removed' }).waitFor();
  assert.equal(await ownRow.locator('.message-attachment').count(), 0);
}
module.exports = { privateAttachmentChecks, publicAttachmentChecks };
