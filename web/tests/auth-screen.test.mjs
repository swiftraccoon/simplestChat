import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const authModule = await loadTypeScript('src/auth.ts');
const flowModule = await loadTypeScript('src/auth-dialog.ts');
const supported = {
  accounts: true,
  passwordLogin: true,
  passkeyLogin: true,
  passwordRegistration: 'open',
  passkeyRegistration: 'open',
};
const password = 'A longer password 12!';

async function fixture() {
  const dom = await uiFixture();
  const { document, Node } = dom;
  Node.prototype.focus = function () {
    document.activeElement = this;
  };
  Node.prototype.scrollIntoView = function (options) {
    this.scrolled = options;
  };
  Node.prototype.removeEventListener = function (type, listener) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((entry) => entry.callback !== listener),
    );
  };
  Node.prototype.toggleAttribute = function (name, on) {
    if (on) this.setAttribute(name, '');
    else this.removeAttribute(name);
  };
  Node.prototype.closest = function (selector) {
    assert.equal(selector, '.setting-group');
    return this.parentNode;
  };
  const elements = Object.fromEntries(
    [
      ['signInBtn', 'button'],
      ['loginModal', 'dialog'],
      ['loginClose', 'button'],
      ['loginEmail', 'input'],
      ['loginPassword', 'input'],
      ['loginSubmit', 'button'],
      ['loginError', 'div'],
      ['loginPasskeyBtn', 'button'],
      ['loginToRegister', 'button'],
      ['registerModal', 'dialog'],
      ['registerClose', 'button'],
      ['registerEmail', 'input'],
      ['registerName', 'input'],
      ['registerPassword', 'input'],
      ['registerConfirm', 'input'],
      ['registerInvite', 'input'],
      ['registerSubmit', 'button'],
      ['registerError', 'div'],
      ['registerPasskeyBtn', 'button'],
      ['registerToLogin', 'button'],
      ['registrationHelp', 'p'],
    ].map(([name, tag]) => {
      const element = document.createElement(tag);
      element.id = name;
      document.body.append(element);
      return [name, element];
    }),
  );
  for (const name of ['loginModal', 'registerModal', 'loginError', 'registerError'])
    elements[name].hidden = true;
  elements.loginSubmit.textContent = 'Sign In';
  elements.registerSubmit.textContent = 'Create Account';
  elements.loginPasskeyBtn.textContent = 'Use a passkey';
  elements.registerPasskeyBtn.textContent = 'Register with passkey';
  const groups = {};
  for (const name of ['loginEmail', 'loginPassword', 'registerInvite']) {
    groups[name] = document.createElement('div');
    groups[name].className = 'setting-group';
    document.body.append(groups[name]);
    groups[name].append(elements[name]);
  }
  const state = { capabilities: supported, reads: 0, cancellations: 0, reloads: 0 };
  const requests = [];
  const ceremonies = [];
  const measurements = [];
  const tasks = [];
  const unexpected = [];
  const notifications = [];
  const spent = [];
  const request = (kind, args) => {
    const task = deferred();
    requests.push({ kind, args, ...task });
    return task.promise;
  };
  const ceremony = (kind, options) => {
    const task = deferred();
    ceremonies.push({ kind, options, ...task });
    return task.promise;
  };
  const { AuthScreen } = await loadTypeScript('src/screens/auth.ts', {
    modules: { '../auth': authModule, '../auth-dialog': flowModule, '../ui': dom.ui },
    globals: {
      window: { location: { reload: () => state.reloads++ } },
      navigator: {
        credentials: {
          create: (options) => ceremony('register', options),
          get: (options) => ceremony('login', options),
        },
      },
    },
  });
  const screen = new AuthScreen({
    elements,
    auth: {
      login: (...args) => request('login', args),
      register: (...args) => request('register', args),
      passkeyLoginStart: (...args) => request('passkey-login-start', args),
      passkeyLoginFinish: (...args) => request('passkey-login-finish', args),
      passkeyRegisterStart: (...args) => request('passkey-register-start', args),
      passkeyRegisterFinish: (...args) => request('passkey-register-finish', args),
      cancelPasskeyAttempt: () => state.cancellations++,
    },
    capabilities: () => {
      state.reads++;
      return state.capabilities;
    },
    telemetry: {
      measure: (name, work) => {
        measurements.push(name);
        return work();
      },
    },
    asyncAction: (action, message) => () => {
      tasks.push(action().catch((error) => unexpected.push({ error, message })));
    },
    notify: (...args) => notifications.push(args),
    spendRegistrationInvite: (code) => spent.push(code),
  });
  const settle = async () => {
    await Promise.all(tasks);
    assert.deepEqual(unexpected, [], 'every expected auth failure belongs to its dialog');
  };
  return {
    ...dom,
    elements,
    groups,
    state,
    requests,
    ceremonies,
    measurements,
    notifications,
    spent,
    screen,
    settle,
  };
}

function fillRegistration(elements) {
  elements.registerEmail.value = '  person@example.test  ';
  elements.registerName.value = '  Person  ';
  elements.registerPassword.value = password;
  elements.registerConfirm.value = password;
}

async function authDialogFixture(capabilities, canDismiss = true) {
  const f = await fixture();
  f.state.capabilities = capabilities;
  const effects = [];
  const dialogs = { login: f.elements.loginModal, register: f.elements.registerModal };
  for (const [name, dialog] of Object.entries(dialogs)) {
    const show = dialog.showModal.bind(dialog);
    dialog.showModal = () => {
      effects.push(`open-${name}`);
      show();
    };
    const input = f.elements[`${name}Email`];
    const focus = input.focus.bind(input);
    input.focus = () => {
      effects.push(`focus-${name}`);
      focus();
    };
  }
  const dismiss = f.screen.dismiss.bind(f.screen);
  f.screen.dismiss = () => {
    effects.push('dismiss');
    return canDismiss && dismiss();
  };
  return { screen: f.screen, dialogs, effects, notices: f.notifications };
}

test('unknown or unavailable account support cannot open login or registration', async () => {
  for (const capabilities of [null, { ...supported, accounts: false }]) {
    const f = await authDialogFixture(capabilities);
    for (const [kind, dialog] of Object.entries(f.dialogs)) {
      f.screen.open(kind);
      assert.equal(dialog.hidden, true);
    }
    assert.deepEqual(f.effects, []);
    assert.deepEqual(f.notices, []);
  }
});

test('disabled registration reports its limit while keeping supported login available', async () => {
  const f = await authDialogFixture({
    ...supported,
    passwordRegistration: 'disabled',
    passkeyRegistration: 'disabled',
  });
  f.screen.open('register');
  assert.equal(f.dialogs.register.hidden, true);
  assert.deepEqual(f.effects, []);
  assert.equal(f.notices.length, 1);
  assert.match(f.notices[0][0], /registration is unavailable/i);
  assert.equal(f.notices[0][2], 'error');
  f.screen.open('login');
  assert.equal(f.dialogs.login.hidden, false);
  assert.deepEqual(f.effects, ['dismiss', 'open-login', 'focus-login']);
});

test('each advertised registration method can open registration and focus its form', async () => {
  for (const capabilities of [
    supported,
    { ...supported, passwordRegistration: 'disabled', passkeyRegistration: 'open' },
  ]) {
    const f = await authDialogFixture(capabilities);
    f.screen.open('register');
    assert.equal(f.dialogs.register.hidden, false);
    assert.deepEqual(f.effects, ['dismiss', 'open-register', 'focus-register']);
    assert.deepEqual(f.notices, []);
  }
});

test('known capabilities cannot bypass an auth attempt that refuses dismissal', async () => {
  const f = await authDialogFixture(supported, false);
  f.screen.open('login');
  assert.equal(f.dialogs.login.hidden, true);
  assert.deepEqual(f.effects, ['dismiss']);
});

test('construction reads no host state, and advertised dialog switches retain focus and secret clearing', async () => {
  const f = await fixture();
  const { elements, screen } = f;
  assert.equal(f.state.reads, 0);
  assert.equal(f.requests.length, 0);
  f.state.capabilities = null;
  screen.open('login');
  f.state.capabilities = { ...supported, accounts: false };
  screen.open('register');
  assert.equal(elements.loginModal.open, false);
  assert.equal(elements.registerModal.open, false);
  f.state.capabilities = {
    ...supported,
    passwordRegistration: 'disabled',
    passkeyRegistration: 'disabled',
  };
  screen.open('register');
  assert.deepEqual(f.notifications, [
    ['Account registration is unavailable on this server.', 4000, 'error'],
  ]);
  f.state.capabilities = supported;
  elements.signInBtn.click();
  assert.equal(elements.loginModal.open, true);
  assert.equal(f.document.activeElement, elements.loginEmail);
  elements.loginEmail.emit('keydown', { key: 'Enter' });
  assert.equal(f.document.activeElement, elements.loginPassword);
  elements.loginPassword.value = 'secret';
  elements.loginToRegister.click();
  assert.equal(elements.loginModal.open, false);
  assert.equal(elements.loginPassword.value, '');
  assert.equal(f.document.activeElement, elements.registerEmail);
  elements.registerToLogin.click();
  assert.equal(elements.registerModal.open, false);
  assert.equal(f.document.activeElement, elements.loginEmail);
  assert.equal(f.state.cancellations, 0, 'idle transitions leave startup restoration alone');
});

test('capability rendering preserves the password, passkey and invite vocabulary', async () => {
  const { elements, groups, screen } = await fixture();
  screen.applyCapabilities(
    {
      ...supported,
      passwordLogin: false,
      passkeyRegistration: 'disabled',
      passwordRegistration: 'invite',
    },
    false,
  );
  assert.equal(elements.loginSubmit.hidden, true);
  assert.equal(groups.loginEmail.getAttribute('hidden'), '');
  assert.equal(groups.loginPassword.getAttribute('hidden'), '');
  assert.equal(elements.loginPasskeyBtn.hidden, false);
  assert.equal(elements.registerPasskeyBtn.hidden, true);
  assert.equal(groups.registerInvite.getAttribute('hidden'), null);
  assert.match(
    elements.registrationHelp.textContent,
    /An invite code lets you create a password account/,
  );
  screen.applyCapabilities(supported, false);
  assert.equal(groups.loginEmail.getAttribute('hidden'), null);
  assert.equal(groups.registerInvite.getAttribute('hidden'), '');
  screen.applyCapabilities(supported, true);
  assert.equal(groups.registerInvite.getAttribute('hidden'), null);
  assert.equal(
    elements.registrationHelp.textContent,
    'Choose a sign-in method supported by this server.',
  );
  screen.setRegistrationInvite('link-code');
  assert.equal(elements.registerInvite.value, 'link-code');
});

test('password login owns every dismissal route and Enter submits once until its cookie result is known', async () => {
  const f = await fixture();
  const { screen, elements } = f;
  screen.open('login');
  elements.loginEmail.value = ' person@example.test ';
  elements.loginPassword.value = password;
  elements.registerPassword.value = 'another secret';
  elements.registerConfirm.value = 'another secret';
  elements.loginPassword.emit('keydown', { key: 'Enter' });
  assert.deepEqual(f.requests[0].args, ['person@example.test', password]);
  assert.equal(elements.loginSubmit.textContent, 'Signing in...');
  assert.equal(elements.loginModal.getAttribute('aria-busy'), 'true');
  assert.equal(elements.loginClose.disabled, true);
  elements.loginClose.emit('click');
  elements.loginModal.emit('click');
  let prevented = false;
  elements.loginModal.emit('cancel', {
    preventDefault: () => {
      prevented = true;
    },
  });
  screen.open('register');
  assert.equal(prevented, true);
  assert.equal(screen.dismiss(), false);
  assert.equal(elements.loginModal.open, true);
  assert.equal(elements.registerModal.open, false);
  elements.loginSubmit.emit('click');
  assert.equal(f.requests.length, 1, 'overlapping attempts cannot install different cookies');
  f.requests[0].resolve();
  await f.settle();
  assert.equal(elements.loginModal.open, false);
  assert.equal(elements.loginEmail.value, '');
  for (const name of ['loginPassword', 'registerPassword', 'registerConfirm'])
    assert.equal(elements[name].value, '');
  assert.equal(elements.loginSubmit.textContent, 'Sign In');
  assert.equal(elements.loginSubmit.disabled, false);
  assert.equal(elements.loginClose.disabled, false);
  assert.deepEqual(f.measurements, ['password_login']);
});

test('a confirmed password failure stays in its dialog as text and permits a fresh attempt', async () => {
  const f = await fixture();
  const { elements, screen } = f;
  screen.open('login');
  elements.loginEmail.value = 'person@example.test';
  elements.loginPassword.value = password;
  elements.loginSubmit.click();
  f.requests[0].reject(new Error('<img src=x> is not an account'));
  await f.settle();
  assert.equal(elements.loginError.textContent, '<img src=x> is not an account');
  assert.equal(elements.loginError.hidden, false);
  assert.deepEqual(elements.loginError.scrolled, { block: 'nearest' });
  assert.equal(elements.loginPassword.value, '');
  assert.equal(elements.loginEmail.value, 'person@example.test');
  assert.equal(elements.loginSubmit.disabled, false);
  assert.equal(elements.loginModal.open, true);
  elements.loginSubmit.click();
  assert.equal(f.requests.length, 2);
  f.requests[1].resolve();
  await f.settle();
});

test('password registration validates locally, spends its link once, and clears completed inputs', async () => {
  const f = await fixture();
  const { elements, screen } = f;
  screen.open('register');
  fillRegistration(elements);
  elements.registerPassword.value = 'short';
  elements.registerConfirm.value = 'short';
  elements.registerSubmit.click();
  await f.settle();
  assert.equal(f.requests.length, 0);
  assert.equal(elements.registerError.hidden, false);
  fillRegistration(elements);
  elements.registerConfirm.value = 'a different longer password';
  elements.registerSubmit.click();
  await f.settle();
  assert.equal(f.requests.length, 0);
  assert.deepEqual(f.spent, []);
  fillRegistration(elements);
  screen.setRegistrationInvite('  ABCdef  ');
  elements.registerSubmit.click();
  assert.deepEqual(f.requests[0].args, ['person@example.test', 'Person', password, 'ABCdef']);
  assert.deepEqual(f.spent, ['ABCdef']);
  assert.equal(elements.registerSubmit.textContent, 'Creating account...');
  assert.equal(screen.dismiss(), false);
  f.requests[0].resolve();
  await f.settle();
  for (const name of [
    'registerEmail',
    'registerName',
    'registerInvite',
    'registerPassword',
    'registerConfirm',
  ])
    assert.equal(elements[name].value, '', name);
  assert.equal(elements.registerModal.open, false);
  assert.deepEqual(f.measurements, ['password_register']);
});

for (const kind of ['login', 'register']) {
  test(`an unconfirmed ${kind} mutation clears secrets and stays locked until reload or external retirement`, async () => {
    const f = await fixture();
    const { elements, screen } = f;
    screen.open(kind);
    fillRegistration(elements);
    elements.loginPassword.value = password;
    elements[`${kind}Submit`].click();
    f.requests[0].reject(new authModule.SessionOutcomeUnknownError());
    await f.settle();
    assert.equal(screen.dismiss(), false);
    assert.equal(elements[`${kind}Modal`].open, true);
    assert.equal(elements[`${kind}Modal`].getAttribute('aria-busy'), 'false');
    for (const name of [
      'loginSubmit',
      'loginPasskeyBtn',
      'registerSubmit',
      'registerPasskeyBtn',
      'loginClose',
      'registerClose',
    ])
      assert.equal(elements[name].disabled, true, name);
    for (const name of ['loginPassword', 'registerPassword', 'registerConfirm'])
      assert.equal(elements[name].value, '', name);
    assert.equal(elements[`${kind}Submit`].textContent, 'Response not confirmed');
    const reload = elements[`${kind}Error`].querySelector('button');
    assert.equal(reload.textContent, 'Reload and check session');
    reload.click();
    assert.equal(f.state.reloads, 1);
    screen.retire();
    assert.equal(elements[`${kind}Modal`].open, false);
    assert.equal(elements[`${kind}Submit`].disabled, false);
    assert.equal(f.state.cancellations, 1);
  });
}

for (const registration of [false, true]) {
  const kind = registration ? 'register' : 'login';
  test(`passkey ${kind} keeps its ceremony cancellable and locks dismissal only before session finish`, async () => {
    const f = await fixture();
    const { elements, screen } = f;
    screen.open(kind);
    fillRegistration(elements);
    elements[`${kind}PasskeyBtn`].click();
    const start = f.requests[0];
    assert.equal(start.kind, `passkey-${kind}-start`);
    const signal = start.args.at(-1);
    if (registration) assert.deepEqual(start.args.slice(0, 2), ['person@example.test', 'Person']);
    assert.equal(signal.aborted, false);
    assert.equal(elements[`${kind}Close`].disabled, false);
    start.resolve({ publicKey: { challenge: 'fixture' } });
    await flush();
    assert.equal(f.ceremonies[0].kind, kind);
    assert.equal(f.ceremonies[0].options.signal, signal);
    const credential = { id: 'fixture-credential' };
    f.ceremonies[0].resolve(credential);
    await flush();
    assert.equal(f.requests[1].kind, `passkey-${kind}-finish`);
    assert.deepEqual(f.requests[1].args, [credential]);
    assert.equal(screen.dismiss(), false);
    assert.equal(elements[`${kind}Modal`].getAttribute('aria-busy'), 'true');
    f.requests[1].resolve();
    await f.settle();
    assert.equal(elements[`${kind}Modal`].open, false);
    assert.equal(elements[`${kind}PasskeyBtn`].disabled, false);
    assert.equal(
      elements[`${kind}PasskeyBtn`].textContent,
      registration ? 'Register with passkey' : 'Use a passkey',
    );
    assert.deepEqual(f.measurements, [
      `passkey_${kind}_start`,
      `passkey_${kind}_ceremony`,
      `passkey_${kind}_finish`,
    ]);
  });
  for (const phase of ['start', 'ceremony']) {
    test(`closing passkey ${kind} during ${phase} aborts ownership and ignores a late credential`, async () => {
      const f = await fixture();
      const { elements, screen } = f;
      screen.open(kind);
      fillRegistration(elements);
      elements[`${kind}PasskeyBtn`].click();
      const start = f.requests[0];
      const signal = start.args.at(-1);
      if (phase === 'ceremony') {
        start.resolve({});
        await flush();
      }
      assert.equal(screen.dismiss(), true);
      assert.equal(signal.aborted, true);
      assert.equal(f.state.cancellations, 1);
      screen.open(kind);
      elements[`${kind}PasskeyBtn`].click();
      assert.equal(elements[`${kind}PasskeyBtn`].disabled, true);
      if (phase === 'start') start.resolve({});
      else f.ceremonies[0].resolve({ id: 'stale-credential' });
      await flush();
      assert.equal(f.requests.length, 2, 'a stale result never starts session creation');
      assert.equal(
        elements[`${kind}PasskeyBtn`].disabled,
        true,
        'a stale finally never resets a newer attempt',
      );
      screen.retire();
      f.requests[1].resolve({});
      await f.settle();
    });
  }
}

test('passkey registration requires identifying fields and cancellation reports retry guidance', async () => {
  const f = await fixture();
  const { elements, screen } = f;
  screen.open('register');
  elements.registerPasskeyBtn.click();
  await f.settle();
  assert.equal(f.requests.length, 0);
  assert.equal(elements.registerError.textContent, 'Fill in email and display name first');
  screen.open('login');
  elements.loginPasskeyBtn.click();
  f.requests[0].resolve({});
  await flush();
  f.ceremonies[0].resolve(null);
  await f.settle();
  assert.match(
    elements.loginError.textContent,
    /Passkey request cancelled or timed out.*try again.*Passkey help/,
  );
  assert.equal(elements.loginPasskeyBtn.disabled, false);
  assert.equal(elements.loginModal.open, true);
});

for (const outcome of ['resolve', 'reject']) {
  test(`external retirement prevents late password ${outcome} from touching a replacement attempt`, async () => {
    const f = await fixture();
    const { elements, screen } = f;
    screen.open('login');
    elements.loginPassword.value = password;
    elements.loginSubmit.click();
    screen.retire();
    screen.open('register');
    fillRegistration(elements);
    elements.registerSubmit.click();
    if (outcome === 'resolve') f.requests[0].resolve();
    else f.requests[0].reject(new authModule.SessionOutcomeUnknownError());
    await flush();
    assert.equal(elements.registerModal.open, true);
    assert.equal(elements.registerPassword.value, password);
    assert.equal(elements.registerSubmit.textContent, 'Creating account...');
    assert.equal(elements.loginError.hidden, true);
    f.requests[1].resolve();
    await f.settle();
  });
}

test('dispose removes listeners and retires every pending result without rereading host state', async () => {
  const f = await fixture();
  const { screen, elements } = f;
  screen.open('login');
  elements.loginPassword.value = password;
  elements.loginSubmit.click();
  screen.dispose();
  screen.dispose();
  const reads = f.state.reads;
  elements.signInBtn.click();
  screen.open('register');
  screen.setRegistrationInvite('ignored');
  screen.applyCapabilities(supported, true);
  assert.equal(f.state.reads, reads);
  assert.equal(elements.registerInvite.value, '');
  assert.equal(elements.loginPassword.value, '');
  for (const element of Object.values(elements))
    assert.ok([...element.listeners.values()].every((listeners) => listeners.length === 0));
  f.requests[0].reject(new authModule.SessionOutcomeUnknownError());
  await f.settle();
  assert.equal(elements.loginError.hidden, true);
  assert.equal(f.state.cancellations, 1);
});
