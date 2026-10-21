/** Real tile rendering and browser layout with synthetic canvas video, no devices or backend. */
const assert = require('node:assert/strict');
const path = require('node:path');

let fixtureBundle;

/** Bundle actual modules for the browser without writing or replacing production assets. */
async function browserFixture() {
  fixtureBundle ??= (async () => {
    const { build } = await import('vite');
    const result = await build({
      configFile: false,
      root: path.resolve(__dirname, '..'),
      logLevel: 'error',
      build: {
        write: false,
        minify: false,
        lib: {
          entry: path.join(__dirname, 'media-layout-fixture.mjs'),
          formats: ['iife'],
          name: 'MediaLayoutFixture',
        },
      },
    });
    const outputs = Array.isArray(result) ? result : [result];
    assert.equal(outputs.length, 1, 'one fixture output');
    assert.ok('output' in outputs[0], 'completed fixture bundle');
    const chunks = outputs[0].output.filter((entry) => entry.type === 'chunk');
    assert.equal(chunks.length, 1, 'self-contained browser fixture');
    // CSS emitted from the real module imports is intentionally omitted: the
    // production page's built CSS is what these layout assertions exercise.
    return chunks[0].code;
  })();
  return fixtureBundle;
}

async function geometry(page) {
  return page.evaluate(() => {
    const grid = document.querySelector('#video-grid');
    const bounds = grid.getBoundingClientRect();
    const boxes = [...grid.querySelectorAll('.video-tile')].map((tile) => {
      const rect = tile.getBoundingClientRect();
      const video = tile.querySelector('video');
      const points = [
        [0.1, 0.1],
        [0.9, 0.1],
        [0.1, 0.9],
        [0.9, 0.9],
        [0.5, 0.5],
      ];
      return {
        name: tile.dataset.participantId,
        screen: tile.classList.contains('screen-share'),
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        ratio: video ? video.videoWidth / video.videoHeight : null,
        objectFit: video ? getComputedStyle(video).objectFit : null,
        horizontallyContained: rect.left >= bounds.left - 1 && rect.right <= bounds.right + 1,
        contained:
          rect.left >= bounds.left - 1 &&
          rect.right <= bounds.right + 1 &&
          rect.top >= bounds.top - 1 &&
          rect.bottom <= bounds.bottom + 1,
        unobscured: points.every(([x, y]) => {
          const hit = document.elementFromPoint(rect.x + rect.width * x, rect.y + rect.height * y);
          return hit !== null && tile.contains(hit);
        }),
      };
    });
    return {
      viewport: { width: innerWidth, height: innerHeight },
      grid: { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height },
      horizontalOverflow:
        document.documentElement.scrollWidth > innerWidth + 1 ||
        grid.scrollWidth > grid.clientWidth + 1,
      boxes,
    };
  });
}

async function mediaLayout(page, artifacts, report) {
  report.mediaScope =
    'Built production page/CSS with current source tile rendering, layout and controls; canvas video, no SFU or device capture.';
  await page.locator('.toast').evaluateAll((nodes) => nodes.forEach((node) => node.click()));
  // Propagate initialization failures before trying to use the fixture API.
  await page.evaluate(await browserFixture());
  await page.evaluate(() => {
    for (let index = 1; index <= 3; index++) window.__mediaLayout.add(`Camera ${index}`, 640, 360);
    window.__mediaLayout.add('Portrait share', 360, 640, true);
  });
  await page.waitForFunction(() => {
    const videos = [...document.querySelectorAll('#video-grid video')];
    return (
      videos.length === 4 && videos.every((video) => video.videoWidth > 0 && video.readyState >= 2)
    );
  });
  report.mediaLayouts = [];
  const inspect = async (name, fullyVisible) => {
    await page.mouse.move(0, 0);
    await page.waitForTimeout(350);
    const result = await geometry(page);
    report.mediaLayouts.push({ name, ...result });
    await page.screenshot({ path: path.join(artifacts, `media-${name}.png`) });
    assert.equal(result.horizontalOverflow, false, `${name}: no horizontal overflow`);
    const first = result.boxes[0];
    const top = Math.min(...result.boxes.map((tile) => tile.y));
    const columns = result.boxes
      .filter((tile) => Math.abs(tile.y - top) <= 1)
      .map((tile) => tile.x)
      .sort((left, right) => left - right);
    for (const [index, tile] of result.boxes.entries()) {
      assert.ok(
        Math.abs(tile.width - first.width) <= 1 && Math.abs(tile.height - first.height) <= 1,
        `${name}: ${tile.name} has the same width and height as every other tile`,
      );
      assert.ok(Math.abs(tile.width / tile.height - 16 / 9) < 0.035, `${name}: uniform 16:9 cell`);
      if (tile.objectFit !== null)
        assert.equal(tile.objectFit, 'contain', `${name}: the full video remains visible`);
      const column = result.boxes.filter(
        (other) => Math.abs(other.y - tile.y) <= 1 && other.x < tile.x - 1,
      ).length;
      assert.ok(
        Math.abs(tile.x - columns[column]) <= 1,
        `${name}: ${tile.name} aligns with the same column, including incomplete rows`,
      );
      assert.equal(tile.horizontallyContained, true, `${name}: ${tile.name} is not side-clipped`);
      if (fullyVisible) {
        assert.equal(tile.contained, true, `${name}: ${tile.name} stays inside the stage`);
        assert.equal(tile.unobscured, true, `${name}: ${tile.name} is fully visible`);
      }
      for (const other of result.boxes.slice(index + 1))
        assert.ok(
          Math.min(tile.x + tile.width, other.x + other.width) - Math.max(tile.x, other.x) <= 1 ||
            Math.min(tile.y + tile.height, other.y + other.height) - Math.max(tile.y, other.y) <= 1,
          `${name}: ${tile.name} and ${other.name} do not overlap`,
        );
    }
    report.checks.push(`Mixed video layout: ${name}`);
  };
  try {
    for (const mode of ['classic', 'modern']) {
      await page.locator('#settings-btn').click();
      const sheet = page.getByRole('region', { name: 'Settings', exact: true });
      await sheet.waitFor({ state: 'visible' });
      await sheet.getByRole('button', { name: 'Layout' }).click();
      await page.locator(`#layout-${mode}`).click();
      await page.keyboard.press('Escape');
      await page.keyboard.press('Escape');
      await sheet.waitFor({ state: 'hidden' });
      for (const viewport of [
        { width: 1440, height: 900 },
        { width: 1280, height: 720 },
      ]) {
        await page.setViewportSize(viewport);
        await inspect(`${mode}-${viewport.width}x${viewport.height}`, true);
      }
    }
    await page.evaluate(() => window.__mediaLayout.rotate('Camera 3', 360, 640));
    await page.waitForFunction(
      () => document.querySelector('[data-participant-id="Camera 3"] video').videoWidth === 360,
    );
    await inspect('equal-portrait-camera-and-share', true);
    const comparable = report.mediaLayouts
      .at(-1)
      .boxes.filter((tile) => tile.name === 'Camera 3' || tile.name === 'Portrait share');
    assert.ok(
      Math.abs(comparable[0].width - comparable[1].width) <= 1 &&
        Math.abs(comparable[0].height - comparable[1].height) <= 1,
      'Equal-aspect camera and share receive the same allocation',
    );
    await page.evaluate(() => window.__mediaLayout.pin('Portrait share'));
    await inspect('pinned-portrait', true);
    const pinned = report.mediaLayouts.at(-1).boxes.find((tile) => tile.name === 'Portrait share');
    const firstPosition = [...report.mediaLayouts.at(-1).boxes].sort(
      (left, right) => left.y - right.y || left.x - right.x,
    )[0];
    assert.equal(firstPosition.name, 'Portrait share', 'Pinning moves the tile to the first cell');
    assert.ok(
      Math.abs(pinned.width - comparable[0].width) <= 1 &&
        Math.abs(pinned.height - comparable[0].height) <= 1,
      'Pinning does not change tile dimensions',
    );
    await page.evaluate(() => window.__mediaLayout.pin(null));
    await page.evaluate(() => window.__mediaLayout.rotate('Portrait share', 640, 360));
    await page.waitForFunction(
      () => document.querySelector('.screen-share video').videoWidth === 640,
    );
    await inspect('share-rotated-landscape', true);
    await page.evaluate(() => window.__mediaLayout.remove('Portrait share'));
    await inspect('share-stopped', true);
    assert.equal(await page.locator('#video-grid .video-tile').count(), 3);
    await page.evaluate(() => {
      window.__mediaLayout.add('Portrait share', 360, 640, true);
      window.__mediaLayout.addAudio('Audio only');
    });
    await inspect('mixed-audio-five-tiles', true);
    assert.equal(
      report.mediaLayouts.at(-1).boxes.find((tile) => tile.name === 'Audio only').ratio,
      null,
      'The audio-only fixture has no video element',
    );
    for (const viewport of [
      { width: 390, height: 844 },
      { width: 844, height: 390 },
    ]) {
      await page.setViewportSize(viewport);
      await inspect(`mixed-five-${viewport.width}x${viewport.height}`, false);
      await page.evaluate(() => window.__mediaLayout.pin('Audio only'));
      await inspect(`pinned-audio-${viewport.width}x${viewport.height}`, false);
      await page.evaluate(() => window.__mediaLayout.pin(null));
    }
    await page.evaluate(() => {
      for (let index = 4; index <= 12; index++)
        window.__mediaLayout.add(`Camera ${index}`, 640, 360);
    });
    for (const viewport of [
      { width: 390, height: 844 },
      { width: 844, height: 390 },
    ]) {
      await page.setViewportSize(viewport);
      await inspect(`crowded-${viewport.width}x${viewport.height}`, false);
      for (const tile of await page.locator('#video-grid .video-tile').all()) {
        await tile.scrollIntoViewIfNeeded();
        assert.equal(
          await tile.evaluate((node) => {
            const box = node.getBoundingClientRect();
            const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
            return hit !== null && node.contains(hit);
          }),
          true,
          'Every crowded tile stays scroll-reachable',
        );
      }
      const tiles = page.locator('#video-grid .video-tile');
      for (const [menuIndex, tile] of [tiles.first(), tiles.last()].entries()) {
        await tile.scrollIntoViewIfNeeded();
        assert.equal(
          await tile.locator(':scope > .tile-pin').count(),
          0,
          'Pin is inside the viewing menu, leaving a single overlay trigger',
        );
        assert.equal(await tile.locator('summary').count(), 1);
        const trigger = tile.locator('summary');
        await trigger.click();
        const panel = tile.locator('.personal-media-panel');
        await panel.waitFor({ state: 'visible' });
        await panel.press('v');
        await panel.press('m');
        assert.equal(
          await page.evaluate(() => window.__captureRequests),
          0,
          'Typing inside viewing controls never activates capture shortcuts',
        );
        assert.equal(
          await panel.evaluate((node) => {
            const box = node.getBoundingClientRect();
            return (
              box.left >= 0 && box.top >= 0 && box.right <= innerWidth && box.bottom <= innerHeight
            );
          }),
          true,
          'Viewing controls fit the viewport even when the tile is small',
        );
        await page.screenshot({
          path: path.join(artifacts, `media-menu-${viewport.width}-${menuIndex}.png`),
        });
        for (const control of await panel.locator('button, input, select').all())
          if ((await control.isVisible()) && (await control.isEnabled()))
            await control.click({ trial: true });
        await panel.locator('.tile-pin').click();
        assert.equal(await tile.evaluate((node) => node.classList.contains('pinned')), true);
        if (!(await panel.isVisible())) await trigger.click();
        await panel.locator('.tile-pin').click();
        assert.equal(await tile.evaluate((node) => node.classList.contains('pinned')), false);
        await trigger.click();
        await panel.waitFor({ state: 'visible' });
        await page.keyboard.press('Escape');
        await panel.waitFor({ state: 'hidden' });
        assert.equal(
          await page.locator('#room-screen').isVisible(),
          true,
          'Escape dismisses viewing controls without leaving the room',
        );
      }
      report.checks.push(
        `Crowded tile controls remain reachable at ${viewport.width}x${viewport.height}`,
      );
    }
  } finally {
    await page.evaluate(() => window.__mediaLayout.close());
  }
}

module.exports = { mediaLayout };
