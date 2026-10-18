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
  const before = await page.evaluate(() => ({
    entry: history.state?.simplestchatNavigation,
    session: history.state?.simplestchatNavigationSession,
  }));
  const observation = await page.evaluateHandle(() => {
    const region = document.getElementById('settings-sheet');
    let transitions = 0;
    const observer = new MutationObserver((records) => {
      transitions = Math.min(8, transitions + records.length);
    });
    if (region) observer.observe(region, { attributes: true, attributeFilter: ['hidden'] });
    return {
      stop() {
        transitions = Math.min(8, transitions + observer.takeRecords().length);
        observer.disconnect();
        return {
          hiddenTransitions: transitions,
          lastHidden: region ? region.hidden === true : null,
        };
      },
    };
  });
  let phase = 'click';
  try {
    await region.getByRole('button', { name: 'Close settings', exact: true }).click();
    phase = 'hidden';
    await region.waitFor({ state: 'hidden' });
    phase = 'history';
    if (typeof before.entry === 'number')
      await page.waitForFunction(
        (entry) => history.state?.simplestchatNavigation !== entry,
        before.entry,
      );
  } catch (error) {
    // The account suite contains real secrets. Emit fixed labels and structural
    // navigation state only; never pass through DOM, input values or errors.
    const state = await page
      .evaluate((previous) => {
        const titles = {
          Settings: 'root',
          Profile: 'profile',
          'Sign-in & security': 'security',
          Invites: 'invites',
          Privacy: 'privacy',
          Notifications: 'notifications',
          'Camera & mic': 'camera',
          Quality: 'quality',
          'Screen sharing': 'screen-sharing',
          'Chat look': 'chat-look',
          'Who can enter': 'room-access',
          'Chat & broadcasting': 'room-participation',
          People: 'room-people',
          Bans: 'room-bans',
          Reports: 'room-reports',
          History: 'room-history',
          'Waiting room': 'room-lobby',
        };
        const params = new URLSearchParams(location.hash.slice(1));
        const region = document.getElementById('settings-sheet');
        const heading = region?.querySelector('h2')?.textContent;
        const section = params.get('section');
        const current = history.state;
        const buttons = [...(region?.querySelectorAll('button') ?? [])];
        const control = (label) => buttons.find((button) => button.textContent === label);
        const visible = (node) => !!node && node.getClientRects().length > 0;
        const add = control('Add passkey');
        return {
          sheetHidden: region ? region.hidden === true : null,
          sheetSection: Object.hasOwn(titles, heading) ? titles[heading] : 'other',
          historyEntry: Number.isSafeInteger(current?.simplestchatNavigation)
            ? current.simplestchatNavigation
            : null,
          sameHistoryEntry: current?.simplestchatNavigation === previous.entry,
          sameHistorySession: current?.simplestchatNavigationSession === previous.session,
          securityControls: {
            addVisible: visible(add),
            addEnabled: !!add && !add.disabled,
            createVisible: visible(control('Create passkey')),
            reloadVisible: visible(control('Reload to check account')),
          },
          route: {
            view: ['settings', 'inbox', 'conversation'].includes(params.get('view'))
              ? params.get('view')
              : null,
            section: Object.values(titles).includes(section) ? section : null,
            segment: ['you', 'room'].includes(params.get('segment')) ? params.get('segment') : null,
          },
        };
      }, before)
      .catch(() => ({ unavailable: true }));
    const transitions = await observation
      .evaluate((value) => value.stop())
      .catch(() => ({ unavailable: true }));
    console.error(JSON.stringify({ kind: 'settings-close', phase, ...state, ...transitions }));
    throw error;
  } finally {
    await observation.evaluate((value) => value.stop()).catch(() => {});
    await observation.dispose().catch(() => {});
  }
}

module.exports = { sheet, sheetRow, toSheetRoot, sheetScreen, closeSheet };
