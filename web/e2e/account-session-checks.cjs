/** Real HTTP and live-socket session revocation, using only the caller's owned
 * disposable account and authenticator. Credentials never leave memory. */
const assert = require('node:assert/strict');

async function sessionManagementChecks({
  page,
  origin,
  playwright,
  credential,
  token,
  openAccount,
  stage,
}) {
  const contexts = [];
  const tokens = [];
  try {
    stage('create-other-sign-ins');
    for (let index = 0; index < 2; index++) {
      const context = await playwright.request.newContext();
      contexts.push(context);
      const startResponse = await context.post(
        new URL('/api/auth/passkey/login/start', origin).toString(),
        { data: {}, timeout: 5000 },
      );
      assert.equal(startResponse.status(), 200);
      const start = await startResponse.json();
      const finish = await context.post(
        new URL('/api/auth/passkey/login/finish', origin).toString(),
        {
          data: { ceremony_id: start.ceremony_id, credential: await credential(start) },
          timeout: 5000,
        },
      );
      assert.equal(finish.status(), 200);
      tokens.push((await finish.json()).token);
    }
    stage('open-owned-sockets');
    await page.evaluate(async (tokens) => {
      window.__sessionChecks = [];
      for (const token of tokens) {
        const response = await fetch('/api/auth/ws-ticket', {
          method: 'POST',
          headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
          body: '{}',
          signal: AbortSignal.timeout(5000),
        });
        if (response.status !== 200) throw new Error('Owned session ticket failed');
        const { ticket } = await response.json();
        const address = new URL('/ws', location.href);
        address.protocol = 'ws:';
        const socket = new WebSocket(address, ['simplestchat', `ticket.${ticket}`]);
        const entry = { socket, closed: false };
        window.__sessionChecks.push(entry);
        socket.addEventListener('close', () => {
          entry.closed = true;
        });
        await new Promise((resolve, reject) => {
          const timer = setTimeout(() => reject(new Error('Owned socket did not open')), 5000);
          socket.addEventListener(
            'open',
            () => {
              clearTimeout(timer);
              resolve();
            },
            { once: true },
          );
          socket.addEventListener(
            'error',
            () => {
              clearTimeout(timer);
              reject(new Error('Owned socket failed'));
            },
            { once: true },
          );
        });
      }
    }, tokens);
    stage('list-sessions');
    const account = await openAccount();
    await account.getByRole('button', { name: 'Sign out session', exact: true }).first().waitFor();
    assert.equal(
      await account.getByRole('button', { name: 'Sign out session', exact: true }).count(),
      2,
    );
    assert.equal(await account.getByText('This session', { exact: true }).count(), 1);
    stage('revoke-one-session');
    await account.getByRole('button', { name: 'Sign out session', exact: true }).first().click();
    await page.waitForFunction(
      () => window.__sessionChecks.filter((entry) => entry.closed).length === 1,
      undefined,
      { timeout: 12000 },
    );
    const profileStatuses = () =>
      page.evaluate(
        async (tokens) =>
          Promise.all(
            tokens.map(
              async (token) =>
                (
                  await fetch('/api/auth/profile', {
                    headers: { Authorization: `Bearer ${token}` },
                    signal: AbortSignal.timeout(5000),
                  })
                ).status,
            ),
          ),
        [...tokens, token],
      );
    stage('verify-single-revocation');
    const single = await profileStatuses();
    assert.deepEqual(single.slice(0, 2).sort(), [200, 401]);
    assert.equal(single[2], 200);
    stage('revoke-other-sessions');
    await account.getByRole('button', { name: 'Sign out other sessions', exact: true }).click();
    await page.waitForFunction(
      () => window.__sessionChecks.every((entry) => entry.closed),
      undefined,
      { timeout: 12000 },
    );
    stage('verify-other-revocation');
    assert.deepEqual(await profileStatuses(), [401, 401, 200]);
    stage('revoke-current-session');
    await account.getByRole('button', { name: 'Sign out this session', exact: true }).click();
    await account.waitFor({ state: 'hidden' });
    await page.locator('#sign-in-btn').waitFor({ state: 'visible' });
    stage('verify-current-revocation');
    assert.equal((await profileStatuses())[2], 401);
    assert.equal(
      await page.evaluate(
        async () =>
          (await fetch('/api/auth/refresh', { method: 'POST', signal: AbortSignal.timeout(5000) }))
            .status,
      ),
      401,
    );
  } finally {
    await page
      .evaluate(() => {
        for (const entry of window.__sessionChecks || []) entry.socket.close();
        delete window.__sessionChecks;
      })
      .catch(() => {});
    await Promise.all(contexts.map((context) => context.dispose()));
  }
}

module.exports = { sessionManagementChecks };
