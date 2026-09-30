import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript } from './source-loader.mjs';

async function fixture() {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === 'acceptPendingInvite',
  );
  assert.ok(declaration, 'Invitation redemption must retain a named ownership boundary');
  const response = Promise.withResolvers();
  const auth = { userId: 'first-account', jwt: 'first-token', isLoggedIn: true };
  const navigation = { revision: 1 };
  const requests = [];
  const opened = [];
  const notifications = [];
  const api = evaluateTypeScript(
    `let pendingInvite = 'abcdefghijklmnopqrst';
     let inviteAccountEpoch = 0;
     ${declaration.getText(ast)}
     export { acceptPendingInvite };
     export function nextAccountEpoch() { inviteAccountEpoch++; }`,
    {
      globals: {
        auth,
        navigation,
        api: {
          redeemInvite: (token, code) => {
            requests.push({ token, code });
            return response.promise;
          },
        },
        showToast: (...args) => notifications.push(args),
        openRoomFromDialog: (id) => opened.push(id),
      },
    },
  );
  return { ...api, auth, navigation, requests, opened, notifications, response };
}

const accepted = { room_id: 'invited-room', display_name: 'Invited room', role: 'user' };

test('an owned invitation is redeemed once and hands the join to navigation once', async () => {
  const f = await fixture();
  const task = f.acceptPendingInvite();
  await f.acceptPendingInvite();
  assert.deepEqual(f.requests, [{ token: 'first-token', code: 'abcdefghijklmnopqrst' }]);
  assert.deepEqual(f.opened, []);
  f.response.resolve(accepted);
  await task;
  await f.acceptPendingInvite();
  assert.deepEqual(f.opened, ['invited-room']);
  assert.equal(f.requests.length, 1);
  assert.match(f.notifications[0][0], /Joining with your current permissions/);
});

test('refreshing the same account token preserves in-flight invitation ownership', async () => {
  const f = await fixture();
  const task = f.acceptPendingInvite();
  f.auth.jwt = 'renewed-token';
  f.response.resolve(accepted);
  await task;
  assert.deepEqual(f.opened, ['invited-room']);
  assert.equal(f.requests.length, 1, 'token renewal must not repeat a mutation');
});

for (const change of ['destination', 'signout', 'different-account', 'same-account-new-session']) {
  for (const outcome of ['accepted', 'failed']) {
    test(`${change} retires an invitation whose late response is ${outcome}`, async () => {
      const f = await fixture();
      const task = f.acceptPendingInvite();
      if (change === 'destination') f.navigation.revision++;
      if (change === 'signout') f.auth.isLoggedIn = false;
      if (change === 'different-account') f.auth.userId = 'second-account';
      if (change === 'same-account-new-session') f.nextAccountEpoch();
      if (outcome === 'accepted') f.response.resolve(accepted);
      else f.response.reject(new Error('A previous account request failed'));
      await task;
      assert.deepEqual(f.opened, []);
      assert.deepEqual(f.notifications, [], 'retired work cannot notify the replacement context');
      assert.equal(f.requests.length, 1);
    });
  }
}

test('an owned failed invitation reports its error without retrying the mutation', async () => {
  const f = await fixture();
  const task = f.acceptPendingInvite();
  f.response.reject(new Error('Invitation expired'));
  await task;
  await f.acceptPendingInvite();
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.notifications, [['Invitation expired', 4000, 'error']]);
  assert.equal(f.requests.length, 1);
});

test('an invitation waits for sign-in without spending its code as a guest', async () => {
  const f = await fixture();
  f.auth.isLoggedIn = false;
  await f.acceptPendingInvite();
  assert.deepEqual(f.requests, []);
  f.auth.isLoggedIn = true;
  const task = f.acceptPendingInvite();
  f.response.resolve(accepted);
  await task;
  assert.deepEqual(f.opened, ['invited-room']);
  assert.equal(f.requests.length, 1);
});
