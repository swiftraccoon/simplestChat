/** Shared real-browser checks against rendered room and chat participants. */
const assert = require('node:assert/strict');

const cardSelector = '.participant-hovercard';

async function withinViewport(card) {
  const bounds = await card.evaluate((node) => {
    const rect = node.getBoundingClientRect();
    return {
      left: rect.left,
      top: rect.top,
      right: rect.right,
      bottom: rect.bottom,
      width: innerWidth,
      height: innerHeight,
    };
  });
  assert.ok(
    bounds.left >= 0 &&
      bounds.top >= 0 &&
      bounds.right <= bounds.width + 1 &&
      bounds.bottom <= bounds.height + 1,
    `Participant hovercard stays within the viewport: ${JSON.stringify(bounds)}`,
  );
}

async function participantHovercardChecks(page, { name, message }) {
  await page.locator('.toast').evaluateAll((nodes) => nodes.forEach((node) => node.click()));
  await page.locator('.toast:visible').first().waitFor({ state: 'hidden' });
  const roster = page
    .locator('#classic-users-panel [data-participant-hovercard]')
    .filter({ hasText: name })
    .first();
  const chat = page
    .locator('.chat-msg')
    .filter({ hasText: message })
    .locator('[data-participant-hovercard]')
    .first();
  await roster.waitFor({ state: 'visible' });
  await chat.waitFor({ state: 'visible' });
  const participantId = await roster.getAttribute('data-participant-hovercard');
  assert.ok(participantId);
  assert.equal(await chat.getAttribute('data-participant-hovercard'), participantId);

  const card = page.locator(cardSelector);
  await page.locator('#chat-input').focus();
  await roster.hover();
  await card.waitFor({ state: 'visible' });
  assert.equal(await card.locator('.participant-hovercard-name').textContent(), name);
  assert.equal(await roster.getAttribute('aria-expanded'), 'true');
  assert.equal(
    await page.locator('#chat-input').evaluate((node) => node === document.activeElement),
    true,
    'Hovering a participant does not steal focus from the composer',
  );
  await withinViewport(card);
  await card.hover();
  // Remain past the dismissal grace period to prove the card is interactive.
  await page.waitForTimeout(250);
  assert.equal(await card.isVisible(), true, 'Pointer can move from the name into its card');
  await page.keyboard.press('Escape');
  await card.waitFor({ state: 'hidden' });
  assert.equal(await roster.getAttribute('aria-expanded'), 'false');

  await chat.hover();
  await card.waitFor({ state: 'visible' });
  assert.equal(await card.locator('.participant-hovercard-name').textContent(), name);
  assert.equal(await page.locator(`${cardSelector}:visible`).count(), 1);
  assert.equal(await chat.getAttribute('aria-expanded'), 'true');
  await withinViewport(card);
  await page.keyboard.press('Escape');
  await card.waitFor({ state: 'hidden' });

  await page.mouse.move(0, 0);
  await chat.focus();
  await card.waitFor({ state: 'visible' });
  await page.keyboard.press('Tab');
  assert.equal(
    await card.evaluate((node) => node.contains(document.activeElement)),
    true,
    'Tab from the participant name reaches its card actions',
  );
  await page.keyboard.press('Escape');
  await card.waitFor({ state: 'hidden' });
  assert.equal(
    await chat.evaluate((node) => node === document.activeElement),
    true,
    'Escape returns keyboard focus to the participant name',
  );
  await chat.press('Enter');
  await card.waitFor({ state: 'visible' });
  await page.keyboard.press('Tab');
  await page.keyboard.press('Shift+Tab');
  await card.waitFor({ state: 'hidden' });
  assert.equal(
    await chat.evaluate((node) => node === document.activeElement),
    true,
    'Shift+Tab from the first action returns to the participant name',
  );
  await chat.press('Enter');
  await card.waitFor({ state: 'visible' });
  const actionCount = await card.locator('button').count();
  for (let index = 0; index < actionCount; index++) await page.keyboard.press('Tab');
  assert.equal(
    await card
      .locator('button')
      .last()
      .evaluate((node) => node === document.activeElement),
    true,
  );
  await page.keyboard.press('Tab');
  await card.waitFor({ state: 'hidden' });
  assert.equal(
    await chat.evaluate(
      (node) =>
        document.activeElement !== document.body &&
        document.activeElement !== node &&
        !!(
          node.compareDocumentPosition(document.activeElement) & Node.DOCUMENT_POSITION_FOLLOWING
        ) &&
        !!document.activeElement.closest('#room-screen'),
    ),
    true,
    'Tab from the last action continues forward from the participant name',
  );

  await page.locator('#chat-input').focus();
  await chat.click();
  await card.waitFor({ state: 'visible' });
  await card.getByRole('button', { name: 'Message', exact: true }).click();
  await page
    .locator(`.conversation-tab[data-conversation-id="${participantId}"][aria-pressed="true"]`)
    .waitFor({ state: 'visible' });
  await card.waitFor({ state: 'hidden' });
  await page.locator('.conversation-tab[data-conversation-id="public"]').click();

  await chat.click();
  await card.waitFor({ state: 'visible' });
  await card.getByRole('button', { name: 'More', exact: true }).click();
  const menu = page.locator('#mod-menu');
  await menu.waitFor({ state: 'visible' });
  assert.equal(await menu.locator('.mod-menu-target-name').textContent(), name);
  assert.ok(
    (await menu.locator('.mod-menu-target').textContent()).includes(participantId.slice(0, 8)),
  );
  await card.waitFor({ state: 'hidden' });
  await page.keyboard.press('Escape');
  await menu.waitFor({ state: 'hidden' });

  await chat.click();
  await card.waitFor({ state: 'visible' });
  await page.locator('#chat-input').click();
  await card.waitFor({ state: 'hidden' });
}

async function participantHovercardTouchChecks(page, { message, screenshot }) {
  const sender = page
    .locator('.chat-msg')
    .filter({ hasText: message })
    .locator('[data-participant-hovercard]')
    .first();
  const card = page.locator(cardSelector);
  await sender.tap();
  await card.waitFor({ state: 'visible' });
  await withinViewport(card);
  assert.equal(await page.locator('#mod-menu').isVisible(), false);
  const messageAction = card.getByRole('button', { name: 'Message', exact: true });
  await messageAction.tap({ trial: true });
  if (screenshot) await page.screenshot({ path: screenshot });
  await page.locator('#chat-input').tap();
  await card.waitFor({ state: 'hidden' });
}

module.exports = {
  participantHovercardChecks,
  participantHovercardTouchChecks,
  withinViewport,
};
