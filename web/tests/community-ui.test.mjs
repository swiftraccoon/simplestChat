import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';
import { loadAppearanceFixture } from './appearance-fixture.mjs';

async function fixture() {
  const dom = await uiFixture();
  const auth = { isLoggedIn: true, userId: 'account-a', jwt: 'token-a' };
  const profile = {
    id: 'account-a',
    email: 'a@example.test',
    display_name: 'Alice',
    bio: '',
    avatar_url: null,
    recovery_enabled: false,
    profile_style: { color: null, style: 'accent' },
  };
  const state = {
    requests: [],
    notifications: [],
    updated: [],
    signedOut: 0,
    room: null,
    handle: async (path) =>
      path === '/api/auth/passkeys'
        ? { password_enabled: true, recovery_enabled: false, passkeys: [], maximum: 10 }
        : { ...profile },
    upload: dom.ui.rasterUpload,
  };
  const request = async (...args) => {
    state.requests.push(args);
    return state.handle(...args);
  };
  const ui = {
    ...dom.ui,
    api: {
      publicProfile: (id, token) => request(`/api/auth/profiles/${encodeURIComponent(id)}`, token),
      accountProfile: (token) => request('/api/auth/profile', token),
      updateProfile: (token, data) => request('/api/auth/profile', token, 'PATCH', data),
      changePassword: (token, data) => request('/api/auth/password', token, 'POST', data),
      passkeySettings: (token) => request('/api/auth/passkeys', token),
      passkeyAction: (token, data) => request('/api/auth/passkeys/start', token, 'POST', data),
      redeemRecovery: (data) => request('/api/auth/recovery/redeem', null, 'POST', data),
      ownRooms: (token) => request('/api/rooms/mine', token),
      updateRoomIdentity: (id, token, data) =>
        request(`/api/rooms/${encodeURIComponent(id)}/identity`, token, 'PATCH', data),
      deleteRoom: (id, token) => request(`/api/rooms/${encodeURIComponent(id)}`, token, 'DELETE'),
    },
    rasterUpload: (...args) => state.upload(...args),
  };
  const security = await loadTypeScript('src/account-security.ts', {
    modules: { './ui': ui, './auth': await loadTypeScript('src/auth.ts') },
    globals: {
      document: dom.document,
      navigator: {
        clipboard: {
          writeText: async (value) => {
            state.copied = value;
          },
        },
      },
    },
  });
  const api = await loadTypeScript('src/community-ui.ts', {
    modules: {
      './ui': ui,
      './appearance': await loadAppearanceFixture({ ...dom, ui }),
      './account-security': {
        ...security,
        mountAccountSecurity: (options) => {
          state.securityCompleted = options.completed;
          return security.mountAccountSecurity(options);
        },
      },
    },
    globals: {
      document: dom.document,
      TextEncoder,
      URL,
      window: {
        location: { href: 'http://localhost:3000/?old=value#old-room' },
        confirm: () => true,
      },
      navigator: {
        clipboard: {
          writeText: async (value) => {
            state.copied = value;
          },
        },
      },
    },
  });
  const community = new api.CommunityUI({
    auth,
    getRoom: () => state.room,
    notify: (value) => state.notifications.push(value),
    onProfileChanged: (value) => state.updated.push(value),
    onRoomsChanged() {},
    async onRoomDeleted() {},
    async onSignedOut() {
      state.signedOut++;
    },
  });
  return { ...dom, ...api, ui, auth, state, profile, community };
}

function dialog(fixture, title) {
  const node = fixture.document
    .querySelectorAll('dialog')
    .find((node) => node.children[0].children[0].textContent === title);
  assert.ok(node, `Expected ${title} dialog`);
  return node;
}

function control(view, label) {
  const wrapper = view
    .querySelectorAll('label')
    .find((node) => node.children[0].textContent === label);
  assert.ok(wrapper, `Expected ${label} field`);
  return wrapper.children[1];
}

function action(view, text) {
  const node = view.querySelectorAll('button').find((node) => node.textContent === text);
  assert.ok(node, `Expected ${text} action`);
  return node;
}

test('ban dismissal remains blocked until acknowledgement, then the owned dialog closes', async () => {
  const f = await fixture();
  const pending = deferred();
  const calls = [];
  f.state.room = {
    membershipVersion: 1,
    ban: (...args) => {
      calls.push(args);
      return pending.promise;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  const reason = control(view, 'Reason (optional)');
  const duration = control(view, 'Duration');
  reason.value = 'Repeated interruption';
  duration.value = '3600';
  const submit = action(view, 'Ban from room');
  submit.click();
  submit.click();
  await flush();
  assert.deepEqual(calls, [['target', 'Repeated interruption', 3600]]);
  assert.equal(reason.disabled, true);
  assert.equal(duration.disabled, true);
  assert.equal(submit.disabled, true);
  action(view, 'Close').click();
  view.emit('click', { clientX: 0, clientY: 0 });
  let prevented = false;
  view.emit('cancel', {
    preventDefault: () => {
      prevented = true;
    },
  });
  assert.equal(prevented, true);
  assert.equal(view.open, true);
  pending.resolve();
  await flush();
  assert.equal(view.open, false);
  assert.equal(view.isConnected, false);
});

test('ban rejection retains reason and duration with a visible error and working retry', async () => {
  const f = await fixture();
  const first = deferred();
  const second = deferred();
  const calls = [];
  f.state.room = {
    membershipVersion: 1,
    ban: (...args) => {
      calls.push(args);
      return calls.length === 1 ? first.promise : second.promise;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  const reason = control(view, 'Reason (optional)');
  const duration = control(view, 'Duration');
  const submit = action(view, 'Ban from room');
  reason.value = 'Keep this explanation';
  duration.value = '86400';
  submit.click();
  first.reject(new Error('The change was not confirmed'));
  await flush();
  assert.equal(view.open, true);
  assert.equal(view.children[1].hidden, false);
  assert.match(view.children[1].textContent, /not confirmed/);
  assert.equal(reason.value, 'Keep this explanation');
  assert.equal(duration.value, '86400');
  assert.equal(reason.disabled, false);
  assert.equal(duration.disabled, false);
  assert.equal(submit.disabled, false);
  submit.click();
  await flush();
  assert.deepEqual(calls, [
    ['target', 'Keep this explanation', 86400],
    ['target', 'Keep this explanation', 86400],
  ]);
  assert.equal(view.children[1].hidden, true);
  second.resolve();
  await flush();
  assert.equal(view.isConnected, false);
});

test('a stale ban form closes without submitting against a replacement membership', async () => {
  const f = await fixture();
  let requests = 0;
  f.state.room = {
    membershipVersion: 1,
    ban: async () => {
      requests++;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  f.state.room.membershipVersion++;
  action(view, 'Ban from room').click();
  await flush();
  assert.equal(requests, 0);
  assert.equal(view.isConnected, false);
});

test('a late ban result retires only its old dialog and cannot dismiss a newer pending ban', async () => {
  const f = await fixture();
  const old = deferred();
  f.state.room = { membershipVersion: 1, ban: () => old.promise };
  f.community.ban('old-target', 'Old participant');
  const oldView = dialog(f, 'Ban Old participant');
  action(oldView, 'Ban from room').click();
  const current = deferred();
  f.state.room = { membershipVersion: 2, ban: () => current.promise };
  f.community.ban('new-target', 'New participant');
  const currentView = dialog(f, 'Ban New participant');
  action(currentView, 'Ban from room').click();
  old.reject(new Error('Old response'));
  await flush();
  assert.equal(oldView.isConnected, false);
  assert.equal(currentView.open, true);
  assert.equal(action(currentView, 'Ban from room').disabled, true);
  assert.equal(currentView.children[1].hidden, true);
  action(currentView, 'Close').click();
  assert.equal(currentView.open, true);
  current.resolve();
  await flush();
  assert.equal(currentView.isConnected, false);
});

test('recovery validates normalized character and byte bounds before sending', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  const password = control(view, 'New password');
  const confirm = control(view, 'Confirm new password');
  const submit = action(view, 'Reset password with key');
  for (const [value, confirmation, error] of [
    ['1234567', '1234567', /15 and 128 characters/],
    ['a'.repeat(129), 'a'.repeat(129), /15 and 128 characters/],
    ['😀'.repeat(129), '😀'.repeat(129), /15 and 128 characters/],
    ['long-password-one', 'long-password-two', /do not match/],
    ['long-password-123\t', 'long-password-123\t', /control characters/],
    ['long-password-123\u0085', 'long-password-123\u0085', /control characters/],
  ]) {
    password.value = value;
    confirm.value = confirmation;
    submit.click();
    await flush();
    assert.equal(f.state.requests.length, 0);
    assert.match(view.children[1].textContent, error);
    assert.equal(submit.disabled, false);
  }
  control(view, 'Email').value = ' a@example.test ';
  const key = control(view, 'Saved recovery key');
  key.value = ' saved-test-key ';
  password.value = confirm.value = 'é'.repeat(64); // Exactly 128 UTF-8 bytes.
  submit.click();
  await flush();
  assert.deepEqual(f.state.requests[0], [
    '/api/auth/recovery/redeem',
    null,
    'POST',
    {
      email: 'a@example.test',
      recovery_key: 'saved-test-key',
      new_password: 'é'.repeat(64),
    },
  ]);
  assert.equal(key.value, '');
  assert.equal(view.isConnected, false);
  assert.match(f.state.notifications[0], /Password reset/);
});

test('recovery accepts normalized equivalent confirmation without altering the request', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  control(view, 'New password').value = 'e\u0301'.repeat(15);
  control(view, 'Confirm new password').value = 'é'.repeat(15);
  action(view, 'Reset password with key').click();
  await flush();
  assert.equal(f.state.requests[0][3].new_password, 'e\u0301'.repeat(15));
});

test('profile text remains text and remote avatar URLs are not rendered', async () => {
  const f = await fixture();
  f.state.handle = async () => ({
    ...f.profile,
    display_name: '<b>Alice</b>',
    bio: '<p>Biography</p>',
    avatar_url: 'https://example.test/avatar.png',
  });
  await f.community.showProfile('account-a');
  const view = dialog(f, 'Profile');
  assert.equal(view.querySelectorAll('h3')[0].textContent, '<b>Alice</b>');
  assert.equal(view.querySelectorAll('p')[1].textContent, '<p>Biography</p>');
  assert.equal(view.querySelectorAll('img').length, 0);
});

test('avatars decorate connected authenticated nodes only and retain text for guests', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const guest = f.ui.el('div', 'Guest');
  f.document.body.append(guest);
  f.community.decorateAvatar(guest, 'guest-id', false);
  assert.equal(f.state.requests.length, 0);
  const live = f.ui.el('div', 'A');
  f.document.body.append(live);
  const removed = f.ui.el('div', 'A');
  f.document.body.append(removed);
  f.community.decorateAvatar(live, 'account-a', true);
  f.community.decorateAvatar(removed, 'account-a', true);
  removed.remove();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' });
  await flush();
  assert.equal(f.state.requests.length, 1, 'profile lookups should be shared');
  assert.equal(live.children[0].tagName, 'IMG');
  assert.equal(live.children[0].alt, '');
  assert.equal(removed.textContent, 'A');
  assert.equal(guest.textContent, 'Guest');
});

test('closing a profile while it loads prevents detached content from being rendered', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const opening = f.community.showProfile('account-a');
  const view = dialog(f, 'Profile');
  view.close();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' });
  await opening;
  assert.equal(view.querySelectorAll('img').length, 0);
  assert.equal(view.querySelectorAll('h3').length, 0);
});

test('image removal wins over a pending upload and cannot be undone by its late result', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.upload = () => pending.promise;
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/png', size: 1024 }];
  file.emit('change');
  assert.throws(() => picker.value(), /finish processing/);
  action(picker.wrapper, 'Remove image').click();
  pending.resolve('data:image/jpeg;base64,Yg==');
  await flush();
  assert.equal(picker.value(), null);
  assert.equal(picker.wrapper.querySelectorAll('img')[0].hidden, true);
  assert.equal(file.disabled, false);
});

test('invalid uploads preserve the current image and display the validation failure safely', async () => {
  const f = await fixture();
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  f.document.body.append(error, picker.wrapper);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/svg+xml', size: 100 }];
  file.emit('change');
  await flush();
  assert.equal(picker.value(), 'data:image/jpeg;base64,YQ==');
  assert.match(error.textContent, /Choose a PNG, JPEG or WebP/);
  assert.equal(error.hidden, false);
  assert.equal(file.disabled, false);
});

for (const [label, method] of [
  ['Account', 'openAccount'],
  ['My rooms', 'openRooms'],
]) {
  test(`${label} click failures are handled in context and ignored after an identity change`, async () => {
    const f = await fixture();
    const first = deferred();
    f.community[method] = () => first.promise;
    action(f.header, label).click();
    first.reject(new Error('Current action failed'));
    await flush();
    assert.deepEqual(f.state.notifications, ['Current action failed']);

    const second = deferred();
    f.community[method] = () => second.promise;
    action(f.header, label).click();
    f.auth.userId = 'account-b';
    f.auth.jwt = 'token-b';
    f.community.refresh();
    second.reject(new Error('Retired action failed'));
    await flush();
    assert.deepEqual(f.state.notifications, ['Current action failed']);
  });
}

test('closed account dialogs do not receive late load failures', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const opening = f.community.openAccount();
  const view = dialog(f, 'Account');
  const error = view.children[1];
  view.close();
  pending.reject(new Error('Late account failure'));
  await opening;
  assert.equal(error.hidden, true);
  assert.equal(error.textContent, '');
});

test('Account saves the separate hovercard appearance and updates its live name preview', async () => {
  const f = await fixture();
  f.profile.profile_style = { color: 'teal', style: 'bubble' };
  await f.community.openAccount();
  const view = dialog(f, 'Account');
  const picker = view.querySelector('.appearance-picker');
  const name = control(view, 'Account display name');
  name.value = 'Changed account name';
  name.emit('input');
  assert.equal(picker.querySelector('.appearance-preview').textContent, 'Changed account name');
  assert.equal(picker.querySelector('.appearance-preview').dataset.appearance, 'bubble');
  const radios = picker.querySelectorAll('input');
  for (const radio of radios) {
    radio.checked = radio.name.endsWith('-color')
      ? radio.value === 'violet'
      : radio.value === 'text';
  }
  radios.find((radio) => radio.value === 'violet').emit('change');
  action(view, 'Save profile').click();
  await flush();
  const request = f.state.requests.find((entry) => entry[2] === 'PATCH');
  assert.deepEqual(request[3], {
    display_name: 'Changed account name',
    bio: '',
    avatar_url: null,
    profile_style: { color: 'violet', style: 'text' },
  });
  assert.equal('chat_style' in request[3], false);
  assert.equal(f.state.updated.length, 1);
});

test('reopening a hovercard fetches current profile appearance without retaining stale cache', async () => {
  const f = await fixture();
  const first = await f.community.participantProfile('account-a');
  assert.deepEqual(first.profile_style, { color: null, style: 'accent' });
  f.profile.profile_style = { color: 'rose', style: 'text' };
  const second = await f.community.participantProfile('account-a');
  assert.deepEqual(second.profile_style, { color: 'rose', style: 'text' });
  assert.equal(f.state.requests.length, 2);
  assert.equal(f.community.profiles.has('account-a'), false);
});

test('identity changes close account dialogs and reject actions from stale controls', async () => {
  const f = await fixture();
  await f.community.openAccount();
  const view = dialog(f, 'Account');
  const save = action(view, 'Save profile');
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.community.refresh();
  assert.equal(view.open, false);
  save.click();
  await flush();
  assert.equal(
    f.state.requests.length,
    2,
    'only the original account and security GETs should be sent',
  );
  assert.equal(f.state.updated.length, 0);
});

test('leaving a room closes management dialogs without affecting auth identity', async () => {
  const f = await fixture();
  f.state.room = {
    currentRoomId: 'one',
    localParticipantId: 'account-a',
    nickname: 'Alice',
    role: 'owner',
  };
  f.community.refresh();
  f.community.openNickname();
  const view = dialog(f, 'Room nickname');
  f.state.room = null;
  f.community.refresh();
  assert.equal(view.open, false);
  assert.equal(f.auth.userId, 'account-a');
});

test('an initial account response cannot render after an identity change', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const opening = f.community.openAccount();
  const view = dialog(f, 'Account');
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.community.refresh();
  pending.resolve(f.profile);
  await opening;
  assert.equal(view.querySelectorAll('input').length, 0);
});

test('recovery key is shown once, copies exactly, and clears on dialog close', async () => {
  const f = await fixture();
  await f.community.openAccount();
  const account = dialog(f, 'Account');
  control(account, 'Current password for verification').value = 'current-password';
  const result = { kind: 'recovery_key', recovery_key: 'sc-recovery-test-only' };
  f.state.handle = async () => result;
  action(account, 'Generate recovery key').click();
  await flush();
  assert.deepEqual(f.state.requests.at(-1), [
    '/api/auth/passkeys/start',
    'token-a',
    'POST',
    { operation: { action: 'recovery_key' }, current_password: 'current-password' },
  ]);
  const view = account;
  const key = control(view, 'Recovery key');
  assert.equal(key.readOnly, true);
  action(view, 'Copy recovery key').click();
  await flush();
  assert.equal(f.state.copied, 'sc-recovery-test-only');
  assert.equal(
    f.created.filter((node) => node.type === 'password').every((node) => node.value === ''),
    true,
  );
  view.close();
  assert.equal(key.value, '');
  assert.equal(result.recovery_key, '');
});

for (const kind of ['removed', 'replaced']) {
  test(`confirmed passkey ${kind} closes Account and reports the matching sign-in guidance`, async () => {
    const f = await fixture();
    await f.community.openAccount();
    const account = dialog(f, 'Account');
    f.state.securityCompleted(kind);
    await flush();
    assert.equal(account.open, false);
    assert.equal(f.state.signedOut, 1);
    assert.deepEqual(f.state.notifications, [
      kind === 'replaced'
        ? 'Passkey replaced. Sign in again with your new passkey.'
        : 'Passkey removed. Sign in again with a remaining sign-in method.',
    ]);
  });
}

for (const reason of ['close', 'identity change']) {
  test(`a recovery-key response after ${reason} does not open another dialog`, async () => {
    const f = await fixture();
    await f.community.openAccount();
    const account = dialog(f, 'Account');
    control(account, 'Current password for verification').value = 'current-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(account, 'Generate recovery key').click();
    await flush();
    if (reason === 'close') account.close();
    else {
      f.auth.userId = 'account-b';
      f.auth.jwt = 'token-b';
      f.community.refresh();
    }
    const result = { kind: 'recovery_key', recovery_key: 'sc-recovery-test-only' };
    pending.resolve(result);
    await flush();
    assert.equal(f.document.querySelectorAll('dialog').length, 0);
    assert.equal(result.recovery_key, '');
  });
}

for (const changedIdentity of [false, true]) {
  test(`successful pending password change ${changedIdentity ? 'does not sign out a new identity' : 'blocks dismissal until the same account is signed out'}`, async () => {
    const f = await fixture();
    await f.community.openAccount();
    const view = dialog(f, 'Account');
    control(view, 'Current password').value = 'current-password';
    control(view, 'New password').value = control(view, 'Confirm new password').value =
      'new-long-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(view, 'Change password').click();
    await flush();
    assert.equal(f.state.requests.at(-1)[1], 'token-a');
    action(view, 'Close').click();
    assert.equal(view.open, true, 'a submitted password mutation owns dismissal');
    if (changedIdentity) {
      f.auth.userId = 'account-b';
      f.auth.jwt = 'token-b';
      f.community.refresh();
    }
    pending.resolve(undefined);
    await flush();
    assert.equal(f.state.signedOut, changedIdentity ? 0 : 1);
  });
}

test('room links preserve the current origin and path while removing unrelated query and fragment', async () => {
  const f = await fixture();
  assert.equal(f.roomLink('friendly room'), 'http://localhost:3000/#friendly%20room');
});

test('password, profile and passkey changes share ownership and stale controls cannot submit concurrently', async () => {
  const f = await fixture();
  await f.community.openAccount();
  const account = dialog(f, 'Account');
  const staleAdd = action(account, 'Add passkey');
  const save = action(account, 'Save profile');
  const change = action(account, 'Change password');
  control(account, 'Current password').value = 'old-password';
  control(account, 'New password').value = control(account, 'Confirm new password').value =
    'new-long-password';
  const pending = deferred();
  f.state.handle = () => pending.promise;
  change.click();
  staleAdd.click();
  save.click();
  await flush();
  assert.equal(f.state.requests.filter((request) => request[2] === 'POST').length, 1);
  assert.equal(f.state.requests.at(-1)[0], '/api/auth/password');
  assert.equal(save.disabled, true);
  assert.equal(change.disabled, true);
  f.auth.userId = 'account-b';
  f.community.refresh();
  pending.resolve();
  await flush();
  assert.equal(f.state.signedOut, 0);
});

test('profile completion cannot re-enable a control during a later uncertain account mutation', async () => {
  const f = await fixture();
  await f.community.openAccount();
  const account = dialog(f, 'Account');
  const staleAdd = action(account, 'Add passkey');
  const save = action(account, 'Save profile');
  const pending = deferred();
  f.state.handle = () => pending.promise;
  save.click();
  staleAdd.click();
  await flush();
  assert.equal(f.state.requests.at(-1)[0], '/api/auth/profile');
  assert.equal(f.state.requests.at(-1)[2], 'PATCH');
  assert.equal(f.state.requests.length, 3, 'the second mutation was refused');
  pending.resolve({ ...f.profile });
  await flush();
  assert.equal(save.disabled, false);
  control(account, 'Current password for verification').value = 'old-password';
  f.state.handle = async () => {
    throw new Error('Lost mutation response');
  };
  action(account, 'Generate recovery key').click();
  await flush();
  assert.equal(save.disabled, true);
  assert.equal(action(account, 'Change password').disabled, true);
  assert.match(account.textContent, /may have completed/);
});

for (const phase of ['pending', 'uncertain', 'recovery']) {
  test(`same-account room membership changes preserve ${phase} account ownership`, async () => {
    const f = await fixture();
    await f.community.openAccount();
    const account = dialog(f, 'Account');
    control(account, 'Current password for verification').value = 'old-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(account, 'Generate recovery key').click();
    await flush();
    if (phase === 'uncertain') pending.reject(new Error('Response lost'));
    if (phase === 'recovery') pending.resolve({ kind: 'recovery_key', recovery_key: 'save-once' });
    await flush();
    f.state.room = {
      currentRoomId: 'different-room',
      localParticipantId: 'new-membership',
      role: 'user',
    };
    f.community.refresh();
    assert.equal(account.open, true);
    if (phase === 'pending' || phase === 'uncertain') {
      action(account, 'Close').click();
      assert.equal(account.open, true);
    } else assert.equal(control(account, 'Recovery key').value, 'save-once');
    f.auth.userId = 'account-b';
    f.auth.jwt = 'token-b';
    f.community.refresh();
    assert.equal(account.open, false);
    if (phase === 'pending') pending.resolve({ kind: 'recovery_key', recovery_key: 'late-secret' });
    await flush();
    assert.ok(
      f.created
        .filter((node) => node.tagName === 'TEXTAREA' && node.readOnly)
        .every((node) => node.value === ''),
    );
  });
}

test('room-scoped actions mount in the room tools while account actions stay in the header', async () => {
  const dom = await uiFixture();
  const roomActions = dom.document.createElement('div');
  roomActions.id = 'room-actions';
  dom.document.body.append(roomActions);
  const security = await loadTypeScript('src/account-security.ts', {
    modules: { './ui': dom.ui, './auth': await loadTypeScript('src/auth.ts') },
    globals: { document: dom.document, navigator: { clipboard: { writeText: async () => {} } } },
  });
  const api = await loadTypeScript('src/community-ui.ts', {
    modules: {
      './ui': dom.ui,
      './account-security': security,
      './appearance': await loadAppearanceFixture(dom),
    },
    globals: {
      document: dom.document,
      TextEncoder,
      URL,
      window: { location: { href: 'http://localhost:3000/' } },
      navigator: { clipboard: { writeText: async () => {} } },
    },
  });
  new api.CommunityUI({
    auth: { isLoggedIn: true, userId: 'account-a', jwt: 'token-a' },
    getRoom: () => null,
    notify() {},
    onProfileChanged() {},
    onRoomsChanged() {},
    async onRoomDeleted() {},
    async onSignedOut() {},
  });
  assert.deepEqual(
    dom.header.children.map((node) => node.textContent),
    ['Account', 'My rooms'],
  );
  assert.deepEqual(
    roomActions.children.map((node) => node.textContent),
    ['Nickname', 'Manage room'],
  );
});

test('roster profiles deduplicate above 100 people, bound concurrency, and evict departed members', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const ids = Array.from({ length: 300 }, (_, index) => `person-${index}`);
  const paint = () => {
    for (const id of ids) {
      const avatar = f.document.createElement('div');
      f.document.body.append(avatar);
      f.community.decorateAvatar(avatar, id, true);
    }
  };
  f.community.retainProfiles(ids);
  paint();
  paint();
  assert.equal(f.state.requests.length, 8, 'pending profile reads have a concurrency ceiling');
  assert.equal(f.community.profiles.size, 300, 'queued and active reads share the same cache');
  pending.resolve({ ...f.profile, avatar_url: null });
  await Promise.all(f.community.profiles.values());
  assert.equal(f.state.requests.length, 300, 'exactly one read per account');
  paint();
  await flush();
  assert.equal(f.state.requests.length, 300, 'unchanged redraws do not fetch profiles again');
  f.community.retainProfiles(ids.slice(1));
  assert.equal(f.community.profiles.has(ids[0]), false);
  assert.equal(f.community.profiles.size, 299);
});

test('very large rosters keep bounded avatars without evicting active cached profiles on redraw', async () => {
  const f = await fixture();
  const ids = Array.from({ length: 700 }, (_, index) => `person-${index}`);
  const paint = () => {
    f.community.retainProfiles(ids);
    for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  };
  paint();
  await Promise.all(f.community.profiles.values());
  assert.equal(f.state.requests.length, 512);
  paint();
  await flush();
  assert.equal(f.state.requests.length, 512);
  assert.equal(f.community.profiles.size, 512);
});

test('retired queued profile reads do not run against a replacement room', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const ids = Array.from({ length: 20 }, (_, index) => `old-person-${index}`);
  f.community.retainProfiles(ids);
  for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  const work = Promise.all(f.community.profiles.values());
  f.state.room = { currentRoomId: 'new-room', localParticipantId: 'local', role: 'user' };
  f.community.refresh();
  pending.resolve(null);
  await work;
  assert.equal(f.state.requests.length, 8, 'only reads already sent may settle after replacement');
  assert.equal(f.community.profiles.size, 0);
});

test('explicit profile visits take the next slot before queued avatar decoration', async () => {
  const f = await fixture();
  const active = Array.from({ length: 8 }, () => deferred());
  let index = 0;
  f.state.handle = () => active[index++]?.promise ?? Promise.resolve(f.profile);
  const ids = Array.from({ length: 30 }, (_, number) => `avatar-${number}`);
  f.community.retainProfiles(ids);
  for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  const shown = f.community.showProfile('requested-account');
  assert.equal(f.state.requests.length, 8);
  active[0].resolve(f.profile);
  await flush();
  assert.equal(f.state.requests[8][0], '/api/auth/profiles/requested-account');
  for (const pending of active.slice(1)) pending.resolve(f.profile);
  await shown;
  await Promise.all(f.community.profiles.values());
});

test('membership pages append unique rooms and keep the cursor on a failed read', async () => {
  const f = await fixture();
  const requests = [];
  let failNext = false;
  const member = (id) => ({ id, display_name: id, role: 'member', participant_count: 1 });
  // Named endpoint contracts are tested separately; this fixture observes dialog ownership.
  f.community.options.onJoinRoom = (id) => requests.push(['join', id]);
  const original = f.community.openRooms;
  assert.equal(typeof original, 'function');
  f.state.handle = async (path) => {
    if (path === '/api/rooms/mine') return [];
    throw new Error('Unexpected fixture request');
  };
  // The shared fixture's API object is supplied by reference to this instance.
  f.community.options.auth.jwt = 'token';
  f.ui.api.memberships = async (_token, cursor) => {
    requests.push(['page', cursor]);
    if (failNext) {
      failNext = false;
      throw new Error('Temporary read failure');
    }
    return cursor
      ? { items: [member('first'), member('second')], next_cursor: null }
      : { items: [member('first')], next_cursor: 'first' };
  };
  await f.community.openRooms();
  const view = dialog(f, 'My rooms');
  const more = view
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Load more memberships');
  assert.ok(more);
  failNext = true;
  more.click();
  await flush();
  assert.equal(more.disabled, false);
  more.click();
  await flush();
  assert.equal(more.hidden, true);
  assert.equal(view.querySelectorAll('.owned-room').length, 2);
  assert.deepEqual(requests, [
    ['page', undefined],
    ['page', 'first'],
    ['page', 'first'],
  ]);
});

test('invitation links use only kind-specific fragments and discard the current query', async () => {
  const f = await fixture();
  const code = 'a'.repeat(32);
  for (const [kind, fragment] of [
    ['room', 'invite'],
    ['registration', 'register-invite'],
  ]) {
    const link = new URL(f.inviteLink(code, kind));
    assert.equal(link.search, '');
    assert.equal(link.hash, `#${fragment}=${code}`);
  }
});

test('issued invitation secrets appear only in a one-time share panel; lists revoke by metadata', async () => {
  const f = await fixture();
  f.Node.prototype.focus = function () {};
  f.Node.prototype.select = function () {};
  const view = f.ui.modal('Invitation test');
  const code = 'a'.repeat(32);
  f.community.showCreatedInvite(code, view, 'room');
  const panel = view.body.querySelector('.created-invitation');
  assert.equal(panel.querySelector('.invite-code').textContent, code);
  action(panel, 'Copy link').click();
  await flush();
  assert.equal(new URL(f.state.copied).hash, `#invite=${code}`);
  let revoked = 0;
  const row = f.community.inviteRow(
    {
      id: '11111111-1111-4111-8111-111111111111',
      uses_left: 1,
      expires_at: '2026-10-05',
      created_at: '2026-09-30',
      role: 'member',
    },
    view,
    async () => {},
    async () => {
      revoked++;
    },
  );
  view.body.append(row);
  assert.equal(row.querySelector('.invite-code'), null);
  assert.equal(row.textContent.includes(code), false);
  assert.equal(
    row.querySelectorAll('button').some((node) => node.textContent === 'Copy link'),
    false,
  );
  action(row, 'Revoke').click();
  await flush();
  assert.equal(revoked, 1);
  view.close();
  assert.equal(panel.isConnected, false);
});
