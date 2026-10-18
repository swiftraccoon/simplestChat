import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import ts from '@typescript/typescript6';
import { evaluateTypeScript } from './source-loader.mjs';

const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
const functions = ['updateRestoreUI', 'handleRestoreState', 'restoreSavedSignIn'].map((name) => {
  const declaration = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
  assert.ok(declaration, `${name} must retain a named restoration boundary`);
  return declaration.getText(ast);
});

function fixture({ accounts = true } = {}) {
  const effects = [];
  const tasks = [];
  const requests = [];
  const authRestoreNotice = { hidden: true };
  const authRestoreStatus = { textContent: '' };
  const authRestoreRetry = { disabled: false };
  const document = {
    activeElement: null,
    getElementById: (id) => ({
      focus() {
        document.activeElement = this;
        effects.push(`focus:${id}`);
      },
    }),
  };
  const auth = {
    restoreState: 'idle',
    isLoggedIn: false,
    canCheckSharedSession: true,
    tryRestore() {
      this.restoreState = 'checking';
      this.canCheckSharedSession = false;
      const request = Promise.withResolvers();
      requests.push(request);
      api.handleRestoreState('checking');
      return request.promise;
    },
  };
  const api = evaluateTypeScript(
    `let restoredAccount = false;
     const pendingInvite = 'pending-invitation';
     function updateAuthUI() { effects.push('auth-ui'); updateRestoreUI(); }
     ${functions.join('\n')}
     export { updateRestoreUI, handleRestoreState, restoreSavedSignIn };
     export const accountChecked = () => restoredAccount;`,
    {
      globals: {
        auth,
        capabilities: { accounts },
        document,
        authRestoreNotice,
        authRestoreStatus,
        authRestoreRetry,
        effects,
        signaling: { connect: () => effects.push('guest-socket') },
        reviewPendingContact: () => effects.push('contact'),
        previewPendingInvite: () => {
          effects.push('invitation');
          return Promise.resolve();
        },
        observeUiTask: (task) => tasks.push(task),
      },
    },
  );
  const notify = (state) => {
    auth.restoreState = state;
    auth.canCheckSharedSession = state !== 'checking';
    api.handleRestoreState(state);
  };
  return {
    ...api,
    auth,
    notify,
    effects,
    tasks,
    requests,
    document,
    authRestoreNotice,
    authRestoreStatus,
    authRestoreRetry,
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
  f.document.getElementById = () => ({ focus: () => f.notify('superseded') });
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
