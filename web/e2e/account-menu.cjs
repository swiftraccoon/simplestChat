/* Drives the signed-in header's account menu: an avatar button that opens Account, Messages,
   My rooms and Sign out on the home page. Inside a room those items sit flat in the More menu. */

/** Opens the account menu once its button appears; inside a room the items are already flat. */
async function openAccountMenu(page) {
  if (await page.locator('#room-screen').isVisible()) return;
  const toggle = page.locator('#account-menu-btn');
  await toggle.waitFor({ state: 'visible' });
  if ((await toggle.getAttribute('aria-expanded')) !== 'true') await toggle.click();
  await page.locator('#account-menu').waitFor({ state: 'visible' });
}

/** The menu's button with this accessible name, with the menu opened first on the home page. */
async function accountMenuButton(page, name) {
  await openAccountMenu(page);
  return page.locator('#account-menu').getByRole('button', { name, exact: true });
}

/** Signs out from the account menu. */
async function signOut(page) {
  await openAccountMenu(page);
  await page.locator('#logout-btn').click();
}

module.exports = { openAccountMenu, accountMenuButton, signOut };
