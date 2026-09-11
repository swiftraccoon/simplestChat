/**
 * Opt-in real-browser smoke. Use ONLY an isolated database/server: this creates
 * disposable accounts and creates/deletes its own uniquely named room.
 * COMMUNITY_E2E=1 BASE_URL=http://127.0.0.1:3109
 * PLAYWRIGHT_MODULE=/path/to/node_modules/playwright node web/e2e/community.cjs
 * PLAYWRIGHT_BROWSERS_PATH can point at an isolated browser installation.
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
if (process.env.COMMUNITY_E2E !== '1') throw new Error('Set COMMUNITY_E2E=1 against a disposable local server/database.');
const base = process.env.BASE_URL || 'http://127.0.0.1:3109';
if (!['localhost', '127.0.0.1', '[::1]'].includes(new URL(base).hostname)) throw new Error('This smoke is restricted to local test servers.');
const { browserOptions } = require('./browser-options.cjs');
const options = browserOptions(process.env.E2E_BROWSER);
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const artifacts = process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-community-e2e.'));
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
const runId = `e2e-${Date.now().toString(36)}`;
const password = 'Disposable-browser-password-2026!';
const clients = [];
let browser;
let activeStep = 'launch';
const report = {
  runId, browser: options.name, browserVersion: null,
  playwrightVersion: require(path.join(path.dirname(require.resolve(process.env.PLAYWRIGHT_MODULE || 'playwright')), 'package.json')).version,
  platform: process.platform, arch: process.arch, baseUrl: base,
  launchOptions: options.launchOptions, contextOptions: options.contextOptions,
  announcedIp: process.env.TEST_ANNOUNCE_IP || null,
  startedAt: new Date().toISOString(), complete: false, passed: false, steps: [], playbackRecoveries: [],
  limitations: ['Fake capture devices; not physical device or permission-prompt coverage.', 'External capture termination is simulated with stop() plus an ended event on an owned fake local-stream track.', 'Mobile checks resize a desktop viewport; they do not run a mobile browser.'],
};
function saveReport() { fs.writeFileSync(path.join(artifacts, 'community-results.json'), `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 }); }
async function deadline(work, milliseconds = 5000) {
  let timer;
  try { return await Promise.race([work, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Browser diagnostics deadline exceeded')), milliseconds); })]); }
  finally { clearTimeout(timer); }
}
async function step(name, work) {
  activeStep = name;
  const result = { name, passed: false };
  report.steps.push(result); saveReport();
  const outcome = await work();
  if (outcome?.skip) {
    result.skipped = true; result.reason = outcome.skip;
    saveReport(); console.log(`SKIP ${name}: ${outcome.skip}`);
    return;
  }
  result.passed = true; saveReport(); console.log(`PASS ${name}`);
}
async function client(label, mobile = false) {
  const context = await browser.newContext({ ...options.contextOptions, viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 } });
  const page = await context.newPage(); page.setDefaultTimeout(10000);
  const entry = { label, context, page, errors: [], frames: [], warnings: [] }; clients.push(entry);
  // Observe native peer connections for failure diagnostics; no SDP/media mocking.
  await page.addInitScript(() => {
    window.__communityPeers = [];
    window.__communityCaptureRequests = 0;
    const devices = navigator.mediaDevices;
    const getUserMedia = devices.getUserMedia.bind(devices);
    devices.getUserMedia = (...args) => {
      window.__communityCaptureRequests++;
      return getUserMedia(...args);
    };
    if (!window.RTCPeerConnection) return;
    window.RTCPeerConnection = new Proxy(window.RTCPeerConnection, {
      construct(target, args, newTarget) {
        const peer = Reflect.construct(target, args, newTarget);
        window.__communityPeers.push(peer);
        return peer;
      },
    });
  });
  page.on('pageerror', error => entry.errors.push(error.stack || error.message));
  page.on('console', message => {
    if (['warning', 'error'].includes(message.type())) {
      entry.warnings.push(message.text());
      if (entry.warnings.length > 40) entry.warnings.shift();
    }
  });
  page.on('websocket', socket => {
    for (const direction of ['framesent', 'framereceived']) socket.on(direction, frame => {
      try { const message = JSON.parse(String(frame.payload)); entry.frames.push({ direction, type: message.type, content: message.content, message: typeof message.message === 'string' ? message.message : message.message?.content }); if (entry.frames.length > 40) entry.frames.shift(); } catch {}
    });
  });
  page.on('dialog', dialog => dialog.dismiss());
  await page.goto(base, { waitUntil: 'networkidle' });
  try { await page.locator('.conversation-toolbar').waitFor({ state: 'attached', timeout: 3000 }); }
  catch { await page.reload({ waitUntil: 'networkidle' }); await page.locator('.conversation-toolbar').waitFor({ state: 'attached' }); }
  assert.deepEqual(entry.errors, [], `${label} startup page errors`);
  return page;
}
async function visible(page, text) { await page.getByText(text, { exact: true }).filter({ visible: true }).last().waitFor({ state: 'visible' }); }
async function remotePlayback(page, kind) {
  await page.waitForFunction(mediaKind => {
    const playing = [...document.querySelectorAll(`.video-tile:not(.local) ${mediaKind}`)].some(element => !element.paused && (mediaKind === 'video' ? element.videoWidth > 0 : element.readyState >= 2 && !element.muted && element.volume > 0));
    const prompt = [...document.querySelectorAll('.personal-playback-blocked')].some(notice => !notice.hidden);
    return playing || prompt;
  }, kind, { timeout: 15000 });
  const retry = page.locator('[data-control="retry-playback"]').filter({ visible: true });
  if (await retry.count()) {
    await retry.first().click();
    report.playbackRecoveries.push({ kind, step: activeStep });
  }
  await page.waitForFunction(mediaKind => [...document.querySelectorAll(`.video-tile:not(.local) ${mediaKind}`)].some(element => !element.paused && (mediaKind === 'video' ? element.videoWidth > 0 : element.readyState >= 2 && !element.muted && element.volume > 0)), kind, { timeout: 15000 });
  await page.waitForFunction(() => [...document.querySelectorAll('.personal-playback-blocked')].every(notice => notice.hidden));
}
async function connected(page) {
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  await page.waitForFunction(() => document.querySelector('#connection-status').textContent === 'Connected');
  await page.getByRole('combobox', { name: 'Conversation', exact: true }).waitFor({ state: 'visible' });
}
async function register(page, email, name) {
  await page.locator('#sign-in-btn').click(); await page.locator('#login-to-register').click();
  await page.locator('#register-email').fill(email); await page.locator('#register-name').fill(name);
  await page.locator('#register-password').fill(password); await page.locator('#register-confirm').fill(password);
  await page.locator('#register-submit').click(); await page.locator('#register-modal').waitFor({ state: 'hidden' });
  await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
}
async function login(page, email, value) {
  if (!await page.locator('#login-modal').isVisible()) await page.locator('#sign-in-btn').click();
  await page.locator('#login-email').fill(email); await page.locator('#login-password').fill(value);
  await page.locator('#login-submit').click(); await page.locator('#login-modal').waitFor({ state: 'hidden' });
  await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
}
const lastJoin = new WeakMap();
async function join(page, name) {
  const remaining = 1100 - (Date.now() - (lastJoin.get(page) || 0));
  if (remaining > 0) await page.waitForTimeout(remaining);
  await page.locator('#name-input').fill(name); await page.locator('#room-input').fill(runId);
  await page.locator('#join-btn').click(); lastJoin.set(page, Date.now()); await connected(page);
}
async function leave(page) { await page.locator('#leave-btn').click(); await page.locator('#join-screen').waitFor({ state: 'visible' }); }
const lastSend = new WeakMap();
async function submitMessage(page) {
  // Human-paced messages preserve the server's 2/s chat flood protection.
  const remaining = 600 - (Date.now() - (lastSend.get(page) || 0));
  if (remaining > 0) await page.waitForTimeout(remaining);
  await page.locator('#chat-send-btn').click(); lastSend.set(page, Date.now());
}
async function send(page, text) { await page.locator('#chat-input').fill(text); await submitMessage(page); }
async function publicChat(page) { await page.getByRole('combobox', { name: 'Conversation', exact: true }).selectOption('public'); }
async function action(page, name, label) {
  const buttons = page.getByRole('button', { name: `Actions for ${name}`, exact: true });
  for (const node of await buttons.all()) if (await node.isVisible()) { await node.click(); break; }
  await page.locator('#mod-menu').getByRole('button', { name: label, exact: true }).click();
}
async function header(page, name) { await page.locator('#community-actions').getByRole('button', { name, exact: true }).click(); return page.getByRole('dialog', { name, exact: true }); }
async function close(dialog) { await dialog.getByRole('button', { name: 'Close', exact: true }).click(); await dialog.waitFor({ state: 'hidden' }); }
async function preferences(page, allow) {
  await page.getByRole('button', { name: 'Chat options', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Chat preferences', exact: true });
  await dialog.getByLabel('Allow incoming private messages', { exact: true }).setChecked(allow);
  await dialog.getByRole('button', { name: 'Save preferences', exact: true }).click(); await dialog.waitFor({ state: 'hidden' });
}
async function chatEnabled(page, value) { await page.waitForFunction(expected => document.querySelector('#chat-input').disabled === !expected, value); }
async function roomSetting(page, id, checked) {
  await page.locator('#room-settings-btn').click(); await page.locator(id).setChecked(checked);
  await page.locator('#room-settings-close').click();
}
async function upload(page, dialog, room = false) {
  const data = await page.evaluate(() => { const canvas = document.createElement('canvas'); canvas.width = 64; canvas.height = 64; const ctx = canvas.getContext('2d'); ctx.fillStyle = '#6644cc'; ctx.fillRect(0, 0, 64, 64); return canvas.toDataURL('image/png').split(',')[1]; });
  await dialog.getByLabel(room ? 'Room image (PNG, JPEG or WebP)' : 'Avatar (PNG, JPEG or WebP)', { exact: true }).setInputFiles({ name: 'test-raster.png', mimeType: 'image/png', buffer: Buffer.from(data, 'base64') });
  await dialog.getByRole('img', { name: room ? 'Room image preview' : 'Avatar preview', exact: true }).waitFor({ state: 'visible' });
}
async function setRole(owner, name, role) {
  const dialog = await header(owner, 'Manage room');
  const row = dialog.locator('.management-entry').filter({ has: owner.getByRole('combobox', { name: `Role for ${name}`, exact: true }) });
  await row.getByRole('combobox').selectOption(String(role)); await row.getByRole('button', { name: 'Update role', exact: true }).click();
  if (role === 1) await row.waitFor({ state: 'detached' });
  else await dialog.getByText(new RegExp(`${name} · ${['guest', 'user', 'member', 'moderator', 'admin', 'owner'][role]}`)).waitFor();
  await close(dialog);
}

(async () => {
  saveReport();
  try {
    browser = await playwright[options.name].launch(options.launchOptions);
    report.browserVersion = browser.version(); saveReport();
    console.log(`Browser: ${options.name} ${report.browserVersion}; Playwright ${report.playwrightVersion}`);
    const owner = await client('owner'); const member = await client('member'); const guest = await client('guest');
    const ownerEmail = `${runId}-owner@example.test`; const memberEmail = `${runId}-member@example.test`;
    await step('register owner/member, create room and guest join', async () => {
      await register(owner, ownerEmail, 'E2E Owner'); await register(member, memberEmail, 'E2E Member');
      await owner.locator('#create-room-btn').click(); await owner.locator('#cr-id').fill(runId);
      await owner.locator('#cr-name').fill('E2E Community Room'); await owner.locator('#create-room-submit').click(); await connected(owner);
      await join(member, 'E2E Member'); await join(guest, 'E2E Guest');
      await send(guest, 'Public hello'); await visible(owner, 'Public hello'); await visible(member, 'Public hello');
    });
    await step('private conversations, unread, acknowledgements and recipient isolation', async () => {
      await action(owner, 'E2E Member', 'Private message'); await send(owner, 'Private owner hello');
      await member.waitForFunction(() => [...document.querySelector('[aria-label="Conversation"]').options].some(o => o.text.includes('E2E Owner') && o.text.includes('(1)')));
      await member.getByRole('combobox', { name: 'Conversation', exact: true }).selectOption({ label: 'E2E Owner (1)' });
      await visible(member, 'Private owner hello'); await visible(owner, 'Private owner hello');
      assert.equal(await owner.locator('.msg-text').filter({ hasText: /^Private owner hello$/ }).count(), 1);
      assert.equal(await guest.getByText('Private owner hello', { exact: true }).count(), 0);
      await send(member, 'Private member reply'); await visible(owner, 'Private member reply');
    });
    await step('private drafts and sent-input recall stay in their conversation', async () => {
      await owner.locator('#chat-input').fill('Unsent private draft'); await publicChat(owner);
      assert.equal(await owner.locator('#chat-input').inputValue(), '');
      await owner.locator('#chat-input').press('ArrowUp'); assert.equal(await owner.locator('#chat-input').inputValue(), '');
      await action(owner, 'E2E Member', 'Private message'); assert.equal(await owner.locator('#chat-input').inputValue(), 'Unsent private draft');
      await owner.locator('#chat-input').fill('');
    });
    await step('PM opt-out returns explicit delivery failure', async () => {
      await preferences(member, false); await send(owner, 'Blocked by opt-out');
      await owner.locator('.chat-msg').filter({ hasText: 'Blocked by opt-out' }).locator('.delivery-error').waitFor({ state: 'visible' });
      assert.equal(await member.getByText('Blocked by opt-out', { exact: true }).count(), 0);
      await preferences(member, true);
    });
    await step('ignore/unignore hides public messages and blocks private delivery', async () => {
      await action(member, 'E2E Owner', 'Ignore messages');
      await publicChat(owner); await publicChat(member); await send(owner, 'Ignored public text'); await visible(guest, 'Ignored public text');
      assert.equal(await member.getByText('Ignored public text', { exact: true }).count(), 0);
      await action(owner, 'E2E Member', 'Private message'); await send(owner, 'Ignored private text');
      await owner.locator('.chat-msg').filter({ hasText: 'Ignored private text' }).locator('.delivery-error').waitFor();
      assert.equal(await member.getByText('Ignored private text', { exact: true }).count(), 0);
      await action(member, 'E2E Owner', 'Unignore messages'); await publicChat(owner); await send(owner, 'Visible after unignore'); await visible(member, 'Visible after unignore');
    });
    await step('nickname, mentions, emoji and sent-input recall', async () => {
      await header(guest, 'Nickname'); const dialog = guest.getByRole('dialog', { name: 'Room nickname', exact: true });
      await dialog.getByLabel('Nickname', { exact: true }).fill('E2ERenamed'); await dialog.getByRole('button', { name: 'Change nickname', exact: true }).click(); await dialog.waitFor({ state: 'hidden' });
      await owner.getByRole('button', { name: 'Actions for E2ERenamed', exact: true }).first().waitFor({ state: 'visible' });
      await owner.locator('#chat-input').fill('@E2ER'); await owner.locator('#chat-input').press('Tab');
      assert.equal(await owner.locator('#chat-input').inputValue(), '@E2ERenamed ');
      await send(owner, '@E2ERenamed welcome'); await guest.locator('.chat-msg.mentioned').filter({ hasText: '@E2ERenamed welcome' }).waitFor();
      await owner.getByRole('button', { name: 'Choose emoji', exact: true }).click(); await owner.getByRole('button', { name: '👋', exact: true }).click();
      await submitMessage(owner); await visible(guest, '👋');
      await owner.locator('#chat-input').press('ArrowUp'); assert.equal(await owner.locator('#chat-input').inputValue(), '👋'); await owner.locator('#chat-input').fill('');
    });
    await step('report submission and moderator review/resolve', async () => {
      await action(guest, 'E2E Member', 'Report to moderators'); const report = guest.getByRole('dialog', { name: 'Report E2E Member', exact: true });
      await report.getByLabel('Reason and details', { exact: true }).fill('Disposable browser test report'); await report.getByRole('button', { name: 'Send report', exact: true }).click(); await report.waitFor({ state: 'hidden' });
      const manage = await header(owner, 'Manage room'); await manage.getByRole('button', { name: 'Reports', exact: true }).click();
      await visible(owner, 'Disposable browser test report'); await manage.getByRole('button', { name: 'Mark resolved', exact: true }).click(); await visible(owner, 'E2E Member · resolved'); await close(manage);
    });
    await step('membership roles, live moderation gates and allow-chat toggle', async () => {
      await action(owner, 'E2E Member', 'Moderator'); await member.locator('#community-actions').getByRole('button', { name: 'Manage room', exact: true }).waitFor({ state: 'visible' });
      await setRole(owner, 'E2E Member', 2);
      await setRole(owner, 'E2E Member', 1); await roomSetting(owner, '#rs-moderated', true); await chatEnabled(member, false); await chatEnabled(guest, false);
      await action(owner, 'E2E Member', 'Member'); await chatEnabled(member, true); await send(member, 'Member has voice'); await visible(owner, 'Member has voice');
      await roomSetting(owner, '#rs-chat', false); await chatEnabled(owner, false); await chatEnabled(member, false);
      await roomSetting(owner, '#rs-chat', true); await roomSetting(owner, '#rs-moderated', false); await chatEnabled(guest, true);
      await action(owner, 'E2E Member', 'Mute Text'); await chatEnabled(member, false); await action(owner, 'E2E Member', 'Text Unmute'); await chatEnabled(member, true);
    });
    await step('offline private recipient disables compose; close removes conversation', async () => {
      await action(owner, 'E2E Member', 'Private message'); await leave(member);
      await owner.waitForFunction(() => document.querySelector('[aria-label="Conversation"]').selectedOptions[0]?.text.includes('offline'));
      await chatEnabled(owner, false); await setRole(owner, 'E2E Member', 3);
      await owner.getByRole('button', { name: 'Close PM', exact: true }).click(); await chatEnabled(owner, true);
      await join(member, 'E2E Member'); await member.locator('#community-actions').getByRole('button', { name: 'Manage room', exact: true }).waitFor({ state: 'visible' });
    });
    await step('ban review/unban and rejoin', async () => {
      await action(owner, 'E2E Member', 'Ban…'); const ban = owner.getByRole('dialog', { name: 'Ban E2E Member', exact: true });
      await ban.getByLabel('Reason (optional)', { exact: true }).fill('Disposable browser test ban'); await ban.getByRole('button', { name: 'Ban from room', exact: true }).click();
      await member.locator('#join-screen').waitFor({ state: 'visible' });
      const manage = await header(owner, 'Manage room'); await manage.getByRole('button', { name: 'Bans', exact: true }).click(); await visible(owner, 'Disposable browser test ban');
      await manage.getByRole('button', { name: 'Unban', exact: true }).click(); await visible(owner, 'No bans on this page.'); await close(manage); await join(member, 'E2E Member');
    });
    await step('local media preview, real camera/audio receive and personal hide/restore', async () => {
      await guest.locator('#mic-setup-btn').click(); const setup = guest.getByRole('dialog', { name: 'Camera & microphone', exact: true });
      await setup.getByLabel('Camera preview', { exact: true }).check(); await setup.getByRole('button', { name: 'Start preview', exact: true }).click();
      await guest.waitForFunction(() => document.querySelector('.media-preview-video')?.videoWidth > 0);
      assert.equal(await owner.locator('.video-tile:not(.local) video').count(), 0, 'preview must not broadcast');
      await setup.getByRole('button', { name: 'Stop preview', exact: true }).click(); await guest.waitForFunction(() => !document.querySelector('.media-preview-video')?.srcObject);
      await setup.getByRole('button', { name: 'Save settings', exact: true }).click(); await setup.waitFor({ state: 'hidden' });
      await guest.locator('#cam-btn').click(); await remotePlayback(owner, 'video');
      await guest.locator('#mic-btn').click(); await remotePlayback(owner, 'audio');
      const audioTime = await owner.locator('.video-tile:not(.local) audio').first().evaluate(audio => audio.currentTime);
      await owner.waitForFunction(previous => [...document.querySelectorAll('.video-tile:not(.local) audio')].some(audio => !audio.paused && !audio.muted && audio.volume > 0 && audio.currentTime > previous + 0.25), audioTime);
      await owner.locator('.personal-media-controls summary').first().click();
      // Deterministic UI recovery control: inject a policy rejection on this one
      // element, then restore native play() before the real user-gesture retry.
      // This is separate from the unmodified native playback assertion above.
      const remoteAudio = owner.locator('.video-tile:not(.local) audio').first();
      await remoteAudio.evaluate(audio => {
        audio.autoplay = false; audio.pause();
        audio.play = () => Promise.reject(new DOMException('Simulated autoplay denial', 'NotAllowedError'));
      });
      try {
        await owner.locator('[data-control="volume"]').first().press('ArrowLeft');
        const retry = owner.getByRole('button', { name: 'Enable playback for E2ERenamed', exact: true });
        await retry.waitFor({ state: 'visible' });
        await owner.screenshot({ path: path.join(artifacts, 'playback-recovery.png'), fullPage: true });
        await remoteAudio.evaluate(audio => { delete audio.play; });
        await retry.click(); await retry.waitFor({ state: 'hidden' });
        const previous = await remoteAudio.evaluate(audio => audio.currentTime);
        await owner.waitForFunction(time => [...document.querySelectorAll('.video-tile:not(.local) audio')].some(audio => !audio.paused && !audio.muted && audio.volume > 0 && audio.currentTime > time + 0.25), previous);
        report.simulatedAutoplayRecovery = true;
      } finally { await deadline(remoteAudio.evaluate(audio => { delete audio.play; audio.autoplay = true; })).catch(() => {}); }
      await owner.getByRole('combobox', { name: 'Video quality for E2ERenamed', exact: true }).selectOption('low');
      await owner.getByRole('button', { name: 'Hide for me', exact: true }).first().click(); await owner.waitForFunction(() => [...document.querySelectorAll('.video-tile:not(.local) video')].every(video => video.paused));
      await owner.getByRole('button', { name: 'Restore broadcast', exact: true }).first().click(); await owner.waitForFunction(() => [...document.querySelectorAll('.video-tile:not(.local) video')].some(video => !video.paused && video.videoWidth > 0));
      await owner.screenshot({ path: path.join(artifacts, 'remote-media.png'), fullPage: true });
      await guest.locator('#cam-btn').click(); await guest.locator('#mic-btn').click();
    });
    await step('simulated capture termination updates both clients and permits explicit restart', async () => {
      await guest.locator('#cam-btn').click();
      await guest.locator('#cam-btn:not(.muted)').waitFor({ state: 'visible' });
      await remotePlayback(owner, 'video');
      await guest.locator('#mic-btn').click();
      await guest.locator('#mic-btn:not(.muted)').waitFor({ state: 'visible' });
      await remotePlayback(owner, 'audio');
      report.simulatedCaptureStops = [];
      // Probe an unpublished clone, not the broadcast. Firefox can suppress
      // dispatchEvent() delivery to track.addEventListener() (Mozilla 1473457).
      // Do not replace real handlers or count an unsupported simulation as a pass.
      report.syntheticTrackEndedSupported = await guest.evaluate(() => {
        const stream = document.querySelector('#local-tile video')?.srcObject;
        if (!stream) throw new Error('Expected the active local capture stream');
        return stream.getTracks().every(track => {
          const probe = track.clone();
          let observed = false;
          probe.addEventListener('ended', () => { observed = true; }, { once: true });
          probe.stop(); probe.dispatchEvent(new Event('ended'));
          return observed;
        });
      });
      if (!report.syntheticTrackEndedSupported) {
        await guest.locator('#cam-btn').click(); await guest.locator('#mic-btn').click();
        return { skip: 'Engine suppresses synthetic track ended events; native device termination still needs manual testing.' };
      }
      for (const kind of ['audio', 'video']) {
        const stopped = await guest.evaluate(mediaKind => {
          const stream = document.querySelector('#local-tile video')?.srcObject;
          const tracks = stream?.getTracks().filter(track => track.kind === mediaKind && track.readyState === 'live') || [];
          if (tracks.length !== 1) throw new Error(`Expected one live owned ${mediaKind} capture track, found ${tracks.length}`);
          const requestsBeforeStop = window.__communityCaptureRequests;
          // stop() is deliberately silent in browsers. Dispatch the external-end
          // event separately; this is not a physical unplug/permission-revoke test.
          tracks[0].stop(); tracks[0].dispatchEvent(new Event('ended'));
          return { requestsBeforeStop };
        }, kind);
        const button = kind === 'audio' ? '#mic-btn' : '#cam-btn';
        const otherButton = kind === 'audio' ? '#cam-btn' : '#mic-btn';
        await guest.locator(`${button}.muted`).waitFor({ state: 'visible' });
        await visible(guest, kind === 'audio'
          ? 'Microphone stopped. Click Unmute (M) to restart.'
          : 'Camera stopped. Click Cam On (V) to restart.');
        assert.equal(await guest.locator(`${otherButton}.muted`).count(), 0, 'other capture stays enabled');
        await owner.locator(`.video-tile:not(.local) ${kind}`).waitFor({ state: 'detached' });
        const otherKind = kind === 'audio' ? 'video' : 'audio';
        await remotePlayback(owner, otherKind);
        assert.equal(await guest.evaluate(() => window.__communityCaptureRequests), stopped.requestsBeforeStop, 'external stop must not recapture');
        await guest.screenshot({ path: path.join(artifacts, `capture-stopped-${kind}.png`), fullPage: true });
        await guest.locator(button).click();
        await guest.locator(`${button}:not(.muted)`).waitFor({ state: 'visible' });
        await remotePlayback(owner, kind);
        assert.equal(await guest.evaluate(() => window.__communityCaptureRequests), stopped.requestsBeforeStop + 1, 'explicit restart captures once');
        report.simulatedCaptureStops.push({ kind, stopped: true, restarted: true });
      }
      await guest.locator('#cam-btn').click(); await guest.locator('#mic-btn').click();
    });
    await step('account profile/avatar update and safe public profile', async () => {
      const account = await header(owner, 'Account'); await account.getByLabel('Account display name', { exact: true }).fill('E2E Profile Owner');
      await account.getByLabel('Bio', { exact: true }).fill('Bio from the browser smoke.'); await upload(owner, account);
      const saved = owner.waitForResponse(response => response.url().endsWith('/api/auth/profile') && response.request().method() === 'PATCH');
      await account.getByRole('button', { name: 'Save profile', exact: true }).click(); assert.equal((await saved).status(), 200); await close(account);
      await action(guest, 'E2E Owner', 'View profile'); const profile = guest.getByRole('dialog', { name: 'Profile', exact: true });
      await visible(guest, 'E2E Profile Owner'); await visible(guest, 'Bio from the browser smoke.'); await profile.getByRole('img').waitFor({ state: 'visible' });
      assert.equal(await profile.getByText(ownerEmail, { exact: true }).count(), 0); await close(profile);
    });
    await step('My rooms edits identity/image and refreshes active room topic', async () => {
      const mine = await header(owner, 'My rooms'); await mine.getByRole('button', { name: 'Edit room', exact: true }).click();
      const edit = owner.getByRole('dialog', { name: 'Edit E2E Community Room', exact: true });
      await edit.getByLabel('Room display name', { exact: true }).fill('E2E Edited Room'); await edit.getByLabel('Topic', { exact: true }).fill('Edited live topic');
      await edit.getByLabel('Description / room rules', { exact: true }).fill('Browser-tested room description'); await upload(owner, edit, true);
      await edit.getByRole('button', { name: 'Save room', exact: true }).click(); await edit.waitFor({ state: 'hidden' }); await visible(owner, 'E2E Edited Room'); await close(mine);
      await guest.locator('#room-topic').filter({ hasText: 'Edited live topic' }).waitFor({ state: 'visible' });
    });
    let recoveryKey;
    await step('save-once recovery key and password change signs out current room', async () => {
      const account = await header(owner, 'Account'); await account.getByLabel('Current password', { exact: true }).fill(password);
      await account.getByRole('button', { name: 'Generate recovery key', exact: true }).click(); const keyDialog = owner.getByRole('dialog', { name: 'Save your recovery key', exact: true });
      recoveryKey = await keyDialog.getByLabel('Recovery key', { exact: true }).inputValue(); assert.match(recoveryKey, /^sc-recovery-/); await close(keyDialog);
      await account.getByLabel('Current password', { exact: true }).fill(password); await account.getByLabel('New password', { exact: true }).fill(`${password}changed`); await account.getByLabel('Confirm new password', { exact: true }).fill(`${password}changed`);
      await account.getByRole('button', { name: 'Change password', exact: true }).click(); await owner.locator('#sign-in-btn').waitFor({ state: 'visible' });
      await login(owner, ownerEmail, `${password}changed`); await owner.locator('#logout-btn').click(); await owner.locator('#sign-in-btn').waitFor({ state: 'visible' });
    });
    await step('saved key recovery, one-time replay rejection and new-password login', async () => {
      await owner.locator('#sign-in-btn').click(); await owner.getByRole('button', { name: 'Recover with a saved key', exact: true }).click();
      let recovery = owner.getByRole('dialog', { name: 'Recover account', exact: true });
      for (let attempt = 0; attempt < 2; attempt++) {
        await recovery.getByLabel('Email', { exact: true }).fill(ownerEmail); await recovery.getByLabel('Saved recovery key', { exact: true }).fill(recoveryKey);
        await recovery.getByLabel('New password', { exact: true }).fill(`${password}recovered`); await recovery.getByLabel('Confirm new password', { exact: true }).fill(`${password}recovered`);
        await recovery.getByRole('button', { name: 'Reset password with key', exact: true }).click();
        if (!attempt) { await recovery.waitFor({ state: 'hidden' }); await owner.getByRole('button', { name: 'Recover with a saved key', exact: true }).click(); recovery = owner.getByRole('dialog', { name: 'Recover account', exact: true }); }
        else { await recovery.getByRole('alert').waitFor({ state: 'visible' }); await close(recovery); }
      }
      recoveryKey = ''; await login(owner, ownerEmail, `${password}recovered`); await join(owner, 'E2E Profile Owner');
    });
    await step('mobile layout and community dialog fit viewport', async () => {
      const mobile = await client('mobile', true); await join(mobile, 'Mobile Guest');
      assert.ok(await mobile.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'mobile page must not overflow horizontally');
      await header(mobile, 'Nickname'); const nickname = mobile.getByRole('dialog', { name: 'Room nickname', exact: true });
      const bounds = await nickname.boundingBox(); assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 391, 'mobile dialog must fit viewport');
      await mobile.screenshot({ path: path.join(artifacts, 'mobile-nickname.png'), fullPage: true }); await close(nickname); await leave(mobile);
      await owner.setViewportSize({ width: 390, height: 844 });
      assert.ok(await owner.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'signed-in mobile header must not overflow');
      const account = await header(owner, 'Account'); await account.getByRole('button', { name: 'Save profile', exact: true }).waitFor();
      const accountBounds = await account.boundingBox(); assert.ok(accountBounds.x >= 0 && accountBounds.x + accountBounds.width <= 391, 'mobile account dialog must fit');
      await owner.screenshot({ path: path.join(artifacts, 'mobile-account.png'), fullPage: true }); await close(account); await owner.setViewportSize({ width: 1440, height: 1000 });
    });
    await step('owned room typed-confirmation deletion disconnects participants', async () => {
      let mine = await header(owner, 'My rooms'); await mine.getByRole('button', { name: 'Delete room…', exact: true }).click();
      const remove = owner.getByRole('dialog', { name: 'Delete room', exact: true }); await remove.getByLabel(`Type ${runId} to confirm`, { exact: true }).fill(runId);
      await remove.getByRole('button', { name: 'Permanently delete room', exact: true }).click(); await remove.waitFor({ state: 'hidden' });
      await owner.locator('#join-screen').waitFor({ state: 'visible' });
      if (!await mine.isVisible()) mine = await header(owner, 'My rooms');
      await visible(owner, 'No owned rooms yet. Use Create Room on the join screen.'); await close(mine);
      await guest.locator('#join-screen').waitFor({ state: 'visible' }); await member.locator('#join-screen').waitFor({ state: 'visible' });
    });
    for (const item of clients) assert.deepEqual(item.errors, [], `${item.label} page errors`);
    report.complete = true; report.passed = true;
    console.log(`PASS complete community smoke (${runId}); artifacts: ${artifacts}`);
  } catch (error) {
    report.failedStep = activeStep; report.error = error.message;
    saveReport();
    report.diagnostics = [];
    for (const item of clients) {
      const media = await deadline(item.page.evaluate(async () => Promise.all(window.__communityPeers.map(async peer => {
        const state = { connectionState: peer.connectionState, iceConnectionState: peer.iceConnectionState, signalingState: peer.signalingState };
        if (peer.connectionState === 'closed') return state;
        let timer;
        try {
          const stats = await Promise.race([peer.getStats(), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('stats deadline')), 2000); })]);
          return { ...state, stats: [...stats.values()].filter(stat => ['transport', 'candidate-pair', 'local-candidate', 'remote-candidate', 'inbound-rtp', 'outbound-rtp', 'codec'].includes(stat.type)) };
        } catch (failure) { return { ...state, error: failure.message }; }
        finally { clearTimeout(timer); }
      })))).catch(failure => ({ error: failure.message }));
      report.diagnostics.push({ label: item.label, pageErrors: item.errors, warnings: item.warnings, media });
      saveReport();
    }
    console.error(`FAIL ${activeStep}`);
    for (const item of clients) { await item.page.screenshot({ path: path.join(artifacts, `failure-${item.label}.png`), fullPage: true, mask: [item.page.locator('input[type="password"], input[readonly], textarea[readonly]')] }).catch(() => {}); console.error(`${item.label} page errors:`, item.errors); console.error(`${item.label} last signaling events:`, JSON.stringify(item.frames)); }
    console.error(`Failure screenshots: ${artifacts}`); throw error;
  } finally {
    for (const item of clients) {
      await deadline(item.page.locator('dialog[open]').evaluateAll(dialogs => dialogs.forEach(dialog => dialog.close()))).catch(() => {});
      if (await item.page.locator('#leave-btn').isVisible().catch(() => false)) await item.page.locator('#leave-btn').click().catch(() => {});
    }
    try { await browser?.close(); }
    catch (error) { report.complete = false; report.passed = false; report.cleanupError = error.message; throw error; }
    finally { report.finishedAt = new Date().toISOString(); saveReport(); }
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
