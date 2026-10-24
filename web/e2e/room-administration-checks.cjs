const assert = require('node:assert/strict');
const { sheet, sheetScreen, closeSheet } = require('./settings-sheet.cjs');

async function observeAdministration(page) {
  await page.evaluate(() => {
    const original = WebSocket.prototype.send;
    const observation = {
      joins: 0,
      leaves: 0,
      captures: window.__communityCaptureRequests,
      original,
      send(data) {
        if (typeof data === 'string') {
          try {
            const type = JSON.parse(data).type;
            if (type === 'joinRoom') observation.joins++;
            if (type === 'leaveRoom') observation.leaves++;
          } catch {}
        }
        return original.call(this, data);
      },
    };
    WebSocket.prototype.send = observation.send;
    window.__roomAdministrationObservation = observation;
  });
}

async function finishObservation(page) {
  const result = await page.evaluate(() => {
    const observed = window.__roomAdministrationObservation;
    if (WebSocket.prototype.send !== observed.send)
      throw new Error('Administration observation was replaced');
    const result = {
      joins: observed.joins,
      leaves: observed.leaves,
      capturesBefore: observed.captures,
      capturesAfter: window.__communityCaptureRequests,
    };
    WebSocket.prototype.send = observed.original;
    delete window.__roomAdministrationObservation;
    return result;
  });
  assert.equal(result.joins, 0, 'administration never joins another call');
  assert.equal(result.leaves, 0, 'administration never leaves the current call');
  assert.equal(
    result.capturesAfter,
    result.capturesBefore,
    'administration never captures a device',
  );
}

/** Uses only this run's generated room and accounts on its owned test server. */
async function checkRoomAdministration(
  owner,
  home,
  { base, runId, client, header, close, connected, paceJoins, recordJoin },
) {
  const roomId = `${runId}-administration`;
  const roomName = 'E2E Managed Away From Call';
  const observers = [owner, home];
  for (const page of observers) await observeAdministration(page);
  let bearer;
  let created = false;
  let failure;
  const cleanupErrors = [];
  const members = [];
  const request = async (method, url, data) => {
    try {
      return await home.request[method](url, {
        headers: { authorization: bearer },
        ...(data && { data }),
      });
    } catch {
      throw new Error('Owned administration setup or cleanup request failed');
    }
  };
  const manage = async (page) => {
    const mine = await header(page, 'My rooms');
    await mine
      .locator('.owned-room')
      .filter({ hasText: roomId })
      .getByRole('button', { name: 'Manage room', exact: true })
      .click();
    await mine.waitFor({ state: 'hidden' });
    const region = sheet(page);
    await region.waitFor({ state: 'visible' });
    return region;
  };
  const save = async (page, region) => {
    const response = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === `/api/rooms/${roomId}/administration/settings` &&
        response.request().method() === 'PATCH',
    );
    await region.getByRole('button', { name: 'Save room settings', exact: true }).click();
    assert.equal((await response).status(), 200, 'selected room settings commit');
  };
  const join = async (name, waiting = false) => {
    const page = await client(`administration-${members.length}`);
    members.push(page);
    await paceJoins(page);
    await page.locator('#name-input').fill(name);
    await page.locator('#room-input').fill(roomId);
    await page.locator('#join-btn').click();
    recordJoin();
    if (waiting) await page.locator('#lobby-screen').waitFor({ state: 'visible' });
    else await connected(page);
    return page;
  };
  const stillConnected = async () => {
    await owner.locator('#room-screen').waitFor({ state: 'visible' });
    await owner.waitForFunction(
      () => document.querySelector('#connection-status').textContent === 'Connected',
    );
  };
  try {
    // Obtain the account header from its normal owned-room read. It remains in
    // this function only and never enters an artifact, assertion or diagnostic.
    const accountRead = home.waitForRequest(
      (request) => new URL(request.url()).pathname === '/api/rooms/mine',
    );
    const mine = await header(home, 'My rooms');
    bearer = (await accountRead).headers().authorization;
    assert.ok(
      typeof bearer === 'string' && bearer.startsWith('Bearer '),
      'Expected current account authorization',
    );
    await close(mine);
    const creation = await request('post', `${base}/api/rooms`, {
      id: roomId,
      display_name: roomName,
    });
    assert.equal(creation.status(), 200);
    created = true;
    let region = await manage(home);
    region = await sheetScreen(home, 'Who can enter');
    await region.getByText(`${roomName} · ${roomId}`, { exact: true }).waitFor();
    await region.getByLabel('Allow guests', { exact: true }).uncheck();
    await save(home, region);
    await closeSheet(home);
    region = await manage(home);
    region = await sheetScreen(home, 'Who can enter');
    assert.equal(
      await region.getByLabel('Allow guests', { exact: true }).isChecked(),
      false,
      'dormant room settings survive reopening',
    );
    await region.getByLabel('Allow guests', { exact: true }).check();
    await save(home, region);
    assert.equal(await home.locator('#join-screen').isVisible(), true);
    assert.equal(await home.evaluate(() => window.__communityCaptureRequests), 0);

    const receiving = await join('E2E Separate Room Member');
    region = await manage(owner);
    region = await sheetScreen(owner, 'Chat & broadcasting');
    await region.getByLabel('Allow chat', { exact: true }).uncheck();
    await save(owner, region);
    await receiving.waitForFunction(() => document.querySelector('#chat-input').disabled);
    assert.equal(
      await owner.locator('#chat-input').isDisabled(),
      false,
      'the current call retains its own chat policy',
    );
    await region.getByLabel('Allow chat', { exact: true }).check();
    await save(owner, region);
    await receiving.waitForFunction(() => !document.querySelector('#chat-input').disabled);
    region = await sheetScreen(owner, 'Who can enter');
    await region.getByText(`${roomName} · ${roomId}`, { exact: true }).waitFor();
    assert.equal(
      new URLSearchParams(new URL(owner.url()).hash.slice(1)).has('section'),
      false,
      'a managed-room section cannot be serialized as a section of the current call',
    );
    await owner.goBack();
    await sheet(owner).getByRole('heading', { name: 'Settings', exact: true }).waitFor();
    await stillConnected();
    await owner.goForward();
    await sheet(owner).getByRole('heading', { name: 'Who can enter', exact: true }).waitFor();
    await sheet(owner).getByText(`${roomName} · ${roomId}`, { exact: true }).waitFor();
    await closeSheet(owner);
    // Transient My rooms entries have no replayable action; advance through them
    // until the settings destination is reached, without creating another room.
    for (let depth = 0; depth < 4 && !(await sheet(owner).isVisible()); depth++)
      await owner.goForward();
    await sheet(owner).getByRole('heading', { name: 'Settings', exact: true }).waitFor();
    assert.equal(
      await sheet(owner).getByLabel('Waiting room', { exact: true }).count(),
      0,
      'Forward after retiring B must not open the call A controls with the same section ID',
    );
    assert.equal(new URLSearchParams(new URL(owner.url()).hash.slice(1)).has('section'), false);
    await stillConnected();
    await closeSheet(owner);
    region = await manage(owner);
    region = await sheetScreen(owner, 'Who can enter');
    await region.getByLabel('Waiting room', { exact: true }).check();
    await save(owner, region);
    await closeSheet(owner);
    const waiting = await join('E2E Waiting Home Admission', true);
    // Keep the home operator's selected room open while another device changes
    // it. Navigation refreshes authority without redundant My rooms requests.
    region = await sheetScreen(home, 'Waiting room');
    const row = region
      .locator('.management-entry')
      .filter({ hasText: 'E2E Waiting Home Admission' });
    await row.getByRole('button', { name: 'Admit', exact: true }).click();
    await connected(waiting);
    assert.equal(await home.locator('#room-screen').isVisible(), false);
    // The peer joins before the operator finishes refreshing the selected room.
    await region.locator('.sheet-screen[aria-busy="false"]').waitFor({ state: 'visible' });
    assert.equal(
      await region.getByRole('alert').isVisible(),
      false,
      'the selected room refresh completed without an administration error',
    );
    await closeSheet(home);
    await connected(owner);
    for (const page of members)
      assert.equal(await page.evaluate(() => window.__communityCaptureRequests), 0);
  } catch (error) {
    failure = error;
  } finally {
    for (const page of members) {
      try {
        if (await page.locator('#lobby-screen').isVisible())
          await page.locator('#lobby-cancel-btn').click();
        if (await page.locator('#room-screen').isVisible()) {
          await page.locator('#leave-btn').click();
          await page.locator('#join-screen').waitFor({ state: 'visible' });
        }
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (created) {
      try {
        const deletion = await request('delete', `${base}/api/rooms/${roomId}`);
        assert.equal(deletion.status(), 204, 'remove only the generated administration room');
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    for (const page of observers) {
      try {
        await finishObservation(page);
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
  }
  if (failure) throw failure;
  if (cleanupErrors.length)
    throw new AggregateError(
      cleanupErrors,
      'Owned room administration cleanup or capture/join checks failed',
    );
  return { savedRoomBackForward: true, retiredRoomContextSafeRoot: true };
}

module.exports = { checkRoomAdministration };
