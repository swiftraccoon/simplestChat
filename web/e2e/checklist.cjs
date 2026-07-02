/* E2E verification of docs/plans/2026-02-18-client-completion-plan.md checklist (17 items)
 * Drives two Chromium instances (A = registered owner, B = guest) against the
 * deployed server via SSH tunnel at http://localhost:3100.
 */
const { chromium } = require('playwright');
const fs = require('fs');

const BASE = 'http://localhost:3100';
const TS = Date.now().toString(36);
const MAIN = `e2e-main-${TS}`;
const MOD = `e2e-mod-${TS}`;
const PW = `e2e-pw-${TS}`;
const PW_SECRET = 'sekret-room-123';
const ADHOC = `e2e-adhoc-${TS}`;
const EMAIL = `e2e-${TS}@test.local`;
const PASS = 'SuperSecret123!';
const ADMIN_NAME = 'AdminE2E';
const GUEST_NAME = 'GuestBob';
const ART = __dirname + '/artifacts';
fs.mkdirSync(ART, { recursive: true });

const results = [];
let pageA, pageB, dlgA, dlgB;

async function check(id, name, fn) {
  try {
    await fn();
    results.push({ id, name, pass: true });
    console.log(`  PASS  [${id}] ${name}`);
  } catch (e) {
    results.push({ id, name, pass: false, err: String(e).split('\n')[0].slice(0, 250) });
    console.log(`  FAIL  [${id}] ${name}\n        ${String(e).split('\n')[0]}`);
    try {
      await pageA?.screenshot({ path: `${ART}/${id}-A.png` });
      await pageB?.screenshot({ path: `${ART}/${id}-B.png` });
    } catch {}
  }
}

function note(msg) { console.log(`  NOTE  ${msg}`); results.push({ id: 'note', name: msg, pass: null }); }

async function waitFor(page, fn, desc, timeout = 12000) {
  const t0 = Date.now();
  let lastErr;
  while (Date.now() - t0 < timeout) {
    try { if (await fn()) return; } catch (e) { lastErr = e; }
    await page.waitForTimeout(200);
  }
  throw new Error(`timeout waiting for: ${desc}${lastErr ? ' (' + lastErr + ')' : ''}`);
}

function attachDialogs(page) {
  const rec = { messages: [], promptQueue: [] };
  page.on('dialog', async (d) => {
    rec.messages.push({ type: d.type(), message: d.message() });
    if (d.type() === 'prompt') {
      const v = rec.promptQueue.length ? rec.promptQueue.shift() : d.defaultValue();
      await d.accept(v);
    } else {
      await d.accept();
    }
  });
  return rec;
}

async function launch() {
  return chromium.launch({
    headless: true,
    args: [
      '--use-fake-device-for-media-stream',
      '--use-fake-ui-for-media-stream',
      '--autoplay-policy=no-user-gesture-required',
    ],
  });
}

const vis = (page, sel) => page.locator(sel).isVisible();
const hidden = (page, sel) => page.locator(sel).isHidden();

async function waitConnected(page) {
  await waitFor(page, () => page.locator('#connection-status.connected').isVisible(), 'WS connected');
}

async function ensureJoinScreen(page) {
  if (await page.locator('#room-screen:not([hidden])').count() > 0) {
    await page.click('#leave-btn');
  }
  await waitFor(page, () => vis(page, '#join-screen'), 'back at join screen');
}

async function joinRoom(page, name, roomId) {
  await page.fill('#name-input', name);
  await page.fill('#room-input', roomId);
  await waitFor(page, () => page.locator('#join-btn:not([disabled])').count().then(c => c > 0), 'join enabled');
  await page.click('#join-btn');
}

async function rightClickParticipant(page, name) {
  // ensure Users tab active
  await page.click('.tab[data-tab="users"]');
  const li = page.locator(`#participant-list li:has-text("${name}")`).first();
  await li.click({ button: 'right' });
  await waitFor(page, () => vis(page, '#mod-menu'), 'mod menu open', 5000);
}

async function modMenuButtons(page) {
  const direct = await page.locator('#mod-menu > button').allTextContents();
  const roleOpts = await page.locator('#mod-menu .mod-menu-group button').allTextContents();
  const hasRoleGroup = await page.locator('#mod-menu .mod-menu-label').count() > 0;
  return { direct, roleOpts, hasRoleGroup };
}

async function closeModMenu(page) {
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
}

async function remoteVideoPlaying(page, name) {
  const sel = `.video-tile:has(.name-tag:text-is("${name}")) video`;
  await waitFor(page, () => page.locator(sel).count().then(c => c > 0), `video element for ${name}`, 15000);
  await waitFor(page, async () => {
    const w = await page.locator(sel).first().evaluate(v => v.videoWidth);
    return w > 0;
  }, `videoWidth > 0 for ${name}`, 15000);
}

(async () => {
  console.log(`E2E run ${TS} against ${BASE}`);
  const browserA = await launch();
  const browserB = await launch();
  pageA = await browserA.newPage();
  pageB = await browserB.newPage();
  dlgA = attachDialogs(pageA);
  dlgB = attachDialogs(pageB);
  const consA = fs.createWriteStream(`${ART}/console-A.log`);
  const consB = fs.createWriteStream(`${ART}/console-B.log`);
  pageA.on('console', m => consA.write(`${new Date().toISOString()} [${m.type()}] ${m.text()}\n`));
  pageB.on('console', m => consB.write(`${new Date().toISOString()} [${m.type()}] ${m.text()}\n`));
  pageA.on('pageerror', e => consA.write(`PAGEERROR ${e}\n`));
  pageB.on('pageerror', e => consB.write(`PAGEERROR ${e}\n`));
  pageA.setDefaultTimeout(12000);
  pageB.setDefaultTimeout(12000);

  // ---------- Item 1: guest join ----------
  await check(1, 'Guest mode: join screen with name + room input works', async () => {
    await pageB.goto(BASE);
    await waitConnected(pageB);
    await joinRoom(pageB, GUEST_NAME, ADHOC);
    await waitFor(pageB, () => vis(pageB, '#room-screen'), 'room screen');
    if (!(await hidden(pageB, '#join-screen'))) throw new Error('join screen still visible');
    await pageB.click('#leave-btn');
    await waitFor(pageB, () => vis(pageB, '#join-screen'), 'back at join screen');
  });

  // ---------- Item 2: sign-in link ----------
  await check(2, 'Sign in link opens login modal', async () => {
    await pageA.goto(BASE);
    await waitConnected(pageA);
    await pageA.click('#sign-in-btn');
    await waitFor(pageA, () => vis(pageA, '#login-modal'), 'login modal');
  });

  // ---------- Item 3: register ----------
  await check(3, 'Register creates account, returns authenticated', async () => {
    await pageA.click('#login-to-register');
    await waitFor(pageA, () => vis(pageA, '#register-modal'), 'register modal');
    await pageA.fill('#register-email', EMAIL);
    await pageA.fill('#register-name', ADMIN_NAME);
    await pageA.fill('#register-password', PASS);
    await pageA.fill('#register-confirm', PASS);
    await pageA.click('#register-submit');
    await waitFor(pageA, () => vis(pageA, '#auth-bar-user'), 'authenticated bar');
    const dn = await pageA.locator('#auth-display-name').textContent();
    if (dn !== ADMIN_NAME) throw new Error(`display name "${dn}"`);
    await waitConnected(pageA); // WS reconnects with token
  });

  // ---------- Item 4: logout + login ----------
  await check(4, 'Login works, shows room browser', async () => {
    await pageA.click('#logout-btn');
    await waitFor(pageA, () => vis(pageA, '#auth-bar-guest'), 'logged out');
    await pageA.click('#sign-in-btn');
    await pageA.fill('#login-email', EMAIL);
    await pageA.fill('#login-password', PASS);
    await pageA.click('#login-submit');
    await waitFor(pageA, () => vis(pageA, '#auth-bar-user'), 'logged back in');
    await waitFor(pageA, () => vis(pageA, '#room-browser'), 'room browser visible');
    await waitConnected(pageA);
  });

  // ---------- Item 5: room browser from API ----------
  await check(5, 'Room browser shows rooms from API', async () => {
    await waitFor(pageA, () => pageA.locator('#room-list .room-card').count().then(c => c > 0), 'room cards');
    const txt = await pageA.locator('#room-list').textContent();
    if (!txt.includes('Test Room')) throw new Error('persisted "Test Room" not listed');
  });

  // ---------- Item 6: create room ----------
  await check(6, 'Create room creates and appears in browser', async () => {
    await pageA.click('#create-room-btn');
    await pageA.fill('#cr-id', MAIN);
    await pageA.fill('#cr-name', 'E2E Main');
    await pageA.fill('#cr-topic', 'Main room topic');
    await pageA.click('#create-room-submit');
    await waitFor(pageA, () => vis(pageA, '#room-screen'), 'auto-joined created room');
    const label = await pageA.locator('#room-label').textContent();
    if (label !== MAIN) throw new Error(`room label "${label}"`);
    await pageA.click('#leave-btn');
    await waitFor(pageA, () => vis(pageA, '#join-screen'), 'left room');
    await pageA.fill('#room-search-input', 'E2E Main');
    await waitFor(pageA, async () => {
      const t = await pageA.locator('#room-list').textContent();
      return t.includes('E2E Main');
    }, 'created room in browser list');
  });

  // ---------- Password enforcement ----------
  await check('pw1', 'Owner joins own password room without prompt', async () => {
    await pageA.click('#create-room-btn');
    await pageA.fill('#cr-id', PW);
    await pageA.fill('#cr-name', 'E2E Password Room');
    await pageA.fill('#cr-password', PW_SECRET);
    const promptsBefore = dlgA.messages.filter(m => m.type === 'prompt').length;
    await pageA.click('#create-room-submit');
    await waitFor(pageA, () => vis(pageA, '#room-screen'), 'owner auto-joined password room');
    if (dlgA.messages.filter(m => m.type === 'prompt').length !== promptsBefore) throw new Error('owner was prompted for own room password');
    await pageA.click('#leave-btn');
    await waitFor(pageA, () => vis(pageA, '#join-screen'), 'owner left');
  });

  await check('pw2', 'Guest with wrong password is rejected', async () => {
    await ensureJoinScreen(pageB);
    const promptsBefore = dlgB.messages.filter(m => m.type === 'prompt').length;
    dlgB.promptQueue.push('totally-wrong');
    await joinRoom(pageB, GUEST_NAME, PW);
    await waitFor(pageB, () => Promise.resolve(
      dlgB.messages.filter(m => m.type === 'prompt').length > promptsBefore), 'password prompt shown', 8000);
    // wrong password → join fails with alert, still on join screen
    await waitFor(pageB, () => Promise.resolve(
      dlgB.messages.some(m => m.type === 'alert' && /password/i.test(m.message))), 'wrong-password alert', 8000);
    if (await vis(pageB, '#room-screen')) throw new Error('guest entered password room with wrong password');
  });

  await check('pw3', 'Guest with correct password joins', async () => {
    await ensureJoinScreen(pageB);
    dlgB.promptQueue.length = 0;
    dlgB.promptQueue.push(PW_SECRET);
    await joinRoom(pageB, GUEST_NAME, PW);
    await waitFor(pageB, () => vis(pageB, '#room-screen'), 'guest in password room');
    await pageB.click('#leave-btn');
    await waitFor(pageB, () => vis(pageB, '#join-screen'), 'guest left password room');
  });

  // Create the moderated + lobby room for the rest of the run
  await pageA.click('#create-room-btn');
  await pageA.fill('#cr-id', MOD);
  await pageA.fill('#cr-name', 'E2E Mod');
  await pageA.fill('#cr-topic', 'Mod topic');
  await pageA.check('#cr-moderated');
  await pageA.check('#cr-lobby');
  await pageA.click('#create-room-submit');
  await waitFor(pageA, () => vis(pageA, '#room-screen'), 'joined mod room');
  await waitFor(pageA, () => vis(pageA, '#room-settings-btn'), 'owner sees room settings btn');

  // ---------- Item 7a: lobby waiting screen ----------
  await check('7a', 'Lobby: guest sees waiting screen', async () => {
    await ensureJoinScreen(pageB);
    await joinRoom(pageB, GUEST_NAME, MOD);
    await waitFor(pageB, () => vis(pageB, '#lobby-screen'), 'lobby screen');
    const rn = await pageB.locator('#lobby-room-name').textContent();
    if (!rn || !rn.trim()) throw new Error('lobby room name empty');
  });

  // ---------- Item 8a: lobby panel + admit ----------
  await check('8a', 'Lobby panel: moderator sees waiting user, admit works', async () => {
    await waitFor(pageA, () => vis(pageA, '#lobby-tab'), 'lobby tab visible');
    await pageA.click('#lobby-tab');
    await waitFor(pageA, async () => {
      const t = await pageA.locator('#lobby-list').textContent();
      return t.includes(GUEST_NAME);
    }, 'waiting guest listed');
    await pageA.click('#lobby-list .lobby-admit-btn');
  });

  // ---------- Item 7b: admitted user gets media (recv path + moderation enforcement) ----------
  await check('7b', 'Admitted user gets media: consumes owner video; produce blocked while unvoiced', async () => {
    await waitFor(pageB, () => vis(pageB, '#room-screen'), 'guest in room after admit');
    // Wait for post-admission media setup (mic button loses its no-media styling)
    await waitFor(pageB, async () => {
      const op = await pageB.locator('#mic-btn').evaluate(el => el.style.opacity);
      return op !== '0.4';
    }, 'post-admission media ready', 15000);
    // Owner publishes, admitted guest consumes (proves post-admission recv path)
    await pageA.click('#cam-btn');
    await remoteVideoPlaying(pageB, ADMIN_NAME);
    // Moderated room: plain user's produce must be REJECTED with feedback
    await pageB.click('#cam-btn');
    await waitFor(pageB, async () => {
      const t = await pageB.locator('#toast-container').textContent();
      return t.includes('not allowed to produce');
    }, 'produce-denied toast for unvoiced user', 8000);
    if (await pageB.locator('#local-tile video').count() > 0) throw new Error('unvoiced user produced video in moderated room');
  });

  // ---------- Item 9: context menu actions ----------
  await check(9, 'Context menu: all 9 actions present for owner', async () => {
    await rightClickParticipant(pageA, GUEST_NAME);
    const { direct, roleOpts, hasRoleGroup } = await modMenuButtons(pageA);
    const expected = ['Close Camera', 'Cam Unban', 'Mute Text', 'Text Unmute', 'Kick', 'Cam Ban', 'Ban', 'Unban'];
    for (const e of expected) if (!direct.includes(e)) throw new Error(`missing action "${e}" (got: ${direct.join(', ')})`);
    if (!hasRoleGroup) throw new Error('Set Role group missing');
    if (direct.length !== 8) throw new Error(`expected 8 direct actions, got ${direct.length}`);
    note(`owner Set Role options: ${roleOpts.join(', ')}`);
    await closeModMenu(pageA);
  });

  // ---------- Item 10a: owner set-role options ----------
  await check('10a', 'Set Role sub-menu shows correct options for owner', async () => {
    await rightClickParticipant(pageA, GUEST_NAME);
    const { roleOpts } = await modMenuButtons(pageA);
    const want = ['User', 'Member', 'Moderator', 'Admin'];
    if (JSON.stringify(roleOpts) !== JSON.stringify(want)) throw new Error(`owner roles: ${roleOpts.join(',')}`);
    await closeModMenu(pageA);
  });

  // ---------- Item 11: voice request grant/dismiss ----------
  await check(11, 'Voice request shows Grant/Dismiss, Grant promotes to member', async () => {
    await waitFor(pageB, () => vis(pageB, '#hand-btn'), 'hand button visible for plain user in moderated room');
    await pageB.click('#hand-btn');
    await waitFor(pageA, () => vis(pageA, '.toast-action'), 'action toast on moderator');
    const toastTxt = await pageA.locator('.toast-action').textContent();
    if (!toastTxt.includes(GUEST_NAME) || !toastTxt.includes('requesting voice')) throw new Error(`toast: "${toastTxt}"`);
    const btns = await pageA.locator('.toast-action .toast-actions button').allTextContents();
    if (!btns.includes('Grant') || !btns.includes('Dismiss')) throw new Error(`buttons: ${btns.join(',')}`);
    await pageA.locator('.toast-action .toast-actions button', { hasText: 'Grant' }).click();
    // B should be promoted to member → hand button hides
    await waitFor(pageB, () => hidden(pageB, '#hand-btn'), 'hand button hidden after grant');
  });

  // ---------- Item 7c: granted member can broadcast (send path post-admission) ----------
  await check('7c', 'Voice-granted member broadcasts video; owner consumes it', async () => {
    await pageB.click('#cam-btn');
    await waitFor(pageB, () => pageB.locator('#local-tile video').count().then(c => c > 0), 'B local video tile');
    await remoteVideoPlaying(pageA, GUEST_NAME);
  });

  // ---------- Active speaker detection (server-side observers) ----------
  await check('as1', 'Speaking member is highlighted on other clients', async () => {
    await pageB.click('#mic-btn'); // fake audio device emits a tone
    await waitFor(pageA, () =>
      pageA.locator('.video-tile.speaking, .video-tile.dominant-speaker').count().then(c => c > 0),
      'speaking/dominant highlight on A', 15000);
    await pageB.click('#mic-btn'); // mute again
  });

  // ---------- Item 10b: moderator sees restricted menu ----------
  await check('10b', 'Moderator sees mod-only actions and User/Member roles only', async () => {
    // Owner promotes guest to moderator
    await rightClickParticipant(pageA, GUEST_NAME);
    await pageA.locator('#mod-menu .mod-menu-group button', { hasText: 'Moderator' }).click();
    await pageA.waitForTimeout(500);
    // Guest (now moderator) opens menu on owner
    await rightClickParticipant(pageB, ADMIN_NAME);
    const { direct, roleOpts } = await modMenuButtons(pageB);
    for (const banned of ['Cam Ban', 'Ban', 'Unban']) {
      if (direct.includes(banned)) throw new Error(`moderator should not see "${banned}"`);
    }
    for (const e of ['Close Camera', 'Cam Unban', 'Mute Text', 'Text Unmute', 'Kick']) {
      if (!direct.includes(e)) throw new Error(`moderator missing "${e}"`);
    }
    if (JSON.stringify(roleOpts) !== JSON.stringify(['User', 'Member'])) throw new Error(`mod roles: ${roleOpts.join(',')}`);
    await closeModMenu(pageB);
  });

  // ---------- Item 14: topic display + click-to-edit ----------
  await check(14, 'Topic displays in header, click-to-edit for admins', async () => {
    const t0 = await pageA.locator('#room-topic').textContent();
    if (t0 !== 'Mod topic') throw new Error(`initial topic "${t0}"`);
    const promptsBefore = dlgA.messages.filter(m => m.type === 'prompt').length;
    dlgA.promptQueue.push('Updated mod topic', 'Updated mod topic', 'Updated mod topic');
    await pageA.click('#room-topic');
    await pageA.waitForTimeout(800);
    const prompts = dlgA.messages.filter(m => m.type === 'prompt').length - promptsBefore;
    if (prompts === 0) throw new Error('no prompt fired for admin');
    if (prompts > 1) note(`BUG CONFIRMED: topic click fired ${prompts} prompts (listener added per join; main.ts:1033)`);
    await waitFor(pageA, async () => (await pageA.locator('#room-topic').textContent()) === 'Updated mod topic', 'topic updated on A');
    await waitFor(pageB, async () => (await pageB.locator('#room-topic').textContent()) === 'Updated mod topic', 'topic propagated to B');
    // Non-admin (moderator) click → no prompt
    const bPrompts = dlgB.messages.filter(m => m.type === 'prompt').length;
    await pageB.click('#room-topic');
    await pageB.waitForTimeout(500);
    if (dlgB.messages.filter(m => m.type === 'prompt').length !== bPrompts) throw new Error('moderator got topic edit prompt');
  });

  // ---------- Item 13: settings modal complete ----------
  await check(13, 'Room settings modal shows all settings by category', async () => {
    await pageA.click('#room-settings-btn');
    await waitFor(pageA, () => vis(pageA, '#room-settings-modal'), 'settings modal');
    const controls = ['rs-topic', 'rs-password', 'rs-guests', 'rs-guests-broadcast', 'rs-require-reg',
      'rs-invite-only', 'rs-secret', 'rs-lobby', 'rs-video', 'rs-chat', 'rs-screen', 'rs-ptt',
      'rs-max-broadcasters', 'rs-max-participants', 'rs-moderated'];
    for (const id of controls) {
      if (!(await vis(pageA, `#${id}`))) throw new Error(`control #${id} missing/hidden`);
    }
    const sections = await pageA.locator('.settings-section-title').allTextContents();
    for (const s of ['General', 'Access', 'Media', 'Moderation']) {
      if (!sections.includes(s)) throw new Error(`section "${s}" missing`);
    }
    // populated from live state
    if (!(await pageA.isChecked('#rs-moderated'))) throw new Error('moderated not pre-checked');
    if (!(await pageA.isChecked('#rs-lobby'))) throw new Error('lobby not pre-checked');
    const tv = await pageA.inputValue('#rs-topic');
    if (tv !== 'Updated mod topic') throw new Error(`topic field "${tv}"`);
  });

  // ---------- Item 15: chat works then disabled ----------
  await check(15, 'Chat disabled when allowChat=false (and works when on)', async () => {
    // B may be on the Users tab from earlier context-menu steps — switch back to Chat
    await pageB.click('.tab[data-tab="chat"]');
    // baseline: chat delivers
    await pageB.fill('#chat-input', 'hello from B');
    await pageB.press('#chat-input', 'Enter');
    await waitFor(pageA, async () => (await pageA.locator('#chat-messages').textContent()).includes('hello from B'), 'chat delivered');
    // disable via settings (modal already open on A)
    await pageA.uncheck('#rs-chat');
    await waitFor(pageB, () => pageB.locator('#chat-input:disabled').count().then(c => c > 0), 'B chat input disabled');
    const ph = await pageB.locator('#chat-input').getAttribute('placeholder');
    if (ph !== 'Chat is disabled') throw new Error(`placeholder "${ph}"`);
    // re-enable
    await pageA.check('#rs-chat');
    await waitFor(pageB, () => pageB.locator('#chat-input:not(:disabled)').count().then(c => c > 0), 'B chat re-enabled');
  });

  // ---------- Item 16: screen share hidden when disabled ----------
  await check(16, 'Screen share hidden when allowScreenSharing=false', async () => {
    if (!(await vis(pageB, '#screen-btn'))) throw new Error('screen btn should start visible');
    await pageA.uncheck('#rs-screen');
    await waitFor(pageB, () => hidden(pageB, '#screen-btn'), 'B screen btn hidden');
    await pageA.check('#rs-screen');
    await waitFor(pageB, () => vis(pageB, '#screen-btn'), 'B screen btn restored');
  });

  // ---------- Item 17: camera hidden when disabled ----------
  await check(17, 'Camera hidden when allowVideo=false', async () => {
    if (!(await vis(pageB, '#cam-btn'))) throw new Error('cam btn should start visible');
    await pageA.uncheck('#rs-video');
    await waitFor(pageB, () => hidden(pageB, '#cam-btn'), 'B cam btn hidden');
    await pageA.check('#rs-video');
    await waitFor(pageB, () => vis(pageB, '#cam-btn'), 'B cam btn restored');
  });

  // close settings modal
  await pageA.click('#room-settings-close');

  // ---------- Item 12a: kick alert ----------
  await check('12a', 'Kicked user sees explanation alert', async () => {
    const before = dlgB.messages.length;
    await rightClickParticipant(pageA, GUEST_NAME);
    await pageA.locator('#mod-menu > button', { hasText: 'Kick' }).click();
    await waitFor(pageB, () => Promise.resolve(dlgB.messages.length > before), 'kick alert', 8000);
    const msg = dlgB.messages[dlgB.messages.length - 1].message;
    if (!msg.includes('kicked')) throw new Error(`alert: "${msg}"`);
  });

  // ---------- Item 8b: lobby deny ----------
  await check('8b', 'Lobby deny returns guest to join screen with alert', async () => {
    await pageB.reload();
    await waitConnected(pageB);
    await joinRoom(pageB, GUEST_NAME, MOD);
    await waitFor(pageB, () => vis(pageB, '#lobby-screen'), 'guest back in lobby');
    await pageA.click('#lobby-tab');
    await waitFor(pageA, async () => (await pageA.locator('#lobby-list').textContent()).includes(GUEST_NAME), 'guest listed');
    const before = dlgB.messages.length;
    await pageA.click('#lobby-list .lobby-deny-btn');
    await waitFor(pageB, () => Promise.resolve(dlgB.messages.length > before), 'deny alert', 8000);
    const msg = dlgB.messages[dlgB.messages.length - 1].message;
    if (!msg.toLowerCase().includes('denied')) throw new Error(`alert: "${msg}"`);
    await waitFor(pageB, () => vis(pageB, '#join-screen'), 'guest back at join screen');
  });

  // ---------- Item 12b: ban alert + rejoin prevention ----------
  await check('12b', 'Banned user sees alert; rejoin behavior recorded', async () => {
    // Disable lobby so guest joins directly
    await pageA.click('#room-settings-btn');
    await pageA.uncheck('#rs-lobby');
    await pageA.click('#room-settings-close');
    await pageB.reload();
    await waitConnected(pageB);
    await joinRoom(pageB, GUEST_NAME, MOD);
    await waitFor(pageB, () => vis(pageB, '#room-screen'), 'guest joined directly (lobby off)');
    const before = dlgB.messages.length;
    await rightClickParticipant(pageA, GUEST_NAME);
    await pageA.locator('#mod-menu > button', { hasText: /^Ban$/ }).click();
    await waitFor(pageB, () => Promise.resolve(dlgB.messages.length > before), 'ban alert', 8000);
    const msg = dlgB.messages[dlgB.messages.length - 1].message;
    if (!msg.includes('banned')) throw new Error(`alert: "${msg}"`);
    // Rejoin after reconnect MUST be blocked (guest identity = IP, persisted in room_states)
    await pageB.reload();
    await waitConnected(pageB);
    const before2 = dlgB.messages.length;
    await joinRoom(pageB, GUEST_NAME, MOD);
    await waitFor(pageB, () => Promise.resolve(
      dlgB.messages.slice(before2).some(m => /banned/i.test(m.message))), 'rejoin-blocked alert', 8000);
    if (await vis(pageB, '#room-screen')) throw new Error('banned guest rejoined after reconnect');
  });

  // ---------- Item 12c: registered-user ban persists across sessions ----------
  await check('12c', 'Banned registered user cannot rejoin after re-login', async () => {
    // B registers an account (authenticated bans key on user id, so IP rows for guests must
    // not block authenticated users — this join must succeed despite the guest-IP ban above)
    await pageB.reload();
    await waitConnected(pageB);
    await pageB.click('#sign-in-btn');
    await pageB.click('#login-to-register');
    await pageB.fill('#register-email', `e2e-b-${TS}@test.local`);
    await pageB.fill('#register-name', 'BobUser');
    await pageB.fill('#register-password', 'BobSecret123!');
    await pageB.fill('#register-confirm', 'BobSecret123!');
    await pageB.click('#register-submit');
    await waitFor(pageB, () => vis(pageB, '#auth-bar-user'), 'B registered');
    await waitConnected(pageB);
    await joinRoom(pageB, 'BobUser', MOD);
    await waitFor(pageB, () => vis(pageB, '#room-screen'), 'authed B joined despite guest-IP ban');
    // A bans the registered user
    await rightClickParticipant(pageA, 'BobUser');
    const before = dlgB.messages.length;
    await pageA.locator('#mod-menu > button', { hasText: /^Ban$/ }).click();
    await waitFor(pageB, () => Promise.resolve(dlgB.messages.length > before), 'ban alert', 8000);
    // Tear the room down (A leaves → room dropped from memory) so only DB
    // persistence can block the rejoin — in-memory ban sets die with the room.
    await pageA.click('#leave-btn');
    await waitFor(pageA, () => vis(pageA, '#join-screen'), 'A left, room torn down');
    // Fresh session, same account — rejoin must be blocked
    await pageB.reload();
    await waitConnected(pageB);
    await waitFor(pageB, () => vis(pageB, '#auth-bar-user'), 'B session restored');
    const before2 = dlgB.messages.length;
    await joinRoom(pageB, 'BobUser', MOD);
    await waitFor(pageB, () => Promise.resolve(
      dlgB.messages.slice(before2).some(m => /banned/i.test(m.message))), 'authed rejoin-blocked alert', 8000);
    if (await vis(pageB, '#room-screen')) throw new Error('banned registered user rejoined');
  });

  // ---------- summary ----------
  const passed = results.filter(r => r.pass === true).length;
  const failed = results.filter(r => r.pass === false).length;
  console.log(`\n=== ${passed} passed, ${failed} failed ===`);
  fs.writeFileSync(`${ART}/results.json`, JSON.stringify({ run: TS, results, dialogsA: dlgA.messages, dialogsB: dlgB.messages }, null, 2));

  await browserA.close();
  await browserB.close();
  process.exit(failed > 0 ? 1 : 0);
})().catch(async (e) => {
  console.error('FATAL:', e);
  try {
    await pageA?.screenshot({ path: `${ART}/fatal-A.png` });
    await pageB?.screenshot({ path: `${ART}/fatal-B.png` });
  } catch {}
  fs.writeFileSync(`${ART}/results.json`, JSON.stringify({ run: TS, results, fatal: String(e) }, null, 2));
  process.exit(2);
});
