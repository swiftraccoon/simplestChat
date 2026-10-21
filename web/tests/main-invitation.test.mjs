import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { uiFixture, flush } from './ui-fixture.mjs';

async function fixture() {
  const f = await uiFixture();
  const preview = Promise.withResolvers();
  const redemption = Promise.withResolvers();
  const auth = { userId: 'first-account', jwt: 'first-token', isLoggedIn: true };
  const selected = [];
  const navigation = { revision: 1, selectRoom: (id) => selected.push(id) };
  const previews = [],
    redemptions = [],
    notifications = [],
    tasks = [],
    authDialogs = [];
  const store = { capabilities: { accounts: true }, restoredAccount: true, room: null };
  const window = {
    location: { href: `https://example.test/#invite=${'a'.repeat(32)}` },
    history: { replaceState() {} },
  };
  const { createInvitations } = await loadTypeScript('src/screens/invitations.ts', {
    modules: {
      '../ui': {
        ...f.ui,
        api: {
          previewInvite: (token, code) => {
            previews.push({ token, code });
            return preview.promise;
          },
          redeemInvite: (token, code) => {
            redemptions.push({ token, code });
            return redemption.promise;
          },
        },
      },
    },
    globals: { window },
  });
  const controller = createInvitations(store, {
    auth,
    navigation,
    appNavigation: { preserveLocation() {}, retire() {} },
    community: { requestContact() {} },
    authScreen: { open: (kind) => authDialogs.push(kind), setRegistrationInvite() {} },
    takeContactOffer: () => null,
    notify: (...args) => notifications.push(args),
    observe: (task) => tasks.push(task),
  });
  controller.readInviteLink();
  const api = {
    previewPendingInvite: controller.previewPendingInvite,
    view: () => controller.view,
    accountChecked: (value) => {
      store.restoredAccount = value;
    },
    nextAccountEpoch: controller.accountChanged,
    registration: () => {
      window.location.href = `https://example.test/#register-invite=${'a'.repeat(32)}`;
      controller.readInviteLink();
    },
  };
  return {
    ...api,
    auth,
    navigation,
    selected,
    previews,
    redemptions,
    notifications,
    preview,
    redemption,
    tasks,
    authDialogs,
    accept: () =>
      api
        .view()
        ?.body.querySelectorAll('button')
        .find((button) => button.textContent === 'Accept invitation'),
  };
}
const offer = { room_id: 'invited-room', display_name: 'Invited room', role: 'member' };

test('opening an invitation previews its room and role, requiring explicit acceptance and a separate join', async () => {
  const f = await fixture();
  const task = f.previewPendingInvite();
  await f.previewPendingInvite();
  assert.equal(f.previews.length, 1);
  assert.deepEqual(f.redemptions, []);
  assert.equal(f.accept(), undefined);
  f.preview.resolve(offer);
  await task;
  assert.match(f.view().body.textContent, /Invited room.*invited-room.*Offered role: member/);
  assert.deepEqual(f.redemptions, []);
  f.accept().click();
  f.accept().click();
  assert.equal(f.redemptions.length, 1, 'a second click cannot repeat an in-flight mutation');
  assert.deepEqual(f.selected, []);
  f.redemption.resolve(offer);
  await Promise.all(f.tasks);
  assert.deepEqual(f.selected, ['invited-room']);
  assert.match(f.notifications[0][0], /Choose Join when ready/);
  await f.previewPendingInvite();
  assert.equal(f.previews.length, 1);
});

test('refreshing the same account token preserves the displayed invitation', async () => {
  const f = await fixture();
  f.preview.resolve(offer);
  await f.previewPendingInvite();
  f.auth.jwt = 'renewed-token';
  f.accept().click();
  assert.equal(f.redemptions[0].token, 'renewed-token');
  f.redemption.resolve(offer);
  await Promise.all(f.tasks);
  assert.deepEqual(f.selected, ['invited-room']);
});

for (const change of [
  'destination',
  'signout',
  'different-account',
  'same-account-new-session',
  'dismissed',
]) {
  for (const phase of ['preview', 'accept']) {
    test(`${change} retires a late invitation ${phase}`, async () => {
      const f = await fixture();
      const previewTask = f.previewPendingInvite();
      if (phase === 'accept') {
        f.preview.resolve(offer);
        await previewTask;
        f.accept().click();
      }
      if (change === 'destination') f.navigation.revision++;
      if (change === 'signout') f.auth.isLoggedIn = false;
      if (change === 'different-account') f.auth.userId = 'second-account';
      if (change === 'same-account-new-session') f.nextAccountEpoch();
      if (change === 'dismissed') f.view().close();
      f.preview.resolve(offer);
      f.redemption.resolve(offer);
      await previewTask;
      await Promise.all(f.tasks);
      assert.deepEqual(f.selected, []);
      assert.deepEqual(f.notifications, []);
      if (phase === 'preview') assert.equal(f.accept(), undefined);
    });
  }
}

test('failed acceptance reports its owned error without automatic retry', async () => {
  const f = await fixture();
  f.preview.resolve(offer);
  await f.previewPendingInvite();
  f.accept().click();
  f.redemption.reject(new Error('Invitation expired'));
  await Promise.all(f.tasks);
  await flush();
  assert.equal(f.view().error.textContent, 'Invitation expired');
  assert.equal(f.accept().disabled, false);
  assert.deepEqual(f.selected, []);
  assert.equal(f.redemptions.length, 1);
});

test('guests choose sign-in before review, and registration links only open their explicit form', async () => {
  const f = await fixture();
  f.auth.isLoggedIn = false;
  await f.previewPendingInvite();
  assert.deepEqual(f.previews, []);
  assert.deepEqual(f.redemptions, []);
  f.view().body.querySelector('button').click();
  assert.deepEqual(f.authDialogs, ['login']);
  const registration = await fixture();
  registration.registration();
  await registration.previewPendingInvite();
  assert.deepEqual(registration.authDialogs, ['register']);
  assert.deepEqual(registration.previews, []);
  assert.deepEqual(registration.redemptions, []);
});

for (const registration of [false, true]) {
  test(`${registration ? 'registration' : 'room'} invitation waits for a verified saved-sign-in result`, async () => {
    const f = await fixture();
    f.auth.isLoggedIn = false;
    f.accountChecked(false);
    if (registration) f.registration();
    await f.previewPendingInvite();
    assert.equal(f.view(), null);
    assert.deepEqual(f.authDialogs, []);
    assert.deepEqual(f.previews, []);
    assert.deepEqual(f.redemptions, []);

    f.accountChecked(true);
    await f.previewPendingInvite();
    if (registration) assert.deepEqual(f.authDialogs, ['register']);
    else assert.match(f.view().body.textContent, /Sign in to review/);
    assert.deepEqual(f.redemptions, []);
  });
}

async function linkFixture(suffix) {
  const f = await uiFixture();
  const registerInvite = { value: '' },
    replaced = [];
  const window = {
    location: { href: `https://example.test/${suffix}` },
    history: { replaceState: (...args) => replaced.push(args[2]) },
  };
  const { createInvitations } = await loadTypeScript('src/screens/invitations.ts', {
    modules: { '../ui': f.ui },
    globals: { window },
  });
  const controller = createInvitations(
    { capabilities: null, restoredAccount: false, room: null },
    {
      auth: { userId: null, jwt: null, isLoggedIn: false },
      navigation: {},
      appNavigation: {},
      community: {},
      authScreen: {
        setRegistrationInvite: (code) => {
          registerInvite.value = code;
        },
      },
      takeContactOffer: () => null,
      notify() {},
      observe() {},
    },
  );
  return { controller, registerInvite, replaced };
}
for (const [suffix, kind] of [
  [`#invite=${'a'.repeat(32)}`, 'room'],
  [`#register-invite=${'a'.repeat(32)}`, 'registration'],
]) {
  test(`invitation extraction removes the secret before room navigation: ${kind} ${suffix[0]}`, async () => {
    const { controller, registerInvite, replaced } = await linkFixture(suffix);
    controller.readInviteLink();
    assert.equal(controller.pendingKind, kind);
    assert.equal(controller.pendingCode, registerInvite.value);
    assert.equal(replaced[0], '/');
  });
}
for (const suffix of [
  `?invite=${'a'.repeat(32)}`,
  `#invite=${'a'.repeat(20)}`,
  `#invite=${'1'.repeat(32)}`,
]) {
  test(`unsupported invitation format cannot create a pending offer: ${suffix.slice(0, 12)}`, async () => {
    const { controller, registerInvite } = await linkFixture(suffix);
    controller.readInviteLink();
    assert.equal(controller.pendingCode, null);
    assert.equal(registerInvite.value, '');
  });
}
