import assert from 'node:assert/strict';
import test from 'node:test';
import { capabilities as supported } from '../e2e/capabilities-fixture.cjs';
import { homeScreenFixture } from './home-screen-fixture.mjs';

function fixture({ accounts = true } = {}) {
  const f = homeScreenFixture({ capabilities: { ...supported, accounts } });
  const effects = [],
    requests = [];
  const auth = f.services.auth;
  auth.tryRestore = function () {
    this.restoreState = 'checking';
    this.canCheckSharedSession = false;
    const request = Promise.withResolvers();
    requests.push(request);
    f.screen.handleRestoreState('checking');
    return request.promise;
  };
  f.services.community.refresh = () => effects.push('auth-ui');
  f.services.signaling.connect = () => effects.push('guest-socket');
  f.services.reviewPendingContact = () => effects.push('contact');
  f.services.previewPendingInvite = () => {
    effects.push('invitation');
    return Promise.resolve();
  };
  f.services.hasPendingInvite = () => true;
  for (const [name, id] of [
    ['accountMenuBtn', 'account-menu-btn'],
    ['signInBtn', 'sign-in-btn'],
  ])
    f.elements[name].focus = function () {
      f.document.activeElement = this;
      effects.push(`focus:${id}`);
    };
  const notify = (state) => {
    auth.restoreState = state;
    auth.canCheckSharedSession = state !== 'checking';
    f.screen.handleRestoreState(state);
  };
  return {
    ...f.screen,
    auth,
    notify,
    effects,
    requests,
    document: f.document,
    get tasks() {
      return f.observed.map(({ task }) => task);
    },
    accountChecked: () => f.store.restoredAccount,
    authRestoreNotice: f.elements.authRestoreNotice,
    authRestoreStatus: f.elements.authRestoreStatus,
    authRestoreRetry: f.elements.authRestoreRetry,
    accountMenuBtn: f.elements.accountMenuBtn,
    signInBtn: f.elements.signInBtn,
    services: f.services,
  };
}

test('an unavailable check offers recovery while guest use and account intent remain separate', () => {
  const f = fixture();
  f.notify('unavailable');
  assert.equal(f.authRestoreNotice.hidden, false);
  assert.match(f.authRestoreStatus.textContent, /couldn’t check your saved sign-in/i);
  assert.equal(f.authRestoreRetry.disabled, false);
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, ['guest-socket']);
});

test('an explicit retry owns one pending request and cannot replay terminal UI work', async () => {
  const f = fixture();
  f.notify('unavailable');
  f.effects.length = 0;
  f.restoreSavedSignIn();
  f.restoreSavedSignIn();
  assert.equal(f.requests.length, 1);
  assert.equal(f.authRestoreNotice.hidden, false);
  assert.equal(f.authRestoreRetry.disabled, true);
  assert.match(f.authRestoreStatus.textContent, /checking saved sign-in/i);
  assert.deepEqual(f.effects, []);

  f.notify('superseded');
  f.requests[0].resolve('restored');
  await Promise.all(f.tasks);
  assert.equal(f.authRestoreNotice.hidden, true);
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, [], 'a retired promise result never owns account or socket effects');
});

for (const result of ['restored', 'unauthenticated']) {
  test(`verified ${result} releases account intent without duplicating an authenticated connection`, async () => {
    const f = fixture();
    f.auth.isLoggedIn = result === 'restored';
    f.document.activeElement = f.authRestoreRetry;
    f.notify(result);
    await Promise.all(f.tasks);
    assert.equal(f.authRestoreNotice.hidden, true);
    assert.equal(f.authRestoreStatus.textContent, '');
    assert.equal(f.accountChecked(), true);
    assert.deepEqual(f.effects, [
      `focus:${result === 'restored' ? 'account-menu-btn' : 'sign-in-btn'}`,
      'auth-ui',
      'contact',
      ...(result === 'unauthenticated' ? ['guest-socket'] : []),
      'invitation',
    ]);
  });
}

test('guest-only servers and a newer authentication operation cannot start a recovery request', () => {
  const guest = fixture({ accounts: false });
  guest.notify('unavailable');
  guest.restoreSavedSignIn();
  assert.equal(guest.authRestoreNotice.hidden, true);
  assert.deepEqual(guest.requests, []);

  const f = fixture();
  f.notify('superseded');
  f.auth.canCheckSharedSession = false;
  f.restoreSavedSignIn();
  f.auth.canCheckSharedSession = true;
  f.auth.isLoggedIn = true;
  f.restoreSavedSignIn();
  assert.deepEqual(f.requests, []);
  assert.deepEqual(f.effects, []);
});

test('focus-triggered retirement prevents stale account intent from being finalized', () => {
  const f = fixture();
  f.document.activeElement = f.authRestoreRetry;
  f.signInBtn.focus = () => f.notify('superseded');
  f.notify('unauthenticated');
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, []);
});

test('a later unconfirmed shared check defers account links again', async () => {
  const f = fixture();
  f.notify('unauthenticated');
  await Promise.all(f.tasks);
  assert.equal(f.accountChecked(), true);
  f.effects.length = 0;
  f.notify('checking');
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, []);
  f.notify('unavailable');
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, ['guest-socket']);
});

test('render-triggered retirement prevents a verified outcome from releasing account intent', () => {
  const f = fixture();
  f.services.community.refresh = () => f.notify('superseded');
  f.notify('unauthenticated');
  assert.equal(f.accountChecked(), false);
  assert.deepEqual(f.effects, []);
});
