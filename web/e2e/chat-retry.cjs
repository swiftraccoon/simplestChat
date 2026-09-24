/** Owned disposable loopback only; drop a bounded selection of real WS frames.
 * Message content, account/session IDs and raw signaling stay in memory. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { browserOptions } = require('./browser-options.cjs');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://127.0.0.1:3119');
if (
  process.env.CHAT_RETRY_E2E !== '1' ||
  process.env.DISPOSABLE_TEST_DATABASE !== '1' ||
  origin.protocol !== 'http:' ||
  !['127.0.0.1', 'localhost', '[::1]'].includes(origin.hostname) ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error('Chat retry requires explicit opt-in and owned disposable loopback services.');
const options = browserOptions(process.env.E2E_BROWSER);
const artifacts =
  process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-chat-retry.'));
fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
const report = {
  browser: options.name,
  passed: false,
  pageErrors: 0,
  checks: [],
  failureStage: null,
  limitations: [
    'Three controlled frame-loss cases against an owned native server; no claim about arbitrary network partitions or expired receipt windows.',
  ],
};
const scenarios = [];
const contexts = [];
const roomName = `chat-retry-${Date.now().toString(36)}`;
let active;
let invalidFrames = 0;
let stage = 'launch';

function frame(raw) {
  try {
    if (Buffer.byteLength(raw) > 262144) throw new Error('Frame exceeds fixture bound');
    return JSON.parse(String(raw));
  } catch {
    invalidFrames++;
    return null;
  }
}

async function client(browser, label, sender) {
  const context = await browser.newContext({
    ...options.contextOptions,
    viewport: { width: 1280, height: 900 },
  });
  contexts.push(context);
  const observed = { snapshot: false };
  await context.routeWebSocket(`${origin.origin.replace(/^http/, 'ws')}/ws`, (route) => {
    const server = route.connectToServer();
    route.onMessage((raw) => {
      const message = frame(raw);
      if (!message) return;
      if (sender && active && ['chatMessage', 'privateMessage'].includes(message.type)) {
        active.firstSends++;
        active.original = message;
        if (active.dropSend && active.firstSends === 1) {
          active.droppedSends++;
          return;
        }
      }
      if (sender && active && message.type === 'retryChatMessage') {
        active.retries++;
        active.sameIdentity =
          message.clientMessageId === active.original?.clientMessageId &&
          message.sequence === active.original?.sequence &&
          message.content === active.original?.content &&
          message.targetParticipantId === active.original?.targetParticipantId &&
          typeof message.chatSessionId === 'string' &&
          message.chatSessionId.length > 0;
      }
      server.send(raw);
    });
    server.onMessage((raw) => {
      const message = frame(raw);
      if (!message) return;
      if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot')
        observed.snapshot = typeof message.data?.chatSessionId === 'string';
      if (sender && message.type === 'messageAck') {
        const scenario = scenarios.find(
          (entry) => entry.original?.clientMessageId === message.clientMessageId,
        );
        if (scenario) {
          scenario.acknowledgements++;
          if (scenario.dropAck && scenario.droppedAcks === 0) {
            scenario.droppedAcks++;
            return;
          }
        }
      }
      if (!sender && ['chatReceived', 'privateMessageReceived'].includes(message.type)) {
        const entry = message.type === 'chatReceived' ? message : message.message;
        const scenario = scenarios.find(
          (item) => item.original?.clientMessageId === entry?.clientMessageId,
        );
        if (scenario) scenario.deliveries++;
      }
      route.send(raw);
    });
  });
  const page = await context.newPage();
  page.on('pageerror', () => {
    report.pageErrors = Math.min(100, report.pageErrors + 1);
  });
  page.setDefaultTimeout(20000);
  await page.addInitScript(() => {
    window.__chatRetryCaptures = 0;
    navigator.mediaDevices.getUserMedia = () => {
      window.__chatRetryCaptures++;
      return Promise.reject(new Error('Capture is forbidden in owned chat checks'));
    };
  });
  await page.goto(origin.toString());
  await page.locator('#name-input').fill(label);
  await page.locator('#room-input').fill(roomName);
  await page.locator('#join-btn').click();
  await page.locator('#room-screen').waitFor({ state: 'visible' });
  const deadline = Date.now() + 15000;
  while (!observed.snapshot && Date.now() < deadline) await page.waitForTimeout(50);
  assert.equal(
    observed.snapshot,
    true,
    'room snapshot supplies this owned membership retry session',
  );
  return page;
}

async function check(sender, receiver, name, dropSend, privateMessage = false) {
  stage = name;
  const scenario = {
    name,
    dropSend,
    dropAck: !dropSend,
    original: null,
    firstSends: 0,
    retries: 0,
    droppedSends: 0,
    droppedAcks: 0,
    acknowledgements: 0,
    deliveries: 0,
    sameIdentity: false,
  };
  scenarios.push(scenario);
  active = scenario;
  const content = `Owned retry fixture ${scenarios.length}`;
  await sender.locator('#chat-input').fill(content);
  await sender.locator('#chat-send-btn').click();
  const row = sender
    .locator('.chat-msg')
    .filter({ has: sender.getByText(content, { exact: true }) });
  await row
    .getByRole('button', { name: 'Retry same message', exact: true })
    .waitFor({ state: 'visible' });
  assert.equal(scenario.deliveries, dropSend ? 0 : 1, 'native receiver frame count before retry');
  await sender.locator('#chat-input').fill('Owned newer unsent draft');
  await row.getByRole('button', { name: 'Retry same message', exact: true }).click();
  await row.locator('.delivery-error').waitFor({ state: 'hidden' });
  await sender.waitForFunction((text) => {
    const node = [...document.querySelectorAll('.chat-msg')].find(
      (element) => element.querySelector('.msg-text')?.textContent === text,
    );
    return (
      node &&
      !node.textContent.includes('Sending…') &&
      !node.textContent.includes('Delivery not confirmed')
    );
  }, content);
  if (privateMessage) {
    const choice = await receiver
      .getByRole('combobox', { name: 'Conversation', exact: true })
      .locator('option')
      .evaluateAll((nodes) => nodes.find((node) => node.textContent.startsWith('Sender'))?.value);
    assert.equal(typeof choice, 'string');
    await receiver
      .getByRole('combobox', { name: 'Conversation', exact: true })
      .selectOption(choice);
  }
  await receiver.getByText(content, { exact: true }).waitFor({ state: 'visible' });
  await sender.waitForTimeout(350);
  assert.equal(scenario.firstSends, 1);
  assert.equal(scenario.retries, 1);
  assert.equal(
    scenario.sameIdentity,
    true,
    'retry preserves exact original ID, sequence, recipient and content',
  );
  assert.equal(
    scenario.deliveries,
    1,
    'native receiver frames prove no duplicate delivery before DOM deduplication',
  );
  assert.equal(scenario.droppedSends, dropSend ? 1 : 0);
  assert.equal(scenario.droppedAcks, dropSend ? 0 : 1);
  assert.equal(scenario.acknowledgements, dropSend ? 1 : 2);
  assert.equal(await receiver.getByText(content, { exact: true }).count(), 1);
  assert.equal(await sender.locator('#chat-input').inputValue(), 'Owned newer unsent draft');
  await sender.locator('#chat-input').fill('');
  report.checks.push({
    name,
    passed: true,
    firstSends: scenario.firstSends,
    retries: scenario.retries,
    nativeDeliveries: scenario.deliveries,
    nativeAcknowledgements: scenario.acknowledgements,
    droppedSends: scenario.droppedSends,
    droppedAcknowledgements: scenario.droppedAcks,
    sameIdentity: true,
  });
  active = null;
}

async function main() {
  const browser = await playwright[options.name].launch(options.launchOptions);
  try {
    stage = 'join';
    const sender = await client(browser, 'Sender', true);
    const receiver = await client(browser, 'Receiver', false);
    await check(sender, receiver, 'public-accepted-ack-lost', false);
    await check(sender, receiver, 'public-send-lost-before-server', true);
    stage = 'private-conversation';
    let opened = false;
    for (const action of await sender
      .getByRole('button', { name: 'Actions for Receiver', exact: true })
      .all()) {
      if (await action.isVisible()) {
        await action.click();
        opened = true;
        break;
      }
    }
    assert.equal(opened, true, 'the active layout exposes participant actions');
    await sender
      .locator('#mod-menu')
      .getByRole('button', { name: 'Private message', exact: true })
      .click();
    await check(sender, receiver, 'private-accepted-ack-lost', false, true);
    stage = 'cleanup';
    assert.equal(invalidFrames, 0);
    assert.equal(report.pageErrors, 0);
    for (const page of [sender, receiver]) {
      assert.equal(await page.evaluate(() => window.__chatRetryCaptures), 0);
      await page.locator('#leave-btn').click();
      await page.locator('#join-screen').waitFor({ state: 'visible' });
    }
    report.passed = true;
  } finally {
    report.failureStage = report.passed ? null : stage;
    await Promise.allSettled(contexts.map((context) => context.close()));
    await browser.close();
    fs.writeFileSync(
      path.join(artifacts, 'chat-retry.json'),
      `${JSON.stringify(report, null, 2)}\n`,
      { mode: 0o600 },
    );
  }
}
main()
  .then(() => console.log('Owned chat retry checks passed.'))
  .catch(() => {
    console.error(`Owned chat retry check failed at ${stage}; see bounded report.`);
    process.exitCode = 1;
  });
