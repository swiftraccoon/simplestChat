/** Exercise account management with native resident credentials on an owned local
 * server. Secrets stay in memory; reports contain fixed outcome labels only. */
const assert = require('node:assert/strict');
const playwright = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
let failureStage = 'owned-browser-setup';

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
  throw new Error('Use the owned PASSKEY_E2E localhost server and disposable database helpers.');

function traceNativeCeremonies() {
  let clicks = 0;
  const calls = [];
  window.__managementCeremonies = calls;
  document.addEventListener(
    'click',
    (event) => {
      if (event.isTrusted) clicks++;
    },
    true,
  );
  for (const kind of ['create', 'get']) {
    const native = navigator.credentials[kind].bind(navigator.credentials);
    navigator.credentials[kind] = (...args) => {
      if (calls.length < 32)
        calls.push({ kind, clicks, active: navigator.userActivation.isActive });
      return native(...args);
    };
  }
}

async function nativeCredential({ options, register }) {
  const decode = (value) =>
    Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), (character) =>
      character.charCodeAt(0),
    ).buffer;
  const encode = (value) =>
    btoa(String.fromCharCode(...new Uint8Array(value)))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '');
  const publicKey = { ...options.publicKey, challenge: decode(options.publicKey.challenge) };
  for (const key of ['excludeCredentials', 'allowCredentials']) {
    if (publicKey[key])
      publicKey[key] = publicKey[key].map((item) => ({ ...item, id: decode(item.id) }));
  }
  if (register) publicKey.user = { ...publicKey.user, id: decode(publicKey.user.id) };
  const credential = await navigator.credentials[register ? 'create' : 'get']({
    publicKey,
    ...(register ? {} : { mediation: options.mediation }),
    signal: AbortSignal.timeout(5000),
  });
  if (!credential) throw new Error('Owned authenticator returned no credential');
  const response = { clientDataJSON: encode(credential.response.clientDataJSON) };
  if (register) response.attestationObject = encode(credential.response.attestationObject);
  else {
    response.authenticatorData = encode(credential.response.authenticatorData);
    response.signature = encode(credential.response.signature);
    response.userHandle = encode(credential.response.userHandle);
  }
  return { id: credential.id, rawId: encode(credential.rawId), type: credential.type, response };
}

async function run() {
  const browser = await playwright.chromium.launch({ headless: true });
  const context = await browser.newContext();
  await context.addInitScript(traceNativeCeremonies);
  let cdp;
  let authenticatorId;
  const checks = [];
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    await page.goto(origin.toString());
    cdp = await context.newCDPSession(page);
    await cdp.send('WebAuthn.enable');
    const authenticator = async () => {
      if (authenticatorId)
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
    };
    await authenticator();
    const request = async (path, token, body, expected = 200) => {
      failureStage = `native-api:${path}`;
      const response = await context.request.fetch(new URL(path, origin).toString(), {
        method: body === undefined ? 'GET' : 'POST',
        headers: { Origin: origin.origin, ...(token ? { Authorization: `Bearer ${token}` } : {}) },
        ...(body === undefined ? {} : { data: body }),
        timeout: 5000,
      });
      assert.equal(response.status(), expected, `Owned ${path} response status`);
      if (expected !== 200) return undefined;
      assert.match(response.headers()['cache-control'] || '', /no-store/);
      return response.json();
    };
    const postAnonymous = async (path, data) => {
      failureStage = `native-api:${path}`;
      const response = await context.request.post(new URL(path, origin).toString(), {
        data,
        timeout: 5000,
      });
      assert.equal(response.status(), 200, `Owned ${path} response status`);
      return response.json();
    };
    const credential = (options, register = false) =>
      page.evaluate(nativeCredential, { options, register });
    const email = `passkey-management-${Date.now().toString(36)}@example.test`;
    const registration = await postAnonymous('/api/auth/passkey/register/start', {
      email,
      display_name: 'Management fixture',
    });
    const registered = await postAnonymous('/api/auth/passkey/register/finish', {
      ceremony_id: registration.ceremony_id,
      credential: await credential(registration, true),
    });
    let token = registered.token;
    const list = () => request('/api/auth/passkeys', token);
    const initial = await list();
    assert.equal(initial.password_enabled, false);
    assert.equal(initial.recovery_enabled, false);
    assert.equal(initial.passkeys.length, 1);
    assert.deepEqual(Object.keys(initial.passkeys[0]).sort(), ['created_at', 'id']);
    const firstId = initial.passkeys[0].id;

    failureStage = 'ui-passkey-only-account';
    await page.reload();
    const openAccount = async () => {
      await page.getByRole('button', { name: 'Account', exact: true }).click();
      const account = page.getByRole('dialog', { name: 'Account', exact: true });
      await account.getByRole('button', { name: 'Add passkey', exact: true }).waitFor();
      return account;
    };
    const closeAccount = async (account) => {
      await account.getByRole('button', { name: 'Close', exact: true }).click();
      await account.waitFor({ state: 'hidden' });
    };
    const ceremonies = () => page.evaluate(() => [...window.__managementCeremonies]);
    let account = await openAccount();
    assert.equal(
      await account.getByRole('button', { name: 'Change password', exact: true }).count(),
      0,
    );
    assert.equal(await account.getByLabel('Current password', { exact: true }).count(), 0);
    assert.equal(await account.getByLabel('New password', { exact: true }).count(), 0);
    assert.equal(
      await account.getByLabel('Current password for verification', { exact: true }).isVisible(),
      false,
    );
    assert.deepEqual(await account.getByRole('combobox').locator('option').allTextContents(), [
      'Existing passkey',
    ]);
    assert.equal(
      await account.getByRole('button', { name: /^Remove passkey / }).isDisabled(),
      true,
    );
    checks.push('ui-passkey-only-account-hides-password-controls-and-blocks-last-removal');

    failureStage = 'ui-fresh-passkey-recovery';
    await account.getByRole('button', { name: 'Generate recovery key', exact: true }).click();
    await account.getByRole('button', { name: 'Verify with passkey', exact: true }).waitFor();
    assert.deepEqual(
      await ceremonies(),
      [],
      'Fetching a challenge must not launch a native ceremony',
    );
    await account.getByRole('button', { name: 'Verify with passkey', exact: true }).click();
    const key = account.getByLabel('Recovery key', { exact: true });
    await key.waitFor();
    assert.ok(
      /^sc-recovery-[A-Za-z0-9_-]{43}$/.test(await key.inputValue()),
      'UI exposes the new recovery key once',
    );
    const keyElement = await key.elementHandle();
    const proofCalls = await ceremonies();
    assert.equal(proofCalls.length, 1);
    assert.equal(proofCalls[0].kind, 'get');
    assert.equal(proofCalls[0].active, true, 'Fresh passkey proof runs from a user gesture');
    assert.ok(proofCalls[0].clicks >= 3, 'Proof follows its separate explicit verification click');
    await account.getByRole('button', { name: 'I saved my recovery key', exact: true }).click();
    await key.waitFor({ state: 'hidden' });
    assert.equal(
      await keyElement.evaluate((element) => element.value),
      '',
      'Dismissal clears even the detached secret field',
    );
    await keyElement.dispose();
    await closeAccount(account);
    account = await openAccount();
    assert.equal(await account.getByLabel('Recovery key', { exact: true }).count(), 0);
    await account.getByRole('button', { name: 'Replace recovery key', exact: true }).waitFor();
    await closeAccount(account);
    checks.push('ui-fresh-passkey-recovery-has-separate-gesture-and-clears-save-once-display');

    const prove = async (operation, expected = 200) => {
      const started = await request('/api/auth/passkeys/start', token, { operation });
      assert.equal(started.kind, 'authenticate');
      return request(
        '/api/auth/passkeys/authorize',
        token,
        {
          ceremony_id: started.ceremony_id,
          credential: await credential(started.options),
        },
        expected,
      );
    };
    const recovered = await prove({ action: 'recovery_key' });
    assert.equal(recovered.kind, 'recovery_key');
    assert.ok(
      /^sc-recovery-[A-Za-z0-9_-]{43}$/.test(recovered.recovery_key),
      'Recovery key has the expected opaque format',
    );
    assert.equal((await list()).recovery_enabled, true);
    checks.push('passkey-only-fresh-proof-creates-usable-recovery-key');
    await prove({ action: 'remove', id: firstId }, 400);
    assert.equal((await list()).passkeys.length, 1);
    checks.push('recovery-key-does-not-allow-removing-last-direct-sign-in');
    const enrollment = await prove({ action: 'add' });
    assert.equal(enrollment.kind, 'register');
    assert.equal(enrollment.options.publicKey.excludeCredentials.length, 1);
    assert.equal(enrollment.options.publicKey.authenticatorSelection.residentKey, 'required');
    await authenticator();
    const added = await request('/api/auth/passkeys/enroll', token, {
      ceremony_id: enrollment.ceremony_id,
      credential: await credential(enrollment.options, true),
    });
    assert.equal(added.kind, 'added');
    assert.equal((await list()).passkeys.length, 2);
    checks.push('existing-passkey-account-enrolls-second-owned-authenticator');
    const removed = await prove({ action: 'remove', id: firstId });
    assert.equal(removed.kind, 'removed');
    await request('/api/auth/passkeys', token, undefined, 401);
    const login = await postAnonymous('/api/auth/passkey/login/start', {});
    const loggedIn = await postAnonymous('/api/auth/passkey/login/finish', {
      ceremony_id: login.ceremony_id,
      credential: await credential(login),
    });
    token = loggedIn.token;
    assert.equal((await list()).passkeys.length, 1);
    checks.push('removal-revokes-old-session-and-remaining-passkey-signs-in');
    const password = `Saved-recovery-${Date.now().toString(36)}!`;
    await request(
      '/api/auth/recovery/redeem',
      null,
      { email, recovery_key: recovered.recovery_key, new_password: password },
      204,
    );
    const passwordLogin = await postAnonymous('/api/auth/login', { email, password });
    token = passwordLogin.token;
    const afterRecovery = await list();
    assert.equal(afterRecovery.password_enabled, true);
    assert.equal(afterRecovery.recovery_enabled, false);
    assert.equal(afterRecovery.passkeys.length, 1);
    checks.push('issued-recovery-key-restores-account-with-password');
    const lastRemoved = await request('/api/auth/passkeys/start', token, {
      operation: { action: 'remove', id: afterRecovery.passkeys[0].id },
      current_password: password,
    });
    assert.equal(lastRemoved.kind, 'removed');
    const again = await postAnonymous('/api/auth/login', { email, password });
    token = again.token;
    assert.equal((await list()).passkeys.length, 0);
    checks.push('password-proof-permits-last-passkey-removal-with-working-fallback');
    const firstAgain = await request('/api/auth/passkeys/start', token, {
      operation: { action: 'add' },
      current_password: password,
    });
    assert.equal(firstAgain.kind, 'register');
    await authenticator();
    const firstAdded = await request('/api/auth/passkeys/enroll', token, {
      ceremony_id: firstAgain.ceremony_id,
      credential: await credential(firstAgain.options, true),
    });
    assert.equal(firstAdded.kind, 'added');
    assert.equal((await list()).passkeys.length, 1);
    checks.push('password-account-enrolls-first-passkey');

    const uiPassword = `Owned-UI-password-${Date.now().toString(36)}!`;
    const uiRegistered = await postAnonymous('/api/auth/register', {
      email: `passkey-ui-${Date.now().toString(36)}@example.test`,
      password: uiPassword,
      display_name: 'Password management fixture',
    });
    failureStage = 'ui-password-account-first-passkey';
    await page.reload();
    account = await openAccount();
    await account
      .getByText('0 of 10 passkeys saved. Password sign-in is also available.', { exact: true })
      .waitFor();
    const proofPassword = account.getByLabel('Current password for verification', { exact: true });
    await proofPassword.fill(uiPassword);
    const passwordElement = await proofPassword.elementHandle();
    await account.getByRole('button', { name: 'Add passkey', exact: true }).click();
    await account.getByRole('button', { name: 'Create passkey', exact: true }).waitFor();
    assert.deepEqual(
      await ceremonies(),
      [],
      'Password verification must not automatically launch credential creation',
    );
    assert.equal(
      await passwordElement.evaluate((element) => element.value),
      '',
      'Submitted proof password is cleared',
    );
    await passwordElement.dispose();
    await account.getByRole('button', { name: 'Create passkey', exact: true }).click();
    await account
      .getByText('1 of 10 passkeys saved. Password sign-in is also available.', { exact: true })
      .waitFor();
    const creationCalls = await ceremonies();
    assert.equal(creationCalls.length, 1);
    assert.equal(creationCalls[0].kind, 'create');
    assert.equal(
      creationCalls[0].active,
      true,
      'Native creation retains its explicit user gesture',
    );
    assert.ok(
      creationCalls[0].clicks >= 3,
      'Credential creation follows its separate explicit click',
    );
    assert.equal((await request('/api/auth/passkeys', uiRegistered.token)).passkeys.length, 1);
    await closeAccount(account);
    checks.push('ui-password-account-enrolls-first-passkey-with-separate-native-gesture');
    process.stdout.write(JSON.stringify({ browser: 'chromium', checks, passed: true }) + '\n');
  } finally {
    if (cdp && authenticatorId)
      await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId }).catch(() => {});
    await context.close();
    await browser.close();
  }
}

run().catch((error) => {
  // Playwright failures can quote filled values. Never print errors, DOM,
  // screenshots, request bodies or credential material from this secret flow.
  const sourceLocation =
    error instanceof Error
      ? /passkey-management\.cjs:\d+:\d+/.exec(error.stack || '')?.[0]
      : undefined;
  console.error(JSON.stringify({ passed: false, failureStage, sourceLocation }));
  process.exitCode = 1;
});
