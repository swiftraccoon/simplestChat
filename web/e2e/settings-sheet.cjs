/* Drives the settings sheet (`#settings-sheet`) the way a person does: it is opened by a
   control in the page, shows one list and one screen at a time, and closes from its header. */

const escapeRegExp = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** The sheet region, present in the room column or docked on the home page. */
const sheet = (page) => page.getByRole('region', { name: 'Settings', exact: true });

/** The list row of one section, matched by its label alone (the status text varies). */
const sheetRow = (page, label) =>
  sheet(page)
    .locator('button.sheet-row-nav')
    .filter({
      has: page.locator('.sheet-row-label', { hasText: new RegExp(`^${escapeRegExp(label)}$`) }),
    });

/** Returns to the root list from any depth; a screen that refuses to leave stops the climb. */
async function toSheetRoot(page) {
  const back = sheet(page).getByRole('button', { name: 'Back', exact: true });
  for (let depth = 0; depth < 4 && (await back.isVisible()); depth++) {
    const title = await sheet(page).getByRole('heading', { level: 2 }).textContent();
    await back.click();
    await page.waitForFunction((previous) => {
      const region = document.getElementById('settings-sheet');
      return (
        region.querySelector('h2')?.textContent !== previous ||
        region.querySelector('.sheet-back')?.hidden
      );
    }, title);
  }
}

/** Reaches one screen of the open sheet, switching segment when the row sits in the other one. */
async function sheetScreen(page, label) {
  const region = sheet(page);
  await region.waitFor({ state: 'visible' });
  const title = region.getByRole('heading', { level: 2 });
  if ((await title.textContent()) === label) return region;
  await toSheetRoot(page);
  if (!(await sheetRow(page, label).count())) {
    const switcher = region.getByRole('group', { name: 'Settings for', exact: true });
    if (await switcher.count()) await switcher.getByRole('button', { pressed: false }).click();
  }
  await sheetRow(page, label).click();
  await region.getByRole('heading', { level: 2, name: label, exact: true }).waitFor();
  return region;
}

/** Closes the sheet from its header and waits until it is gone. */
async function closeSheet(page) {
  const region = sheet(page);
  const previous = await page.evaluate(() => history.state?.simplestchatNavigation);
  await region.getByRole('button', { name: 'Close settings', exact: true }).click();
  await region.waitFor({ state: 'hidden' });
  if (typeof previous === 'number')
    await page.waitForFunction(
      (entry) => history.state?.simplestchatNavigation !== entry,
      previous,
    );
}

module.exports = { sheet, sheetRow, toSheetRoot, sheetScreen, closeSheet };
