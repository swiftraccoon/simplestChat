/** Reveal infrequent joined-room tools while keeping homepage actions directly reachable. */
async function openRoomMenu(page) {
  const more = page.locator('#room-more-btn');
  if ((await more.isVisible()) && (await more.getAttribute('aria-expanded')) !== 'true')
    await more.click();
}

module.exports = { openRoomMenu };
