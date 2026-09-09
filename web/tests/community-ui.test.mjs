import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

async function fixture() {
  const dom = await uiFixture();
  const auth = { isLoggedIn: true, userId: 'account-a', jwt: 'token-a' };
  const profile = { id: 'account-a', email: 'a@example.test', display_name: 'Alice', bio: '', avatar_url: null, recovery_enabled: false };
  const state = { requests: [], notifications: [], updated: [], signedOut: 0, room: null,
    handle: async () => ({ ...profile }), upload: dom.ui.rasterUpload };
  const api = await loadTypeScript('src/community-ui.ts', {
    modules: { './ui': { ...dom.ui,
      api: async (...args) => { state.requests.push(args); return state.handle(...args); },
      rasterUpload: (...args) => state.upload(...args),
    } },
    globals: { document: dom.document, TextEncoder, URL,
      window: { location: { href: 'http://localhost:3000/?old=value#old-room' }, confirm: () => true },
      navigator: { clipboard: { writeText: async value => { state.copied = value; } } },
    },
  });
  const community = new api.CommunityUI({ auth, getRoom: () => state.room,
    notify: value => state.notifications.push(value), onProfileChanged: value => state.updated.push(value),
    onRoomsChanged() {}, async onRoomDeleted() {}, async onSignedOut() { state.signedOut++; },
  });
  return { ...dom, ...api, auth, state, profile, community };
}

function dialog(fixture, title) {
  const node = fixture.document.querySelectorAll('dialog').find(node => node.children[0].children[0].textContent === title);
  assert.ok(node, `Expected ${title} dialog`);
  return node;
}

function control(view, label) {
  const wrapper = view.querySelectorAll('label').find(node => node.children[0].textContent === label);
  assert.ok(wrapper, `Expected ${label} field`);
  return wrapper.children[1];
}

function action(view, text) {
  const node = view.querySelectorAll('button').find(node => node.textContent === text);
  assert.ok(node, `Expected ${text} action`);
  return node;
}

test('recovery validates UTF-8 byte boundaries, confirmation, and control characters before sending', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  const password = control(view, 'New password');
  const confirm = control(view, 'Confirm new password');
  const submit = action(view, 'Reset password with key');
  for (const [value, confirmation, error] of [
    ['1234567', '1234567', /8 and 128 bytes/],
    ['a'.repeat(129), 'a'.repeat(129), /8 and 128 bytes/],
    ['é'.repeat(65), 'é'.repeat(65), /8 and 128 bytes/],
    ['password1', 'password2', /do not match/],
    ['password\t', 'password\t', /control characters/],
    ['password\u0085', 'password\u0085', /control characters/],
  ]) {
    password.value = value; confirm.value = confirmation; submit.click(); await flush();
    assert.equal(f.state.requests.length, 0);
    assert.match(view.children[1].textContent, error);
    assert.equal(submit.disabled, false);
  }
  control(view, 'Email').value = ' a@example.test ';
  const key = control(view, 'Saved recovery key'); key.value = ' saved-test-key ';
  password.value = confirm.value = 'é'.repeat(64); // Exactly 128 UTF-8 bytes.
  submit.click(); await flush();
  assert.deepEqual(f.state.requests[0], ['/api/auth/recovery/redeem', null, 'POST', {
    email: 'a@example.test', recovery_key: 'saved-test-key', new_password: 'é'.repeat(64),
  }]);
  assert.equal(key.value, '');
  assert.equal(view.isConnected, false);
  assert.match(f.state.notifications[0], /Password reset/);
});

test('recovery accepts an eight-byte multibyte password without altering its characters', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  control(view, 'New password').value = control(view, 'Confirm new password').value = 'éééé';
  action(view, 'Reset password with key').click(); await flush();
  assert.equal(f.state.requests[0][3].new_password, 'éééé');
});

test('profile text remains text and remote avatar URLs are not rendered', async () => {
  const f = await fixture();
  f.state.handle = async () => ({ ...f.profile, display_name: '<b>Alice</b>', bio: '<p>Biography</p>', avatar_url: 'https://example.test/avatar.png' });
  await f.community.showProfile('account-a');
  const view = dialog(f, 'Profile');
  assert.equal(view.querySelectorAll('h3')[0].textContent, '<b>Alice</b>');
  assert.equal(view.querySelectorAll('p')[1].textContent, '<p>Biography</p>');
  assert.equal(view.querySelectorAll('img').length, 0);
});

test('avatars decorate connected authenticated nodes only and retain text for guests', async () => {
  const f = await fixture();
  const pending = deferred(); f.state.handle = () => pending.promise;
  const guest = f.ui.el('div', 'Guest'); f.document.body.append(guest);
  f.community.decorateAvatar(guest, 'guest-id', false);
  assert.equal(f.state.requests.length, 0);
  const live = f.ui.el('div', 'A'); f.document.body.append(live);
  const removed = f.ui.el('div', 'A'); f.document.body.append(removed);
  f.community.decorateAvatar(live, 'account-a', true);
  f.community.decorateAvatar(removed, 'account-a', true);
  removed.remove();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' }); await flush();
  assert.equal(f.state.requests.length, 1, 'profile lookups should be shared');
  assert.equal(live.children[0].tagName, 'IMG');
  assert.equal(live.children[0].alt, '');
  assert.equal(removed.textContent, 'A');
  assert.equal(guest.textContent, 'Guest');
});

test('closing a profile while it loads prevents detached content from being rendered', async () => {
  const f = await fixture();
  const pending = deferred(); f.state.handle = () => pending.promise;
  const opening = f.community.showProfile('account-a');
  const view = dialog(f, 'Profile'); view.close();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' }); await opening;
  assert.equal(view.querySelectorAll('img').length, 0);
  assert.equal(view.querySelectorAll('h3').length, 0);
});

test('image removal wins over a pending upload and cannot be undone by its late result', async () => {
  const f = await fixture();
  const pending = deferred(); f.state.upload = () => pending.promise;
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/png', size: 1024 }]; file.emit('change');
  assert.throws(() => picker.value(), /finish processing/);
  action(picker.wrapper, 'Remove image').click();
  pending.resolve('data:image/jpeg;base64,Yg=='); await flush();
  assert.equal(picker.value(), null);
  assert.equal(picker.wrapper.querySelectorAll('img')[0].hidden, true);
  assert.equal(file.disabled, false);
});

test('invalid uploads preserve the current image and display the validation failure safely', async () => {
  const f = await fixture();
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/svg+xml', size: 100 }]; file.emit('change'); await flush();
  assert.equal(picker.value(), 'data:image/jpeg;base64,YQ==');
  assert.match(error.textContent, /Choose a PNG, JPEG or WebP/);
  assert.equal(error.hidden, false);
  assert.equal(file.disabled, false);
});

test('identity changes close account dialogs and reject actions from stale controls', async () => {
  const f = await fixture(); await f.community.openAccount();
  const view = dialog(f, 'Account');
  const save = action(view, 'Save profile');
  f.auth.userId = 'account-b'; f.auth.jwt = 'token-b'; f.community.refresh();
  assert.equal(view.open, false);
  save.click(); await flush();
  assert.equal(f.state.requests.length, 1, 'only the original account GET should be sent');
  assert.equal(f.state.updated.length, 0);
});

test('leaving a room closes management dialogs without affecting auth identity', async () => {
  const f = await fixture();
  f.state.room = { currentRoomId: 'one', localParticipantId: 'account-a', nickname: 'Alice', role: 'owner' };
  f.community.refresh(); f.community.openNickname();
  const view = dialog(f, 'Room nickname');
  f.state.room = null; f.community.refresh();
  assert.equal(view.open, false);
  assert.equal(f.auth.userId, 'account-a');
});

test('an initial account response cannot render after an identity change', async () => {
  const f = await fixture(); const pending = deferred(); f.state.handle = () => pending.promise;
  const opening = f.community.openAccount();
  const view = dialog(f, 'Account');
  f.auth.userId = 'account-b'; f.auth.jwt = 'token-b'; f.community.refresh();
  pending.resolve(f.profile); await opening;
  assert.equal(view.querySelectorAll('input').length, 0);
});

test('recovery key is shown once, copies exactly, and clears on dialog close', async () => {
  const f = await fixture(); await f.community.openAccount();
  const account = dialog(f, 'Account');
  control(account, 'Current password').value = 'current-password';
  const result = { recovery_key: 'sc-recovery-test-only' }; f.state.handle = async () => result;
  action(account, 'Generate recovery key').click(); await flush();
  assert.deepEqual(f.state.requests.at(-1), ['/api/auth/recovery/key', 'token-a', 'POST', { current_password: 'current-password' }]);
  const view = dialog(f, 'Save your recovery key');
  const key = control(view, 'Recovery key');
  assert.equal(key.readOnly, true);
  action(view, 'Copy recovery key').click(); await flush();
  assert.equal(f.state.copied, 'sc-recovery-test-only');
  assert.equal(control(account, 'Current password').value, '');
  view.close();
  assert.equal(key.value, ''); assert.equal(result.recovery_key, '');
});

for (const reason of ['close', 'identity change']) {
  test(`a recovery-key response after ${reason} does not open another dialog`, async () => {
    const f = await fixture(); await f.community.openAccount();
    const account = dialog(f, 'Account'); control(account, 'Current password').value = 'current-password';
    const pending = deferred(); f.state.handle = () => pending.promise;
    action(account, 'Generate recovery key').click(); await flush();
    if (reason === 'close') account.close();
    else { f.auth.userId = 'account-b'; f.auth.jwt = 'token-b'; f.community.refresh(); }
    const result = { recovery_key: 'sc-recovery-test-only' }; pending.resolve(result); await flush();
    assert.equal(f.document.querySelectorAll('dialog').length, 0);
    assert.equal(result.recovery_key, '');
  });
}

for (const changedIdentity of [false, true]) {
  test(`successful pending password change ${changedIdentity ? 'does not sign out a new identity' : 'signs out the same account even after dialog close'}`, async () => {
    const f = await fixture(); await f.community.openAccount();
    const view = dialog(f, 'Account');
    control(view, 'Current password').value = 'current-password';
    control(view, 'New password').value = control(view, 'Confirm new password').value = 'new-password';
    const pending = deferred(); f.state.handle = () => pending.promise;
    action(view, 'Change password').click(); await flush();
    assert.equal(f.state.requests.at(-1)[1], 'token-a');
    view.close();
    if (changedIdentity) { f.auth.userId = 'account-b'; f.auth.jwt = 'token-b'; f.community.refresh(); }
    pending.resolve(undefined); await flush();
    assert.equal(f.state.signedOut, changedIdentity ? 0 : 1);
  });
}

test('room links preserve the current origin and path while removing unrelated query and fragment', async () => {
  const f = await fixture();
  assert.equal(f.roomLink('friendly room'), 'http://localhost:3000/#friendly%20room');
});
