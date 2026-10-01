/** Real local server + Chromium virtual WebAuthn authenticator. All credentials
 * remain in the disposable database/context and in memory; no secrets in reports.
 * This verifies native ceremonies, not OS or password-manager selection UX. */
const assert = require('node:assert/strict');
const { once } = require('node:events');
const { createServer } = require('node:http');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const origin = new URL(process.env.BASE_URL || 'http://localhost:3119');
if (
  process.env.PASSKEY_E2E !== '1' ||
  process.env.DISPOSABLE_TEST_DATABASE !== '1' ||
  origin.hostname !== 'localhost' ||
  origin.protocol !== 'http:' ||
  origin.username ||
  origin.password ||
  origin.pathname !== '/' ||
  origin.search ||
  origin.hash
)
  throw new Error('Use PASSKEY_E2E=1 with the owned localhost server/disposable database helpers.');

/** Runs in the browser. Signing stays in its native virtual authenticator. */
async function nativeAssertion(options) {
  const encode = (buffer) =>
    btoa(String.fromCharCode(...new Uint8Array(buffer)))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '');
  const decode = (value) =>
    Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0))
      .buffer;
  const assertion = await navigator.credentials.get({
    publicKey: { ...options.publicKey, challenge: decode(options.publicKey.challenge) },
    mediation: options.mediation,
    signal: AbortSignal.timeout(5000),
  });
  if (!assertion) throw new Error('Owned resident authenticator returned no credential');
  return {
    ceremony_id: options.ceremony_id,
    credential: {
      id: assertion.id,
      rawId: encode(assertion.rawId),
      type: assertion.type,
      response: {
        clientDataJSON: encode(assertion.response.clientDataJSON),
        authenticatorData: encode(assertion.response.authenticatorData),
        signature: encode(assertion.response.signature),
        userHandle: encode(assertion.response.userHandle),
      },
    },
  };
}

/** Prove account coexistence and routing, without claiming password-manager chooser coverage. */
async function multipleAccounts(page, cdp, authenticatorId) {
  const accounts = [];
  const nonce = Date.now().toString(36);
  await page.addInitScript(() => {
    const get = navigator.credentials.get.bind(navigator.credentials);
    navigator.credentials.get = (options) => {
      window.passkeyRequestShape = {
        mediation: options.mediation,
        rpId: options.publicKey.rpId,
        allowCredentials: options.publicKey.allowCredentials?.length,
        userVerification: options.publicKey.userVerification,
        binaryChallenge: options.publicKey.challenge instanceof ArrayBuffer,
      };
      return get(options);
    };
  });
  await page.goto(origin.toString());
  for (let index = 0; index < 3; index++) {
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-to-register').click();
    const displayName = `Passkey account ${index + 1}`;
    await page.locator('#register-email').fill(`passkey-multi-${nonce}-${index}@example.test`);
    await page.locator('#register-name').fill(displayName);
    const responses = Promise.all(
      ['start', 'finish'].map((step) =>
        page.waitForResponse((response) =>
          response.url().endsWith(`/api/auth/passkey/register/${step}`),
        ),
      ),
    );
    await page.locator('#register-passkey-btn').click();
    const [start, finish] = await responses;
    assert.equal(start.status(), 200, 'owned account registration starts');
    assert.equal(finish.status(), 200, 'owned account registration finishes');
    const options = await start.json();
    const { user } = await finish.json();
    const handle = Buffer.from(options.publicKey.user.id, 'base64url');
    assert.equal(handle.length, 16, 'registration uses a 16-byte account handle');
    assert.ok(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(user.id),
      'account identity is a UUIDv4',
    );
    assert.ok(
      handle.equals(Buffer.from(user.id.replaceAll('-', ''), 'hex')),
      'registration handle matches the committed account identity',
    );
    assert.ok(!accounts.some((account) => account.id === user.id), 'account identities differ');
    assert.equal(options.publicKey.authenticatorSelection.residentKey, 'required');
    assert.equal(options.publicKey.authenticatorSelection.requireResidentKey, true);
    const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
    assert.equal(credentials.length, index + 1, 'new accounts do not replace existing credentials');
    const owned = credentials.find((key) =>
      Buffer.from(key.userHandle || '', 'base64').equals(handle),
    );
    assert.ok(owned, 'native registration preserves the decoded user handle');
    assert.ok(owned.isResidentCredential, 'each account has a resident credential');
    assert.equal(owned.rpId, origin.hostname);
    assert.ok(
      !accounts.some((account) => account.credentialId === owned.credentialId),
      'account credentials differ',
    );
    accounts.push({ id: user.id, handle, credentialId: owned.credentialId, displayName });
    await page.locator('#register-modal').waitFor({ state: 'hidden' });
    await page.waitForFunction(
      (name) => document.getElementById('auth-display-name').textContent === name,
      displayName,
    );
    await page.locator('#logout-btn').click();
    await page.locator('#sign-in-btn').waitFor({ state: 'visible' });
  }

  const signIn = async (expected) => {
    await page.locator('#sign-in-btn').click();
    assert.equal(await page.locator('#login-email').inputValue(), '');
    const response = page.waitForResponse((item) =>
      item.url().endsWith('/api/auth/passkey/login/finish'),
    );
    await page.locator('#login-passkey-btn').click();
    const completed = await response;
    assert.equal(completed.status(), 200, 'native discovery signs into an owned account');
    const request = completed.request().postDataJSON();
    const { user } = await completed.json();
    const account = accounts.find((item) => item.id === user.id);
    assert.ok(account, 'the returned identity belongs to a registered account');
    assert.ok(
      account.handle.equals(Buffer.from(request.credential.response.userHandle, 'base64url')) &&
        Buffer.from(account.credentialId, 'base64').equals(
          Buffer.from(request.credential.id, 'base64url'),
        ),
      'native selected credential, account handle and returned identity agree',
    );
    if (expected) assert.ok(account.id === expected.id, 'each selected account signs in as itself');
    assert.deepEqual(await page.evaluate(() => window.passkeyRequestShape), {
      mediation: 'required',
      rpId: origin.hostname,
      allowCredentials: 0,
      userVerification: 'required',
      binaryChallenge: true,
    });
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    await page.waitForFunction(
      (name) => document.getElementById('auth-display-name').textContent === name,
      account.displayName,
    );
    await page.locator('#logout-btn').click();
    await page.locator('#sign-in-btn').waitFor({ state: 'visible' });
  };

  // Leave all three credentials available for an unmodified native discovery.
  // Chromium chooses a fixture credential; this does not exercise a vault chooser.
  await signIn();
  for (const account of accounts) {
    const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
    assert.equal(credentials.length, 3, 'all account credentials coexist before selection');
    const others = credentials.filter((key) => key.credentialId !== account.credentialId);
    assert.equal(others.length, 2);
    // Control only availability in the owned authenticator, not page options or
    // assertions. Keep fresh snapshots and leave the selected key's advanced
    // counter in place when restoring the other two credentials.
    for (const key of others)
      await cdp.send('WebAuthn.removeCredential', {
        authenticatorId,
        credentialId: key.credentialId,
      });
    await signIn(account);
    for (const credential of others)
      await cdp.send('WebAuthn.addCredential', { authenticatorId, credential });
    const restored = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
    assert.equal(restored.credentials.length, 3, 'all account credentials are restored');
    assert.ok(
      accounts.every((item) =>
        restored.credentials.some(
          (key) =>
            key.credentialId === item.credentialId &&
            key.isResidentCredential &&
            Buffer.from(key.userHandle || '', 'base64').equals(item.handle),
        ),
      ),
      'restoration preserves each account and resident handle',
    );
  }
  await signIn();
}

async function run() {
  const browser = await playwright.chromium.launch({ headless: true });
  const context = await browser.newContext();
  const checks = [];
  let cdp;
  let authenticatorId;
  let alternateServer;
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    const failures = [];
    const starts = [];
    let finishes = 0;
    page.on('pageerror', () => failures.push('pageerror'));
    page.on('request', (request) => {
      const pathname = new URL(request.url()).pathname;
      if (pathname === '/api/auth/passkey/login/start') starts.push(request.postDataJSON());
      if (pathname === '/api/auth/passkey/login/finish') finishes++;
    });
    cdp = await context.newCDPSession(page);
    await cdp.send('WebAuthn.enable');
    ({ authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'usb',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    }));
    await page.goto(origin.toString());
    await page.locator('#sign-in-btn').click();
    await page.locator('#login-to-register').click();
    const email = `passkey-${Date.now().toString(36)}@example.test`;
    await page.locator('#register-email').fill(email);
    await page.locator('#register-name').fill('Passkey fixture');
    const registrationOptions = page.waitForResponse((response) =>
      response.url().endsWith('/api/auth/passkey/register/start'),
    );
    await page.locator('#register-passkey-btn').click();
    const registration = await (await registrationOptions).json();
    assert.equal(registration.publicKey.authenticatorSelection.residentKey, 'required');
    assert.equal(registration.publicKey.authenticatorSelection.requireResidentKey, true);
    assert.equal(registration.publicKey.authenticatorSelection.userVerification, 'required');
    assert.equal(registration.publicKey.authenticatorSelection.authenticatorAttachment, undefined);
    await page.locator('#register-modal').waitFor({ state: 'hidden' });
    await page.locator('#logout-btn').waitFor({ state: 'visible' });
    const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
    assert.equal(credentials.length, 1);
    const credential = credentials[0];
    assert.equal(credential.isResidentCredential, true);
    checks.push('resident-required-native-registration');

    await page.locator('#logout-btn').click();
    await page.locator('#sign-in-btn').click();
    assert.equal(await page.locator('#login-email').inputValue(), '');
    const loginOptions = page.waitForResponse((response) =>
      response.url().endsWith('/api/auth/passkey/login/start'),
    );
    await page.locator('#login-passkey-btn').click();
    const login = await (await loginOptions).json();
    assert.equal(login.mediation, 'required');
    assert.deepEqual(login.publicKey.allowCredentials, []);
    assert.equal(login.publicKey.userVerification, 'required');
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    await page.locator('#logout-btn').waitFor({ state: 'visible' });
    assert.deepEqual(starts, [{}]);
    checks.push('usernameless-native-login-real-verification');
    await page.locator('#logout-btn').click();
    await page.locator('#sign-in-btn').waitFor({ state: 'visible' });

    const legacyStatuses = await page.evaluate(async (knownEmail) => {
      const statuses = [];
      for (const email of [knownEmail, 'unregistered-passkey@example.test']) {
        const response = await fetch('/api/auth/passkey/login/start', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ email }),
          signal: AbortSignal.timeout(5000),
        });
        statuses.push(response.status);
      }
      return statuses;
    }, email);
    assert.deepEqual(legacyStatuses, [422, 422]);
    checks.push('email-payload-rejected-independently-of-account');

    for (const scenario of ['missing-handle', 'wrong-handle', 'altered-signature', 'replay']) {
      const result = await page.evaluate(async (scenario) => {
        const post = (path, body) =>
          fetch(path, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            ...(body === undefined ? {} : { body: JSON.stringify(body) }),
            signal: AbortSignal.timeout(5000),
          });
        const encode = (buffer) =>
          btoa(String.fromCharCode(...new Uint8Array(buffer)))
            .replace(/\+/g, '-')
            .replace(/\//g, '_')
            .replace(/=+$/, '');
        const decode = (value) =>
          Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0))
            .buffer;
        const response = await post('/api/auth/passkey/login/start', {});
        if (!response.ok) throw new Error('Could not start owned passkey assertion');
        const options = await response.json();
        options.publicKey.challenge = decode(options.publicKey.challenge);
        const assertion = await navigator.credentials.get({
          publicKey: options.publicKey,
          mediation: options.mediation,
          signal: AbortSignal.timeout(5000),
        });
        if (!assertion) throw new Error('Owned resident authenticator returned no credential');
        const body = {
          ceremony_id: options.ceremony_id,
          credential: {
            id: assertion.id,
            rawId: encode(assertion.rawId),
            type: assertion.type,
            response: {
              clientDataJSON: encode(assertion.response.clientDataJSON),
              authenticatorData: encode(assertion.response.authenticatorData),
              signature: encode(assertion.response.signature),
              ...(scenario === 'missing-handle'
                ? {}
                : {
                    userHandle:
                      scenario === 'wrong-handle'
                        ? encode(new Uint8Array(16))
                        : encode(assertion.response.userHandle),
                  }),
            },
          },
        };
        if (scenario === 'altered-signature') {
          const signature = new Uint8Array(assertion.response.signature.slice(0));
          signature[signature.length - 1] ^= 1;
          body.credential.response.signature = encode(signature);
        }
        const finish = () => post('/api/auth/passkey/login/finish', body);
        const first = await finish();
        const second = await finish();
        const refresh = await post('/api/auth/refresh');
        await post('/api/auth/logout');
        return { status: first.status, replayStatus: second.status, refreshStatus: refresh.status };
      }, scenario);
      assert.equal(result.status, scenario === 'replay' ? 200 : 401, scenario);
      assert.equal(result.replayStatus, 401, scenario);
      assert.equal(result.refreshStatus, scenario === 'replay' ? 200 : 401, scenario);
      checks.push(`assertion-${scenario}`);
    }

    const post = (path, body) =>
      context.request.post(new URL(path, origin).toString(), {
        ...(body === undefined ? {} : { data: body }),
        headers: { Origin: origin.origin },
        timeout: 5000,
      });
    const start = async () => {
      const response = await post('/api/auth/passkey/login/start', {});
      assert.equal(response.status(), 200, 'owned challenge creation');
      return response.json();
    };
    const finish = (body) => post('/api/auth/passkey/login/finish', body);
    const signedCounter = (body) =>
      Buffer.from(body.credential.response.authenticatorData, 'base64url').readUInt32BE(33);
    const older = await page.evaluate(nativeAssertion, await start());
    const newer = await page.evaluate(nativeAssertion, await start());
    const latestCounter = signedCounter(newer);
    assert.ok(
      latestCounter > signedCounter(older),
      'native assertions have distinct advancing counters',
    );
    assert.equal((await finish(newer)).status(), 200, 'newer signed counter accepted');
    assert.equal((await post('/api/auth/refresh')).status(), 200);
    assert.equal((await post('/api/auth/logout')).status(), 204);
    assert.equal(
      (await finish(older)).status(),
      401,
      'older signed counter rejected on its unused challenge',
    );
    assert.equal(
      (await post('/api/auth/refresh')).status(),
      401,
      'stale assertion creates no session',
    );
    checks.push('out-of-order-signed-counter-rejected');

    await cdp.send('WebAuthn.removeCredential', {
      authenticatorId,
      credentialId: credential.credentialId,
    });
    await cdp.send('WebAuthn.addCredential', {
      authenticatorId,
      credential: { ...credential, signCount: latestCounter - 1 },
    });
    const equal = await page.evaluate(nativeAssertion, await start());
    assert.equal(
      signedCounter(equal),
      latestCounter,
      'native authenticator signs the repeated counter',
    );
    assert.equal(
      (await finish(equal)).status(),
      401,
      'equal signed counter rejected on a fresh challenge',
    );
    assert.equal((await post('/api/auth/refresh')).status(), 401);
    checks.push('equal-signed-counter-rejected');

    // A second owned localhost origin shares the RP domain, but its port is not
    // an allowed server origin. Native WebAuthn signs that actual origin; this
    // negative therefore does not depend on an independently broken signature.
    alternateServer = createServer((_, response) => {
      response.setHeader('Content-Type', 'text/html');
      response.end('<!doctype html><title>Owned passkey origin fixture</title>');
    });
    alternateServer.listen(0, '127.0.0.1');
    await once(alternateServer, 'listening');
    const alternateOrigin = `http://localhost:${alternateServer.address().port}`;
    assert.notEqual(alternateOrigin, origin.origin);
    const wrongOriginOptions = await start();
    await page.goto(alternateOrigin);
    const wrongOriginAssertion = await page.evaluate(nativeAssertion, wrongOriginOptions);
    const signedClient = JSON.parse(
      Buffer.from(wrongOriginAssertion.credential.response.clientDataJSON, 'base64url').toString(
        'utf8',
      ),
    );
    assert.equal(signedClient.origin, alternateOrigin);
    assert.ok(signedCounter(wrongOriginAssertion) > latestCounter);
    assert.equal(
      (await finish(wrongOriginAssertion)).status(),
      401,
      'valid native signature from unapproved origin rejected',
    );
    assert.equal((await post('/api/auth/refresh')).status(), 401);
    await page.goto(origin.toString());
    const validAfterFailures = await page.evaluate(nativeAssertion, await start());
    assert.equal(
      (await finish(validAfterFailures)).status(),
      200,
      'same credential remains usable from approved origin',
    );
    assert.equal((await post('/api/auth/logout')).status(), 204);
    checks.push('native-origin-mismatch-rejected-approved-origin-still-works');

    // Reuse the registered key only inside this disposable authenticator, but
    // make it nonresident to model historical credentials without DB rewrites.
    await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId });
    ({ authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'usb',
        hasResidentKey: false,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    }));
    await cdp.send('WebAuthn.addCredential', {
      authenticatorId,
      credential: {
        credentialId: credential.credentialId,
        isResidentCredential: false,
        rpId: credential.rpId,
        privateKey: credential.privateKey,
        signCount: credential.signCount + 100,
      },
    });
    const availableById = await page.evaluate(async (credentialId) => {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), 3000);
      try {
        const assertion = await navigator.credentials.get({
          publicKey: {
            rpId: 'localhost',
            challenge: crypto.getRandomValues(new Uint8Array(32)),
            allowCredentials: [
              {
                type: 'public-key',
                id: Uint8Array.from(atob(credentialId), (c) => c.charCodeAt(0)),
              },
            ],
            userVerification: 'required',
          },
          signal: controller.signal,
        });
        return assertion !== null;
      } finally {
        clearTimeout(timer);
      }
    }, credential.credentialId);
    assert.equal(
      availableById,
      true,
      'nonresident key remains functional when explicitly selected',
    );
    const finishesBefore = finishes;
    const startsBefore = starts.length;
    await page.locator('#sign-in-btn').click();
    const unavailableOptions = page.waitForResponse((response) =>
      response.url().endsWith('/api/auth/passkey/login/start'),
    );
    await page.locator('#login-passkey-btn').click();
    await unavailableOptions;
    await page.waitForTimeout(1000);
    assert.equal(finishes, finishesBefore, 'nonresident key cannot finish discovery');
    assert.equal(starts.length, startsBefore + 1, 'no automatic named-login fallback');
    assert.deepEqual(starts.at(-1), {});
    assert.equal(await page.locator('#login-modal').isVisible(), true);
    await page.keyboard.press('Escape');
    await page.locator('#login-modal').waitFor({ state: 'hidden' });
    checks.push('functional-nonresident-key-unavailable-to-discovery-no-fallback');

    await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId });
    ({ authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'usb',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    }));
    await multipleAccounts(page, cdp, authenticatorId);
    checks.push('three-ui-accounts-coexist-in-one-native-authenticator');
    checks.push('each-native-account-signs-in-through-unmodified-discovery-ui');
    assert.deepEqual(failures, []);
    process.stdout.write(JSON.stringify({ browser: 'chromium', checks, passed: true }) + '\n');
  } finally {
    if (cdp && authenticatorId)
      await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId }).catch(() => {});
    await context.close();
    await browser.close();
    if (alternateServer) {
      alternateServer.closeAllConnections();
      await new Promise((resolve, reject) =>
        alternateServer.close((error) => (error ? reject(error) : resolve())),
      );
    }
  }
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
