/** Opt-in accessibility smoke against owned disposable loopback services. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { browserOptions } = require('./browser-options.cjs');

const tags = Object.freeze(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']);
// Pinned axe color-contrast reason codes; never retain free-form check payloads.
const contrastReasonCodes = new Set([
  'bgImage',
  'bgGradient',
  'imgNode',
  'bgOverlap',
  'fgAlpha',
  'elmPartiallyObscured',
  'elmPartiallyObscuring',
  'outsideViewport',
  'equalRatio',
  'shortTextContent',
  'nonBmp',
  'pseudoContent',
  'colorParse',
  'complexTextShadows',
]);

function configuration(env) {
  if (env.ACCESSIBILITY_E2E !== '1' || env.DISPOSABLE_TEST_DATABASE !== '1') {
    throw new Error(
      'Set ACCESSIBILITY_E2E=1 and DISPOSABLE_TEST_DATABASE=1 for owned test services.',
    );
  }
  const base = new URL(env.BASE_URL || 'http://127.0.0.1:3119');
  if (
    base.protocol !== 'http:' ||
    !['localhost', '127.0.0.1', '[::1]'].includes(base.hostname) ||
    base.username ||
    base.password ||
    base.pathname !== '/' ||
    base.search ||
    base.hash
  )
    throw new Error(
      'Accessibility smoke requires a loopback HTTP origin without credentials or paths.',
    );
  return { base: base.origin, options: browserOptions(env.E2E_BROWSER) };
}

function summarizeFindings(findings) {
  return {
    ruleCount: findings.length,
    omittedRules: Math.max(0, findings.length - 50),
    rules: findings.slice(0, 50).map((finding) => ({
      id: String(finding.id).slice(0, 100),
      impact: finding.impact ?? null,
      nodeCount: finding.nodes.length,
      omittedNodes: Math.max(0, finding.nodes.length - 20),
      targets: finding.nodes.slice(0, 20).map((node) => JSON.stringify(node.target).slice(0, 512)),
      reasonCodes: finding.nodes.slice(0, 20).map((node) =>
        [...(node.any ?? []), ...(node.all ?? []), ...(node.none ?? [])]
          .filter(
            (check) =>
              check.id === 'color-contrast' && contrastReasonCodes.has(check.data?.messageKey),
          )
          .slice(0, 8)
          .map((check) => check.data.messageKey),
      ),
    })),
  };
}

/** Only approve opaque computed RGB pairs; unsupported colors stay unverified. */
function opaqueContrastRatio(foreground, background) {
  const luminance = (value) => {
    const match =
      /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)(?:\s*,\s*([\d.]+))?\s*\)$/.exec(value);
    if (!match || (match[4] !== undefined && Number(match[4]) !== 1)) return null;
    const channels = match.slice(1, 4).map(Number);
    if (channels.some((channel) => !Number.isFinite(channel) || channel < 0 || channel > 255))
      return null;
    const linear = channels.map((channel) => {
      const value = channel / 255;
      return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    });
    return linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
  };
  const first = luminance(foreground);
  const second = luminance(background);
  return first === null || second === null
    ? null
    : (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
}

async function scanPage(page, name, AxeBuilder) {
  const result = await new AxeBuilder({ page }).withTags([...tags]).analyze();
  const targets = [...result.violations, ...result.incomplete]
    .filter((finding) => finding.id === 'color-contrast')
    .flatMap((finding) => finding.nodes.map((node) => node.target))
    .filter((target) => target.length === 1 && typeof target[0] === 'string')
    .slice(0, 50)
    .map((target) => target[0].slice(0, 512));
  // Incomplete checks need evidence too. Keep computed CSS/geometry, never HTML,
  // text, form values or image URLs. A hit-test sample does not prove full visibility.
  const contrastStyles = targets.length
    ? await page.evaluate(
        (selectors) =>
          selectors.map((target) => {
            try {
              const node = document.querySelector(target);
              if (!node) return { target, unavailable: true };
              const style = getComputedStyle(node);
              const imageKind = (value) =>
                !value || value === 'none'
                  ? 'none'
                  : !value.includes('url(') &&
                      /^(?:repeating-)?(?:linear|radial|conic)-gradient\(/.test(value)
                    ? 'gradient'
                    : 'other';
              const box = (rect) => ({
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
              });
              const bounds = node.getBoundingClientRect();
              const fragments = [];
              const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
              let textNode;
              let inspectedTextNodes = 0;
              while (
                inspectedTextNodes < 32 &&
                fragments.length < 32 &&
                (textNode = walker.nextNode())
              ) {
                inspectedTextNodes++;
                if (!textNode.textContent.trim()) continue;
                const range = document.createRange();
                range.selectNodeContents(textNode);
                for (const rect of range.getClientRects()) {
                  if (fragments.length >= 32) break;
                  const x = rect.x + rect.width / 2;
                  const y = rect.y + rect.height / 2;
                  const top = document.elementFromPoint(x, y);
                  fragments.push({
                    ...box(rect),
                    centerHitsTarget: top !== null && node.contains(top),
                  });
                }
              }
              const backgrounds = [];
              for (
                let ancestor = node.parentElement;
                ancestor && backgrounds.length < 6;
                ancestor = ancestor.parentElement
              ) {
                const ancestorStyle = getComputedStyle(ancestor);
                backgrounds.push({
                  color: ancestorStyle.backgroundColor,
                  opacity: ancestorStyle.opacity,
                  imageKind: imageKind(ancestorStyle.backgroundImage),
                  overflowX: ancestorStyle.overflowX,
                  overflowY: ancestorStyle.overflowY,
                  bounds: box(ancestor.getBoundingClientRect()),
                });
              }
              return {
                target,
                color: style.color,
                backgroundColor: style.backgroundColor,
                opacity: style.opacity,
                fontSize: style.fontSize,
                fontWeight: style.fontWeight,
                textFillColor: style.webkitTextFillColor,
                backgroundImageKind: imageKind(style.backgroundImage),
                bounds: box(bounds),
                textFragments: fragments,
                textFragmentsLimit: 32,
                textNodesLimit: 32,
                backgrounds,
              };
            } catch {
              return { target, unavailable: true };
            }
          }),
        targets,
      )
    : [];
  return {
    name,
    axeVersion: result.testEngine.version,
    violations: summarizeFindings(result.violations),
    incomplete: summarizeFindings(result.incomplete),
    contrastStyles,
  };
}

async function run(env = process.env) {
  const { base, options } = configuration(env);
  const artifacts =
    env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-accessibility.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const report = {
    startedAt: new Date().toISOString(),
    complete: false,
    passed: false,
    browser: options.name,
    baseUrl: base,
    tags: [...tags],
    scans: [],
    checks: [],
    pageErrorCount: 0,
    limitations: [
      'Automated WCAG-tagged rule checks and selected keyboard paths, not full WCAG conformance.',
      'Incomplete axe findings need manual review; no rule disabling or selector exclusions.',
      'Isolated headless desktop browser and resized viewport, not screen-reader or mobile-browser coverage.',
      'Contrast geometry samples at most 32 text nodes/fragments per target; center hit tests do not establish full visibility or focus conformance.',
    ],
  };
  const save = () =>
    fs.writeFileSync(
      path.join(artifacts, 'accessibility-results.json'),
      `${JSON.stringify(report, null, 2)}\n`,
      { mode: 0o600 },
    );
  let browser;
  let activeStep = 'launch';
  let originalError;
  const scan = async (page, name, AxeBuilder) => {
    activeStep = name;
    // Opening fades temporarily change contrast; inspect the settled UI without
    // disabling motion or changing application styles.
    await page.waitForFunction(
      () =>
        document
          .getAnimations()
          .every(
            (animation) =>
              animation.effect?.getComputedTiming().iterations === Infinity ||
              ['finished', 'idle'].includes(animation.playState),
          ),
      null,
      { timeout: 3000 },
    );
    const result = await scanPage(page, name, AxeBuilder);
    report.scans.push(result);
    save();
    console.log(
      `SCAN ${name}: ${result.violations.ruleCount} violations, ${result.incomplete.ruleCount} manual-review rules`,
    );
  };
  const check = async (name, work) => {
    activeStep = name;
    await work();
    report.checks.push({ name, passed: true });
    save();
  };
  const scanBothSizes = async (page, name, AxeBuilder) => {
    await scan(page, `${name}-desktop`, AxeBuilder);
    await page.setViewportSize({ width: 320, height: 800 });
    await scan(page, `${name}-mobile-320`, AxeBuilder);
    await check(`${name}-mobile has no horizontal overflow`, async () => {
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
        true,
      );
    });
    await page.setViewportSize({ width: 1440, height: 1000 });
  };
  save();
  try {
    const playwright = require(env.PLAYWRIGHT_MODULE || 'playwright');
    report.playwrightVersion = require(
      path.join(
        path.dirname(require.resolve(env.PLAYWRIGHT_MODULE || 'playwright')),
        'package.json',
      ),
    ).version;
    browser = await playwright[options.name].launch(options.launchOptions);
    const { default: AxeBuilder } = require('@axe-core/playwright');
    report.browserVersion = browser.version();
    const context = await browser.newContext({
      ...options.contextOptions,
      viewport: { width: 1440, height: 1000 },
    });
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    page.on('pageerror', () => {
      report.pageErrorCount++;
    });
    activeStep = 'load public join';
    await page.goto(base, { waitUntil: 'networkidle' });
    await page.locator('.conversation-toolbar').waitFor({ state: 'attached' });
    await scan(page, 'public-join-desktop', AxeBuilder);
    await page.screenshot({ path: path.join(artifacts, 'join-desktop.png') });
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-email').waitFor({ state: 'visible' });
    await scanBothSizes(page, 'sign-in', AxeBuilder);
    await page.locator('#login-to-register').click();
    await page.locator('#register-email').waitFor({ state: 'visible' });
    await scanBothSizes(page, 'register', AxeBuilder);

    activeStep = 'register owned account';
    const runId = `a11y-${Date.now().toString(36)}`;
    await page.locator('#register-email').fill(`${runId}@example.test`);
    await page.locator('#register-name').fill('Accessibility Owner');
    await page.locator('#register-password').fill('Disposable-accessibility-password-2026!');
    await page.locator('#register-confirm').fill('Disposable-accessibility-password-2026!');
    await page.locator('#register-submit').click();
    await page.locator('#auth-bar-user').waitFor({ state: 'visible' });
    await page.locator('#create-room-btn').click();
    await page.locator('#cr-id').waitFor({ state: 'visible' });
    await scanBothSizes(page, 'create-room', AxeBuilder);
    await page.locator('#cr-id').fill(runId);
    await page.locator('#cr-name').fill('Accessibility room');
    await page.locator('#create-room-submit').click();
    await page.locator('#room-screen').waitFor({ state: 'visible' });
    await page.waitForFunction(
      () => document.querySelector('#connection-status').textContent === 'Connected',
    );
    await page
      .getByRole('combobox', { name: 'Conversation', exact: true })
      .waitFor({ state: 'visible' });
    await scan(page, 'joined-chat-desktop', AxeBuilder);
    await check('participant initials have readable rendered contrast', async () => {
      const colors = await page.locator('.participant-avatar').evaluateAll((avatars) =>
        avatars
          .filter((avatar) => avatar.textContent.trim())
          .map((avatar) => {
            const style = getComputedStyle(avatar);
            return {
              color: style.color,
              backgroundColor: style.backgroundColor,
              opacity: style.opacity,
            };
          }),
      );
      assert.ok(colors.length > 0, 'The owned participant must have an initial');
      report.avatarContrast = colors.map((pair) => ({
        ...pair,
        ratio: opaqueContrastRatio(pair.color, pair.backgroundColor),
      }));
      for (const evidence of report.avatarContrast) {
        assert.equal(evidence.opacity, '1');
        assert.ok(
          evidence.ratio !== null && evidence.ratio >= 4.5,
          'Rendered initial needs normal-text contrast',
        );
      }
    });

    await check('newest-message button is keyboard-operable after real chat overflow', async () => {
      const content = 'Accessibility keyboard scrolling check. '.repeat(45).trim();
      await page.locator('#chat-input').fill(content);
      await page.locator('#chat-send-btn').click();
      await page.waitForFunction(
        (text) =>
          [...document.querySelectorAll('.chat-msg')].some(
            (node) =>
              node.querySelector('.msg-text')?.textContent === text &&
              !node.dataset.messageId.startsWith('pending:'),
          ),
        content,
      );
      const messages = page.locator('#chat-messages');
      assert.equal(
        await messages.evaluate((node) => node.scrollHeight > node.clientHeight + 100),
        true,
        'real message creates overflow',
      );
      await messages.evaluate((node) => {
        node.scrollTop = 0;
      });
      const newest = page.getByRole('button', { name: 'Scroll to newest messages', exact: true });
      await newest.waitFor({ state: 'visible' });
      assert.equal(await newest.evaluate((node) => node.tagName), 'BUTTON');
      await newest.focus();
      await page.keyboard.press('Enter');
      await newest.waitFor({ state: 'hidden' });
      assert.equal(
        await messages.evaluate(
          (node) => node.scrollHeight - node.scrollTop - node.clientHeight < 48,
        ),
        true,
      );
    });

    const settings = async (prefix) => {
      const opener = page.locator('#settings-btn');
      await opener.focus();
      await opener.press('Enter');
      const dialog = page.getByRole('dialog', { name: 'Your settings', exact: true });
      await dialog.waitFor({ state: 'visible' });
      for (const name of ['Audio & video', 'Appearance']) {
        await dialog.getByRole('tab', { name, exact: true }).click();
        await scan(page, `${prefix}-personal-${name}`, AxeBuilder);
        if (prefix === 'desktop' && name === 'Audio & video')
          await page.screenshot({ path: path.join(artifacts, 'personal-settings-desktop.png') });
        if (prefix === 'mobile-320' && name === 'Audio & video')
          await page.screenshot({ path: path.join(artifacts, 'personal-settings-mobile-320.png') });
      }
      await dialog.getByRole('tab', { name: 'Audio & video', exact: true }).click();
      await dialog.getByText('Advanced capture settings', { exact: true }).click();
      await scan(page, `${prefix}-advanced-capture`, AxeBuilder);
      await check(`${prefix}-personal-Escape-restores-focus`, async () => {
        await page.keyboard.press('Escape');
        await dialog.waitFor({ state: 'hidden' });
        assert.equal(await opener.evaluate((node) => node === document.activeElement), true);
      });
      const roomOpener = page.locator('#room-settings-btn');
      await roomOpener.focus();
      await roomOpener.press('Enter');
      const room = page.getByRole('dialog', { name: 'Room settings', exact: true });
      for (const name of ['Basics', 'Access', 'Participation']) {
        await room.getByRole('tab', { name, exact: true }).click();
        await scan(page, `${prefix}-room-${name}`, AxeBuilder);
      }
      await check(`${prefix}-room-Escape-restores-focus`, async () => {
        await page.keyboard.press('Escape');
        await room.waitFor({ state: 'hidden' });
        assert.equal(await roomOpener.evaluate((node) => node === document.activeElement), true);
      });
      const accountOpener = page
        .locator('#community-actions')
        .getByRole('button', { name: 'Account', exact: true });
      await accountOpener.focus();
      await accountOpener.press('Enter');
      const account = page.getByRole('dialog', { name: 'Account', exact: true });
      await account
        .getByLabel('Account display name', { exact: true })
        .waitFor({ state: 'visible' });
      await scan(page, `${prefix}-account`, AxeBuilder);
      await check(`${prefix}-account-Escape-restores-focus`, async () => {
        await page.keyboard.press('Escape');
        await account.waitFor({ state: 'hidden' });
        assert.equal(await accountOpener.evaluate((node) => node === document.activeElement), true);
      });
    };
    await settings('desktop');
    await page.setViewportSize({ width: 320, height: 800 });
    await scan(page, 'joined-chat-mobile-320', AxeBuilder);
    await page.screenshot({ path: path.join(artifacts, 'joined-chat-mobile-320.png') });
    await settings('mobile-320');
    await check('joined mobile document has no horizontal overflow', async () => {
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
        true,
      );
    });

    await check('Help opens a separate tab without leaving the room', async () => {
      const label = await page.locator('#room-label').textContent();
      const opened = page.waitForEvent('popup');
      await page.getByRole('link', { name: 'Help (opens in a new tab)', exact: true }).click();
      const help = await opened;
      try {
        await help.waitForLoadState('networkidle');
        assert.equal(new URL(help.url()).pathname, '/help.html');
        assert.equal(await help.locator('script').count(), 0, 'help document contains no scripts');
        await help.setViewportSize({ width: 320, height: 800 });
        for (const colorScheme of ['light', 'dark']) {
          await help.emulateMedia({ colorScheme });
          await scan(help, `help-${colorScheme}-320`, AxeBuilder);
          assert.equal(
            await help.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1),
            true,
          );
        }
        await page.locator('#room-screen').waitFor({ state: 'visible' });
        assert.equal(await page.locator('#room-label').textContent(), label);
        assert.equal(await page.locator('#connection-status').textContent(), 'Connected');
      } finally {
        await help.close();
      }
    });
    const anonymous = await browser.newContext({
      ...options.contextOptions,
      viewport: { width: 320, height: 800 },
    });
    const publicPage = await anonymous.newPage();
    publicPage.on('pageerror', () => {
      report.pageErrorCount++;
    });
    await publicPage.goto(base, { waitUntil: 'networkidle' });
    await publicPage.locator('.conversation-toolbar').waitFor({ state: 'attached' });
    await scan(publicPage, 'public-join-mobile-320', AxeBuilder);
    await anonymous.close();
    await page.locator('#leave-btn').click();
    await page.locator('#join-screen').waitFor({ state: 'visible' });
    report.complete = true;
    report.violationCount = report.scans.reduce(
      (total, entry) => total + entry.violations.ruleCount,
      0,
    );
    report.manualReviewCount = report.scans.reduce(
      (total, entry) => total + entry.incomplete.ruleCount,
      0,
    );
    report.passed =
      report.pageErrorCount === 0 &&
      report.scans.every((entry) => entry.violations.ruleCount === 0);
    if (!report.passed) {
      activeStep = 'scan result checks';
      throw new Error('Accessibility checks reported violations or page errors');
    }
  } catch (error) {
    originalError = error;
    report.passed = false;
    report.failedStep = activeStep;
    // Playwright exception messages can embed DOM content. Keep only the step.
    report.failure = 'Accessibility smoke did not pass; inspect scan results and the failed step.';
  } finally {
    try {
      await browser?.close();
    } catch (error) {
      originalError ??= error;
      report.cleanupFailed = true;
      report.complete = false;
      report.passed = false;
    }
    report.finishedAt = new Date().toISOString();
    save();
  }
  if (originalError)
    throw new Error(
      `Accessibility smoke failed during ${report.failedStep || activeStep}; report: ${artifacts}`,
    );
  console.log(`PASS accessibility smoke: ${report.scans.length} scans; report: ${artifacts}`);
  return report;
}

module.exports = { configuration, summarizeFindings, opaqueContrastRatio, scanPage, tags, run };
if (require.main === module)
  run().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
