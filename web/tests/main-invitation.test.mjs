import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript } from './source-loader.mjs';
import { uiFixture, flush } from './ui-fixture.mjs';

async function functionSource(name) {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(declaration, `${name} must retain a named ownership boundary`);
  return declaration.getText(ast);
}

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
  const api = evaluateTypeScript(
    `let pendingInvite = '${'a'.repeat(32)}';
     let pendingInviteKind = 'room';
     let inviteAccountEpoch = 0;
     let inviteView = null;
     ${await functionSource('previewPendingInvite')}
     export { previewPendingInvite };
     export const view = () => inviteView;
     export function nextAccountEpoch() { inviteAccountEpoch++; }
     export function registration() { pendingInviteKind = 'registration'; }`,
    {
      globals: {
        ...f.ui,
        room: null,
        auth,
        navigation,
        loginModal: 'login',
        registerModal: 'register',
        openAuthDialog: (view) => authDialogs.push(view),
        observeUiTask: (task) => tasks.push(task),
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
        showToast: (...args) => notifications.push(args),
      },
    },
  );
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
        .body.querySelectorAll('button')
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

for (const [suffix, kind] of [
  [`#invite=${'a'.repeat(32)}`, 'room'],
  [`#register-invite=${'a'.repeat(32)}`, 'registration'],
]) {
  test(`invitation extraction removes the secret before room navigation: ${kind} ${suffix[0]}`, async () => {
    const registerInvite = { value: '' },
      replaced = [];
    const api = evaluateTypeScript(
      `let pendingInvite = null; let pendingInviteKind = 'room'; ${await functionSource('readInviteLink')} export {readInviteLink}; export const pending = () => ({code:pendingInvite,kind:pendingInviteKind});`,
      {
        globals: {
          URL,
          URLSearchParams,
          registerInvite,
          window: {
            location: { href: `https://example.test/${suffix}` },
            history: { replaceState: (...args) => replaced.push(args[2]) },
          },
          showToast: () => assert.fail('valid link'),
        },
      },
    );
    api.readInviteLink();
    assert.equal(api.pending().kind, kind);
    assert.equal(api.pending().code, registerInvite.value);
    assert.equal(replaced[0], suffix.includes('#room') ? '/#room' : '/');
  });
}

for (const suffix of [
  `?invite=${'a'.repeat(32)}`,
  `#invite=${'a'.repeat(20)}`,
  `#invite=${'1'.repeat(32)}`,
]) {
  test(`unsupported invitation format cannot create a pending offer: ${suffix.slice(0, 12)}`, async () => {
    const registerInvite = { value: '' };
    const api = evaluateTypeScript(
      `let pendingInvite = null; let pendingInviteKind = 'room'; ${await functionSource('readInviteLink')} export {readInviteLink}; export const pending = () => pendingInvite;`,
      {
        globals: {
          URL,
          URLSearchParams,
          registerInvite,
          window: {
            location: { href: `https://example.test/${suffix}` },
            history: { replaceState() {} },
          },
          showToast() {},
        },
      },
    );
    api.readInviteLink();
    assert.equal(api.pending(), null);
    assert.equal(registerInvite.value, '');
  });
}
